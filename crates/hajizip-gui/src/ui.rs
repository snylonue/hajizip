//! Thin presentational widgets. All archive logic lives in the controller
//! (see `controller.rs`); these components only translate user gestures into
//! intents/callbacks and render state.

use std::collections::HashSet;

use dioxus::prelude::*;
use hajizip_core::{EntryMeta, EntryPath, FilenameEncoding, NodeKind, OverwritePolicy};

use crate::config::AppConfig;
use crate::controller::{BreadcrumbSegment, ProgressUpdate};
use crate::viewmodel;

// ---------------------------------------------------------------------------
// Global CSS design system
// ---------------------------------------------------------------------------
//
// Stylesheets live in `assets/css/` as plain `.css` files so editors provide
// syntax highlighting, linting, and auto-completion.  `include_str!` embeds
// them at compile time — zero runtime I/O, zero extra dependencies.
//
// The files are concatenated in cascade order:
//   tokens  →  base  →  layout  →  components

/// Complete CSS stylesheet for the application.
///
/// Injected once at the root via a `<style>` element. Uses CSS custom
/// properties for a consistent, themeable design.
pub const CSS: &str = concat!(
    include_str!("../assets/css/tokens.css"),
    "\n",
    include_str!("../assets/css/base.css"),
    "\n",
    include_str!("../assets/css/layout.css"),
    "\n",
    include_str!("../assets/css/components.css"),
);

// ---------------------------------------------------------------------------
// Breadcrumb
// ---------------------------------------------------------------------------

/// Clickable breadcrumb bar.
#[component]
pub fn Breadcrumb(
    /// Breadcrumb segments (last one is the current location).
    segments: Vec<BreadcrumbSegment>,
    /// Called with the segment index when a crumb is clicked.
    on_jump: EventHandler<usize>,
) -> Element {
    let last = segments.len().saturating_sub(1);
    rsx! {
        nav { class: "breadcrumb",
            for (i, segment) in segments.iter().enumerate() {
                {
                    let label = segment.label.clone();
                    let index = i;
                    let is_last = i == last;
                    rsx! {
                        Fragment {
                            key: "{index}",
                            if i > 0 {
                                span { class: "breadcrumb-sep", "›" }
                            }
                            if is_last {
                                span { class: "breadcrumb-current", "{label}" }
                            } else {
                                button {
                                    class: "breadcrumb-link",
                                    onclick: move |_| on_jump.call(index),
                                    "{label}"
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
// Tree view
// ---------------------------------------------------------------------------

/// A row of the left tree panel.
#[derive(Clone)]
struct TreeRowData {
    depth: usize,
    entry: EntryMeta,
    is_dir: bool,
}

/// Precomputed display data for one tree row.
struct RowView {
    path: EntryPath,
    is_dir: bool,
    indent: usize,
    arrow: &'static str,
    icon: &'static str,
    name: String,
    row_class: &'static str,
}

/// Left tree panel: the archive's directory structure, expandable per dir.
#[component]
pub fn TreeView(
    /// Flat listing of the current archive.
    entries: Signal<Vec<EntryMeta>>,
    /// Directories currently expanded (full paths).
    expanded: Signal<HashSet<EntryPath>>,
    /// Called when a directory is double-clicked (navigate into it).
    on_navigate: EventHandler<EntryPath>,
) -> Element {
    let rows = build_rows(&entries.read().clone(), expanded.read().clone());
    let views: Vec<RowView> = rows
        .iter()
        .map(|row| {
            let path = row.entry.path.clone();
            let is_dir = row.is_dir;
            let is_expanded = expanded.read().contains(&path);
            let arrow = if !is_dir {
                ""
            } else if is_expanded {
                "▾"
            } else {
                "▸"
            };
            let icon = if is_dir { "📁" } else { "📄" };
            let name = path.as_str().rsplit('/').next().unwrap_or("").to_string();
            RowView {
                path,
                is_dir,
                indent: row.depth * 18,
                arrow,
                icon,
                name,
                row_class: if is_dir {
                    "tree-row tree-row-dir"
                } else {
                    "tree-row tree-row-file"
                },
            }
        })
        .collect();

    rsx! {
        div { class: "tree",
            for (i, row) in views.iter().enumerate() {
                { render_tree_row(row, i, expanded, on_navigate) }
            }
        }
    }
}

/// Render one tree row as an element.
fn render_tree_row(
    row: &RowView,
    i: usize,
    mut expanded: Signal<HashSet<EntryPath>>,
    on_navigate: EventHandler<EntryPath>,
) -> Element {
    let path = row.path.clone();
    let is_dir = row.is_dir;
    let indent = row.indent;
    let arrow = row.arrow;
    let icon = row.icon;
    let name = row.name.clone();
    let row_class = row.row_class;
    let toggle_path = path.clone();
    let nav_path = path.clone();
    rsx! {
        div {
            key: "{i}",
            class: "{row_class}",
            style: "padding-left: {indent}px;",
            onclick: move |_| {
                if is_dir {
                    let mut set = expanded.write();
                    if !set.remove(&toggle_path) {
                        set.insert(toggle_path.clone());
                    }
                }
            },
            ondoubleclick: move |_| {
                if is_dir {
                    on_navigate.call(nav_path.clone());
                }
            },
            span { class: "tree-arrow", "{arrow}" }
            span { class: "tree-icon", "{icon}" }
            span { class: "tree-name", "{name}" }
        }
    }
}

/// Compute the visible tree rows (depth-flattened) given the expansion set.
fn build_rows(entries: &[EntryMeta], expanded: HashSet<EntryPath>) -> Vec<TreeRowData> {
    viewmodel::tree_rows(entries, &expanded)
        .into_iter()
        .map(|row| TreeRowData {
            depth: row.depth,
            entry: row.entry,
            is_dir: row.is_dir,
        })
        .collect()
}

// ---------------------------------------------------------------------------
// File list
// ---------------------------------------------------------------------------

/// Precomputed display data for one file-list row.
struct FileRowView {
    path: EntryPath,
    selected: bool,
    name: String,
    lock: &'static str,
    size: String,
    kind: String,
    time: String,
    /// Whether this row is a plain file (double-click previews it).
    is_file: bool,
}

/// Right file list: children of the current focus directory.
#[component]
pub fn FileList(
    /// Flat listing of the current archive.
    entries: Signal<Vec<EntryMeta>>,
    /// Directory currently in focus (None = archive root).
    focus: Signal<Option<EntryPath>>,
    /// Currently selected paths (multi-select).
    selected: Signal<HashSet<EntryPath>>,
    /// Called when a dir/archive entry is double-clicked (navigate).
    on_open: EventHandler<EntryPath>,
    /// Called when a plain file entry is double-clicked (preview).
    on_preview: EventHandler<EntryPath>,
) -> Element {
    let flat = entries.read().clone();
    let f = focus.read().clone();
    let children = viewmodel::children_of(&flat, f.as_ref());
    let selected_now = selected.read().clone();

    let views: Vec<FileRowView> = children
        .iter()
        .map(|entry| {
            let path = entry.path.clone();
            FileRowView {
                selected: selected_now.contains(&path),
                name: path.as_str().rsplit('/').next().unwrap_or("").to_string(),
                lock: if entry.encrypted { "🔒" } else { "" },
                size: viewmodel::size_label(entry),
                kind: viewmodel::kind_label(entry.kind).to_string(),
                time: viewmodel::time_label(entry.mtime),
                path,
                is_file: entry.kind == NodeKind::File,
            }
        })
        .collect();

    rsx! {
        div { class: "filelist",
            table {
                thead {
                    tr {
                        th { class: "col-lock", "" }
                        th { "Name" }
                        th { class: "col-size", "Size" }
                        th { class: "col-type", "Type" }
                        th { class: "col-modified", "Modified" }
                    }
                }
                tbody {
                    for (i, row) in views.iter().enumerate() {
                        { render_file_row(row, i, selected, on_open, on_preview) }
                    }
                }
            }
        }
    }
}

/// Render one file-list row as an element.
fn render_file_row(
    row: &FileRowView,
    i: usize,
    mut selected: Signal<HashSet<EntryPath>>,
    on_open: EventHandler<EntryPath>,
    on_preview: EventHandler<EntryPath>,
) -> Element {
    let path = row.path.clone();
    let name = row.name.clone();
    let lock = row.lock;
    let size = row.size.clone();
    let kind = row.kind.clone();
    let time = row.time.clone();
    let toggle_path = path.clone();
    let open_path = path.clone();
    let preview_path = path.clone();
    let is_file = row.is_file;
    let row_class = if row.selected {
        "file-row file-row-selected"
    } else {
        "file-row"
    };
    rsx! {
        tr {
            key: "{i}",
            class: "{row_class}",
            onclick: move |_| {
                let mut set = selected.write();
                if !set.remove(&toggle_path) {
                    set.insert(toggle_path.clone());
                }
            },
            ondoubleclick: move |_| {
                if is_file {
                    on_preview.call(preview_path.clone());
                } else {
                    on_open.call(open_path.clone());
                }
            },
            td { class: "col-lock", "{lock}" }
            td { class: "col-name", "{name}" }
            td { class: "col-size", "{size}" }
            td { class: "col-type", "{kind}" }
            td { class: "col-modified", "{time}" }
        }
    }
}

// ---------------------------------------------------------------------------
// Password dialog
// ---------------------------------------------------------------------------

/// Modal asking for a password to open an encrypted archive.
#[component]
pub fn PasswordDialog(
    /// Displayed in the dialog title.
    path: String,
    /// Error to show above the input (e.g. wrong password), if any.
    error: Option<String>,
    /// Called with the entered password on submit.
    on_submit: EventHandler<String>,
    /// Called when the user dismisses the dialog.
    on_cancel: EventHandler<()>,
) -> Element {
    let mut value = use_signal(String::new);
    rsx! {
        div { class: "modal-overlay",
            div { class: "modal-card modal-card-sm",
                h3 { class: "modal-title", "🔐 Password Required" }
                p { class: "modal-desc", "{path}" }
                if let Some(err) = error {
                    p { class: "modal-error", "{err}" }
                }
                input {
                    class: "modal-input",
                    r#type: "password",
                    value: "{value}",
                    placeholder: "Enter password…",
                    oninput: move |e| value.set(e.value()),
                }
                div { class: "modal-actions",
                    button { class: "btn", onclick: move |_| on_cancel.call(()), "Cancel" }
                    button {
                        class: "btn btn-primary",
                        onclick: move |_| on_submit.call(value.read().clone()),
                        "Unlock"
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Progress dialog
// ---------------------------------------------------------------------------

/// Modal showing extraction progress with a cancel button.
#[component]
pub fn ProgressDialog(
    /// Latest progress snapshot.
    progress: ProgressUpdate,
    /// Called when the user cancels.
    on_cancel: EventHandler<()>,
) -> Element {
    let (percent, label) = progress_label(&progress);
    rsx! {
        div { class: "modal-overlay",
            div { class: "modal-card modal-card-md",
                h3 { class: "modal-title", "📦 Extracting…" }
                p { class: "modal-desc", "{label}" }
                div { class: "progress-track",
                    div { class: "progress-fill", style: "width: {percent}%;" }
                }
                p { class: "progress-info", "{progress.entries_done} entries processed" }
                div { class: "modal-actions",
                    button { class: "btn btn-danger", onclick: move |_| on_cancel.call(()), "Cancel" }
                }
            }
        }
    }
}

/// Compute (percent, description) for the progress dialog.
fn progress_label(progress: &ProgressUpdate) -> (u32, String) {
    let label = match &progress.current {
        Some(path) => path.as_str().to_string(),
        None => "Preparing…".to_string(),
    };
    let percent = match progress.bytes_total {
        Some(total) if total > 0 => ((progress.bytes_done as f64 / total as f64) * 100.0) as u32,
        _ => 0,
    };
    (percent.min(100), label)
}

// ---------------------------------------------------------------------------
// Settings panel
// ---------------------------------------------------------------------------

/// Settings panel (modal): encoding, overwrite policy, mtime preservation.
#[component]
pub fn SettingsPanel(
    /// Current config values shown in the controls.
    config: Signal<AppConfig>,
    /// Called when the user picks a filename encoding.
    on_encoding: EventHandler<FilenameEncoding>,
    /// Called when the user picks an overwrite policy.
    on_overwrite: EventHandler<OverwritePolicy>,
    /// Called when the user toggles mtime preservation.
    on_mtime: EventHandler<bool>,
    /// Called to restore all defaults.
    on_defaults: EventHandler<()>,
    /// Called to close the panel.
    on_close: EventHandler<()>,
) -> Element {
    let cfg = config.read().clone();
    let enc_value = encoding_value(cfg.filename_encoding);
    let ovr_value = overwrite_value(cfg.overwrite_policy);

    rsx! {
        div { class: "modal-overlay",
            div { class: "modal-card modal-card-lg",
                h3 { class: "modal-title", "⚙️ Settings" }
                div { class: "settings-group",
                    label { class: "settings-label", "Filename encoding" }
                    select {
                        class: "settings-select",
                        value: enc_value,
                        onchange: move |e| {
                            if let Some(enc) = parse_encoding(&e.value()) {
                                on_encoding.call(enc);
                            }
                        },
                        option { value: "auto", "Auto (detect)" }
                        option { value: "utf8", "UTF-8" }
                        option { value: "gbk", "GBK (Simplified Chinese)" }
                        option { value: "shift-jis", "Shift-JIS (Japanese)" }
                        option { value: "big5", "Big5 (Traditional Chinese)" }
                        option { value: "cp437", "CP437 (DOS)" }
                    }
                }
                div { class: "settings-group",
                    label { class: "settings-label", "Overwrite existing files" }
                    select {
                        class: "settings-select",
                        value: ovr_value,
                        onchange: move |e| {
                            if let Some(policy) = parse_overwrite(&e.value()) {
                                on_overwrite.call(policy);
                            }
                        },
                        option { value: "ask", "Ask (skip existing for now)" }
                        option { value: "always", "Always overwrite" }
                        option { value: "never", "Never overwrite" }
                        option { value: "newer", "Overwrite if newer" }
                    }
                }
                div { class: "settings-checkbox-row",
                    input {
                        r#type: "checkbox",
                        id: "preserve-mtime",
                        checked: cfg.preserve_mtime,
                        onchange: move |e| on_mtime.call(e.value().parse().unwrap_or(true)),
                    }
                    label { r#for: "preserve-mtime", "Preserve modification times" }
                }
                div { class: "modal-actions-between",
                    button { class: "btn", onclick: move |_| on_defaults.call(()), "Restore Defaults" }
                    button { class: "btn btn-primary", onclick: move |_| on_close.call(()), "Close" }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Encoding / overwrite helpers
// ---------------------------------------------------------------------------

/// `<option>` value for a filename encoding.
fn encoding_value(e: FilenameEncoding) -> &'static str {
    match e {
        FilenameEncoding::Auto => "auto",
        FilenameEncoding::Forced(hajizip_core::Codepage::Utf8) => "utf8",
        FilenameEncoding::Forced(hajizip_core::Codepage::Gbk) => "gbk",
        FilenameEncoding::Forced(hajizip_core::Codepage::ShiftJis) => "shift-jis",
        FilenameEncoding::Forced(hajizip_core::Codepage::Big5) => "big5",
        FilenameEncoding::Forced(hajizip_core::Codepage::Cp437) => "cp437",
    }
}

/// Parse an `<option>` value back into a filename encoding.
fn parse_encoding(s: &str) -> Option<FilenameEncoding> {
    use hajizip_core::Codepage;
    Some(match s {
        "auto" => FilenameEncoding::Auto,
        "utf8" => FilenameEncoding::Forced(Codepage::Utf8),
        "gbk" => FilenameEncoding::Forced(Codepage::Gbk),
        "shift-jis" => FilenameEncoding::Forced(Codepage::ShiftJis),
        "big5" => FilenameEncoding::Forced(Codepage::Big5),
        "cp437" => FilenameEncoding::Forced(Codepage::Cp437),
        _ => return None,
    })
}

/// `<option>` value for an overwrite policy.
fn overwrite_value(p: OverwritePolicy) -> &'static str {
    match p {
        OverwritePolicy::Ask => "ask",
        OverwritePolicy::Always => "always",
        OverwritePolicy::Never => "never",
        OverwritePolicy::Newer => "newer",
    }
}

/// Parse an `<option>` value back into an overwrite policy.
fn parse_overwrite(s: &str) -> Option<OverwritePolicy> {
    Some(match s {
        "ask" => OverwritePolicy::Ask,
        "always" => OverwritePolicy::Always,
        "never" => OverwritePolicy::Never,
        "newer" => OverwritePolicy::Newer,
        _ => return None,
    })
}
