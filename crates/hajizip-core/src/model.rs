//! Core data model shared across the library.

use std::time::SystemTime;

use crate::error::{Error, Result};

/// A timestamp associated with an entry.
pub type Timestamp = SystemTime;

/// The kind of a node within an archive tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeKind {
    /// A regular file with content.
    File,
    /// A directory containing other nodes.
    Dir,
    /// An entry that is itself an openable archive.
    Archive,
    /// A symbolic link.
    Symlink,
}

/// Identifier for a supported single-stream compression codec.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodecId {
    /// Raw DEFLATE.
    Deflate,
    /// gzip (DEFLATE with gzip framing).
    Gzip,
    /// bzip2.
    Bzip2,
    /// xz / LZMA2.
    Xz,
    /// Zstandard.
    Zstd,
    /// LZ4.
    Lz4,
    /// Brotli.
    Brotli,
}

/// Identifier for a supported archive / container format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormatKind {
    /// ZIP archive.
    Zip,
    /// 7-Zip archive.
    SevenZ,
    /// tar archive (uncompressed).
    Tar,
    /// gzip single-stream.
    Gzip,
    /// bzip2 single-stream.
    Bzip2,
    /// xz single-stream.
    Xz,
    /// Zstandard single-stream.
    Zstd,
}

impl FormatKind {
    /// All archive/container formats known to the registry.
    pub fn all() -> &'static [FormatKind] {
        &[
            FormatKind::Zip,
            FormatKind::SevenZ,
            FormatKind::Tar,
            FormatKind::Gzip,
            FormatKind::Bzip2,
            FormatKind::Xz,
            FormatKind::Zstd,
        ]
    }
}

/// A compression level. Interpretation is codec-specific.
#[derive(Debug, Clone, Copy)]
pub struct Level(pub u32);

impl Default for Level {
    fn default() -> Self {
        Level(6)
    }
}

/// A secret value such as a password.
///
/// The value is redacted in `Debug` output so it does not leak into logs.
// TODO: integrate a zeroizing wrapper so the bytes are cleared on drop.
#[derive(Clone)]
pub struct Secret(Vec<u8>);

impl Secret {
    /// Create a secret from a UTF-8 string.
    pub fn new(s: &str) -> Self {
        Self(s.as_bytes().to_vec())
    }

    /// The raw secret bytes.
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}

/// A normalized, validated path of an entry inside an archive.
///
/// Construction rejects parent-directory traversal (`..`) and strips any
/// leading separators so the path is always relative. This is the first line
/// of defense against zip-slip when extracting.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct EntryPath(String);

impl EntryPath {
    /// Validate and normalize a raw entry path.
    pub fn new(raw: &str) -> Result<Self> {
        Ok(Self(normalize(raw)?))
    }

    /// The normalized path as a string slice (using `/` separators).
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for EntryPath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Normalize a raw path: split on `/` or `\`, drop empty/`.` components,
/// reject `..`, and rejoin with `/`.
fn normalize(raw: &str) -> Result<String> {
    let mut out: Vec<&str> = Vec::new();
    for part in raw.split(['/', '\\']) {
        match part {
            "" | "." => continue,
            ".." => {
                return Err(Error::InvalidPath(format!(
                    "parent-directory traversal in {raw:?}"
                )));
            }
            other => out.push(other),
        }
    }
    if out.is_empty() {
        return Err(Error::InvalidPath(format!("no valid component in {raw:?}")));
    }
    Ok(out.join("/"))
}

/// Metadata describing a single archive entry.
#[derive(Debug, Clone)]
pub struct EntryMeta {
    /// Normalized, validated path inside the archive.
    pub path: EntryPath,
    /// The original raw name bytes (for round-tripping and debugging).
    pub raw_name: Vec<u8>,
    /// The node kind.
    pub kind: NodeKind,
    /// Uncompressed size in bytes, if known.
    pub uncompressed_size: Option<u64>,
    /// Compressed size in bytes, if known.
    pub compressed_size: Option<u64>,
    /// Last modification time, if known.
    pub mtime: Option<Timestamp>,
    /// Unix permission bits, if present.
    pub mode: Option<u32>,
    /// CRC-32 checksum, if present.
    pub crc: Option<u32>,
    /// Whether the entry is encrypted.
    pub encrypted: bool,
    /// Optional comment.
    pub comment: Option<String>,
}

/// A fully-qualified location across nested archives.
///
/// Example: `"outer.zip/inner.7z/docs/readme.txt"`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Location(pub String);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_separators_and_dots() {
        assert_eq!(EntryPath::new("a/./b//c").unwrap().as_str(), "a/b/c");
        assert_eq!(EntryPath::new("\\a\\b").unwrap().as_str(), "a/b");
        // Leading separators are stripped, making the path relative.
        assert_eq!(
            EntryPath::new("/etc/passwd").unwrap().as_str(),
            "etc/passwd"
        );
    }

    #[test]
    fn rejects_parent_traversal() {
        assert!(EntryPath::new("a/../b").is_err());
        assert!(EntryPath::new("..").is_err());
        assert!(EntryPath::new("../secret").is_err());
    }

    #[test]
    fn rejects_empty_paths() {
        assert!(EntryPath::new("").is_err());
        assert!(EntryPath::new("///").is_err());
        assert!(EntryPath::new(".").is_err());
    }

    #[test]
    fn secret_is_redacted_in_debug() {
        let s = Secret::new("hunter2");
        assert_eq!(format!("{s:?}"), "Secret(<redacted>)");
    }
}
