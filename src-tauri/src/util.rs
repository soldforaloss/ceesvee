//! Small shared helpers for translating wire values into core values.

/// Convert a one-character delimiter string from the UI into a byte. Accepts an
/// actual tab character or the literal escape `\t`; falls back to a comma.
pub fn delimiter_to_byte(s: &str) -> u8 {
    match s {
        "\\t" | "\t" => b'\t',
        _ => s.bytes().next().unwrap_or(b','),
    }
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
}
