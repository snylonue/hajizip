//! 7z archive implementation backed by `sevenz-rust2`.
//!
//! Selection rationale and the ZSTD-method decision (official crate, C-FFI
//! `zstd` feature approved as an exception) are documented in
//! `local-doc/research-7z.md`.
//!
//! 7z stores files in *blocks* (folders). Non-solid archives map each file to
//! its own block, so reading an entry seeks to that block's pack stream and
//! decodes only it. Solid archives share one block across many files, so
//! reading a single entry decodes its whole containing block (the same
//! behavior as 7-Zip itself; bounded by the declared entry size).
//!
//! Entry names in 7z are UTF-16 by spec, so no legacy-codepage decoding is
//! needed here (unlike zip/tar).

use std::collections::HashMap;
use std::io::Write;
use std::sync::{Mutex, MutexGuard};
use std::time::SystemTime;

use crate::archive::{
    Archive, ArchiveAdapter, ArchiveState, Capabilities, NodeKind, OpenOptions, SNIFF_ENTRY_CAP,
    SOLID_SNIFF_ARCHIVE_CAP,
};
use crate::error::{Error, Result};
use crate::format::ArchiveFormat;
use crate::model::{EntryMeta, EntryPath};
use crate::source::{ReadSeek, Source};

/// 7z magic bytes: `7z` 0xBC 0xAF 0x27 0x1C. Shared with the nested-archive
/// detection in `archive::mod`.
pub(crate) const SEVEN_Z_MAGIC: &[u8] = b"7z\xbc\xaf\x27\x1c";

/// 7z AES-256-SHA256 coder method id (`EncoderMethod::ID_AES256_SHA256`).
const AES256_SHA256_METHOD: &[u8] = &[0x06, 0xF1, 0x07, 0x01];

/// Format identity and detection for 7z archives.
pub struct SevenZipFormat;

impl ArchiveFormat for SevenZipFormat {
    fn id(&self) -> &str {
        "7z"
    }

    fn display_name(&self) -> &str {
        "7-Zip"
    }

    fn extensions(&self) -> &[&str] {
        &["7z"]
    }

    fn matches(&self, head: &[u8], ext: Option<&str>) -> bool {
        head.starts_with(SEVEN_Z_MAGIC) || matches!(ext, Some("7z"))
    }

    fn open(&self, src: Source, opts: &OpenOptions) -> Result<Box<dyn Archive>> {
        let reader = src.open()?;
        Ok(Box::new(ArchiveAdapter::new(
            SevenZipInner::open(reader, opts)?,
            "7z",
        )))
    }
}

/// Shared state of an open 7z archive.
///
/// The underlying `ArchiveReader` stays behind a `Mutex` (its read API is
/// `&mut self`); entry metadata is snapshotted at open time so listing never
/// touches the lock.
struct SevenZipInner {
    src: Mutex<sevenz_rust2::ArchiveReader<Box<dyn ReadSeek + Send>>>,
    /// Entry metadata, in archive order (parallel to `names`).
    entries: Vec<EntryMeta>,
    /// Original (pre-normalization) entry names, as stored in the 7z header.
    names: Vec<String>,
    by_path: HashMap<EntryPath, usize>,
    is_solid: bool,
    encrypted: bool,
    needs_password: bool,
}

impl SevenZipInner {
    /// Parse the header, snapshot metadata and build the reader.
    fn open(mut src: Box<dyn ReadSeek + Send>, opts: &OpenOptions) -> Result<Self> {
        let password = password_from_opts(opts);
        let archive = sevenz_rust2::Archive::read(&mut src, &password).map_err(map_error)?;

        let mut entries = Vec::new();
        let mut names = Vec::new();
        let mut by_path = HashMap::new();
        for (i, f) in archive.files.iter().enumerate() {
            // 7z names are UTF-16 by spec, already decoded to UTF-8 by
            // sevenz-rust2; entries with invalid paths are skipped.
            let Ok(path) = EntryPath::new(&f.name) else {
                continue;
            };
            let kind = if f.is_directory {
                NodeKind::Dir
            } else {
                NodeKind::File
            };
            // An entry is encrypted when its block's coder chain contains the
            // 7z AES method; files without a stream (dirs / empty files) are
            // never encrypted.
            let encrypted = archive
                .stream_map
                .file_block_index
                .get(i)
                .and_then(|b| *b)
                .map(|bi| {
                    archive.blocks[bi]
                        .coders
                        .iter()
                        .any(|c| c.encoder_method_id() == AES256_SHA256_METHOD)
                })
                .unwrap_or(false);
            let meta = EntryMeta {
                path: path.clone(),
                raw_name: f.name.as_bytes().to_vec(),
                kind,
                uncompressed_size: (!f.is_directory).then_some(f.size),
                compressed_size: (!f.is_directory).then_some(f.compressed_size),
                mtime: f
                    .has_last_modified_date
                    .then_some(SystemTime::from(f.last_modified_date)),
                mode: None,
                crc: f.has_crc.then_some(f.crc as u32),
                encrypted,
                comment: None,
            };
            by_path.insert(path, entries.len());
            entries.push(meta);
            names.push(f.name.clone());
        }

        let is_solid = archive.is_solid;
        let encrypted = archive.blocks.iter().any(|b| {
            b.coders
                .iter()
                .any(|c| c.encoder_method_id() == AES256_SHA256_METHOD)
        });
        let needs_password = encrypted && opts.password.is_none();

        // Bounded nested-archive marking: sniff the leading bytes of each
        // eligible file entry and promote it to `NodeKind::Archive` (so
        // walk/Navigator can recurse). Decoding one entry in a solid archive
        // decodes its whole block, so the budget is capped: only non-solid
        // archives or small solid archives (≤ 8 MiB total) are sniffed, and
        // only entries ≤ 1 MiB. Larger entries stay `File`; `open_nested`
        // still works on them programmatically.
        let total_size = src.seek(std::io::SeekFrom::End(0))?;
        let sniff_ok = !is_solid || total_size <= SOLID_SNIFF_ARCHIVE_CAP;

        let mut reader = sevenz_rust2::ArchiveReader::from_archive(archive, src, password);
        if sniff_ok {
            for (meta, name) in entries.iter_mut().zip(&names) {
                if meta.kind != NodeKind::File
                    || meta.encrypted
                    || meta.uncompressed_size.unwrap_or(0) > SNIFF_ENTRY_CAP
                {
                    continue;
                }
                let Ok(bytes) = reader.read_file(name) else {
                    continue;
                };
                let head = &bytes[..bytes.len().min(512)];
                if crate::archive::looks_like_nested_archive(head) {
                    meta.kind = NodeKind::Archive;
                }
            }
        }

        Ok(Self {
            src: Mutex::new(reader),
            entries,
            names,
            by_path,
            is_solid,
            encrypted,
            needs_password,
        })
    }
}

impl SevenZipInner {
    /// Lock the reader, recovering from mutex poisoning.
    fn lock(&self) -> MutexGuard<'_, sevenz_rust2::ArchiveReader<Box<dyn ReadSeek + Send>>> {
        self.src.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn locate(&self, meta: &EntryMeta) -> Result<usize> {
        self.by_path
            .get(&meta.path)
            .copied()
            .ok_or_else(|| Error::CorruptArchive(format!("no such entry in 7z: {}", meta.path)))
    }

    fn read_entry_bytes(&self, meta: &EntryMeta) -> Result<Vec<u8>> {
        if meta.kind == NodeKind::Dir {
            return Ok(Vec::new());
        }
        let idx = self.locate(meta)?;
        let mut guard = self.lock();
        guard.read_file(&self.names[idx]).map_err(map_error)
    }
}

impl ArchiveState for SevenZipInner {
    fn entries(&self) -> &[EntryMeta] {
        &self.entries
    }

    fn index_of(&self, path: &EntryPath) -> Option<usize> {
        self.by_path.get(path).copied()
    }

    fn read_entry_bytes(&self, meta: &EntryMeta) -> Result<Vec<u8>> {
        SevenZipInner::read_entry_bytes(self, meta)
    }

    fn extract_to(&self, meta: &EntryMeta, sink: &mut dyn Write) -> Result<u64> {
        let bytes = self.read_entry_bytes(meta)?;
        sink.write_all(&bytes)?;
        Ok(bytes.len() as u64)
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            // Non-solid archives seek directly to an entry's block; solid
            // archives must decode the whole containing block.
            random_access: !self.is_solid,
            encrypted: self.encrypted,
            needs_password: self.needs_password,
            can_write: false,
        }
    }
}

/// Build the sevenz password from our open options.
///
/// Our `Secret` stores UTF-8 bytes; sevenz passwords are UTF-16 (LE), which
/// `Password::new` encodes for us.
fn password_from_opts(opts: &OpenOptions) -> sevenz_rust2::Password {
    match &opts.password {
        Some(secret) => {
            let s = String::from_utf8_lossy(secret.as_bytes());
            sevenz_rust2::Password::new(&s)
        }
        None => sevenz_rust2::Password::empty(),
    }
}

/// Map sevenz errors onto the core error type.
fn map_error(e: sevenz_rust2::Error) -> Error {
    use sevenz_rust2::Error as S7;
    match e {
        S7::BadSignature(_) => Error::UnsupportedFormat("bad 7z signature".into()),
        S7::UnsupportedVersion { .. } => {
            Error::UnsupportedFeature("unsupported 7z format version".into())
        }
        S7::ChecksumVerificationFailed
        | S7::NextHeaderCrcMismatch
        | S7::BadTerminatedStreamsInfo(_)
        | S7::BadTerminatedUnpackInfo
        | S7::BadTerminatedPackInfo(_)
        | S7::BadTerminatedSubStreamsInfo
        | S7::BadTerminatedHeader(_)
        | S7::FileNotFound => Error::CorruptArchive(format!("7z: {e}")),
        S7::Io(e, _) | S7::FileOpen(e, _) => Error::Io(e),
        S7::Other(msg) => Error::CorruptArchive(msg.into_owned()),
        S7::ExternalUnsupported => Error::UnsupportedFeature("external 7z coder".into()),
        S7::UnsupportedCompressionMethod(m) => {
            Error::UnsupportedFeature(format!("7z compression method: {m}"))
        }
        S7::MaxMemLimited { .. } => Error::UnsupportedFeature("7z memory limit".into()),
        S7::PasswordRequired => Error::PasswordRequired,
        S7::MaybeBadPassword(_) => Error::WrongPassword,
        S7::Unsupported(msg) => Error::UnsupportedFeature(msg.into_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_by_magic_and_extension() {
        let fmt = SevenZipFormat;
        assert_eq!(fmt.id(), "7z");
        assert_eq!(fmt.display_name(), "7-Zip");
        assert_eq!(fmt.extensions(), &["7z"]);
        assert!(fmt.matches(b"7z\xbc\xaf\x27\x1c", None));
        assert!(fmt.matches(b"junk", Some("7z")));
        assert!(!fmt.matches(b"junk", None));
        assert!(!fmt.matches(b"PK\x03\x04", None));
        assert!(!fmt.matches(&[0x1f, 0x8b], None));
        assert!(!fmt.matches(&[0xfd, b'7', b'z', b'X', b'Z', 0x00], None));
    }
}
