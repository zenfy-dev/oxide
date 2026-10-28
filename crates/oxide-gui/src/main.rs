//! Oxide desktop application.

#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]
#![forbid(unsafe_code)]

mod app;
mod components;
#[cfg(debug_assertions)]
mod devtools;
mod theme;
mod ui;

use std::path::PathBuf;

use eframe::egui;

fn main() -> eframe::Result {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_env("OXIDE_LOG").unwrap_or_else(|_| "warn".into()))
        .init();

    // An archive passed on the command line (e.g. from a file association) is opened at start.
    let initial: Option<PathBuf> = std::env::args_os().nth(1).map(PathBuf::from);

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Oxide")
            .with_app_id("io.github.oxide-archiver.Oxide")
            .with_inner_size([980.0, 640.0])
            .with_min_inner_size([620.0, 400.0])
            .with_drag_and_drop(true)
            .with_icon(theme::app_icon(128)),
        ..Default::default()
    };
    eframe::run_native(
        "Oxide",
        options,
        Box::new(move |cc| {
            theme::install(&cc.egui_ctx);
            Ok(Box::new(app::OxideApp::new(&cc.egui_ctx, initial)))
        }),
    )
}
