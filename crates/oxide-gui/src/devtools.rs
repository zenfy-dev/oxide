//! Debug-build-only helpers for capturing screenshots of the app itself
//! (used for documentation and visual checks; compiled out of release builds).
//!
//! `OXIDE_SCREENSHOT=out.bmp` saves the window after a few frames and exits.
//! `OXIDE_DEMO=create|extract|select|test|preview|quit` opens a dialog, selects rows or starts a test;
//! `OXIDE_PREVIEW_ENTRY=path` picks an exact stored file path for the preview demo;
//! `OXIDE_THEME=light|dark` forces a theme; `OXIDE_PASSWORD` answers the password prompt;
//! `OXIDE_RECENT=a.zip:b.7z` seeds the recent list.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

static TEST_STARTED: AtomicBool = AtomicBool::new(false);
static RECENT_SEEDED: AtomicBool = AtomicBool::new(false);
static PREVIEW_STARTED: AtomicBool = AtomicBool::new(false);

use eframe::egui;

use crate::app::OxideApp;

pub struct Capture {
    path: PathBuf,
    frames_left: u32,
    requested: bool,
}

pub fn from_env(ctx: &egui::Context) -> Option<Capture> {
    match std::env::var("OXIDE_THEME").as_deref() {
        Ok("light") => ctx.set_theme(egui::ThemePreference::Light),
        Ok("dark") => ctx.set_theme(egui::ThemePreference::Dark),
        _ => {}
    }
    let path = std::env::var_os("OXIDE_SCREENSHOT")?;
    Some(Capture { path: path.into(), frames_left: 20, requested: false })
}

pub fn apply_demo(app: &mut OxideApp, ctx: &egui::Context) {
    // Keep the toolbar's theme icon in step with a forced theme.
    match std::env::var("OXIDE_THEME").as_deref() {
        Ok("light") if app.theme != egui::ThemePreference::Light => app.set_theme(ctx, egui::ThemePreference::Light),
        Ok("dark") if app.theme != egui::ThemePreference::Dark => app.set_theme(ctx, egui::ThemePreference::Dark),
        _ => {}
    }
    if let Ok(list) = std::env::var("OXIDE_RECENT")
        && !RECENT_SEEDED.swap(true, Ordering::Relaxed)
    {
        app.recent = std::env::split_paths(&list).collect();
    }
    if app.password_prompt.is_some()
        && let Ok(pw) = std::env::var("OXIDE_PASSWORD")
    {
        app.password_prompt = None;
        app.retry_with_password(ctx, pw.into());
    }
    match std::env::var("OXIDE_DEMO").as_deref() {
        Ok("create") if app.create.is_none() && app.archive.is_some() => {
            let p = app.archive.as_ref().map(|a| a.info.path.with_extension("")).unwrap_or_default();
            app.open_create_dialog(vec![p]);
        }
        Ok("extract") if app.extract.is_none() && app.archive.is_some() && !app.busy() => {
            app.open_extract_dialog(false);
        }
        Ok("test") if app.archive.is_some() && !app.busy() && !TEST_STARTED.swap(true, Ordering::Relaxed) => {
            app.test_archive(ctx);
        }
        Ok("preview") if app.archive.is_some() && !app.busy() && !PREVIEW_STARTED.swap(true, Ordering::Relaxed) => {
            let path = std::env::var("OXIDE_PREVIEW_ENTRY").ok().or_else(|| {
                app.archive
                    .as_ref()?
                    .info
                    .entries
                    .iter()
                    .find(|e| e.kind == oxide_core::EntryKind::File)
                    .map(|e| e.path.clone())
            });
            if let Some(path) = path {
                app.open_entry(ctx, &path, crate::file_access::FileIntent::Preview);
            }
        }
        // Closes the window normally once an archive is open, so settings are saved.
        Ok("quit") if app.archive.is_some() && !app.busy() => {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        Ok("select") if app.archive.as_ref().is_some_and(|a| a.selection.is_empty() && !a.rows.is_empty()) => {
            if let Some(a) = &mut app.archive {
                let rows: Vec<String> = a.rows.iter().skip(1).take(2).map(|r| r.path.clone()).collect();
                a.selection.extend(rows);
            }
        }
        _ => {}
    }
}

impl Capture {
    pub fn tick(&mut self, ctx: &egui::Context) {
        if self.frames_left > 0 {
            self.frames_left -= 1;
            ctx.request_repaint();
        } else if !self.requested {
            self.requested = true;
            let path = self.path.clone();
            let done = ctx.clone();
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::ScreenshotCallback::new(
                move |image: Arc<egui::ColorImage>| {
                    if let Err(e) = write_bmp(&path, &image) {
                        eprintln!("screenshot failed: {e}");
                    }
                    // Exit normally so preview copies and textures are dropped.
                    done.send_viewport_cmd(egui::ViewportCommand::Close);
                },
            )));
            ctx.request_repaint();
        }
    }
}

fn write_bmp(path: &std::path::Path, image: &egui::ColorImage) -> std::io::Result<()> {
    let [w, h] = image.size;
    let mut out = Vec::with_capacity(54 + w * h * 4);
    let data_len = (w * h * 4) as u32;
    out.extend(b"BM");
    out.extend((54 + data_len).to_le_bytes());
    out.extend([0u8; 4]);
    out.extend(54u32.to_le_bytes());
    out.extend(40u32.to_le_bytes());
    out.extend((w as i32).to_le_bytes());
    out.extend((-(h as i32)).to_le_bytes()); // top-down rows
    out.extend(1u16.to_le_bytes());
    out.extend(32u16.to_le_bytes());
    out.extend([0u8; 4]);
    out.extend(data_len.to_le_bytes());
    out.extend([0u8; 16]);
    for px in &image.pixels {
        let [r, g, b, a] = px.to_srgba_unmultiplied();
        out.extend([b, g, r, a]);
    }
    std::fs::write(path, out)
}
