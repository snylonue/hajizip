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
    ExtractOptions, ExtractReport, FilenameEncoding, NodeKind, OpenOptions, OverwritePolicy,
    ProgressSink, Registry, Secret, Source,
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
    /// The archive is encrypted and needs a password to open.
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
    /// Directory currently in focus within this archive (None = root).
    focus: Option<EntryPath>,
    /// Cached flat entry listing (avoids re-listing on every navigation).
    entries: Arc<Vec<EntryMeta>>,
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
    /// Last successfully opened source, to re-open on encoding changes.
    last_open: Option<(PathBuf, Option<String>)>,
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
    fn with_cancel_slot(registry: Arc<Registry>, config: AppConfig, cancel: CancelSlot) -> Self {
        Self {
            registry,
            config,
            stack: Vec::new(),
            cancel,
            last_open: None,
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
        match archive.entries() {
            Ok(entries) => {
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.display().to_string());
                self.stack = vec![NavFrame {
                    archive,
                    name: name.clone(),
                    focus: None,
                    entries: Arc::new(entries.clone()),
                }];
                self.last_open = Some((path.to_path_buf(), password.map(str::to_owned)));
                emit(Event::Opened { name, entries });
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
                .any(|e| e.path.as_str().starts_with(&format!("{}/", path.as_str()))) =>
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
                let parent = focus
                    .as_str()
                    .rsplit_once('/')
                    .map(|(p, _)| EntryPath::new(p).ok());
                let frame = self.stack.last_mut().unwrap();
                frame.focus = parent.flatten();
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
                    out.push(BreadcrumbSegment {
                        label: component.to_string(),
                        frame: i,
                        focus: Some(EntryPath::new(&prefix).expect("prefix is valid")),
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

    /// Run an extraction by delegating to core's [`ExtractEngine`], bridging
    /// progress and cancellation to the UI surface.
    ///
    /// Selection semantics differ from core's exact-match list: the GUI treats
    /// selecting a directory as selecting its whole subtree (`is_under`), so
    /// the selection is expanded to exact entry paths first.
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
        };

        let result = ExtractEngine::run(archive.as_ref(), &exact, &opts, &mut bridge, &token);
        *self.cancel.lock().unwrap() = None;
        match result {
            Ok(report) => emit(Event::Done(report)),
            Err(CoreError::Cancelled) => emit(Event::Cancelled),
            Err(e) => emit(Event::Error(e.to_string())),
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
}

/// Whether `entry` is `selection` itself or a descendant of a selected dir.
fn is_under(entry: &EntryMeta, selection: &EntryPath) -> bool {
    entry.path.as_str() == selection.as_str()
        || entry
            .path
            .as_str()
            .starts_with(&format!("{}/", selection.as_str()))
}

/// A handle used by the UI to submit intents and cancel in-flight work.
#[derive(Clone)]
pub struct ControllerHandle {
    /// Send an intent to the worker thread. Never blocks.
    pub commands: tokio::sync::mpsc::UnboundedSender<Intent>,
    /// Shared cancellation slot; cancelling never waits for the worker.
    cancel: CancelSlot,
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
    let cancel: CancelSlot = Arc::new(Mutex::new(None));
    let cancel_for_core = cancel.clone();

    std::thread::spawn(move || {
        let mut core = ControllerCore::with_cancel_slot(registry, config, cancel_for_core);
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
            [Event::Opened { name, entries }] => {
                assert_eq!(name, &path.file_name().unwrap().to_string_lossy());
                assert_eq!(entries.len(), 2);
            }
            other => panic!("expected Opened, got {other:?}"),
        }
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
        // Default policy is Ask → skip existing files.
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
            matches!(&events[..], [Event::ConfigChanged(_), Event::Opened { .. }]),
            "expected ConfigChanged + Opened, got {events:?}"
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
            [Event::Opened { entries, .. }] => entries.clone(),
            other => panic!("expected Opened, got {other:?}"),
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
                [Event::Opened { entries, .. }] => {
                    assert_eq!(entries.len(), 3, "{rel} should list a.txt/dir/dir-b.txt")
                }
                other => panic!("expected Opened for {rel}, got {other:?}"),
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
            [Event::Opened { entries, .. }] => entries.clone(),
            other => panic!("expected Opened, got {other:?}"),
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
            [Event::Opened { entries, .. }] => entries.clone(),
            other => panic!("expected Opened, got {other:?}"),
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
            [Event::Opened { entries, .. }] => {
                assert_eq!(entries.len(), 3, "tar.xz should list a.txt/dir/dir-b.txt")
            }
            other => panic!("expected Opened, got {other:?}"),
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
            [Event::Opened { entries, .. }] => assert_eq!(entries.len(), 3),
            other => panic!("expected Opened, got {other:?}"),
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
}
