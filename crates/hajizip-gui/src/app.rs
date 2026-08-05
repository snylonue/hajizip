//! The Dioxus root component and view state.
//!
//! The component is intentionally thin: it owns a few signals for display,
//! wires them to the background [`Controller`](crate::controller) and renders
//! what it is told. All archive logic lives in the controller, which is tested
//! without any UI (see `test-plan.md` §11).
//!
//! Open / extract dialogs use `rfd` (research: `local-doc/research-rfd.md`).
//! rfd's synchronous API blocks its calling thread, so dialogs are spawned on
//! worker threads and the picked path is forwarded through the controller
//! command channel — the UI thread never blocks.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use dioxus::html::HasFileData;
use dioxus::prelude::*;
use hajizip_core::{EntryMeta, EntryPath, FilenameEncoding, OverwriteDecision, OverwritePolicy};

use crate::config::AppConfig;
use crate::controller::{
    BreadcrumbSegment, ControllerHandle, Event, Intent, ProgressUpdate, spawn_controller,
};
use crate::icons::{Icon, IconView};
use crate::registry::compose_registry;
use crate::ui::{Breadcrumb, CSS, FileList, PasswordDialog, SettingsPanel, TreeView};
use crate::viewmodel;

/// The application root component.
#[component]
pub fn App() -> Element {
    // View state (updated on the reactive thread only).
    let mut status = use_signal(|| "No archive open.".to_string());
    let mut archive_name = use_signal(String::new);
    let mut breadcrumb = use_signal(Vec::<BreadcrumbSegment>::new);
    let mut focus = use_signal(|| Option::<EntryPath>::None);
    let mut entries = use_signal(Vec::<EntryMeta>::new);
    let mut selection = use_signal(HashSet::<EntryPath>::new);
    let mut expanded = use_signal(HashSet::<EntryPath>::new);
    let mut has_archive = use_signal(|| false);
    let mut search_query = use_signal(String::new);
    let mut drag_over = use_signal(|| false);

    // Dialog state.
    let mut password_prompt = use_signal(|| Option::<PasswordPrompt>::None);
    let mut progress = use_signal(|| Option::<ProgressUpdate>::None);
    let mut settings_open = use_signal(|| false);
    let mut config = use_signal(AppConfig::load);

    // Create the background controller once and start draining its events.
    let handle = use_hook(|| -> ControllerHandle {
        let registry = Arc::new(compose_registry());
        let (handle, mut events) = spawn_controller(registry, AppConfig::load());
        // Window handle for the dynamic title (archive name — hajizip).
        let window = dioxus::desktop::use_window();

        // Ask-overwrite dialog worker: conflicts are forwarded over an mpsc
        // queue and answered in order by a dedicated thread (rfd blocks its
        // thread, the UI thread never blocks). The controller blocks inside
        // core's `on_ask_overwrite` until the answer arrives, so conflicts
        // are strictly serialized.
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

        // Fold controller events into view state. This future runs on the
        // reactive thread, so mutating signals here is safe.
        spawn(async move {
            while let Some(event) = events.recv().await {
                match event {
                    Event::Opened {
                        name,
                        entries: list,
                    } => {
                        // The archive name lives in the window title; the
                        // in-window chrome shows only the breadcrumb trail.
                        window.set_title(&format!("{name} — hajizip"));
                        archive_name.set(name);
                        entries.set(list);
                        breadcrumb.set(Vec::new());
                        focus.set(None);
                        selection.set(HashSet::new());
                        expanded.set(HashSet::new());
                        has_archive.set(true);
                        password_prompt.set(None);
                        search_query.set(String::new());
                        status.set("Archive opened.".to_string());
                    }
                    Event::Navigated {
                        breadcrumb: crumbs,
                        focus: f,
                        entries: list,
                    } => {
                        breadcrumb.set(crumbs);
                        focus.set(f);
                        entries.set(list);
                        status.set(String::new());
                    }
                    Event::PasswordRequired { path } => {
                        password_prompt.set(Some(PasswordPrompt { path, error: None }));
                    }
                    Event::WrongPassword { path } => {
                        password_prompt.set(Some(PasswordPrompt {
                            path,
                            error: Some("Wrong password, try again.".to_string()),
                        }));
                    }
                    Event::Progress(update) => {
                        progress.set(Some(update));
                    }
                    Event::AskOverwrite { path, dest } => {
                        // Forward to the dialog worker; it answers in order.
                        let _ = ask_evt_tx.send((path, dest));
                    }
                    Event::PreviewReady { temp_path } => {
                        // Open the extracted temp file with the system default
                        // app (fire-and-forget; the platform helper never
                        // blocks the UI thread).
                        match crate::platform::open_with_default_app(&temp_path) {
                            Ok(()) => {
                                status.set(format!("Opened preview: {}", temp_path.display()))
                            }
                            Err(e) => {
                                status.set(format!("Error opening preview: {e:#}"));
                            }
                        }
                    }
                    Event::Done(report) => {
                        progress.set(None);
                        status.set(format!(
                            "Extracted {} entries ({} bytes), skipped {}, failed {}.",
                            report.extracted,
                            report.total_bytes,
                            report.skipped,
                            report.failed.len()
                        ));
                    }
                    Event::Cancelled => {
                        progress.set(None);
                        status.set("Operation cancelled.".to_string());
                    }
                    Event::Unsupported(message) => {
                        status.set(format!("Not supported yet: {message}"));
                    }
                    Event::Error(message) => {
                        status.set(format!("Error: {message}"));
                    }
                    Event::ConfigChanged(cfg) => {
                        config.set(cfg.clone());
                        let _ = cfg.save();
                    }
                }
            }
        });

        handle
    });

    // -- Open interaction -----------------------------------------------------

    let open_dialog = {
        let handle = handle.clone();
        move |_| {
            let handle = handle.clone();
            // rfd blocks its thread; never block the UI thread.
            std::thread::spawn(move || {
                if let Some(path) = rfd::FileDialog::new().set_title("Open archive").pick_file() {
                    let _ = handle.commands.send(Intent::Open {
                        path,
                        password: None,
                    });
                }
            });
        }
    };

    let open_path = {
        let handle = handle.clone();
        move |path: PathBuf| {
            let _ = handle.commands.send(Intent::Open {
                path,
                password: None,
            });
        }
    };

    // -- Extract interaction --------------------------------------------------

    let extract_all = {
        let handle = handle.clone();
        move |_| {
            let handle = handle.clone();
            let selection: Vec<EntryPath> = selection.read().iter().cloned().collect();
            std::thread::spawn(move || {
                if let Some(dir) = rfd::FileDialog::new()
                    .set_title("Extract to…")
                    .pick_folder()
                {
                    let _ = handle.commands.send(Intent::Extract {
                        selection,
                        dest_dir: dir,
                    });
                }
            });
        }
    };

    let cancel_extract = {
        let handle = handle.clone();
        move |_| handle.cancel()
    };

    // -- Navigation -----------------------------------------------------------

    // Two clones: TreeView and FileList each consume an `EventHandler` prop.
    let enter = {
        let handle = handle.clone();
        move |path: EntryPath| {
            let _ = handle.commands.send(Intent::Enter { path });
        }
    };
    let enter_list = {
        let handle = handle.clone();
        move |path: EntryPath| {
            let _ = handle.commands.send(Intent::Enter { path });
        }
    };

    // Preview a file entry: extract it to temp and open it externally. The
    // FileList dispatches: dirs/archives → Enter, plain files → Preview.
    let preview = {
        let handle = handle.clone();
        move |path: EntryPath| {
            let _ = handle.commands.send(Intent::Preview { path });
        }
    };

    let jump = {
        let handle = handle.clone();
        move |depth: usize| {
            let _ = handle.commands.send(Intent::JumpTo { depth });
        }
    };

    let back = {
        let handle = handle.clone();
        move |_| {
            let _ = handle.commands.send(Intent::Back);
        }
    };

    // -- Settings -------------------------------------------------------------

    let set_encoding = {
        let handle = handle.clone();
        move |enc: FilenameEncoding| {
            let _ = handle.commands.send(Intent::SetEncoding(enc));
        }
    };
    let set_overwrite = {
        let handle = handle.clone();
        move |policy: OverwritePolicy| {
            let _ = handle.commands.send(Intent::SetOverwrite(policy));
        }
    };
    let set_mtime = {
        let handle = handle.clone();
        move |value: bool| {
            let _ = handle.commands.send(Intent::SetPreserveMtime(value));
        }
    };

    let restore_defaults = {
        let handle = handle.clone();
        move |_| {
            let _ = handle
                .commands
                .send(Intent::SetEncoding(FilenameEncoding::Auto));
            let _ = handle
                .commands
                .send(Intent::SetOverwrite(OverwritePolicy::default()));
            let _ = handle.commands.send(Intent::SetPreserveMtime(true));
        }
    };

    // Drop the archive name from breadcrumb display in the header.
    let has_archive_value = *has_archive.read();
    let status_text = status.read().clone();
    let status_has_text = !status_text.is_empty();

    // Flat archives (no directories at the root) get the whole left sidebar
    // hidden; the space returns to the file list (§4.3 of review-ui-v2).
    // Implied dirs count too, so this uses the same child_entries view the
    // tree renders.
    let has_sidebar = viewmodel::children_of(&entries.read(), None)
        .iter()
        .any(|e| e.kind == hajizip_core::NodeKind::Dir);

    // Status bar, left segment: whole-archive summary. The compressed total
    // is shown only when the format records it (most do).
    let (item_count, total_size, total_compressed) = {
        let list = entries.read();
        viewmodel::archive_summary(&list)
    };
    let summary_text = if has_archive_value {
        if total_compressed > 0 {
            format!(
                "{item_count} items · {} (compressed {})",
                viewmodel::format_bytes(total_size),
                viewmodel::format_bytes(total_compressed)
            )
        } else {
            format!(
                "{item_count} items · {}",
                viewmodel::format_bytes(total_size)
            )
        }
    } else {
        String::new()
    };

    // Status bar, middle segment: selected entries summary.
    let selection_text = {
        let list = entries.read();
        let (sel_count, sel_total) = viewmodel::selection_summary(&list, &selection.read());
        if sel_count > 0 {
            format!(
                "{} selected · {}",
                sel_count,
                viewmodel::format_bytes(sel_total)
            )
        } else {
            String::new()
        }
    };

    // The Extract button doubles as "extract partial files": with no selection
    // it extracts the whole archive; with a selection it extracts only the
    // chosen entries (directories select their whole subtree). The label
    // reflects the live selection so the behavior is never ambiguous.
    let selection_count = selection.read().len();
    let extract_label = if selection_count == 0 {
        "Extract all".to_string()
    } else {
        format!("Extract ({selection_count} selected)")
    };

    rsx! {
        // Global stylesheet (injected once into the WebView document head).
        style { {CSS} }

        div {
            class: "app",
            ondragover: move |evt| {
                evt.prevent_default();
                drag_over.set(true);
            },
            ondragleave: move |_| drag_over.set(false),
            ondrop: move |evt| {
                evt.prevent_default();
                drag_over.set(false);
                if let Some(file) = evt.files().first() {
                    let path = file.path();
                    if !path.as_os_str().is_empty() {
                        open_path(path);
                    }
                }
            },

            // ── Header ─────────────────────────────────────────────────
            header { class: "app-header",
                div { class: "search-box",
                    span { class: "search-icon",
                        IconView { icon: Icon::Search, size: 14 }
                    }
                    input {
                        class: "search-input",
                        placeholder: "Search files…",
                        value: "{search_query}",
                        oninput: move |e| search_query.set(e.value()),
                        onkeydown: move |e| {
                            if e.key() == Key::Escape {
                                search_query.set(String::new());
                            }
                        },
                    }
                    if !search_query.read().is_empty() {
                        button {
                            class: "search-clear",
                            title: "Clear search",
                            onclick: move |_| search_query.set(String::new()),
                            IconView { icon: Icon::X, size: 12 }
                        }
                    }
                }
                div { class: "header-spacer" }
                div { class: "header-actions",                    button { class: "btn btn-primary", onclick: open_dialog,
                        IconView { icon: Icon::FolderOpen, size: 16 }
                        "Open"
                    }
                    button {
                        class: "btn",
                        onclick: extract_all,
                        disabled: !has_archive_value,
                        title: "Extract the selected entries, or the whole archive if nothing is selected",
                        IconView { icon: Icon::Download, size: 16 }
                        "{extract_label}"
                    }
                    button {
                        class: "btn btn-icon",
                        onclick: move |_| settings_open.set(true),
                        title: "Settings",
                        IconView { icon: Icon::Settings, size: 16 }
                    }
                }
                div { class: "header-spacer" }
            }

            // ── Main content ────────────────────────────────────────────
            if has_archive_value {
                // Navigation row: back button + breadcrumb trail in one
                // line (the first crumb is the archive name; clicking it
                // returns to the archive root).
                div { class: "navrow",
                    button { class: "btn btn-icon", onclick: back, title: "Go up one level",
                        IconView { icon: Icon::CornerUpLeft, size: 16 }
                    }
                    Breadcrumb {
                        segments: breadcrumb.read().clone(),
                        on_jump: jump,
                    }
                }

                div { class: "browser",
                    if has_sidebar {
                        TreeView {
                            entries: entries,
                            expanded: expanded,
                            on_navigate: enter,
                        }
                    }
                    FileList {
                        entries: entries,
                        focus: focus,
                        selected: selection,
                        query: search_query.read().clone(),
                        on_open: enter_list,
                        on_preview: preview,
                    }
                }
            } else {
                div {
                    class: if *drag_over.read() {
                        "empty-state empty-state-dragover"
                    } else {
                        "empty-state"
                    },
                    div { class: "empty-icon",
                        IconView { icon: Icon::Package, size: 56 }
                    }
                    div { class: "empty-title", "Drop an archive to open it" }
                    div { class: "empty-hint",
                        "…or click "
                        strong { "Open" }
                        " to browse for a file."
                    }
                    if !config.read().recent_files.is_empty() {
                        div { class: "recent-files",
                            div { class: "recent-title", "Recent files" }
                            for path in config.read().recent_files.clone() {
                                {
                                    let open_recent = open_path.clone();
                                    rsx! {
                                        button {
                                            class: "btn btn-sm recent-file",
                                            onclick: move |_| open_recent(path.clone()),
                                            "{path.display()}"
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }

            // ── Status bar ──────────────────────────────────────────────
            footer {
                class: if status_has_text { "statusbar statusbar-has-text" } else { "statusbar" },
                // Left: archive summary (whole current archive).
                if has_archive_value {
                    span { class: "status-summary", "{summary_text}" }
                }
                // Middle: transient feedback, then the selection summary.
                span {
                    class: "status-message",
                    if !status_text.is_empty() {
                        "{status_text}"
                    } else if !selection_text.is_empty() {
                        "{selection_text}"
                    }
                }
                // Right: inline extraction progress + cancel.
                if let Some(update) = progress.read().clone() {
                    span { class: "status-progress",
                        span { class: "status-progress-bar",
                            span {
                                class: "status-progress-fill",
                                style: "width: {viewmodel::progress_percent(&update)}%;",
                            }
                        }
                        span { class: "status-progress-label",
                            "{update.entries_done} items · {viewmodel::progress_percent(&update)}%"
                        }
                        button {
                            class: "btn btn-ghost btn-xs",
                            onclick: cancel_extract,
                            "Cancel"
                        }
                    }
                }
            }

            // ── Modal dialogs ───────────────────────────────────────────
            if let Some(prompt) = password_prompt.read().clone() {
                PasswordDialog {
                    path: prompt.path.display().to_string(),
                    error: prompt.error,
                    on_submit: move |password| {
                        let path = prompt.path.clone();
                        let _ = handle.commands.send(Intent::Open {
                            path,
                            password: Some(password),
                        });
                    },
                    on_cancel: move |_| password_prompt.set(None),
                }
            }

            if *settings_open.read() {
                SettingsPanel {
                    config: config,
                    on_encoding: set_encoding,
                    on_overwrite: set_overwrite,
                    on_mtime: set_mtime,
                    on_defaults: restore_defaults,
                    on_close: move |_| settings_open.set(false),
                }
            }
        }
    }
}

/// State backing the password prompt dialog.
#[derive(Clone)]
struct PasswordPrompt {
    /// Archive path the password unlocks.
    path: PathBuf,
    /// Error to display (e.g. wrong password), if any.
    error: Option<String>,
}
