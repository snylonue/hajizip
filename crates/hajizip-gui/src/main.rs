//! `hajizip-gui` — the Dioxus desktop front-end for the hajizip archive tool.
//!
//! The whole crate forbids `unsafe` (see `AGENTS.md`). The underlying windowing
//! stack (wry / tao) is framework-level and outside this crate's source.

#![windows_subsystem = "windows"]
#![forbid(unsafe_code)]

mod app;
mod config;
mod controller;
mod registry;
mod ui;
mod viewmodel;

use dioxus::desktop::{Config, WindowBuilder};
use dioxus::prelude::*;

fn main() {
    dioxus::LaunchBuilder::new()
        .with_cfg(desktop! {
            Config::new().with_window(
                WindowBuilder::new()
                    .with_title("hajizip")
                    .with_inner_size(dioxus::desktop::LogicalSize::new(900.0, 600.0))
            )
        })
        .launch(app::App);
}
