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

/// Encode a UTF-8 string into the target encoding's bytes.
///
/// `encoding_rs` does not implement a UTF-16 *encoder* (the WHATWG standard
/// only defines UTF-16 decoders), so we serialize UTF-16 by hand from the
/// string's UTF-16 code units.
pub fn encode(text: &str, encoding: &'static Encoding) -> Vec<u8> {
    if encoding == UTF_16LE {
        let mut out = Vec::with_capacity(text.len() * 2);
        for unit in text.encode_utf16() {
            out.extend_from_slice(&unit.to_le_bytes());
        }
        out
    } else if encoding == UTF_16BE {
        let mut out = Vec::with_capacity(text.len() * 2);
        for unit in text.encode_utf16() {
            out.extend_from_slice(&unit.to_be_bytes());
        }
        out
    } else {
        let (cow, _used, _had_unmappable) = encoding.encode(text);
        cow.into_owned()
    }
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
        let bytes = encode(original, UTF_16LE);
        let (decoded, errors) = decode(&bytes, UTF_16LE);
        assert_eq!(decoded, original);
        assert!(!errors);
    }

    #[test]
    fn windows_1252_round_trip() {
        let original = "naïve café";
        let bytes = encode(original, WINDOWS_1252);
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
}
