//! Archive abstraction for container formats that hold a file tree.

pub mod rar;
pub mod sevenz;
pub mod tar;
pub mod zip;

use std::collections::BTreeMap;
use std::io::{Cursor, Read, Write};
use std::sync::Arc;

use crate::codec::Codec;
use crate::encoding::{FilenameEncoding, Utf8Flag, decode_filename};
use crate::error::Result;
use crate::format::ArchiveFormat;
use crate::model::{EntryMeta, EntryPath, NodeKind, Secret};
use crate::source::Source;

/// Cap on bytes materialized in memory when opening a nested archive entry or
/// a decompressed codec stream (zip-bomb guard). Entries beyond this are
/// rejected; spill-to-temp-file is a future coordination point. Shared by
/// `open_nested_bytes` here and `Registry::open_archive`.
pub(crate) const IN_MEMORY_OPEN_CAP: u64 = 512 * 1024 * 1024;

/// Whether the head bytes look like a (POSIX/GNU) tar archive: `ustar` magic
/// at offset 257.
pub(crate) fn looks_like_tar(head: &[u8]) -> bool {
    head.get(257..262).is_some_and(|m| m == b"ustar")
}

/// Whether the head bytes look like a nested archive (zip, 7z, tar, gzip,
/// xz or rar). Used to mark entries as [`NodeKind::Archive`] so walk/Navigator
/// can recurse into them.
pub(crate) fn looks_like_nested_archive(head: &[u8]) -> bool {
    head.starts_with(b"PK\x03\x04")
        || head.starts_with(b"PK\x05\x06")
        || head.starts_with(b"7z\xbc\xaf\x27\x1c")
        || looks_like_tar(head)
        || head.starts_with(&[0x1f, 0x8b])
        || head.starts_with(&[0xfd, b'7', b'z', b'X', b'Z', 0x00])
        // RAR 1.5-4.x (7 bytes) and RAR 5+ (8 bytes) signatures.
        || head.starts_with(b"Rar!\x1a\x07\x00")
        || head.starts_with(b"Rar!\x1a\x07\x01\x00")
}

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

/// State shared by an open archive that backs [`DirNode`]/[`FileNode`].
///
/// Implementations are the per-format inner state structs (e.g. zip's
/// `ZipArchiveInner`); nodes hold an `Arc` to it so `NodeRef` stays `'static`.
pub(crate) trait ArchiveState: Send + Sync {
    /// Flat entry listing.
    fn entries(&self) -> &[EntryMeta];

    /// Read a file entry fully into memory (preview / nested-open path).
    fn read_entry_bytes(&self, meta: &EntryMeta) -> Result<Vec<u8>>;
}

/// A directory node in an archive tree (or the root when `path` is `None`).
pub(crate) struct DirNode<I: ArchiveState + 'static> {
    pub inner: Arc<I>,
    pub path: Option<EntryPath>,
    pub meta: EntryMeta,
}

impl<I: ArchiveState + 'static> Node for DirNode<I> {
    fn meta(&self) -> &EntryMeta {
        &self.meta
    }

    fn kind(&self) -> NodeKind {
        NodeKind::Dir
    }

    fn children(&self) -> Result<Vec<NodeRef>> {
        Ok(child_entries(self.inner.entries(), self.path.as_ref())
            .into_iter()
            .map(|meta| node_from_meta(self.inner.clone(), meta))
            .collect())
    }

    fn reader<'s>(&'s self) -> Result<Box<dyn Read + Send + 's>> {
        Ok(Box::new(Cursor::new(Vec::new())))
    }

    fn open_archive(&self, _opts: &OpenOptions) -> Result<Box<dyn Archive>> {
        Err(crate::error::Error::CorruptArchive(
            "directory is not an archive".into(),
        ))
    }
}

/// A file (or symlink) node in an archive tree.
pub(crate) struct FileNode<I: ArchiveState + 'static> {
    pub inner: Arc<I>,
    pub meta: EntryMeta,
}

impl<I: ArchiveState + 'static> Node for FileNode<I> {
    fn meta(&self) -> &EntryMeta {
        &self.meta
    }

    fn kind(&self) -> NodeKind {
        self.meta.kind
    }

    fn children(&self) -> Result<Vec<NodeRef>> {
        Ok(Vec::new())
    }

    fn reader<'s>(&'s self) -> Result<Box<dyn Read + Send + 's>> {
        Ok(Box::new(Cursor::new(
            self.inner.read_entry_bytes(&self.meta)?,
        )))
    }

    fn open_archive(&self, opts: &OpenOptions) -> Result<Box<dyn Archive>> {
        let bytes = self.inner.read_entry_bytes(&self.meta)?;
        open_nested_bytes(bytes, opts)
    }
}

/// Build a node for an entry from its metadata.
pub(crate) fn node_from_meta<I: ArchiveState + 'static>(inner: Arc<I>, meta: EntryMeta) -> NodeRef {
    if meta.kind == NodeKind::Dir {
        Box::new(DirNode {
            inner,
            path: Some(meta.path.clone()),
            meta,
        })
    } else {
        Box::new(FileNode { inner, meta })
    }
}

/// Open a nested archive from raw entry bytes, self-detecting the format.
///
/// This is the crate-internal dispatcher used by every format's
/// `Archive::open_nested` (zip-in-zip, tar.gz-in-zip, zip-in-tar, ...). It is
/// deliberately *not* part of the public API: the public composition root is
/// [`crate::registry::Registry`]. Adding a format extends this list in one
/// place (documented in `local-doc/research-zip.md` §5.2).
pub(crate) fn open_nested_bytes(bytes: Vec<u8>, opts: &OpenOptions) -> Result<Box<dyn Archive>> {
    if bytes.len() as u64 > IN_MEMORY_OPEN_CAP {
        return Err(crate::error::Error::UnsupportedFeature(
            "nested archive exceeds in-memory open cap".into(),
        ));
    }
    // Compressed single-stream: decompress and expect tar inside
    // (e.g. tar.gz / tar.xz inside an archive).
    if bytes.starts_with(&[0x1f, 0x8b]) {
        let mut reader = crate::codec::gzip::GzipCodec.decompress(Box::new(bytes.as_slice()))?;
        let mut inner = Vec::new();
        reader
            .by_ref()
            .take(IN_MEMORY_OPEN_CAP + 1)
            .read_to_end(&mut inner)?;
        if inner.len() as u64 > IN_MEMORY_OPEN_CAP {
            return Err(crate::error::Error::UnsupportedFeature(
                "nested archive exceeds in-memory open cap".into(),
            ));
        }
        if looks_like_tar(&inner) {
            return tar::TarFormat.open(Source::Memory(inner), opts);
        }
        return Err(crate::error::Error::UnsupportedFormat(
            "decompressed nested entry is not a recognized archive".into(),
        ));
    }
    if bytes.starts_with(&[0xfd, b'7', b'z', b'X', b'Z', 0x00]) {
        let mut reader = crate::codec::xz::XzCodec.decompress(Box::new(bytes.as_slice()))?;
        let mut inner = Vec::new();
        reader
            .by_ref()
            .take(IN_MEMORY_OPEN_CAP + 1)
            .read_to_end(&mut inner)?;
        if inner.len() as u64 > IN_MEMORY_OPEN_CAP {
            return Err(crate::error::Error::UnsupportedFeature(
                "nested archive exceeds in-memory open cap".into(),
            ));
        }
        if looks_like_tar(&inner) {
            return tar::TarFormat.open(Source::Memory(inner), opts);
        }
        return Err(crate::error::Error::UnsupportedFormat(
            "decompressed nested entry is not a recognized archive".into(),
        ));
    }
    if looks_like_tar(&bytes) {
        return tar::TarFormat.open(Source::Memory(bytes), opts);
    }
    if bytes.starts_with(b"PK\x03\x04") || bytes.starts_with(b"PK\x05\x06") {
        return zip::ZipFormat.open(Source::Memory(bytes), opts);
    }
    if bytes.starts_with(b"7z\xbc\xaf\x27\x1c") {
        return sevenz::SevenZipFormat.open(Source::Memory(bytes), opts);
    }
    if bytes.starts_with(b"Rar!\x1a\x07\x00") || bytes.starts_with(b"Rar!\x1a\x07\x01\x00") {
        return rar::RarFormat.open(Source::Memory(bytes), opts);
    }
    Err(crate::error::Error::UnsupportedFormat(
        "nested entry is not a recognized archive format".into(),
    ))
}

/// Synthetic root-node metadata. The archive root has no real entry; its path
/// is a reserved placeholder that never appears in listings.
pub(crate) fn root_meta() -> EntryMeta {
    EntryMeta {
        path: EntryPath::new("<archive root>").expect("reserved root path is valid"),
        raw_name: b"<archive root>".to_vec(),
        kind: NodeKind::Dir,
        uncompressed_size: None,
        compressed_size: None,
        mtime: None,
        mode: None,
        crc: None,
        encrypted: false,
        comment: None,
    }
}

/// Direct children of `focus` (`None` = root) within the flat listing.
///
/// Directories implied by path prefixes (e.g. `a/b.txt` implies dir `a`) are
/// synthesized when the archive does not list them explicitly. Dirs come
/// first, then alphabetical. This is the single source of truth for building
/// trees from a flat listing; the GUI delegates its `children_of` here (see
/// `local-doc/review-duplication.md` §1).
pub fn child_entries(entries: &[EntryMeta], focus: Option<&EntryPath>) -> Vec<EntryMeta> {
    let prefix = focus
        .map(|f| format!("{}/", f.as_str()))
        .unwrap_or_default();
    let mut by_name: BTreeMap<String, EntryMeta> = BTreeMap::new();

    // Explicit entries directly under `focus`.
    for e in entries {
        let p = e.path.as_str();
        if !p.starts_with(&prefix) {
            continue;
        }
        let rest = &p[prefix.len()..];
        if rest.is_empty() || rest.contains('/') {
            continue;
        }
        by_name.insert(rest.to_string(), e.clone());
    }

    // Directories implied by deeper paths, only if not explicit.
    for e in entries {
        let p = e.path.as_str();
        if !p.starts_with(&prefix) {
            continue;
        }
        let rest = &p[prefix.len()..];
        let Some((first, _)) = rest.split_once('/') else {
            continue;
        };
        by_name.entry(first.to_string()).or_insert_with(|| {
            let full = format!("{prefix}{first}");
            EntryMeta {
                path: EntryPath::new(&full).expect("implied dir path is valid"),
                raw_name: first.as_bytes().to_vec(),
                kind: NodeKind::Dir,
                uncompressed_size: None,
                compressed_size: None,
                mtime: None,
                mode: None,
                crc: None,
                encrypted: false,
                comment: None,
            }
        });
    }

    let mut out: Vec<EntryMeta> = by_name.into_values().collect();
    out.sort_by(|a, b| {
        let a_is_dir = a.kind == NodeKind::Dir;
        let b_is_dir = b.kind == NodeKind::Dir;
        b_is_dir
            .cmp(&a_is_dir)
            .then_with(|| a.path.as_str().cmp(b.path.as_str()))
    });
    out
}

/// Decode a raw entry name for listing.
///
/// Legacy codepages (GBK, Shift-JIS, ...) are not implemented yet (M3): names
/// that fail decoding fall back to lossy UTF-8 so the archive still lists.
/// The raw bytes are preserved in [`EntryMeta::raw_name`] for later
/// re-decoding once the encoding milestone lands.
pub(crate) fn decode_name(raw: &[u8], enc: FilenameEncoding) -> String {
    match decode_filename(raw, enc, Utf8Flag(false)) {
        Ok(s) => s,
        Err(_) => String::from_utf8_lossy(raw).into_owned(),
    }
}
