//! GUI controller: the UI-independent state machine that drives `hajizip-core`.
//!
//! The controller is deliberately split into two halves:
//!
//! * [`ControllerCore`] — a pure, synchronous state machine that turns an
//!   [`Intent`] into a stream of [`Event`]s via an emit callback. It owns the
//!   composed [`Registry`] (composition root), a client-side navigation stack
//!   built on the frozen `Archive` API (core's `Navigator` is a placeholder
//!   until M1), and the configuration. It contains no threads and no Dioxus
//!   types, so it can be unit-tested with fake `Archive` / `ArchiveFormat`
//!   implementations (see `test-plan.md` §11).
//! * [`spawn_controller`] — the transport layer used by the UI. It runs a
//!   [`ControllerCore`] on a dedicated worker thread and bridges intents and
//!   events to the UI thread over channels, so long-running core calls never
//!   block the interface (see `architecture.md` §5.5). Extraction progress is
//!   streamed through the emit callback, and cancellation goes through a
//!   shared token slot that the UI can trigger without waiting for the worker.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use hajizip_core::{
    Archive, CancellationToken, EntryMeta, EntryPath, Error as CoreError, ExtractEngine,
    ExtractOptions, ExtractReport, FilenameEncoding, NodeKind, OpenOptions, OverwriteDecision,
    OverwritePolicy, ProgressSink, Registry, Secret, Source,
};

use crate::config::AppConfig;

/// A user intention submitted to the controller.
#[derive(Debug, Clone)]
pub enum Intent {
    /// Open a top-level archive from disk, optionally with a password.
    Open {
        /// Path to the archive file.
        path: PathBuf,
        /// Password for encrypted archives, if any.
        password: Option<String>,
    },
    /// Enter a child directory or a nested archive.
    Enter {
        /// Path of the entry to enter (relative to the current archive).
        path: EntryPath,
    },
    /// Go back up one level (parent directory or outer archive).
    Back,
    /// Jump to a breadcrumb segment (identified by its index).
    JumpTo {
        /// Index into the breadcrumb emitted in the last `Navigated` event.
        depth: usize,
    },
    /// Extract a selection of entries (empty means all) to a directory.
    Extract {
        /// Entries to extract; empty means the whole current archive.
        selection: Vec<EntryPath>,
        /// Destination directory.
        dest_dir: PathBuf,
    },
    /// Extract a single file entry to a temporary location so the UI can open
    /// it with the system default application (preview).
    Preview {
        /// Path of the file entry to preview.
        path: EntryPath,
    },
    /// Cancel the in-flight operation (extraction).
    ///
    /// The UI uses [`ControllerHandle::cancel`] directly (no worker round
    /// trip); this variant is kept as part of the stable intent surface.
    #[allow(dead_code)]
    Cancel,
    /// Change the filename decoding strategy and refresh the view.
    SetEncoding(FilenameEncoding),
    /// Change the overwrite policy used by extraction.
    SetOverwrite(OverwritePolicy),
    /// Change whether extraction preserves entry modification times.
    SetPreserveMtime(bool),
}

/// A progress snapshot emitted during a long-running operation.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ProgressUpdate {
    /// Path of the entry currently being processed, if any.
    pub current: Option<EntryPath>,
    /// Bytes processed so far.
    pub bytes_done: u64,
    /// Total bytes expected, if known.
    pub bytes_total: Option<u64>,
    /// Entries completed so far.
    pub entries_done: u64,
}

/// A clickable breadcrumb segment emitted in `Navigated` events.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BreadcrumbSegment {
    /// Display label (archive file name, nested-archive entry name, or a
    /// directory component).
    pub label: String,
    /// Frame index in the navigation stack this segment points into.
    pub frame: usize,
    /// Directory within that frame (None = frame root).
    pub focus: Option<EntryPath>,
}

/// An event produced by the controller for the UI to render.
#[derive(Debug)]
pub enum Event {
    /// A top-level archive was opened successfully (root view).
    Opened {
        /// Display name of the archive (e.g. its file name).
        name: String,
        /// Flat listing of the archive's entries (the UI builds the tree).
        entries: Vec<EntryMeta>,
    },
    /// The view changed (Enter / Back / JumpTo / nested archive).
    Navigated {
        /// Clickable breadcrumb segments.
        breadcrumb: Vec<BreadcrumbSegment>,
        /// Directory currently in focus within the top archive (None = root).
        focus: Option<EntryPath>,
        /// Flat listing of the archive at the top of the navigation stack.
        entries: Vec<EntryMeta>,
    },
    /// The archive is encrypted and needs a password: either its header is
    /// encrypted (opening fails), or its members are (listing works, but
    /// reading — preview/extract — requires the password).
    PasswordRequired {
        /// Path of the archive that needs a password.
        path: PathBuf,
    },
    /// The supplied password was rejected.
    WrongPassword {
        /// Path of the archive whose password was wrong.
        path: PathBuf,
    },
    /// Progress of an in-flight extraction.
    Progress(ProgressUpdate),
    /// An existing destination file conflicts with an entry while the
    /// overwrite policy is [`OverwritePolicy::Ask`]. The worker thread blocks
    /// until the UI sends an [`OverwriteDecision`] back through the handle.
    AskOverwrite {
        /// Entry path whose destination already exists.
        path: EntryPath,
        /// Destination path that already exists.
        dest: PathBuf,
    },
    /// A single file entry was extracted to `temp_path` for previewing; the
    /// UI should open it with the system default application.
    PreviewReady {
        /// Path of the extracted temporary file.
        temp_path: PathBuf,
    },
    /// An extraction run completed.
    Done(ExtractReport),
    /// The in-flight operation was cancelled.
    Cancelled,
    /// The requested capability is not available yet (e.g. a milestone
    /// feature is not implemented, or a file entry cannot be opened).
    Unsupported(String),
    /// A generic, user-presentable error.
    Error(String),
    /// The configuration changed (settings UI should refresh and persist).
    ConfigChanged(AppConfig),
}

/// One level of the controller's navigation stack.
#[derive(Clone)]
struct NavFrame {
    /// The archive open at this level.
    archive: Arc<dyn Archive>,
    /// Display name (file name for the root frame, entry name for nested).
    name: String,
    /// Archive entry that opened this frame (None for the root frame).
    entry_path: Option<EntryPath>,
    /// Directory currently in focus within this archive (None = root).
    focus: Option<EntryPath>,
    /// Cached flat entry listing (avoids re-listing on every navigation).
    entries: Arc<Vec<EntryMeta>>,
}

/// Navigation context captured when a password-gated operation is deferred:
/// which top-level archive to re-open, and which nested-archive chain to
/// re-enter so the retry lands in the same frame it failed in.
#[derive(Clone)]
struct PendingContext {
    /// Top-level archive path (re-opened with the password).
    archive: PathBuf,
    /// Nested-archive entry chain below the root, re-entered in order.
    nested_chain: Vec<EntryPath>,
}

/// A preview deferred for lack of a password; retried after re-open.
struct PendingPreview {
    context: PendingContext,
    /// Entry to preview once the chain is re-entered.
    entry: EntryPath,
}

/// An extraction deferred for lack of a password; retried after re-open.
struct PendingExtract {
    context: PendingContext,
    /// Exact entry paths to extract (already expanded from the selection).
    selection: Vec<EntryPath>,
    /// Destination directory.
    dest_dir: PathBuf,
}

/// Shared slot holding the token of the in-flight operation, if any. The UI
/// handle can cancel it directly without going through the worker queue.
type CancelSlot = Arc<Mutex<Option<CancellationToken>>>;

/// The synchronous, UI-independent controller state machine.
pub struct ControllerCore {
    registry: Arc<Registry>,
    config: AppConfig,
    /// Navigation stack (single-stack by design; architecture.md §4.3).
    stack: Vec<NavFrame>,
    /// Token slot shared with the UI handle.
    cancel: CancelSlot,
    /// Receiver for per-file overwrite decisions made by the UI while an
    /// Ask-policy extraction blocks inside `on_ask_overwrite`.
    ask_decisions: Option<tokio::sync::mpsc::UnboundedReceiver<OverwriteDecision>>,
    /// Last successfully opened source, to re-open on encoding changes.
    last_open: Option<(PathBuf, Option<String>)>,
    /// Directory holding the most recent preview extraction (cleaned up
    /// before the next preview or when a new archive is opened).
    preview_dir: Option<PathBuf>,
    /// Preview deferred for lack of a password (content-encrypted archives
    /// open without a password, so the first read is what reveals the lock);
    /// retried once the archive is re-opened with a password.
    pending_preview: Option<PendingPreview>,
    /// Extraction deferred for lack of a password; retried after re-open.
    pending_extract: Option<PendingExtract>,
}

impl ControllerCore {
    /// Create a controller composing the given format registry and config.
    ///
    /// Tests construct cores directly with their own cancellation slot;
    /// production wiring goes through [`spawn_controller`].
    #[cfg(test)]
    pub fn new(registry: Arc<Registry>, config: AppConfig) -> Self {
        Self::with_cancel_slot(registry, config, Arc::new(Mutex::new(None)))
    }

    /// Create a controller sharing the given cancellation slot with the UI.
    #[cfg(test)]
    fn with_cancel_slot(registry: Arc<Registry>, config: AppConfig, cancel: CancelSlot) -> Self {
        Self::with_channels(registry, config, cancel, None)
    }

    /// Create a controller with an optional per-file overwrite decision
    /// channel (used by the interactive UI; `None` falls back to the safe
    /// skip default in `on_ask_overwrite`).
    fn with_channels(
        registry: Arc<Registry>,
        config: AppConfig,
        cancel: CancelSlot,
        ask_decisions: Option<tokio::sync::mpsc::UnboundedReceiver<OverwriteDecision>>,
    ) -> Self {
        Self {
            registry,
            config,
            stack: Vec::new(),
            cancel,
            ask_decisions,
            last_open: None,
            preview_dir: None,
            pending_preview: None,
            pending_extract: None,
        }
    }

    /// The active configuration.
    #[allow(dead_code)] // Used by tests and the settings UI via events.
    pub fn config(&self) -> &AppConfig {
        &self.config
    }

    /// Process a single intent, emitting events through `emit`.
    ///
    /// This is the heart of the controller and is safe to call from tests
    /// without any UI or threads. Long-running operations (extraction) stream
    /// progress events through `emit` as they run.
    pub fn handle(&mut self, intent: &Intent, emit: &mut (dyn FnMut(Event) + Send)) {
        match intent {
            Intent::Open { path, password } => self.open(path, password.as_deref(), emit),
            Intent::Enter { path } => self.enter(path, emit),
            Intent::Back => self.back(emit),
            Intent::JumpTo { depth } => self.jump_to(*depth, emit),
            Intent::Extract {
                selection,
                dest_dir,
            } => self.extract(selection, dest_dir, emit),
            Intent::Preview { path } => self.preview(path, emit),
            Intent::Cancel => {
                if let Some(token) = self.cancel.lock().unwrap().as_ref() {
                    token.cancel();
                }
                emit(Event::Cancelled);
            }
            Intent::SetEncoding(encoding) => {
                self.config.filename_encoding = *encoding;
                emit(Event::ConfigChanged(self.config.clone()));
                self.refresh_view(emit);
            }
            Intent::SetOverwrite(policy) => {
                self.config.overwrite_policy = *policy;
                emit(Event::ConfigChanged(self.config.clone()));
            }
            Intent::SetPreserveMtime(value) => {
                self.config.preserve_mtime = *value;
                emit(Event::ConfigChanged(self.config.clone()));
            }
        }
    }

    /// Open a top-level archive and list its entries.
    fn open(&mut self, path: &Path, password: Option<&str>, emit: &mut (dyn FnMut(Event) + Send)) {
        // A previous preview lives in the system temp dir; drop it when the
        // user opens a different archive (best effort).
        if let Some(dir) = self.preview_dir.take() {
            let _ = std::fs::remove_dir_all(&dir);
        }
        let opts = OpenOptions {
            password: password.map(Secret::new),
            encoding: self.config.filename_encoding,
        };

        let archive = match self
            .registry
            .open_archive(Source::Path(path.to_path_buf()), &opts)
        {
            Ok(archive) => archive,
            Err(CoreError::PasswordRequired) => {
                emit(Event::PasswordRequired {
                    path: path.to_path_buf(),
                });
                return;
            }
            Err(CoreError::WrongPassword) => {
                emit(Event::WrongPassword {
                    path: path.to_path_buf(),
                });
                return;
            }
            Err(e) => {
                emit(Event::Error(e.to_string()));
                return;
            }
        };

        let archive: Arc<dyn Archive> = Arc::from(archive);
        // Content-encrypted archives (RAR/7z with plain headers, the WinRAR
        // default) open without a password and list fine; the lock only shows
        // when a member is read. Prompt for a password right away so preview
        // and extraction work after the dialog instead of failing per read.
        let needs_password = password.is_none() && archive.capabilities().needs_password;
        match archive.entries() {
            Ok(entries) => {
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.display().to_string());
                self.stack = vec![NavFrame {
                    archive,
                    name: name.clone(),
                    entry_path: None,
                    focus: None,
                    entries: Arc::new(entries.clone()),
                }];
                self.last_open = Some((path.to_path_buf(), password.map(str::to_owned)));
                // Remember the archive for the recent-files list (pure config
                // bookkeeping; the UI persists it via `ConfigChanged`).
                self.config.record_recent(path.to_path_buf());
                emit(Event::ConfigChanged(self.config.clone()));
                emit(Event::Opened { name, entries });
                if needs_password {
                    emit(Event::PasswordRequired {
                        path: path.to_path_buf(),
                    });
                }
                // Retry any operation deferred for lack of a password, now
                // that this archive is open with one.
                if password.is_some() {
                    self.retry_pending(path, emit);
                }
            }
            Err(e) => emit(Event::Error(e.to_string())),
        }
    }

    /// Re-open the last source with the current encoding (encoding switch
    /// refresh). Navigation resets to the archive root.
    fn refresh_view(&mut self, emit: &mut (dyn FnMut(Event) + Send)) {
        let Some((path, password)) = self.last_open.clone() else {
            return;
        };
        self.open(&path, password.as_deref(), emit);
    }

    /// Enter a child directory or a nested archive.
    fn enter(&mut self, path: &EntryPath, emit: &mut (dyn FnMut(Event) + Send)) {
        let Some(top) = self.stack.last().cloned() else {
            emit(Event::Error("no archive open".into()));
            return;
        };

        let entry = top.entries.iter().find(|e| e.path == *path).cloned();
        let kind = match &entry {
            Some(e) => e.kind,
            // A directory implied by path prefixes (e.g. "a/b.txt" implies "a").
            None if top
                .entries
                .iter()
                .any(|e| e.path.is_under(path) && e.path != *path) =>
            {
                NodeKind::Dir
            }
            None => {
                emit(Event::Error(format!("entry not found: {path}")));
                return;
            }
        };

        match kind {
            NodeKind::Dir => {
                if let Some(frame) = self.stack.last_mut() {
                    frame.focus = Some(path.clone());
                }
                self.emit_navigated(emit);
            }
            NodeKind::Archive => {
                let Some(entry) = entry else {
                    emit(Event::Error("archive entry not found".into()));
                    return;
                };
                let opts = OpenOptions {
                    password: self
                        .last_open
                        .as_ref()
                        .and_then(|(_, p)| p.as_deref())
                        .map(Secret::new),
                    encoding: self.config.filename_encoding,
                };
                match top.archive.open_nested(&entry, &opts) {
                    Ok(nested) => {
                        let nested: Arc<dyn Archive> = Arc::from(nested);
                        match nested.entries() {
                            Ok(entries) => {
                                let name = path
                                    .as_str()
                                    .rsplit('/')
                                    .next()
                                    .unwrap_or("archive")
                                    .to_string();
                                self.stack.push(NavFrame {
                                    archive: nested,
                                    name,
                                    entry_path: Some(path.clone()),
                                    focus: None,
                                    entries: Arc::new(entries),
                                });
                                self.emit_navigated(emit);
                            }
                            Err(e) => emit(Event::Error(e.to_string())),
                        }
                    }
                    Err(e) => emit(Event::Error(e.to_string())),
                }
            }
            _ => emit(Event::Unsupported(format!(
                "opening '{}' is not implemented yet",
                path.as_str()
            ))),
        }
    }

    /// Go back up one level: parent directory, or the outer archive.
    fn back(&mut self, emit: &mut (dyn FnMut(Event) + Send)) {
        if self.stack.is_empty() {
            emit(Event::Error("no archive open".into()));
            return;
        }
        let at_root = {
            let top = self.stack.last().unwrap();
            top.focus.is_none() && self.stack.len() == 1
        };
        if at_root {
            return; // Already at the top; stay put.
        }
        let top_focus = self.stack.last().and_then(|f| f.focus.clone());
        match top_focus {
            Some(focus) => {
                // Move to the parent directory within the same archive. The
                // parent of a validated `EntryPath` is always valid; handle the
                // (impossible) failure gracefully instead of panicking.
                let frame = self.stack.last_mut().unwrap();
                frame.focus = focus.parent();
            }
            None => {
                // Pop the nested-archive frame.
                self.stack.pop();
            }
        }
        self.emit_navigated(emit);
    }

    /// Jump to a breadcrumb segment (truncating the stack).
    fn jump_to(&mut self, depth: usize, emit: &mut (dyn FnMut(Event) + Send)) {
        let segments = self.breadcrumb();
        let Some(segment) = segments.get(depth).cloned() else {
            emit(Event::Error("invalid breadcrumb depth".into()));
            return;
        };
        self.stack.truncate(segment.frame + 1);
        if let Some(frame) = self.stack.last_mut() {
            frame.focus = segment.focus;
        }
        self.emit_navigated(emit);
    }

    /// Flatten the navigation stack into breadcrumb segments.
    fn breadcrumb(&self) -> Vec<BreadcrumbSegment> {
        let mut out = Vec::new();
        for (i, frame) in self.stack.iter().enumerate() {
            out.push(BreadcrumbSegment {
                label: frame.name.clone(),
                frame: i,
                focus: None,
            });
            if let Some(focus) = &frame.focus {
                let mut prefix = String::new();
                for component in focus.as_str().split('/') {
                    if !prefix.is_empty() {
                        prefix.push('/');
                    }
                    prefix.push_str(component);
                    // The prefix is built from validated path components, so
                    // construction cannot fail; skip the (impossible) failure
                    // gracefully instead of panicking.
                    let Some(path) = EntryPath::new(&prefix).ok() else {
                        continue;
                    };
                    out.push(BreadcrumbSegment {
                        label: component.to_string(),
                        frame: i,
                        focus: Some(path),
                    });
                }
            }
        }
        out
    }

    /// Emit the current view (top frame's focus + cached entries).
    fn emit_navigated(&mut self, emit: &mut (dyn FnMut(Event) + Send)) {
        let Some(top) = self.stack.last() else {
            return;
        };
        emit(Event::Navigated {
            breadcrumb: self.breadcrumb(),
            focus: top.focus.clone(),
            entries: top.entries.as_ref().clone(),
        });
    }

    /// Extract a single file entry to a temporary location for previewing.
    ///
    /// The temp file lives in the system temp dir under a per-process
    /// directory; the previous preview directory is removed first (best
    /// effort — the OS may still hold the file open). The resulting path is
    /// reported via [`Event::PreviewReady`] so the UI can hand it to the
    /// platform's default application.
    ///
    /// Reading a content-encrypted member without a password fails with
    /// [`CoreError::PasswordRequired`]; the entry is remembered and a
    /// [`Event::PasswordRequired`] is emitted so the UI can collect a
    /// password, after which `open` retries this preview automatically.
    fn preview(&mut self, path: &EntryPath, emit: &mut (dyn FnMut(Event) + Send)) {
        let Some(top) = self.stack.last().cloned() else {
            emit(Event::Error("no archive open".into()));
            return;
        };
        let Some(entry) = top.entries.iter().find(|e| e.path == *path).cloned() else {
            emit(Event::Error(format!("entry not found: {path}")));
            return;
        };
        if entry.kind != NodeKind::File {
            emit(Event::Error(format!(
                "cannot preview '{}' (not a file)",
                path.as_str()
            )));
            return;
        }

        // Drop the previous preview dir (best effort; the OS may still hold
        // the file open until the external app is done with it).
        if let Some(dir) = self.preview_dir.take() {
            let _ = std::fs::remove_dir_all(&dir);
        }
        let dir = std::env::temp_dir().join(format!("hajizip-preview-{}", std::process::id()));
        let base = path.as_str().rsplit('/').next().unwrap_or("preview");
        let dest = dir.join(base);

        let result = (|| -> Result<(), CoreError> {
            std::fs::create_dir_all(&dir)?;
            let mut reader = top.archive.reader(&entry)?;
            let mut file = std::fs::File::create(&dest)?;
            std::io::copy(&mut reader, &mut file)?;
            Ok(())
        })();

        match result {
            Ok(()) => {
                self.preview_dir = Some(dir);
                emit(Event::PreviewReady { temp_path: dest });
            }
            Err(CoreError::PasswordRequired) => {
                // The archive is content-encrypted and was opened without a
                // password. Remember the entry and ask for a password; the
                // dialog re-opens the archive, and the retry picks the
                // preview back up (see `retry_pending`). Drop the empty
                // temp dir.
                let _ = std::fs::remove_dir_all(&dir);
                let context = self.pending_context();
                self.pending_preview = Some(PendingPreview {
                    context: context.clone(),
                    entry: path.clone(),
                });
                emit(Event::PasswordRequired {
                    path: context.archive,
                });
            }
            Err(e) => emit(Event::Error(e.to_string())),
        }
    }

    /// Run an extraction by delegating to core's [`ExtractEngine`], bridging
    /// progress and cancellation to the UI surface.
    ///
    /// Selection semantics differ from core's exact-match list: the GUI treats
    /// selecting a directory as selecting its whole subtree (`is_under`), so
    /// the selection is expanded to exact entry paths first.
    ///
    /// When the archive was opened without a password and the selection
    /// contains encrypted entries, the run is deferred and a
    /// [`Event::PasswordRequired`] is emitted instead; after the dialog
    /// re-opens the archive with a password, `retry_pending` re-runs it.
    fn extract(
        &mut self,
        selection: &[EntryPath],
        dest_dir: &Path,
        emit: &mut (dyn FnMut(Event) + Send),
    ) {
        let Some(top) = self.stack.last().cloned() else {
            emit(Event::Error("no archive open".into()));
            return;
        };
        let archive = top.archive.clone();
        let all = top.entries.as_ref().clone();

        let selected: Vec<EntryMeta> = if selection.is_empty() {
            all.clone()
        } else {
            all.into_iter()
                .filter(|e| selection.iter().any(|s| is_under(e, s)))
                .collect()
        };
        let exact: Vec<EntryPath> = selected.iter().map(|e| e.path.clone()).collect();

        // Content-encrypted entries cannot be read without a password; ask
        // for one up front instead of failing per entry. The extraction is
        // deferred and retried once the archive is re-opened with a password
        // (see `retry_pending`).
        if top.archive.capabilities().needs_password && selected.iter().any(|e| e.encrypted) {
            let context = self.pending_context();
            self.pending_extract = Some(PendingExtract {
                context: context.clone(),
                selection: exact,
                dest_dir: dest_dir.to_path_buf(),
            });
            emit(Event::PasswordRequired {
                path: context.archive,
            });
            return;
        }

        let opts = ExtractOptions {
            dest_dir: dest_dir.to_path_buf(),
            overwrite: self.config.overwrite_policy,
            preserve_mtime: self.config.preserve_mtime,
            // No archive name is available here (frozen ExtractEngine API);
            // the GUI does not offer a top-folder option either.
            create_top_folder: false,
            limits: self.config.safety_limits,
        };

        // Use a fresh token, or honour one already installed (e.g. the UI
        // cancelled just before the run started).
        let token = self.cancel.lock().unwrap().clone().unwrap_or_default();
        *self.cancel.lock().unwrap() = Some(token.clone());

        let total_bytes: u64 = selected.iter().filter_map(|e| e.uncompressed_size).sum();
        let mut bridge = ExtractProgressBridge {
            emit,
            total_bytes,
            done_bytes: 0,
            entries_done: 0,
            ask_decisions: self.ask_decisions.as_mut(),
        };

        let result = ExtractEngine::run(archive.as_ref(), &exact, &opts, &mut bridge, &token);
        *self.cancel.lock().unwrap() = None;
        match result {
            Ok(report) => emit(Event::Done(report)),
            Err(CoreError::Cancelled) => emit(Event::Cancelled),
            Err(e) => emit(Event::Error(e.to_string())),
        }
    }

    /// Snapshot the current navigation context for a deferred operation: the
    /// top-level archive path and the nested-archive entry chain below it.
    fn pending_context(&self) -> PendingContext {
        PendingContext {
            archive: self
                .last_open
                .as_ref()
                .map(|(p, _)| p.clone())
                .unwrap_or_else(|| PathBuf::from("archive")),
            nested_chain: self
                .stack
                .iter()
                .skip(1)
                .filter_map(|f| f.entry_path.clone())
                .collect(),
        }
    }

    /// Re-run an operation deferred for lack of a password, now that the
    /// archive is open with one. Re-enters the recorded nested-archive chain
    /// so the retry lands in the frame the operation failed in.
    fn retry_pending(&mut self, path: &Path, emit: &mut (dyn FnMut(Event) + Send)) {
        if let Some(pending) = self.pending_preview.take()
            && pending.context.archive == path
        {
            self.reenter_chain(&pending.context.nested_chain, emit);
            self.preview(&pending.entry, emit);
        }
        if let Some(pending) = self.pending_extract.take()
            && pending.context.archive == path
        {
            self.reenter_chain(&pending.context.nested_chain, emit);
            self.extract(&pending.selection, &pending.dest_dir, emit);
        }
    }

    /// Re-enter a recorded nested-archive chain (each entry is an archive
    /// node in its parent frame). Failures surface as ordinary errors.
    fn reenter_chain(&mut self, chain: &[EntryPath], emit: &mut (dyn FnMut(Event) + Send)) {
        for path in chain {
            self.enter(path, emit);
        }
    }
}

/// Bridges core's [`ProgressSink`] callbacks into [`Event::Progress`]
/// emissions (one event per entry start, matching the previous controller
/// behaviour so the progress dialog keeps working).
struct ExtractProgressBridge<'a> {
    emit: &'a mut (dyn FnMut(Event) + Send),
    total_bytes: u64,
    done_bytes: u64,
    entries_done: u64,
    /// Per-file overwrite decision channel, when the UI provides one.
    ask_decisions: Option<&'a mut tokio::sync::mpsc::UnboundedReceiver<OverwriteDecision>>,
}

impl ProgressSink for ExtractProgressBridge<'_> {
    fn on_entry_start(&mut self, path: &EntryPath, _size: Option<u64>) {
        (self.emit)(Event::Progress(ProgressUpdate {
            current: Some(path.clone()),
            bytes_done: self.done_bytes,
            bytes_total: Some(self.total_bytes),
            entries_done: self.entries_done,
        }));
    }

    fn on_bytes(&mut self, delta: u64) {
        self.done_bytes += delta;
    }

    fn on_entry_done(&mut self, _path: &EntryPath) {
        self.entries_done += 1;
    }

    fn on_ask_overwrite(&mut self, path: &EntryPath, dest: &Path) -> OverwriteDecision {
        // Ask the UI, then block until it answers. The worker thread is the
        // only source of `AskOverwrite` events and waits for exactly one
        // decision per event, so decisions arrive in order.
        (self.emit)(Event::AskOverwrite {
            path: path.clone(),
            dest: dest.to_path_buf(),
        });
        match self.ask_decisions.as_mut() {
            // The UI dropped the decision channel: fall back to safe skip.
            Some(rx) => rx.blocking_recv().unwrap_or(OverwriteDecision::Skip),
            None => OverwriteDecision::Skip,
        }
    }
}

/// Whether `entry` is `selection` itself or a descendant of a selected dir.
fn is_under(entry: &EntryMeta, selection: &EntryPath) -> bool {
    entry.path.is_under(selection)
}

/// A handle used by the UI to submit intents and cancel in-flight work.
#[derive(Clone)]
pub struct ControllerHandle {
    /// Send an intent to the worker thread. Never blocks.
    pub commands: tokio::sync::mpsc::UnboundedSender<Intent>,
    /// Shared cancellation slot; cancelling never waits for the worker.
    cancel: CancelSlot,
    /// Send a per-file overwrite decision back to the worker while it blocks
    /// inside `on_ask_overwrite`. Never blocks.
    pub decisions: tokio::sync::mpsc::UnboundedSender<OverwriteDecision>,
}

impl ControllerHandle {
    /// Request cancellation of the in-flight operation without round-tripping
    /// through the worker thread.
    pub fn cancel(&self) {
        if let Some(token) = self.cancel.lock().unwrap().as_ref() {
            token.cancel();
        }
    }
}

/// Spawn a [`ControllerCore`] on a dedicated worker thread.
///
/// Intents are fed in over the returned [`ControllerHandle`]; events come out
/// of the returned receiver, which the UI drains on the reactive thread. The
/// worker owns the core and all blocking `hajizip-core` calls happen there,
/// keeping the interface responsive.
pub fn spawn_controller(
    registry: Arc<Registry>,
    config: AppConfig,
) -> (
    ControllerHandle,
    tokio::sync::mpsc::UnboundedReceiver<Event>,
) {
    let (cmd_tx, mut cmd_rx) = tokio::sync::mpsc::unbounded_channel::<Intent>();
    let (event_tx, event_rx) = tokio::sync::mpsc::unbounded_channel::<Event>();
    let (decision_tx, decision_rx) = tokio::sync::mpsc::unbounded_channel::<OverwriteDecision>();
    let cancel: CancelSlot = Arc::new(Mutex::new(None));
    let cancel_for_core = cancel.clone();

    std::thread::spawn(move || {
        let mut core =
            ControllerCore::with_channels(registry, config, cancel_for_core, Some(decision_rx));
        // `blocking_recv` is fine here: this is a dedicated OS thread whose
        // whole job is to wait for and process intents.
        while let Some(intent) = cmd_rx.blocking_recv() {
            let mut alive = true;
            core.handle(&intent, &mut |event| {
                // If the UI dropped the receiver, stop the worker.
                alive &= event_tx.send(event).is_ok();
            });
            if !alive {
                break;
            }
        }
    });

    (
        ControllerHandle {
            commands: cmd_tx,
            cancel,
            decisions: decision_tx,
        },
        event_rx,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use hajizip_core::{
        ArchiveFormat, Capabilities, Error, NodeRef, Result, Utf8Flag, decode_filename,
    };
    use std::io::{Read, Write};
    use std::sync::Mutex as StdMutex;

    /// Run an intent, collecting emitted events.
    fn run(core: &mut ControllerCore, intent: &Intent) -> Vec<Event> {
        let mut events = Vec::new();
        core.handle(intent, &mut |e| events.push(e));
        events
    }

    /// Build a minimal file entry for tests.
    fn meta(path: &str) -> EntryMeta {
        EntryMeta {
            path: EntryPath::new(path).unwrap(),
            raw_name: path.as_bytes().to_vec(),
            kind: NodeKind::File,
            uncompressed_size: Some(0),
            compressed_size: None,
            mtime: None,
            mode: None,
            crc: None,
            encrypted: false,
            comment: None,
        }
    }

    fn caps() -> Capabilities {
        Capabilities {
            random_access: true,
            encrypted: false,
            needs_password: false,
            can_write: false,
        }
    }

    /// A fake archive that simply returns a fixed entry list and echoes bytes
    /// on extraction.
    #[derive(Clone)]
    struct FakeArchive {
        entries: Vec<EntryMeta>,
        /// Called before each `extract_to`; lets tests inject cancellation.
        before_extract: Option<Arc<dyn Fn() + Send + Sync>>,
    }

    impl Archive for FakeArchive {
        fn entries(&self) -> Result<Vec<EntryMeta>> {
            Ok(self.entries.clone())
        }
        fn root(&self) -> Result<NodeRef> {
            Err(Error::UnsupportedFeature("fake".into()))
        }
        fn node(&self, _path: &EntryPath) -> Result<NodeRef> {
            Err(Error::UnsupportedFeature("fake".into()))
        }
        fn reader<'s>(&'s self, entry: &EntryMeta) -> Result<Box<dyn Read + Send + 's>> {
            // Core's `ExtractEngine` reads through `Archive::reader`; return the
            // same synthetic content as `extract_to`.
            let n = entry.uncompressed_size.unwrap_or(0);
            Ok(Box::new(std::io::Cursor::new(vec![b'x'; n as usize])))
        }
        fn extract_to(&self, entry: &EntryMeta, sink: &mut dyn Write) -> Result<u64> {
            if let Some(hook) = &self.before_extract {
                hook();
            }
            let n = entry.uncompressed_size.unwrap_or(0);
            let bytes = vec![b'x'; n as usize];
            sink.write_all(&bytes)?;
            Ok(n)
        }
        fn open_nested(&self, _entry: &EntryMeta, _opts: &OpenOptions) -> Result<Box<dyn Archive>> {
            Err(Error::UnsupportedFeature("fake".into()))
        }
        fn capabilities(&self) -> Capabilities {
            caps()
        }
    }

    /// A format that always matches and opens a [`FakeArchive`].
    struct OkFormat {
        entries: Vec<EntryMeta>,
    }

    impl ArchiveFormat for OkFormat {
        fn id(&self) -> &str {
            "fake"
        }
        fn display_name(&self) -> &str {
            "Fake"
        }
        fn extensions(&self) -> &[&str] {
            &["fake"]
        }
        fn matches(&self, _head: &[u8], _ext: Option<&str>) -> bool {
            true
        }
        fn open(&self, _src: Source, _opts: &OpenOptions) -> Result<Box<dyn Archive>> {
            Ok(Box::new(FakeArchive {
                entries: self.entries.clone(),
                before_extract: None,
            }))
        }
    }

    /// A format that decodes `raw_name` with the requested encoding, so
    /// encoding-switch refresh is observable.
    struct EncodingFormat {
        raw: Vec<Vec<u8>>,
        last_encoding: Arc<StdMutex<Option<FilenameEncoding>>>,
    }

    impl ArchiveFormat for EncodingFormat {
        fn id(&self) -> &str {
            "enc"
        }
        fn display_name(&self) -> &str {
            "Enc"
        }
        fn extensions(&self) -> &[&str] {
            &["enc"]
        }
        fn matches(&self, _head: &[u8], _ext: Option<&str>) -> bool {
            true
        }
        fn open(&self, _src: Source, opts: &OpenOptions) -> Result<Box<dyn Archive>> {
            *self.last_encoding.lock().unwrap() = Some(opts.encoding);
            let entries = self
                .raw
                .iter()
                .map(|raw| {
                    let name = decode_filename(raw, opts.encoding, Utf8Flag(false))?;
                    let mut e = meta(&name);
                    e.raw_name = raw.clone();
                    Ok(e)
                })
                .collect::<Result<Vec<_>>>()?;
            Ok(Box::new(FakeArchive {
                entries,
                before_extract: None,
            }))
        }
    }

    /// A format that always matches but demands a password.
    struct PasswordFormat;

    impl ArchiveFormat for PasswordFormat {
        fn id(&self) -> &str {
            "locked"
        }
        fn display_name(&self) -> &str {
            "Locked"
        }
        fn extensions(&self) -> &[&str] {
            &["locked"]
        }
        fn matches(&self, _head: &[u8], _ext: Option<&str>) -> bool {
            true
        }
        fn open(&self, _src: Source, _opts: &OpenOptions) -> Result<Box<dyn Archive>> {
            Err(Error::PasswordRequired)
        }
    }

    /// Write a small temp file so `Source::Path` can sniff real bytes.
    fn temp_archive(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("hajizip-gui-test-{name}"));
        std::fs::write(&path, b"some archive bytes").unwrap();
        path
    }

    fn core_with(registry: Registry) -> ControllerCore {
        ControllerCore::new(Arc::new(registry), AppConfig::default())
    }

    /// Build a core with an overwrite-decision channel so tests can answer
    /// the per-file `Ask` prompt from another thread (the controller blocks
    /// inside `ExtractEngine::run` until the decision arrives).
    fn core_with_ask(
        registry: Registry,
    ) -> (
        ControllerCore,
        tokio::sync::mpsc::UnboundedSender<OverwriteDecision>,
    ) {
        let (ask_tx, ask_rx) = tokio::sync::mpsc::unbounded_channel();
        let core = ControllerCore::with_channels(
            Arc::new(registry),
            AppConfig::default(),
            Arc::new(Mutex::new(None)),
            Some(ask_rx),
        );
        (core, ask_tx)
    }

    #[test]
    fn open_with_registered_format_emits_opened() {
        let registry = Registry::new().register_archive(OkFormat {
            entries: vec![meta("a.txt"), meta("dir/b.txt")],
        });
        let mut core = core_with(registry);

        let path = temp_archive("ok.fake");
        let events = run(
            &mut core,
            &Intent::Open {
                path: path.clone(),
                password: None,
            },
        );

        match &events[..] {
            [Event::ConfigChanged(_), Event::Opened { name, entries }] => {
                assert_eq!(name, &path.file_name().unwrap().to_string_lossy());
                assert_eq!(entries.len(), 2);
            }
            other => panic!("expected ConfigChanged + Opened, got {other:?}"),
        }
    }

    #[test]
    fn open_records_recent_file_in_config() {
        let registry = Registry::new().register_archive(OkFormat {
            entries: vec![meta("a.txt")],
        });
        let mut core = core_with(registry);

        let first = temp_archive("recent1.fake");
        let second = temp_archive("recent2.fake");
        run(
            &mut core,
            &Intent::Open {
                path: first.clone(),
                password: None,
            },
        );
        run(
            &mut core,
            &Intent::Open {
                path: second.clone(),
                password: None,
            },
        );

        // Most recent first; duplicates are not recorded twice.
        assert_eq!(
            core.config().recent_files,
            vec![second.clone(), first.clone()]
        );

        // Re-opening the same path moves it back to the front.
        run(
            &mut core,
            &Intent::Open {
                path: first.clone(),
                password: None,
            },
        );
        assert_eq!(core.config().recent_files, vec![first, second]);
    }

    #[test]
    fn preview_extracts_file_to_temp_and_emits_ready() {
        let mut f = meta("photo.png");
        f.uncompressed_size = Some(5);
        let registry = Registry::new().register_archive(OkFormat { entries: vec![f] });
        let mut core = core_with(registry);
        run(
            &mut core,
            &Intent::Open {
                path: temp_archive("preview.fake"),
                password: None,
            },
        );

        let events = run(
            &mut core,
            &Intent::Preview {
                path: EntryPath::new("photo.png").unwrap(),
            },
        );
        match &events[..] {
            [Event::PreviewReady { temp_path }] => {
                // The fake archive's reader yields `x` bytes.
                assert_eq!(std::fs::read(temp_path).unwrap(), vec![b'x'; 5]);
                let _ = std::fs::remove_dir_all(temp_path.parent().unwrap());
            }
            other => panic!("expected PreviewReady, got {other:?}"),
        }
    }

    #[test]
    fn preview_non_file_entry_errors() {
        let registry = Registry::new().register_archive(OkFormat {
            entries: vec![meta("dir/b.txt")],
        });
        let mut core = core_with(registry);
        run(
            &mut core,
            &Intent::Open {
                path: temp_archive("preview-dir.fake"),
                password: None,
            },
        );

        // "dir" is an implied directory, not a file entry.
        let events = run(
            &mut core,
            &Intent::Preview {
                path: EntryPath::new("dir").unwrap(),
            },
        );
        assert!(
            matches!(&events[..], [Event::Error(_)]),
            "expected Error, got {events:?}"
        );
    }

    #[test]
    fn preview_unknown_entry_errors() {
        let registry = Registry::new().register_archive(OkFormat {
            entries: vec![meta("a.txt")],
        });
        let mut core = core_with(registry);
        run(
            &mut core,
            &Intent::Open {
                path: temp_archive("preview-missing.fake"),
                password: None,
            },
        );

        let events = run(
            &mut core,
            &Intent::Preview {
                path: EntryPath::new("nope.txt").unwrap(),
            },
        );
        assert!(
            matches!(&events[..], [Event::Error(_)]),
            "expected Error, got {events:?}"
        );
    }

    #[test]
    fn open_with_empty_registry_emits_error() {
        let mut core = core_with(Registry::new());
        let events = run(
            &mut core,
            &Intent::Open {
                path: PathBuf::from("/nonexistent/definitely-missing.zip"),
                password: None,
            },
        );
        assert!(
            matches!(&events[..], [Event::Error(_)]),
            "expected Error, got {events:?}"
        );
    }

    #[test]
    fn open_encrypted_without_password_emits_password_required() {
        let registry = Registry::new().register_archive(PasswordFormat);
        let mut core = core_with(registry);

        let path = temp_archive("locked.fake");
        let events = run(
            &mut core,
            &Intent::Open {
                path: path.clone(),
                password: None,
            },
        );

        assert!(
            matches!(&events[..], [Event::PasswordRequired { .. }]),
            "expected PasswordRequired, got {events:?}"
        );
    }

    #[test]
    fn enter_dir_emits_navigated_with_breadcrumb() {
        let registry = Registry::new().register_archive(OkFormat {
            entries: vec![meta("a.txt"), meta("dir/b.txt")],
        });
        let mut core = core_with(registry);
        let path = temp_archive("nav.fake");
        run(
            &mut core,
            &Intent::Open {
                path: path.clone(),
                password: None,
            },
        );

        let events = run(
            &mut core,
            &Intent::Enter {
                path: EntryPath::new("dir").unwrap(),
            },
        );
        match &events[..] {
            [
                Event::Navigated {
                    breadcrumb,
                    focus,
                    entries,
                },
            ] => {
                assert_eq!(focus.as_ref().unwrap().as_str(), "dir");
                assert_eq!(breadcrumb.len(), 2); // archive name + "dir"
                assert_eq!(
                    breadcrumb[0].label,
                    path.file_name().unwrap().to_string_lossy()
                );
                assert_eq!(breadcrumb[1].label, "dir");
                assert_eq!(entries.len(), 2);
            }
            other => panic!("expected Navigated, got {other:?}"),
        }
    }

    #[test]
    fn back_returns_to_root_then_is_noop() {
        let registry = Registry::new().register_archive(OkFormat {
            entries: vec![meta("a.txt"), meta("dir/sub/b.txt")],
        });
        let mut core = core_with(registry);
        run(
            &mut core,
            &Intent::Open {
                path: temp_archive("back.fake"),
                password: None,
            },
        );

        // dir → dir/sub
        run(
            &mut core,
            &Intent::Enter {
                path: EntryPath::new("dir").unwrap(),
            },
        );
        run(
            &mut core,
            &Intent::Enter {
                path: EntryPath::new("dir/sub").unwrap(),
            },
        );
        // back → dir
        let events = run(&mut core, &Intent::Back);
        match &events[..] {
            [Event::Navigated { focus, .. }] => {
                assert_eq!(focus.as_ref().unwrap().as_str(), "dir");
            }
            other => panic!("expected Navigated, got {other:?}"),
        }
        // back → root
        run(&mut core, &Intent::Back);
        // back at root → no event
        let events = run(&mut core, &Intent::Back);
        assert!(events.is_empty(), "expected no events, got {events:?}");
    }

    #[test]
    fn enter_nested_archive_pushes_frame() {
        // The fake cannot open_nested; instead verify the error path surfaces
        // cleanly, and that a dir-implied path entry still works.
        let registry = Registry::new().register_archive(OkFormat {
            entries: vec![meta("a.txt")],
        });
        let mut core = core_with(registry);
        run(
            &mut core,
            &Intent::Open {
                path: temp_archive("nested.fake"),
                password: None,
            },
        );
        // Enter a file: unsupported, not a crash.
        let events = run(
            &mut core,
            &Intent::Enter {
                path: EntryPath::new("a.txt").unwrap(),
            },
        );
        assert!(matches!(&events[..], [Event::Unsupported(_)]));
    }

    #[test]
    fn extract_emits_progress_then_done() {
        let entries = vec![meta("a.txt"), meta("b.txt"), meta("c.txt")];
        let registry = Registry::new().register_archive(OkFormat { entries });
        let mut core = core_with(registry);
        run(
            &mut core,
            &Intent::Open {
                path: temp_archive("extract.fake"),
                password: None,
            },
        );

        let dest = std::env::temp_dir().join(format!("hajizip-gui-extract-{}", std::process::id()));
        std::fs::create_dir_all(&dest).unwrap();

        let events = run(
            &mut core,
            &Intent::Extract {
                selection: vec![],
                dest_dir: dest.clone(),
            },
        );
        let progress = events
            .iter()
            .filter(|e| matches!(e, Event::Progress(_)))
            .count();
        assert_eq!(progress, 3, "one progress per entry, got {events:?}");
        match events.last() {
            Some(Event::Done(report)) => {
                assert_eq!(report.extracted, 3);
                assert_eq!(report.skipped, 0);
                assert!(report.failed.is_empty());
            }
            other => panic!("expected Done, got {other:?}"),
        }
        assert!(dest.join("a.txt").exists());
        let _ = std::fs::remove_dir_all(&dest);
    }

    #[test]
    fn extract_honours_overwrite_policy() {
        let entries = vec![meta("a.txt")];
        let registry = Registry::new().register_archive(OkFormat { entries });
        let mut core = core_with(registry);
        run(
            &mut core,
            &Intent::Open {
                path: temp_archive("overwrite.fake"),
                password: None,
            },
        );
        // Default policy is Ask → without a decision channel, existing files
        // are skipped (safe default).
        let dest =
            std::env::temp_dir().join(format!("hajizip-gui-overwrite-{}", std::process::id()));
        std::fs::create_dir_all(&dest).unwrap();
        std::fs::write(dest.join("a.txt"), b"existing").unwrap();

        let events = run(
            &mut core,
            &Intent::Extract {
                selection: vec![],
                dest_dir: dest.clone(),
            },
        );
        match events.last() {
            Some(Event::Done(report)) => {
                assert_eq!(report.skipped, 1);
                assert_eq!(report.extracted, 0);
            }
            other => panic!("expected Done, got {other:?}"),
        }
        // Existing content preserved.
        assert_eq!(
            std::fs::read_to_string(dest.join("a.txt")).unwrap(),
            "existing"
        );
        let _ = std::fs::remove_dir_all(&dest);
    }

    #[test]
    fn ask_policy_prompts_and_overwrites_on_yes() {
        let entries = vec![meta("a.txt")];
        let registry = Registry::new().register_archive(OkFormat { entries });
        let (mut core, ask_tx) = core_with_ask(registry);
        run(
            &mut core,
            &Intent::Open {
                path: temp_archive("ask-yes.fake"),
                password: None,
            },
        );

        let dest = std::env::temp_dir().join(format!("hajizip-gui-ask-yes-{}", std::process::id()));
        std::fs::create_dir_all(&dest).unwrap();
        std::fs::write(dest.join("a.txt"), b"existing").unwrap();

        // Answer from another thread: the controller blocks inside
        // `ExtractEngine::run` until the decision arrives.
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(50));
            ask_tx.send(OverwriteDecision::Overwrite).unwrap();
        });

        let events = run(
            &mut core,
            &Intent::Extract {
                selection: vec![],
                dest_dir: dest.clone(),
            },
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e, Event::AskOverwrite { .. })),
            "expected an AskOverwrite event, got {events:?}"
        );
        match events.last() {
            Some(Event::Done(report)) => {
                assert_eq!(report.extracted, 1);
                assert_eq!(report.skipped, 0);
            }
            other => panic!("expected Done, got {other:?}"),
        }
        // The existing file was replaced by the archive entry (empty content:
        // the fake entry's uncompressed size is 0).
        assert_eq!(std::fs::read_to_string(dest.join("a.txt")).unwrap(), "");
        let _ = std::fs::remove_dir_all(&dest);
    }

    #[test]
    fn ask_policy_prompts_and_skips_on_no() {
        let entries = vec![meta("a.txt")];
        let registry = Registry::new().register_archive(OkFormat { entries });
        let (mut core, ask_tx) = core_with_ask(registry);
        run(
            &mut core,
            &Intent::Open {
                path: temp_archive("ask-no.fake"),
                password: None,
            },
        );

        let dest = std::env::temp_dir().join(format!("hajizip-gui-ask-no-{}", std::process::id()));
        std::fs::create_dir_all(&dest).unwrap();
        std::fs::write(dest.join("a.txt"), b"existing").unwrap();

        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(50));
            ask_tx.send(OverwriteDecision::Skip).unwrap();
        });

        let events = run(
            &mut core,
            &Intent::Extract {
                selection: vec![],
                dest_dir: dest.clone(),
            },
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e, Event::AskOverwrite { .. })),
            "expected an AskOverwrite event, got {events:?}"
        );
        match events.last() {
            Some(Event::Done(report)) => {
                assert_eq!(report.extracted, 0);
                assert_eq!(report.skipped, 1);
            }
            other => panic!("expected Done, got {other:?}"),
        }
        // Existing content preserved.
        assert_eq!(
            std::fs::read_to_string(dest.join("a.txt")).unwrap(),
            "existing"
        );
        let _ = std::fs::remove_dir_all(&dest);
    }

    #[test]
    fn ask_policy_cancel_aborts_the_run() {
        // The UI's behaviour on "Cancel" in the ask dialog: cancel the token
        // first, then release the blocked worker with a Skip. The run must
        // abort with Cancelled instead of continuing.
        let entries = vec![meta("a.txt"), meta("b.txt")];
        let registry = Registry::new().register_archive(OkFormat { entries });
        let (mut core, ask_tx) = core_with_ask(registry);
        run(
            &mut core,
            &Intent::Open {
                path: temp_archive("ask-cancel.fake"),
                password: None,
            },
        );

        let dest =
            std::env::temp_dir().join(format!("hajizip-gui-ask-cancel-{}", std::process::id()));
        std::fs::create_dir_all(&dest).unwrap();
        std::fs::write(dest.join("a.txt"), b"existing").unwrap();

        // The cancellation token is installed at extraction start; poll until
        // it appears, cancel it, then release the blocked worker.
        let cancel_slot = core.cancel.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(50));
            loop {
                let token = cancel_slot.lock().unwrap().clone();
                if let Some(t) = token {
                    t.cancel();
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
            ask_tx.send(OverwriteDecision::Skip).unwrap();
        });

        let events = run(
            &mut core,
            &Intent::Extract {
                selection: vec![],
                dest_dir: dest.clone(),
            },
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e, Event::AskOverwrite { .. })),
            "expected an AskOverwrite event, got {events:?}"
        );
        assert!(
            matches!(events.last(), Some(Event::Cancelled)),
            "expected Cancelled, got {events:?}"
        );
        let _ = std::fs::remove_dir_all(&dest);
    }

    #[test]
    fn cancel_before_extract_emits_cancelled() {
        let registry = Registry::new().register_archive(OkFormat {
            entries: vec![meta("a.txt"), meta("b.txt")],
        });
        let mut core = core_with(registry);
        run(
            &mut core,
            &Intent::Open {
                path: temp_archive("cancel.fake"),
                password: None,
            },
        );
        // Install a pre-cancelled token in the shared slot: extraction must
        // notice it before the first entry and emit Cancelled.
        let token = CancellationToken::new();
        token.cancel();
        *core.cancel.lock().unwrap() = Some(token);

        let dest = std::env::temp_dir().join(format!("hajizip-gui-cancel-{}", std::process::id()));
        std::fs::create_dir_all(&dest).unwrap();
        let events = run(
            &mut core,
            &Intent::Extract {
                selection: vec![],
                dest_dir: dest.clone(),
            },
        );
        assert!(
            matches!(events.last(), Some(Event::Cancelled)),
            "expected Cancelled, got {events:?}"
        );
        let _ = std::fs::remove_dir_all(&dest);
    }

    #[test]
    fn cancel_mid_extract_emits_cancelled() {
        let registry = Registry::new().register_archive(OkFormat {
            entries: vec![meta("a.txt"), meta("b.txt"), meta("c.txt")],
        });
        let mut core = core_with(registry);
        run(
            &mut core,
            &Intent::Open {
                path: temp_archive("midcancel.fake"),
                password: None,
            },
        );
        let slot = core.cancel.clone();
        let dest =
            std::env::temp_dir().join(format!("hajizip-gui-midcancel-{}", std::process::id()));
        std::fs::create_dir_all(&dest).unwrap();

        // Run extraction on a worker thread and cancel after the first entry.
        let events = Arc::new(StdMutex::new(Vec::new()));
        std::thread::scope(|scope| {
            let core = &mut core;
            let slot = &slot;
            let dest = &dest;
            let events = &events;
            let mut first = true;
            scope.spawn(move || {
                core.handle(
                    &Intent::Extract {
                        selection: vec![],
                        dest_dir: dest.to_path_buf(),
                    },
                    &mut |e| {
                        if let Event::Progress(_) = &e
                            && first
                        {
                            first = false;
                            slot.lock().unwrap().as_ref().unwrap().cancel();
                        }
                        events.lock().unwrap().push(e);
                    },
                );
            });
        });
        let mut guard = events.lock().unwrap();
        let last = guard.pop();
        assert!(
            matches!(last, Some(Event::Cancelled)),
            "expected Cancelled, got {last:?}"
        );
        drop(guard);
        let _ = std::fs::remove_dir_all(&dest);
    }

    #[test]
    fn extract_selection_extracts_only_selected_files() {
        // A non-empty selection extracts exactly the chosen files, not the
        // whole archive ("extract partial files").
        let entries = vec![meta("a.txt"), meta("b.txt"), meta("c.txt")];
        let registry = Registry::new().register_archive(OkFormat { entries });
        let mut core = core_with(registry);
        run(
            &mut core,
            &Intent::Open {
                path: temp_archive("partial.fake"),
                password: None,
            },
        );

        let dest = std::env::temp_dir().join(format!("hajizip-gui-partial-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dest);
        std::fs::create_dir_all(&dest).unwrap();

        let events = run(
            &mut core,
            &Intent::Extract {
                selection: vec![
                    EntryPath::new("a.txt").unwrap(),
                    EntryPath::new("c.txt").unwrap(),
                ],
                dest_dir: dest.clone(),
            },
        );
        match events.last() {
            Some(Event::Done(report)) => {
                assert_eq!(report.extracted, 2, "only a.txt + c.txt");
                assert_eq!(report.skipped, 0);
                assert!(report.failed.is_empty());
            }
            other => panic!("expected Done, got {other:?}"),
        }
        assert!(dest.join("a.txt").exists(), "a.txt extracted");
        assert!(dest.join("c.txt").exists(), "c.txt extracted");
        assert!(!dest.join("b.txt").exists(), "b.txt must NOT be extracted");
        let _ = std::fs::remove_dir_all(&dest);
    }

    #[test]
    fn extract_selection_expands_directory_subtree() {
        // Selecting a directory extracts its whole subtree (GUI semantics:
        // `is_under`), while siblings outside the selection stay behind.
        let entries = vec![meta("a.txt"), meta("dir/b.txt"), meta("dir/sub/c.txt")];
        let registry = Registry::new().register_archive(OkFormat { entries });
        let mut core = core_with(registry);
        run(
            &mut core,
            &Intent::Open {
                path: temp_archive("subtree.fake"),
                password: None,
            },
        );

        let dest = std::env::temp_dir().join(format!("hajizip-gui-subtree-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dest);
        std::fs::create_dir_all(&dest).unwrap();

        let events = run(
            &mut core,
            &Intent::Extract {
                selection: vec![EntryPath::new("dir").unwrap()],
                dest_dir: dest.clone(),
            },
        );
        match events.last() {
            Some(Event::Done(report)) => {
                assert_eq!(report.extracted, 2, "dir/b.txt + dir/sub/c.txt");
                assert!(report.failed.is_empty());
            }
            other => panic!("expected Done, got {other:?}"),
        }
        assert!(dest.join("dir/b.txt").exists());
        assert!(dest.join("dir/sub/c.txt").exists());
        assert!(
            !dest.join("a.txt").exists(),
            "a.txt is outside the selection"
        );
        let _ = std::fs::remove_dir_all(&dest);
    }

    #[test]
    fn real_zip_extract_selection_only_writes_chosen_files() {
        // Partial extraction against a real archive: selecting only a.txt must
        // not materialize dir/b.txt.
        let mut core = real_core();
        run(
            &mut core,
            &Intent::Open {
                path: fixture("zip/basic.zip"),
                password: None,
            },
        );

        let dest = temp_dir("e2e-zip-partial");
        let events = run(
            &mut core,
            &Intent::Extract {
                selection: vec![EntryPath::new("a.txt").unwrap()],
                dest_dir: dest.clone(),
            },
        );
        match events.last() {
            Some(Event::Done(report)) => {
                assert_eq!(report.extracted, 1, "only a.txt");
                assert!(report.failed.is_empty());
            }
            other => panic!("expected Done, got {other:?}"),
        }
        assert!(dest.join("a.txt").exists());
        assert!(
            !dest.join("dir/b.txt").exists(),
            "dir/b.txt is not in the selection"
        );
        let _ = std::fs::remove_dir_all(&dest);
    }

    #[test]
    fn set_encoding_updates_config() {
        let mut core = core_with(Registry::new());
        assert_eq!(core.config().filename_encoding, FilenameEncoding::Auto);
        let events = run(
            &mut core,
            &Intent::SetEncoding(FilenameEncoding::Forced(hajizip_core::Codepage::Gbk)),
        );
        assert_eq!(
            core.config().filename_encoding,
            FilenameEncoding::Forced(hajizip_core::Codepage::Gbk)
        );
        assert!(
            matches!(&events[..], [Event::ConfigChanged(_)]),
            "expected ConfigChanged, got {events:?}"
        );
    }

    #[test]
    fn set_encoding_refreshes_open_archive() {
        let last_encoding = Arc::new(StdMutex::new(None));
        let registry = Registry::new().register_archive(EncodingFormat {
            raw: vec![b"hello.txt".to_vec()],
            last_encoding: last_encoding.clone(),
        });
        let mut core = core_with(registry);
        run(
            &mut core,
            &Intent::Open {
                path: temp_archive("enc.fake"),
                password: None,
            },
        );
        assert_eq!(*last_encoding.lock().unwrap(), Some(FilenameEncoding::Auto));

        let events = run(
            &mut core,
            &Intent::SetEncoding(FilenameEncoding::Forced(hajizip_core::Codepage::Utf8)),
        );
        // ConfigChanged then a fresh Opened from the re-open.
        assert_eq!(
            *last_encoding.lock().unwrap(),
            Some(FilenameEncoding::Forced(hajizip_core::Codepage::Utf8))
        );
        assert!(
            matches!(
                &events[..],
                [
                    Event::ConfigChanged(_),
                    Event::ConfigChanged(_),
                    Event::Opened { .. }
                ]
            ),
            "expected ConfigChanged + ConfigChanged + Opened, got {events:?}"
        );
    }

    #[test]
    fn config_intents_emit_config_changed() {
        let mut core = core_with(Registry::new());
        let events = run(&mut core, &Intent::SetOverwrite(OverwritePolicy::Always));
        assert!(matches!(&events[..], [Event::ConfigChanged(_)]));
        assert_eq!(core.config().overwrite_policy, OverwritePolicy::Always);

        let events = run(&mut core, &Intent::SetPreserveMtime(false));
        assert!(matches!(&events[..], [Event::ConfigChanged(_)]));
        assert!(!core.config().preserve_mtime);
    }

    // ---- End-to-end with real formats (composed registry + fixtures) ----

    /// Repo-root `testdata/` fixture (shared with the core integration tests).
    fn fixture(rel: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../testdata")
            .join(rel)
    }

    fn real_core() -> ControllerCore {
        ControllerCore::new(
            Arc::new(crate::registry::compose_registry()),
            AppConfig::default(),
        )
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let dest = std::env::temp_dir().join(format!("hajizip-gui-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dest);
        dest
    }

    #[test]
    fn real_zip_opens_navigates_and_extracts() {
        let mut core = real_core();

        let events = run(
            &mut core,
            &Intent::Open {
                path: fixture("zip/basic.zip"),
                password: None,
            },
        );
        let entries = match &events[..] {
            [Event::ConfigChanged(_), Event::Opened { entries, .. }] => entries.clone(),
            other => panic!("expected ConfigChanged + Opened, got {other:?}"),
        };
        assert_eq!(entries.len(), 3, "a.txt + dir/ + dir/b.txt");

        // Enter the dir/ directory → Navigated with a breadcrumb.
        let events = run(
            &mut core,
            &Intent::Enter {
                path: EntryPath::new("dir").unwrap(),
            },
        );
        match &events[..] {
            [
                Event::Navigated {
                    breadcrumb, focus, ..
                },
            ] => {
                assert_eq!(focus.as_ref().unwrap().as_str(), "dir");
                assert_eq!(breadcrumb.len(), 2, "archive name + dir");
            }
            other => panic!("expected Navigated, got {other:?}"),
        }

        // Back to the root, then extract everything.
        run(&mut core, &Intent::Back);
        let dest = temp_dir("e2e-zip");
        let events = run(
            &mut core,
            &Intent::Extract {
                selection: vec![],
                dest_dir: dest.clone(),
            },
        );
        match events.last() {
            Some(Event::Done(report)) => {
                assert_eq!(report.extracted, 3, "2 files + 1 dir");
                assert_eq!(report.skipped, 0);
                assert!(report.failed.is_empty(), "failed: {:?}", report.failed);
            }
            other => panic!("expected Done, got {other:?}"),
        }
        assert_eq!(std::fs::metadata(dest.join("a.txt")).unwrap().len(), 16);
        assert_eq!(std::fs::metadata(dest.join("dir/b.txt")).unwrap().len(), 15);
        assert!(dest.join("dir").is_dir());
        let _ = std::fs::remove_dir_all(&dest);
    }

    #[test]
    fn real_tar_gz_opens_through_the_codec_chain() {
        let mut core = real_core();
        for rel in ["tar/basic.tar.gz", "tar/hello.tgz"] {
            let events = run(
                &mut core,
                &Intent::Open {
                    path: fixture(rel),
                    password: None,
                },
            );
            match &events[..] {
                [Event::ConfigChanged(_), Event::Opened { entries, .. }] => {
                    assert_eq!(entries.len(), 3, "{rel} should list a.txt/dir/dir-b.txt")
                }
                other => panic!("expected ConfigChanged + Opened for {rel}, got {other:?}"),
            }
        }
    }

    #[test]
    fn real_bare_gzip_is_rejected_gracefully() {
        let mut core = real_core();
        let events = run(
            &mut core,
            &Intent::Open {
                path: fixture("gzip/hello.txt.gz"),
                password: None,
            },
        );
        assert!(
            matches!(&events[..], [Event::Error(_)]),
            "expected a graceful Error, got {events:?}"
        );
    }

    #[test]
    fn real_encrypted_zip_lists_but_extraction_fails_gracefully() {
        let mut core = real_core();
        let events = run(
            &mut core,
            &Intent::Open {
                path: fixture("zip/enc.zip"),
                password: None,
            },
        );
        let entries = match &events[..] {
            [Event::ConfigChanged(_), Event::Opened { entries, .. }] => entries.clone(),
            other => panic!("expected ConfigChanged + Opened, got {other:?}"),
        };
        assert_eq!(entries.len(), 1);
        assert!(entries[0].encrypted);

        // M1 cannot read encrypted entries; the run must finish with the
        // failure recorded, not crash.
        let dest = temp_dir("e2e-enc");
        let events = run(
            &mut core,
            &Intent::Extract {
                selection: vec![],
                dest_dir: dest.clone(),
            },
        );
        match events.last() {
            Some(Event::Done(report)) => {
                assert_eq!(report.failed.len(), 1);
                assert_eq!(report.extracted, 0);
            }
            other => panic!("expected Done, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dest);
    }

    // ---- M2 formats end-to-end (7z archive + xz codec) ----

    #[test]
    fn real_sevenz_opens_navigates_and_extracts() {
        let mut core = real_core();
        let events = run(
            &mut core,
            &Intent::Open {
                path: fixture("7z/basic.7z"),
                password: None,
            },
        );
        let entries = match &events[..] {
            [Event::ConfigChanged(_), Event::Opened { entries, .. }] => entries.clone(),
            other => panic!("expected ConfigChanged + Opened, got {other:?}"),
        };
        assert_eq!(entries.len(), 3, "a.txt + dir/ + dir/b.txt");

        // Enter dir/ → Navigated with a breadcrumb, then back to the root.
        let events = run(
            &mut core,
            &Intent::Enter {
                path: EntryPath::new("dir").unwrap(),
            },
        );
        match &events[..] {
            [
                Event::Navigated {
                    focus, breadcrumb, ..
                },
            ] => {
                assert_eq!(focus.as_ref().unwrap().as_str(), "dir");
                assert_eq!(breadcrumb.len(), 2, "archive name + dir");
            }
            other => panic!("expected Navigated, got {other:?}"),
        }
        run(&mut core, &Intent::Back);

        // Extract everything and verify the bytes on disk.
        let dest = temp_dir("e2e-7z");
        let events = run(
            &mut core,
            &Intent::Extract {
                selection: vec![],
                dest_dir: dest.clone(),
            },
        );
        match events.last() {
            Some(Event::Done(report)) => {
                assert_eq!(report.extracted, 3, "2 files + 1 dir");
                assert!(report.failed.is_empty(), "failed: {:?}", report.failed);
            }
            other => panic!("expected Done, got {other:?}"),
        }
        assert_eq!(std::fs::metadata(dest.join("a.txt")).unwrap().len(), 16);
        assert_eq!(std::fs::metadata(dest.join("dir/b.txt")).unwrap().len(), 15);
        let _ = std::fs::remove_dir_all(&dest);
    }

    #[test]
    fn real_tar_xz_opens_through_the_codec_chain() {
        let mut core = real_core();
        let events = run(
            &mut core,
            &Intent::Open {
                path: fixture("tar/basic.tar.xz"),
                password: None,
            },
        );
        match &events[..] {
            [Event::ConfigChanged(_), Event::Opened { entries, .. }] => {
                assert_eq!(entries.len(), 3, "tar.xz should list a.txt/dir/dir-b.txt")
            }
            other => panic!("expected ConfigChanged + Opened, got {other:?}"),
        }
    }

    #[test]
    fn real_bare_xz_is_rejected_gracefully() {
        let mut core = real_core();
        let events = run(
            &mut core,
            &Intent::Open {
                path: fixture("xz/hello.txt.xz"),
                password: None,
            },
        );
        assert!(
            matches!(&events[..], [Event::Error(_)]),
            "expected a graceful Error, got {events:?}"
        );
    }

    #[test]
    fn real_encrypted_7z_password_flow() {
        let mut core = real_core();

        // enc.7z encrypts its header: opening without a password asks for one.
        let events = run(
            &mut core,
            &Intent::Open {
                path: fixture("7z/enc.7z"),
                password: None,
            },
        );
        assert!(
            matches!(&events[..], [Event::PasswordRequired { .. }]),
            "expected PasswordRequired, got {events:?}"
        );

        // The right password opens it fully…
        let events = run(
            &mut core,
            &Intent::Open {
                path: fixture("7z/enc.7z"),
                password: Some("secret".to_string()),
            },
        );
        match &events[..] {
            [Event::ConfigChanged(_), Event::Opened { entries, .. }] => {
                assert_eq!(entries.len(), 3)
            }
            other => panic!("expected ConfigChanged + Opened, got {other:?}"),
        }

        // …and extraction decodes the encrypted content.
        let dest = temp_dir("e2e-7z-enc");
        let events = run(
            &mut core,
            &Intent::Extract {
                selection: vec![],
                dest_dir: dest.clone(),
            },
        );
        match events.last() {
            Some(Event::Done(report)) => {
                assert_eq!(report.extracted, 3);
                assert!(report.failed.is_empty(), "failed: {:?}", report.failed);
            }
            other => panic!("expected Done, got {other:?}"),
        }
        assert_eq!(std::fs::metadata(dest.join("a.txt")).unwrap().len(), 16);
        let _ = std::fs::remove_dir_all(&dest);
    }

    // ---- RAR content encryption (plain header, WinRAR default) ----

    #[test]
    fn real_content_encrypted_rar_prompts_for_password_on_open() {
        // rar5-enc.rar encrypts only the member data (header is plain, the
        // WinRAR default): it opens and lists without a password, but the
        // GUI must still prompt so preview/extraction don't fail per read.
        let mut core = real_core();
        let events = run(
            &mut core,
            &Intent::Open {
                path: fixture("rar/rar5-enc.rar"),
                password: None,
            },
        );
        assert!(
            matches!(
                &events[..],
                [
                    Event::ConfigChanged(_),
                    Event::Opened { .. },
                    Event::PasswordRequired { .. }
                ]
            ),
            "expected ConfigChanged + Opened + PasswordRequired, got {events:?}"
        );
    }

    #[test]
    fn real_content_encrypted_rar_preview_prompts_then_retries_with_password() {
        let mut core = real_core();
        // Open without a password: the archive lists fine (plain header).
        run(
            &mut core,
            &Intent::Open {
                path: fixture("rar/rar5-enc.rar"),
                password: None,
            },
        );
        // Preview without a password → PasswordRequired, not a dead-end
        // Error, and no temp file is left behind.
        let events = run(
            &mut core,
            &Intent::Preview {
                path: EntryPath::new("text.txt").unwrap(),
            },
        );
        assert!(
            matches!(&events[..], [Event::PasswordRequired { .. }]),
            "expected PasswordRequired, got {events:?}"
        );
        let fail_dir = std::env::temp_dir().join(format!("hajizip-preview-{}", std::process::id()));
        assert!(
            !fail_dir.exists(),
            "failed preview must not leave temp files"
        );

        // Submitting the password re-opens the archive and automatically
        // retries the pending preview.
        let events = run(
            &mut core,
            &Intent::Open {
                path: fixture("rar/rar5-enc.rar"),
                password: Some("test".to_string()),
            },
        );
        match &events[..] {
            [
                Event::ConfigChanged(_),
                Event::Opened { .. },
                Event::PreviewReady { temp_path },
            ] => {
                assert_eq!(std::fs::metadata(temp_path).unwrap().len(), 2118);
                let _ = std::fs::remove_dir_all(temp_path.parent().unwrap());
            }
            other => panic!("expected ConfigChanged + Opened + PreviewReady, got {other:?}"),
        }
    }

    #[test]
    fn real_content_encrypted_rar_wrong_password_keeps_pending_preview() {
        let mut core = real_core();
        run(
            &mut core,
            &Intent::Open {
                path: fixture("rar/rar5-enc.rar"),
                password: None,
            },
        );
        // Preview triggers a password prompt…
        run(
            &mut core,
            &Intent::Preview {
                path: EntryPath::new("text.txt").unwrap(),
            },
        );
        // …a wrong password is rejected (dialog stays up)…
        let events = run(
            &mut core,
            &Intent::Open {
                path: fixture("rar/rar5-enc.rar"),
                password: Some("nope".to_string()),
            },
        );
        assert!(
            matches!(&events[..], [Event::WrongPassword { .. }]),
            "expected WrongPassword, got {events:?}"
        );
        // …and the right one afterwards retries the pending preview.
        let events = run(
            &mut core,
            &Intent::Open {
                path: fixture("rar/rar5-enc.rar"),
                password: Some("test".to_string()),
            },
        );
        match &events[..] {
            [
                Event::ConfigChanged(_),
                Event::Opened { .. },
                Event::PreviewReady { temp_path },
            ] => {
                let _ = std::fs::remove_dir_all(temp_path.parent().unwrap());
            }
            other => panic!("expected ConfigChanged + Opened + PreviewReady, got {other:?}"),
        }
    }

    #[test]
    fn real_content_encrypted_rar_extract_prompts_then_retries_with_password() {
        let mut core = real_core();
        // Open without a password (the archive lists fine; the open prompt
        // is dismissed by the test).
        run(
            &mut core,
            &Intent::Open {
                path: fixture("rar/rar5-enc.rar"),
                password: None,
            },
        );
        // Extracting encrypted entries without a password defers the run and
        // asks for a password instead of failing per entry.
        let dest = temp_dir("e2e-rar-enc-extract");
        let events = run(
            &mut core,
            &Intent::Extract {
                selection: vec![],
                dest_dir: dest.clone(),
            },
        );
        assert!(
            matches!(&events[..], [Event::PasswordRequired { .. }]),
            "expected PasswordRequired, got {events:?}"
        );
        assert!(
            !dest.join("text.txt").exists(),
            "deferred run must not extract"
        );

        // Submitting the password re-opens the archive and retries the run.
        let events = run(
            &mut core,
            &Intent::Open {
                path: fixture("rar/rar5-enc.rar"),
                password: Some("test".to_string()),
            },
        );
        match events.last() {
            Some(Event::Done(report)) => {
                assert_eq!(report.extracted, 2);
                assert!(report.failed.is_empty(), "failed: {:?}", report.failed);
            }
            other => panic!("expected Done, got {other:?}"),
        }
        assert_eq!(
            std::fs::metadata(dest.join("text.txt")).unwrap().len(),
            2118
        );
        let _ = std::fs::remove_dir_all(&dest);
    }

    #[test]
    fn real_nested_encrypted_rar_preview_retries_through_chain() {
        let mut core = real_core();
        // Open the outer (plain) zip and enter the nested content-encrypted
        // rar; previewing a member without a password must prompt and
        // remember the nested chain.
        run(
            &mut core,
            &Intent::Open {
                path: fixture("zip/nested-enc-rar.zip"),
                password: None,
            },
        );
        run(
            &mut core,
            &Intent::Enter {
                path: EntryPath::new("rar5-enc.rar").unwrap(),
            },
        );
        let events = run(
            &mut core,
            &Intent::Preview {
                path: EntryPath::new("text.txt").unwrap(),
            },
        );
        assert!(
            matches!(&events[..], [Event::PasswordRequired { .. }]),
            "expected PasswordRequired, got {events:?}"
        );

        // Opening the OUTER archive with the password re-enters the chain
        // (Navigated) and retries the preview automatically.
        let events = run(
            &mut core,
            &Intent::Open {
                path: fixture("zip/nested-enc-rar.zip"),
                password: Some("test".to_string()),
            },
        );
        match &events[..] {
            [
                Event::ConfigChanged(_),
                Event::Opened { .. },
                Event::Navigated { .. },
                Event::PreviewReady { temp_path },
            ] => {
                assert_eq!(std::fs::metadata(temp_path).unwrap().len(), 2118);
                let _ = std::fs::remove_dir_all(temp_path.parent().unwrap());
            }
            other => {
                panic!("expected ConfigChanged + Opened + Navigated + PreviewReady, got {other:?}")
            }
        }
    }
}
