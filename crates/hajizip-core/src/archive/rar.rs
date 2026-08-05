//! RAR archive implementation backed by `rars`.
//!
//! Selection rationale, coverage and the accepted defect are documented in
//! `local-doc/research-rar.md`. `rars` covers the full family spectrum
//! (RE~^ / RAR 1.3-1.4, RAR 1.5-4.x including PPMd and encryption, RAR5 and
//! RAR7) with a workspace-level `forbid(unsafe_code)`; we use it with default
//! features only (no `parallel` / `fast`).
//!
//! Reading model: `rars` parses the whole archive index up front and decodes
//! members on demand. Non-solid archives seek to a member's packed range and
//! decode only it (`random_access = true`). Solid archives must decode the
//! whole solid chain to reach a member — the same cost model as solid 7z —
//! so `random_access = false` and single-member reads decode the chain and
//! keep only the target member's bytes.
//!
//! Known limitation: multivolume archives are listed from the first volume,
//! but split members are not readable without loading all volumes (the GUI
//! opens one file); reading them reports `UnsupportedFeature`.
//!
//! Known defect (accepted, see `local-doc/report-rar5-stored-encrypted.md`):
//! RAR5 stored (m0) + encrypted members are rejected by `rars` as
//! "RAR 5 encrypted stored file has non-zero padding" even for archives that
//! unrar can open. We surface it as `CorruptArchive` until upstream fixes it.

use std::collections::{HashMap, HashSet};
use std::io::{Cursor, Read, Write};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use crate::archive::{
    Archive, ArchiveState, Capabilities, DirNode, NodeKind, NodeRef, OpenOptions, node_from_meta,
    open_nested_bytes, root_meta,
};
use crate::encoding::{FilenameEncoding, Utf8Flag, decode_filename};
use crate::error::{Error, Result};
use crate::format::ArchiveFormat;
use crate::model::{EntryMeta, EntryPath, Secret};
use crate::source::Source;

/// RAR 1.5-4.x magic (`Rar!\x1a\x07\x00`, 7 bytes).
const RAR15_SIGNATURE: &[u8] = b"Rar!\x1a\x07\x00";
/// RAR 5+ magic (`Rar!\x1a\x07\x01\x00`, 8 bytes).
const RAR50_SIGNATURE: &[u8] = b"Rar!\x1a\x07\x01\x00";

/// Entries larger than this are never sniffed for nested-archive marking
/// (they are practically never archives themselves).
const SNIFF_ENTRY_CAP: u64 = 1024 * 1024;

/// Solid archives larger than this are not sniffed at open time: reading any
/// member of a solid chain decodes the whole chain, so eager marking would
/// materialize the entire archive. Small solid archives are still sniffed
/// with a single chain decode. Mirrors the 7z bounds.
const SOLID_SNIFF_ARCHIVE_CAP: u64 = 8 * 1024 * 1024;

/// Format identity and detection for RAR archives.
pub struct RarFormat;

impl ArchiveFormat for RarFormat {
    fn id(&self) -> &str {
        "rar"
    }

    fn display_name(&self) -> &str {
        "RAR"
    }

    fn extensions(&self) -> &[&str] {
        &["rar"]
    }

    fn matches(&self, head: &[u8], ext: Option<&str>) -> bool {
        head.starts_with(RAR15_SIGNATURE)
            || head.starts_with(RAR50_SIGNATURE)
            || matches!(ext, Some("rar"))
    }

    fn open(&self, src: Source, opts: &OpenOptions) -> Result<Box<dyn Archive>> {
        let options = rars_read_options(opts);
        let (archive, size) = match src {
            Source::Path(path) => {
                let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                (
                    rars::ArchiveReader::read_path_with_options(&path, options)
                        .map_err(map_error)?,
                    size,
                )
            }
            Source::Memory(bytes) => {
                let size = bytes.len() as u64;
                (
                    rars::ArchiveReader::read_owned_with_options(bytes, options)
                        .map_err(map_error)?,
                    size,
                )
            }
        };
        Ok(Box::new(RarArchive::open(archive, size, opts)?))
    }
}

/// An open RAR archive.
///
/// `rars`'s read API is `&self`-based (parsed index + on-demand member
/// decode), so no internal lock is needed; metadata is snapshotted into
/// `entries` at open time.
pub struct RarArchive {
    inner: Arc<RarInner>,
}

/// Shared state of an open RAR archive.
struct RarInner {
    archive: rars::Archive,
    entries: Vec<EntryMeta>,
    index_by_path: HashMap<EntryPath, usize>,
    is_solid: bool,
    encrypted: bool,
    needs_password: bool,
    /// Password captured at open time, replayed for member reads.
    password: Option<Secret>,
}

impl RarArchive {
    /// Snapshot metadata and indexes from a parsed RAR archive.
    fn open(archive: rars::Archive, total_size: u64, opts: &OpenOptions) -> Result<Self> {
        // Concrete file references in member order (all families expose the
        // same file-block sequence as `members()`); used for the RAR5
        // redirection (symlink/hardlink) check without re-scanning per entry.
        let redirections: Vec<bool> = match &archive {
            rars::Archive::Rar13(a) => a.entries.iter().map(|_| false).collect(),
            rars::Archive::Rar15To40(a) => a.files().map(|_| false).collect(),
            rars::Archive::Rar50Plus(a) => a.files().map(|f| f.is_redirection()).collect(),
            _ => {
                return Err(Error::UnsupportedFeature(
                    "unknown RAR archive family".into(),
                ));
            }
        };
        let mut entries = Vec::new();
        let mut index_by_path = HashMap::new();
        let mut encrypted = false;
        for (member, is_redirection) in archive.members().zip(redirections) {
            let m = &member.meta;
            let decoded = decode_entry_name(&m.name, opts.encoding, m.family);
            // Paths that fail validation (e.g. `../evil.txt`) are skipped; the
            // first line of path-traversal defense (architecture.md §4.8).
            let Ok(path) = EntryPath::new(&decoded) else {
                continue;
            };
            let kind = if m.is_directory {
                NodeKind::Dir
            } else if is_redirection {
                // RAR5 redirection entries (symlink / hardlink / file copy).
                // ExtractEngine never materializes symlinks (safety default).
                NodeKind::Symlink
            } else {
                NodeKind::File
            };
            encrypted |= m.is_encrypted;
            let crc = match &member.detail {
                rars::ArchiveMemberDetail::Rar15To40 { crc32, .. } => Some(*crc32),
                rars::ArchiveMemberDetail::Rar50Plus { crc32, .. } => *crc32,
                rars::ArchiveMemberDetail::Rar13 { .. } => None,
                _ => None,
            };
            let meta = EntryMeta {
                path: path.clone(),
                raw_name: m.name.clone(),
                kind,
                // Directories carry no meaningful size.
                uncompressed_size: (!m.is_directory).then_some(m.unpacked_size),
                compressed_size: (!m.is_directory).then_some(m.packed_size),
                mtime: m.file_time.and_then(dos_time_to_system_time),
                mode: None,
                crc,
                encrypted: m.is_encrypted,
                comment: None,
            };
            index_by_path.insert(path, entries.len());
            entries.push(meta);
        }

        let is_solid = archive.as_rar15_40().is_some_and(|a| a.main.is_solid())
            || archive.as_rar50().is_some_and(|a| a.main.is_solid())
            || archive.as_rar13().is_some_and(|a| a.main.is_solid());
        let needs_password = encrypted && opts.password.is_none();
        let password = opts.password.clone();

        let mut inner = RarInner {
            archive,
            entries,
            index_by_path,
            is_solid,
            encrypted,
            needs_password,
            password,
        };
        inner.sniff_nested(total_size);
        Ok(Self {
            inner: Arc::new(inner),
        })
    }
}

impl RarInner {
    fn locate(&self, meta: &EntryMeta) -> Result<usize> {
        self.index_by_path
            .get(&meta.path)
            .copied()
            .ok_or_else(|| Error::CorruptArchive(format!("no such entry in rar: {}", meta.path)))
    }

    /// The concrete file header for a member index (in `members()` order).
    fn file_at(&self, idx: usize) -> Result<RarFileRef<'_>> {
        match &self.archive {
            rars::Archive::Rar13(a) => a
                .entries
                .get(idx)
                .map(RarFileRef::Rar13)
                .ok_or_else(|| Error::CorruptArchive(format!("no such entry in rar: {idx}"))),
            rars::Archive::Rar15To40(a) => a
                .files()
                .nth(idx)
                .map(RarFileRef::Rar15To40)
                .ok_or_else(|| Error::CorruptArchive(format!("no such entry in rar: {idx}"))),
            rars::Archive::Rar50Plus(a) => a
                .files()
                .nth(idx)
                .map(RarFileRef::Rar50Plus)
                .ok_or_else(|| Error::CorruptArchive(format!("no such entry in rar: {idx}"))),
            _ => Err(Error::UnsupportedFeature(
                "unknown RAR archive family".into(),
            )),
        }
    }

    /// Decode one member with a fresh decoder (non-solid archives only).
    fn write_file_to(&self, file: RarFileRef<'_>, out: &mut dyn Write) -> Result<u64> {
        let password = self.password.as_ref().map(Secret::as_bytes);
        let mut counted = CountWriter {
            inner: out,
            count: 0,
        };
        match file {
            RarFileRef::Rar13(f) => {
                if f.is_split_before() || f.is_split_after() {
                    return Err(split_error());
                }
                f.write_to(
                    self.archive.as_rar13().expect("family matches"),
                    password,
                    &mut counted,
                )
                .map_err(map_error)?;
            }
            RarFileRef::Rar15To40(f) => {
                if f.is_split_before() || f.is_split_after() {
                    return Err(split_error());
                }
                f.write_to(
                    self.archive.as_rar15_40().expect("family matches"),
                    password,
                    &mut counted,
                )
                .map_err(map_error)?;
            }
            RarFileRef::Rar50Plus(f) => {
                if f.is_split_before() || f.is_split_after() {
                    return Err(split_error());
                }
                f.write_to(
                    self.archive.as_rar50().expect("family matches"),
                    password,
                    &mut counted,
                )
                .map_err(map_error)?;
            }
        }
        Ok(counted.count)
    }

    /// Decode a member by streaming the whole solid chain and keeping only the
    /// target member's bytes. `target` is the target's raw name; the first
    /// chain member with that name wins (duplicate names are pathological).
    fn write_solid_entry(&self, target: &[u8], sink: &mut dyn Write) -> Result<u64> {
        let password = self.password.as_ref().map(Secret::as_bytes);
        let captured: Arc<Mutex<Option<Vec<u8>>>> = Arc::new(Mutex::new(None));
        let cap = captured.clone();
        self.archive
            .extract_to(password, move |meta| {
                if meta.name == target {
                    return Ok(Box::new(CaptureWriter {
                        target: cap.clone(),
                    }) as Box<dyn Write>);
                }
                Ok(Box::new(std::io::sink()) as Box<dyn Write>)
            })
            .map_err(map_error)?;
        let bytes = captured
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
            .ok_or_else(|| {
                Error::CorruptArchive(format!("rar solid entry not found: {target:?}"))
            })?;
        sink.write_all(&bytes)?;
        Ok(bytes.len() as u64)
    }

    /// Read a file entry fully into memory (preview / nested-open path).
    fn read_entry_bytes(&self, meta: &EntryMeta) -> Result<Vec<u8>> {
        if meta.kind == NodeKind::Dir {
            return Ok(Vec::new());
        }
        let idx = self.locate(meta)?;
        if self.is_solid {
            let mut buf = Vec::new();
            self.write_solid_entry(&self.entries[idx].raw_name, &mut buf)?;
            Ok(buf)
        } else {
            let file = self.file_at(idx)?;
            let mut buf = Vec::new();
            self.write_file_to(file, &mut buf)?;
            Ok(buf)
        }
    }

    /// Extract a file entry into `sink`, returning the bytes written.
    fn extract_entry(&self, meta: &EntryMeta, sink: &mut dyn Write) -> Result<u64> {
        if meta.kind == NodeKind::Dir {
            return Ok(0);
        }
        let idx = self.locate(meta)?;
        if self.is_solid {
            self.write_solid_entry(&self.entries[idx].raw_name, sink)
        } else {
            let file = self.file_at(idx)?;
            self.write_file_to(file, sink)
        }
    }

    /// Mark entries that are themselves archives by sniffing the leading
    /// bytes of their decompressed content (walk/Navigator recurse into
    /// `NodeKind::Archive` entries).
    ///
    /// Non-solid archives decode each candidate member individually. Solid
    /// archives decode the whole chain per member, so only small solid
    /// archives are sniffed, with a single chain-wide pass that captures the
    /// head of each eligible member.
    fn sniff_nested(&mut self, total_size: u64) {
        let eligible: Vec<usize> = self
            .entries
            .iter()
            .enumerate()
            .filter(|(_, e)| {
                e.kind == NodeKind::File
                    && !e.encrypted
                    && e.uncompressed_size.unwrap_or(0) <= SNIFF_ENTRY_CAP
            })
            .map(|(i, _)| i)
            .collect();
        if eligible.is_empty() {
            return;
        }
        if !self.is_solid {
            for &idx in &eligible {
                let Ok(head) = self.read_head(idx, 512) else {
                    continue;
                };
                if crate::archive::looks_like_nested_archive(&head) {
                    self.entries[idx].kind = NodeKind::Archive;
                }
            }
            return;
        }
        if total_size > SOLID_SNIFF_ARCHIVE_CAP {
            return;
        }
        // One chain decode; capture the head of every eligible member.
        let wanted: HashSet<Vec<u8>> = eligible
            .iter()
            .map(|&idx| self.entries[idx].raw_name.clone())
            .collect();
        let heads: Arc<Mutex<HashMap<Vec<u8>, Vec<u8>>>> = Arc::new(Mutex::new(HashMap::new()));
        let heads_cap = heads.clone();
        let password = self.password.as_ref().map(Secret::as_bytes);
        let Ok(()) = self
            .archive
            .extract_to(password, move |meta| {
                if wanted.contains(&meta.name) {
                    let writer = LimitedWriter {
                        buf: Vec::new(),
                        limit: 512,
                    };
                    let shared = heads_cap.clone();
                    return Ok(Box::new(SharedHeadWriter {
                        inner: writer,
                        shared,
                        name: meta.name.clone(),
                    }) as Box<dyn Write>);
                }
                Ok(Box::new(std::io::sink()) as Box<dyn Write>)
            })
            .map_err(map_error)
        else {
            return;
        };
        let heads = heads.lock().unwrap_or_else(|e| e.into_inner());
        for &idx in &eligible {
            if heads
                .get(&self.entries[idx].raw_name)
                .is_some_and(|head| crate::archive::looks_like_nested_archive(head))
            {
                self.entries[idx].kind = NodeKind::Archive;
            }
        }
    }

    /// Read up to `n` leading decompressed bytes of a member (non-solid).
    fn read_head(&self, idx: usize, n: usize) -> Result<Vec<u8>> {
        let file = self.file_at(idx)?;
        let mut head = LimitedWriter {
            buf: Vec::new(),
            limit: n,
        };
        self.write_file_to(file, &mut head)?;
        Ok(head.buf)
    }
}

impl ArchiveState for RarInner {
    fn entries(&self) -> &[EntryMeta] {
        &self.entries
    }

    fn read_entry_bytes(&self, meta: &EntryMeta) -> Result<Vec<u8>> {
        self.read_entry_bytes(meta)
    }
}

impl Archive for RarArchive {
    fn entries(&self) -> Result<Vec<EntryMeta>> {
        Ok(self.inner.entries.clone())
    }

    fn root(&self) -> Result<NodeRef> {
        Ok(Box::new(DirNode {
            inner: self.inner.clone(),
            path: None,
            meta: root_meta(),
        }))
    }

    fn node(&self, path: &EntryPath) -> Result<NodeRef> {
        let idx = self
            .inner
            .index_by_path
            .get(path)
            .copied()
            .ok_or_else(|| Error::CorruptArchive(format!("no such entry in rar: {path}")))?;
        Ok(node_from_meta(
            self.inner.clone(),
            self.inner.entries[idx].clone(),
        ))
    }

    fn reader<'s>(&'s self, entry: &EntryMeta) -> Result<Box<dyn Read + Send + 's>> {
        if entry.kind == NodeKind::Dir {
            return Ok(Box::new(Cursor::new(Vec::new())));
        }
        Ok(Box::new(Cursor::new(self.inner.read_entry_bytes(entry)?)))
    }

    fn extract_to(&self, entry: &EntryMeta, sink: &mut dyn Write) -> Result<u64> {
        self.inner.extract_entry(entry, sink)
    }

    fn open_nested(&self, entry: &EntryMeta, opts: &OpenOptions) -> Result<Box<dyn Archive>> {
        let bytes = self.inner.read_entry_bytes(entry)?;
        open_nested_bytes(bytes, opts)
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            // Solid archives must decode the whole chain to reach a member.
            random_access: !self.inner.is_solid,
            encrypted: self.inner.encrypted,
            needs_password: self.inner.needs_password,
            can_write: false,
        }
    }
}

/// A concrete member reference used for single-member decoding.
enum RarFileRef<'a> {
    Rar13(&'a rars::rar13::Entry),
    Rar15To40(&'a rars::rar15_40::FileHeader),
    Rar50Plus(&'a rars::rar50::FileHeader),
}

/// A writer that counts bytes and forwards to an inner sink.
struct CountWriter<'a> {
    inner: &'a mut dyn Write,
    count: u64,
}

impl Write for CountWriter<'_> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.count += n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

/// A writer that captures the first `limit` bytes of a member into a buffer
/// (used for nested-archive sniffing of non-solid members).
struct LimitedWriter {
    buf: Vec<u8>,
    limit: usize,
}

impl Write for LimitedWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if self.buf.len() < self.limit {
            let room = self.limit - self.buf.len();
            self.buf.extend_from_slice(&buf[..buf.len().min(room)]);
        }
        // Report full consumption so the decoder keeps going; we only sniff.
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// A [`LimitedWriter`] that additionally forwards its captured head into a
/// shared map keyed by member name (used for the single-pass solid sniff).
struct SharedHeadWriter {
    inner: LimitedWriter,
    shared: Arc<Mutex<HashMap<Vec<u8>, Vec<u8>>>>,
    name: Vec<u8>,
}

impl Write for SharedHeadWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let n = self.inner.write(buf)?;
        if self.inner.buf.len() == self.inner.limit {
            self.shared
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(self.name.clone(), self.inner.buf.clone());
        }
        Ok(n)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// A writer that captures a member's full bytes into a shared slot.
struct CaptureWriter {
    target: Arc<Mutex<Option<Vec<u8>>>>,
}

impl Write for CaptureWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let mut target = self.target.lock().unwrap_or_else(|e| e.into_inner());
        target.get_or_insert_with(Vec::new).extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Build the rars read options from our open options.
fn rars_read_options(opts: &OpenOptions) -> rars::ArchiveReadOptions<'_> {
    match &opts.password {
        Some(secret) => rars::ArchiveReadOptions::with_password(secret.as_bytes()),
        None => rars::ArchiveReadOptions::new(),
    }
}

/// Decode a raw RAR entry name.
///
/// RAR5 names are UTF-8 by spec, so a strict UTF-8 hint applies there; RAR4
/// names have no specified encoding (older WinRAR wrote OEM codepages), so
/// the `Auto` fallback (UTF-8, else configured codepage) applies.
fn decode_entry_name(raw: &[u8], enc: FilenameEncoding, family: rars::ArchiveFamily) -> String {
    let ut8_ok = family == rars::ArchiveFamily::Rar50Plus;
    match decode_filename(raw, enc, Utf8Flag(ut8_ok)) {
        Ok(s) => s,
        Err(_) => String::from_utf8_lossy(raw).into_owned(),
    }
}

/// Convert a DOS/FAT timestamp (as stored by RAR file headers) to a
/// `SystemTime`. Invalid dates yield `None`.
fn dos_time_to_system_time(dos: u32) -> Option<SystemTime> {
    let year = 1980 + ((dos >> 25) & 0x7f) as i32;
    let month = ((dos >> 21) & 0x0f) as u8;
    let day = ((dos >> 16) & 0x1f) as u8;
    let hour = ((dos >> 11) & 0x1f) as u8;
    let minute = ((dos >> 5) & 0x3f) as u8;
    let second = (dos & 0x1f) as u8 * 2;
    let date =
        time::Date::from_calendar_date(year, time::Month::try_from(month).ok()?, day).ok()?;
    let time_of_day = time::Time::from_hms(hour, minute, second).ok()?;
    Some(SystemTime::from(date.with_time(time_of_day).assume_utc()))
}

/// Error for split members when only the first volume is loaded.
fn split_error() -> Error {
    Error::UnsupportedFeature("multivolume RAR extraction requires loading all volumes".into())
}

/// Map a `rars` error onto the core error model.
fn map_error(e: rars::Error) -> Error {
    use rars::Error as R;
    match e {
        R::NeedPassword => Error::PasswordRequired,
        R::WrongPasswordOrCorruptData => Error::WrongPassword,
        R::UnsupportedSignature | R::TooShort => {
            Error::UnsupportedFormat("bad RAR signature".into())
        }
        R::InvalidHeader(msg) => Error::CorruptArchive(format!("RAR: {msg}")),
        R::AtArchiveOffset { source, .. } | R::AtEntry { source, .. } => map_error(*source),
        R::UnsupportedVersion(v) => Error::UnsupportedFeature(format!("RAR version {v:?}")),
        R::UnsupportedFeature { version, feature } => {
            Error::UnsupportedFeature(format!("{feature} (RAR {version:?})"))
        }
        R::UnsupportedFamilyFeature { family, feature } => {
            Error::UnsupportedFeature(format!("{feature} (RAR {family:?})"))
        }
        R::UnsupportedCompression {
            family,
            unpack_version,
            method,
        } => Error::UnsupportedFeature(format!(
            "RAR {family} compression (unpack v{unpack_version}, method {method:#04x})"
        )),
        R::UnsupportedEncryption {
            family,
            unpack_version,
        } => Error::UnsupportedFeature(format!(
            "RAR {family} encryption (unpack v{unpack_version})"
        )),
        R::Rar50BufferedDecodeLimitExceeded { .. } => Error::UnsupportedFeature(
            "RAR 5 filtered member exceeds the buffered decode limit".into(),
        ),
        R::Io(e) => Error::Io(std::io::Error::new(e.kind, e.message)),
        R::Cancelled => Error::Cancelled,
        R::CrcMismatch { expected, actual } => Error::CorruptArchive(format!(
            "RAR checksum mismatch: expected {expected:#06x}, got {actual:#06x}"
        )),
        R::Crc32Mismatch { expected, actual } => Error::CorruptArchive(format!(
            "RAR CRC32 mismatch: expected {expected:#010x}, got {actual:#010x}"
        )),
        R::HashMismatch { .. } => Error::CorruptArchive("RAR hash mismatch".into()),
        R::Codec(e) => Error::CorruptArchive(format!("RAR codec: {e}")),
        R::Rar3Recovery(e) => Error::CorruptArchive(format!("RAR 3 recovery: {e}")),
        R::Rar5Recovery(e) => Error::CorruptArchive(format!("RAR 5 recovery: {e}")),
        R::Rar20Crypto(e) => Error::CorruptArchive(format!("RAR 2.0 crypto: {e}")),
        R::Rar30Crypto(e) => Error::CorruptArchive(format!("RAR 3.0 crypto: {e}")),
        R::Rar50Crypto(rars::crypto::rar50::Error::BadPassword) => Error::WrongPassword,
        R::Rar50Crypto(e) => Error::CorruptArchive(format!("RAR 5 crypto: {e}")),
        _ => Error::UnsupportedFeature(format!("RAR: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_by_magic_and_extension() {
        let fmt = RarFormat;
        assert_eq!(fmt.id(), "rar");
        assert_eq!(fmt.display_name(), "RAR");
        assert_eq!(fmt.extensions(), &["rar"]);
        assert!(fmt.matches(b"Rar!\x1a\x07\x00....", None));
        assert!(fmt.matches(b"Rar!\x1a\x07\x01\x00....", None));
        assert!(fmt.matches(b"junk", Some("rar")));
        assert!(!fmt.matches(b"Rar!\x1a\x07\x01", None));
        assert!(!fmt.matches(b"Rar", None));
        assert!(!fmt.matches(b"PK\x03\x04", None));
        assert!(!fmt.matches(b"junk", None));
    }

    #[test]
    fn dos_time_converts_to_system_time() {
        // 2024-02-29 12:34:56 local fields in DOS format: year 2024-1980=44,
        // month 2, day 29, hour 12, minute 34, second 56 (dos seconds = 28).
        let dos = (44u32 << 25) | (2 << 21) | (29 << 16) | (12 << 11) | (34 << 5) | 28;
        let st = dos_time_to_system_time(dos).expect("valid date");
        let expected = SystemTime::UNIX_EPOCH
            + std::time::Duration::from_secs(19_782 * 86_400 + 12 * 3_600 + 34 * 60 + 56);
        assert_eq!(st, expected);
    }

    #[test]
    fn invalid_dos_time_yields_none() {
        // Month 13 is invalid.
        let dos = (44u32 << 25) | (13 << 21) | (1 << 16);
        assert!(dos_time_to_system_time(dos).is_none());
    }
}
