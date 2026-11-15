//! Round-trip tests against JSON modelled on the RFC 8620 / RFC 8621 examples.

use jmap_proto::mail::*;
use jmap_proto::request::InvocationError;
use jmap_proto::*;
use pretty_assertions::assert_eq;
use serde_json::{Value, json};

fn fixture(name: &str) -> Value {
    let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

#[test]
fn session_parses_and_round_trips() {
    let raw = fixture("session.json");
    let s: Session = serde_json::from_value(raw.clone()).unwrap();

    assert_eq!(s.username, "john@example.com");
    assert_eq!(
        s.primary_account(capability::MAIL),
        Some(&Id::from("A13824"))
    );
    assert!(s.has_capability(capability::MAIL));
    assert!(!s.has_capability(capability::SUBMISSION));
    let core = s.core();
    assert_eq!(core.max_calls_in_request, 32);
    assert_eq!(core.max_objects_in_get, 256);
    assert!(s.accounts[&Id::from("A97813")].is_read_only);

    assert_eq!(
        s.download_url_for(&"A13824".into(), &"B1".into(), "a b.png", "image/png"),
        "https://jmap.example.com/download/A13824/B1/a%20b.png?accept=image%2Fpng"
    );
    assert_eq!(
        s.event_source_url_for("*", "no", 60),
        "https://jmap.example.com/eventsource/?types=%2A&closeafter=no&ping=60"
    );

    assert_eq!(serde_json::to_value(&s).unwrap(), raw);
}

#[test]
fn request_with_back_references_serializes_like_rfc() {
    let account: Id = "A1".into();
    let mut query = Query::new(account.clone());
    query.filter = Some(
        EmailFilterCondition {
            in_mailbox: Some("id_of_inbox".into()),
            ..Default::default()
        }
        .into(),
    );
    query.sort = Some(vec![Comparator::desc(EmailSortProperty::ReceivedAt)]);
    query.position = Some(0);
    query.limit = Some(10);
    query.calculate_total = Some(true);
    let q = EmailQuery {
        query,
        collapse_threads: true,
    };
    let g = EmailGet::new(
        Get::new(account.clone())
            .ids_ref(ResultReference::new("t0", "Email/query", "/ids"))
            .properties(vec![EmailProperty::ThreadId]),
    );
    let t = ThreadGet(Get::new(account).ids_ref(ResultReference::new(
        "t1",
        "Email/get",
        "/list/*/threadId",
    )));

    let req = Request {
        using: vec![capability::CORE.into(), capability::MAIL.into()],
        method_calls: vec![
            Invocation::new(EmailQuery::NAME, serde_json::to_value(&q).unwrap(), "t0"),
            Invocation::new(EmailGet::NAME, serde_json::to_value(&g).unwrap(), "t1"),
            Invocation::new(ThreadGet::NAME, serde_json::to_value(&t).unwrap(), "t2"),
        ],
        created_ids: None,
    };
    assert_eq!(
        serde_json::to_value(&req).unwrap(),
        fixture("request_backref.json")
    );

    // And it parses back.
    let back: Request = serde_json::from_value(fixture("request_backref.json")).unwrap();
    assert_eq!(back, req);
}

#[test]
fn mail_response_parses_each_invocation() {
    let resp: Response = serde_json::from_value(fixture("response_mail.json")).unwrap();
    assert_eq!(resp.session_state, State::from("75128aab4b1b"));
    let r = &resp.method_responses;

    let q: QueryResponse = r[0].parse().unwrap();
    assert_eq!(q.total, Some(115));
    assert_eq!(q.ids.len(), 2);
    assert!(q.can_calculate_changes);

    let g: GetResponse<Email> = r[1].parse().unwrap();
    assert_eq!(g.not_found, vec![Id::from("Mec5c8d3e9e46")]);
    let e = &g.list[0];
    assert!(e.is_seen());
    assert!(e.is_flagged());
    assert!(e.in_mailbox(&"MB23cfa8094c0f41e6".into()));
    assert_eq!(e.from.as_ref().unwrap()[0].display(), "Joe Bloggs");
    assert_eq!(e.to.as_ref().unwrap()[0].display(), "john@example.com");
    assert_eq!(
        e.received_at.unwrap().to_rfc3339(),
        "2014-10-27T13:58:06+00:00"
    );
    assert_eq!(e.sent_at.unwrap().offset().local_minus_utc(), 10 * 3600);
    assert_eq!(
        e.html_body.as_ref().unwrap()[0].content_type.as_deref(),
        Some("text/html")
    );
    assert_eq!(
        e.body_values.as_ref().unwrap()["1"].value,
        "Hey, do you want to go out to dinner on Thursday?"
    );

    let t: GetResponse<Thread> = r[2].parse().unwrap();
    assert_eq!(t.list[0].email_ids.len(), 2);

    match r[3].parse::<ChangesResponse>() {
        Err(InvocationError::Method(err)) => {
            assert_eq!(err.kind, MethodErrorType::CannotCalculateChanges)
        }
        other => panic!("expected method error, got {other:?}"),
    }

    let s: SetResponse<Email> = r[4].parse().unwrap();
    assert!(s.updated.unwrap().contains_key(&Id::from("Msf5a8c3e0ef4")));
    assert_eq!(
        s.not_updated.unwrap()[&Id::from("Mec5c8d3e9e46")].kind,
        "notFound"
    );
}

#[test]
fn mailbox_get_response() {
    let g: GetResponse<Mailbox> = serde_json::from_value(fixture("mailbox_get.json")).unwrap();
    let inbox = &g.list[0];
    assert_eq!(inbox.role, Some(MailboxRole::Inbox));
    assert_eq!(inbox.unread_emails, Some(13905));
    assert!(inbox.my_rights.unwrap().may_set_seen);
    assert_eq!(g.list[1].role, None);
    assert!(!g.list[0].my_rights.unwrap().may_rename);
}

#[test]
fn query_changes_response() {
    let qc: QueryChangesResponse = serde_json::from_value(fixture("query_changes.json")).unwrap();
    assert_eq!(qc.removed.len(), 1);
    assert_eq!(
        qc.added,
        vec![AddedItem {
            id: "Mb3b2a3d8e9c0e8b17".into(),
            index: 0
        }]
    );
}

#[test]
fn email_set_patch_serialization() {
    let mut p = PatchObject::new();
    patch::keyword(&mut p, keyword::SEEN, true);
    patch::keyword(&mut p, keyword::FLAGGED, false);
    patch::mailbox(&mut p, &"MB/odd~id".into(), false);
    let set = EmailSet(Set::new("A1".into()).update("M1".into(), p));
    assert_eq!(
        serde_json::to_value(&set).unwrap(),
        json!({
            "accountId": "A1",
            "update": {
                "M1": {
                    "keywords/$seen": true,
                    "keywords/$flagged": null,
                    "mailboxIds/MB~1odd~0id": null
                }
            }
        })
    );

    let mut mv = PatchObject::new();
    let archive: Id = "MBarchive".into();
    patch::set_mailboxes(&mut mv, [&archive]);
    assert_eq!(
        Value::Object(mv),
        json!({ "mailboxIds": { "MBarchive": true } })
    );
}

#[test]
fn email_get_body_options() {
    let mut g = EmailGet::new(Get::new("A1".into()).ids(vec!["M1".into()]));
    g.fetch_html_body_values = true;
    g.max_body_value_bytes = Some(256);
    assert_eq!(
        serde_json::to_value(&g).unwrap(),
        json!({
            "accountId": "A1",
            "ids": ["M1"],
            "fetchHTMLBodyValues": true,
            "maxBodyValueBytes": 256
        })
    );
}

#[test]
fn filter_operator_tree() {
    let f: Filter<EmailFilterCondition> = Filter::and(vec![
        EmailFilterCondition {
            in_mailbox: Some("I".into()),
            ..Default::default()
        }
        .into(),
        Filter::not(vec![
            EmailFilterCondition {
                has_keyword: Some("$seen".into()),
                ..Default::default()
            }
            .into(),
        ]),
    ]);
    let v = serde_json::to_value(&f).unwrap();
    assert_eq!(
        v,
        json!({
            "operator": "AND",
            "conditions": [
                { "inMailbox": "I" },
                { "operator": "NOT", "conditions": [ { "hasKeyword": "$seen" } ] }
            ]
        })
    );
    let back: Filter<EmailFilterCondition> = serde_json::from_value(v).unwrap();
    assert_eq!(back, f);
}

#[test]
fn invocation_rejects_wrong_arity() {
    assert!(serde_json::from_value::<Invocation>(json!(["a", {}])).is_err());
    assert!(serde_json::from_value::<Invocation>(json!(["a", {}, "c", 1])).is_err());
}
