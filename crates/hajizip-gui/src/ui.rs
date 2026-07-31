//! Thin presentational widgets. All archive logic lives in the controller
//! (see `controller.rs`); these components only translate user gestures into
//! intents/callbacks and render state.

use std::collections::HashSet;

use dioxus::prelude::*;
use hajizip_core::{EntryMeta, EntryPath, FilenameEncoding, OverwritePolicy};

use crate::config::AppConfig;
use crate::controller::{BreadcrumbSegment, ProgressUpdate};
use crate::viewmodel;

/// Clickable breadcrumb bar.
#[component]
pub fn Breadcrumb(
    /// Breadcrumb segments (last one is the current location).
    segments: Vec<BreadcrumbSegment>,
    /// Called with the segment index when a crumb is clicked.
    on_jump: EventHandler<usize>,
) -> Element {
    rsx! {
        nav {
            class: "breadcrumb",
            style: "display: flex; flex-wrap: wrap; gap: 4px; padding: 6px 10px; \
                    border-bottom: 1px solid #ddd; background: #fafafa; align-items: center;",
            for (i, segment) in segments.iter().enumerate() {
                {
                    let label = segment.label.clone();
                    let index = i;
                    let separator = (i > 0).then_some(());
                    rsx! {
                        Fragment {
                            key: "{index}",
                            if separator.is_some() {
                                span { style: "color: #999;", "/" }
                            }
                            button {
                                style: "border: none; background: none; cursor: pointer; color: #2b5c8a; \
                                        font: inherit; padding: 2px 4px; border-radius: 4px;",
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

/// A row of the left tree panel.
#[derive(Clone)]
struct TreeRowData {
    depth: usize,
    entry: EntryMeta,
    is_dir: bool,
}

/// Precomputed display data for one tree row (avoids complex expressions in
/// the rsx `for` loop body).
struct RowView {
    path: EntryPath,
    is_dir: bool,
    indent: usize,
    arrow: &'static str,
    name: String,
    weight: &'static str,
}

/// Left tree panel: the archive's directory structure, expandable per dir.
#[component]
pub fn TreeView(
    /// Flat listing of the current archive (as a signal so expansion and
    /// navigation re-render the tree).
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
                " "
            } else if is_expanded {
                "▼"
            } else {
                "▶"
            };
            let name = path.as_str().rsplit('/').next().unwrap_or("").to_string();
            RowView {
                path,
                is_dir,
                indent: row.depth * 16,
                arrow,
                name,
                weight: if is_dir { "600" } else { "400" },
            }
        })
        .collect();

    rsx! {
        div {
            class: "tree",
            style: "width: 280px; min-width: 200px; overflow: auto; border-right: 1px solid #ddd; \
                    padding: 6px; font-family: monospace;",
            for (i, row) in views.iter().enumerate() {
                {render_tree_row(row, i, expanded, on_navigate)}
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
    let name = row.name.clone();
    let weight = row.weight;
    let toggle_path = path.clone();
    let nav_path = path.clone();
    rsx! {
        div {
            key: "{i}",
            style: "display: flex; align-items: center; gap: 4px; padding: 2px 4px; \
                    white-space: nowrap; border-radius: 4px;",
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
            span { style: "width: {indent}px; display: inline-block;", }
            span { "{arrow}" }
            span { style: "font-weight: {weight};", "{name}" }
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

/// Precomputed display data for one file-list row.
struct FileRowView {
    path: EntryPath,
    bg: &'static str,
    name: String,
    lock: &'static str,
    size: String,
    kind: String,
    time: String,
}

/// Right file list: children of the current focus directory.
#[component]
pub fn FileList(
    /// Flat listing of the current archive (as a signal so selection and
    /// navigation re-render the list).
    entries: Signal<Vec<EntryMeta>>,
    /// Directory currently in focus (None = archive root).
    focus: Signal<Option<EntryPath>>,
    /// Currently selected paths (multi-select).
    selected: Signal<HashSet<EntryPath>>,
    /// Called when an entry is double-clicked.
    on_open: EventHandler<EntryPath>,
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
                bg: if selected_now.contains(&path) {
                    "#dbe9f7"
                } else {
                    "transparent"
                },
                name: path.as_str().rsplit('/').next().unwrap_or("").to_string(),
                lock: if entry.encrypted { "🔒" } else { "" },
                size: viewmodel::size_label(entry),
                kind: viewmodel::kind_label(entry.kind).to_string(),
                time: viewmodel::time_label(entry.mtime),
                path,
            }
        })
        .collect();

    rsx! {
        div {
            class: "filelist",
            style: "flex: 1; overflow: auto; font-family: sans-serif;",
            table {
                style: "width: 100%; border-collapse: collapse;",
                thead {
                    tr {
                        style: "text-align: left; background: #f5f5f5; position: sticky; top: 0;",
                        th { style: "padding: 6px 10px;", "" }
                        th { style: "padding: 6px 10px;", "Name" }
                        th { style: "padding: 6px 10px; text-align: right;", "Size" }
                        th { style: "padding: 6px 10px;", "Type" }
                        th { style: "padding: 6px 10px;", "Modified" }
                    }
                }
                tbody {
                    for (i, row) in views.iter().enumerate() {
                        {render_file_row(row, i, selected, on_open)}
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
) -> Element {
    let path = row.path.clone();
    let bg = row.bg;
    let name = row.name.clone();
    let lock = row.lock;
    let size = row.size.clone();
    let kind = row.kind.clone();
    let time = row.time.clone();
    let toggle_path = path.clone();
    let open_path = path.clone();
    rsx! {
        tr {
            key: "{i}",
            style: "cursor: default; background: {bg};",
            onclick: move |_| {
                let mut set = selected.write();
                if !set.remove(&toggle_path) {
                    set.insert(toggle_path.clone());
                }
            },
            ondoubleclick: move |_| on_open.call(open_path.clone()),
            td { style: "padding: 4px 10px;", "{lock}" }
            td { style: "padding: 4px 10px;", "{name}" }
            td { style: "padding: 4px 10px; text-align: right;", "{size}" }
            td { style: "padding: 4px 10px;", "{kind}" }
            td { style: "padding: 4px 10px; color: #777;", "{time}" }
        }
    }
}

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
        div {
            style: "position: fixed; inset: 0; background: rgba(0,0,0,0.35); display: flex; \
                    align-items: center; justify-content: center; z-index: 100;",
            div {
                style: "background: white; border-radius: 8px; padding: 20px; width: 380px; \
                        box-shadow: 0 4px 20px rgba(0,0,0,0.25);",
                h3 { "Password required" }
                p { style: "color: #555; font-size: 13px; word-break: break-all;", "{path}" }
                if let Some(err) = error {
                    p { style: "color: #b00020; font-size: 13px;", "{err}" }
                }
                input {
                    r#type: "password",
                    value: "{value}",
                    placeholder: "Password",
                    style: "width: 100%; padding: 8px; box-sizing: border-box; margin: 8px 0;",
                    oninput: move |e| value.set(e.value()),
                }
                div { style: "display: flex; justify-content: flex-end; gap: 8px;",
                    button {
                        onclick: move |_| on_cancel.call(()),
                        style: "padding: 6px 14px; border: 1px solid #ccc; background: white; \
                                border-radius: 4px; cursor: pointer;",
                        "Cancel"
                    }
                    button {
                        onclick: move |_| on_submit.call(value.read().clone()),
                        style: "padding: 6px 14px; border: none; background: #2b5c8a; color: white; \
                                border-radius: 4px; cursor: pointer;",
                        "Unlock"
                    }
                }
            }
        }
    }
}

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
        div {
            style: "position: fixed; inset: 0; background: rgba(0,0,0,0.35); display: flex; \
                    align-items: center; justify-content: center; z-index: 100;",
            div {
                style: "background: white; border-radius: 8px; padding: 20px; width: 440px; \
                        box-shadow: 0 4px 20px rgba(0,0,0,0.25);",
                h3 { "Extracting…" }
                p {
                    style: "color: #555; font-size: 13px; word-break: break-all;",
                    "{label}"
                }
                div {
                    style: "height: 10px; background: #eee; border-radius: 5px; overflow: hidden; margin: 12px 0;",
                    div {
                        style: "height: 100%; width: {percent}%; background: #2b5c8a; transition: width 0.2s;",
                    }
                }
                p { style: "color: #777; font-size: 12px;", "Entries done: {progress.entries_done}" }
                div { style: "display: flex; justify-content: flex-end;",
                    button {
                        onclick: move |_| on_cancel.call(()),
                        style: "padding: 6px 14px; border: none; background: #b00020; color: white; \
                                border-radius: 4px; cursor: pointer;",
                        "Cancel"
                    }
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

/// Settings panel (modal): encoding, overwrite policy, mtime preservation.
#[component]
pub fn SettingsPanel(
    /// Current config values shown in the controls (as a signal so changes
    /// from `ConfigChanged` events re-render the panel).
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
        div {
            style: "position: fixed; inset: 0; background: rgba(0,0,0,0.35); display: flex; \
                    align-items: center; justify-content: center; z-index: 100;",
            div {
                style: "background: white; border-radius: 8px; padding: 20px; width: 420px; \
                        box-shadow: 0 4px 20px rgba(0,0,0,0.25);",
                h3 { "Settings" }
                div { style: "margin: 10px 0;",
                    label { style: "font-size: 13px;", "Filename encoding" }
                    select {
                        style: "width: 100%; padding: 6px; margin-top: 4px;",
                        value: enc_value,
                        onchange: move |e| {
                            if let Some(enc) = parse_encoding(&e.value()) {
                                on_encoding.call(enc);
                            }
                        },
                        option { value: "auto", "Auto" }
                        option { value: "utf8", "UTF-8" }
                        option { value: "gbk", "GBK" }
                        option { value: "shift-jis", "Shift-JIS" }
                        option { value: "big5", "Big5" }
                        option { value: "cp437", "CP437" }
                    }
                }
                div { style: "margin: 10px 0;",
                    label { style: "font-size: 13px;", "Overwrite existing files" }
                    select {
                        style: "width: 100%; padding: 6px; margin-top: 4px;",
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
                div { style: "margin: 10px 0; display: flex; align-items: center; gap: 8px;",
                    input {
                        r#type: "checkbox",
                        checked: cfg.preserve_mtime,
                        onchange: move |e| on_mtime.call(e.value().parse().unwrap_or(true)),
                    }
                    label { style: "font-size: 13px;", "Preserve modification times" }
                }
                div { style: "display: flex; justify-content: space-between; margin-top: 16px;",
                    button {
                        onclick: move |_| on_defaults.call(()),
                        style: "padding: 6px 14px; border: 1px solid #ccc; background: white; \
                                border-radius: 4px; cursor: pointer;",
                        "Restore defaults"
                    }
                    button {
                        onclick: move |_| on_close.call(()),
                        style: "padding: 6px 14px; border: none; background: #2b5c8a; color: white; \
                                border-radius: 4px; cursor: pointer;",
                        "Close"
                    }
                }
            }
        }
    }
}

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
