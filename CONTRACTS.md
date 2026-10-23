# Spate — contracts between the pieces

Owned by the PM. The Rust types for everything below live in `crates/spate-proto`; that crate is the source of truth.

## Paths and names

| What | Value |
|---|---|
| App bundle id | `com.spate.app` |
| Data dir | `~/Library/Application Support/Spate/` |
| Daemon socket | `~/Library/Application Support/Spate/spate.sock` (mode 0600) |
| Database | `~/Library/Application Support/Spate/spate.sqlite` |
| Settings | `~/Library/Application Support/Spate/settings.toml` |
| Logs | `~/Library/Logs/Spate/` |
| Default download dir | `~/Downloads/Spate/` |
| Partial file | `<dir>/<filename>.spate-part` |
| Resume state | `<dir>/<filename>.spate-part.json` |
| Native messaging host name | `com.spate.host` |
| Host manifest (Brave) | `~/Library/Application Support/BraveSoftware/Brave-Browser/NativeMessagingHosts/com.spate.host.json` |

## Engine API (`spate-core`)

```rust
spate_core::run(job: Job, progress: Arc<Mutex<Snapshot>>, cancel: tokio::sync::watch::Receiver<bool>) -> anyhow::Result<PathBuf>
```
- `Job` and `Snapshot` are defined in `spate-proto` (`Job`, `JobView` is built from `Snapshot` by the daemon).
- Cancel = pause: state is saved; calling `run` again with the same job resumes.
- On success returns the final path (already renamed, de-duplicated as `name (1).ext`, tagged, quarantined).

## Daemon IPC (Unix socket, newline-delimited JSON)

Request: `{"id": 1, "method": "add", "params": {...}}` → Response: `{"id": 1, "result": ...}` or `{"id": 1, "error": {"code": "...", "message": "..."}}`.

| Method | Params | Result |
|---|---|---|
| `ping` | — | `{"version": "0.1.0"}` |
| `add` | `AddRequest` | `{"job_id": "<uuid>"}` |
| `list` | — | `[JobView]` |
| `pause` / `resume` / `cancel` / `remove` | `{"job_id"}` | `{}` |
| `subscribe` | — | `{}` then pushes `{"event": "job", "job": JobView}` lines (≈4/s per running job) until the client disconnects |

`AddRequest`: `{url, headers: [[name, value]], filename?, referrer?, size_hint?, mime?, source: "brave" | "cli" | "paste", kind: "file" | "media"}`.
`kind: "media"` is accepted but answered with error code `not_supported` until v2.

`JobView`: `{id, url, filename, path?, state: "queued"|"running"|"paused"|"done"|"failed", total?, downloaded, speed_bps, eta_secs?, connections, segments: [{start, end, pos}], error?, created_at, finished_at?}`.

## Native messaging (extension ↔ `spate-host`)

Standard Chrome framing (4-byte little-endian length + JSON). One request → one response.

| Extension sends | Host answers |
|---|---|
| `{"type": "ping"}` | `{"ok": true, "version": "0.1.0"}` |
| `{"type": "add", ...AddRequest}` | `{"ok": true, "job_id"}` or `{"ok": false, "error": "app_not_running" \| "<message>"}` |

The host forwards to the daemon socket. If the socket isn't there, it runs `open -b com.spate.app`, waits up to 1.5 s for the socket, then gives up with `app_not_running`.

## Capture flow (extension)

1. `downloads.onCreated` → apply filters (≥ threshold, default 20 MB, or extension in the type list; never `blob:`/`data:`; skip downloads the extension started itself).
2. `downloads.pause(id)`, collect cookies (`cookies.getAll`), referrer, user agent.
3. Send `add`. On `ok: true` → `downloads.cancel` + `downloads.erase`. On anything else, or no answer in 2 s → `downloads.resume` (Brave keeps it). Nothing is ever lost.
4. Holding Option (Alt) while clicking a link keeps that download in Brave.
5. Cookies are never logged or stored.
