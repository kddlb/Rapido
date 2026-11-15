//! Read-only check against real Fastmail. Run with:
//! `RAPIDO_FASTMAIL_TOKEN=… cargo test -p jmap-http --test live_fastmail -- --ignored`

use std::sync::Arc;

use jmap_http::{FASTMAIL_SESSION_URL, JmapClient, TokenAuth};
use jmap_proto::capability;
use jmap_proto::mail::*;
use jmap_proto::*;

#[tokio::test]
#[ignore = "needs RAPIDO_FASTMAIL_TOKEN and network"]
async fn fastmail_session_and_mailboxes() {
    let token = std::env::var("RAPIDO_FASTMAIL_TOKEN").expect("set RAPIDO_FASTMAIL_TOKEN");
    let c = JmapClient::connect(FASTMAIL_SESSION_URL, Arc::new(TokenAuth::new(token)))
        .await
        .unwrap();
    let s = c.session();
    assert!(s.has_capability(capability::MAIL));
    let account = s.primary_account(capability::MAIL).unwrap().clone();

    let mut b = c.batch();
    let mb = b.call(MailboxGet(Get::new(account.clone()))).unwrap();
    let q = b
        .call(EmailQuery {
            query: Query {
                limit: Some(5),
                sort: Some(vec![Comparator::desc(EmailSortProperty::ReceivedAt)]),
                ..Query::new(account.clone())
            },
            collapse_threads: true,
        })
        .unwrap();
    let g = b
        .call(EmailGet::new(
            Get::new(account)
                .ids_ref(q.result_ref("/ids"))
                .properties(vec![EmailProperty::Subject, EmailProperty::ReceivedAt]),
        ))
        .unwrap();
    let r = b.send().await.unwrap();
    let mailboxes = r.get(&mb).unwrap().list;
    assert!(mailboxes.iter().any(|m| m.role == Some(MailboxRole::Inbox)));
    let emails = r.get(&g).unwrap().list;
    println!(
        "{} mailboxes; latest {} emails fetched",
        mailboxes.len(),
        emails.len()
    );
}
