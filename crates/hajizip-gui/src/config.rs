//! Application configuration for the GUI.
//!
//! Configuration values that overlap with core concepts (overwrite policy,
//! safety limits, filename encoding) reuse the core types directly, so there is
//! a single source of truth (see `architecture.md` §5.1 and §5.6).
//!
//! Persistence (saving to / loading from the user config directory) requires a
//! serialization + directory crate that has not been researched yet, so for now
//! the config lives in memory only. See `local-doc/` for the pending research.

use std::path::PathBuf;

use hajizip_core::{FilenameEncoding, OverwritePolicy, SafetyLimits};

/// User-facing application settings.
//
// Only `filename_encoding` is consumed in M0; the remaining fields drive the
// extraction and settings features from M1 onwards (architecture.md §5.6).
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct AppConfig {
    /// Default directory offered for extraction, if any.
    pub default_extract_dir: Option<PathBuf>,
    /// How to handle existing files at the destination.
    pub overwrite_policy: OverwritePolicy,
    /// Whether to preserve modification times when extracting.
    pub preserve_mtime: bool,
    /// Safety limits guarding against zip-bombs and runaway recursion.
    pub safety_limits: SafetyLimits,
    /// Nested archives whose uncompressed size is at most this many bytes are
    /// buffered in memory; larger ones go through a temporary file.
    pub nested_buffer_threshold: u64,
    /// Filename decoding strategy for non-UTF-8 entry names.
    pub filename_encoding: FilenameEncoding,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            default_extract_dir: None,
            overwrite_policy: OverwritePolicy::default(),
            preserve_mtime: true,
            safety_limits: SafetyLimits::default(),
            // 64 MiB is a reasonable default split between memory and disk.
            nested_buffer_threshold: 64 * 1024 * 1024,
            filename_encoding: FilenameEncoding::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_sane() {
        let config = AppConfig::default();
        assert_eq!(config.overwrite_policy, OverwritePolicy::Ask);
        assert!(config.preserve_mtime);
        assert_eq!(config.filename_encoding, FilenameEncoding::Auto);
        assert!(config.nested_buffer_threshold > 0);
        assert!(config.default_extract_dir.is_none());
    }
}
