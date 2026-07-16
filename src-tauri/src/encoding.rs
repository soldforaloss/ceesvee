//! Encoding detection and conversion.
//!
//! Detection order: look for a BOM first (authoritative), otherwise fall back
//! to [`chardetng`] for legacy single-byte encodings. Decoding always produces
//! a UTF-8 `String`; encoding back out supports UTF-8, UTF-16 (LE/BE) and
//! Windows-1252 (which is also the practical superset of ISO-8859-1/Latin-1).

use encoding_rs::{Encoding, UTF_16BE, UTF_16LE, UTF_8};

/// The encodings we expose in the override UI, in display order.
pub const SUPPORTED: &[&str] = &["UTF-8", "UTF-16LE", "UTF-16BE", "windows-1252"];

/// Resolve a canonical encoding name (or label such as `latin1`) to a static
/// [`Encoding`], defaulting to UTF-8 for anything unrecognised.
pub fn from_name(name: &str) -> &'static Encoding {
    Encoding::for_label(name.as_bytes()).unwrap_or(UTF_8)
}

/// Detect the encoding of a raw byte buffer.
///
/// Returns the detected encoding plus whether a byte-order mark was present.
pub fn detect(bytes: &[u8]) -> (&'static Encoding, bool) {
    if let Some((encoding, _bom_len)) = Encoding::for_bom(bytes) {
        return (encoding, true);
    }
    let mut detector = chardetng::EncodingDetector::new();
    detector.feed(bytes, true);
    // `allow_utf8 = true` lets the detector report UTF-8 for clean ASCII/UTF-8.
    (detector.guess(None, true), false)
}

/// Decode raw bytes to a UTF-8 `String`, stripping a leading BOM that matches
/// `encoding`. The boolean indicates whether any malformed sequences were
/// replaced with U+FFFD.
pub fn decode(bytes: &[u8], encoding: &'static Encoding) -> (String, bool) {
    let (cow, had_errors) = encoding.decode_with_bom_removal(bytes);
    (cow.into_owned(), had_errors)
}

/// Encode a UTF-8 string into the target encoding's bytes, also reporting
/// whether any character was unmappable (in which case `encoding_rs`
/// substituted an HTML numeric reference). Callers that must not lose data
/// check the flag and fail instead of writing the substituted bytes.
///
/// `encoding_rs` does not implement a UTF-16 *encoder* (the WHATWG standard
/// only defines UTF-16 decoders), so we serialize UTF-16 by hand from the
/// string's UTF-16 code units.
pub fn encode_checked(text: &str, encoding: &'static Encoding) -> (Vec<u8>, bool) {
    if encoding == UTF_16LE {
        let mut out = Vec::with_capacity(text.len() * 2);
        for unit in text.encode_utf16() {
            out.extend_from_slice(&unit.to_le_bytes());
        }
        (out, false)
    } else if encoding == UTF_16BE {
        let mut out = Vec::with_capacity(text.len() * 2);
        for unit in text.encode_utf16() {
            out.extend_from_slice(&unit.to_be_bytes());
        }
        (out, false)
    } else {
        let (cow, _used, had_unmappable) = encoding.encode(text);
        (cow.into_owned(), had_unmappable)
    }
}

/// Whether `text` contains characters the target encoding cannot represent.
/// Unicode encodings can represent everything; only legacy single-byte
/// encodings (Windows-1252 here) can lose characters.
pub fn has_unmappable(text: &str, encoding: &'static Encoding) -> bool {
    if encoding == UTF_8 || encoding == UTF_16LE || encoding == UTF_16BE {
        return false;
    }
    encoding.encode(text).2
}

/// The byte-order mark for an encoding, or an empty slice if it has none.
pub fn bom_for(encoding: &'static Encoding) -> &'static [u8] {
    if encoding == UTF_8 {
        &[0xEF, 0xBB, 0xBF]
    } else if encoding == UTF_16LE {
        &[0xFF, 0xFE]
    } else if encoding == UTF_16BE {
        &[0xFE, 0xFF]
    } else {
        &[]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use encoding_rs::WINDOWS_1252;

    #[test]
    fn detects_utf8_bom() {
        let bytes = b"\xEF\xBB\xBFhello";
        let (enc, had_bom) = detect(bytes);
        assert_eq!(enc, UTF_8);
        assert!(had_bom);
        let (text, errors) = decode(bytes, enc);
        assert_eq!(text, "hello");
        assert!(!errors);
    }

    #[test]
    fn detects_utf16le_bom() {
        let mut bytes = vec![0xFF, 0xFE];
        for unit in "hi".encode_utf16() {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        let (enc, had_bom) = detect(&bytes);
        assert_eq!(enc, UTF_16LE);
        assert!(had_bom);
        let (text, _) = decode(&bytes, enc);
        assert_eq!(text, "hi");
    }

    #[test]
    fn round_trips_utf16le() {
        let original = "café — ☕";
        let bytes = encode_checked(original, UTF_16LE).0;
        let (decoded, errors) = decode(&bytes, UTF_16LE);
        assert_eq!(decoded, original);
        assert!(!errors);
    }

    #[test]
    fn windows_1252_round_trip() {
        let original = "naïve café";
        let bytes = encode_checked(original, WINDOWS_1252).0;
        let (decoded, _) = decode(&bytes, WINDOWS_1252);
        assert_eq!(decoded, original);
    }

    #[test]
    fn bom_bytes_are_correct() {
        assert_eq!(bom_for(UTF_8), &[0xEF, 0xBB, 0xBF]);
        assert_eq!(bom_for(UTF_16LE), &[0xFF, 0xFE]);
        assert_eq!(bom_for(UTF_16BE), &[0xFE, 0xFF]);
        assert_eq!(bom_for(WINDOWS_1252), &[] as &[u8]);
    }

    #[test]
    fn latin1_label_maps_to_windows_1252() {
        assert_eq!(from_name("latin1"), WINDOWS_1252);
        assert_eq!(from_name("iso-8859-1"), WINDOWS_1252);
    }

    #[test]
    fn unmappable_detection_is_encoding_aware() {
        // "→" has no Windows-1252 representation; "café €" does (€ is 0x80).
        assert!(has_unmappable("a → b", WINDOWS_1252));
        assert!(!has_unmappable("café €", WINDOWS_1252));
        // Unicode encodings can represent everything.
        assert!(!has_unmappable("a → b", UTF_8));
        assert!(!has_unmappable("a → b", UTF_16LE));

        let (_, lossy) = encode_checked("a → b", WINDOWS_1252);
        assert!(lossy);
        let (_, clean) = encode_checked("plain", WINDOWS_1252);
        assert!(!clean);
    }
}
