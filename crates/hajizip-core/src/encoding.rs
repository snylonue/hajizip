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
/// M0: only the UTF-8 path is implemented. Legacy codepage decoding requires
/// an encoding library (see the research todo in `local-doc/`) and currently
/// returns [`Error::UnsupportedFeature`].
pub fn decode_filename(raw: &[u8], enc: FilenameEncoding, hint: Utf8Flag) -> Result<String> {
    match enc {
        FilenameEncoding::Auto if hint.0 => utf8(raw),
        FilenameEncoding::Auto => match std::str::from_utf8(raw) {
            Ok(s) => Ok(s.to_owned()),
            Err(_) => Err(Error::UnsupportedFeature(
                "legacy codepage decoding (auto)".into(),
            )),
        },
        FilenameEncoding::Forced(Codepage::Utf8) => utf8(raw),
        FilenameEncoding::Forced(cp) => Err(Error::UnsupportedFeature(format!(
            "codepage {cp:?} decoding"
        ))),
    }
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
        let bytes = [0xC4u8, 0xE3u8]; // "你" in GBK, invalid as UTF-8
        assert!(decode_filename(&bytes, FilenameEncoding::Auto, Utf8Flag(true)).is_err());
    }

    #[test]
    fn legacy_codepage_is_unsupported_for_now() {
        let bytes = [0xC4u8, 0xE3u8];
        assert!(decode_filename(&bytes, FilenameEncoding::Auto, Utf8Flag(false)).is_err());
        assert!(
            decode_filename(
                &bytes,
                FilenameEncoding::Forced(Codepage::Gbk),
                Utf8Flag(false)
            )
            .is_err()
        );
    }
}
