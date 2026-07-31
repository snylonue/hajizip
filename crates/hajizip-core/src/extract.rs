//! Extraction engine shared by the GUI and any future CLI.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::archive::Archive;
use crate::error::{Error, Result};
use crate::model::EntryPath;

/// How to handle existing files at the destination.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OverwritePolicy {
    /// Ask the user per conflict.
    #[default]
    Ask,
    /// Always overwrite.
    Always,
    /// Never overwrite.
    Never,
    /// Overwrite only if the source is newer.
    Newer,
}

/// Safety limits guarding against zip-bombs and runaway recursion.
#[derive(Debug, Clone, Copy)]
pub struct SafetyLimits {
    /// Maximum total uncompressed bytes across the whole run.
    pub max_total_bytes: u64,
    /// Maximum number of entries processed.
    pub max_entries: u64,
    /// Maximum nested-archive depth.
    pub max_depth: usize,
}

impl Default for SafetyLimits {
    fn default() -> Self {
        Self {
            max_total_bytes: 16 * 1024 * 1024 * 1024,
            max_entries: 1_000_000,
            max_depth: 8,
        }
    }
}

/// Options controlling an extraction run.
#[derive(Debug, Clone, Default)]
pub struct ExtractOptions {
    /// Destination directory.
    pub dest_dir: PathBuf,
    /// Conflict resolution policy.
    pub overwrite: OverwritePolicy,
    /// Whether to preserve modification times.
    pub preserve_mtime: bool,
    /// Whether to create a top-level folder named after the archive.
    pub create_top_folder: bool,
    /// Safety limits.
    pub limits: SafetyLimits,
}

/// A summary of a completed extraction run.
#[derive(Debug, Default)]
pub struct ExtractReport {
    /// Number of entries successfully extracted.
    pub extracted: u64,
    /// Number of entries skipped (e.g. due to policy).
    pub skipped: u64,
    /// Entries that failed, with their errors.
    pub failed: Vec<(EntryPath, Error)>,
    /// Total bytes written.
    pub total_bytes: u64,
}

/// Receives progress callbacks during extraction.
pub trait ProgressSink: Send {
    /// Called when an entry begins.
    fn on_entry_start(&mut self, path: &EntryPath, size: Option<u64>);
    /// Called as bytes are written, with the incremental byte count.
    fn on_bytes(&mut self, delta: u64);
    /// Called when an entry finishes.
    fn on_entry_done(&mut self, path: &EntryPath);
}

/// A thread-safe cancellation token.
#[derive(Clone, Default)]
pub struct CancellationToken {
    flag: Arc<AtomicBool>,
}

impl CancellationToken {
    /// Create a new, uncancelled token.
    pub fn new() -> Self {
        Self::default()
    }

    /// Request cancellation.
    pub fn cancel(&self) {
        self.flag.store(true, Ordering::Relaxed);
    }

    /// Whether cancellation has been requested.
    pub fn is_cancelled(&self) -> bool {
        self.flag.load(Ordering::Relaxed)
    }
}

/// The extraction engine.
pub struct ExtractEngine;

impl ExtractEngine {
    /// Extract `selection` (empty means all entries) from `archive`.
    pub fn run(
        _archive: &dyn Archive,
        _selection: &[EntryPath],
        _opts: &ExtractOptions,
        _progress: &mut dyn ProgressSink,
        _cancel: &CancellationToken,
    ) -> Result<ExtractReport> {
        Err(Error::UnsupportedFeature("ExtractEngine::run".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancellation_token_roundtrip() {
        let token = CancellationToken::new();
        assert!(!token.is_cancelled());
        let clone = token.clone();
        clone.cancel();
        assert!(token.is_cancelled());
    }

    #[test]
    fn default_limits_are_sane() {
        let limits = SafetyLimits::default();
        assert!(limits.max_total_bytes > 0);
        assert!(limits.max_entries > 0);
        assert!(limits.max_depth > 0);
    }
}
