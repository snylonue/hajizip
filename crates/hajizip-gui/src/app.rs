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
use crate::ui::{Breadcrumb, FileList, PasswordDialog, ProgressDialog, SettingsPanel, TreeView};

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

    rsx! {
        div {
            class: "app",
            style: "display: flex; flex-direction: column; height: 100vh; font-family: sans-serif;",
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

            header {
                style: "display: flex; align-items: center; gap: 10px; padding: 10px 14px; \
                        border-bottom: 1px solid #ddd; background: #fff;",
                h1 { style: "font-size: 16px; margin: 0;", "hajizip" }
                button { onclick: open_dialog, "Open…" }
                button {
                    onclick: extract_all,
                    disabled: !has_archive_value,
                    "Extract…"
                }
                button {
                    onclick: move |_| settings_open.set(true),
                    "Settings"
                }
                if has_archive_value {
                    span { style: "color: #555; margin-left: auto; font-size: 13px;", "{archive_name}" }
                }
            }

            if has_archive_value {
                div {
                    style: "display: flex; align-items: center; padding: 4px 10px; gap: 8px; \
                            border-bottom: 1px solid #eee;",
                    button {
                        onclick: back,
                        style: "padding: 4px 10px; border: 1px solid #ccc; background: white; \
                                border-radius: 4px; cursor: pointer;",
                        "← Up"
                    }
                }
                Breadcrumb {
                    segments: breadcrumb.read().clone(),
                    on_jump: jump,
                }
                div {
                    class: "browser",
                    style: "display: flex; flex: 1; overflow: hidden;",
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
                    }
                }
            } else {
                div {
                    style: "flex: 1; display: flex; align-items: center; justify-content: center; \
                            color: #888;",
                    "Open an archive to browse it (menu, or drag & drop a file here)."
                }
            }

            footer {
                style: "padding: 6px 14px; border-top: 1px solid #ddd; color: #555; font-size: 12px;",
                "{status}"
            }

            // Modal dialogs.
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
