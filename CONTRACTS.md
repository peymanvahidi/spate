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
| Host manifest | `~/Library/Application Support/Google/Chrome/NativeMessagingHosts/com.spate.host.json` (this is where Brave 1.96 on macOS actually looks) and, for older/newer Brave builds, `~/Library/Application Support/BraveSoftware/Brave-Browser/NativeMessagingHosts/com.spate.host.json` |

## Engine API (`spate-core`)

```rust
spate_core::run(job: Job, progress: Arc<Mutex<Snapshot>>, cancel: tokio::sync::watch::Receiver<bool>) -> anyhow::Result<PathBuf>
```
- `Job` and `Snapshot` are defined in `spate-proto` (`Job`, `JobView` is built from `Snapshot` by the daemon).
- Cancel = pause: state is saved; calling `run` again with the same job resumes.
- On success returns the final path (already renamed, de-duplicated as `name (1).ext`, tagged, quarantined).
- Errors the caller must tell apart (downcast the `anyhow::Error`): `spate_core::Paused` (cancel was requested; state saved) and `spate_core::LinkExpired { status }` (401/403/410: ask the extension for a fresh URL). Anything else is a failure.
- The "Where from" referrer is taken from a `Referer` entry in `job.headers`.
- `SegmentView` ranges are `[start, end)`.

## Daemon IPC (Unix socket, newline-delimited JSON)

Request: `{"id": 1, "method": "add", "params": {...}}` → Response: `{"id": 1, "result": ...}` or `{"id": 1, "error": {"code": "...", "message": "..."}}`.

| Method | Params | Result |
|---|---|---|
| `ping` | — | `{"version": "0.1.0"}` |
| `add` | `AddRequest` | `{"job_id": "<uuid>"}` |
| `list` | — | `[JobView]` |
| `pause` / `resume` / `cancel` / `remove` | `{"job_id"}` | `{}` |
| `set_path` | `{"job_id", "path"}` | `{}`: records that a finished job's file was moved (absolute path to an existing regular file; job must be `done`). Updates `path`/`filename`/`dir`, persists, pushes one `job` event. Never touches the file. |
| `subscribe` | — | `{}` then pushes `{"event": "job", "job": JobView}` lines (≈4/s per running job, plus one on every state change) and `{"event": "removed", "job_id"}` after `remove`, until the client disconnects |

`AddRequest`: `{url, headers: [[name, value]], filename?, referrer?, size_hint?, mime?, source: "brave" | "cli" | "paste", kind: "file" | "media"}`.
`kind: "media"` is accepted but answered with error code `not_supported` until v2.

Error codes: `not_supported`, `unknown_method`, `bad_request`, `not_found` (no such job), `bad_state` (e.g. pause a finished job, remove a running one), `internal`, `link_expired` (in a failed job's `error`).

`subscribe` first pushes the current state of every job right after `{}`, so a new window starts complete.

`JobView`: `{id, url, filename, path?, state: "queued"|"running"|"paused"|"done"|"failed", total?, downloaded, speed_bps, eta_secs?, connections, segments: [{start, end, pos, active}], error?, created_at, finished_at?}`. Segment ranges are `[start, end)`. `source` (`"brave"|"cli"|"paste"`, may be absent for old jobs) tells the app which jobs get a capture card.

`cancel` stops the job, deletes its `.spate-part` and state files, and leaves it in the history as `state: "failed", error: "cancelled"`. `remove` drops a finished or failed job from the history (never deletes a finished file) and emits `removed`.

## Native messaging (extension ↔ `spate-host`)

Standard Chrome framing (4-byte little-endian length + JSON). One request → one response.

| Extension sends | Host answers |
|---|---|
| `{"type": "ping"}` | `{"ok": true, "version": "0.1.0"}` |
| `{"type": "add", ...AddRequest}` | `{"ok": true, "job_id"}` or `{"ok": false, "error": "app_not_running" \| "<message>"}` |

The host forwards to the daemon socket. If the socket isn't there, it runs `open -g -b com.spate.app`, waits up to 1.5 s for the socket, then gives up with `app_not_running`.

Error strings the extension can see: `app_not_running`, `<daemon code>: <message>` (e.g. `not_supported: …`), `bad_request: …`, `unknown_type: …`, `timeout: daemon did not answer` (10 s), `io_error: …` from the host; `timeout` and `host_missing` from the extension itself.

Extension ID (pinned by `key` in `extension/manifest.json`): `akcgnbcpioinnkkmoobjdgnnmebipnil`.

## Capture flow (extension)

1. `downloads.onCreated` → apply filters (≥ threshold, default 20 MB, or extension in the type list; never `blob:`/`data:`; skip downloads the extension started itself).
2. `downloads.pause(id)`, collect cookies (`cookies.getAll`), referrer, user agent.
3. Send `add`. On `ok: true` → `downloads.cancel` + `downloads.erase`. On anything else, or no answer in 4 s → `downloads.resume` (Brave keeps it). Nothing is ever lost.
4. Holding Option (Alt) while clicking a link keeps that download in Brave.
5. Cookies are never logged or stored.
