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
    /// Prefer the format's UTF-8 flag or valid UTF-8, else fall back to a
    /// configured codepage.
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
/// or valid UTF-8, then falls back to the default legacy codepage (GBK — the
/// frozen `Auto` variant carries no codepage field; the GUI can force a
/// codepage via `Forced`). `Forced` decodes with the given codepage;
/// `Forced(Utf8)` rejects invalid UTF-8 with [`Error::CorruptArchive`].
/// `Cp437` is deferred and returns [`Error::UnsupportedFeature`].
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
}
