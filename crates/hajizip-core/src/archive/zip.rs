//! ZIP archive implementation backed by the `zip` crate (zip-rs).
//!
//! Selection rationale and feature configuration are documented in
//! `local-doc/research-zip.md`. The `deflate-flate2` feature keeps DEFLATE on
//! the already-approved miniz_oxide backend (pure safe Rust) instead of the
//! crate's default zlib-rs backend.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::{Mutex, MutexGuard};
use std::time::SystemTime;

use crate::archive::{
    Archive, ArchiveAdapter, ArchiveState, Capabilities, NodeKind, OpenOptions, decode_name,
};
use crate::encoding::{Codepage, FilenameEncoding, detect_codepage};
use crate::error::{Error, Result};
use crate::format::ArchiveFormat;
use crate::model::{EntryMeta, EntryPath};
use crate::source::{ReadSeek, Source};

/// Zip local-file-header magic (`PK\x03\x04`). Shared with the nested-archive
/// detection in `archive::mod`.
pub(crate) const ZIP_LOCAL_HEADER: &[u8] = b"PK\x03\x04";
/// Zip end-of-central-directory magic (`PK\x05\x06`, empty archives).
pub(crate) const ZIP_EMPTY_ARCHIVE: &[u8] = b"PK\x05\x06";

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
        Ok(Box::new(ArchiveAdapter::new(
            ZipArchiveInner::new(inner, opts)?,
            "zip",
        )))
    }
}

/// Shared state of an open zip archive.
struct ZipArchiveInner {
    inner: Mutex<zip::ZipArchive<Box<dyn ReadSeek + Send>>>,
    entries: Vec<EntryMeta>,
    index_by_path: HashMap<EntryPath, usize>,
    any_encrypted: bool,
}

impl ZipArchiveInner {
    /// Snapshot metadata and indexes from a parsed zip archive.
    fn new(archive: zip::ZipArchive<Box<dyn ReadSeek + Send>>, opts: &OpenOptions) -> Result<Self> {
        let mut archive = archive;
        let mut entries = Vec::with_capacity(archive.len());
        let mut index_by_path = HashMap::new();
        let mut any_encrypted = false;
        // Maps each central-directory index to its (filtered) entry index.
        let mut cd_to_entry: Vec<Option<usize>> = Vec::with_capacity(archive.len());

        // Pre-pass under the `Auto` strategy: detect the archive-wide legacy
        // codepage from the concatenation of all non-UTF-8 raw names. A single
        // short name is usually too little signal for reliable detection (e.g.
        // a 2-char Japanese name); aggregating every legacy name gives the
        // detector a proper corpus. Valid UTF-8 names are excluded — they
        // decode through the UTF-8 path and must not bias the guess.
        let detected_codepage: Option<Codepage> = if opts.encoding == FilenameEncoding::Auto {
            let mut corpus = Vec::new();
            for i in 0..archive.len() {
                let file = archive.by_index_raw(i).map_err(map_zip_err)?;
                let raw = file.name_raw();
                if !raw.is_ascii() && std::str::from_utf8(raw).is_err() {
                    corpus.extend_from_slice(raw);
                }
            }
            detect_codepage(&corpus)
        } else {
            None
        };

        for i in 0..archive.len() {
            // `by_index_raw` avoids the password check so encrypted archives
            // can still be listed (reads are rejected later, see `with_file`).
            let file = archive.by_index_raw(i).map_err(map_zip_err)?;
            let raw_name = file.name_raw().to_vec();
            let decoded = match detected_codepage {
                // A detected codepage applies only to legacy (non-UTF-8)
                // names; valid UTF-8 names still win in `Auto` mode, so
                // mixed archives (UTF-8 + legacy) keep both correct.
                Some(cp) => match std::str::from_utf8(&raw_name) {
                    Ok(s) => s.to_owned(),
                    Err(_) => decode_name(&raw_name, FilenameEncoding::Forced(cp)),
                },
                None => decode_name(&raw_name, opts.encoding),
            };
            // Entries whose paths fail validation (e.g. `../evil.txt`) cannot
            // be extracted safely; they are skipped from the listing (first
            // line of zip-slip defense, architecture.md §4.8).
            let Ok(path) = EntryPath::new(&decoded) else {
                cd_to_entry.push(None);
                continue;
            };
            let kind = if file.is_dir() {
                NodeKind::Dir
            } else if file.is_symlink() {
                NodeKind::Symlink
            } else {
                NodeKind::File
            };
            let mtime = file.last_modified().and_then(zip_datetime_to_system_time);
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
            cd_to_entry.push(Some(entries.len()));
            index_by_path.insert(path, entries.len());
            entries.push(meta);
        }

        // Second pass: mark entries that are themselves archives by sniffing
        // the first bytes of their decompressed content (walk/Navigator recurse
        // into `NodeKind::Archive` entries).
        for i in 0..archive.len() {
            let Some(Some(entry_idx)) = cd_to_entry.get(i) else {
                continue;
            };
            let entry_idx = *entry_idx;
            if entries[entry_idx].kind != NodeKind::File || entries[entry_idx].encrypted {
                continue;
            }
            let mut file = archive.by_index(i).map_err(map_zip_err)?;
            let mut head = [0u8; 512];
            let n = file.read(&mut head)?;
            if crate::archive::looks_like_nested_archive(&head[..n]) {
                entries[entry_idx].kind = NodeKind::Archive;
            }
        }

        Ok(Self {
            inner: Mutex::new(archive),
            entries,
            index_by_path,
            any_encrypted,
        })
    }
}

impl ZipArchiveInner {
    /// Lock the underlying zip reader, recovering from mutex poisoning (a
    /// panic inside the lock cannot corrupt the archive for later calls).
    fn lock(&self) -> MutexGuard<'_, zip::ZipArchive<Box<dyn ReadSeek + Send>>> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
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
        let idx = self
            .index_of(&meta.path)
            .ok_or_else(|| Error::CorruptArchive(format!("no such entry in zip: {}", meta.path)))?;
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
}

impl ArchiveState for ZipArchiveInner {
    fn entries(&self) -> &[EntryMeta] {
        &self.entries
    }

    fn index_of(&self, path: &EntryPath) -> Option<usize> {
        self.index_by_path.get(path).copied()
    }

    fn read_entry_bytes(&self, meta: &EntryMeta) -> Result<Vec<u8>> {
        self.read_to_vec(meta)
    }

    fn extract_to(&self, meta: &EntryMeta, sink: &mut dyn Write) -> Result<u64> {
        if meta.kind == NodeKind::Dir {
            return Ok(0);
        }
        self.with_file(meta, |file| Ok(std::io::copy(file, sink)?))
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            random_access: true,
            encrypted: self.any_encrypted,
            // M1: encrypted entries are not readable, so a password would not
            // help; `needs_password` stays false until M3 adds decryption.
            needs_password: false,
            can_write: false,
        }
    }
}

/// Convert a zip MS-DOS timestamp to a `SystemTime`.
///
/// The zip crate implements `TryFrom<zip::DateTime> for time::PrimitiveDateTime`
/// (behind its `time` feature); we reuse it instead of hand-rolling civil-date
/// math (see `local-doc/research-time-filetime.md`). Invalid (out-of-range)
/// dates fail the conversion and are filtered by the caller.
fn zip_datetime_to_system_time(dt: zip::DateTime) -> Option<SystemTime> {
    time::PrimitiveDateTime::try_from(dt)
        .ok()
        .map(|pdt| SystemTime::from(pdt.assume_utc()))
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

    /// Cross-check zip MS-DOS timestamps convert to the expected epoch values.
    #[test]
    fn zip_datetime_converts_to_system_time() {
        // 2024-02-29 12:34:56 (a valid MS-DOS date, 1980..2107); 2024-02-29 is
        // 19,782 days after the epoch (verified independently).
        let dt = zip::DateTime::from_date_and_time(2024, 2, 29, 12, 34, 56).expect("valid date");
        let st = zip_datetime_to_system_time(dt).expect("in range");
        let expected = SystemTime::UNIX_EPOCH
            + std::time::Duration::from_secs(19_782 * 86_400 + 12 * 3_600 + 34 * 60 + 56);
        assert_eq!(st, expected);
    }
}
