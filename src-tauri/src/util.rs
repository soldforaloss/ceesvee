//! Small shared helpers for translating wire values into core values.

use std::path::Path;

use crate::dto::FileFingerprint;

/// Convert a one-character delimiter string from the UI into a byte. Accepts an
/// actual tab character or the literal escape `\t`; falls back to a comma.
pub fn delimiter_to_byte(s: &str) -> u8 {
    match s {
        "\\t" | "\t" => b'\t',
        _ => s.bytes().next().unwrap_or(b','),
    }
}

/// Heuristic shared by every open path: treat the first record as a header
/// when none of its cells is numeric.
pub fn looks_like_header(first_record: &[String]) -> bool {
    if first_record.is_empty() {
        return false;
    }
    first_record.iter().all(|cell| {
        let trimmed = cell.trim();
        trimmed.is_empty() || trimmed.parse::<f64>().is_err()
    })
}

/// Stat a file into its identity fingerprint (size + mtime in millis).
/// `None` when the file is missing or its metadata is unreadable.
pub fn stat_fingerprint(path: &Path) -> Option<FileFingerprint> {
    let meta = std::fs::metadata(path).ok()?;
    let modified_at_ms = meta
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_millis() as u64;
    Some(FileFingerprint {
        size: meta.len(),
        modified_at_ms,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_delimiters() {
        assert_eq!(delimiter_to_byte(","), b',');
        assert_eq!(delimiter_to_byte(";"), b';');
        assert_eq!(delimiter_to_byte("\t"), b'\t');
        assert_eq!(delimiter_to_byte("\\t"), b'\t');
        assert_eq!(delimiter_to_byte("|"), b'|');
        assert_eq!(delimiter_to_byte(""), b',');
    }

    #[test]
    fn fingerprint_tracks_size_and_mtime() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("data.csv");
        std::fs::write(&path, b"a,b\n1,2\n").unwrap();
        let first = stat_fingerprint(&path).expect("fingerprint");
        assert_eq!(first.size, 8);

        // Grow the file; the size (and usually the mtime) must change.
        std::fs::write(&path, b"a,b\n1,2\n3,4\n").unwrap();
        let second = stat_fingerprint(&path).expect("fingerprint");
        assert_ne!(first, second);
        assert_eq!(second.size, 12);

        // Missing files have no fingerprint.
        assert!(stat_fingerprint(&dir.path().join("gone.csv")).is_none());
    }
}
