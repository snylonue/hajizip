//! Navigation over an archive tree, including nested archives.

use std::sync::Arc;

use crate::archive::{Archive, OpenOptions};
use crate::error::{Error, Result};
use crate::model::{EntryMeta, EntryPath, Location, Secret};
use crate::source::Source;

/// One frame of the navigation stack: an open archive plus the focused path.
pub struct Frame {
    /// The archive open at this level.
    pub archive: Arc<dyn Archive>,
    /// The path currently in focus within that archive, if any.
    pub focus: Option<EntryPath>,
}

/// Navigates a single open archive, including descending into nested archives.
///
/// Single-stack by design: only one archive is open at a time (no tabs).
/// Opening a new top-level archive replaces the whole navigator.
pub struct Navigator {
    stack: Vec<Frame>,
}

impl Navigator {
    /// Open a top-level archive and create a navigator for it.
    pub fn open_root(_src: Source, _opts: &OpenOptions) -> Result<Self> {
        Err(Error::UnsupportedFeature("Navigator::open_root".into()))
    }

    /// The archive at the top of the stack.
    pub fn current(&self) -> Result<&dyn Archive> {
        self.stack
            .last()
            .map(|f| f.archive.as_ref())
            .ok_or_else(|| Error::UnsupportedFeature("navigator stack is empty".into()))
    }

    /// Enter a child directory or nested archive.
    pub fn enter(&mut self, _entry: &EntryMeta) -> Result<()> {
        Err(Error::UnsupportedFeature("Navigator::enter".into()))
    }

    /// Go back up one level.
    pub fn back(&mut self) -> Result<()> {
        Err(Error::UnsupportedFeature("Navigator::back".into()))
    }

    /// The navigation stack, for breadcrumb display.
    pub fn breadcrumb(&self) -> &[Frame] {
        &self.stack
    }
}

/// Options for recursive traversal.
#[derive(Debug, Clone)]
pub struct WalkOptions {
    /// Whether to descend into nested archives.
    pub recurse_nested_archives: bool,
    /// Maximum nesting depth.
    pub max_depth: usize,
    /// Maximum total number of entries to yield.
    pub max_total_entries: usize,
    /// Password used to open encrypted nested archives.
    pub password: Option<Secret>,
}

impl Default for WalkOptions {
    fn default() -> Self {
        Self {
            recurse_nested_archives: false,
            max_depth: 8,
            max_total_entries: 1_000_000,
            password: None,
        }
    }
}

/// A single item yielded by [`walk`].
#[derive(Debug, Clone)]
pub struct WalkItem {
    /// Fully-qualified location across nested archives.
    pub location: Location,
    /// The entry metadata.
    pub meta: EntryMeta,
}

/// An iterator over archive entries, optionally descending into nested archives.
pub struct Walk<'a> {
    _archive: &'a dyn Archive,
    _opts: WalkOptions,
}

impl Iterator for Walk<'_> {
    type Item = Result<WalkItem>;

    fn next(&mut self) -> Option<Self::Item> {
        // M0: traversal is not implemented yet.
        None
    }
}

/// Begin a recursive walk over `archive`.
pub fn walk(archive: &dyn Archive, opts: WalkOptions) -> Walk<'_> {
    Walk {
        _archive: archive,
        _opts: opts,
    }
}
