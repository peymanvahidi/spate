//! macOS extended attributes: quarantine, "Where from", Finder tag.

use std::path::Path;

/// Best effort: failures are returned but never fatal for the download.
pub fn apply(path: &Path, url: &str, referrer: Option<&str>, tag: Option<&str>) -> anyhow::Result<()> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    xattr::set(path, "com.apple.quarantine", format!("0081;{now:x};Spate;").as_bytes())?;

    let mut froms = vec![plist::Value::String(url.to_string())];
    if let Some(r) = referrer.filter(|r| !r.is_empty()) {
        froms.push(plist::Value::String(r.to_string()));
    }
    xattr::set(path, "com.apple.metadata:kMDItemWhereFroms", &binary_plist(froms)?)?;

    if let Some(tag) = tag {
        let tags = vec![plist::Value::String(tag.to_string())];
        xattr::set(path, "com.apple.metadata:_kMDItemUserTags", &binary_plist(tags)?)?;
    }
    Ok(())
}

fn binary_plist(items: Vec<plist::Value>) -> anyhow::Result<Vec<u8>> {
    let mut buf = Vec::new();
    plist::to_writer_binary(&mut buf, &plist::Value::Array(items))?;
    Ok(buf)
}
