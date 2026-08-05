//! User gestures → controller intents.
//!
//! Every interactive action (open, extract, navigate, settings, preview) is
//! built here from the [`ControllerHandle`]. Each function returns a `'static`
//! closure suitable for a Dioxus event prop or component callback, so the root
//! component only wires them into the view — no gesture logic lives in
//! `app.rs` and none of it touches signals.
//!
//! Native dialogs use `rfd` (research: `local-doc/research-rfd.md`). rfd's
//! synchronous API blocks its calling thread, so dialogs are spawned on
//! worker threads and the picked path is forwarded through the command
//! channel — the UI thread never blocks.

use std::path::PathBuf;

use dioxus::prelude::*;
use hajizip_core::{EntryPath, FilenameEncoding, OverwritePolicy};

use crate::controller::{ControllerHandle, Intent};

/// Open button: pick an archive via a native file dialog and open it.
pub fn open_dialog(handle: &ControllerHandle) -> impl Fn(MouseEvent) + 'static {
    let handle = handle.clone();
    move |_| {
        let handle = handle.clone();
        std::thread::spawn(move || {
            if let Some(path) = rfd::FileDialog::new().set_title("Open archive").pick_file() {
                let _ = handle.commands.send(Intent::Open {
                    path,
                    password: None,
                });
            }
        });
    }
}

/// Open a specific archive path (drag & drop, recent-files list).
pub fn open_path(handle: &ControllerHandle) -> impl Fn(PathBuf) + 'static {
    let handle = handle.clone();
    move |path| {
        let _ = handle.commands.send(Intent::Open {
            path,
            password: None,
        });
    }
}

/// Extract button: pick a destination folder and extract `selection` (an
/// empty selection extracts the whole archive).
pub fn extract_all(
    handle: &ControllerHandle,
    selection: Vec<EntryPath>,
) -> impl Fn(MouseEvent) + 'static {
    let handle = handle.clone();
    move |_| {
        let handle = handle.clone();
        let selection = selection.clone();
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
}

/// Cancel the in-flight extraction (no worker round trip).
pub fn cancel(handle: &ControllerHandle) -> impl Fn(MouseEvent) + 'static {
    let handle = handle.clone();
    move |_| handle.cancel()
}

/// Enter a directory or nested archive (file list / tree double-click).
pub fn enter(handle: &ControllerHandle) -> impl Fn(EntryPath) + 'static {
    let handle = handle.clone();
    move |path| {
        let _ = handle.commands.send(Intent::Enter { path });
    }
}

/// Preview a plain file entry: extract it to temp and open it externally.
pub fn preview(handle: &ControllerHandle) -> impl Fn(EntryPath) + 'static {
    let handle = handle.clone();
    move |path| {
        let _ = handle.commands.send(Intent::Preview { path });
    }
}

/// Jump to a breadcrumb segment.
pub fn jump_to(handle: &ControllerHandle) -> impl Fn(usize) + 'static {
    let handle = handle.clone();
    move |depth| {
        let _ = handle.commands.send(Intent::JumpTo { depth });
    }
}

/// Go up one level (parent directory or outer archive).
pub fn back(handle: &ControllerHandle) -> impl Fn(MouseEvent) + 'static {
    let handle = handle.clone();
    move |_| {
        let _ = handle.commands.send(Intent::Back);
    }
}

/// Change the filename decoding strategy.
pub fn set_encoding(handle: &ControllerHandle) -> impl Fn(FilenameEncoding) + 'static {
    let handle = handle.clone();
    move |enc| {
        let _ = handle.commands.send(Intent::SetEncoding(enc));
    }
}

/// Change the overwrite policy used by extraction.
pub fn set_overwrite(handle: &ControllerHandle) -> impl Fn(OverwritePolicy) + 'static {
    let handle = handle.clone();
    move |policy| {
        let _ = handle.commands.send(Intent::SetOverwrite(policy));
    }
}

/// Toggle whether extraction preserves entry modification times.
pub fn set_mtime(handle: &ControllerHandle) -> impl Fn(bool) + 'static {
    let handle = handle.clone();
    move |value| {
        let _ = handle.commands.send(Intent::SetPreserveMtime(value));
    }
}

/// Restore all settings to their defaults (settings panel action).
pub fn restore_defaults(handle: &ControllerHandle) -> impl Fn(()) + 'static {
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
}

/// Submit a password for the pending encrypted archive (password dialog).
pub fn submit_password(handle: &ControllerHandle) -> impl Fn((PathBuf, String)) + 'static {
    let handle = handle.clone();
    move |(path, password)| {
        let _ = handle.commands.send(Intent::Open {
            path,
            password: Some(password),
        });
    }
}
