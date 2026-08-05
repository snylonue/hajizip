//! TAR archive implementation backed by the `tar` crate (tar-rs).
//!
//! Selection rationale and the random-access design (memory index + seek,
//! 方案 A) are documented in `local-doc/research-tar.md`.
//!
//! tar is a sequential format: at open time we scan the headers once
//! (`entries_with_seek`, seeking over file contents) and build an in-memory
//! index of each entry's raw file offset and size. Reading an entry then
//! seeks directly to its data — O(1) random access on a seekable source.
//! Compressed tars (`.tar.gz`) are materialized by the `Registry` before
//! being opened here (see `crate::registry::Registry::open_archive`).

use std::collections::HashMap;
use std::io::{Read, Seek, Write};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, SystemTime};

use crate::archive::{
    Archive, ArchiveAdapter, ArchiveState, Capabilities, NodeKind, OpenOptions, decode_name,
};
use crate::error::{Error, Result};
use crate::format::ArchiveFormat;
use crate::model::{EntryMeta, EntryPath};
use crate::source::{ReadSeek, Source};

/// Format identity and detection for TAR archives.
pub struct TarFormat;

impl ArchiveFormat for TarFormat {
    fn id(&self) -> &str {
        "tar"
    }

    fn display_name(&self) -> &str {
        "Tar"
    }

    fn extensions(&self) -> &[&str] {
        &["tar", "tgz"]
    }

    fn matches(&self, head: &[u8], ext: Option<&str>) -> bool {
        crate::archive::looks_like_tar(head) || matches!(ext, Some("tar" | "tgz"))
    }

    fn open(&self, src: Source, opts: &OpenOptions) -> Result<Box<dyn Archive>> {
        let reader = src.open()?;
        Ok(Box::new(ArchiveAdapter::new(
            TarArchiveInner::open(reader, opts)?,
            "tar",
        )))
    }
}

/// Shared state of an open tar archive.
///
/// The raw source stays behind a `Mutex` because random access mutates the
/// shared seek cursor; entry metadata is snapshotted at open time so listing
/// never touches the lock.
struct TarArchiveInner {
    src: Mutex<Box<dyn ReadSeek + Send>>,
    /// Entry metadata, in archive order (parallel to `records`).
    entries: Vec<EntryMeta>,
    /// Data position and size per entry.
    records: Vec<TarRecord>,
    by_path: HashMap<EntryPath, usize>,
}

/// Data location of a single tar entry.
struct TarRecord {
    file_pos: u64,
    size: u64,
}

impl TarArchiveInner {
    /// Scan the tar headers once and build the in-memory index.
    fn open(mut src: Box<dyn ReadSeek + Send>, opts: &OpenOptions) -> Result<Self> {
        src.seek(std::io::SeekFrom::Start(0))?;
        let mut archive = tar::Archive::new(src);
        let mut entries = Vec::new();
        let mut records = Vec::new();
        let mut by_path = HashMap::new();

        for entry in archive.entries_with_seek()? {
            let entry = entry?;
            let raw_name = entry.path_bytes().into_owned();
            let decoded = decode_name(&raw_name, opts.encoding);
            // Entries with invalid paths (`..` traversal) are skipped; leading
            // separators are normalized away by `EntryPath` (absolute paths
            // become relative, architecture.md §4.8).
            let Ok(path) = EntryPath::new(&decoded) else {
                continue;
            };
            let size = entry.size();
            let file_pos = entry.raw_file_position();
            let kind = match entry.header().entry_type() {
                tar::EntryType::Directory => NodeKind::Dir,
                tar::EntryType::Symlink => NodeKind::Symlink,
                // Regular files, hard links, devices, ... are all listed as
                // files (special handling is a later milestone).
                _ => NodeKind::File,
            };
            let mtime = entry
                .header()
                .mtime()
                .ok()
                .map(|s| SystemTime::UNIX_EPOCH + Duration::from_secs(s));
            let meta = EntryMeta {
                path: path.clone(),
                raw_name,
                kind,
                uncompressed_size: Some(size),
                compressed_size: None,
                mtime,
                mode: entry.header().mode().ok(),
                crc: None,
                encrypted: false,
                comment: None,
            };
            by_path.insert(path, entries.len());
            entries.push(meta);
            records.push(TarRecord { file_pos, size });
        }

        let mut src = archive.into_inner();

        // Second pass: mark entries that are themselves archives by sniffing
        // the first bytes of their data (walk/Navigator recurse into
        // `NodeKind::Archive` entries).
        for i in 0..entries.len() {
            if entries[i].kind != NodeKind::File {
                continue;
            }
            src.seek(std::io::SeekFrom::Start(records[i].file_pos))?;
            let mut head = [0u8; 512];
            let n = src.read(&mut head)?;
            if crate::archive::looks_like_nested_archive(&head[..n]) {
                entries[i].kind = NodeKind::Archive;
            }
        }

        Ok(Self {
            src: Mutex::new(src),
            entries,
            records,
            by_path,
        })
    }
}

impl TarArchiveInner {
    /// Lock the source, recovering from mutex poisoning.
    fn lock(&self) -> MutexGuard<'_, Box<dyn ReadSeek + Send>> {
        self.src.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn locate(&self, meta: &EntryMeta) -> Result<(u64, u64)> {
        let idx =
            self.by_path.get(&meta.path).copied().ok_or_else(|| {
                Error::CorruptArchive(format!("no such entry in tar: {}", meta.path))
            })?;
        Ok((self.records[idx].file_pos, self.records[idx].size))
    }
}

impl ArchiveState for TarArchiveInner {
    fn entries(&self) -> &[EntryMeta] {
        &self.entries
    }

    fn index_of(&self, path: &EntryPath) -> Option<usize> {
        self.by_path.get(path).copied()
    }

    fn read_entry_bytes(&self, meta: &EntryMeta) -> Result<Vec<u8>> {
        if meta.kind == NodeKind::Dir {
            return Ok(Vec::new());
        }
        let (pos, size) = self.locate(meta)?;
        let mut guard = self.lock();
        guard.seek(std::io::SeekFrom::Start(pos))?;
        let mut buf = Vec::with_capacity(size.min(64 * 1024 * 1024) as usize);
        guard.by_ref().take(size).read_to_end(&mut buf)?;
        Ok(buf)
    }

    fn extract_to(&self, meta: &EntryMeta, sink: &mut dyn Write) -> Result<u64> {
        if meta.kind == NodeKind::Dir {
            return Ok(0);
        }
        let (pos, size) = self.locate(meta)?;
        let mut guard = self.lock();
        guard.seek(std::io::SeekFrom::Start(pos))?;
        Ok(std::io::copy(&mut guard.by_ref().take(size), sink)?)
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            random_access: true,
            encrypted: false,
            needs_password: false,
            can_write: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_by_magic_and_extension() {
        let fmt = TarFormat;
        assert_eq!(fmt.id(), "tar");
        assert_eq!(fmt.display_name(), "Tar");
        assert_eq!(fmt.extensions(), &["tar", "tgz"]);
        // POSIX magic "ustar\0" at offset 257.
        let mut head = [0u8; 512];
        head[257..262].copy_from_slice(b"ustar");
        assert!(fmt.matches(&head, None));
        // GNU magic "ustar " (six bytes, shared prefix; version follows at 263).
        let mut head = [0u8; 512];
        head[257..263].copy_from_slice(b"ustar ");
        assert!(fmt.matches(&head, None));
        assert!(fmt.matches(b"junk", Some("tar")));
        assert!(!fmt.matches(&[0u8; 512], None));
        assert!(!fmt.matches(b"\x1f\x8b...", None));
        assert!(!fmt.matches(b"PK\x03\x04", None));
    }
}
