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
use hajizip_core::{EntryMeta, EntryPath, FilenameEncoding, OverwritePolicy};

use crate::config::AppConfig;
use crate::controller::{
    BreadcrumbSegment, ControllerHandle, Event, Intent, ProgressUpdate, spawn_controller,
};
use crate::registry::compose_registry;
use crate::ui::{
    Breadcrumb, CSS, FileList, PasswordDialog, ProgressDialog, SettingsPanel, TreeView,
};

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

    // Dialog state.
    let mut password_prompt = use_signal(|| Option::<PasswordPrompt>::None);
    let mut progress = use_signal(|| Option::<ProgressUpdate>::None);
    let mut settings_open = use_signal(|| false);
    let mut config = use_signal(AppConfig::load);

    // Create the background controller once and start draining its events.
    let handle = use_hook(|| -> ControllerHandle {
        let registry = Arc::new(compose_registry());
        let (handle, mut events) = spawn_controller(registry, AppConfig::load());

        // Fold controller events into view state. This future runs on the
        // reactive thread, so mutating signals here is safe.
        spawn(async move {
            while let Some(event) = events.recv().await {
                match event {
                    Event::Opened {
                        name,
                        entries: list,
                    } => {
                        archive_name.set(name);
                        entries.set(list);
                        breadcrumb.set(Vec::new());
                        focus.set(None);
                        selection.set(HashSet::new());
                        expanded.set(HashSet::new());
                        has_archive.set(true);
                        password_prompt.set(None);
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

    // The Extract button doubles as "extract partial files": with no selection
    // it extracts the whole archive; with a selection it extracts only the
    // chosen entries (directories select their whole subtree). The label
    // reflects the live selection so the behavior is never ambiguous.
    let selection_count = selection.read().len();
    let extract_label = if selection_count == 0 {
        "📤 Extract all".to_string()
    } else {
        format!("📤 Extract ({selection_count} selected)")
    };

    rsx! {
        // Global stylesheet (injected once into the WebView document head).
        style { {CSS} }

        div {
            class: "app",
            ondragover: move |evt| evt.prevent_default(),
            ondrop: move |evt| {
                evt.prevent_default();
                if let Some(file) = evt.files().first() {
                    let path = file.path();
                    if !path.as_os_str().is_empty() {
                        open_path(path);
                    }
                }
            },

            // ── Header ─────────────────────────────────────────────────
            header { class: "app-header",
                div { class: "app-logo",
                    span { class: "app-logo-icon", "📦" }
                    "hajizip"
                }
                div { class: "header-actions",
                    button { class: "btn btn-primary btn-sm", onclick: open_dialog,
                        "📂 Open"
                    }
                    button {
                        class: "btn btn-sm",
                        onclick: extract_all,
                        disabled: !has_archive_value,
                        title: "Extract the selected entries, or the whole archive if nothing is selected",
                        "{extract_label}"
                    }
                    button {
                        class: "btn btn-ghost btn-sm",
                        onclick: move |_| settings_open.set(true),
                        "⚙️"
                    }
                }
                div { class: "header-spacer" }
                if has_archive_value {
                    span { class: "header-archive-name", "{archive_name}" }
                }
            }

            // ── Main content ────────────────────────────────────────────
            if has_archive_value {
                // Toolbar with navigation
                div { class: "toolbar",
                    button { class: "btn btn-sm btn-icon", onclick: back, title: "Go up one level",
                        "↑"
                    }
                }

                Breadcrumb {
                    segments: breadcrumb.read().clone(),
                    on_jump: jump,
                }

                div { class: "browser",
                    TreeView {
                        entries: entries,
                        expanded: expanded,
                        on_navigate: enter,
                    }
                    FileList {
                        entries: entries,
                        focus: focus,
                        selected: selection,
                        on_open: enter_list,
                        on_preview: preview,
                    }
                }
            } else {
                div { class: "empty-state",
                    div { class: "empty-icon", "📦" }
                    div { class: "empty-title", "No Archive Open" }
                    div { class: "empty-hint",
                        "Click "
                        strong { "Open" }
                        " to browse for an archive file,"
                        br {}
                        "or drag and drop a file onto this window."
                    }
                }
            }

            // ── Status bar ──────────────────────────────────────────────
            footer {
                class: if status_has_text { "statusbar statusbar-has-text" } else { "statusbar" },
                "{status_text}"
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

            if let Some(update) = progress.read().clone() {
                ProgressDialog {
                    progress: update,
                    on_cancel: cancel_extract,
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
