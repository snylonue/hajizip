//! The Dioxus root component and view state.
//!
//! The component is intentionally thin: it owns a few signals for display,
//! wires them to the background [`Controller`](crate::controller) and renders
//! what it is told. All archive logic lives in the controller, which is tested
//! without any UI (see `test-plan.md` §11).

use std::path::PathBuf;
use std::sync::Arc;

use dioxus::prelude::*;
use hajizip_core::EntryMeta;

use crate::config::AppConfig;
use crate::controller::{ControllerHandle, Event, Intent, spawn_controller};
use crate::registry::compose_registry;

/// Format an optional byte size for display.
fn size_label(entry: &EntryMeta) -> String {
    match entry.uncompressed_size {
        Some(bytes) => format!("{bytes} B"),
        None => "—".to_string(),
    }
}

/// The application root component.
#[component]
pub fn App() -> Element {
    // View state (updated on the reactive thread only).
    let mut status = use_signal(|| "No archive open.".to_string());
    let mut archive_name = use_signal(String::new);
    let mut entries = use_signal(Vec::<EntryMeta>::new);
    let mut path_input = use_signal(String::new);

    // Create the background controller once and start draining its events.
    let handle = use_hook(|| -> ControllerHandle {
        let registry = Arc::new(compose_registry());
        let (handle, mut events) = spawn_controller(registry, AppConfig::default());

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
                        status.set("Archive opened.".to_string());
                    }
                    Event::PasswordRequired { path } => {
                        status.set(format!("Password required to open {}.", path.display()));
                    }
                    Event::WrongPassword { path } => {
                        status.set(format!("Wrong password for {}.", path.display()));
                    }
                    Event::Unsupported(message) => {
                        status.set(format!("Not supported yet: {message}"));
                    }
                    Event::Error(message) => {
                        status.set(format!("Error: {message}"));
                    }
                    Event::Cancelled => {
                        status.set("Operation cancelled.".to_string());
                    }
                    // Progress / Done arrive once extraction is implemented.
                    Event::Progress(_) | Event::Done(_) => {}
                }
            }
        });

        handle
    });

    let open = move |_| {
        let path = PathBuf::from(path_input.read().clone());
        let _ = handle.commands.send(Intent::Open {
            path,
            password: None,
        });
    };

    rsx! {
        div {
            style: "font-family: sans-serif; padding: 16px;",
            h1 { "hajizip" }
            p { style: "color: #666;", "A memory-safe archive tool (7-Zip alternative)." }

            div {
                style: "display: flex; gap: 8px; margin: 12px 0;",
                input {
                    value: "{path_input}",
                    placeholder: "/path/to/archive.zip",
                    style: "flex: 1; padding: 4px;",
                    oninput: move |event| path_input.set(event.value()),
                }
                button { onclick: open, "Open" }
            }

            p { style: "color: #444;", "{status}" }

            if !archive_name.read().is_empty() {
                h3 { "{archive_name}" }
                ul {
                    for entry in entries.read().iter() {
                        li { "{entry.path} — {size_label(entry)}" }
                    }
                }
            }
        }
    }
}
