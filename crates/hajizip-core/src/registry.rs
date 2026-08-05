//! Format registry: composes concrete formats into an opener.

use std::io::Read;
use std::sync::Arc;

use crate::archive::{Archive, IN_MEMORY_OPEN_CAP, OpenOptions};
use crate::error::{Error, Result};
use crate::format::{ArchiveFormat, CodecFormat};
use crate::source::Source;

/// Number of leading bytes sniffed to detect a format. Large enough for tar's
/// `ustar` magic at offset 257.
const SNIFF_BYTES: usize = 512;

/// Composes concrete formats and opens archives by auto-detection.
///
/// The registry type holds **no mandatory format list**: an application can
/// always compose a custom subset with [`Registry::new`] +
/// [`register_archive`](Self::register_archive) /
/// [`register_codec`](Self::register_codec) (e.g. a build that omits the
/// approved `unsafe`-exception formats). For the common case, core also ships
/// [`Registry::with_all_formats`] — the canonical composition of *every*
/// format core implements, which core keeps up to date as formats are added,
/// so front-ends can simply reference it instead of enumerating formats
/// themselves. Adding a format changes `with_all_formats`, never this type.
#[derive(Default)]
pub struct Registry {
    archive_formats: Vec<Arc<dyn ArchiveFormat>>,
    codecs: Vec<Arc<dyn CodecFormat>>,
}

impl Registry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a registry with **every format core implements** registered.
    ///
    /// This is the canonical, batteries-included composition that core
    /// maintains: when a new format lands in core it is added here, and every
    /// front-end that calls this constructor picks it up automatically (no
    /// per-application enumeration to keep in sync).
    ///
    /// It includes the formats that are approved `unsafe` exceptions (`tar`'s
    /// internal `unsafe`, 7z's C-FFI `zstd` method). Applications that need a
    /// restricted subset (for example a strictly pure-Rust build) should
    /// compose manually with [`Registry::new`] plus specific `register_*`
    /// calls instead.
    pub fn with_all_formats() -> Self {
        // The single place in core that enumerates the concrete formats.
        Self::new()
            .register_archive(crate::archive::zip::ZipFormat)
            .register_archive(crate::archive::tar::TarFormat)
            .register_archive(crate::archive::sevenz::SevenZipFormat)
            .register_archive(crate::archive::rar::RarFormat)
            .register_codec(crate::codec::gzip::GzipFormat)
            .register_codec(crate::codec::xz::XzFormat)
    }

    /// Register an archive format (builder-style).
    pub fn register_archive(mut self, format: impl ArchiveFormat + 'static) -> Self {
        self.archive_formats.push(Arc::new(format));
        self
    }

    /// Register a codec format (builder-style).
    pub fn register_codec(mut self, format: impl CodecFormat + 'static) -> Self {
        self.codecs.push(Arc::new(format));
        self
    }

    /// Registered archive formats (e.g. for the GUI to build menus and
    /// file-dialog filters).
    pub fn archive_formats(&self) -> &[Arc<dyn ArchiveFormat>] {
        &self.archive_formats
    }

    /// Registered codec formats.
    pub fn codecs(&self) -> &[Arc<dyn CodecFormat>] {
        &self.codecs
    }

    /// Detect the archive format matching the given leading bytes / extension.
    ///
    /// Magic bytes are preferred over the extension. The extension fallback
    /// is only consulted when the head gives no competing signal: if a
    /// registered codec matches the head (e.g. gzip magic), the bytes are
    /// compressed, not a direct archive (a `.tgz` is gzip, not tar).
    pub fn detect_archive(&self, head: &[u8], ext: Option<&str>) -> Option<Arc<dyn ArchiveFormat>> {
        if let Some(f) = self.archive_formats.iter().find(|f| f.matches(head, None)) {
            return Some(f.clone());
        }
        let compressed = self.codecs.iter().any(|c| c.matches(head, None));
        if !compressed
            && let Some(ext) = ext
            && let Some(f) = self
                .archive_formats
                .iter()
                .find(|f| f.matches(head, Some(ext)))
        {
            return Some(f.clone());
        }
        None
    }

    /// Detect the codec matching the given leading bytes / extension.
    pub fn detect_codec(&self, head: &[u8], ext: Option<&str>) -> Option<Arc<dyn CodecFormat>> {
        self.codecs.iter().find(|f| f.matches(head, ext)).cloned()
    }

    /// Open an archive from `src`, auto-detecting its format.
    ///
    /// Direct archive formats (zip, tar) are opened as-is. If no archive
    /// format matches but a registered codec does (e.g. `.tar.gz`), the
    /// stream is decompressed into a bounded in-memory buffer and the inner
    /// format is detected on the decompressed content.
    pub fn open_archive(&self, src: Source, opts: &OpenOptions) -> Result<Box<dyn Archive>> {
        let head = sniff(&src)?;
        let ext = src.extension();
        if let Some(format) = self.detect_archive(&head, ext.as_deref()) {
            return format.open(src, opts);
        }

        // Not a direct archive: try a registered codec and re-detect inside.
        if let Some(codec_fmt) = self.detect_codec(&head, ext.as_deref()) {
            let codec = codec_fmt.build();
            let mut reader = codec.decompress(src.open()?)?;
            let mut buf = Vec::new();
            reader
                .by_ref()
                .take(IN_MEMORY_OPEN_CAP + 1)
                .read_to_end(&mut buf)?;
            if buf.len() as u64 > IN_MEMORY_OPEN_CAP {
                return Err(Error::UnsupportedFeature(
                    "decompressed archive exceeds in-memory open cap".into(),
                ));
            }
            let inner_head = &buf[..buf.len().min(SNIFF_BYTES)];
            if let Some(format) = self.detect_archive(inner_head, None) {
                return format.open(Source::Memory(buf), opts);
            }
            return Err(Error::UnsupportedFormat(
                "decompressed content is not a recognized archive".into(),
            ));
        }

        Err(Error::UnsupportedFormat(
            "no registered format matched".into(),
        ))
    }
}

/// Read up to [`SNIFF_BYTES`] leading bytes from the source.
fn sniff(src: &Source) -> Result<Vec<u8>> {
    let mut buf = Vec::new();
    src.open()?.take(SNIFF_BYTES as u64).read_to_end(&mut buf)?;
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A throwaway format used only to exercise registration and detection.
    struct FakeFormat;

    impl ArchiveFormat for FakeFormat {
        fn id(&self) -> &str {
            "fake"
        }
        fn display_name(&self) -> &str {
            "Fake"
        }
        fn extensions(&self) -> &[&str] {
            &["fake"]
        }
        fn matches(&self, head: &[u8], _ext: Option<&str>) -> bool {
            head.starts_with(b"FAKE")
        }
        fn open(&self, _src: Source, _opts: &OpenOptions) -> Result<Box<dyn Archive>> {
            Err(Error::UnsupportedFeature("fake open".into()))
        }
    }

    #[test]
    fn empty_registry_detects_nothing() {
        let reg = Registry::new();
        assert!(reg.detect_archive(b"FAKEdata", None).is_none());
        assert!(
            reg.open_archive(Source::Memory(b"FAKE".to_vec()), &OpenOptions::default())
                .is_err()
        );
    }

    #[test]
    fn registered_format_is_detected() {
        let reg = Registry::new().register_archive(FakeFormat);
        assert_eq!(reg.archive_formats().len(), 1);
        let fmt = reg.detect_archive(b"FAKEdata", None).expect("detected");
        assert_eq!(fmt.id(), "fake");
        assert_eq!(fmt.display_name(), "Fake");
    }

    #[test]
    fn with_all_formats_registers_every_core_format() {
        let reg = Registry::with_all_formats();
        // Keep these counts in sync with the formats core implements: zip,
        // tar, 7z and rar archives; gzip and xz codecs.
        assert_eq!(reg.archive_formats().len(), 4, "zip + tar + 7z + rar");
        assert_eq!(reg.codecs().len(), 2, "gzip + xz");
        // Each one is reachable through auto-detection (magic bytes, or the
        // extension fallback for tar whose magic sits at offset 257).
        assert_eq!(
            reg.detect_archive(b"PK\x03\x04", None).expect("zip").id(),
            "zip"
        );
        assert_eq!(
            reg.detect_archive(b"7z\xbc\xaf\x27\x1c", None)
                .expect("7z")
                .id(),
            "7z"
        );
        assert_eq!(
            reg.detect_archive(b"Rar!\x1a\x07\x00", None)
                .expect("rar4")
                .id(),
            "rar"
        );
        assert_eq!(
            reg.detect_archive(b"Rar!\x1a\x07\x01\x00", None)
                .expect("rar5")
                .id(),
            "rar"
        );
        assert_eq!(
            reg.detect_archive(b"", Some("tar")).expect("tar").id(),
            "tar"
        );
        assert_eq!(
            reg.detect_codec(b"\x1f\x8b", None).expect("gzip").id(),
            "gzip"
        );
        assert_eq!(
            reg.detect_codec(b"\xfd7zXZ\x00", None).expect("xz").id(),
            "xz"
        );
    }

    #[test]
    fn registry_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Registry>();
    }
}
