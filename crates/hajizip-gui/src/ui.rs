//! Thin presentational widgets. All archive logic lives in the controller
//! (see `controller.rs`); these components only translate user gestures into
//! intents/callbacks and render state.

use std::collections::HashSet;

use dioxus::prelude::*;
use hajizip_core::{EntryMeta, EntryPath, FilenameEncoding, NodeKind, OverwritePolicy};

use crate::config::AppConfig;
use crate::controller::BreadcrumbSegment;
use crate::icons::{Icon, IconView};
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
    arrow: Option<Icon>,
    icon: Icon,
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
                None
            } else if is_expanded {
                Some(Icon::ChevronDown)
            } else {
                Some(Icon::ChevronRight)
            };
            let icon = if is_dir { Icon::Folder } else { Icon::File };
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
            div { class: "tree-header", "Folders" }
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
            if let Some(arrow) = arrow {
                span { class: "tree-arrow",
                    IconView { icon: arrow, size: 12 }
                }
            } else {
                span { class: "tree-arrow" }
            }
            span { class: "tree-icon",
                IconView { icon: icon, size: 14 }
            }
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
    locked: bool,
    size: String,
    kind: String,
    time: String,
    /// Whether this row is a plain file (double-click previews it).
    is_file: bool,
    /// Type icon for the NAME column plus its tint class.
    type_icon: Icon,
    type_class: &'static str,
    /// Whether this row is a directory (bold name to distinguish types).
    is_dir: bool,
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
    /// Live search filter (empty = all rows).
    query: String,
    /// Called when a dir/archive entry is double-clicked (navigate).
    on_open: EventHandler<EntryPath>,
    /// Called when a plain file entry is double-clicked (preview).
    on_preview: EventHandler<EntryPath>,
) -> Element {
    let flat = entries.read().clone();
    let f = focus.read().clone();
    let mut children = viewmodel::filter_children(&flat, f.as_ref(), &query);

    // Column sort lives in the list's own state: cycle Asc → Desc → off.
    let mut sort = use_signal(viewmodel::Sort::default);
    if let Some((field, dir)) = *sort.read() {
        children = viewmodel::sort_children(children, field, dir);
    }

    let selected_now = selected.read().clone();

    let views: Vec<FileRowView> = children
        .iter()
        .map(|entry| {
            let path = entry.path.clone();
            let (type_icon, type_class) = viewmodel::file_icon(entry);
            FileRowView {
                selected: selected_now.contains(&path),
                name: path.as_str().rsplit('/').next().unwrap_or("").to_string(),
                locked: entry.encrypted,
                size: viewmodel::size_label(entry),
                kind: viewmodel::kind_label(entry.kind).to_string(),
                time: viewmodel::relative_time_label(entry.mtime),
                path,
                is_file: entry.kind == NodeKind::File,
                is_dir: entry.kind == NodeKind::Dir,
                type_icon,
                type_class,
            }
        })
        .collect();

    // Sort indicator for the header of the active column.
    let active_sort = *sort.read();
    let sort_icon = |field: viewmodel::SortField| -> Option<Icon> {
        match active_sort {
            Some((f, viewmodel::SortDir::Asc)) if f == field => Some(Icon::ArrowUp),
            Some((f, viewmodel::SortDir::Desc)) if f == field => Some(Icon::ArrowDown),
            _ => None,
        }
    };

    rsx! {
        div { class: "filelist",
            table {
                thead {
                    tr {
                        th { class: "col-lock", "" }
                        th {
                            class: "th-sortable",
                            title: "Sort by name",
                            onclick: move |_| {
                                let next = viewmodel::cycle_sort(*sort.read(), viewmodel::SortField::Name);
                                sort.set(next);
                            },
                            "Name"
                            if let Some(icon) = sort_icon(viewmodel::SortField::Name) {
                                IconView { icon: icon, size: 12, class: Some("th-sort-icon".to_string()) }
                            }
                        }
                        th {
                            class: "col-size th-sortable",
                            title: "Sort by size",
                            onclick: move |_| {
                                let next = viewmodel::cycle_sort(*sort.read(), viewmodel::SortField::Size);
                                sort.set(next);
                            },
                            "Size"
                            if let Some(icon) = sort_icon(viewmodel::SortField::Size) {
                                IconView { icon: icon, size: 12, class: Some("th-sort-icon".to_string()) }
                            }
                        }
                        th {
                            class: "col-type th-sortable",
                            title: "Sort by type",
                            onclick: move |_| {
                                let next = viewmodel::cycle_sort(*sort.read(), viewmodel::SortField::Type);
                                sort.set(next);
                            },
                            "Type"
                            if let Some(icon) = sort_icon(viewmodel::SortField::Type) {
                                IconView { icon: icon, size: 12, class: Some("th-sort-icon".to_string()) }
                            }
                        }
                        th {
                            class: "col-modified th-sortable",
                            title: "Sort by modification time",
                            onclick: move |_| {
                                let next = viewmodel::cycle_sort(*sort.read(), viewmodel::SortField::Modified);
                                sort.set(next);
                            },
                            "Modified"
                            if let Some(icon) = sort_icon(viewmodel::SortField::Modified) {
                                IconView { icon: icon, size: 12, class: Some("th-sort-icon".to_string()) }
                            }
                        }
                    }
                }
                tbody {
                    if views.is_empty() {
                        tr {
                            td { class: "no-match", colspan: "5",
                                if query.trim().is_empty() {
                                    "This folder is empty"
                                } else {
                                    "No entries match the search"
                                }
                            }
                        }
                    } else {
                        for (i, row) in views.iter().enumerate() {
                            { render_file_row(row, i, selected, on_open, on_preview) }
                        }
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
    let locked = row.locked;
    let size = row.size.clone();
    let kind = row.kind.clone();
    let time = row.time.clone();
    let type_icon = row.type_icon;
    let type_class = row.type_class;
    let toggle_path = path.clone();
    let open_path = path.clone();
    let preview_path = path.clone();
    let is_file = row.is_file;
    let row_class = if row.selected {
        if row.is_dir {
            "file-row file-row-selected file-row-dir"
        } else {
            "file-row file-row-selected"
        }
    } else if row.is_dir {
        "file-row file-row-dir"
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
            td { class: "col-lock",
                if locked {
                    IconView { icon: Icon::Lock, size: 12, class: Some("icon-lock".to_string()) }
                }
            }
            td { class: "col-name",
                span { class: "name-icon",
                    IconView { icon: type_icon, size: 15, class: Some(type_class.to_string()) }
                }
                span { class: "name-text", "{name}" }
            }
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
                h3 { class: "modal-title",
                    IconView { icon: Icon::Key, size: 16, class: Some("title-icon".to_string()) }
                    "Password Required"
                }
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
// Settings panel
// ---------------------------------------------------------------------------

/// Settings panel (modal): encoding, overwrite policy, mtime preservation.
///
/// Changes apply immediately (the config is persisted on every change); the
/// panel only has a "Done" button to close it. Esc / ✕ also close it (see
/// `app.rs`).
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
    // Per-option help line under each select, updated live.
    let enc_hint = encoding_hint(&cfg.filename_encoding);
    let ovr_hint = overwrite_hint(cfg.overwrite_policy);

    rsx! {
        div { class: "modal-overlay",
            div { class: "modal-card modal-card-lg",
                div { class: "modal-card-head",
                    h3 { class: "modal-title",
                        IconView { icon: Icon::Settings, size: 16, class: Some("title-icon".to_string()) }
                        "Settings"
                    }
                    button {
                        class: "btn btn-icon",
                        onclick: move |_| on_close.call(()),
                        title: "Close (Esc)",
                        IconView { icon: Icon::X, size: 16 }
                    }
                }
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
                    p { class: "settings-hint", "{enc_hint}" }
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
                        option { value: "ask", "Ask when a file exists" }
                        option { value: "always", "Always overwrite" }
                        option { value: "never", "Never overwrite" }
                        option { value: "newer", "Overwrite if newer" }
                        option { value: "rename", disabled: true, "Keep both — rename (coming soon)" }
                    }
                    p { class: "settings-hint", "{ovr_hint}" }
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
                p { class: "settings-hint", "Keep each file's original modification time from the archive." }
                div { class: "modal-actions-between",
                    button { class: "link-danger", onclick: move |_| on_defaults.call(()), "Restore defaults" }
                    button { class: "btn", onclick: move |_| on_close.call(()), "Done" }
                }
            }
        }
    }
}

/// One-line explanation for the current filename encoding option.
fn encoding_hint(e: &FilenameEncoding) -> &'static str {
    use hajizip_core::Codepage;
    match e {
        FilenameEncoding::Auto => {
            "Detect: UTF-8 when flagged, otherwise sniff GBK / Shift-JIS / Big5 from the entry names."
        }
        FilenameEncoding::Forced(Codepage::Utf8) => "Treat every entry name as UTF-8.",
        FilenameEncoding::Forced(Codepage::Gbk) => {
            "Decode legacy Chinese names (GBK) — the usual fix for garbled mainland-China archives."
        }
        FilenameEncoding::Forced(Codepage::ShiftJis) => {
            "Decode legacy Japanese names (Shift-JIS) — the usual fix for garbled Japanese archives."
        }
        FilenameEncoding::Forced(Codepage::Big5) => {
            "Decode legacy Traditional-Chinese names (Big5)."
        }
        FilenameEncoding::Forced(Codepage::Cp437) => "Decode MS-DOS code page 437 names (rare).",
    }
}

/// One-line explanation for the current overwrite policy.
fn overwrite_hint(p: OverwritePolicy) -> &'static str {
    match p {
        OverwritePolicy::Ask => "Ask before replacing each existing file.",
        OverwritePolicy::Always => "Replace existing files without asking.",
        OverwritePolicy::Never => "Skip existing files and leave them untouched.",
        OverwritePolicy::Newer => "Replace only when the file in the archive is newer.",
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
