//! Rapido's local cache (SQLite) and the JMAP sync engine that keeps it fresh.

mod db;
mod error;
mod model;
mod schema;
mod sync;

pub use db::Store;
pub use error::{Error, Result};
pub use model::{
    EmailBody, EmailMeta, MailboxRow, QueryCache, StoreEvent, ThreadRow, mailbox_tree,
};
pub use sync::{PAGE_SIZE, SearchPage, SyncEngine, TriageOp};
