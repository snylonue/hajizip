//! Format detection and registry.

use crate::archive::{Archive, OpenOptions};
use crate::codec::Codec;
use crate::error::{Error, Result};
use crate::model::{CodecId, FormatKind};
use crate::source::Source;

/// Detects formats and constructs the appropriate readers / codecs.
pub trait FormatRegistry: Send + Sync {
    /// Detect a format from leading bytes (preferred) and an optional file
    /// extension (fallback).
    fn detect(&self, head: &[u8], ext: Option<&str>) -> Option<FormatKind>;

    /// Open an archive from a source.
    fn open_archive(&self, src: Source, opts: &OpenOptions) -> Result<Box<dyn Archive>>;

    /// Construct a codec by id.
    fn open_codec(&self, kind: CodecId) -> Result<Box<dyn Codec>>;
}

/// The default [`FormatRegistry`].
///
/// This is the concrete entry point applications use to open archives. Its
/// public signature is stable: as formats are implemented, only the internals
/// change, so callers (the GUI, a future CLI) never need to change.
#[derive(Debug, Default, Clone, Copy)]
pub struct Registry;

impl Registry {
    /// Create the default registry.
    pub fn new() -> Self {
        Self
    }
}

impl FormatRegistry for Registry {
    fn detect(&self, _head: &[u8], _ext: Option<&str>) -> Option<FormatKind> {
        // M0: format detection is not implemented yet.
        None
    }

    fn open_archive(&self, _src: Source, _opts: &OpenOptions) -> Result<Box<dyn Archive>> {
        Err(Error::UnsupportedFeature("Registry::open_archive".into()))
    }

    fn open_codec(&self, kind: CodecId) -> Result<Box<dyn Codec>> {
        Err(Error::UnsupportedFeature(format!(
            "Registry::open_codec({kind:?})"
        )))
    }
}

/// Open an archive from `src` using the default [`Registry`].
///
/// This is the primary entry point for applications; it hides the registry
/// entirely so callers depend only on this stable function.
pub fn open(src: Source, opts: &OpenOptions) -> Result<Box<dyn Archive>> {
    Registry::new().open_archive(src, opts)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_is_constructible_and_stubbed() {
        // The entry point exists and is callable; formats land later.
        assert!(Registry::new().detect(&[], None).is_none());
        assert!(open(Source::Memory(Vec::new()), &OpenOptions::default()).is_err());
    }
}
