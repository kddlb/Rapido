//! A small Server-Sent Events parser and the JMAP EventSource stream.

use std::pin::Pin;
use std::task::{Context, Poll};

use bytes::Bytes;
use futures::{Stream, StreamExt};
use jmap_proto::StateChange;

use crate::{Error, Result};

/// One dispatched SSE event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseEvent {
    pub event: String,
    pub data: String,
    pub id: Option<String>,
}

/// Incremental SSE parser (WHATWG HTML §9.2.6). Feed it arbitrary byte chunks.
#[derive(Debug, Default)]
pub struct SseParser {
    buf: Vec<u8>,
    event: String,
    data: String,
    has_data: bool,
    id: Option<String>,
}

impl SseParser {
    pub fn feed(&mut self, chunk: &[u8]) -> Vec<SseEvent> {
        self.buf.extend_from_slice(chunk);
        let mut out = Vec::new();
        while let Some(pos) = self.buf.iter().position(|&b| b == b'\n' || b == b'\r') {
            // Treat \r\n as one line break.
            let mut end = pos + 1;
            if self.buf[pos] == b'\r' {
                match self.buf.get(pos + 1) {
                    Some(b'\n') => end += 1,
                    None => break, // might be the first half of \r\n
                    _ => {}
                }
            }
            let line: Vec<u8> = self.buf.drain(..end).take(pos).collect();
            let line = String::from_utf8_lossy(&line);
            self.line(&line, &mut out);
        }
        out
    }

    fn line(&mut self, line: &str, out: &mut Vec<SseEvent>) {
        if line.is_empty() {
            if self.has_data {
                out.push(SseEvent {
                    event: if self.event.is_empty() {
                        "message".into()
                    } else {
                        std::mem::take(&mut self.event)
                    },
                    data: std::mem::take(&mut self.data),
                    id: self.id.clone(),
                });
            }
            self.event.clear();
            self.data.clear();
            self.has_data = false;
            return;
        }
        if line.starts_with(':') {
            return;
        }
        let (field, value) = match line.split_once(':') {
            Some((f, v)) => (f, v.strip_prefix(' ').unwrap_or(v)),
            None => (line, ""),
        };
        match field {
            "event" => self.event = value.to_owned(),
            "data" => {
                if self.has_data {
                    self.data.push('\n');
                }
                self.data.push_str(value);
                self.has_data = true;
            }
            "id" if !value.contains('\0') => self.id = Some(value.to_owned()),
            _ => {}
        }
    }
}

/// A JMAP push event.
#[derive(Debug, Clone, PartialEq)]
pub enum PushEvent {
    StateChange(StateChange),
    Ping,
}

/// A live EventSource connection. Ends when the server closes it; reconnecting
/// is the caller's job.
pub struct EventStream {
    inner: Pin<Box<dyn Stream<Item = reqwest::Result<Bytes>> + Send>>,
    parser: SseParser,
    pending: std::collections::VecDeque<SseEvent>,
}

impl EventStream {
    pub(crate) fn new(resp: reqwest::Response) -> Self {
        Self {
            inner: Box::pin(resp.bytes_stream()),
            parser: SseParser::default(),
            pending: Default::default(),
        }
    }
}

fn to_push(ev: SseEvent) -> Option<Result<PushEvent>> {
    match ev.event.as_str() {
        "state" => Some(
            serde_json::from_str(&ev.data)
                .map(PushEvent::StateChange)
                .map_err(Error::from),
        ),
        "ping" => Some(Ok(PushEvent::Ping)),
        _ => None,
    }
}

impl Stream for EventStream {
    type Item = Result<PushEvent>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        loop {
            while let Some(ev) = self.pending.pop_front() {
                if let Some(item) = to_push(ev) {
                    return Poll::Ready(Some(item));
                }
            }
            match self.inner.poll_next_unpin(cx) {
                Poll::Ready(Some(Ok(chunk))) => {
                    let events = self.parser.feed(&chunk);
                    self.pending.extend(events);
                }
                Poll::Ready(Some(Err(e))) => return Poll::Ready(Some(Err(e.into()))),
                Poll::Ready(None) => return Poll::Ready(None),
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_events_split_across_chunks() {
        let mut p = SseParser::default();
        let mut got = p.feed(b": comment\r\nevent: st");
        assert!(got.is_empty());
        got.extend(p.feed(b"ate\r\ndata: {\"a\":\r"));
        got.extend(p.feed(b"\ndata: 1}\r\n\r\nevent: ping\ndata: {}\n\n"));
        assert_eq!(
            got,
            vec![
                SseEvent {
                    event: "state".into(),
                    data: "{\"a\":\n1}".into(),
                    id: None
                },
                SseEvent {
                    event: "ping".into(),
                    data: "{}".into(),
                    id: None
                },
            ]
        );
    }

    #[test]
    fn default_event_name_and_ignores_empty_dispatch() {
        let mut p = SseParser::default();
        assert!(p.feed(b"event: x\n\n").is_empty());
        let got = p.feed(b"id: 7\ndata:hi\n\n");
        assert_eq!(got[0].event, "message");
        assert_eq!(got[0].data, "hi");
        assert_eq!(got[0].id.as_deref(), Some("7"));
    }

    #[test]
    fn maps_jmap_events() {
        let ev = SseEvent {
            event: "state".into(),
            data: r#"{"@type":"StateChange","changed":{"a":{"Email":"s1"}}}"#.into(),
            id: None,
        };
        assert!(matches!(to_push(ev), Some(Ok(PushEvent::StateChange(_)))));
        let other = SseEvent {
            event: "message".into(),
            data: "x".into(),
            id: None,
        };
        assert!(to_push(other).is_none());
    }
}
