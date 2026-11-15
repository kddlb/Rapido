mod support;

use std::sync::Arc;

use jmap_http::{JmapClient, TokenAuth};
use jmap_proto::Id;
use rapido_store::*;
use support::FakeJmap;

async fn setup() -> (wiremock::MockServer, FakeJmap, SyncEngine) {
    let (server, fake) = FakeJmap::start().await;
    fake.with(|s| {
        s.add_mailbox("inbox", "Inbox", Some("inbox"));
        s.add_mailbox("archive", "Archive", Some("archive"));
        s.add_mailbox("trash", "Trash", Some("trash"));
        s.add_email("m1", "t1", "inbox", 1, true);
        s.add_email("m2", "t1", "inbox", 2, false);
        s.add_email("m3", "t2", "inbox", 3, true);
        s.add_email("m4", "t3", "archive", 4, true);
    });
    let client = JmapClient::connect(fake.session_url(), Arc::new(TokenAuth::new("t")))
        .await
        .unwrap();
    let engine = SyncEngine::new(
        Arc::new(client),
        Arc::new(Store::open_in_memory().unwrap()),
        "A1".into(),
    );
    (server, fake, engine)
}

fn inbox() -> Id {
    "inbox".into()
}

fn list_ids(engine: &SyncEngine, mb: &Id) -> Vec<String> {
    engine
        .store()
        .thread_list(mb)
        .unwrap()
        .into_iter()
        .map(|r| r.email_id.to_string())
        .collect()
}

#[tokio::test]
async fn initial_sync_loads_mailboxes_and_threads() {
    let (_s, _fake, engine) = setup().await;
    engine.sync_mailboxes().await.unwrap();
    let tree = mailbox_tree(engine.store().mailboxes().unwrap());
    assert_eq!(tree.len(), 3);
    let inbox_row = tree.iter().find(|m| m.id == inbox()).unwrap();
    assert_eq!(inbox_row.total_emails, 3);
    assert_eq!(inbox_row.unread_emails, 1);

    engine.load_mailbox(&inbox(), 0, PAGE_SIZE).await.unwrap();
    let rows = engine.store().thread_list(&inbox()).unwrap();
    assert_eq!(rows.len(), 2, "two threads in inbox");
    assert_eq!(rows[0].email_id, Id::from("m3"), "newest first");
    assert_eq!(rows[1].thread_id, Id::from("t1"));
    assert_eq!(rows[1].count, 2);
    assert!(rows[1].unread);
    assert!(!rows[0].unread);
    assert_eq!(
        engine.store().query(&inbox()).unwrap().unwrap().total,
        Some(2)
    );

    let thread = engine.load_thread(&"t1".into()).await.unwrap();
    assert_eq!(
        thread.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(),
        ["m1", "m2"]
    );
}

#[tokio::test]
async fn paging_appends() {
    let (_s, _fake, engine) = setup().await;
    engine.load_mailbox(&inbox(), 0, 1).await.unwrap();
    assert_eq!(list_ids(&engine, &inbox()), ["m3"]);
    engine.load_mailbox(&inbox(), 1, 1).await.unwrap();
    assert_eq!(list_ids(&engine, &inbox()), ["m3", "m2"]);
}

#[tokio::test]
async fn delta_sync_applies_server_changes() {
    let (_s, fake, engine) = setup().await;
    engine.sync_mailboxes().await.unwrap();
    engine.load_mailbox(&inbox(), 0, PAGE_SIZE).await.unwrap();
    let mut events = engine.subscribe();

    fake.with(|s| {
        s.add_email("m5", "t4", "inbox", 5, false); // new thread on top
        s.set_keyword("m2", "$seen", true); // t1 becomes read
        s.destroy_email("m3");
    });
    engine.sync_all().await.unwrap();

    assert_eq!(list_ids(&engine, &inbox()), ["m5", "m2"]);
    let rows = engine.store().thread_list(&inbox()).unwrap();
    assert!(rows[0].unread);
    assert!(!rows[1].unread, "flag update arrived via Email/changes");
    assert!(engine.store().email(&"m3".into()).unwrap().is_none());
    let inbox_row = engine
        .store()
        .mailboxes()
        .unwrap()
        .into_iter()
        .find(|m| m.id == inbox())
        .unwrap();
    assert_eq!(inbox_row.total_emails, 3);

    let mut saw_list = false;
    while let Ok(ev) = events.try_recv() {
        saw_list |= ev == StoreEvent::ListChanged(inbox());
    }
    assert!(saw_list);
}

#[tokio::test]
async fn new_email_in_existing_thread_replaces_representative() {
    let (_s, fake, engine) = setup().await;
    engine.load_mailbox(&inbox(), 0, PAGE_SIZE).await.unwrap();
    fake.with(|s| s.add_email("m6", "t1", "inbox", 6, false));
    engine.sync_all().await.unwrap();
    assert_eq!(list_ids(&engine, &inbox()), ["m6", "m3"]);
    assert_eq!(engine.store().thread_list(&inbox()).unwrap()[0].count, 3);
}

#[tokio::test]
async fn cannot_calculate_changes_falls_back_to_resync() {
    let (_s, fake, engine) = setup().await;
    engine.load_mailbox(&inbox(), 0, PAGE_SIZE).await.unwrap();
    fake.with(|s| {
        s.fail_email_changes = true;
        s.set_keyword("m2", "$seen", true);
        s.add_email("m7", "t5", "inbox", 7, true);
    });
    engine.sync_emails().await.unwrap();
    assert_eq!(list_ids(&engine, &inbox()), ["m7", "m3", "m2"]);
    assert!(
        engine
            .store()
            .email(&"m2".into())
            .unwrap()
            .unwrap()
            .is_seen()
    );
}

#[tokio::test]
async fn triage_is_optimistic_and_reaches_server() {
    let (_s, fake, engine) = setup().await;
    engine.sync_mailboxes().await.unwrap();
    engine.load_mailbox(&inbox(), 0, PAGE_SIZE).await.unwrap();

    engine
        .apply(&["m2".into()], TriageOp::SetSeen(true))
        .await
        .unwrap();
    assert!(
        engine
            .store()
            .email(&"m2".into())
            .unwrap()
            .unwrap()
            .is_seen()
    );
    assert_eq!(fake.with(|s| s.keywords("m2")), ["$seen"]);

    engine
        .apply(&["m3".into()], TriageOp::Archive { from: inbox() })
        .await
        .unwrap();
    assert_eq!(
        list_ids(&engine, &inbox()),
        ["m2"],
        "archived row hidden at once"
    );
    assert_eq!(fake.with(|s| s.mailbox_ids("m3")), ["archive"]);

    engine
        .apply(&["m1".into(), "m2".into()], TriageOp::Trash)
        .await
        .unwrap();
    assert_eq!(fake.with(|s| s.mailbox_ids("m1")), ["trash"]);
    assert!(list_ids(&engine, &inbox()).is_empty());

    // The server-side change log then flows back without disturbing anything.
    engine.sync_all().await.unwrap();
    assert!(list_ids(&engine, &inbox()).is_empty());
}

#[tokio::test]
async fn triage_chunks_by_max_objects_in_set() {
    let (_s, fake, engine) = setup().await;
    engine.load_mailbox(&inbox(), 0, PAGE_SIZE).await.unwrap();
    fake.with(|s| s.add_email("m8", "t6", "inbox", 8, false));
    engine.sync_all().await.unwrap();
    let ids: Vec<Id> = ["m1", "m2", "m3", "m8"].map(Id::from).to_vec();
    engine
        .apply(&ids, TriageOp::SetFlagged(true))
        .await
        .unwrap();
    assert_eq!(fake.with(|s| s.set_calls), 2, "maxObjectsInSet is 3");
    assert!(
        fake.with(|s| s.keywords("m8"))
            .contains(&"$flagged".to_string())
    );
}

#[tokio::test]
async fn rejected_triage_rolls_back() {
    let (_s, fake, engine) = setup().await;
    engine.load_mailbox(&inbox(), 0, PAGE_SIZE).await.unwrap();
    fake.with(|s| s.reject_set = Some("forbidden".into()));
    let err = engine
        .apply(&["m2".into()], TriageOp::SetFlagged(true))
        .await
        .unwrap_err();
    assert!(
        matches!(err, Error::Rejected(ref r) if r.len() == 1),
        "{err:?}"
    );
    assert!(
        !engine
            .store()
            .email(&"m2".into())
            .unwrap()
            .unwrap()
            .is_flagged()
    );
}

#[tokio::test]
async fn offline_triage_is_queued_then_flushed() {
    let (_s, fake, engine) = setup().await;
    engine.load_mailbox(&inbox(), 0, PAGE_SIZE).await.unwrap();
    fake.with(|s| s.unavailable = true);
    engine
        .apply(&["m2".into()], TriageOp::SetFlagged(true))
        .await
        .unwrap();
    assert!(
        engine
            .store()
            .email(&"m2".into())
            .unwrap()
            .unwrap()
            .is_flagged(),
        "kept locally"
    );
    assert_eq!(engine.store().pending_ops().unwrap().len(), 1);

    // Still down: flushing keeps the op.
    assert!(engine.flush_pending().await.unwrap_err().is_transient());
    assert_eq!(engine.store().pending_ops().unwrap().len(), 1);

    fake.with(|s| s.unavailable = false);
    engine.sync_all().await.unwrap();
    assert!(engine.store().pending_ops().unwrap().is_empty());
    assert!(
        fake.with(|s| s.keywords("m2"))
            .contains(&"$flagged".to_string())
    );
    assert!(
        engine
            .store()
            .email(&"m2".into())
            .unwrap()
            .unwrap()
            .is_flagged()
    );
}

#[tokio::test]
async fn body_is_fetched_once_and_cached() {
    let (_s, _fake, engine) = setup().await;
    engine.load_mailbox(&inbox(), 0, PAGE_SIZE).await.unwrap();
    let b = engine.body(&"m1".into()).await.unwrap();
    assert_eq!(b.plain_text().as_deref(), Some("Body of m1"));
    assert_eq!(b.html().as_deref(), Some("<p>Body of m1</p>"));
    assert!(engine.store().body(&"m1".into()).unwrap().is_some());
}

#[tokio::test]
async fn store_persists_on_disk() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.sqlite");
    {
        let s = Store::open(&path).unwrap();
        s.set_state("Email", "42").unwrap();
    }
    let s = Store::open(&path).unwrap();
    assert_eq!(s.state("Email").unwrap().as_deref(), Some("42"));
}

#[tokio::test]
async fn headers_and_raw_source() {
    let (_s, _fake, engine) = setup().await;
    engine.load_mailbox(&inbox(), 0, PAGE_SIZE).await.unwrap();
    let headers = engine.headers(&"m1".into()).await.unwrap();
    assert_eq!(headers.len(), 3);
    assert_eq!(headers[0].name, "Received");
    assert!(headers[0].value.contains("\r\n\tby"), "raw folding kept");
    let raw = engine.raw_message(&"m1".into()).await.unwrap();
    assert!(raw.starts_with(b"From: x@example.com"));
}
