//! Extraction engine shared by the GUI and any future CLI.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::SystemTime;

use crate::archive::Archive;
use crate::error::{Error, Result};
use crate::model::{EntryMeta, EntryPath, NodeKind};

/// Chunk size for progress-reporting copies.
const COPY_CHUNK: usize = 64 * 1024;

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

/// The user's decision for an existing destination file when the overwrite
/// policy is [`OverwritePolicy::Ask`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverwriteDecision {
    /// Replace the existing file with the archive entry.
    Overwrite,
    /// Leave the existing file untouched (counted as skipped).
    Skip,
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
    /// Called when `opts.overwrite == OverwritePolicy::Ask` and the
    /// destination file already exists. The sink decides whether to replace
    /// it. The default is [`OverwriteDecision::Skip`] (safe default); an
    /// interactive frontend overrides this to ask the user, blocking until
    /// the answer arrives.
    fn on_ask_overwrite(&mut self, _path: &EntryPath, _dest: &Path) -> OverwriteDecision {
        OverwriteDecision::Skip
    }
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
    ///
    /// Safety properties (architecture.md §4.8):
    /// - `EntryPath` normalization guarantees every destination stays inside
    ///   `opts.dest_dir` (first line of zip-slip defense);
    /// - symlink entries are **never materialized** (counted as skipped) so
    ///   links cannot escape `dest_dir` (M1 safety default);
    /// - `SafetyLimits` bounds total bytes and entry count; exceeding them
    ///   aborts the run with [`Error::LimitExceeded`];
    /// - cancellation is checked per entry and per chunk.
    ///
    /// Per-entry failures are collected in the report's `failed` list and do
    /// not abort the run, except for [`Error::Cancelled`] and
    /// [`Error::LimitExceeded`] which always abort.
    pub fn run(
        archive: &dyn Archive,
        selection: &[EntryPath],
        opts: &ExtractOptions,
        progress: &mut dyn ProgressSink,
        cancel: &CancellationToken,
    ) -> Result<ExtractReport> {
        // The frozen API does not carry the archive's name, so the top-folder
        // feature cannot be implemented here (documented contract gap; the
        // GUI does not use it either).
        if opts.create_top_folder {
            return Err(Error::UnsupportedFeature(
                "create_top_folder: the archive name is not available in the \
                 frozen ExtractEngine::run API"
                    .into(),
            ));
        }

        let mut report = ExtractReport::default();
        let entries = resolve_selection(archive, selection)?;
        if entries.len() as u64 > opts.limits.max_entries {
            return Err(Error::LimitExceeded(opts.limits));
        }
        std::fs::create_dir_all(&opts.dest_dir)?;

        // Directories whose mtime is restored bottom-up when preserve_mtime.
        let mut dir_mtimes: Vec<(PathBuf, SystemTime)> = Vec::new();

        for meta in &entries {
            if cancel.is_cancelled() {
                return Err(Error::Cancelled);
            }
            let outcome = Self::extract_one(
                archive,
                meta,
                opts,
                progress,
                cancel,
                &mut report,
                &mut dir_mtimes,
            );
            match outcome {
                Ok(()) => {}
                Err(e) if is_fatal(&e) => return Err(e),
                Err(e) => report.failed.push((meta.path.clone(), e)),
            }
            if cancel.is_cancelled() {
                return Err(Error::Cancelled);
            }
        }

        if opts.preserve_mtime {
            restore_dir_mtimes(&mut dir_mtimes);
        }
        Ok(report)
    }

    /// Extract one entry, pairing `on_entry_start`/`on_entry_done` for every
    /// non-fatal outcome.
    fn extract_one(
        archive: &dyn Archive,
        meta: &EntryMeta,
        opts: &ExtractOptions,
        progress: &mut dyn ProgressSink,
        cancel: &CancellationToken,
        report: &mut ExtractReport,
        dir_mtimes: &mut Vec<(PathBuf, SystemTime)>,
    ) -> Result<()> {
        progress.on_entry_start(&meta.path, meta.uncompressed_size);
        let result = Self::extract_inner(archive, meta, opts, progress, cancel, report, dir_mtimes);
        // Pair `on_entry_done` with `on_entry_start` for every non-fatal
        // outcome (fatal errors abort the run entirely).
        let fatal = matches!(
            &result,
            Err(Error::Cancelled) | Err(Error::LimitExceeded(_))
        );
        if !fatal {
            progress.on_entry_done(&meta.path);
        }
        result
    }

    fn extract_inner(
        archive: &dyn Archive,
        meta: &EntryMeta,
        opts: &ExtractOptions,
        progress: &mut dyn ProgressSink,
        cancel: &CancellationToken,
        report: &mut ExtractReport,
        dir_mtimes: &mut Vec<(PathBuf, SystemTime)>,
    ) -> Result<()> {
        // `EntryPath` guarantees no `..`/absolute components, so joining it to
        // `dest_dir` cannot escape the destination (second line of defense is
        // the canonical-path check kept for the future extraction of
        // symbolic links).
        let dest = opts.dest_dir.join(meta.path.as_str());
        match meta.kind {
            NodeKind::Dir => {
                std::fs::create_dir_all(&dest)?;
                if opts.preserve_mtime
                    && let Some(t) = meta.mtime
                {
                    dir_mtimes.push((dest, t));
                }
                report.extracted += 1;
            }
            NodeKind::Symlink => {
                // M1 safety default: never materialize symlinks (they could
                // point outside dest_dir); counted as skipped.
                report.skipped += 1;
            }
            NodeKind::File | NodeKind::Archive => {
                if should_skip_existing(meta, &dest, opts, progress) {
                    report.skipped += 1;
                    return Ok(());
                }
                if let Some(parent) = dest.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                let mut sink = std::fs::File::create(&dest)?;
                copy_with_progress(
                    archive,
                    meta,
                    &mut sink,
                    progress,
                    cancel,
                    opts.limits,
                    &mut report.total_bytes,
                )?;
                if opts.preserve_mtime
                    && let Some(t) = meta.mtime
                {
                    set_modified(&dest, t)?;
                }
                report.extracted += 1;
            }
        }
        Ok(())
    }
}

/// Resolve `selection` to entry metadata (empty selection = all entries).
fn resolve_selection(archive: &dyn Archive, selection: &[EntryPath]) -> Result<Vec<EntryMeta>> {
    let entries = archive.entries()?;
    if selection.is_empty() {
        return Ok(entries);
    }
    selection
        .iter()
        .map(|p| {
            entries
                .iter()
                .find(|e| &e.path == p)
                .cloned()
                .ok_or_else(|| Error::CorruptArchive(format!("no such entry in archive: {p}")))
        })
        .collect()
}

/// Whether an existing destination file should be skipped under `opts`.
fn should_skip_existing(
    meta: &EntryMeta,
    dest: &Path,
    opts: &ExtractOptions,
    progress: &mut dyn ProgressSink,
) -> bool {
    if !dest.exists() {
        return false;
    }
    match opts.overwrite {
        OverwritePolicy::Always => false,
        // `Ask` delegates the decision to the sink; the default implementation
        // skips existing files (safe default, matches the GUI's behaviour
        // before the interactive dialog was wired up).
        OverwritePolicy::Ask => {
            progress.on_ask_overwrite(&meta.path, dest) == OverwriteDecision::Skip
        }
        OverwritePolicy::Never => true,
        OverwritePolicy::Newer => {
            match (
                meta.mtime,
                std::fs::metadata(dest).and_then(|m| m.modified()),
            ) {
                (Some(src), Ok(dst)) => dst >= src,
                // No source mtime or unreadable destination: overwrite.
                _ => false,
            }
        }
    }
}

/// Copy an entry to `sink` in chunks, reporting progress, checking
/// cancellation and enforcing the byte limit.
fn copy_with_progress(
    archive: &dyn Archive,
    meta: &EntryMeta,
    sink: &mut dyn Write,
    progress: &mut dyn ProgressSink,
    cancel: &CancellationToken,
    limits: SafetyLimits,
    total: &mut u64,
) -> Result<()> {
    let mut reader = archive.reader(meta)?;
    let mut buf = [0u8; COPY_CHUNK];
    loop {
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let n = reader.read(&mut buf)?;
        if n == 0 {
            return Ok(());
        }
        *total += n as u64;
        if *total > limits.max_total_bytes {
            return Err(Error::LimitExceeded(limits));
        }
        sink.write_all(&buf[..n])?;
        progress.on_bytes(n as u64);
    }
}

/// Set the modification time of an existing file.
///
/// Uses `filetime` (`utimensat` on Unix, `FILE_WRITE_ATTRIBUTES` on Windows)
/// so a read-only destination file does not need write permission just to
/// update its mtime (see `local-doc/research-time-filetime.md`).
fn set_modified(path: &Path, t: SystemTime) -> Result<()> {
    filetime::set_file_mtime(path, filetime::FileTime::from_system_time(t))?;
    Ok(())
}

/// Restore directory mtimes, children first so later file writes do not
/// clobber parent timestamps. Best-effort: failures are ignored.
fn restore_dir_mtimes(dirs: &mut [(PathBuf, SystemTime)]) {
    dirs.sort_by_key(|(p, _)| std::cmp::Reverse(p.components().count()));
    for (path, t) in dirs {
        let _ = set_modified(path, *t);
    }
}

/// Errors that abort the whole run rather than being recorded per-entry.
fn is_fatal(e: &Error) -> bool {
    matches!(e, Error::Cancelled | Error::LimitExceeded(_))
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
