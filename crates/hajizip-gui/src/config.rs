//! Application configuration for the GUI.
//!
//! Configuration values that overlap with core concepts (overwrite policy,
//! safety limits, filename encoding) reuse the core types directly, so there is
//! a single source of truth (see `architecture.md` §5.1 and §5.6).
//!
//! Persistence uses a serde DTO ([`PersistedConfig`]) rather than deriving
//! serde on `AppConfig`: the core types inside `AppConfig` do not implement
//! `Serialize` (core stays a zero-serde library; see
//! `local-doc/research-config-persistence.md`). The DTO is a pure serialization
//! carrier — semantics stay with the core types.

use std::path::{Path, PathBuf};

use hajizip_core::{Codepage, FilenameEncoding, OverwritePolicy, SafetyLimits};
use serde::{Deserialize, Serialize};

/// Maximum number of recently opened archives remembered.
pub const MAX_RECENT_FILES: usize = 10;

/// User-facing application settings.
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
    /// Recently opened archive paths, most recent first.
    pub recent_files: Vec<PathBuf>,
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
            recent_files: Vec::new(),
        }
    }
}

impl AppConfig {
    /// Record a successfully opened archive in the recent-files list.
    ///
    /// The path is moved to the front (most recent first), duplicates are
    /// removed, and the list is capped at [`MAX_RECENT_FILES`].
    pub fn record_recent(&mut self, path: PathBuf) {
        self.recent_files.retain(|p| p != &path);
        self.recent_files.insert(0, path);
        self.recent_files.truncate(MAX_RECENT_FILES);
    }
}

impl AppConfig {
    /// Load the config from the platform config directory (defaults on error).
    pub fn load() -> Self {
        let Some(path) = config_path() else {
            return Self::default();
        };
        Self::load_from(&path)
    }

    /// Save the config to the platform config directory.
    pub fn save(&self) -> anyhow::Result<()> {
        let path = config_path().ok_or_else(|| anyhow::anyhow!("no config directory found"))?;
        self.save_to(&path)
    }

    /// Load from an explicit path (missing or malformed file → defaults).
    fn load_from(path: &Path) -> Self {
        let Ok(text) = std::fs::read_to_string(path) else {
            return Self::default();
        };
        let Ok(persisted) = toml::from_str::<PersistedConfig>(&text) else {
            return Self::default();
        };
        persisted.into()
    }

    /// Save to an explicit path, creating parent directories.
    fn save_to(&self, path: &Path) -> anyhow::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = toml::to_string_pretty(&PersistedConfig::from(self))?;
        std::fs::write(path, text)?;
        Ok(())
    }
}

/// Location of the user config file: `<config_dir>/hajizip/config.toml`.
fn config_path() -> Option<PathBuf> {
    dirs::config_dir().map(|dir| dir.join("hajizip").join("config.toml"))
}

/// Serializable mirror of [`AppConfig`] (see module docs).
#[derive(Debug, Clone, Serialize, Deserialize)]
struct PersistedConfig {
    default_extract_dir: Option<String>,
    overwrite_policy: String,
    preserve_mtime: bool,
    max_total_bytes: u64,
    max_entries: u64,
    max_depth: usize,
    nested_buffer_threshold: u64,
    filename_encoding: String,
    recent_files: Vec<String>,
}

impl From<&AppConfig> for PersistedConfig {
    fn from(cfg: &AppConfig) -> Self {
        Self {
            default_extract_dir: cfg
                .default_extract_dir
                .as_ref()
                .map(|p| p.display().to_string()),
            overwrite_policy: overwrite_to_str(cfg.overwrite_policy).to_string(),
            preserve_mtime: cfg.preserve_mtime,
            max_total_bytes: cfg.safety_limits.max_total_bytes,
            max_entries: cfg.safety_limits.max_entries,
            max_depth: cfg.safety_limits.max_depth,
            nested_buffer_threshold: cfg.nested_buffer_threshold,
            filename_encoding: encoding_to_str(cfg.filename_encoding).to_string(),
            recent_files: cfg
                .recent_files
                .iter()
                .map(|p| p.display().to_string())
                .collect(),
        }
    }
}

impl From<PersistedConfig> for AppConfig {
    fn from(p: PersistedConfig) -> Self {
        let defaults = AppConfig::default();
        Self {
            default_extract_dir: p
                .default_extract_dir
                .filter(|s| !s.is_empty())
                .map(PathBuf::from),
            overwrite_policy: str_to_overwrite(&p.overwrite_policy)
                .unwrap_or(defaults.overwrite_policy),
            preserve_mtime: p.preserve_mtime,
            safety_limits: SafetyLimits {
                max_total_bytes: p.max_total_bytes,
                max_entries: p.max_entries,
                max_depth: p.max_depth,
            },
            nested_buffer_threshold: p.nested_buffer_threshold,
            filename_encoding: str_to_encoding(&p.filename_encoding)
                .unwrap_or(defaults.filename_encoding),
            recent_files: p
                .recent_files
                .into_iter()
                .filter(|s| !s.is_empty())
                .map(PathBuf::from)
                .collect(),
        }
    }
}

/// Stable string tags for overwrite policies (TOML is human-readable).
///
/// Shared with the UI's `<option>` values so persistence and the settings
/// panel can never drift apart (see `local-doc/review-current-2026-08-05.md`
/// §2).
pub fn overwrite_to_str(p: OverwritePolicy) -> &'static str {
    match p {
        OverwritePolicy::Ask => "ask",
        OverwritePolicy::Always => "always",
        OverwritePolicy::Never => "never",
        OverwritePolicy::Newer => "newer",
    }
}

pub fn str_to_overwrite(s: &str) -> Option<OverwritePolicy> {
    match s {
        "ask" => Some(OverwritePolicy::Ask),
        "always" => Some(OverwritePolicy::Always),
        "never" => Some(OverwritePolicy::Never),
        "newer" => Some(OverwritePolicy::Newer),
        _ => None,
    }
}

/// Stable string tags for filename encodings.
pub fn encoding_to_str(e: FilenameEncoding) -> &'static str {
    match e {
        FilenameEncoding::Auto => "auto",
        FilenameEncoding::Forced(Codepage::Utf8) => "utf8",
        FilenameEncoding::Forced(Codepage::Gbk) => "gbk",
        FilenameEncoding::Forced(Codepage::ShiftJis) => "shift-jis",
        FilenameEncoding::Forced(Codepage::Big5) => "big5",
        FilenameEncoding::Forced(Codepage::Cp437) => "cp437",
    }
}

pub fn str_to_encoding(s: &str) -> Option<FilenameEncoding> {
    match s {
        "auto" => Some(FilenameEncoding::Auto),
        "utf8" => Some(FilenameEncoding::Forced(Codepage::Utf8)),
        "gbk" => Some(FilenameEncoding::Forced(Codepage::Gbk)),
        "shift-jis" => Some(FilenameEncoding::Forced(Codepage::ShiftJis)),
        "big5" => Some(FilenameEncoding::Forced(Codepage::Big5)),
        "cp437" => Some(FilenameEncoding::Forced(Codepage::Cp437)),
        _ => None,
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
        assert!(config.recent_files.is_empty());
    }

    #[test]
    fn record_recent_moves_to_front_dedupes_and_caps() {
        let mut config = AppConfig::default();
        for i in 0..MAX_RECENT_FILES + 3 {
            config.record_recent(PathBuf::from(format!("/tmp/a{i}.zip")));
        }
        // Capped at MAX_RECENT_FILES, most recent first.
        assert_eq!(config.recent_files.len(), MAX_RECENT_FILES);
        assert_eq!(
            config.recent_files[0],
            PathBuf::from(format!("/tmp/a{}.zip", MAX_RECENT_FILES + 2))
        );

        // Re-recording an existing path moves it to the front, no duplicate.
        config.record_recent(PathBuf::from("/tmp/a0.zip"));
        assert_eq!(config.recent_files.len(), MAX_RECENT_FILES);
        assert_eq!(config.recent_files[0], PathBuf::from("/tmp/a0.zip"));
        assert_eq!(
            config
                .recent_files
                .iter()
                .filter(|p| p.as_path() == Path::new("/tmp/a0.zip"))
                .count(),
            1
        );
    }

    #[test]
    fn roundtrip_preserves_all_fields() {
        let config = AppConfig {
            default_extract_dir: Some(PathBuf::from("/tmp/extract")),
            overwrite_policy: OverwritePolicy::Always,
            preserve_mtime: false,
            safety_limits: SafetyLimits {
                max_total_bytes: 1234,
                max_entries: 56,
                max_depth: 3,
            },
            nested_buffer_threshold: 999,
            filename_encoding: FilenameEncoding::Forced(Codepage::Gbk),
            recent_files: vec![
                PathBuf::from("/tmp/one.zip"),
                PathBuf::from("/tmp/two.tar.gz"),
            ],
        };

        let persisted = PersistedConfig::from(&config);
        let restored = AppConfig::from(persisted);

        assert_eq!(restored.default_extract_dir, config.default_extract_dir);
        assert_eq!(restored.overwrite_policy, config.overwrite_policy);
        assert_eq!(restored.preserve_mtime, config.preserve_mtime);
        assert_eq!(
            restored.safety_limits.max_total_bytes,
            config.safety_limits.max_total_bytes
        );
        assert_eq!(
            restored.safety_limits.max_entries,
            config.safety_limits.max_entries
        );
        assert_eq!(
            restored.safety_limits.max_depth,
            config.safety_limits.max_depth
        );
        assert_eq!(
            restored.nested_buffer_threshold,
            config.nested_buffer_threshold
        );
        assert_eq!(restored.filename_encoding, config.filename_encoding);
        assert_eq!(restored.recent_files, config.recent_files);
    }

    #[test]
    fn unknown_strings_fall_back_to_defaults() {
        let persisted = PersistedConfig {
            default_extract_dir: None,
            overwrite_policy: "bogus".into(),
            preserve_mtime: false,
            max_total_bytes: 1,
            max_entries: 1,
            max_depth: 1,
            nested_buffer_threshold: 1,
            filename_encoding: "bogus".into(),
            recent_files: vec!["/tmp/x.zip".into(), "".into()],
        };
        let restored = AppConfig::from(persisted);
        assert_eq!(restored.overwrite_policy, OverwritePolicy::Ask);
        assert_eq!(restored.filename_encoding, FilenameEncoding::Auto);
        // Empty strings are dropped; valid paths survive.
        assert_eq!(restored.recent_files, vec![PathBuf::from("/tmp/x.zip")]);
    }

    #[test]
    fn save_then_load_roundtrips() {
        let config = AppConfig {
            default_extract_dir: None,
            overwrite_policy: OverwritePolicy::Newer,
            preserve_mtime: true,
            safety_limits: SafetyLimits::default(),
            nested_buffer_threshold: 64 * 1024 * 1024,
            filename_encoding: FilenameEncoding::Forced(Codepage::ShiftJis),
            recent_files: vec![PathBuf::from("/tmp/recent.7z")],
        };

        let dir = std::env::temp_dir().join(format!("hajizip-gui-config-{}", std::process::id()));
        let path = dir.join("config.toml");
        config.save_to(&path).unwrap();

        let loaded = AppConfig::load_from(&path);
        assert_eq!(loaded.overwrite_policy, OverwritePolicy::Newer);
        assert_eq!(
            loaded.filename_encoding,
            FilenameEncoding::Forced(Codepage::ShiftJis)
        );
        assert_eq!(loaded.recent_files, config.recent_files);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_missing_file_returns_defaults() {
        let missing = PathBuf::from("/nonexistent/hajizip/config.toml");
        let loaded = AppConfig::load_from(&missing);
        let defaults = AppConfig::default();
        assert_eq!(loaded.overwrite_policy, defaults.overwrite_policy);
        assert_eq!(loaded.preserve_mtime, defaults.preserve_mtime);
        assert_eq!(loaded.filename_encoding, defaults.filename_encoding);
        assert_eq!(loaded.default_extract_dir, defaults.default_extract_dir);
        assert_eq!(
            loaded.nested_buffer_threshold,
            defaults.nested_buffer_threshold
        );
    }
}
