//! Main window layout: toolbar, browser, status bar, and modal dialogs.

use std::path::PathBuf;

use eframe::egui::{self, Align, Key, KeyboardShortcut, Layout, Modifiers, RichText};

use crate::app::{OxideApp, file_name};
use crate::components::{create_dialog, extract_dialog, file_table, human_bytes, progress};
use crate::theme;

const OPEN: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::O);
const CREATE: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::N);
const EXTRACT: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::E);
const TEST: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::T);
const ADD: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND.plus(Modifiers::SHIFT), Key::A);
const FIND: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::F);
const SELECT_ALL: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::A);
const UP: KeyboardShortcut = KeyboardShortcut::new(Modifiers::ALT, Key::ArrowUp);

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
    if app.busy() || app.create.is_some() || app.extract.is_some() || app.password_prompt.is_some() {
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
    if let Some(a) = &mut app.archive {
        if !typing && pressed(&SELECT_ALL) {
            a.selection = a.rows.iter().map(|r| r.path.clone()).collect();
        }
        if pressed(&UP) || (!typing && ctx.input(|i| i.key_pressed(Key::Backspace))) {
            a.go_up();
        }
        if !typing && ctx.input(|i| i.key_pressed(Key::Enter)) {
            let target = a.rows.iter().find(|r| r.is_dir && a.selection.len() == 1 && a.selection.contains(&r.path));
            if let Some(dir) = target.map(|r| r.path.clone()) {
                a.navigate(dir);
            }
        }
    }
    if ctx.input(|i| i.key_pressed(Key::Escape)) && !app.search.is_empty() {
        app.search.clear();
    }
}

fn tool_button(ui: &mut egui::Ui, enabled: bool, icon: &str, label: &str, tip: String) -> bool {
    let text = RichText::new(format!("{icon}  {label}"));
    ui.add_enabled(enabled, egui::Button::new(text).min_size(egui::vec2(0.0, 30.0)))
        .on_hover_text(tip)
        .on_disabled_hover_text("Open an archive first")
        .clicked()
}

fn toolbar(app: &mut OxideApp, ui: &mut egui::Ui) {
    let ctx = ui.ctx().clone();
    let idle = !app.busy();
    let has = app.archive.is_some() && idle;
    let can_add = has && app.archive.as_ref().is_some_and(|a| a.supports_add());
    let sc = |s: &KeyboardShortcut| ctx.format_shortcut(s);
    ui.horizontal(|ui| {
        if tool_button(ui, idle, "📂", "Open", format!("Open an archive ({})", sc(&OPEN))) {
            app.pick_and_open(&ctx);
        }
        if tool_button(ui, idle, "🗜", "Create", format!("Create a new archive ({})", sc(&CREATE))) {
            app.open_create_dialog(Vec::new());
        }
        ui.separator();
        if tool_button(ui, has, "⬇", "Extract", format!("Extract files ({})", sc(&EXTRACT))) {
            app.open_extract_dialog(false);
        }
        if tool_button(ui, can_add, "➕", "Add", format!("Add files to this archive ({})", sc(&ADD))) {
            app.pick_and_add(&ctx);
        }
        if tool_button(ui, has, "✔", "Test", format!("Verify the archive's integrity ({})", sc(&TEST))) {
            app.test_archive(&ctx);
        }

        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let (icon, next, tip) = match app.theme {
                egui::ThemePreference::System => ("🖥", egui::ThemePreference::Light, "Theme: follow system"),
                egui::ThemePreference::Light => ("☀", egui::ThemePreference::Dark, "Theme: light"),
                egui::ThemePreference::Dark => ("🌙", egui::ThemePreference::System, "Theme: dark"),
            };
            if ui.add(egui::Button::new(icon).frame(false)).on_hover_text(tip).clicked() {
                app.theme = next;
                ctx.set_theme(next);
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
        if ui.add_enabled(!archive.back.is_empty(), egui::Button::new("⬅").frame(false)).on_hover_text("Back").clicked()
        {
            archive.go_back();
        }
        if ui
            .add_enabled(!archive.cwd.is_empty(), egui::Button::new("⬆").frame(false))
            .on_hover_text("Enclosing folder (Backspace)")
            .clicked()
        {
            archive.go_up();
        }
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
            let search = ui.add(
                egui::TextEdit::singleline(&mut app.search).hint_text("🔍 Search in archive").desired_width(220.0),
            );
            if app.focus_search {
                search.request_focus();
                app.focus_search = false;
            }
        });
    });
    ui.add_space(4.0);

    archive.refresh_rows(&app.search);
    if archive.rows.is_empty() {
        ui.centered_and_justified(|ui| {
            let msg = if app.search.is_empty() { "This folder is empty" } else { "No matches" };
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
}

fn empty_state(app: &mut OxideApp, ui: &mut egui::Ui) {
    let ctx = ui.ctx().clone();
    ui.vertical_centered(|ui| {
        ui.add_space((ui.available_height() / 2.0 - 110.0).max(20.0));
        ui.label(RichText::new("🗜").size(52.0));
        ui.add_space(6.0);
        ui.label(RichText::new("Drop an archive to open it").heading());
        ui.label(RichText::new("or drop files and folders to compress them").color(theme::weak(ui)));
        ui.add_space(16.0);
        ui.horizontal(|ui| {
            let width = 260.0;
            ui.add_space((ui.available_width() - width).max(0.0) / 2.0);
            if ui.add(theme::primary_button("Open archive…").min_size(egui::vec2(120.0, 32.0))).clicked() {
                app.pick_and_open(&ctx);
            }
            if ui.add(egui::Button::new("Create archive…").min_size(egui::vec2(120.0, 32.0))).clicked() {
                app.open_create_dialog(Vec::new());
            }
        });
    });
}

fn status_bar(app: &OxideApp, ui: &mut egui::Ui) {
    let weak = theme::weak(ui);
    ui.horizontal(|ui| {
        let Some(a) = &app.archive else {
            ui.label(RichText::new(format!("Oxide {}", env!("CARGO_PKG_VERSION"))).small().color(weak));
            return;
        };
        let info = &a.info;
        let mut text = format!(
            "{} · {} files · {} folders · {} → {}",
            info.format,
            info.file_count(),
            info.dir_count().max(a.tree.children("").iter().filter(|r| r.is_dir).count()),
            human_bytes(info.total_size()),
            human_bytes(info.archive_size)
        );
        if let Some(ratio) = info.compression_ratio() {
            text.push_str(&format!(" ({:.0}%)", ratio * 100.0));
        }
        if info.is_encrypted() {
            text.push_str(" · 🔒 encrypted");
        }
        ui.label(RichText::new(text).small().color(weak));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if !a.selection.is_empty() {
                let bytes: u64 = a.rows.iter().filter(|r| a.selection.contains(&r.path)).map(|r| r.size).sum();
                ui.label(
                    RichText::new(format!("{} selected · {}", a.selection.len(), human_bytes(bytes)))
                        .small()
                        .color(weak),
                );
            }
        });
    });
}

fn dialogs(app: &mut OxideApp, ctx: &egui::Context) {
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
        ui.heading("🔒 Password required");
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
