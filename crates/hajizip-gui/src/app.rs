//! The Dioxus root component.
//!
//! The component is intentionally thin: it owns the view state (a
//! [`ViewState`](crate::events::ViewState) bundle of signals), wires them to
//! the background [`Controller`](crate::controller) and renders what it is
//! told. All archive logic lives in the controller, which is tested without
//! any UI (see `test-plan.md` §11).
//!
//! The body is assembled from three parts:
//! * [`spawn_event_loop`](crate::events::spawn_event_loop) — the background
//!   controller wiring and the event → view-state folding (see `events.rs`);
//! * [`actions`](crate::actions) — user gestures turned into controller
//!   intents (native dialogs run on worker threads, the UI thread never
//!   blocks; research: `local-doc/research-rfd.md`);
//! * [`view`](crate::view) — the presentational sections (header, browser,
//!   empty state, status bar, modals).

use std::sync::Arc;

use dioxus::desktop::tao::event::Event as TaoEvent;
use dioxus::desktop::tao::keyboard::KeyCode;
use dioxus::desktop::{WindowEvent, use_wry_event_handler};
use dioxus::html::HasFileData;
use dioxus::prelude::*;
use hajizip_core::EntryPath;

use crate::actions;
use crate::controller::{ControllerHandle, spawn_controller};
use crate::events::{ViewState, spawn_event_loop};
use crate::registry::compose_registry;
use crate::ui::CSS;
use crate::view::{AppHeader, BrowserPane, EmptyState, Modals, StatusBar};

/// The application root component.
#[component]
pub fn App() -> Element {
    // View state (updated on the reactive thread only; see `events.rs`).
    let mut state = ViewState::new();

    // Window handle: dynamic title on open, close for the Exit action.
    let window = dioxus::desktop::use_window();

    // Esc closes any open modal (settings / password). Window-level keyboard
    // events arrive through wry; the handler runs on the main thread.
    use_wry_event_handler(move |event, _target| {
        if let TaoEvent::WindowEvent {
            event: WindowEvent::KeyboardInput { event: key, .. },
            ..
        } = event
            && key.physical_key == KeyCode::Escape
        {
            state.settings_open.set(false);
            state.about_open.set(false);
            state.password_prompt.set(None);
        }
    });

    // Create the background controller once and start draining its events
    // into the view state (the event loop lives in `events.rs`).
    // Load the config once: `ViewState::new` already initialized the signal,
    // and the controller starts from that same snapshot (its `ConfigChanged`
    // events take over from there) — no second config-file read at startup.
    let initial_config = state.config.read().clone();
    let handle = use_hook(|| -> ControllerHandle {
        let registry = Arc::new(compose_registry());
        let (handle, events) = spawn_controller(registry, initial_config.clone());
        spawn_event_loop(&handle, events, window.clone(), state);
        handle
    });

    let has_archive_value = *state.has_archive.read();
    let open_path = actions::open_path(&handle);

    rsx! {
        // Global stylesheet (injected once into the WebView document head).
        style { {CSS} }

        div {
            class: "app",
            ondragover: move |evt| {
                evt.prevent_default();
                state.drag_over.set(true);
            },
            ondragleave: move |_| state.drag_over.set(false),
            ondrop: move |evt| {
                evt.prevent_default();
                state.drag_over.set(false);
                if let Some(file) = evt.files().first() {
                    let path = file.path();
                    if !path.as_os_str().is_empty() {
                        open_path(path);
                    }
                }
            },

            // ── Header ─────────────────────────────────────────────────
            AppHeader {
                search_query: state.search_query,
                has_archive: has_archive_value,
                selection: state.selection,
                on_open: actions::open_dialog(&handle),
                on_extract: actions::extract_all(&handle, selected_paths(&state)),
                on_settings: move |_| state.settings_open.set(true),
                on_about: move |_| state.about_open.set(true),
                on_exit: move |_| window.close(),
            }

            // ── Main content ────────────────────────────────────────────
            if has_archive_value {
                BrowserPane {
                    breadcrumb: state.breadcrumb,
                    focus: state.focus,
                    entries: state.entries,
                    expanded: state.expanded,
                    selection: state.selection,
                    query: state.search_query.read().clone(),
                    config: state.config,
                    banner_dismissed: state.banner_dismissed,
                    on_back: actions::back(&handle),
                    on_jump: actions::jump_to(&handle),
                    on_enter_tree: actions::enter(&handle),
                    on_enter_list: actions::enter(&handle),
                    on_preview: actions::preview(&handle),
                    on_encoding: actions::set_encoding(&handle),
                    on_dismiss: move |_| state.banner_dismissed.set(true),
                }
            } else {
                EmptyState {
                    drag_over: state.drag_over,
                    config: state.config,
                    on_open_path: actions::open_path(&handle),
                }
            }

            // ── Status bar ──────────────────────────────────────────────
            StatusBar {
                has_archive: has_archive_value,
                status: state.status,
                entries: state.entries,
                selection: state.selection,
                progress: state.progress,
                on_cancel: actions::cancel(&handle),
            }

            // ── Modal dialogs ───────────────────────────────────────────
            Modals {
                password_prompt: state.password_prompt,
                settings_open: state.settings_open,
                about_open: state.about_open,
                config: state.config,
                on_password: actions::submit_password(&handle),
                on_encoding: actions::set_encoding(&handle),
                on_overwrite: actions::set_overwrite(&handle),
                on_mtime: actions::set_mtime(&handle),
                on_defaults: actions::restore_defaults(&handle),
            }
        }
    }
}

/// Snapshot of the currently selected paths, taken at render time.
///
/// The Extract button captures this snapshot so the dialog opens with the
/// selection the user actually saw; an empty selection means "whole archive".
fn selected_paths(state: &ViewState) -> Vec<EntryPath> {
    state.selection.read().iter().cloned().collect()
}
