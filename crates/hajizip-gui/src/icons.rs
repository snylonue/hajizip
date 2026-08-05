//! Inline SVG icons (Lucide line style, ISC license).
//!
//! Emoji glyphs render inconsistently across platforms and cannot follow the
//! theme (they are full-colour fonts). Instead every icon is a small set of
//! SVG paths drawn with `stroke="currentColor"`, so icons inherit the text
//! colour and work in both light and dark themes.
//!
//! Path data is taken from the [Lucide](https://lucide.dev) icon set (ISC
//! license) and embedded directly — no runtime dependency, no network.

use dioxus::prelude::*;

/// Identifiers for the icons used by the UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Icon {
    /// Folder (directory nodes).
    Folder,
    /// Open folder (the "Open archive" action).
    FolderOpen,
    /// Generic file.
    File,
    /// Archive file (zip / tar / 7z / rar…).
    FileArchive,
    /// Executable / source file.
    FileCode,
    /// Image file.
    FileImage,
    /// Text file.
    FileText,
    /// Extract / download action.
    Download,
    /// Settings gear.
    Settings,
    /// Encrypted entry marker.
    Lock,
    /// Password prompt.
    Key,
    /// Tree collapse / breadcrumb separator.
    ChevronRight,
    /// Tree expand indicator.
    ChevronDown,
    /// Go back one level.
    CornerUpLeft,
    /// Empty state / package.
    Package,
}

/// Path data for [`Icon`], in the order they should be drawn.
fn icon_paths(icon: Icon) -> &'static [&'static str] {
    use Icon::*;
    match icon {
        Folder => &[
            "M20 20a2 2 0 0 0 2-2V8a2 2 0 0 0-2-2h-7.9a2 2 0 0 1-1.69-.9L9.6 3.9A2 2 0 0 0 7.93 3H4a2 2 0 0 0-2 2v13a2 2 0 0 0 2 2Z",
        ],
        FolderOpen => &[
            "m6 14 1.45-2.9A2 2 0 0 1 9.24 10H20a2 2 0 0 1 1.94 2.5l-1.55 6a2 2 0 0 1-1.94 1.5H4a2 2 0 0 1-2-2V5c0-1.1.9-2 2-2h3.93a2 2 0 0 1 1.66.9l.82 1.2a2 2 0 0 0 1.66.9H18a2 2 0 0 1 2 2v2",
        ],
        File => &[
            "M15 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V7Z",
            "M14 2v4a2 2 0 0 0 2 2h4",
        ],
        FileArchive => &[
            "M10 12v-1",
            "M10 18v-2",
            "M10 7v6",
            "M14 2v4a2 2 0 0 0 2 2h4",
            "M15.5 22H18a2 2 0 0 0 2-2V7l-5-5H6a2 2 0 0 0-2 2v16a2 2 0 0 0 .274 1.01",
            "M10 20a2 2 0 1 0 0-4 2 2 0 0 0 0 4Z",
        ],
        FileCode => &[
            "M15 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V7Z",
            "M14 2v4a2 2 0 0 0 2 2h4",
            "m10 13-2 2 2 2",
            "m14 17 2-2-2-2",
        ],
        FileImage => &[
            "M15 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V7Z",
            "M14 2v4a2 2 0 0 0 2 2h4",
            "M10 12a2 2 0 1 0 0-.01",
            "m20 16-1.9-1.9a2 2 0 0 0-2.83 0L14 15.3",
        ],
        FileText => &[
            "M15 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V7Z",
            "M14 2v4a2 2 0 0 0 2 2h4",
            "M10 9H8",
            "M16 13H8",
            "M16 17H8",
        ],
        Download => &[
            "M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4",
            "m7 10 5 5 5-5",
            "M12 15V3",
        ],
        Settings => &[
            "M12.22 2h-.44a2 2 0 0 0-2 2v.18a2 2 0 0 1-1 1.73l-.43.25a2 2 0 0 1-2 0l-.15-.08a2 2 0 0 0-2.73.73l-.22.38a2 2 0 0 0 .73 2.73l.15.1a2 2 0 0 1 1 1.72v.51a2 2 0 0 1-1 1.74l-.15.09a2 2 0 0 0-.73 2.73l.22.38a2 2 0 0 0 2.73.73l.15-.08a2 2 0 0 1 2 0l.43.25a2 2 0 0 1 1 1.73V20a2 2 0 0 0 2 2h.44a2 2 0 0 0 2-2v-.18a2 2 0 0 1 1-1.73l.43-.25a2 2 0 0 1 2 0l.15.08a2 2 0 0 0 2.73-.73l.22-.39a2 2 0 0 0-.73-2.73l-.15-.08a2 2 0 0 1-1-1.74v-.5a2 2 0 0 1 1-1.74l.15-.09a2 2 0 0 0 .73-2.73l-.22-.38a2 2 0 0 0-2.73-.73l-.15.08a2 2 0 0 1-2 0l-.43-.25a2 2 0 0 1-1-1.73V4a2 2 0 0 0-2-2z",
            "M12 9a3 3 0 1 0 0 6 3 3 0 0 0 0-6Z",
        ],
        Lock => &["M7 11V7a5 5 0 0 1 10 0v4", "M3 11h18v10H3Z"],
        Key => &[
            "M2.586 17.414A2 2 0 0 0 2 18.828V21a1 1 0 0 0 1 1h3a1 1 0 0 0 1-1v-1a1 1 0 0 1 1-1h1a1 1 0 0 0 1-1v-1a1 1 0 0 1 1-1h.172a2 2 0 0 0 1.414-.586l.814-.814a6.5 6.5 0 1 0-4-4z",
            "M16.5 8.5a.5.5 0 1 0 0-1 0.5.5 0 0 0 0 1Z",
        ],
        ChevronRight => &["m9 18 6-6-6-6"],
        ChevronDown => &["m6 9 6 6 6-6"],
        CornerUpLeft => &["M9 14 4 9l5-5", "M20 20v-7a4 4 0 0 0-4-4H4"],
        Package => &[
            "M21 8a2 2 0 0 0-1-1.73l-7-4a2 2 0 0 0-2 0l-7 4A2 2 0 0 0 3 8v8a2 2 0 0 0 1 1.73l7 4a2 2 0 0 0 2 0l7-4A2 2 0 0 0 21 16Z",
            "m3.3 7 8.7 5 8.7-5",
            "M12 22V12",
        ],
    }
}

/// Render one icon as an inline SVG.
///
/// `size` is the rendered width/height in px (default 16). `class` is
/// appended to the base `icon` class so callers can tint icons (e.g. type
/// colours in the file list).
#[component]
pub fn IconView(icon: Icon, size: Option<u32>, class: Option<String>) -> Element {
    let size = size.unwrap_or(16);
    let class = class
        .map(|c| format!("icon {c}"))
        .unwrap_or_else(|| "icon".to_string());
    rsx! {
        svg {
            class: "{class}",
            xmlns: "http://www.w3.org/2000/svg",
            width: "{size}",
            height: "{size}",
            view_box: "0 0 24 24",
            fill: "none",
            stroke: "currentColor",
            stroke_width: "1.5",
            stroke_linecap: "round",
            stroke_linejoin: "round",
            for p in icon_paths(icon) {
                path { d: p }
            }
        }
    }
}
