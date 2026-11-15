//! Transport tests against a wiremock JMAP server.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use futures::StreamExt;
use jmap_http::*;
use jmap_proto::mail::*;
use jmap_proto::*;
use serde_json::{Value, json};
use wiremock::matchers::{body_partial_json, header, method, path};
use wiremock::{Mock, MockServer, Request as WmRequest, ResponseTemplate};

fn session_json(base: &str, state: &str, max_calls: u64) -> Value {
    json!({
        "capabilities": {
            "urn:ietf:params:jmap:core": {
                "maxSizeUpload": 50000000, "maxConcurrentUpload": 4,
                "maxSizeRequest": 10000000, "maxConcurrentRequests": 4,
                "maxCallsInRequest": max_calls, "maxObjectsInGet": 2,
                "maxObjectsInSet": 2, "collationAlgorithms": []
            },
            "urn:ietf:params:jmap:mail": {}
        },
        "accounts": { "A1": { "name": "me@example.com", "isPersonal": true, "isReadOnly": false, "accountCapabilities": {} } },
        "primaryAccounts": { "urn:ietf:params:jmap:mail": "A1" },
        "username": "me@example.com",
        "apiUrl": format!("{base}/api/"),
        "downloadUrl": format!("{base}/download/{{accountId}}/{{blobId}}/{{name}}?type={{type}}"),
        "uploadUrl": format!("{base}/upload/{{accountId}}/"),
        "eventSourceUrl": format!("{base}/events?types={{types}}&closeafter={{closeafter}}&ping={{ping}}"),
        "state": state
    })
}

async fn server(max_calls: u64) -> MockServer {
    let s = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/jmap/session"))
        .and(header("authorization", "Bearer secret"))
        .respond_with(ResponseTemplate::new(200).set_body_json(session_json(
            &s.uri(),
            "S1",
            max_calls,
        )))
        .mount(&s)
        .await;
    s
}

async fn connect(s: &MockServer) -> JmapClient {
    JmapClient::connect(
        format!("{}/jmap/session", s.uri()),
        Arc::new(TokenAuth::new("secret")),
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn connects_and_reads_session() {
    let s = server(16).await;
    let c = connect(&s).await;
    let sess = c.session();
    assert_eq!(sess.username, "me@example.com");
    assert_eq!(sess.core().max_objects_in_get, 2);
}

#[tokio::test]
async fn bad_token_is_unauthorized() {
    let s = MockServer::start().await;
    Mock::given(path("/jmap/session"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&s)
        .await;
    let err = JmapClient::connect(
        format!("{}/jmap/session", s.uri()),
        Arc::new(TokenAuth::new("nope")),
    )
    .await
    .unwrap_err();
    assert!(matches!(err, Error::Unauthorized), "{err:?}");
}

#[tokio::test]
async fn batch_with_back_references_returns_typed_results() {
    let s = server(16).await;
    Mock::given(method("POST"))
        .and(path("/api/"))
        .and(body_partial_json(json!({
            "using": ["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:mail"],
            "methodCalls": [
                ["Email/query", { "accountId": "A1", "collapseThreads": true }, "c0"],
                ["Email/get", { "accountId": "A1", "#ids": { "resultOf": "c0", "name": "Email/query", "path": "/ids" } }, "c1"]
            ]
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "methodResponses": [
                ["Email/query", { "accountId": "A1", "queryState": "q1", "canCalculateChanges": true, "position": 0, "ids": ["M1"] }, "c0"],
                ["Email/get", { "accountId": "A1", "state": "e1", "list": [{ "id": "M1", "subject": "Hi" }], "notFound": [] }, "c1"]
            ],
            "sessionState": "S1"
        })))
        .expect(1)
        .mount(&s)
        .await;

    let c = connect(&s).await;
    let mut b = c.batch();
    let q = b
        .call(EmailQuery {
            query: Query::new("A1".into()),
            collapse_threads: true,
        })
        .unwrap();
    let g = b
        .call(EmailGet::new(
            Get::new("A1".into()).ids_ref(q.result_ref("/ids")),
        ))
        .unwrap();
    let r = b.send().await.unwrap();
    assert_eq!(r.get(&q).unwrap().ids, vec![Id::from("M1")]);
    assert_eq!(r.get(&g).unwrap().list[0].subject.as_deref(), Some("Hi"));
}

#[tokio::test]
async fn method_error_and_missing_response() {
    let s = server(16).await;
    Mock::given(method("POST"))
        .and(path("/api/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "methodResponses": [["error", { "type": "cannotCalculateChanges" }, "c0"]],
            "sessionState": "S1"
        })))
        .mount(&s)
        .await;
    let c = connect(&s).await;
    let mut b = c.batch();
    let h = b
        .call(EmailChanges(Changes {
            account_id: "A1".into(),
            since_state: "old".into(),
            max_changes: None,
        }))
        .unwrap();
    let h2 = b
        .call(MailboxChanges(Changes {
            account_id: "A1".into(),
            since_state: "old".into(),
            max_changes: None,
        }))
        .unwrap();
    let r = b.send().await.unwrap();
    let err = r.get(&h).unwrap_err();
    assert_eq!(
        err.method_error().unwrap().kind,
        MethodErrorType::CannotCalculateChanges
    );
    assert!(matches!(r.get(&h2), Err(Error::MissingResponse(_))));
}

#[tokio::test]
async fn request_level_problem_details() {
    let s = server(16).await;
    Mock::given(method("POST"))
        .and(path("/api/"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({
            "type": "urn:ietf:params:jmap:error:limit", "status": 400, "limit": "maxSizeRequest"
        })))
        .mount(&s)
        .await;
    let c = connect(&s).await;
    let mut b = c.batch();
    b.call(MailboxGet(Get::new("A1".into()))).unwrap();
    match b.send().await.unwrap_err() {
        Error::Problem(p) => {
            assert_eq!(p.kind, ProblemDetails::LIMIT);
            assert_eq!(p.limit.as_deref(), Some("maxSizeRequest"));
        }
        e => panic!("unexpected {e:?}"),
    }
}

#[tokio::test]
async fn too_many_calls_rejected_locally() {
    let s = server(1).await;
    let c = connect(&s).await;
    let mut b = c.batch();
    b.call(MailboxGet(Get::new("A1".into()))).unwrap();
    b.call(ThreadGet(Get::new("A1".into()))).unwrap();
    assert!(matches!(
        b.send().await,
        Err(Error::TooManyCalls { calls: 2, max: 1 })
    ));
}

#[tokio::test]
async fn session_state_change_triggers_refresh() {
    let s = MockServer::start().await;
    let refreshed = Arc::new(AtomicBool::new(false));
    let flag = refreshed.clone();
    let uri = s.uri();
    Mock::given(method("GET"))
        .and(path("/jmap/session"))
        .respond_with(move |_: &WmRequest| {
            let state = if flag.swap(true, Ordering::SeqCst) {
                "S2"
            } else {
                "S1"
            };
            ResponseTemplate::new(200).set_body_json(session_json(&uri, state, 16))
        })
        .mount(&s)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "methodResponses": [["Mailbox/get", { "accountId": "A1", "state": "m1", "list": [], "notFound": [] }, "c0"]],
            "sessionState": "S2"
        })))
        .mount(&s)
        .await;
    let c = connect(&s).await;
    assert_eq!(c.session().state.as_str(), "S1");
    let mut b = c.batch();
    b.call(MailboxGet(Get::new("A1".into()))).unwrap();
    b.send().await.unwrap();
    assert_eq!(c.session().state.as_str(), "S2");
}

#[derive(Debug)]
struct FlakyAuth {
    refreshed: AtomicBool,
}

impl Auth for FlakyAuth {
    fn authorization(&self) -> Result<reqwest::header::HeaderValue> {
        let t = if self.refreshed.load(Ordering::SeqCst) {
            "Bearer secret"
        } else {
            "Bearer stale"
        };
        Ok(reqwest::header::HeaderValue::from_static(t))
    }
    fn refresh(&self) -> AuthFuture<'_> {
        Box::pin(async {
            self.refreshed.store(true, Ordering::SeqCst);
            Ok(true)
        })
    }
}

#[tokio::test]
async fn refreshable_auth_retries_once_after_401() {
    let s = MockServer::start().await;
    let uri = s.uri();
    Mock::given(method("GET"))
        .and(path("/jmap/session"))
        .respond_with(ResponseTemplate::new(200).set_body_json(session_json(&uri, "S1", 16)))
        .mount(&s)
        .await;
    Mock::given(method("GET"))
        .and(path("/download/A1/B1/f.txt"))
        .and(header("authorization", "Bearer stale"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&s)
        .await;
    Mock::given(method("GET"))
        .and(path("/download/A1/B1/f.txt"))
        .and(header("authorization", "Bearer secret"))
        .respond_with(ResponseTemplate::new(200).set_body_string("blob!"))
        .mount(&s)
        .await;
    let c = JmapClient::connect(
        format!("{}/jmap/session", s.uri()),
        Arc::new(FlakyAuth {
            refreshed: AtomicBool::new(false),
        }),
    )
    .await
    .unwrap();
    let bytes = c
        .download(&"A1".into(), &"B1".into(), "f.txt", "text/plain")
        .await
        .unwrap();
    assert_eq!(&bytes[..], b"blob!");
}

#[tokio::test]
async fn event_source_yields_state_changes() {
    let s = server(16).await;
    let body = "event: ping\ndata: {\"interval\":60}\n\n\
                event: state\ndata: {\"@type\":\"StateChange\",\"changed\":{\"A1\":{\"Email\":\"e2\"}}}\n\n";
    Mock::given(method("GET"))
        .and(path("/events"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(body),
        )
        .mount(&s)
        .await;
    let c = connect(&s).await;
    let events: Vec<_> = c.event_source(60).await.unwrap().collect().await;
    assert!(matches!(events[0], Ok(PushEvent::Ping)));
    match &events[1] {
        Ok(PushEvent::StateChange(sc)) => {
            assert_eq!(sc.changed[&Id::from("A1")]["Email"].as_str(), "e2")
        }
        e => panic!("unexpected {e:?}"),
    }
    assert_eq!(events.len(), 2);
}

#[test]
fn chunking() {
    let ids: Vec<u32> = (0..5).collect();
    let chunks: Vec<_> = chunk_ids(&ids, 2).collect();
    assert_eq!(chunks, vec![vec![0, 1], vec![2, 3], vec![4]]);
    assert_eq!(chunk_ids(&ids, 0).count(), 5);
}
