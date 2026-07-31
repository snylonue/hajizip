//! Abstraction over where archive bytes come from.

use std::fs::File;
use std::io::{Cursor, Read, Seek};
use std::path::PathBuf;

use crate::error::Result;

/// A readable + seekable source.
///
/// This combines `Read` and `Seek` into a single trait so it can be used as a
/// trait object (`dyn ReadSeek`); a trait object may only name one non-auto
/// trait directly.
pub trait ReadSeek: Read + Seek {}
impl<T: Read + Seek> ReadSeek for T {}

/// The origin of archive bytes.
#[derive(Debug, Clone)]
pub enum Source {
    /// A file on disk.
    Path(PathBuf),
    /// An in-memory buffer.
    Memory(Vec<u8>),
}

impl Source {
    /// Open the source as a seekable, thread-safe reader.
    ///
    /// Archive readers generally require `Seek` (e.g. to read a central
    /// directory). Non-seekable origins should be buffered into `Memory` or a
    /// temporary file before being wrapped here.
    pub fn open(&self) -> Result<Box<dyn ReadSeek + Send>> {
        match self {
            Source::Path(p) => Ok(Box::new(File::open(p)?)),
            Source::Memory(bytes) => Ok(Box::new(Cursor::new(bytes.clone()))),
        }
    }

    /// The lowercased file extension, if this is a path source that has one.
    ///
    /// Used as a fallback hint for format detection.
    pub fn extension(&self) -> Option<String> {
        match self {
            Source::Path(p) => p
                .extension()
                .and_then(|e| e.to_str())
                .map(|s| s.to_ascii_lowercase()),
            Source::Memory(_) => None,
        }
    }
}
