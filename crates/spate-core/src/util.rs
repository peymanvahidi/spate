//! Pure helpers: connection-count rule, split selection, filename parsing, de-duplication, tags.

use std::path::{Path, PathBuf};
use std::time::Instant;

pub const MIB: u64 = 1024 * 1024;
/// Smallest piece a segment may be split into.
pub const MIN_PIECE: u64 = 4 * MIB;
/// Split points are aligned to this.
pub const ALIGN: u64 = 64 * 1024;

/// `clamp(floor(log2(size_MiB / 4)), 1, 8)`, capped by `max` and by how many min-size pieces fit.
pub fn initial_connections(size: Option<u64>, max: usize) -> usize {
    let max = max.max(1);
    let Some(size) = size else { return 1 };
    let quarter_mb = size as f64 / MIB as f64 / 4.0;
    let n = if quarter_mb < 2.0 { 1 } else { quarter_mb.log2().floor() as usize };
    let pieces = (size / MIN_PIECE).max(1) as usize;
    n.clamp(1, 8).min(max).min(pieces)
}

/// Exponential backoff with full jitter: 1 s · 2^(n-1), capped at 60 s, then a random factor in [0.5, 1].
pub fn backoff(attempt: u32) -> std::time::Duration {
    let base = (1u64 << attempt.saturating_sub(1).min(6)).min(60) as f64;
    let jitter = 0.5 + rand::random::<f64>() * 0.5;
    std::time::Duration::from_secs_f64(base * jitter)
}

/// The view of a segment that split selection needs.
#[derive(Debug, Clone)]
pub struct SegInfo {
    pub pos: u64,
    pub end: u64,
    pub done: bool,
    pub active: bool,
    pub speed: f64,
    pub not_before: Option<Instant>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pick {
    /// Pick up this inactive unfinished segment.
    Resume(usize),
    /// Split segment `idx` at `at`; the new connection takes `[at, old_end)`.
    Split(usize, u64),
}

/// What a freed connection should do: first any inactive unfinished segment (lowest index first),
/// otherwise split the active segment with the most time left (remaining / speed), if it is
/// at least `2 * min_piece` long.
pub fn pick(segs: &[SegInfo], now: Instant, min_piece: u64, can_split: bool) -> Option<Pick> {
    if let Some(i) = segs
        .iter()
        .position(|s| !s.done && !s.active && s.not_before.is_none_or(|t| t <= now))
    {
        return Some(Pick::Resume(i));
    }
    if !can_split {
        return None;
    }
    let mut best: Option<(usize, f64)> = None;
    for (i, s) in segs.iter().enumerate() {
        if !s.active || s.done {
            continue;
        }
        let remaining = s.end.saturating_sub(s.pos);
        if remaining < 2 * min_piece {
            continue;
        }
        let time_left = remaining as f64 / s.speed.max(1.0);
        if best.is_none_or(|(_, t)| time_left > t) {
            best = Some((i, time_left));
        }
    }
    let (i, _) = best?;
    let s = &segs[i];
    let at = split_point(s.pos, s.end)?;
    Some(Pick::Split(i, at))
}

/// Midpoint of `[pos, end)` rounded up to `ALIGN`, if it lies strictly inside.
pub fn split_point(pos: u64, end: u64) -> Option<u64> {
    let mid = pos + (end - pos) / 2;
    let at = mid.div_ceil(ALIGN) * ALIGN;
    (at > pos && at < end).then_some(at)
}

/// Parse a Content-Disposition header. `filename*` (RFC 5987) wins over `filename`.
pub fn filename_from_content_disposition(v: &str) -> Option<String> {
    let mut plain = None;
    let mut extended = None;
    for part in split_params(v) {
        let Some((k, val)) = part.split_once('=') else { continue };
        let k = k.trim().to_ascii_lowercase();
        let val = val.trim();
        if k == "filename*" {
            let val = unquote(val);
            let mut it = val.splitn(3, '\'');
            let (Some(charset), Some(_lang), Some(enc)) = (it.next(), it.next(), it.next()) else {
                continue;
            };
            let bytes: Vec<u8> = percent_encoding::percent_decode_str(enc).collect();
            let s = if charset.eq_ignore_ascii_case("utf-8") {
                String::from_utf8_lossy(&bytes).into_owned()
            } else {
                // ISO-8859-1 and friends: bytes map 1:1 to code points.
                bytes.iter().map(|&b| b as char).collect()
            };
            if !s.is_empty() {
                extended = Some(s);
            }
        } else if k == "filename" {
            let s = unquote(val);
            if !s.is_empty() {
                plain = Some(s);
            }
        }
    }
    extended.or(plain)
}

fn split_params(v: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    let mut escape = false;
    for c in v.chars() {
        if escape {
            cur.push(c);
            escape = false;
            continue;
        }
        match c {
            '\\' if quoted => {
                cur.push(c);
                escape = true;
            }
            '"' => {
                quoted = !quoted;
                cur.push(c);
            }
            ';' if !quoted => out.push(std::mem::take(&mut cur)),
            _ => cur.push(c),
        }
    }
    out.push(cur);
    out
}

fn unquote(v: &str) -> String {
    let v = v.trim();
    if v.len() >= 2 && v.starts_with('"') && v.ends_with('"') {
        let inner = &v[1..v.len() - 1];
        let mut out = String::new();
        let mut esc = false;
        for c in inner.chars() {
            if esc {
                out.push(c);
                esc = false;
            } else if c == '\\' {
                esc = true;
            } else {
                out.push(c);
            }
        }
        out
    } else {
        v.to_string()
    }
}

/// Last non-empty path segment of a URL, percent-decoded.
pub fn filename_from_url(url: &str) -> Option<String> {
    let u = reqwest::Url::parse(url).ok()?;
    let last = u.path_segments()?.rev().find(|s| !s.is_empty())?;
    let s = percent_encoding::percent_decode_str(last).decode_utf8_lossy().into_owned();
    (!s.is_empty()).then_some(s)
}

/// Make a name safe for the file system: no separators, colons or control characters,
/// no leading dots, at most 255 bytes. Falls back to "download".
pub fn sanitize_filename(name: &str) -> String {
    // Keep only the last path component if someone sent a path.
    let name = name.rsplit(['/', '\\']).next().unwrap_or("");
    let mut s: String = name
        .chars()
        .map(|c| if c == ':' || c.is_control() { '_' } else { c })
        .collect();
    s = s.trim().trim_start_matches('.').trim_end_matches(['.', ' ']).to_string();
    if s.len() > 255 {
        let (stem, ext) = split_ext(&s);
        let ext = if ext.len() > 32 { "" } else { ext };
        let mut cut = 255 - ext.len();
        while !stem.is_char_boundary(cut.min(stem.len())) {
            cut -= 1;
        }
        s = format!("{}{}", &stem[..cut.min(stem.len())], ext);
    }
    if s.is_empty() {
        "download".to_string()
    } else {
        s
    }
}

/// Split "name.ext" into ("name", ".ext"); keeps ".tar.gz"-style double extensions together.
pub fn split_ext(name: &str) -> (&str, &str) {
    let lower = name.to_ascii_lowercase();
    for double in [".tar.gz", ".tar.bz2", ".tar.xz", ".tar.zst", ".tar.lz"] {
        if lower.ends_with(double) && name.len() > double.len() {
            let i = name.len() - double.len();
            return (&name[..i], &name[i..]);
        }
    }
    match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    }
}

/// First of `name`, `name (1).ext`, `name (2).ext`, … for which `taken` is false.
pub fn dedup_name(name: &str, taken: impl Fn(&str) -> bool) -> String {
    if !taken(name) {
        return name.to_string();
    }
    let (stem, ext) = split_ext(name);
    (1..)
        .map(|i| format!("{stem} ({i}){ext}"))
        .find(|c| !taken(c))
        .expect("infinite iterator")
}

/// Final path in `dir` that does not exist yet.
pub fn dedup_path(dir: &Path, name: &str) -> PathBuf {
    dir.join(dedup_name(name, |n| dir.join(n).symlink_metadata().is_ok()))
}

/// Finder tag for a file name, by extension.
pub fn finder_tag(name: &str) -> Option<&'static str> {
    let (_, ext) = split_ext(name);
    let ext = ext.trim_start_matches('.').to_ascii_lowercase();
    let ext = ext.rsplit('.').next().unwrap_or("");
    Some(match ext {
        "mp4" | "mkv" | "mov" | "avi" | "webm" | "m4v" | "flv" | "wmv" | "mpg" | "mpeg" | "ts" | "3gp" => {
            "Video"
        }
        "mp3" | "m4a" | "aac" | "flac" | "wav" | "ogg" | "opus" | "wma" | "aiff" | "aif" | "alac" => "Audio",
        "zip" | "rar" | "7z" | "tar" | "gz" | "tgz" | "bz2" | "xz" | "zst" | "tbz2" | "txz" | "lz" => {
            "Archive"
        }
        "dmg" | "pkg" | "mpkg" | "app" | "iso" => "App",
        "pdf" | "doc" | "docx" | "xls" | "xlsx" | "ppt" | "pptx" | "txt" | "rtf" | "epub" | "md" | "csv"
        | "odt" | "ods" | "pages" | "numbers" | "key" => "Document",
        "jpg" | "jpeg" | "png" | "gif" | "webp" | "heic" | "heif" | "tif" | "tiff" | "bmp" | "svg" | "avif" => {
            "Image"
        }
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connection_rule() {
        let mb = |n: u64| Some(n * MIB);
        assert_eq!(initial_connections(None, 8), 1);
        assert_eq!(initial_connections(mb(1), 8), 1);
        assert_eq!(initial_connections(mb(7), 8), 1);
        assert_eq!(initial_connections(mb(8), 8), 1);
        assert_eq!(initial_connections(mb(16), 8), 2);
        assert_eq!(initial_connections(mb(20), 8), 2);
        assert_eq!(initial_connections(mb(100), 8), 4);
        assert_eq!(initial_connections(mb(500), 8), 6);
        assert_eq!(initial_connections(mb(1024), 8), 8);
        assert_eq!(initial_connections(mb(50_000), 8), 8);
        assert_eq!(initial_connections(mb(1024), 3), 3);
        assert_eq!(initial_connections(mb(1024), 0), 1);
    }

    fn seg(pos: u64, end: u64, active: bool, speed: f64) -> SegInfo {
        SegInfo { pos, end, done: pos >= end, active, speed, not_before: None }
    }

    #[test]
    fn pick_prefers_inactive_unfinished() {
        let now = Instant::now();
        let segs = vec![seg(0, 100 * MIB, true, 1e6), seg(100 * MIB, 200 * MIB, false, 0.0)];
        assert_eq!(pick(&segs, now, MIN_PIECE, true), Some(Pick::Resume(1)));
        // ...unless it is waiting for a retry.
        let mut segs = segs;
        segs[1].not_before = Some(now + std::time::Duration::from_secs(5));
        assert!(matches!(pick(&segs, now, MIN_PIECE, true), Some(Pick::Split(0, _))));
    }

    #[test]
    fn pick_splits_most_time_left() {
        let now = Instant::now();
        // seg 0: 40 MiB left at 10 MB/s = 4 s; seg 1: 20 MiB left at 1 MB/s = 20 s.
        let segs = vec![seg(0, 40 * MIB, true, 10e6), seg(40 * MIB, 60 * MIB, true, 1e6)];
        match pick(&segs, now, MIN_PIECE, true) {
            Some(Pick::Split(1, at)) => {
                assert_eq!(at % ALIGN, 0);
                assert_eq!(at, 50 * MIB);
            }
            other => panic!("{other:?}"),
        }
        // Too small to split (< 2 × min piece).
        let segs = vec![seg(0, 7 * MIB, true, 1.0)];
        assert_eq!(pick(&segs, now, MIN_PIECE, true), None);
        // Finished and inactive segments are not split.
        let segs = vec![seg(10 * MIB, 10 * MIB, false, 0.0), seg(0, 100 * MIB, false, 0.0)];
        assert_eq!(pick(&segs, now, MIN_PIECE, true), Some(Pick::Resume(1)));
        // No splitting without range support.
        let segs = vec![seg(0, 100 * MIB, true, 1.0)];
        assert_eq!(pick(&segs, now, MIN_PIECE, false), None);
    }

    #[test]
    fn split_point_alignment() {
        assert_eq!(split_point(0, 8 * MIB), Some(4 * MIB));
        let at = split_point(12_345, 9_000_001).unwrap();
        assert_eq!(at % ALIGN, 0);
        assert!(at > 12_345 && at < 9_000_001);
        assert_eq!(split_point(0, 1000), None);
    }

    #[test]
    fn content_disposition() {
        let f = filename_from_content_disposition;
        assert_eq!(f("attachment; filename=\"a b.zip\"").as_deref(), Some("a b.zip"));
        assert_eq!(f("attachment; filename=plain.iso").as_deref(), Some("plain.iso"));
        assert_eq!(
            f("attachment; filename=\"fallback.txt\"; filename*=UTF-8''na%C3%AFve%20file.txt").as_deref(),
            Some("naïve file.txt")
        );
        assert_eq!(f("attachment; filename*=utf-8'en'%E2%82%AC%20rates.pdf").as_deref(), Some("€ rates.pdf"));
        assert_eq!(f("attachment; filename*=iso-8859-1''caf%E9.txt").as_deref(), Some("café.txt"));
        assert_eq!(f("attachment; filename=\"semi;colon \\\"q\\\".txt\"").as_deref(), Some("semi;colon \"q\".txt"));
        assert_eq!(f("inline").as_deref(), None);
    }

    #[test]
    fn url_and_sanitize() {
        assert_eq!(filename_from_url("https://x.org/a/b/My%20File.dmg?x=1").as_deref(), Some("My File.dmg"));
        assert_eq!(filename_from_url("https://x.org/dir/").as_deref(), Some("dir"));
        assert_eq!(filename_from_url("https://x.org/").as_deref(), None);
        assert_eq!(sanitize_filename("../../etc/passwd"), "passwd");
        assert_eq!(sanitize_filename("a:b\u{0}c.txt"), "a_b_c.txt");
        assert_eq!(sanitize_filename("..."), "download");
        assert_eq!(sanitize_filename(".hidden"), "hidden");
        assert_eq!(sanitize_filename("  "), "download");
        let long = format!("{}.zip", "é".repeat(200));
        let s = sanitize_filename(&long);
        assert!(s.len() <= 255 && s.ends_with(".zip"));
    }

    #[test]
    fn dedup() {
        let taken = ["a.zip", "a (1).zip", "x.tar.gz", "noext"];
        let t = |n: &str| taken.contains(&n);
        assert_eq!(dedup_name("b.zip", t), "b.zip");
        assert_eq!(dedup_name("a.zip", t), "a (2).zip");
        assert_eq!(dedup_name("x.tar.gz", t), "x (1).tar.gz");
        assert_eq!(dedup_name("noext", t), "noext (1)");
    }

    #[test]
    fn tags() {
        assert_eq!(finder_tag("movie.MKV"), Some("Video"));
        assert_eq!(finder_tag("x.tar.gz"), Some("Archive"));
        assert_eq!(finder_tag("Installer.dmg"), Some("App"));
        assert_eq!(finder_tag("song.flac"), Some("Audio"));
        assert_eq!(finder_tag("paper.pdf"), Some("Document"));
        assert_eq!(finder_tag("pic.heic"), Some("Image"));
        assert_eq!(finder_tag("100MB.bin"), None);
    }
}
