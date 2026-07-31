//! Gzip (RFC 1952) codec implementation.
//!
//! The DEFLATE decoding core is `miniz_oxide` (pure safe Rust, `forbid(unsafe_code)`),
//! reached through `flate2`'s `miniz_oxide` backend. See
//! `local-doc/research-flate2.md` for the selection rationale.

use std::io::{Read, Write};

use crate::codec::Codec;
use crate::error::Result;
use crate::format::CodecFormat;
use crate::model::Level;

/// The gzip codec: wraps a reader in a stream that decompresses on the fly.
///
/// Stateless and thread-safe; identity and detection live in [`GzipFormat`].
pub struct GzipCodec;

impl Codec for GzipCodec {
    /// Wrap `input` in a gzip-decompressing reader.
    ///
    /// [`flate2::read::MultiGzDecoder`] is used rather than `GzDecoder` so that
    /// concatenated gzip members (produced by some tools) are all decoded, and
    /// each member's CRC-32 trailer is verified. Corruption surfaces as an I/O
    /// error while reading.
    fn decompress<'r>(
        &self,
        input: Box<dyn Read + Send + 'r>,
    ) -> Result<Box<dyn Read + Send + 'r>> {
        Ok(Box::new(flate2::read::MultiGzDecoder::new(input)))
    }

    /// Compression is reserved for a future milestone (§4.9 of the design doc);
    /// the default implementation reports it as unsupported.
    fn compress<'w>(
        &self,
        _output: Box<dyn Write + Send + 'w>,
        _level: Level,
    ) -> Result<Box<dyn Write + Send + 'w>> {
        Err(crate::error::Error::UnsupportedFeature(
            "gzip compression".into(),
        ))
    }
}

/// Format identity and detection for gzip.
pub struct GzipFormat;

impl CodecFormat for GzipFormat {
    fn id(&self) -> &str {
        "gzip"
    }

    fn display_name(&self) -> &str {
        "Gzip"
    }

    fn extensions(&self) -> &[&str] {
        &["gz", "gzip"]
    }

    fn matches(&self, head: &[u8], ext: Option<&str>) -> bool {
        // RFC 1952 magic: 0x1f 0x8b.
        head.starts_with(&[0x1f, 0x8b]) || matches!(ext, Some("gz" | "gzip"))
    }

    fn build(&self) -> Box<dyn Codec> {
        Box::new(GzipCodec)
    }
}

#[cfg(test)]
mod tests {
    use std::io::Read;

    use super::*;
    use crate::registry::Registry;

    /// A fixed gzip stream of `"Hello, hajizip!\n"`, produced by system
    /// `gzip -n -9` (deterministic: mtime zeroed). Serves as a known vector
    /// cross-checking our decoder against an independent implementation.
    const HELLO_GZ: &[u8] = &[
        0x1f, 0x8b, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02, 0x03, 0xf3, 0x48, 0xcd, 0xc9, 0xc9,
        0xd7, 0x51, 0xc8, 0x48, 0xcc, 0xca, 0xac, 0xca, 0x2c, 0x50, 0xe4, 0x02, 0x00, 0x7a, 0x53,
        0x23, 0x5d, 0x10, 0x00, 0x00, 0x00,
    ];
    const HELLO: &[u8] = b"Hello, hajizip!\n";

    fn decode(bytes: &[u8]) -> crate::Result<Vec<u8>> {
        let codec = GzipCodec;
        // `&[u8]` implements `Read` but not `Seek`, proving the streaming path
        // does not require a seekable (fully-buffered) input.
        let mut reader = codec.decompress(Box::new(bytes))?;
        let mut out = Vec::new();
        reader.read_to_end(&mut out)?;
        Ok(out)
    }

    #[test]
    fn decodes_known_vector() {
        assert_eq!(decode(HELLO_GZ).unwrap(), HELLO);
    }

    #[test]
    fn decodes_multiple_concatenated_members() {
        let mut multi = HELLO_GZ.to_vec();
        multi.extend_from_slice(HELLO_GZ);
        assert_eq!(decode(&multi).unwrap(), [HELLO, HELLO].concat());
    }

    #[test]
    fn truncated_stream_errors() {
        assert!(decode(&HELLO_GZ[..20]).is_err());
    }

    #[test]
    fn corrupt_crc_errors() {
        let mut bad = HELLO_GZ.to_vec();
        let last = bad.len() - 8;
        bad[last] ^= 0xff; // flip a byte of the CRC-32 trailer
        let err = decode(&bad).unwrap_err();
        assert!(matches!(err, crate::error::Error::Io(_)));
    }

    #[test]
    fn compression_is_reserved() {
        let codec = GzipCodec;
        let sink: Vec<u8> = Vec::new();
        let result = codec.compress(Box::new(sink), Level::default());
        assert!(matches!(
            result,
            Err(crate::error::Error::UnsupportedFeature(_))
        ));
    }

    #[test]
    fn detects_by_magic_and_extension() {
        let fmt = GzipFormat;
        assert_eq!(fmt.id(), "gzip");
        assert_eq!(fmt.display_name(), "Gzip");
        assert_eq!(fmt.extensions(), &["gz", "gzip"]);
        assert!(fmt.matches(&[0x1f, 0x8b, 0x08, 0x00], None));
        assert!(fmt.matches(b"junk", Some("gz")));
        // `ext` is lowercased by the caller (`Source::extension`); "GZ" must not match.
        assert!(!fmt.matches(b"junk", Some("GZ")));
        assert!(!fmt.matches(b"junk", None));
        assert!(!fmt.matches(b"PK\x03\x04", None));
    }

    #[test]
    fn registered_codec_is_detectable() {
        let reg = Registry::new().register_codec(GzipFormat);
        assert_eq!(reg.codecs().len(), 1);
        let fmt = reg
            .detect_codec(&[0x1f, 0x8b, 0x08], None)
            .expect("detected");
        assert_eq!(fmt.id(), "gzip");
    }
}
