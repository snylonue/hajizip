//! Format-handler abstraction: concrete formats plug in here.
//!
//! The abstract core does **not** enumerate the supported formats. Each format
//! is an implementation of [`ArchiveFormat`] / [`CodecFormat`], carrying its
//! own identity (id / display name / extensions) and detection logic. Adding a
//! format means adding an implementation — no central enum to edit.
//!
//! Applications compose the formats they support into a [`crate::Registry`]
//! (the composition root, typically the GUI) by referencing the concrete
//! implementations.

use crate::archive::{Archive, OpenOptions};
use crate::codec::Codec;
use crate::error::Result;
use crate::source::Source;

/// A concrete archive format (e.g. zip, 7z, tar) that can detect and open
/// archives of its kind.
pub trait ArchiveFormat: Send + Sync {
    /// Canonical short identifier, e.g. `"zip"`, `"7z"`, `"tar"`.
    fn id(&self) -> &str;

    /// Human-readable display name, e.g. `"Zip"`, `"7-Zip"`.
    fn display_name(&self) -> &str;

    /// Typical file extensions without a leading dot, e.g. `["zip"]`.
    fn extensions(&self) -> &[&str];

    /// Whether this format can open the input, given leading bytes and an
    /// optional (lowercased) file extension.
    fn matches(&self, head: &[u8], ext: Option<&str>) -> bool;

    /// Open an archive from the source.
    fn open(&self, src: Source, opts: &OpenOptions) -> Result<Box<dyn Archive>>;
}

/// A concrete single-stream codec (e.g. gzip, xz) that can detect and build
/// itself.
pub trait CodecFormat: Send + Sync {
    /// Canonical short identifier, e.g. `"gzip"`, `"xz"`.
    fn id(&self) -> &str;

    /// Human-readable display name.
    fn display_name(&self) -> &str;

    /// Typical file extensions without a leading dot.
    fn extensions(&self) -> &[&str];

    /// Whether this codec applies to the input, given leading bytes and an
    /// optional (lowercased) file extension.
    fn matches(&self, head: &[u8], ext: Option<&str>) -> bool;

    /// Build the (stateless) codec.
    fn build(&self) -> Box<dyn Codec>;
}
