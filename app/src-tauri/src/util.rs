//! Plain helpers with no UI: the menu-bar speed text, moving a finished file to the folder
//! chosen on the capture card, and reading the map older versions kept of those moves.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Compact speed for the menu bar, same rules as the UI's `formatSpeedShort`:
/// decimal units, one decimal below 10 MB/s ("4.2 MB/s"), none above ("38 MB/s").
pub fn speed_short(bps: f64) -> String {
    if !bps.is_finite() || bps <= 0.0 {
        return "0 KB/s".into();
    }
    if bps < 1000.0 {
        return format!("{} B/s", bps.round() as u64);
    }
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut i = 0;
    let mut v = bps;
    while v >= 1000.0 && i < UNITS.len() - 1 {
        v /= 1000.0;
        i += 1;
    }
    let digits = if i >= 2 && v < 10.0 { 1 } else { 0 };
    // Rounding can carry into the next unit ("999.7 KB/s" → "1000 KB/s" → "1.0 MB/s").
    if format!("{v:.digits$}").parse::<f64>().unwrap_or(v) >= 1000.0 && i < UNITS.len() - 1 {
        v /= 1000.0;
        i += 1;
        let d = if i >= 2 && v < 10.0 { 1 } else { 0 };
        return format!("{v:.d$} {}/s", UNITS[i]);
    }
    format!("{v:.digits$} {}/s", UNITS[i])
}

/// `dir/name`, or `dir/name (1).ext`, `(2)`… if taken.
pub fn free_name(dir: &Path, name: &str) -> PathBuf {
    let first = dir.join(name);
    if first.symlink_metadata().is_err() {
        return first;
    }
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    };
    (1..)
        .map(|n| dir.join(format!("{stem} ({n}){ext}")))
        .find(|p| p.symlink_metadata().is_err())
        .expect("unbounded")
}

/// Move a finished download into `dir` (keeping its name, de-duplicated). Same volume:
/// rename. Across volumes: `/bin/mv`, which keeps the Finder tags, the quarantine flag and
/// "Where from". Returns the new path.
pub fn move_into(file: &Path, dir: &Path) -> std::io::Result<PathBuf> {
    let name = file
        .file_name()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "no file name"))?;
    if file.parent() == Some(dir) {
        return Ok(file.to_path_buf());
    }
    std::fs::create_dir_all(dir)?;
    let dest = free_name(dir, &name.to_string_lossy());
    match std::fs::rename(file, &dest) {
        Ok(()) => Ok(dest),
        Err(e) if e.raw_os_error() == Some(18) => {
            // EXDEV: different volume.
            let st = std::process::Command::new("/bin/mv").arg("-n").arg(file).arg(&dest).status()?;
            if st.success() && dest.exists() {
                Ok(dest)
            } else {
                Err(std::io::Error::other(format!("mv exited with {st}")))
            }
        }
        Err(e) => Err(e),
    }
}

/// The map an older shell kept of the finished files it moved (job id → new path), in
/// `moved-files.json`. Read once at startup to hand the entries to the daemon (`set_path`).
pub fn load_legacy_moves(path: &Path) -> HashMap<String, PathBuf> {
    std::fs::read(path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn speeds() {
        assert_eq!(speed_short(0.0), "0 KB/s");
        assert_eq!(speed_short(512.0), "512 B/s");
        assert_eq!(speed_short(42_000.0), "42 KB/s");
        assert_eq!(speed_short(4_200_000.0), "4.2 MB/s");
        assert_eq!(speed_short(38_400_000.0), "38 MB/s");
        assert_eq!(speed_short(999_700.0), "1.0 MB/s");
        assert_eq!(speed_short(1_260_000_000.0), "1.3 GB/s");
    }

    #[test]
    fn free_names() {
        let d = tempfile::tempdir().unwrap();
        assert_eq!(free_name(d.path(), "a.zip"), d.path().join("a.zip"));
        std::fs::write(d.path().join("a.zip"), b"x").unwrap();
        std::fs::write(d.path().join("a (1).zip"), b"x").unwrap();
        assert_eq!(free_name(d.path(), "a.zip"), d.path().join("a (2).zip"));
        std::fs::write(d.path().join("README"), b"x").unwrap();
        assert_eq!(free_name(d.path(), "README"), d.path().join("README (1)"));
    }

    #[test]
    fn move_and_legacy_map() {
        let d = tempfile::tempdir().unwrap();
        let src = d.path().join("dl");
        let dst = d.path().join("chosen");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::create_dir_all(&dst).unwrap();
        std::fs::write(src.join("f.bin"), b"hello").unwrap();
        std::fs::write(dst.join("f.bin"), b"older").unwrap();
        let p = move_into(&src.join("f.bin"), &dst).unwrap();
        assert_eq!(p, dst.join("f (1).bin"));
        assert_eq!(std::fs::read(&p).unwrap(), b"hello");
        assert!(!src.join("f.bin").exists());

        let store = d.path().join("moved-files.json");
        assert!(load_legacy_moves(&store).is_empty());
        std::fs::write(&store, format!("{{\"job1\": {:?}}}", p.display().to_string())).unwrap();
        assert_eq!(load_legacy_moves(&store).get("job1"), Some(&p));
        std::fs::write(&store, b"not json").unwrap();
        assert!(load_legacy_moves(&store).is_empty());
    }
}
