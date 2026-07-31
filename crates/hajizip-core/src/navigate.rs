//! Navigation over an archive tree, including nested archives.

use std::sync::Arc;

use crate::archive::{Archive, OpenOptions};
use crate::codec::gzip::GzipFormat;
use crate::encoding::FilenameEncoding;
use crate::error::{Error, Result};
use crate::extract::SafetyLimits;
use crate::model::{EntryMeta, EntryPath, Location, NodeKind, Secret};
use crate::registry::Registry;
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
    /// Options used when opening nested archives (password / encoding).
    opts: OpenOptions,
}

impl Navigator {
    /// Open a top-level archive and create a navigator for it.
    ///
    /// The core's built-in format set (zip / tar / gzip) is used. Applications
    /// that compose their own extended `Registry` (e.g. the GUI) should open
    /// the top level themselves and rely on `enter`/`back` here.
    pub fn open_root(src: Source, opts: &OpenOptions) -> Result<Self> {
        let registry = Registry::new()
            .register_archive(crate::archive::zip::ZipFormat)
            .register_archive(crate::archive::tar::TarFormat)
            .register_codec(GzipFormat);
        let archive = registry.open_archive(src, opts)?;
        Ok(Self {
            stack: vec![Frame {
                archive: Arc::from(archive),
                focus: None,
            }],
            opts: opts.clone(),
        })
    }

    /// The archive at the top of the stack.
    pub fn current(&self) -> Result<&dyn Archive> {
        self.stack
            .last()
            .map(|f| f.archive.as_ref())
            .ok_or_else(|| Error::CorruptArchive("navigator stack is empty".into()))
    }

    /// Enter a child directory or nested archive.
    ///
    /// - [`NodeKind::Dir`]: moves the focus within the current archive;
    /// - [`NodeKind::Archive`] / [`NodeKind::File`]: opens the entry as a
    ///   nested archive (sniff-based `Archive` marking may not catch every
    ///   case, so plain files are probed) and pushes a new level;
    /// - [`NodeKind::Symlink`]: rejected (symlinks are never materialized).
    pub fn enter(&mut self, entry: &EntryMeta) -> Result<()> {
        match entry.kind {
            NodeKind::Dir => {
                let frame = self
                    .stack
                    .last_mut()
                    .ok_or_else(|| Error::CorruptArchive("navigator stack is empty".into()))?;
                frame.focus = Some(entry.path.clone());
                Ok(())
            }
            NodeKind::Archive | NodeKind::File => {
                let frame = self
                    .stack
                    .last()
                    .ok_or_else(|| Error::CorruptArchive("navigator stack is empty".into()))?;
                let nested = frame.archive.open_nested(entry, &self.opts)?;
                self.stack.push(Frame {
                    archive: Arc::from(nested),
                    focus: None,
                });
                Ok(())
            }
            NodeKind::Symlink => Err(Error::CorruptArchive(format!(
                "cannot enter a symlink: {}",
                entry.path
            ))),
        }
    }

    /// Go back one level: from a subdirectory to the current archive's root,
    /// or from a nested archive to the enclosing level. Fails at the top.
    pub fn back(&mut self) -> Result<()> {
        let frame = self
            .stack
            .last_mut()
            .ok_or_else(|| Error::CorruptArchive("navigator stack is empty".into()))?;
        if frame.focus.take().is_some() {
            return Ok(());
        }
        if self.stack.len() > 1 {
            self.stack.pop();
            Ok(())
        } else {
            Err(Error::CorruptArchive("already at the top level".into()))
        }
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
    /// Maximum nesting depth of nested archives.
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

/// Owns an archive at a walk level: the borrowed top-level archive or an
/// owned nested archive.
enum Owner<'a> {
    Borrowed(&'a dyn Archive),
    Owned(Box<dyn Archive>),
}

impl Owner<'_> {
    fn open_nested(&self, entry: &EntryMeta, opts: &OpenOptions) -> Result<Box<dyn Archive>> {
        match self {
            Owner::Borrowed(a) => a.open_nested(entry, opts),
            Owner::Owned(a) => a.open_nested(entry, opts),
        }
    }
}

/// One DFS level of the walk: a nested-archive level plus its flat listing.
struct Level<'a> {
    owner: Owner<'a>,
    entries: Vec<EntryMeta>,
    pos: usize,
    /// Accumulated nested-archive path prefix, e.g. `"outer.zip/inner/"`.
    prefix: String,
    /// Nested-archive depth (0 = top level).
    depth: usize,
}

/// An iterator over archive entries, optionally descending into nested
/// archives (depth-first, pre-order; directories are not expanded separately
/// because the flat listing already contains them).
pub struct Walk<'a> {
    opts: WalkOptions,
    stack: Vec<Level<'a>>,
    yielded: usize,
    limit_hit: bool,
    pending_error: Option<Error>,
}

impl Iterator for Walk<'_> {
    type Item = Result<WalkItem>;

    fn next(&mut self) -> Option<Self::Item> {
        if let Some(e) = self.pending_error.take() {
            return Some(Err(e));
        }
        if self.limit_hit {
            return None;
        }
        if self.yielded >= self.opts.max_total_entries {
            self.limit_hit = true;
            return Some(Err(Error::LimitExceeded(SafetyLimits {
                max_entries: self.opts.max_total_entries as u64,
                ..SafetyLimits::default()
            })));
        }
        // Drop exhausted levels.
        while self
            .stack
            .last()
            .is_some_and(|top| top.pos >= top.entries.len())
        {
            self.stack.pop();
        }
        let top = self.stack.last_mut()?;
        let entry = top.entries[top.pos].clone();
        top.pos += 1;
        self.yielded += 1;
        let prefix = top.prefix.clone();
        let depth = top.depth;

        let location = Location(format!("{prefix}{}", entry.path));

        // Descend into nested archives (the entry itself is still yielded).
        if self.opts.recurse_nested_archives
            && entry.kind == NodeKind::Archive
            && depth < self.opts.max_depth
        {
            let nested_opts = OpenOptions {
                password: self.opts.password.clone(),
                // Nested archives are decoded with the default Auto strategy
                // (WalkOptions has no encoding field; a future contract
                // adjustment could thread it through).
                encoding: FilenameEncoding::Auto,
            };
            let nested = self
                .stack
                .last()
                .and_then(|l| l.owner.open_nested(&entry, &nested_opts).ok());
            if let Some(nested) = nested {
                let nested_entries = nested.entries().unwrap_or_default();
                self.stack.push(Level {
                    owner: Owner::Owned(nested),
                    entries: nested_entries,
                    pos: 0,
                    prefix: format!("{prefix}{}/", entry.path),
                    depth: depth + 1,
                });
            }
        }

        Some(Ok(WalkItem {
            location,
            meta: entry,
        }))
    }
}

/// Begin a recursive walk over `archive`.
///
/// Locations are relative to the archive (e.g. `inner.zip/docs/readme.txt`
/// for a nested archive `inner.zip` at the top level).
pub fn walk(archive: &dyn Archive, opts: WalkOptions) -> Walk<'_> {
    match archive.entries() {
        Ok(entries) => Walk {
            opts,
            stack: vec![Level {
                owner: Owner::Borrowed(archive),
                entries,
                pos: 0,
                prefix: String::new(),
                depth: 0,
            }],
            yielded: 0,
            limit_hit: false,
            pending_error: None,
        },
        Err(e) => Walk {
            opts,
            stack: Vec::new(),
            yielded: 0,
            limit_hit: false,
            pending_error: Some(e),
        },
    }
}
