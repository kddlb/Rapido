# Rapido

A native desktop mail client written in Rust with [GPUI Kit](https://gpui-kit.com), speaking [JMAP](https://jmap.io/spec/) (RFC 8620 / RFC 8621). Fastmail is the first target.

Work in progress.

## Layout

| Crate | Purpose |
| --- | --- |
| `jmap-proto` | JMAP protocol types (no IO) |
| `jmap-http` | HTTP transport: auth, session, batched calls, blobs, EventSource push |
| `rapido-store` | SQLite cache and sync engine (delta sync, optimistic triage, offline queue) |
| `rapido` | The GPUI Kit app |

## Development

```sh
cargo test --workspace
cargo run -p rapido
# Read-only check against a real Fastmail account:
RAPIDO_FASTMAIL_TOKEN=… cargo test -p jmap-http --test live_fastmail -- --ignored --nocapture
```

## License

MIT, see [LICENSE](LICENSE).
