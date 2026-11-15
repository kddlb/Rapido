//! A small stateful fake JMAP mail server for sync tests, served via wiremock.
//! It implements just enough of RFC 8620/8621 for the sync engine: back-references,
//! /get with properties, /changes, Email/query (collapseThreads), Email/queryChanges
//! and Email/set keyword/mailbox patches.

#![allow(dead_code)]

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, Mutex};

use serde_json::{Map, Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Created,
    Updated,
    Destroyed,
}

#[derive(Default)]
pub struct FakeState {
    pub mailboxes: BTreeMap<String, Value>,
    pub emails: BTreeMap<String, Value>,
    mailbox_state: u64,
    email_state: u64,
    mailbox_log: Vec<(u64, String, Kind)>,
    email_log: Vec<(u64, String, Kind)>,
    query_snapshots: HashMap<(String, u64), Vec<String>>,
    /// Force `cannotCalculateChanges` from Email/changes.
    pub fail_email_changes: bool,
    /// Reject every Email/set update with this SetError type.
    pub reject_set: Option<String>,
    pub set_calls: usize,
    /// Answer every API request with HTTP 503.
    pub unavailable: bool,
}

impl FakeState {
    pub fn add_mailbox(&mut self, id: &str, name: &str, role: Option<&str>) {
        self.mailbox_state += 1;
        self.mailboxes.insert(
            id.into(),
            json!({ "id": id, "name": name, "parentId": null, "role": role, "sortOrder": 0, "isSubscribed": true }),
        );
        self.mailbox_log
            .push((self.mailbox_state, id.into(), Kind::Created));
    }

    pub fn add_email(&mut self, id: &str, thread: &str, mailbox: &str, received: u32, seen: bool) {
        self.email_state += 1;
        let kw = if seen {
            json!({ "$seen": true })
        } else {
            json!({})
        };
        self.emails.insert(
            id.into(),
            json!({
                "id": id, "blobId": format!("B{id}"), "threadId": thread,
                "mailboxIds": { mailbox: true }, "keywords": kw, "size": 100,
                "receivedAt": format!("2025-01-01T00:{:02}:{:02}Z", received / 60, received % 60),
                "messageId": [format!("{id}@x")], "inReplyTo": null, "references": null,
                "from": [{ "name": format!("Sender {id}"), "email": format!("{id}@example.com") }],
                "to": [{ "name": null, "email": "me@example.com" }], "cc": null, "replyTo": null,
                "subject": format!("Subject {id}"), "sentAt": "2025-01-01T00:00:00+01:00",
                "hasAttachment": false, "preview": format!("Preview {id}"),
                "headers": [
                    { "name": "Received", "value": " from mx.example.com\r\n\tby mx2.example.com" },
                    { "name": "From", "value": format!(" Sender {id} <{id}@example.com>") },
                    { "name": "Subject", "value": format!(" Subject {id}") }
                ],
                "textBody": [{ "partId": "1", "blobId": format!("T{id}"), "type": "text/plain", "size": 10 }],
                "htmlBody": [{ "partId": "2", "blobId": format!("H{id}"), "type": "text/html", "size": 20 }],
                "attachments": [],
                "bodyValues": {
                    "1": { "value": format!("Body of {id}"), "isEncodingProblem": false, "isTruncated": false },
                    "2": { "value": format!("<p>Body of {id}</p>"), "isEncodingProblem": false, "isTruncated": false }
                }
            }),
        );
        self.email_log
            .push((self.email_state, id.into(), Kind::Created));
        self.touch_mailboxes();
    }

    pub fn set_keyword(&mut self, id: &str, kw: &str, on: bool) {
        let e = self.emails.get_mut(id).unwrap();
        let kws = e["keywords"].as_object_mut().unwrap();
        if on {
            kws.insert(kw.into(), json!(true));
        } else {
            kws.remove(kw);
        }
        self.email_updated(id);
    }

    pub fn destroy_email(&mut self, id: &str) {
        self.emails.remove(id);
        self.email_state += 1;
        self.email_log
            .push((self.email_state, id.into(), Kind::Destroyed));
        self.touch_mailboxes();
    }

    pub fn keywords(&self, id: &str) -> Vec<String> {
        let mut v: Vec<String> = self.emails[id]["keywords"]
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect();
        v.sort();
        v
    }

    pub fn mailbox_ids(&self, id: &str) -> Vec<String> {
        let mut v: Vec<String> = self.emails[id]["mailboxIds"]
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect();
        v.sort();
        v
    }

    fn email_updated(&mut self, id: &str) {
        self.email_state += 1;
        self.email_log
            .push((self.email_state, id.into(), Kind::Updated));
        self.touch_mailboxes();
    }

    /// Counts changed: every mailbox is "updated".
    fn touch_mailboxes(&mut self) {
        self.mailbox_state += 1;
        let ids: Vec<String> = self.mailboxes.keys().cloned().collect();
        for id in ids {
            self.mailbox_log
                .push((self.mailbox_state, id, Kind::Updated));
        }
    }

    fn mailbox_with_counts(&self, id: &str) -> Value {
        let mut m = self.mailboxes[id].clone();
        let in_mb: Vec<&Value> = self
            .emails
            .values()
            .filter(|e| e["mailboxIds"].get(id).is_some())
            .collect();
        let unread = in_mb
            .iter()
            .filter(|e| e["keywords"].get("$seen").is_none())
            .count();
        let threads: HashSet<&str> = in_mb
            .iter()
            .map(|e| e["threadId"].as_str().unwrap())
            .collect();
        let o = m.as_object_mut().unwrap();
        o.insert("totalEmails".into(), json!(in_mb.len()));
        o.insert("unreadEmails".into(), json!(unread));
        o.insert("totalThreads".into(), json!(threads.len()));
        o.insert("unreadThreads".into(), json!(0));
        m
    }

    fn query_ids(&self, mailbox: &str) -> Vec<String> {
        let mut list: Vec<&Value> = self
            .emails
            .values()
            .filter(|e| e["mailboxIds"].get(mailbox).is_some())
            .collect();
        list.sort_by(|a, b| {
            b["receivedAt"]
                .as_str()
                .cmp(&a["receivedAt"].as_str())
                .then(b["id"].as_str().cmp(&a["id"].as_str()))
        });
        let mut seen = HashSet::new();
        list.into_iter()
            .filter(|e| seen.insert(e["threadId"].as_str().unwrap().to_string()))
            .map(|e| e["id"].as_str().unwrap().to_string())
            .collect()
    }

    fn changes(
        log: &[(u64, String, Kind)],
        since: u64,
        exists: impl Fn(&str) -> bool,
    ) -> (Vec<String>, Vec<String>, Vec<String>) {
        let mut created = Vec::new();
        let mut updated = Vec::new();
        let mut destroyed = Vec::new();
        let mut created_in_range = HashSet::new();
        for (s, id, k) in log.iter().filter(|(s, _, _)| *s > since) {
            let _ = s;
            match k {
                Kind::Created => {
                    created_in_range.insert(id.clone());
                }
                Kind::Updated => {}
                Kind::Destroyed => {}
            }
        }
        let mut done = HashSet::new();
        for (_, id, k) in log.iter().filter(|(s, _, _)| *s > since) {
            if !done.insert((id.clone(), *k as u8)) {
                continue;
            }
            match k {
                Kind::Created if exists(id) => created.push(id.clone()),
                Kind::Updated
                    if exists(id) && !created_in_range.contains(id) && !updated.contains(id) =>
                {
                    updated.push(id.clone())
                }
                Kind::Destroyed if !created_in_range.contains(id) && !destroyed.contains(id) => {
                    destroyed.push(id.clone())
                }
                _ => {}
            }
        }
        (created, updated, destroyed)
    }
}

fn filter_props(obj: &Value, props: Option<&Value>, always: &[&str]) -> Value {
    let Some(props) = props.and_then(Value::as_array) else {
        return obj.clone();
    };
    let mut out = Map::new();
    for p in props
        .iter()
        .filter_map(Value::as_str)
        .chain(always.iter().copied())
    {
        if let Some(v) = obj.get(p) {
            out.insert(p.into(), v.clone());
        }
    }
    Value::Object(out)
}

/// Evaluate a JMAP result-reference path (JSON pointer with `*` over arrays).
fn eval_path(v: &Value, path: &str) -> Value {
    fn walk(v: &Value, segs: &[&str]) -> Value {
        let Some((first, rest)) = segs.split_first() else {
            return v.clone();
        };
        if *first == "*" {
            let mut out = Vec::new();
            for item in v.as_array().cloned().unwrap_or_default() {
                match walk(&item, rest) {
                    Value::Array(a) => out.extend(a),
                    other => out.push(other),
                }
            }
            return Value::Array(out);
        }
        walk(v.get(*first).unwrap_or(&Value::Null), rest)
    }
    let segs: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    walk(v, &segs)
}

#[derive(Clone)]
pub struct FakeJmap {
    pub state: Arc<Mutex<FakeState>>,
    base: String,
}

impl FakeJmap {
    pub async fn start() -> (MockServer, Self) {
        let server = MockServer::start().await;
        let fake = Self {
            state: Arc::new(Mutex::new(FakeState::default())),
            base: server.uri(),
        };
        let base = server.uri();
        Mock::given(method("GET"))
            .and(path("/jmap/session"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "capabilities": {
                    "urn:ietf:params:jmap:core": {
                        "maxSizeUpload": 50000000, "maxConcurrentUpload": 4, "maxSizeRequest": 10000000,
                        "maxConcurrentRequests": 4, "maxCallsInRequest": 16, "maxObjectsInGet": 100,
                        "maxObjectsInSet": 3, "collationAlgorithms": []
                    },
                    "urn:ietf:params:jmap:mail": {}
                },
                "accounts": { "A1": { "name": "me", "isPersonal": true, "isReadOnly": false, "accountCapabilities": {} } },
                "primaryAccounts": { "urn:ietf:params:jmap:mail": "A1" },
                "username": "me@example.com",
                "apiUrl": format!("{base}/api/"),
                "downloadUrl": format!("{base}/download/{{accountId}}/{{blobId}}/{{name}}?type={{type}}"),
                "uploadUrl": format!("{base}/upload/{{accountId}}/"),
                "eventSourceUrl": format!("{base}/events?types={{types}}&closeafter={{closeafter}}&ping={{ping}}"),
                "state": "S1"
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/"))
            .respond_with(fake.clone())
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(wiremock::matchers::path_regex(
                r"^/download/A1/B[^/]+/message\.eml$",
            ))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string("From: x@example.com\r\nSubject: raw\r\n\r\nbody\r\n"),
            )
            .mount(&server)
            .await;
        (server, fake)
    }

    pub fn session_url(&self) -> String {
        format!("{}/jmap/session", self.base)
    }

    pub fn with<T>(&self, f: impl FnOnce(&mut FakeState) -> T) -> T {
        f(&mut self.state.lock().unwrap())
    }

    fn handle(
        &self,
        name: &str,
        mut args: Value,
        prior: &[(String, String, Value)],
    ) -> (String, Value) {
        // Resolve "#foo" back-references.
        if let Some(obj) = args.as_object_mut() {
            let refs: Vec<String> = obj.keys().filter(|k| k.starts_with('#')).cloned().collect();
            for k in refs {
                let r = obj.remove(&k).unwrap();
                let result_of = r["resultOf"].as_str().unwrap();
                let Some((_, _, val)) = prior
                    .iter()
                    .find(|(n, id, _)| id == result_of && n == r["name"].as_str().unwrap())
                else {
                    return ("error".into(), json!({ "type": "invalidResultReference" }));
                };
                obj.insert(
                    k[1..].to_string(),
                    eval_path(val, r["path"].as_str().unwrap()),
                );
            }
        }
        let mut st = self.state.lock().unwrap();
        let st = &mut *st;
        let ids: Option<Vec<String>> = args["ids"].as_array().map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        });
        match name {
            "Mailbox/get" => {
                let ids = ids.unwrap_or_else(|| st.mailboxes.keys().cloned().collect());
                let (list, not_found): (Vec<_>, Vec<_>) = ids
                    .into_iter()
                    .partition(|id| st.mailboxes.contains_key(id));
                let list: Vec<Value> = list.iter().map(|id| st.mailbox_with_counts(id)).collect();
                (
                    name.into(),
                    json!({ "accountId": "A1", "state": st.mailbox_state.to_string(), "list": list, "notFound": not_found }),
                )
            }
            "Mailbox/changes" | "Email/changes" => {
                let since: u64 = args["sinceState"].as_str().unwrap().parse().unwrap();
                let email = name == "Email/changes";
                let current = if email {
                    st.email_state
                } else {
                    st.mailbox_state
                };
                if (email && st.fail_email_changes) || since > current {
                    return ("error".into(), json!({ "type": "cannotCalculateChanges" }));
                }
                let (c, u, d) = if email {
                    FakeState::changes(&st.email_log, since, |id| st.emails.contains_key(id))
                } else {
                    FakeState::changes(&st.mailbox_log, since, |id| st.mailboxes.contains_key(id))
                };
                let mut resp = json!({
                    "accountId": "A1", "oldState": since.to_string(), "newState": current.to_string(),
                    "hasMoreChanges": false, "created": c, "updated": u, "destroyed": d
                });
                if !email {
                    resp["updatedProperties"] = Value::Null;
                }
                (name.into(), resp)
            }
            "Email/get" => {
                let ids = ids.unwrap_or_default();
                let fetch_values = args["fetchTextBodyValues"].as_bool().unwrap_or(false)
                    || args["fetchHTMLBodyValues"].as_bool().unwrap_or(false);
                let mut list = Vec::new();
                let mut not_found = Vec::new();
                for id in ids {
                    match st.emails.get(&id) {
                        Some(e) => {
                            let mut e = filter_props(e, args.get("properties"), &["id"]);
                            if let Some(o) = e.as_object_mut()
                                && !fetch_values
                                && o.contains_key("bodyValues")
                            {
                                o.insert("bodyValues".into(), json!({}));
                            }
                            list.push(e)
                        }
                        None => not_found.push(id),
                    }
                }
                (
                    name.into(),
                    json!({ "accountId": "A1", "state": st.email_state.to_string(), "list": list, "notFound": not_found }),
                )
            }
            "Thread/get" => {
                let ids = ids.unwrap_or_default();
                let mut list = Vec::new();
                let mut not_found = Vec::new();
                for tid in ids {
                    let mut members: Vec<&Value> = st
                        .emails
                        .values()
                        .filter(|e| e["threadId"].as_str() == Some(tid.as_str()))
                        .collect();
                    if members.is_empty() {
                        not_found.push(tid);
                        continue;
                    }
                    members.sort_by(|a, b| a["receivedAt"].as_str().cmp(&b["receivedAt"].as_str()));
                    let email_ids: Vec<&str> =
                        members.iter().map(|e| e["id"].as_str().unwrap()).collect();
                    list.push(json!({ "id": tid, "emailIds": email_ids }));
                }
                (
                    name.into(),
                    json!({ "accountId": "A1", "state": st.email_state.to_string(), "list": list, "notFound": not_found }),
                )
            }
            "Email/query" => {
                let mailbox = args["filter"]["inMailbox"].as_str().unwrap().to_string();
                let all = st.query_ids(&mailbox);
                st.query_snapshots
                    .insert((mailbox, st.email_state), all.clone());
                let pos = args["position"].as_u64().unwrap_or(0) as usize;
                let limit = args["limit"].as_u64().unwrap_or(u64::MAX) as usize;
                let page: Vec<&String> = all.iter().skip(pos).take(limit).collect();
                (
                    name.into(),
                    json!({
                        "accountId": "A1", "queryState": st.email_state.to_string(), "canCalculateChanges": true,
                        "position": pos, "ids": page, "total": all.len()
                    }),
                )
            }
            "Email/queryChanges" => {
                let mailbox = args["filter"]["inMailbox"].as_str().unwrap().to_string();
                let since: u64 = args["sinceQueryState"].as_str().unwrap().parse().unwrap();
                let Some(old) = st.query_snapshots.get(&(mailbox.clone(), since)).cloned() else {
                    return ("error".into(), json!({ "type": "cannotCalculateChanges" }));
                };
                let new = st.query_ids(&mailbox);
                st.query_snapshots
                    .insert((mailbox, st.email_state), new.clone());
                let removed: Vec<&String> = old.iter().filter(|id| !new.contains(id)).collect();
                let added: Vec<Value> = new
                    .iter()
                    .enumerate()
                    .filter(|(_, id)| !old.contains(id))
                    .map(|(i, id)| json!({ "id": id, "index": i }))
                    .collect();
                (
                    name.into(),
                    json!({
                        "accountId": "A1", "oldQueryState": since.to_string(), "newQueryState": st.email_state.to_string(),
                        "total": new.len(), "removed": removed, "added": added
                    }),
                )
            }
            "Email/set" => {
                st.set_calls += 1;
                let old = st.email_state.to_string();
                let mut updated = Map::new();
                let mut not_updated = Map::new();
                for (id, patch) in args["update"].as_object().cloned().unwrap_or_default() {
                    if let Some(kind) = &st.reject_set {
                        not_updated.insert(id, json!({ "type": kind }));
                        continue;
                    }
                    let Some(e) = st.emails.get_mut(&id) else {
                        not_updated.insert(id, json!({ "type": "notFound" }));
                        continue;
                    };
                    for (p, v) in patch.as_object().unwrap() {
                        let (field, key) = match p.split_once('/') {
                            Some((f, k)) => (f, Some(k.replace("~1", "/").replace("~0", "~"))),
                            None => (p.as_str(), None),
                        };
                        match key {
                            Some(k) => {
                                let m = e[field].as_object_mut().unwrap();
                                if v.as_bool() == Some(true) {
                                    m.insert(k, json!(true));
                                } else {
                                    m.remove(&k);
                                }
                            }
                            None => e[field] = v.clone(),
                        }
                    }
                    updated.insert(id.clone(), Value::Null);
                    st.email_updated(&id);
                }
                let mut resp = json!({ "accountId": "A1", "oldState": old, "newState": st.email_state.to_string(), "updated": updated });
                if !not_updated.is_empty() {
                    resp["notUpdated"] = Value::Object(not_updated);
                }
                (name.into(), resp)
            }
            other => (
                "error".into(),
                json!({ "type": "unknownMethod", "description": other }),
            ),
        }
    }
}

impl Respond for FakeJmap {
    fn respond(&self, req: &Request) -> ResponseTemplate {
        if self.state.lock().unwrap().unavailable {
            return ResponseTemplate::new(503);
        }
        let body: Value = serde_json::from_slice(&req.body).unwrap();
        let mut done: Vec<(String, String, Value)> = Vec::new();
        for call in body["methodCalls"].as_array().unwrap() {
            let name = call[0].as_str().unwrap();
            let call_id = call[2].as_str().unwrap().to_string();
            let (resp_name, resp) = self.handle(name, call[1].clone(), &done);
            done.push((resp_name, call_id, resp));
        }
        let responses: Vec<Value> = done
            .into_iter()
            .map(|(n, id, v)| json!([n, v, id]))
            .collect();
        ResponseTemplate::new(200)
            .set_body_json(json!({ "methodResponses": responses, "sessionState": "S1" }))
    }
}
