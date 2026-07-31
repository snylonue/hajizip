//! Pure view-model helpers: client-side tree building and formatting.
//!
//! The core `Navigator` is still a placeholder (M1), so the GUI builds its own
//! navigation from the flat `Archive::entries()` listing (architecture.md
//! §5.2). Everything here is a pure function so it can be unit-tested without
//! any UI or threads.

use std::collections::{BTreeMap, HashSet};
use std::time::{SystemTime, UNIX_EPOCH};

use hajizip_core::{EntryMeta, EntryPath, NodeKind};

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
/// Directories implied by path prefixes (e.g. `a/b.txt` implies dir `a`) are
/// synthesized when the archive does not list them explicitly. Directories are
/// listed before files, both alphabetically.
pub fn children_of(entries: &[EntryMeta], focus: Option<&EntryPath>) -> Vec<EntryMeta> {
    let prefix = focus
        .map(|f| format!("{}/", f.as_str()))
        .unwrap_or_default();
    let mut by_name: BTreeMap<String, EntryMeta> = BTreeMap::new();

    // Pass 1: explicit entries directly under `focus`.
    for e in entries {
        let p = e.path.as_str();
        if !p.starts_with(&prefix) {
            continue;
        }
        let rest = &p[prefix.len()..];
        if rest.is_empty() || rest.contains('/') {
            continue;
        }
        by_name.insert(rest.to_string(), e.clone());
    }

    // Pass 2: directories implied by deeper paths, only if not explicit.
    for e in entries {
        let p = e.path.as_str();
        if !p.starts_with(&prefix) {
            continue;
        }
        let rest = &p[prefix.len()..];
        let Some((first, _)) = rest.split_once('/') else {
            continue;
        };
        by_name.entry(first.to_string()).or_insert_with(|| {
            let full = format!("{prefix}{first}");
            EntryMeta {
                path: EntryPath::new(&full).expect("implied dir path is valid"),
                raw_name: first.as_bytes().to_vec(),
                kind: NodeKind::Dir,
                uncompressed_size: None,
                compressed_size: None,
                mtime: None,
                mode: None,
                crc: None,
                encrypted: false,
                comment: None,
            }
        });
    }

    let mut out: Vec<EntryMeta> = by_name.into_values().collect();
    out.sort_by(|a, b| {
        let a_is_dir = a.kind == NodeKind::Dir;
        let b_is_dir = b.kind == NodeKind::Dir;
        b_is_dir
            .cmp(&a_is_dir)
            .then_with(|| a.path.as_str().cmp(b.path.as_str()))
    });
    out
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

/// Format an optional timestamp as `YYYY-MM-DD HH:MM` (local time zone offset
/// intentionally not applied; a plain civil date keeps this dependency-free).
pub fn time_label(mtime: Option<SystemTime>) -> String {
    let Some(mtime) = mtime else {
        return "—".to_string();
    };
    let secs = mtime
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let (y, mo, d, h, mi) = civil_from_epoch(secs);
    format!("{y:04}-{mo:02}-{d:02} {h:02}:{mi:02}")
}

/// Convert Unix seconds to (year, month, day, hour, minute) civil time using
/// Howard Hinnant's algorithm; pure integer math, no external crates.
fn civil_from_epoch(secs: u64) -> (i64, u32, u32, u32, u32) {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let (h, mi) = ((rem / 3600) as u32, ((rem % 3600) / 60) as u32);

    // Civil-from-days (Howard Hinnant).
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d, h, mi)
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
    fn time_label_known_epoch() {
        // 2026-01-08 11:04:05 UTC (verified with `date -u -d @1767870245`).
        let t = UNIX_EPOCH + std::time::Duration::new(1_767_870_245, 0);
        assert_eq!(time_label(Some(t)), "2026-01-08 11:04");
        assert_eq!(time_label(None), "—");
    }
}
