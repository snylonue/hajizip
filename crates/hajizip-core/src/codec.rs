//! Codec abstraction for single-stream compression formats.

pub mod gzip;
pub mod xz;

use std::io::{Read, Write};

use crate::error::{Error, Result};
use crate::model::Level;

/// A compression codec operating on a single byte stream (e.g. gzip, xz).
///
/// Codecs are streaming: they wrap a reader/writer rather than buffering the
/// whole payload in memory. A codec carries no format identity of its own;
/// identity and detection live with the concrete implementation (see
/// [`crate::format::CodecFormat`]).
pub trait Codec: Send + Sync {
    /// Wrap `input` in a reader that decompresses on the fly.
    fn decompress<'r>(&self, input: Box<dyn Read + Send + 'r>)
    -> Result<Box<dyn Read + Send + 'r>>;

    /// Wrap `output` in a writer that compresses on the fly.
    ///
    /// Reserved for future write support; defaults to unsupported.
    fn compress<'w>(
        &self,
        _output: Box<dyn Write + Send + 'w>,
        _level: Level,
    ) -> Result<Box<dyn Write + Send + 'w>> {
        Err(Error::UnsupportedFeature("compression".into()))
    }
}
