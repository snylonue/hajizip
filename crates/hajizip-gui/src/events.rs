//! Controller event plumbing: folding controller events into view state.
//!
//! The root component stays thin (see `app.rs`); this module owns everything
//! that happens once the background controller is created: the ask-overwrite
//! dialog worker thread and the reactive event loop that translates
//! [`Event`]s into signal updates. It also defines [`ViewState`], the bundle
//! of view signals the component and the event loop share.

use std::collections::HashSet;
use std::path::PathBuf;

use dioxus::desktop::DesktopContext;
use dioxus::prelude::*;
use hajizip_core::{EntryMeta, EntryPath, OverwriteDecision};

use crate::config::AppConfig;
use crate::controller::{BreadcrumbSegment, ControllerHandle, Event, ProgressUpdate};

/// All view signals owned by the root component, bundled so the event loop
/// can fold controller events into them without a dozen parameters.
#[derive(Clone, Copy)]
pub struct ViewState {
    /// Transient status-bar feedback.
    pub status: Signal<String>,
    /// Display name of the archive (shown in the window title).
    pub archive_name: Signal<String>,
    /// Breadcrumb trail for the current navigation view.
    pub breadcrumb: Signal<Vec<BreadcrumbSegment>>,
    /// Directory currently in focus (the file list shows its children).
    pub focus: Signal<Option<EntryPath>>,
    /// Flat listing of the current archive's entries.
    pub entries: Signal<Vec<EntryMeta>>,
    /// Currently selected entry paths (multi-select).
    pub selection: Signal<HashSet<EntryPath>>,
    /// Directories expanded in the left tree panel.
    pub expanded: Signal<HashSet<EntryPath>>,
    /// Whether an archive is open (drives the browser / empty-state switch).
    pub has_archive: Signal<bool>,
    /// Live file-list search query.
    pub search_query: Signal<String>,
    /// Whether a drag & drop is hovering the window (empty-state highlight).
    pub drag_over: Signal<bool>,
    /// Whether the garbled-name encoding banner was dismissed.
    pub banner_dismissed: Signal<bool>,
    /// Password prompt dialog state.
    pub password_prompt: Signal<Option<PasswordPrompt>>,
    /// Extraction progress popup state.
    pub progress: Signal<Option<ProgressUpdate>>,
    /// Settings modal open state.
    pub settings_open: Signal<bool>,
    /// About modal open state.
    pub about_open: Signal<bool>,
    /// Current application config.
    pub config: Signal<AppConfig>,
}

impl ViewState {
    /// Create fresh view state. Must be called from a component body: the
    /// signals are Dioxus hooks and need a reactive scope.
    pub fn new() -> Self {
        Self {
            status: use_signal(|| "No archive open.".to_string()),
            archive_name: use_signal(String::new),
            breadcrumb: use_signal(Vec::<BreadcrumbSegment>::new),
            focus: use_signal(|| Option::<EntryPath>::None),
            entries: use_signal(Vec::<EntryMeta>::new),
            selection: use_signal(HashSet::<EntryPath>::new),
            expanded: use_signal(HashSet::<EntryPath>::new),
            has_archive: use_signal(|| false),
            search_query: use_signal(String::new),
            drag_over: use_signal(|| false),
            banner_dismissed: use_signal(|| false),
            password_prompt: use_signal(|| Option::<PasswordPrompt>::None),
            progress: use_signal(|| Option::<ProgressUpdate>::None),
            settings_open: use_signal(|| false),
            about_open: use_signal(|| false),
            config: use_signal(AppConfig::load),
        }
    }
}

/// State backing the password prompt dialog.
#[derive(Clone)]
pub struct PasswordPrompt {
    /// Archive path the password unlocks.
    pub path: PathBuf,
    /// Error to display (e.g. wrong password), if any.
    pub error: Option<String>,
}

/// Spawn the background controller's event loop and its dialog worker.
///
/// Two threads are involved, mirroring the original inline wiring:
///
/// * the ask-overwrite dialog worker — `rfd` blocks its calling thread, so
///   overwrite conflicts are forwarded over an mpsc queue and answered in
///   order by a dedicated thread. The controller blocks inside core's
///   `on_ask_overwrite` until the answer arrives, so conflicts are strictly
///   serialized and the UI thread never blocks;
/// * the event-draining future — runs on the reactive thread and folds each
///   controller [`Event`] into [`ViewState`] via [`apply_event`].
pub fn spawn_event_loop(
    handle: &ControllerHandle,
    mut events: tokio::sync::mpsc::UnboundedReceiver<Event>,
    window: DesktopContext,
    state: ViewState,
) {
    let (ask_evt_tx, ask_evt_rx) = std::sync::mpsc::channel::<(EntryPath, PathBuf)>();
    let dialog_handle = handle.clone();
    std::thread::spawn(move || {
        while let Ok((_path, dest)) = ask_evt_rx.recv() {
            let result = rfd::MessageDialog::new()
                .set_title("Overwrite existing file?")
                .set_description(format!(
                    "{} already exists at the destination.\n\nOverwrite it?",
                    dest.display()
                ))
                .set_buttons(rfd::MessageButtons::YesNoCancel)
                .show();
            let decision = match result {
                rfd::MessageDialogResult::Yes => OverwriteDecision::Overwrite,
                rfd::MessageDialogResult::No => OverwriteDecision::Skip,
                // Cancel anywhere in the batch aborts the whole run.
                _ => {
                    dialog_handle.cancel();
                    OverwriteDecision::Skip
                }
            };
            // The worker is blocked waiting for exactly this decision.
            let _ = dialog_handle.decisions.send(decision);
        }
    });

    // Draining runs on the reactive thread, so mutating the signals here is
    // safe. If the controller handle is dropped, the sender closes and the
    // loop ends with the component.
    spawn(async move {
        while let Some(event) = events.recv().await {
            apply_event(event, state, &window, &ask_evt_tx);
        }
    });
}

/// Fold one controller event into view state.
fn apply_event(
    event: Event,
    mut state: ViewState,
    window: &DesktopContext,
    ask_evt_tx: &std::sync::mpsc::Sender<(EntryPath, PathBuf)>,
) {
    match event {
        Event::Opened {
            name,
            entries: list,
        } => {
            // The archive name lives in the window title; the in-window
            // chrome shows only the breadcrumb trail.
            window.set_title(&format!("{name} — hajizip"));
            state.archive_name.set(name);
            state.entries.set(list);
            state.breadcrumb.set(Vec::new());
            state.focus.set(None);
            state.selection.set(HashSet::new());
            state.expanded.set(HashSet::new());
            state.has_archive.set(true);
            state.password_prompt.set(None);
            state.search_query.set(String::new());
            state.banner_dismissed.set(false);
            state.status.set("Archive opened.".to_string());
        }
        Event::Navigated {
            breadcrumb: crumbs,
            focus: f,
            entries: list,
        } => {
            state.breadcrumb.set(crumbs);
            state.focus.set(f);
            state.entries.set(list);
            state.status.set(String::new());
        }
        Event::PasswordRequired { path } => {
            state
                .password_prompt
                .set(Some(PasswordPrompt { path, error: None }));
        }
        Event::WrongPassword { path } => {
            state.password_prompt.set(Some(PasswordPrompt {
                path,
                error: Some("Wrong password, try again.".to_string()),
            }));
        }
        Event::Progress(update) => {
            state.progress.set(Some(update));
        }
        Event::AskOverwrite { path, dest } => {
            // Forward to the dialog worker; it answers in order.
            let _ = ask_evt_tx.send((path, dest));
        }
        Event::PreviewReady { temp_path } => {
            // Open the extracted temp file with the system default app
            // (fire-and-forget; the platform helper never blocks the UI).
            match crate::platform::open_with_default_app(&temp_path) {
                Ok(()) => state
                    .status
                    .set(format!("Opened preview: {}", temp_path.display())),
                Err(e) => {
                    state.status.set(format!("Error opening preview: {e:#}"));
                }
            }
        }
        Event::Done(report) => {
            state.progress.set(None);
            state.status.set(format!(
                "Extracted {} entries ({} bytes), skipped {}, failed {}.",
                report.extracted,
                report.total_bytes,
                report.skipped,
                report.failed.len()
            ));
        }
        Event::Cancelled => {
            state.progress.set(None);
            state.status.set("Operation cancelled.".to_string());
        }
        Event::Unsupported(message) => {
            state.status.set(format!("Not supported yet: {message}"));
        }
        Event::Error(message) => {
            state.status.set(format!("Error: {message}"));
        }
        Event::ConfigChanged(cfg) => {
            state.config.set(cfg.clone());
            let _ = cfg.save();
        }
    }
}
