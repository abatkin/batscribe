//! Saving a document's text to a plain-text file.

use std::io::Write;
use std::path::{Path, PathBuf};

/// Converts what a file dialog returns (`file:///…`, percent-encoded) or a plain path into a
/// path. Returns `None` for other URL schemes.
pub fn path_from_url(s: &str) -> Option<PathBuf> {
    let Some(rest) = s.strip_prefix("file://") else {
        return (!s.contains("://") && !s.is_empty()).then(|| PathBuf::from(s));
    };
    // file://host/path is only meaningful for the local host.
    let rest = rest.strip_prefix("localhost").unwrap_or(rest);
    if !rest.starts_with('/') {
        return None;
    }
    let bytes = rest.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let Ok(b) = u8::from_str_radix(std::str::from_utf8(&bytes[i + 1..i + 3]).ok()?, 16)
        {
            out.push(b);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    Some(PathBuf::from(String::from_utf8(out).ok()?))
}

/// Writes `text` (ending with a newline) to `path` atomically: a temporary file in the same
/// directory is written, synced and renamed over the target, so a failure never leaves a
/// half-written file.
pub fn write_text_file(path: &Path, text: &str) -> std::io::Result<()> {
    let dir = path
        .parent()
        .filter(|d| !d.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let name = path
        .file_name()
        .ok_or_else(|| std::io::Error::other("not a file path"))?;
    let tmp = dir.join(format!(
        ".{}.{}.tmp",
        name.to_string_lossy(),
        std::process::id()
    ));
    let result = (|| {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(text.as_bytes())?;
        if !text.is_empty() && !text.ends_with('\n') {
            f.write_all(b"\n")?;
        }
        f.sync_all()?;
        std::fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// A document title from a file name ("notes.txt" -> "notes").
pub fn title_from_path(path: &Path) -> Option<String> {
    path.file_stem()
        .map(|s| s.to_string_lossy().trim().to_owned())
        .filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_and_paths() {
        assert_eq!(
            path_from_url("file:///home/a/My%20Notes/caf%C3%A9.txt"),
            Some(PathBuf::from("/home/a/My Notes/café.txt"))
        );
        assert_eq!(
            path_from_url("file://localhost/tmp/x"),
            Some(PathBuf::from("/tmp/x"))
        );
        assert_eq!(
            path_from_url("/tmp/plain.txt"),
            Some(PathBuf::from("/tmp/plain.txt"))
        );
        assert_eq!(path_from_url("https://example.com/x"), None);
        assert_eq!(path_from_url(""), None);
    }

    #[test]
    fn writes_atomically_with_trailing_newline() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.txt");
        write_text_file(&path, "Hello 😀\nsecond").unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "Hello 😀\nsecond\n"
        );
        write_text_file(&path, "replaced\n").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "replaced\n");
        assert_eq!(
            std::fs::read_dir(dir.path()).unwrap().count(),
            1,
            "no temp files left"
        );
        assert!(write_text_file(&dir.path().join("missing/dir/x.txt"), "x").is_err());
        assert_eq!(title_from_path(&path).as_deref(), Some("out"));
    }
}
