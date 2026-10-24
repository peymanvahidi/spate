# Spate

Spate is a download accelerator for macOS. It splits a download into several ranges, fetches
them over parallel connections, and resumes where it stopped after a pause, a crash or a
reboot. It lives in the menu bar, shows the current speed there, and keeps a history of what
it downloaded.

A Brave extension hands large downloads over to Spate, with the cookies and referrer of the
page, so downloads that need a login work too. There is also a `spate` command-line tool that
talks to the same app.

## Install

Needs macOS 13 or later, Rust (via rustup), the Tauri CLI and Node (for `make test` only).

```
make install
```

This builds a release, then:

- quits a running Spate first (its downloads are paused and kept),
- installs `~/Applications/Spate.app` (ad-hoc signed, so it runs on Apple Silicon),
- installs the native messaging host in `~/Library/Application Support/Spate/bin/spate-host`
  and registers it with Brave,
- links the `spate` CLI as `~/.local/bin/spate` (add `~/.local/bin` to your `PATH` if it isn't).

No sudo is needed. Then:

1. Open `brave://extensions` and turn on **Developer mode**.
2. **Load unpacked** and choose the `extension/` folder of this repository.
3. Check that the extension ID is `akcgnbcpioinnkkmoobjdgnnmebipnil`.
4. Launch Spate once: `open ~/Applications/Spate.app`. From then on it starts at login
   (toggle **Start at Login** in the menu-bar menu).

**Update:** `git pull && make install`. The running app is quit and started again; paused and
queued downloads are kept. Reload the extension in `brave://extensions` if it changed.

**Uninstall:** `make uninstall` removes the app, the host, the Brave manifest, the CLI link
and the login item. Your history, settings, logs and `~/Downloads/Spate` stay;
`make uninstall PURGE=1` removes them too. Remove the extension in `brave://extensions`.

Other targets: `make build`, `make test`, `make dev` (runs the app from source).

## How capture works

When Brave starts a download, the extension pauses it and offers it to Spate if it is at least
20 MB or has one of the listed file types (both set in the extension's options). If Spate
accepts it, Brave's copy is cancelled; if Spate isn't reachable within a few seconds, Brave
resumes and keeps it. Nothing is lost either way.

- **Option-click** a link to keep that download in Brave.
- **Right-click** a link → **Download with Spate** to send any link, whatever its size.
- **Paste a link** in the field at the top of the menu-bar dropdown.

A small card appears for each captured download, where you can pick another folder. The
download also shows in the notch island while it runs.

## Command line

```
spate add <url>        queue a download in the app
spate ls               list downloads
spate watch            live view
spate pause|resume|cancel|rm <id>   (a unique ID prefix is enough)
spate get <url>        download in this process, without the app
```

The commands other than `get` need the app to be running.

## Where things live

| What | Where |
|---|---|
| Downloads | `~/Downloads/Spate/` (unfinished: `<name>.spate-part` + `.spate-part.json`) |
| History, settings, socket | `~/Library/Application Support/Spate/` |
| Settings | `~/Library/Application Support/Spate/settings.toml` |
| Logs | `~/Library/Logs/Spate/` |
| App | `~/Applications/Spate.app` |
| Login item | `~/Library/LaunchAgents/Spate.plist` |

Finished files get the usual "Where from" information and quarantine flag, like a download
made by the browser.

## Privacy

- Cookies (and `Authorization` headers) passed by the extension are kept in memory only. They
  are never written to disk or logged. After Spate restarts, an unfinished download has lost
  them; if the server then refuses, start it again from the browser.
- Logs record host names, not full URLs, query strings or headers.
- Spate makes no network requests of its own: no telemetry, no update checks.

## Known limitations

- Video and streaming downloads (HLS/DASH, YouTube) are planned for v2; the extension detects
  them but Spate answers "not supported".
- There is no settings screen yet. "Settings" in the menu opens `settings.toml`
  (`download_dir`, `max_concurrent`, `max_connections`) in TextEdit; restart Spate after
  editing it.
- Only Brave is supported. The app is built locally and ad-hoc signed, not notarized.

## Repository layout

| Path | What |
|---|---|
| `crates/spate-proto` | Types shared by everything (jobs, IPC messages, paths) |
| `crates/spate-core` | Download engine: segmented, resumable HTTP downloads |
| `crates/spate-daemon` | Queue, SQLite history, Unix-socket server |
| `crates/spate-cli` | The `spate` command |
| `crates/spate-host` | Brave native messaging host |
| `app/src-tauri` | The macOS app (Tauri 2): menu bar, windows, runs the daemon in-process |
| `app/ui` | The app's web UI (plain HTML/CSS/JS) |
| `extension/` | The Brave extension (Manifest V3, no build step) |
| `scripts/` | Install scripts used by the `Makefile` |
