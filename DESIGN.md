# Spate — design decisions

A personal IDM-style download accelerator for macOS (Apple Silicon) with a Brave extension.
Design brief and questionnaire: https://claude.ai/artifact/J5RTRAFSv5igPM9KsQ5J6Y

## Technical (decided)

- **Engine:** own Rust engine (tokio + reqwest), HTTP/1.1 Range requests, one connection per segment.
- **Starting connections:** `clamp(floor(log2(size_MB / 4)), 1, 8)` → <8 MB: 1, 20 MB: 2, 100 MB: 4, 500 MB: 6, ≥1 GB: 8. Minimum piece size 4 MB.
- **Dynamic segmentation:** when a connection frees up, split the active segment with the most *time* left (remaining bytes / its speed) in half; the freed connection takes the back half.
- **Adaptive count (balanced):** add a connection every ~2–3 s while throughput rises ≥10%, cap 8; drop one on 429/503 or a throughput fall; remember the best count per host.
- **Slow/stalled:** EWMA speed per connection; <30% of median for 5 s → steal its back half; no bytes for 15 s → reconnect.
- **Retry:** exponential backoff with jitter, cap 60 s, ~10 tries, honour `Retry-After`. 401/403/410 mid-download → ask the extension for a fresh URL.
- **Disk:** preallocate one `.part` file, `pwrite` at offsets, rename on finish (no merge step).
- **Resume:** sidecar state (segments, ETag, Last-Modified, final URL) flushed atomically ~1 s; resume with `If-Range`.
- **App:** Tauri 2, menu-bar agent started at login; one core shared by app, native-messaging host and `spate` CLI over a Unix socket.
- **Storage:** `~/Library/Application Support/Spate/` (SQLite WAL + settings.toml).
- **Brave:** MV3 extension + native messaging host (no open ports); forwards cookies / referer / UA in memory only; the Brave download is paused while Spate is asked; if Spate doesn't accept within 4 s, Brave resumes and keeps it.
- **Files:** quarantine flag + "Where from" set on downloads; sleep prevented while active.
- **Install:** `make install` → ~/Applications, native-messaging manifest, login item; extension via Load unpacked with pinned key.

## User experience (chosen by the user)

1. **Capture:** take over files ≥ 20 MB or listed types automatically; Option-click keeps it in Brave.
2. **After capture:** small card that auto-starts after 2 s, showing the live per-connection bar, speed graph and ETA.
3. **Home:** menu bar + optional window; plus a notch "island" (borderless window around the notch, expands on hover; pill under the menu bar on Macs without a notch).
4. **Progress:** one coloured piece per connection.
5. **Aggressiveness:** balanced, adaptive 4–8.
6. **Files:** `~/Downloads/Spate/`, Finder tags by type.
7. **Video:** extension-icon badge with a list: thumbnail, title, resolution/quality, duration, estimated size, format, "playing now". No DRM.
8. **Copied links:** paste only (⌘V, or drop on the menu-bar icon). No clipboard watching.

## Build order

1. Engine + CLI
2. Brave extension + native host
3. Menu-bar app + capture card
4. Notch island
5. v2: video (yt-dlp/ffmpeg), scheduler, speed limit, link grabbing, checksums
