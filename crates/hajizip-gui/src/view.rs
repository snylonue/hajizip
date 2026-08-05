//! App-level view sections: header, browser, empty state, status bar, modals.
//!
//! These are the larger presentational blocks of the root [`App`](crate::app::App)
//! component. They are pure view: controller state arrives as signals, user
//! gestures are forwarded through `on_*` callbacks, and no archive logic lives
//! here (that stays in the controller, see `controller.rs`).

use std::collections::HashSet;
use std::path::PathBuf;

use dioxus::prelude::*;
use hajizip_core::{EntryMeta, EntryPath, FilenameEncoding, NodeKind, OverwritePolicy};

use crate::config::AppConfig;
use crate::controller::{BreadcrumbSegment, ProgressUpdate};
use crate::events::PasswordPrompt;
use crate::icons::{Icon, IconView};
use crate::ui::{
    AboutModal, Breadcrumb, EncodingBanner, FileList, OverflowMenu, PasswordDialog, SettingsPanel,
    TreeView,
};
use crate::viewmodel;

// ---------------------------------------------------------------------------
// Header
// ---------------------------------------------------------------------------

/// Top bar: search box, open / extract / settings actions, overflow menu.
#[component]
pub fn AppHeader(
    /// Live search query (bound to the search input).
    search_query: Signal<String>,
    /// Whether an archive is open (enables the Extract button).
    has_archive: bool,
    /// Currently selected entries (drives the Extract button label).
    selection: Signal<HashSet<EntryPath>>,
    /// Open-archive button.
    on_open: EventHandler<MouseEvent>,
    /// Extract button: extracts the selection, or the whole archive when
    /// nothing is selected (the label reflects the live selection).
    on_extract: EventHandler<MouseEvent>,
    /// Open the settings modal.
    on_settings: EventHandler<()>,
    /// Open the about modal.
    on_about: EventHandler<()>,
    /// Exit the application (the window is closed by the caller).
    on_exit: EventHandler<()>,
) -> Element {
    let selection_count = selection.read().len();
    let extract_label = if selection_count == 0 {
        "Extract all".to_string()
    } else {
        format!("Extract ({selection_count} selected)")
    };
    rsx! {
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
            div { class: "header-actions",
                button { class: "btn btn-primary", onclick: move |evt| on_open.call(evt),
                    IconView { icon: Icon::FolderOpen, size: 16 }
                    "Open"
                }
                button {
                    class: "btn",
                    onclick: move |evt| on_extract.call(evt),
                    disabled: !has_archive,
                    title: "Extract the selected entries, or the whole archive if nothing is selected",
                    IconView { icon: Icon::Download, size: 16 }
                    "{extract_label}"
                }
                button {
                    class: "btn btn-icon",
                    onclick: move |_| on_settings.call(()),
                    title: "Settings",
                    IconView { icon: Icon::Settings, size: 16 }
                }
                OverflowMenu {
                    on_settings: move |_| on_settings.call(()),
                    on_about: move |_| on_about.call(()),
                    on_exit: move |_| on_exit.call(()),
                }
            }
            div { class: "header-spacer" }
        }
    }
}

// ---------------------------------------------------------------------------
// Browser
// ---------------------------------------------------------------------------

/// Browser section: navigation row, left tree, file list, encoding banner.
///
/// Rendered only while an archive is open (the caller decides).
#[component]
pub fn BrowserPane(
    /// Breadcrumb trail for the current view.
    breadcrumb: Signal<Vec<BreadcrumbSegment>>,
    /// Directory currently in focus (the file list shows its children).
    focus: Signal<Option<EntryPath>>,
    /// Flat listing of the current archive's entries.
    entries: Signal<Vec<EntryMeta>>,
    /// Directories expanded in the left tree panel.
    expanded: Signal<HashSet<EntryPath>>,
    /// Currently selected entry paths (multi-select).
    selection: Signal<HashSet<EntryPath>>,
    /// Live search filter (empty = all rows).
    query: String,
    /// Current config (drives the encoding banner).
    config: Signal<AppConfig>,
    /// Whether the garbled-name banner was dismissed.
    banner_dismissed: Signal<bool>,
    /// Back button (navigate up one level).
    on_back: EventHandler<MouseEvent>,
    /// Jump to a breadcrumb segment.
    on_jump: EventHandler<usize>,
    /// Enter a directory / nested archive (tree panel).
    on_enter_tree: EventHandler<EntryPath>,
    /// Enter a directory / nested archive (file list).
    on_enter_list: EventHandler<EntryPath>,
    /// Preview a plain file entry.
    on_preview: EventHandler<EntryPath>,
    /// Change the filename encoding from the banner.
    on_encoding: EventHandler<FilenameEncoding>,
    /// Dismiss the garbled-name banner.
    on_dismiss: EventHandler<()>,
) -> Element {
    // Flat archives (no directories at the root) get the whole left sidebar
    // hidden; the space returns to the file list (§4.3 of review-ui-v2).
    // Implied dirs count too, so this uses the same child_entries view the
    // tree renders.
    let has_sidebar = viewmodel::children_of(&entries.read(), None)
        .iter()
        .any(|e| e.kind == NodeKind::Dir);

    rsx! {
        // Navigation row: back button + breadcrumb trail in one line (the
        // first crumb is the archive name; clicking it returns to the root).
        div { class: "navrow",
            button { class: "btn btn-icon", onclick: move |evt| on_back.call(evt), title: "Go up one level",
                IconView { icon: Icon::CornerUpLeft, size: 16 }
            }
            Breadcrumb {
                segments: breadcrumb.read().clone(),
                on_jump: on_jump,
            }
        }

        div { class: "browser",
            if has_sidebar {
                TreeView {
                    entries: entries,
                    expanded: expanded,
                    on_navigate: on_enter_tree,
                }
            }
            div { class: "browser-main",
                // Garbled-name hint: U+FFFD in any decoded name means the
                // current encoding is wrong for this archive.
                if !*banner_dismissed.read() && viewmodel::has_mangled_names(&entries.read()) {
                    EncodingBanner {
                        config: config,
                        on_encoding: on_encoding,
                        on_dismiss: on_dismiss,
                    }
                }
                FileList {
                    entries: entries,
                    focus: focus,
                    selected: selection,
                    query: query,
                    on_open: on_enter_list,
                    on_preview: on_preview,
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Empty state
// ---------------------------------------------------------------------------

/// Empty state (no archive open): drop zone plus the recent-files list.
#[component]
pub fn EmptyState(
    /// Whether a drag & drop is hovering the window (highlight overlay).
    drag_over: Signal<bool>,
    /// Current config (recent-files list).
    config: Signal<AppConfig>,
    /// Open the dropped / clicked archive path.
    on_open_path: EventHandler<PathBuf>,
) -> Element {
    rsx! {
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
                            let open_recent = on_open_path;
                            rsx! {
                                button {
                                    class: "btn btn-sm recent-file",
                                    onclick: move |_| open_recent.call(path.clone()),
                                    "{path.display()}"
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Status bar
// ---------------------------------------------------------------------------

/// Bottom status bar: archive summary, transient message, selection summary,
/// and inline extraction progress with a cancel button.
#[component]
pub fn StatusBar(
    /// Whether an archive is open (shows the archive summary segment).
    has_archive: bool,
    /// Transient status feedback (preview results, errors, completion).
    status: Signal<String>,
    /// Flat listing of the current archive's entries.
    entries: Signal<Vec<EntryMeta>>,
    /// Currently selected entry paths (multi-select).
    selection: Signal<HashSet<EntryPath>>,
    /// Extraction progress popup state.
    progress: Signal<Option<ProgressUpdate>>,
    /// Cancel the in-flight extraction.
    on_cancel: EventHandler<MouseEvent>,
) -> Element {
    let status_text = status.read().clone();
    let status_has_text = !status_text.is_empty();

    // Left segment: whole-archive summary. The compressed total is shown
    // only when the format records it (most do).
    let summary_text = {
        let list = entries.read();
        let (item_count, total_size, total_compressed) = viewmodel::archive_summary(&list);
        if has_archive {
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
        }
    };

    // Middle segment: selected entries summary.
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

    rsx! {
        footer {
            class: if status_has_text { "statusbar statusbar-has-text" } else { "statusbar" },
            // Left: archive summary (whole current archive).
            if has_archive {
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
                        onclick: move |evt| on_cancel.call(evt),
                        "Cancel"
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Modals
// ---------------------------------------------------------------------------

/// Modal dialogs: password prompt, settings panel, about dialog.
#[component]
pub fn Modals(
    /// Password prompt dialog state.
    password_prompt: Signal<Option<PasswordPrompt>>,
    /// Settings modal open state.
    settings_open: Signal<bool>,
    /// About modal open state.
    about_open: Signal<bool>,
    /// Current config (settings panel).
    config: Signal<AppConfig>,
    /// Submit a password for the pending encrypted archive `(path, password)`.
    on_password: EventHandler<(PathBuf, String)>,
    /// Change the filename encoding (settings panel).
    on_encoding: EventHandler<FilenameEncoding>,
    /// Change the overwrite policy (settings panel).
    on_overwrite: EventHandler<OverwritePolicy>,
    /// Toggle mtime preservation (settings panel).
    on_mtime: EventHandler<bool>,
    /// Restore all settings to defaults (settings panel).
    on_defaults: EventHandler<()>,
) -> Element {
    rsx! {
        if let Some(prompt) = password_prompt.read().clone() {
            PasswordDialog {
                path: prompt.path.display().to_string(),
                error: prompt.error,
                on_submit: move |password| {
                    let path = prompt.path.clone();
                    on_password.call((path, password));
                },
                on_cancel: move |_| password_prompt.set(None),
            }
        }

        if *settings_open.read() {
            SettingsPanel {
                config: config,
                on_encoding: on_encoding,
                on_overwrite: on_overwrite,
                on_mtime: on_mtime,
                on_defaults: on_defaults,
                on_close: move |_| settings_open.set(false),
            }
        }

        if *about_open.read() {
            AboutModal {
                on_close: move |_| about_open.set(false),
            }
        }
    }
}
