//! ZIP archive implementation backed by the `zip` crate (zip-rs).
//!
//! Selection rationale and feature configuration are documented in
//! `local-doc/research-zip.md`. The `deflate-flate2` feature keeps DEFLATE on
//! the already-approved miniz_oxide backend (pure safe Rust) instead of the
//! crate's default zlib-rs backend.

use std::collections::{BTreeMap, HashMap};
use std::io::{Cursor, Read, Write};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, SystemTime};

use crate::archive::{Archive, Capabilities, Node, NodeKind, NodeRef, OpenOptions};
use crate::encoding::{FilenameEncoding, Utf8Flag, decode_filename};
use crate::error::{Error, Result};
use crate::format::ArchiveFormat;
use crate::model::{EntryMeta, EntryPath};
use crate::source::{ReadSeek, Source};

/// Cap on bytes read into memory when opening a nested archive entry
/// (zip-bomb guard: nested entries beyond this are rejected).
const NESTED_OPEN_CAP: u64 = 512 * 1024 * 1024;

/// Zip local-file-header magic (`PK\x03\x04`).
const ZIP_LOCAL_HEADER: &[u8] = b"PK\x03\x04";
/// Zip end-of-central-directory magic (`PK\x05\x06`, empty archives).
const ZIP_EMPTY_ARCHIVE: &[u8] = b"PK\x05\x06";

/// Format identity and detection for ZIP archives.
pub struct ZipFormat;

impl ArchiveFormat for ZipFormat {
    fn id(&self) -> &str {
        "zip"
    }

    fn display_name(&self) -> &str {
        "Zip"
    }

    fn extensions(&self) -> &[&str] {
        &["zip"]
    }

    fn matches(&self, head: &[u8], ext: Option<&str>) -> bool {
        head.starts_with(ZIP_LOCAL_HEADER)
            || head.starts_with(ZIP_EMPTY_ARCHIVE)
            || matches!(ext, Some("zip"))
    }

    fn open(&self, src: Source, opts: &OpenOptions) -> Result<Box<dyn Archive>> {
        let reader = src.open()?;
        let inner = zip::ZipArchive::new(reader).map_err(map_zip_err)?;
        Ok(Box::new(ZipArchive::new(inner, opts)?))
    }
}

/// An open ZIP archive.
///
/// Metadata is snapshotted at open time so listing never touches the lock.
/// The `zip` reader API needs `&mut self` and borrows the archive, which does
/// not fit the `Archive` trait (`&self` + `Send + Sync`); an internal `Mutex`
/// serializes access. Reading is safe to share across threads.
pub struct ZipArchive {
    inner: Arc<ZipArchiveInner>,
}

/// Shared state of an open zip archive.
struct ZipArchiveInner {
    inner: Mutex<zip::ZipArchive<Box<dyn ReadSeek + Send>>>,
    entries: Vec<EntryMeta>,
    index_by_path: HashMap<EntryPath, usize>,
    any_encrypted: bool,
}

impl ZipArchive {
    /// Snapshot metadata and indexes from a parsed zip archive.
    fn new(archive: zip::ZipArchive<Box<dyn ReadSeek + Send>>, opts: &OpenOptions) -> Result<Self> {
        let mut archive = archive;
        let mut entries = Vec::with_capacity(archive.len());
        let mut index_by_path = HashMap::new();
        let mut any_encrypted = false;

        for i in 0..archive.len() {
            // `by_index_raw` avoids the password check so encrypted archives
            // can still be listed (reads are rejected later, see `with_file`).
            let file = archive.by_index_raw(i).map_err(map_zip_err)?;
            let raw_name = file.name_raw().to_vec();
            let decoded = decode_name(&raw_name, opts.encoding);
            // Entries whose paths fail validation (e.g. `../evil.txt`) cannot
            // be extracted safely; they are skipped from the listing (first
            // line of zip-slip defense, architecture.md §4.8).
            let Ok(path) = EntryPath::new(&decoded) else {
                continue;
            };
            let kind = if file.is_dir() {
                NodeKind::Dir
            } else if file.is_symlink() {
                NodeKind::Symlink
            } else {
                NodeKind::File
            };
            let mtime = file
                .last_modified()
                .filter(|dt| dt.is_valid())
                .map(|dt| zip_datetime_to_system_time(&dt));
            let encrypted = file.encrypted();
            any_encrypted |= encrypted;
            let meta = EntryMeta {
                path: path.clone(),
                raw_name,
                kind,
                uncompressed_size: Some(file.size()),
                compressed_size: Some(file.compressed_size()),
                mtime,
                mode: file.unix_mode(),
                crc: Some(file.crc32()),
                encrypted,
                comment: None,
            };
            index_by_path.insert(path, entries.len());
            entries.push(meta);
        }

        Ok(Self {
            inner: Arc::new(ZipArchiveInner {
                inner: Mutex::new(archive),
                entries,
                index_by_path,
                any_encrypted,
            }),
        })
    }
}

impl ZipArchiveInner {
    /// Lock the underlying zip reader, recovering from mutex poisoning (a
    /// panic inside the lock cannot corrupt the archive for later calls).
    fn lock(&self) -> MutexGuard<'_, zip::ZipArchive<Box<dyn ReadSeek + Send>>> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn index_of(&self, path: &EntryPath) -> Result<usize> {
        self.index_by_path
            .get(path)
            .copied()
            .ok_or_else(|| Error::CorruptArchive(format!("no such entry in zip: {path}")))
    }

    /// Run `f` with the decompressing reader of `meta` opened.
    ///
    /// Directory entries yield nothing; encrypted entries are not readable in
    /// M1 (decryption support is deferred, see `research-zip.md` §5).
    fn with_file<T>(
        &self,
        meta: &EntryMeta,
        f: impl FnOnce(&mut zip::read::ZipFile<'_, Box<dyn ReadSeek + Send>>) -> Result<T>,
    ) -> Result<T> {
        if meta.kind == NodeKind::Dir {
            return Err(Error::CorruptArchive(format!(
                "entry is a directory: {}",
                meta.path
            )));
        }
        let idx = self.index_of(&meta.path)?;
        if self.entries[idx].encrypted {
            return Err(Error::UnsupportedFeature("encrypted zip entries".into()));
        }
        let mut guard = self.lock();
        let mut file = guard.by_index(idx).map_err(map_zip_err)?;
        f(&mut file)
    }

    /// Read a file entry fully into memory (preview / nested-open path).
    fn read_to_vec(&self, meta: &EntryMeta) -> Result<Vec<u8>> {
        let mut buf = Vec::new();
        self.with_file(meta, |file| {
            file.read_to_end(&mut buf)?;
            Ok(())
        })?;
        Ok(buf)
    }

    /// Stream a file entry into `sink`, returning the bytes written.
    fn extract_to(&self, meta: &EntryMeta, sink: &mut dyn Write) -> Result<u64> {
        self.with_file(meta, |file| Ok(std::io::copy(file, sink)?))
    }
}

impl Archive for ZipArchive {
    fn entries(&self) -> Result<Vec<EntryMeta>> {
        Ok(self.inner.entries.clone())
    }

    fn root(&self) -> Result<NodeRef> {
        Ok(Box::new(ZipDirNode {
            inner: self.inner.clone(),
            path: None,
            meta: root_meta(),
        }))
    }

    fn node(&self, path: &EntryPath) -> Result<NodeRef> {
        let idx = self.inner.index_of(path)?;
        Ok(node_from_meta(
            self.inner.clone(),
            self.inner.entries[idx].clone(),
        ))
    }

    fn reader<'s>(&'s self, entry: &EntryMeta) -> Result<Box<dyn Read + Send + 's>> {
        if entry.kind == NodeKind::Dir {
            return Ok(Box::new(Cursor::new(Vec::new())));
        }
        Ok(Box::new(Cursor::new(self.inner.read_to_vec(entry)?)))
    }

    fn extract_to(&self, entry: &EntryMeta, sink: &mut dyn Write) -> Result<u64> {
        if entry.kind == NodeKind::Dir {
            return Ok(0);
        }
        self.inner.extract_to(entry, sink)
    }

    fn open_nested(&self, entry: &EntryMeta, opts: &OpenOptions) -> Result<Box<dyn Archive>> {
        let bytes = self.inner.read_to_vec(entry)?;
        if bytes.len() as u64 > NESTED_OPEN_CAP {
            return Err(Error::UnsupportedFeature(
                "nested archive exceeds in-memory open cap".into(),
            ));
        }
        // M1: only zip-in-zip is recognized (other formats need the
        // registry-composition design, see `research-zip.md` §5.2).
        if !(bytes.starts_with(ZIP_LOCAL_HEADER) || bytes.starts_with(ZIP_EMPTY_ARCHIVE)) {
            return Err(Error::UnsupportedFormat(
                "nested entry is not a recognized archive format".into(),
            ));
        }
        ZipFormat.open(Source::Memory(bytes), opts)
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            random_access: true,
            encrypted: self.inner.any_encrypted,
            // M1: encrypted entries are not readable, so a password would not
            // help; `needs_password` stays false until M3 adds decryption.
            needs_password: false,
            can_write: false,
        }
    }
}

/// A directory node in the zip tree (or the archive root when `path` is
/// `None`).
struct ZipDirNode {
    inner: Arc<ZipArchiveInner>,
    path: Option<EntryPath>,
    meta: EntryMeta,
}

impl Node for ZipDirNode {
    fn meta(&self) -> &EntryMeta {
        &self.meta
    }

    fn kind(&self) -> NodeKind {
        NodeKind::Dir
    }

    fn children(&self) -> Result<Vec<NodeRef>> {
        Ok(child_entries(&self.inner.entries, self.path.as_ref())
            .into_iter()
            .map(|meta| node_from_meta(self.inner.clone(), meta))
            .collect())
    }

    fn reader<'s>(&'s self) -> Result<Box<dyn Read + Send + 's>> {
        Ok(Box::new(Cursor::new(Vec::new())))
    }

    fn open_archive(&self, _opts: &OpenOptions) -> Result<Box<dyn Archive>> {
        Err(Error::CorruptArchive("directory is not an archive".into()))
    }
}

/// A file (or symlink) node in the zip tree.
struct ZipFileNode {
    inner: Arc<ZipArchiveInner>,
    meta: EntryMeta,
}

impl Node for ZipFileNode {
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
        Ok(Box::new(Cursor::new(self.inner.read_to_vec(&self.meta)?)))
    }

    fn open_archive(&self, opts: &OpenOptions) -> Result<Box<dyn Archive>> {
        let bytes = self.inner.read_to_vec(&self.meta)?;
        if bytes.len() as u64 > NESTED_OPEN_CAP {
            return Err(Error::UnsupportedFeature(
                "nested archive exceeds in-memory open cap".into(),
            ));
        }
        if !(bytes.starts_with(ZIP_LOCAL_HEADER) || bytes.starts_with(ZIP_EMPTY_ARCHIVE)) {
            return Err(Error::UnsupportedFormat(
                "nested entry is not a recognized archive format".into(),
            ));
        }
        ZipFormat.open(Source::Memory(bytes), opts)
    }
}

/// Build a node for an entry (or a synthesized child) from its metadata.
fn node_from_meta(inner: Arc<ZipArchiveInner>, meta: EntryMeta) -> NodeRef {
    if meta.kind == NodeKind::Dir {
        Box::new(ZipDirNode {
            inner,
            path: Some(meta.path.clone()),
            meta,
        })
    } else {
        Box::new(ZipFileNode { inner, meta })
    }
}

/// Synthetic root-node metadata. The archive root has no real entry; its path
/// is a reserved placeholder that never appears in listings.
fn root_meta() -> EntryMeta {
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
/// synthesized when the archive does not list them explicitly, mirroring the
/// GUI's client-side tree builder. Dirs come first, then alphabetical.
fn child_entries(entries: &[EntryMeta], focus: Option<&EntryPath>) -> Vec<EntryMeta> {
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
/// Legacy codepages (GBK, Shift-JIS, ...) are not implemented yet (M3):
/// names that fail decoding fall back to lossy UTF-8 so the archive still
/// lists. The raw bytes are preserved in [`EntryMeta::raw_name`] for later
/// re-decoding once the encoding milestone lands.
fn decode_name(raw: &[u8], enc: FilenameEncoding) -> String {
    match decode_filename(raw, enc, Utf8Flag(false)) {
        Ok(s) => s,
        Err(_) => String::from_utf8_lossy(raw).into_owned(),
    }
}

/// Convert a zip MS-DOS timestamp to a `SystemTime`.
fn zip_datetime_to_system_time(dt: &zip::DateTime) -> SystemTime {
    let days = days_from_civil(
        i64::from(dt.year()),
        u32::from(dt.month()),
        u32::from(dt.day()),
    );
    let secs = days * 86_400
        + i64::from(dt.hour()) * 3_600
        + i64::from(dt.minute()) * 60
        + i64::from(dt.second());
    SystemTime::UNIX_EPOCH + Duration::from_secs(secs as u64)
}

/// Days since 1970-01-01 for a civil date (Howard Hinnant's algorithm).
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = i64::from(if m > 2 { m - 3 } else { m + 9 });
    let doy = (153 * mp + 2) / 5 + i64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Map a `zip` crate error onto the core error model.
fn map_zip_err(e: zip::result::ZipError) -> Error {
    match e {
        zip::result::ZipError::Io(io) => Error::Io(io),
        zip::result::ZipError::InvalidArchive(msg) => Error::CorruptArchive(msg.into_owned()),
        zip::result::ZipError::UnsupportedArchive(msg) => Error::UnsupportedFeature(msg.to_owned()),
        zip::result::ZipError::FileNotFound => {
            Error::CorruptArchive("entry not found in zip archive".into())
        }
        zip::result::ZipError::InvalidPassword => Error::WrongPassword,
        // `ZipError` is `#[non_exhaustive]`; treat unknown future variants as
        // corrupt input rather than panicking.
        _ => Error::CorruptArchive("unknown zip error".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_by_magic_and_extension() {
        let fmt = ZipFormat;
        assert_eq!(fmt.id(), "zip");
        assert_eq!(fmt.display_name(), "Zip");
        assert_eq!(fmt.extensions(), &["zip"]);
        assert!(fmt.matches(b"PK\x03\x04....", None));
        assert!(fmt.matches(b"PK\x05\x06....", None));
        assert!(fmt.matches(b"junk", Some("zip")));
        assert!(!fmt.matches(b"PK\x03", None));
        assert!(!fmt.matches(b"\x1f\x8b...", None));
        assert!(!fmt.matches(b"junk", None));
    }

    /// Cross-check the civil-date conversion against known epoch values.
    #[test]
    fn days_from_civil_matches_known_epochs() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(1980, 1, 1), 3652); // zip MS-DOS epoch
        assert_eq!(days_from_civil(2000, 1, 1), 10_957);
        assert_eq!(days_from_civil(2024, 2, 29), 19_782); // leap day
    }

    #[test]
    fn zip_datetime_converts_to_system_time() {
        // 2024-02-29 12:34:56 (a valid MS-DOS date, 1980..2107).
        let dt = zip::DateTime::from_date_and_time(2024, 2, 29, 12, 34, 56).expect("valid date");
        let st = zip_datetime_to_system_time(&dt);
        let expected = SystemTime::UNIX_EPOCH
            + Duration::from_secs(19_782 * 86_400 + 12 * 3_600 + 34 * 60 + 56);
        assert_eq!(st, expected);
    }
}
