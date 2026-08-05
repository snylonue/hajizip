//! Pure view-model helpers: client-side tree building and formatting.
//!
//! The core `Navigator` is still a placeholder (M1), so the GUI builds its own
//! navigation from the flat `Archive::entries()` listing (architecture.md
//! §5.2). Everything here is a pure function so it can be unit-tested without
//! any UI or threads.

use std::collections::HashSet;
use std::time::SystemTime;

use hajizip_core::{EntryMeta, EntryPath, NodeKind};

use crate::icons::Icon;

/// A row of the (left) tree panel: the entry plus its indentation depth.
#[derive(Debug, Clone)]
pub struct TreeRow {
    /// Indentation depth (0 = top level).
    pub depth: usize,
    /// The entry displayed on this row.
    pub entry: EntryMeta,
    /// Whether this row is a directory that can be expanded.
    pub is_dir: bool,
}

/// Direct children of `focus` within `entries` (None = archive root).
///
/// Delegates to the core's `archive::child_entries` — the single source of
/// truth for building trees from a flat listing (previously a copy-paste of
/// the core helper; see `local-doc/review-duplication.md` §1).
pub fn children_of(entries: &[EntryMeta], focus: Option<&EntryPath>) -> Vec<EntryMeta> {
    hajizip_core::archive::child_entries(entries, focus)
}

/// Flatten the archive tree into indented rows, expanding only the dirs in
/// `expanded`. An explicit stack keeps the walk iterative (no recursion depth
/// worries for deep archives).
pub fn tree_rows(entries: &[EntryMeta], expanded: &HashSet<EntryPath>) -> Vec<TreeRow> {
    let mut rows = Vec::new();
    let mut stack: Vec<(usize, Option<EntryPath>)> = vec![(0, None)];
    while let Some((depth, focus)) = stack.pop() {
        let children = children_of(entries, focus.as_ref());
        // Push in reverse so the first child is popped first.
        for child in children.into_iter().rev() {
            let is_dir = child.kind == NodeKind::Dir;
            rows.push(TreeRow {
                depth,
                entry: child.clone(),
                is_dir,
            });
            if is_dir && expanded.contains(&child.path) {
                stack.push((depth + 1, Some(child.path.clone())));
            }
        }
    }
    rows
}

/// Human-readable byte size.
pub fn size_label(entry: &EntryMeta) -> String {
    match entry.uncompressed_size {
        Some(bytes) => format_bytes(bytes),
        None => "—".to_string(),
    }
}

/// Format a byte count as B / KB / MB / GB / TB.
pub fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{value:.0} {}", UNITS[unit])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// Human-readable node kind.
pub fn kind_label(kind: NodeKind) -> &'static str {
    match kind {
        NodeKind::File => "File",
        NodeKind::Dir => "Folder",
        NodeKind::Archive => "Archive",
        NodeKind::Symlink => "Symlink",
    }
}

/// Type icon for a file-list row, chosen by entry kind and file extension.
///
/// Archives get a distinct icon, executables/sources a code icon, images an
/// image icon, text a text icon, and everything else a neutral file icon.
/// Directories are always a folder. Returns the icon plus the CSS class that
/// tints it (the icon itself is theme-aware via `currentColor`).
pub fn file_icon(entry: &EntryMeta) -> (Icon, &'static str) {
    match entry.kind {
        NodeKind::Dir => (Icon::Folder, "icon-type-dir"),
        NodeKind::Archive => (Icon::FileArchive, "icon-type-archive"),
        NodeKind::Symlink => (Icon::File, "icon-type-default"),
        NodeKind::File => file_icon_by_ext(entry.path.as_str()),
    }
}

/// Icon for a plain file, chosen by its extension.
fn file_icon_by_ext(name: &str) -> (Icon, &'static str) {
    let ext = name.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        // Archives that core may report as plain files (e.g. unmarked).
        "zip" | "tar" | "gz" | "tgz" | "xz" | "txz" | "7z" | "rar" => {
            (Icon::FileArchive, "icon-type-archive")
        }
        "exe" | "msi" | "bat" | "cmd" | "sh" | "bash" | "ps1" | "app" | "dll" | "so" | "dylib"
        | "rs" | "py" | "js" | "ts" | "c" | "h" | "cpp" | "java" | "go" | "html" | "css"
        | "json" | "xml" | "yml" | "yaml" | "toml" | "ini" | "conf" | "sql" => {
            (Icon::FileCode, "icon-type-code")
        }
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "svg" | "ico" | "avif" | "heic"
        | "tiff" | "tif" => (Icon::FileImage, "icon-type-image"),
        "txt" | "md" | "markdown" | "log" | "rtf" | "csv" | "tsv" | "pdf" | "doc" | "docx"
        | "xls" | "xlsx" | "ppt" | "pptx" | "odt" | "epub" => (Icon::FileText, "icon-type-text"),
        _ => (Icon::File, "icon-type-default"),
    }
}

/// Format an optional timestamp as `YYYY-MM-DD HH:MM` in the local time zone
/// (falls back to UTC when the local offset is unavailable). Uses the `time`
/// crate instead of hand-rolled civil-date math (see
/// `local-doc/research-time-filetime.md`).
pub fn time_label(mtime: Option<SystemTime>) -> String {
    let Some(mtime) = mtime else {
        return "—".to_string();
    };
    let offset = time::UtcOffset::current_local_offset().unwrap_or(time::UtcOffset::UTC);
    format_civil(time::OffsetDateTime::from(mtime).to_offset(offset))
}

/// Format a date-time as `YYYY-MM-DD HH:MM` (no seconds).
fn format_civil(odt: time::OffsetDateTime) -> String {
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}",
        odt.year(),
        u8::from(odt.month()),
        odt.day(),
        odt.hour(),
        odt.minute(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(path: &str, kind: NodeKind, size: Option<u64>) -> EntryMeta {
        EntryMeta {
            path: EntryPath::new(path).unwrap(),
            raw_name: path.as_bytes().to_vec(),
            kind,
            uncompressed_size: size,
            compressed_size: None,
            mtime: None,
            mode: None,
            crc: None,
            encrypted: false,
            comment: None,
        }
    }

    fn entries() -> Vec<EntryMeta> {
        vec![
            meta("a.txt", NodeKind::File, Some(10)),
            meta("dir/b.txt", NodeKind::File, Some(20)),
            meta("dir/sub/c.txt", NodeKind::File, Some(30)),
            meta("empty", NodeKind::Dir, None),
        ]
    }

    #[test]
    fn root_children_include_implied_dirs() {
        let kids = children_of(&entries(), None);
        let names: Vec<&str> = kids.iter().map(|e| e.path.as_str()).collect();
        // dirs first, then files, alphabetical
        assert_eq!(names, vec!["dir", "empty", "a.txt"]);
    }

    #[test]
    fn nested_children_include_implied_dirs() {
        let focus = EntryPath::new("dir").unwrap();
        let kids = children_of(&entries(), Some(&focus));
        let names: Vec<&str> = kids.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(names, vec!["dir/sub", "dir/b.txt"]);
    }

    #[test]
    fn explicit_dir_beats_implied_dir() {
        let list = vec![
            meta("x/y.txt", NodeKind::File, Some(1)),
            meta("x", NodeKind::Dir, Some(0)),
        ];
        let kids = children_of(&list, None);
        assert_eq!(kids.len(), 1);
        assert_eq!(kids[0].kind, NodeKind::Dir);
        assert_eq!(kids[0].uncompressed_size, Some(0));
    }

    #[test]
    fn tree_rows_respect_expansion() {
        let expanded: HashSet<EntryPath> = HashSet::new();
        let rows = tree_rows(&entries(), &expanded);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].depth, 0);

        let mut expanded = HashSet::new();
        expanded.insert(EntryPath::new("dir").unwrap());
        let rows = tree_rows(&entries(), &expanded);
        assert_eq!(rows.len(), 5);
        // The expanded dir's children are pushed at depth 1.
        assert_eq!(rows.iter().filter(|r| r.depth == 1).count(), 2);
        assert!(rows.iter().any(|r| r.entry.path.as_str() == "dir/b.txt"));
    }

    #[test]
    fn format_bytes_units() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(1023), "1023 B");
        assert_eq!(format_bytes(1024), "1.0 KB");
        assert_eq!(format_bytes(5 * 1024 * 1024), "5.0 MB");
    }

    #[test]
    fn file_icon_maps_kinds_and_extensions() {
        use crate::icons::Icon;
        // Dir / archive / symlink kinds.
        assert_eq!(
            file_icon(&meta("dir/", NodeKind::Dir, None)),
            (Icon::Folder, "icon-type-dir")
        );
        assert_eq!(
            file_icon(&meta("pkg.zip", NodeKind::Archive, None)),
            (Icon::FileArchive, "icon-type-archive")
        );
        assert_eq!(
            file_icon(&meta("link", NodeKind::Symlink, None)),
            (Icon::File, "icon-type-default")
        );
        // Extensions: archives / code / image / text / default.
        assert_eq!(
            file_icon(&meta("a.7z", NodeKind::File, None)),
            (Icon::FileArchive, "icon-type-archive")
        );
        assert_eq!(
            file_icon(&meta("run.exe", NodeKind::File, None)),
            (Icon::FileCode, "icon-type-code")
        );
        assert_eq!(
            file_icon(&meta("UPPER.PNG", NodeKind::File, None)),
            (Icon::FileImage, "icon-type-image")
        );
        assert_eq!(
            file_icon(&meta("notes.md", NodeKind::File, None)),
            (Icon::FileText, "icon-type-text")
        );
        assert_eq!(
            file_icon(&meta("mystery.bin", NodeKind::File, None)),
            (Icon::File, "icon-type-default")
        );
        // No extension.
        assert_eq!(
            file_icon(&meta("README", NodeKind::File, None)),
            (Icon::File, "icon-type-default")
        );
    }

    #[test]
    fn time_label_formats_civil_time() {
        // Build a fixed-offset date-time directly (independent of the test
        // machine's time zone): 2026-01-08 11:04:05 +08:00.
        let odt = time::Date::from_calendar_date(2026, time::Month::January, 8)
            .expect("valid date")
            .with_time(time::Time::from_hms(11, 4, 5).expect("valid time"))
            .assume_offset(time::UtcOffset::from_hms(8, 0, 0).expect("valid offset"));
        assert_eq!(format_civil(odt), "2026-01-08 11:04");
        assert_eq!(time_label(None), "—");
    }
}
