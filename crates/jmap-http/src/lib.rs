//! JMAP over HTTP (RFC 8620 §2–§7): session discovery, batched method calls,
//! blob download and EventSource push.

mod auth;
mod batch;
mod client;
mod error;
mod sse;

pub use auth::{Auth, AuthFuture, TokenAuth};
pub use batch::{Batch, BatchResponse, CallHandle};
pub use client::{FASTMAIL_SESSION_URL, JmapClient};
pub use error::{Error, Result};
pub use sse::{EventStream, PushEvent, SseEvent, SseParser};

/// Split ids into chunks of at most `max` (e.g. the session's `maxObjectsInGet`).
pub fn chunk_ids<T: Clone>(ids: &[T], max: u64) -> impl Iterator<Item = Vec<T>> + '_ {
    let max = usize::try_from(max).unwrap_or(usize::MAX).max(1);
    ids.chunks(max).map(<[T]>::to_vec)
}
