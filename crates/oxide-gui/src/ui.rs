//! Main window layout: toolbar, browser, status bar, and modal dialogs.

use std::path::PathBuf;

use eframe::egui::{self, Align, Key, KeyboardShortcut, Layout, Modifiers, RichText};

use crate::app::{OxideApp, file_name};
use crate::components::icons::{self, Icon};
use crate::components::{create_dialog, extract_dialog, file_table, human_bytes, progress};
use crate::file_access::FileIntent;
use crate::theme;

const OPEN: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::O);
const CREATE: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::N);
const EXTRACT: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::E);
const TEST: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::T);
const ADD: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND.plus(Modifiers::SHIFT), Key::A);
const FIND: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::F);
const SELECT_ALL: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::A);
const UP: KeyboardShortcut = KeyboardShortcut::new(Modifiers::ALT, Key::ArrowUp);
const PREVIEW: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::P);

pub fn draw(app: &mut OxideApp, ui: &mut egui::Ui) {
    let ctx = ui.ctx().clone();
    let window = ui.max_rect();
    shortcuts(app, &ctx);

    egui::Panel::top("toolbar")
        .frame(egui::Frame::new().fill(ui.visuals().panel_fill).inner_margin(egui::Margin::symmetric(12, 8)))
        .show(ui, |ui| toolbar(app, ui));
    egui::Panel::bottom("status")
        .frame(egui::Frame::new().fill(ui.visuals().panel_fill).inner_margin(egui::Margin::symmetric(12, 5)))
        .show(ui, |ui| status_bar(app, ui));
    egui::CentralPanel::default_margins().show(ui, |ui| {
        if app.archive.is_some() {
            browser(app, ui);
        } else {
            empty_state(app, ui);
        }
    });

    dialogs(app, &ctx);
    toast(app, &ctx, window);
    drop_zone(app, &ctx, window);
}

fn shortcuts(app: &mut OxideApp, ctx: &egui::Context) {
    // Shortcuts are inert while a dialog or job owns the window.
    if app.busy()
        || app.create.is_some()
        || app.extract.is_some()
        || app.preview.is_some()
        || app.password_prompt.is_some()
        || app.error.is_some()
        || app.drop_choice.is_some()
    {
        return;
    }
    let pressed = |s: &KeyboardShortcut| ctx.input_mut(|i| i.consume_shortcut(s));
    let has_archive = app.archive.is_some();
    if pressed(&OPEN) {
        app.pick_and_open(ctx);
    } else if pressed(&CREATE) {
        app.open_create_dialog(Vec::new());
    } else if has_archive && pressed(&EXTRACT) {
        app.open_extract_dialog(false);
    } else if has_archive && pressed(&TEST) {
        app.test_archive(ctx);
    } else if app.archive.as_ref().is_some_and(|a| a.supports_add()) && pressed(&ADD) {
        app.pick_and_add(ctx);
    } else if has_archive && pressed(&FIND) {
        app.focus_search = true;
    }
    let typing = ctx.egui_wants_keyboard_input();
    let mut open_file = None;
    if let Some(a) = &mut app.archive {
        if !typing && pressed(&SELECT_ALL) {
            a.selection = a.rows.iter().map(|r| r.path.clone()).collect();
        }
        if pressed(&UP) || (!typing && ctx.input(|i| i.key_pressed(Key::Backspace))) {
            a.go_up();
        }
        let step = ctx.input(|i| {
            if i.key_pressed(Key::ArrowDown) {
                1
            } else if i.key_pressed(Key::ArrowUp) && !i.modifiers.alt {
                -1
            } else {
                0
            }
        });
        if !typing && step != 0 && !a.rows.is_empty() {
            let last = a.rows.len() as i64 - 1;
            let next = match a.anchor {
                Some(i) => (i as i64 + step).clamp(0, last),
                None if step > 0 => 0,
                None => last,
            } as usize;
            a.selection.clear();
            a.selection.insert(a.rows[next].path.clone());
            a.anchor = Some(next);
            a.scroll_to = Some(next);
        }
        if !typing && ctx.input(|i| i.key_pressed(Key::Enter)) {
            let target = a.rows.iter().find(|r| a.selection.len() == 1 && a.selection.contains(&r.path));
            if let Some(row) = target {
                if row.is_dir {
                    let path = row.path.clone();
                    a.navigate(path);
                    app.search.clear();
                } else {
                    open_file = row.file_path.clone();
                }
            }
        }
    }
    if let Some(path) = open_file {
        app.open_entry(ctx, &path, FileIntent::Open);
    } else if !typing && (pressed(&PREVIEW) || ctx.input(|i| i.key_pressed(Key::Space) && i.modifiers.is_none())) {
        app.preview_selected(ctx);
    }
    if ctx.input(|i| i.key_pressed(Key::Escape)) && !app.search.is_empty() {
        app.search.clear();
    }
}

fn tool_button(ui: &mut egui::Ui, enabled: bool, icon: Icon, label: &str, tip: String) -> bool {
    icons::button(ui, icon, label, enabled, true)
        .on_hover_text(tip)
        .on_disabled_hover_text(format!("{label}: open an archive first"))
        .clicked()
}

fn toolbar(app: &mut OxideApp, ui: &mut egui::Ui) {
    let ctx = ui.ctx().clone();
    let idle = !app.busy();
    let has = app.archive.is_some() && idle;
    let can_add = has && app.archive.as_ref().is_some_and(|a| a.supports_add());
    let can_preview = has && app.archive.as_ref().and_then(|a| a.selected_file()).is_some();
    let sc = |s: &KeyboardShortcut| ctx.format_shortcut(s);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        if tool_button(ui, idle, Icon::Open, "Open", format!("Open an archive ({})", sc(&OPEN))) {
            app.pick_and_open(&ctx);
        }
        if tool_button(ui, idle, Icon::Create, "Create", format!("Create a new archive ({})", sc(&CREATE))) {
            app.open_create_dialog(Vec::new());
        }
        ui.add_space(4.0);
        ui.separator();
        ui.add_space(4.0);
        if tool_button(ui, has, Icon::Extract, "Extract", format!("Extract files ({})", sc(&EXTRACT))) {
            app.open_extract_dialog(false);
        }
        let add_tip = if has && !can_add {
            "Adding files is supported for ZIP and TAR archives".to_owned()
        } else {
            format!("Add files to this archive ({})", sc(&ADD))
        };
        if tool_button(ui, can_add, Icon::Add, "Add", add_tip) {
            app.pick_and_add(&ctx);
        }
        if tool_button(ui, has, Icon::Test, "Test", format!("Verify the archive's integrity ({})", sc(&TEST))) {
            app.test_archive(&ctx);
        }
        if tool_button(
            ui,
            can_preview,
            Icon::Preview,
            "Preview",
            format!("Preview selected file (Space / {})", sc(&PREVIEW)),
        ) {
            app.preview_selected(&ctx);
        }

        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let (icon, next, tip) = match app.theme {
                egui::ThemePreference::System => (Icon::System, egui::ThemePreference::Light, "Theme: follow system"),
                egui::ThemePreference::Light => (Icon::Sun, egui::ThemePreference::Dark, "Theme: light"),
                egui::ThemePreference::Dark => (Icon::Moon, egui::ThemePreference::System, "Theme: dark"),
            };
            if icons::button(ui, icon, "", true, false).on_hover_text(format!("{tip} (click to change)")).clicked() {
                app.set_theme(&ctx, next);
            }
        });
    });
}

fn browser(app: &mut OxideApp, ui: &mut egui::Ui) {
    let Some(archive) = app.archive.as_mut() else {
        return;
    };
    // Navigation row: back / up, breadcrumbs, search.
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        if icons::button(ui, Icon::Back, "", !archive.back.is_empty(), false).on_hover_text("Back").clicked() {
            archive.go_back();
        }
        if icons::button(ui, Icon::Up, "", !archive.cwd.is_empty(), false)
            .on_hover_text("Enclosing folder (Backspace)")
            .clicked()
        {
            archive.go_up();
        }
        ui.add_space(4.0);
        let mut target = None;
        let root = file_name(&archive.info.path);
        if ui.add(egui::Button::new(RichText::new(root).strong()).frame(false)).clicked() {
            target = Some(String::new());
        }
        let mut acc = String::new();
        for part in archive.cwd.split('/').filter(|p| !p.is_empty()) {
            ui.label(RichText::new("›").color(theme::weak(ui)));
            acc = if acc.is_empty() { part.to_owned() } else { format!("{acc}/{part}") };
            if ui.add(egui::Button::new(part).frame(false)).clicked() {
                target = Some(acc.clone());
            }
        }
        if let Some(t) = target {
            archive.navigate(t);
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            search_field(ui, &mut app.search, &mut app.focus_search);
        });
    });
    ui.add_space(4.0);

    archive.refresh_rows(&app.search);
    if archive.rows.is_empty() {
        ui.centered_and_justified(|ui| {
            let msg = if app.search.is_empty() {
                "This folder is empty".to_owned()
            } else {
                format!("Nothing in this archive matches “{}”", app.search.trim())
            };
            ui.label(RichText::new(msg).color(theme::weak(ui)));
        });
        return;
    }
    let events = file_table::show(
        ui,
        &archive.rows,
        &mut archive.selection,
        &mut archive.anchor,
        archive.sort,
        archive.ascending,
        archive.scroll_to.take(),
    );
    if let Some(key) = events.sort {
        if archive.sort == key {
            archive.ascending = !archive.ascending;
        } else {
            archive.sort = key;
            archive.ascending = true;
        }
    }
    if let Some(dir) = events.open_dir {
        app.search.clear();
        archive.navigate(dir);
    }
    if events.extract_selection {
        app.open_extract_dialog(true);
    }
    let ctx = ui.ctx().clone();
    if let Some(path) = events.open_file {
        app.open_entry(&ctx, &path, FileIntent::Open);
    } else if let Some(path) = events.preview_file {
        app.open_entry(&ctx, &path, FileIntent::Preview);
    }
}

/// A search box with a leading icon and a clear button.
fn search_field(ui: &mut egui::Ui, text: &mut String, focus: &mut bool) {
    // A fixed-size, left-to-right slot inside the right-to-left navigation row.
    ui.allocate_ui_with_layout(egui::vec2(250.0, 30.0), Layout::left_to_right(Align::Center), |ui| {
        search_frame(ui, text, focus);
    });
}

fn search_frame(ui: &mut egui::Ui, text: &mut String, focus: &mut bool) {
    egui::Frame::new()
        .fill(ui.visuals().extreme_bg_color)
        .stroke(ui.visuals().widgets.inactive.bg_stroke)
        .corner_radius(8)
        .inner_margin(egui::Margin::symmetric(8, 2))
        .show(ui, |ui| search_contents(ui, text, focus));
}

fn search_contents(ui: &mut egui::Ui, text: &mut String, focus: &mut bool) {
    {
        {
            ui.spacing_mut().item_spacing.x = 6.0;
            ui.set_width(232.0);
            let weak = theme::weak(ui);
            icons::show(ui, Icon::Search, weak);
            let edit = egui::TextEdit::singleline(text)
                .hint_text("Search in archive")
                .frame(egui::Frame::NONE)
                .align(egui::Align2::LEFT_CENTER)
                .min_size(egui::vec2(0.0, 24.0))
                .desired_width(176.0);
            let response =
                ui.add(edit).on_hover_text("Search full paths; separate words to match each anywhere in the path.");
            if *focus {
                response.request_focus();
                *focus = false;
            }
            if !text.is_empty()
                && icons::button(ui, Icon::Close, "", true, false).on_hover_text("Clear (Esc)").clicked()
            {
                text.clear();
            }
        }
    }
}

fn empty_state(app: &mut OxideApp, ui: &mut egui::Ui) {
    let ctx = ui.ctx().clone();
    let weak = theme::weak(ui);
    let icon = app
        .icon
        .get_or_insert_with(|| ctx.load_texture("app-icon", theme::icon_image(), egui::TextureOptions::LINEAR))
        .id();
    let recent: Vec<PathBuf> = app.recent.iter().filter(|p| p.is_file()).cloned().collect();
    let content_height = if recent.is_empty() { 254.0 } else { 324.0 + 34.0 * recent.len() as f32 };

    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        ui.vertical_centered(|ui| {
            ui.add_space(((ui.available_height() - content_height) / 2.0).max(24.0));
            ui.add(egui::Image::new(egui::load::SizedTexture::new(icon, egui::vec2(96.0, 96.0))));
            ui.add_space(10.0);
            ui.label(RichText::new("Drop an archive to open it").heading());
            ui.label(RichText::new("or drop files and folders to compress them").color(weak));
            ui.add_space(16.0);
            ui.horizontal(|ui| {
                ui.add_space((ui.available_width() - 262.0).max(0.0) / 2.0);
                if ui.add(theme::primary_button("Open archive…").min_size(egui::vec2(124.0, 32.0))).clicked() {
                    app.pick_and_open(&ctx);
                }
                if ui.add(egui::Button::new("Create archive…").min_size(egui::vec2(124.0, 32.0))).clicked() {
                    app.open_create_dialog(Vec::new());
                }
            });

            if recent.is_empty() {
                return;
            }
            ui.add_space(30.0);
            let width = 440.0_f32.min(ui.available_width() - 32.0);
            ui.allocate_ui_with_layout(egui::vec2(width, 0.0), Layout::top_down(Align::Min), |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Recent").strong());
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.add(egui::Button::new(RichText::new("Clear").small().color(weak)).frame(false)).clicked()
                        {
                            app.recent.clear();
                        }
                    });
                });
                ui.add_space(4.0);
                for path in &recent {
                    if recent_row(ui, path, width).clicked() {
                        app.open_path(&ctx, path.clone());
                    }
                }
            });
        });
    });
}

/// One clickable line in the recent-archives list.
fn recent_row(ui: &mut egui::Ui, path: &std::path::Path, width: f32) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, 34.0), egui::Sense::click());
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        if response.hovered() {
            painter.rect_filled(rect, 6.0, ui.visuals().widgets.hovered.weak_bg_fill);
        }
        let text = ui.visuals().text_color();
        let weak = theme::weak(ui);
        let icon_rect =
            egui::Rect::from_center_size(egui::pos2(rect.left() + 16.0, rect.center().y), egui::Vec2::splat(16.0));
        icons::paint(painter, icon_rect, Icon::Archive, weak);
        let name = painter.layout_no_wrap(file_name(path), egui::FontId::proportional(14.0), text);
        let name_w = name.size().x;
        painter.galley(egui::pos2(rect.left() + 34.0, rect.center().y - name.size().y / 2.0), name, text);
        let max = (rect.width() - name_w - 60.0).max(40.0);
        let folder = short_folder(painter, path, egui::FontId::proportional(12.0), weak, max);
        let pos = egui::pos2(rect.right() - 10.0 - folder.size().x, rect.center().y - folder.size().y / 2.0);
        painter.galley(pos, folder, weak);
    }
    response.on_hover_text(path.display().to_string()).on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// The folder containing `path` on one line: home shown as `~`, and the
/// start trimmed with `…` until it fits in `max_width`.
fn short_folder(
    painter: &egui::Painter,
    path: &std::path::Path,
    font: egui::FontId,
    color: egui::Color32,
    max_width: f32,
) -> std::sync::Arc<egui::Galley> {
    let parent = path.parent().unwrap_or(path);
    let mut text = match std::env::home_dir().and_then(|h| parent.strip_prefix(h).ok().map(|r| r.to_path_buf())) {
        Some(rel) if rel.as_os_str().is_empty() => "~".to_owned(),
        Some(rel) => format!("~/{}", rel.display()),
        None => parent.display().to_string(),
    };
    loop {
        let galley = painter.layout_no_wrap(text.clone(), font.clone(), color);
        let chars = text.chars().count();
        if galley.size().x <= max_width || chars <= 4 {
            return galley;
        }
        // Drop roughly the overflowing share of characters from the front.
        let keep = ((chars as f32 * max_width / galley.size().x) as usize).saturating_sub(2).max(3).min(chars - 2);
        let tail: String = text.chars().skip(chars - keep).collect();
        text = format!("…{}", tail.trim_start_matches('…'));
    }
}

fn status_bar(app: &OxideApp, ui: &mut egui::Ui) {
    let weak = theme::weak(ui);
    ui.horizontal(|ui| {
        let Some(a) = &app.archive else {
            ui.label(RichText::new(format!("Oxide {}", env!("CARGO_PKG_VERSION"))).small().color(weak));
            return;
        };
        let info = &a.info;
        let folders = a.tree.dir_count();
        let mut text = format!(
            "{} archive  ·  {} files, {} folders  ·  {} uncompressed, {} on disk",
            info.format,
            a.stats.files,
            folders,
            human_bytes(a.stats.total_size),
            human_bytes(info.archive_size)
        );
        // Below 1% the saving is noise; near 100% "saves 100%" would be untrue.
        match a.stats.compression_ratio.map(|r| (1.0 - r) * 100.0) {
            Some(saved) if saved >= 99.5 => text.push_str(" (saves over 99%)"),
            Some(saved) if saved >= 1.0 => text.push_str(&format!(" (saves {saved:.0}%)")),
            _ => {}
        }
        ui.label(RichText::new(text).small().color(weak));
        if a.stats.encrypted {
            ui.add_space(4.0);
            icons::show(ui, Icon::Lock, weak);
            ui.label(RichText::new("Encrypted").small().color(weak));
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let selected = a.selection.len();
            let label = if !app.search.trim().is_empty() {
                format!("{} matches", a.rows.len())
            } else if selected > 0 {
                let bytes: u64 = a.rows.iter().filter(|r| a.selection.contains(&r.path)).map(|r| r.size).sum();
                format!("{selected} selected  ·  {}", human_bytes(bytes))
            } else {
                String::new()
            };
            ui.label(RichText::new(label).small().color(weak));
        });
    });
}

fn dialogs(app: &mut OxideApp, ctx: &egui::Context) {
    let busy = app.busy()
        || app.password_prompt.is_some()
        || app.error.is_some()
        || app.create.is_some()
        || app.extract.is_some()
        || app.drop_choice.is_some()
        || !app.warnings.is_empty();
    if let Some(preview) = &mut app.preview {
        let events = preview.show(ctx, busy);
        let file = events.open.then(|| preview.file.clone());
        if events.close {
            app.preview = None;
        }
        if let Some(file) = file {
            app.open_staged(ctx, file);
        }
    }
    if let Some(dialog) = &mut app.create {
        match dialog.show(ctx) {
            create_dialog::Outcome::Open => {}
            create_dialog::Outcome::Cancel => app.create = None,
            create_dialog::Outcome::Create { inputs, output, options } => {
                app.create = None;
                app.start_create(ctx, inputs, output, options);
            }
        }
    }
    if let Some(dialog) = &mut app.extract {
        match dialog.show(ctx) {
            extract_dialog::Outcome::Open => {}
            extract_dialog::Outcome::Cancel => app.extract = None,
            extract_dialog::Outcome::Extract { archive, dest, options } => {
                app.extract = None;
                if let Some(pw) = &options.password {
                    app.password = Some(pw.clone());
                }
                app.run(ctx, crate::app::Action::Extract { archive, dest, options });
            }
        }
    }
    if let Some(job) = &app.job
        && progress::show(ctx, job)
    {
        job.cancel.cancel();
    }

    password_prompt(app, ctx);
    drop_choice(app, ctx);

    if let Some(message) = app.error.clone() {
        let r = egui::Modal::new(egui::Id::unique("error")).show(ctx, |ui| {
            ui.set_width(420.0);
            ui.heading("Something went wrong");
            ui.add_space(6.0);
            ui.add(egui::Label::new(message).wrap());
            ui.add_space(12.0);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| ui.button("OK").clicked()).inner
        });
        if r.inner || r.should_close() {
            app.error = None;
        }
    } else if !app.warnings.is_empty() && app.job.is_none() {
        let warnings = app.warnings.clone();
        let r = egui::Modal::new(egui::Id::unique("warnings")).show(ctx, |ui| {
            ui.set_width(460.0);
            ui.heading("Finished with warnings");
            ui.add_space(6.0);
            egui::ScrollArea::vertical().max_height(220.0).show(ui, |ui| {
                for w in &warnings {
                    ui.label(format!("• {w}"));
                }
            });
            ui.add_space(12.0);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| ui.button("OK").clicked()).inner
        });
        if r.inner || r.should_close() {
            app.warnings.clear();
        }
    }
}

fn password_prompt(app: &mut OxideApp, ctx: &egui::Context) {
    let Some(prompt) = &mut app.password_prompt else {
        return;
    };
    let mut submit = false;
    let mut cancel = false;
    let r = egui::Modal::new(egui::Id::unique("password")).show(ctx, |ui| {
        ui.set_width(360.0);
        ui.heading("Password required");
        ui.add_space(6.0);
        let msg = if prompt.wrong { "That password didn't work. Try again." } else { "This archive is encrypted." };
        ui.label(RichText::new(msg).color(if prompt.wrong { ui.visuals().warn_fg_color } else { theme::weak(ui) }));
        ui.add_space(8.0);
        let field = ui.add(egui::TextEdit::singleline(&mut prompt.text).password(true).desired_width(f32::INFINITY));
        field.request_focus();
        if field.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
            submit = true;
        }
        ui.add_space(12.0);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if ui.add_enabled(!prompt.text.is_empty(), theme::primary_button("Unlock")).clicked() {
                submit = true;
            }
            if ui.button("Cancel").clicked() {
                cancel = true;
            }
        });
    });
    if submit && !prompt.text.is_empty() {
        let pw = std::mem::take(&mut prompt.text);
        app.password_prompt = None;
        app.retry_with_password(ctx, pw.into());
    } else if cancel || r.should_close() {
        app.password_prompt = None;
        app.last_action = None;
    }
}

fn drop_choice(app: &mut OxideApp, ctx: &egui::Context) {
    let Some(paths) = app.drop_choice.clone() else {
        return;
    };
    let name = app.archive.as_ref().map(|a| file_name(&a.info.path)).unwrap_or_default();
    let mut choice = 0;
    let r = egui::Modal::new(egui::Id::unique("drop_choice")).show(ctx, |ui| {
        ui.set_width(400.0);
        ui.heading(format!("{} item(s) dropped", paths.len()));
        ui.add_space(6.0);
        ui.label(format!("Add them to {name}, or create a new archive?"));
        ui.add_space(12.0);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if ui.add(theme::primary_button(&format!("Add to {name}"))).clicked() {
                choice = 1;
            }
            if ui.button("New archive…").clicked() {
                choice = 2;
            }
            if ui.button("Cancel").clicked() {
                choice = 3;
            }
        });
    });
    match choice {
        1 => {
            app.drop_choice = None;
            app.start_add(ctx, paths);
        }
        2 => {
            app.drop_choice = None;
            app.open_create_dialog(paths);
        }
        3 => app.drop_choice = None,
        _ if r.should_close() => app.drop_choice = None,
        _ => {}
    }
}

fn toast(app: &mut OxideApp, ctx: &egui::Context, window: egui::Rect) {
    let Some(t) = &app.toast else {
        return;
    };
    let now = std::time::Instant::now();
    if now >= t.until {
        app.toast = None;
        return;
    }
    egui::Area::new(egui::Id::unique("toast"))
        .order(egui::Order::Foreground)
        .pivot(egui::Align2::CENTER_BOTTOM)
        .fixed_pos(egui::pos2(window.center().x, window.bottom() - 40.0))
        .interactable(false)
        .show(ctx, |ui| {
            egui::Frame::popup(ui.style()).inner_margin(egui::Margin::symmetric(14, 8)).show(ui, |ui| {
                ui.label(&t.text);
            });
        });
    ctx.request_repaint_after(t.until - now);
}

fn drop_zone(app: &mut OxideApp, ctx: &egui::Context, window: egui::Rect) {
    let hovering = ctx.input(|i| !i.raw.hovered_files.is_empty());
    if hovering {
        let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::unique("drop_zone")));
        let rect = window.shrink(10.0);
        painter.rect(
            rect,
            14.0,
            theme::ACCENT.gamma_multiply(0.12),
            egui::Stroke::new(2.0, theme::ACCENT),
            egui::StrokeKind::Inside,
        );
        let text = if app.archive.as_ref().is_some_and(|a| a.supports_add()) {
            "Drop to open, add, or compress"
        } else {
            "Drop to open or compress"
        };
        painter.text(rect.center(), egui::Align2::CENTER_CENTER, text, egui::FontId::proportional(22.0), theme::ACCENT);
    }
    let dropped: Vec<PathBuf> = ctx.input(|i| {
        i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).filter(|p| !p.as_os_str().is_empty()).collect()
    });
    if !dropped.is_empty() {
        if let Some(dialog) = &mut app.create {
            dialog.add_inputs(dropped);
        } else {
            app.handle_drop(ctx, dropped);
        }
    }
}
