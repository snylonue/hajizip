//! Archive abstraction for container formats that hold a file tree.

use std::io::{Read, Write};

use crate::encoding::FilenameEncoding;
use crate::error::Result;
use crate::model::{EntryMeta, EntryPath, NodeKind, Secret};

/// Options controlling how an archive is opened.
#[derive(Debug, Clone, Default)]
pub struct OpenOptions {
    /// Password for encrypted archives, if any.
    pub password: Option<Secret>,
    /// Filename decoding strategy.
    pub encoding: FilenameEncoding,
}

/// Describes what an archive implementation can do.
#[derive(Debug, Clone, Copy)]
pub struct Capabilities {
    /// Whether entries can be accessed randomly (vs. sequential-only).
    pub random_access: bool,
    /// Whether the archive contains any encrypted entries.
    pub encrypted: bool,
    /// Whether a password is required to proceed.
    pub needs_password: bool,
    /// Whether writing/creation is supported.
    pub can_write: bool,
}

/// An owned handle to any node (file / dir / nested archive) in the tree.
///
/// This is `Box<dyn Node + 'static>`: a node does not borrow the archive but
/// typically holds a shared (`Arc`) handle to it, so callers (e.g. the GUI)
/// may store nodes freely.
pub type NodeRef = Box<dyn Node>;

/// A node within an archive tree.
///
/// `Dir` and `Archive` nodes both support [`Node::children`]; only `Archive`
/// nodes additionally support [`Node::open_archive`], which is what enables
/// recursive navigation into nested archives.
pub trait Node: Send {
    /// Metadata for this node.
    fn meta(&self) -> &EntryMeta;

    /// The kind of this node.
    fn kind(&self) -> NodeKind;

    /// List children (for `Dir` and `Archive` nodes).
    fn children(&self) -> Result<Vec<NodeRef>>;

    /// A reader over the decompressed content (for `File` nodes).
    fn reader<'s>(&'s self) -> Result<Box<dyn Read + Send + 's>>;

    /// Open this node as a nested archive (for `Archive` nodes).
    fn open_archive(&self, opts: &OpenOptions) -> Result<Box<dyn Archive>>;
}

/// A readable archive container (zip / 7z / tar / ...).
pub trait Archive: Send + Sync {
    /// Flat listing of all entry metadata (no content).
    fn entries(&self) -> Result<Vec<EntryMeta>>;

    /// The root directory node.
    fn root(&self) -> Result<NodeRef>;

    /// Locate a node by path.
    fn node(&self, path: &EntryPath) -> Result<NodeRef>;

    /// A reader over an entry's decompressed bytes.
    fn reader<'s>(&'s self, entry: &EntryMeta) -> Result<Box<dyn Read + Send + 's>>;

    /// Extract a single entry into `sink`, returning the number of bytes written.
    fn extract_to(&self, entry: &EntryMeta, sink: &mut dyn Write) -> Result<u64>;

    /// Open an entry that is itself an archive, returning an owned archive.
    ///
    /// The result is independent of `self` (`'static`): implementations read
    /// the entry's bytes into memory or a temporary file (per the configured
    /// threshold) and parse them. This enables recursive navigation into
    /// nested archives without lifetime entanglement, and is what `Navigator`
    /// and the GUI use to descend into nested archives.
    fn open_nested(&self, entry: &EntryMeta, opts: &OpenOptions) -> Result<Box<dyn Archive>>;

    /// Query the capabilities of this archive.
    fn capabilities(&self) -> Capabilities;
}
