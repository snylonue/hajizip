//! Format detection and registry.

use crate::archive::{Archive, OpenOptions};
use crate::codec::Codec;
use crate::error::Result;
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
