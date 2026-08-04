//! Filename encoding detection and decoding (multi-language support).
//!
//! Archive entry names may be encoded in legacy, non-UTF-8 codepages (zip
//! CP437, Chinese GBK, Japanese Shift-JIS, Traditional Chinese Big5, ...).
//! This module provides the decoding abstraction described in the design doc.

use crate::error::{Error, Result};

/// A legacy codepage used to decode non-UTF-8 entry names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Codepage {
    /// UTF-8.
    Utf8,
    /// Simplified Chinese GBK.
    Gbk,
    /// Japanese Shift-JIS.
    ShiftJis,
    /// Traditional Chinese Big5.
    Big5,
    /// DOS / zip default CP437.
    ///
    /// Deferred: not part of the WHATWG encoding set and not yet shipped
    /// (decoding returns [`Error::UnsupportedFeature`]). See
    /// `local-doc/research-encoding.md` §4.
    Cp437,
}

/// Filename decoding strategy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FilenameEncoding {
    /// Prefer the format's UTF-8 flag or valid UTF-8, else fall back to the
    /// default legacy codepage (GBK). The zip reader additionally performs
    /// an archive-level codepage detection before decoding (see
    /// `ZipArchive::new`): other formats (tar...) have no declared encoding
    /// or a UTF-8-native one, so no detection is attempted for them.
    #[default]
    Auto,
    /// Force a specific codepage regardless of flags.
    Forced(Codepage),
}

/// Whether the format signalled that a name is UTF-8 (e.g. the zip EFS bit).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Utf8Flag(pub bool);

/// Decode raw entry-name bytes into a UTF-8 `String`.
///
/// Strategy (architecture.md §4.10): `Auto` prefers the format's UTF-8 flag
/// or valid UTF-8, then falls back to the default legacy codepage (GBK).
/// Legacy-codepage *detection* is deliberately not part of this function: it
/// is an archive-level concern (the zip reader aggregates raw names and
/// detects once, feeding the result back via `Forced`; tar's USTAR names have
/// no declared encoding, so guessing per name is unreliable). `Forced` decodes
/// with the given codepage; `Forced(Utf8)` rejects invalid UTF-8 with
/// [`Error::CorruptArchive`]. `Cp437` is deferred and returns
/// [`Error::UnsupportedFeature`].
pub fn decode_filename(raw: &[u8], enc: FilenameEncoding, hint: Utf8Flag) -> Result<String> {
    match enc {
        FilenameEncoding::Auto if hint.0 => utf8(raw),
        FilenameEncoding::Auto => match std::str::from_utf8(raw) {
            Ok(s) => Ok(s.to_owned()),
            // Not valid UTF-8: fall back to the default codepage (GBK).
            Err(_) => decode_codepage(raw, Codepage::Gbk),
        },
        FilenameEncoding::Forced(Codepage::Utf8) => utf8(raw),
        FilenameEncoding::Forced(cp) => decode_codepage(raw, cp),
    }
}

/// Detect the legacy codepage of `bytes` (the concatenation of an archive's
/// non-UTF-8 raw names) using `chardetng` — the same detector Firefox ships
/// for legacy Web content, by the author of `encoding_rs`.
///
/// Used by the zip reader's archive-level pre-pass: a single short name is
/// usually too little signal for reliable detection, so every legacy name is
/// aggregated before calling. Only guesses that map onto a supported codepage
/// are trusted; anything else (windows-125x, EUC-KR, ...) yields `None` so
/// callers fall back to the default codepage (GBK).
pub(crate) fn detect_codepage(bytes: &[u8]) -> Option<Codepage> {
    if bytes.is_empty() {
        return None;
    }
    let mut detector = chardetng::EncodingDetector::new();
    detector.feed(bytes, true);
    match detector.guess(None, true).name() {
        "GBK" => Some(Codepage::Gbk),
        "Shift_JIS" => Some(Codepage::ShiftJis),
        "Big5" => Some(Codepage::Big5),
        _ => None,
    }
}

/// Decode bytes as a legacy codepage via `encoding_rs` (WHATWG standard).
///
/// The WHATWG decoders never fail: invalid byte sequences become U+FFFD
/// (replacement character), which matches the "force decode" semantics of
/// `Forced` and the Auto fallback.
fn decode_codepage(raw: &[u8], cp: Codepage) -> Result<String> {
    let (decoded, _encoding, _had_errors) = match cp {
        Codepage::Utf8 => return utf8(raw),
        Codepage::Gbk => encoding_rs::GBK.decode(raw),
        Codepage::ShiftJis => encoding_rs::SHIFT_JIS.decode(raw),
        Codepage::Big5 => encoding_rs::BIG5.decode(raw),
        Codepage::Cp437 => {
            return Err(Error::UnsupportedFeature(
                "CP437 decoding (deferred)".into(),
            ));
        }
    };
    Ok(decoded.into_owned())
}

/// Strictly decode bytes as UTF-8.
fn utf8(raw: &[u8]) -> Result<String> {
    std::str::from_utf8(raw)
        .map(str::to_owned)
        .map_err(|e| Error::CorruptArchive(format!("invalid utf-8 name: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_decodes_valid_utf8() {
        let s =
            decode_filename("hello".as_bytes(), FilenameEncoding::Auto, Utf8Flag(false)).unwrap();
        assert_eq!(s, "hello");
    }

    #[test]
    fn utf8_flag_forces_strict_utf8() {
        let bytes = [0xc4u8, 0xe3u8]; // "你" in GBK, invalid as UTF-8
        assert!(decode_filename(&bytes, FilenameEncoding::Auto, Utf8Flag(true)).is_err());
    }

    #[test]
    fn gbk_decodes_hello() {
        // GBK bytes of "你好".
        let bytes = [0xc4u8, 0xe3u8, 0xba, 0xc3];
        assert_eq!(
            decode_filename(
                &bytes,
                FilenameEncoding::Forced(Codepage::Gbk),
                Utf8Flag(false)
            )
            .unwrap(),
            "你好"
        );
    }

    #[test]
    fn shift_jis_decodes_konnichiwa() {
        // Shift-JIS bytes of "こんにちは".
        let bytes = [0x82, 0xb1, 0x82, 0xf1, 0x82, 0xc9, 0x82, 0xbf, 0x82, 0xcd];
        assert_eq!(
            decode_filename(
                &bytes,
                FilenameEncoding::Forced(Codepage::ShiftJis),
                Utf8Flag(false)
            )
            .unwrap(),
            "こんにちは"
        );
    }

    #[test]
    fn big5_decodes_hello() {
        // Big5 bytes of "你好" (A7 41 A6 6E).
        let bytes = [0xa7, 0x41, 0xa6, 0x6e];
        assert_eq!(
            decode_filename(
                &bytes,
                FilenameEncoding::Forced(Codepage::Big5),
                Utf8Flag(false)
            )
            .unwrap(),
            "你好"
        );
    }

    #[test]
    fn cp437_is_deferred() {
        let bytes = [0xda];
        assert!(matches!(
            decode_filename(
                &bytes,
                FilenameEncoding::Forced(Codepage::Cp437),
                Utf8Flag(false)
            ),
            Err(Error::UnsupportedFeature(_))
        ));
    }

    #[test]
    fn auto_falls_back_to_gbk_for_non_utf8() {
        let bytes = [0xc4u8, 0xe3u8, 0xba, 0xc3]; // GBK "你好", not valid UTF-8
        assert_eq!(
            decode_filename(&bytes, FilenameEncoding::Auto, Utf8Flag(false)).unwrap(),
            "你好"
        );
    }

    #[test]
    fn forced_utf8_rejects_invalid_bytes() {
        let bytes = [0xc4u8, 0xe3u8];
        assert!(matches!(
            decode_filename(
                &bytes,
                FilenameEncoding::Forced(Codepage::Utf8),
                Utf8Flag(false)
            ),
            Err(Error::CorruptArchive(_))
        ));
    }

    #[test]
    fn invalid_bytes_become_replacement_chars() {
        // WHATWG decoders never fail: invalid sequences become U+FFFD.
        let bytes = [0xff, 0x41];
        let s = decode_filename(
            &bytes,
            FilenameEncoding::Forced(Codepage::Gbk),
            Utf8Flag(false),
        )
        .unwrap();
        assert!(s.contains('\u{fffd}'));
    }

    #[test]
    fn detect_codepage_recognizes_supported_legacy_encodings() {
        let sjis = [
            0x83u8, 0x45, 0x83, 0x89, 0x83, 0x8c, 0x83, 0x5e, 0x83, 0x45, 0x83, 0x93,
        ];
        assert_eq!(detect_codepage(&sjis), Some(Codepage::ShiftJis));
        let gbk = [
            0xb2u8, 0xe2, 0xca, 0xd4, 0xce, 0xc4, 0xb5, 0xb5, 0xcb, 0xb5, 0xc3, 0xf7,
        ];
        assert_eq!(detect_codepage(&gbk), Some(Codepage::Gbk));
        let big5 = [0xbb, 0xa1, 0xa9, 0xfa, 0xae, 0xd1, 0x2e, 0x74, 0x78, 0x74]; // 說明書.txt
        assert_eq!(detect_codepage(&big5), Some(Codepage::Big5));
    }

    #[test]
    fn detect_codepage_rejects_unsupported_guesses() {
        // Too short to classify: chardetng guesses EUC-KR / windows-125x,
        // which map onto no supported codepage -> None (caller falls back).
        assert_eq!(detect_codepage(&[0xc4u8, 0xe3, 0xba, 0xc3]), None);
        assert_eq!(detect_codepage(b""), None);
        // Pure ASCII/UTF-8 input is not a legacy codepage detection target.
        assert_eq!(detect_codepage(b"hello"), None);
    }

    #[test]
    fn detect_codepage_aggregate_short_shift_jis_names() {
        // Each name alone is too short to classify, but the aggregate of the
        // archive's legacy names is enough signal (this is what the zip
        // archive's pre-pass feeds).
        let mut corpus = Vec::new();
        corpus.extend_from_slice(&[0x90, 0xe0, 0x96, 0xbe, 0x2e, 0x74, 0x78, 0x74]); // 説明.txt
        corpus.extend_from_slice(&[0x96, 0x7b, 0x2e, 0x74, 0x78, 0x74]); // 本.txt
        corpus.extend_from_slice(&[
            0x83, 0x45, 0x83, 0x89, 0x83, 0x8c, 0x83, 0x5e, 0x83, 0x45, 0x83, 0x93, 0x2e, 0x65,
            0x78, 0x65,
        ]); // ウラレタウン.exe
        assert_eq!(detect_codepage(&corpus), Some(Codepage::ShiftJis));
    }
}
