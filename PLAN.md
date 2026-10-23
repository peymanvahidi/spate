# Spate — build plan

Claude acts as project manager: it owns this plan, `CONTRACTS.md` and the `spate-proto` crate, reviews each piece and merges it.
Subagents build the pieces, each in its own git worktree/branch off `feat/engine-cli`.

| Phase | Piece | Owner | Directory | Depends on | Status |
|---|---|---|---|---|---|
| 1 | Download engine + `spate get` CLI | agent | `crates/spate-core`, `crates/spate-cli` | contracts | in progress |
| 1 | Brave extension + native host | agent | `extension/`, `crates/spate-host` | contracts | in progress |
| 2 | Daemon: queue, SQLite history, socket server | agent | `crates/spate-daemon` | engine | todo |
| 3 | Menu-bar app, capture card, main window (Tauri 2) | agent | `app/` | daemon | todo |
| 4 | Notch island | agent | `app/` | app | todo |
| 5 | `make install`, end-to-end test in Brave | agent + PM | `Makefile`, `scripts/` | all | todo |
| v2 | Video download (yt-dlp/ffmpeg), scheduler, speed limit, link grabbing, checksums | — | — | — | later |

## Rules for every agent
- Touch only your own directories. Changes to `CONTRACTS.md`, `crates/spate-proto` or the root `Cargo.toml` go through the PM: propose them in your report.
- `cargo build`, `cargo clippy` and `cargo test` must pass for your crates before you report.
- Commit on your branch with clear messages. Do not merge or push.
- Report: what works, how you tested it (real commands + output), what's left, and any contract change you need.

## Build environment
Rust comes from Homebrew's rustup and `~/.cargo` is not writable in the sandbox, so every cargo command runs with:
```
export PATH=/opt/homebrew/opt/rustup/bin:$PATH CARGO_HOME=/Users/heispv/Documents/spate/.cargo-home
```
Network for cargo: `index.crates.io`, `static.crates.io`, `crates.io`.
