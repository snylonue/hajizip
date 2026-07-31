//! XZ (`.xz`) codec implementation.
//!
//! The decoding core is `lzma-rust2` (pure Rust, port of Tukaani XZ for Java).
//! See `local-doc/research-xz.md` for the selection rationale. The crate is
//! `forbid(unsafe_code)` unless its `optimization` feature is enabled; that
//! feature is pulled in transitively by `sevenz-rust2` (feature unification),
//! which the maintainer accepted (miri/sanitizer/fuzz coverage planned).

use std::io::{Read, Write};

use crate::codec::Codec;
use crate::error::{Error, Result};
use crate::format::CodecFormat;
use crate::model::Level;

/// XZ stream magic bytes (RFC/`xz` format): `\xFD7zXZ\x00`.
const XZ_MAGIC: &[u8] = &[0xFD, b'7', b'z', b'X', b'Z', 0x00];

/// The xz codec: wraps a reader in a stream that decompresses on the fly.
///
/// Stateless and thread-safe; identity and detection live in [`XzFormat`].
pub struct XzCodec;

impl Codec for XzCodec {
    /// Wrap `input` in an xz-decompressing reader.
    ///
    /// `allow_multiple_streams = true` decodes concatenated xz streams (produced
    /// by some tools), mirroring the multi-member gzip behavior. Per-stream
    /// CRC32/CRC64/SHA-256 checks are verified by the decoder; corruption
    /// surfaces as an I/O error while reading.
    fn decompress<'r>(
        &self,
        input: Box<dyn Read + Send + 'r>,
    ) -> Result<Box<dyn Read + Send + 'r>> {
        Ok(Box::new(lzma_rust2::XzReader::new(input, true)))
    }

    /// Compression is reserved for a future milestone (§4.9 of the design doc).
    fn compress<'w>(
        &self,
        _output: Box<dyn Write + Send + 'w>,
        _level: Level,
    ) -> Result<Box<dyn Write + Send + 'w>> {
        Err(Error::UnsupportedFeature("xz compression".into()))
    }
}

/// Format identity and detection for xz.
pub struct XzFormat;

impl CodecFormat for XzFormat {
    fn id(&self) -> &str {
        "xz"
    }

    fn display_name(&self) -> &str {
        "XZ"
    }

    fn extensions(&self) -> &[&str] {
        &["xz", "txz"]
    }

    fn matches(&self, head: &[u8], ext: Option<&str>) -> bool {
        head.starts_with(XZ_MAGIC) || matches!(ext, Some("xz" | "txz"))
    }

    fn build(&self) -> Box<dyn Codec> {
        Box::new(XzCodec)
    }
}

#[cfg(test)]
mod tests {
    use std::io::Read;

    use super::*;
    use crate::registry::Registry;

    /// `xz -9` of `"Hello, hajizip!\n"` (xz embeds no mtime, so the output is
    /// deterministic), produced with system `xz` 5.8.3. Serves as a known
    /// vector cross-checking our decoder against an independent implementation.
    const HELLO_XZ: &[u8] = &[
        0xfd, 0x37, 0x7a, 0x58, 0x5a, 0x00, 0x00, 0x04, 0xe6, 0xd6, 0xb4, 0x46, 0x04, 0xc0, 0x14,
        0x10, 0x21, 0x01, 0x1c, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x88, 0xb0,
        0x67, 0x08, 0x01, 0x00, 0x0f, 0x48, 0x65, 0x6c, 0x6c, 0x6f, 0x2c, 0x20, 0x68, 0x61, 0x6a,
        0x69, 0x7a, 0x69, 0x70, 0x21, 0x0a, 0x00, 0x47, 0x35, 0x9c, 0x8a, 0x40, 0x4c, 0xa6, 0xea,
        0x00, 0x01, 0x30, 0x10, 0xbc, 0x93, 0x77, 0xe2, 0x1f, 0xb6, 0xf3, 0x7d, 0x01, 0x00, 0x00,
        0x00, 0x00, 0x04, 0x59, 0x5a,
    ];
    const HELLO: &[u8] = b"Hello, hajizip!\n";

    fn decode(bytes: &[u8]) -> crate::Result<Vec<u8>> {
        let codec = XzCodec;
        // `&[u8]` implements `Read` but not `Seek`, proving the streaming path
        // does not require a seekable (fully-buffered) input.
        let mut reader = codec.decompress(Box::new(bytes))?;
        let mut out = Vec::new();
        reader.read_to_end(&mut out)?;
        Ok(out)
    }

    #[test]
    fn decodes_known_vector() {
        assert_eq!(decode(HELLO_XZ).unwrap(), HELLO);
    }

    #[test]
    fn decodes_concatenated_streams() {
        let mut multi = HELLO_XZ.to_vec();
        multi.extend_from_slice(HELLO_XZ);
        assert_eq!(decode(&multi).unwrap(), [HELLO, HELLO].concat());
    }

    #[test]
    fn truncated_stream_errors() {
        assert!(decode(&HELLO_XZ[..20]).is_err());
    }

    #[test]
    fn corrupt_stream_errors() {
        let mut bad = HELLO_XZ.to_vec();
        // Flip a byte inside the LZMA2 payload (after the stream header).
        let mid = bad.len() / 2;
        bad[mid] ^= 0xff;
        let err = decode(&bad).unwrap_err();
        assert!(matches!(err, crate::error::Error::Io(_)));
    }

    #[test]
    fn compression_is_reserved() {
        let codec = XzCodec;
        let sink: Vec<u8> = Vec::new();
        let result = codec.compress(Box::new(sink), Level::default());
        assert!(matches!(
            result,
            Err(crate::error::Error::UnsupportedFeature(_))
        ));
    }

    #[test]
    fn detects_by_magic_and_extension() {
        let fmt = XzFormat;
        assert_eq!(fmt.id(), "xz");
        assert_eq!(fmt.display_name(), "XZ");
        assert_eq!(fmt.extensions(), &["xz", "txz"]);
        assert!(fmt.matches(&[0xfd, b'7', b'z', b'X', b'Z', 0x00], None));
        assert!(fmt.matches(b"junk", Some("xz")));
        assert!(!fmt.matches(b"junk", None));
        assert!(!fmt.matches(b"\x1f\x8b\x08", None));
        assert!(!fmt.matches(b"7z\xbc\xaf\x27\x1c", None));
    }

    #[test]
    fn registered_codec_is_detectable() {
        let reg = Registry::new().register_codec(XzFormat);
        assert_eq!(reg.codecs().len(), 1);
        let fmt = reg
            .detect_codec(&[0xfd, b'7', b'z', b'X', b'Z', 0x00], None)
            .expect("detected");
        assert_eq!(fmt.id(), "xz");
    }
}
