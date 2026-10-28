//! "Create archive" dialog.

use std::path::{Path, PathBuf};

use eframe::egui::{self, RichText};
use oxide_core::{ArchiveFormat, CompressionMethod, CreateOptions, EncryptionMethod};

use super::{folder_field, form_label};
use crate::theme;

pub struct CreateDialog {
    pub inputs: Vec<PathBuf>,
    pub name: String,
    pub dest: String,
    pub format: ArchiveFormat,
    pub method: CompressionMethod,
    pub level: i32,
    pub password: String,
    pub confirm: String,
    pub encryption: EncryptionMethod,
    pub preserve_paths: bool,
    pub include_hidden: bool,
    pub overwrite: bool,
}

pub enum Outcome {
    Open,
    Cancel,
    Create { inputs: Vec<PathBuf>, output: PathBuf, options: CreateOptions },
}

impl CreateDialog {
    pub fn new(inputs: Vec<PathBuf>) -> Self {
        let name = match inputs.as_slice() {
            [one] => one.file_stem().map(|s| s.to_string_lossy().into_owned()),
            _ => None,
        }
        .unwrap_or_else(|| "Archive".into());
        let dest = inputs
            .first()
            .and_then(|p| p.parent())
            .map(Path::to_path_buf)
            .or_else(|| std::env::home_dir().map(|h| h.join("Documents")).filter(|d| d.is_dir()))
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_default();
        let mut dialog = CreateDialog {
            inputs,
            name,
            dest: dest.display().to_string(),
            format: ArchiveFormat::Zip,
            method: CompressionMethod::Deflate,
            level: 6,
            password: String::new(),
            confirm: String::new(),
            encryption: EncryptionMethod::Aes256,
            preserve_paths: true,
            include_hidden: true,
            overwrite: false,
        };
        dialog.set_format(ArchiveFormat::Zip);
        dialog
    }

    fn set_format(&mut self, format: ArchiveFormat) {
        self.format = format;
        self.set_method(format.default_method());
        if let Some(&first) = format.info().encryption.first() {
            self.encryption = first;
        }
    }

    fn set_method(&mut self, method: CompressionMethod) {
        self.method = method;
        if let Some(r) = method.level_range() {
            self.level = r.default;
        }
    }

    pub fn output(&self) -> PathBuf {
        Path::new(&self.dest).join(format!("{}.{}", self.name.trim(), self.format.extension()))
    }

    fn validation_error(&self) -> Option<&'static str> {
        let info = self.format.info();
        if self.inputs.is_empty() {
            Some("Add at least one file or folder.")
        } else if self.name.trim().is_empty() {
            Some("Enter a name for the archive.")
        } else if self.dest.trim().is_empty() {
            Some("Choose where to save the archive.")
        } else if !info.multi_file && (self.inputs.len() != 1 || self.inputs[0].is_dir()) {
            Some("This format holds a single file. Use a TAR format for folders.")
        } else if self.password != self.confirm {
            Some("Passwords don't match.")
        } else {
            None
        }
    }

    pub fn show(&mut self, ctx: &egui::Context) -> Outcome {
        let mut outcome = Outcome::Open;
        let response = egui::Modal::new(egui::Id::unique("create_dialog")).show(ctx, |ui| {
            ui.set_width(520.0);
            ui.heading("Create archive");
            ui.add_space(8.0);

            self.inputs_section(ui);
            ui.add_space(10.0);

            let info = self.format.info();
            egui::Grid::new("create_grid").num_columns(2).spacing([14.0, 10.0]).show(ui, |ui| {
                form_label(ui, "Name");
                ui.horizontal(|ui| {
                    ui.add(egui::TextEdit::singleline(&mut self.name).desired_width(260.0));
                    ui.label(RichText::new(format!(".{}", self.format.extension())).color(theme::weak(ui)));
                });
                ui.end_row();

                form_label(ui, "Save in");
                folder_field(ui, "create_dest", &mut self.dest);
                ui.end_row();

                form_label(ui, "Format");
                let mut format = self.format;
                egui::ComboBox::from_id_salt("format").selected_text(format.name()).show_ui(ui, |ui| {
                    for f in ArchiveFormat::ALL.into_iter().filter(|f| f.info().can_create) {
                        ui.selectable_value(&mut format, f, format!("{}  (.{})", f.name(), f.extension()));
                    }
                });
                if format != self.format {
                    self.set_format(format);
                }
                ui.end_row();

                if info.methods.len() > 1 {
                    form_label(ui, "Method");
                    let mut method = self.method;
                    egui::ComboBox::from_id_salt("method").selected_text(method.name()).show_ui(ui, |ui| {
                        for &m in info.methods {
                            ui.selectable_value(&mut method, m, m.name());
                        }
                    });
                    if method != self.method {
                        self.set_method(method);
                    }
                    ui.end_row();
                }

                if let Some(range) = self.method.level_range() {
                    form_label(ui, "Level");
                    ui.horizontal(|ui| {
                        ui.add(egui::Slider::new(&mut self.level, range.min..=range.max));
                        let hint = if self.level <= range.min + (range.max - range.min) / 4 {
                            "faster"
                        } else if self.level >= range.max - (range.max - range.min) / 4 {
                            "smaller"
                        } else {
                            "balanced"
                        };
                        ui.label(RichText::new(hint).color(theme::weak(ui)));
                    });
                    ui.end_row();
                }

                if !info.encryption.is_empty() {
                    form_label(ui, "Password");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.password)
                            .password(true)
                            .hint_text("Optional")
                            .desired_width(260.0),
                    );
                    ui.end_row();
                    if !self.password.is_empty() {
                        form_label(ui, "Confirm");
                        ui.add(egui::TextEdit::singleline(&mut self.confirm).password(true).desired_width(260.0));
                        ui.end_row();
                        if info.encryption.len() > 1 {
                            form_label(ui, "Encryption");
                            egui::ComboBox::from_id_salt("enc").selected_text(self.encryption.name()).show_ui(
                                ui,
                                |ui| {
                                    for &e in info.encryption {
                                        ui.selectable_value(&mut self.encryption, e, e.name());
                                    }
                                },
                            );
                            ui.end_row();
                        }
                    }
                } else {
                    self.password.clear();
                    self.confirm.clear();
                }

                form_label(ui, "Options");
                ui.vertical(|ui| {
                    ui.checkbox(&mut self.preserve_paths, "Keep folder structure");
                    ui.checkbox(&mut self.include_hidden, "Include hidden files");
                    if self.output().exists() {
                        ui.checkbox(&mut self.overwrite, "Replace the existing file");
                    }
                });
                ui.end_row();
            });

            ui.add_space(12.0);
            let error = self.validation_error().or_else(|| {
                (self.output().exists() && !self.overwrite).then_some("A file with this name already exists.")
            });
            ui.horizontal(|ui| {
                if let Some(msg) = error {
                    ui.label(RichText::new(msg).color(ui.visuals().warn_fg_color));
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.add_enabled(error.is_none(), theme::primary_button("Create")).clicked() {
                        outcome = Outcome::Create {
                            inputs: self.inputs.clone(),
                            output: self.output(),
                            options: self.options(),
                        };
                    }
                    if ui.button("Cancel").clicked() {
                        outcome = Outcome::Cancel;
                    }
                });
            });
        });
        if response.should_close() && matches!(outcome, Outcome::Open) {
            outcome = Outcome::Cancel;
        }
        outcome
    }

    fn inputs_section(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(RichText::new(format!("{} item(s) to add", self.inputs.len())).strong());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Add folder…").clicked()
                    && let Some(dirs) = rfd::FileDialog::new().pick_folders()
                {
                    self.add_inputs(dirs);
                }
                if ui.button("Add files…").clicked()
                    && let Some(files) = rfd::FileDialog::new().pick_files()
                {
                    self.add_inputs(files);
                }
            });
        });
        egui::Frame::new()
            .fill(ui.visuals().extreme_bg_color)
            .stroke(ui.visuals().widgets.noninteractive.bg_stroke)
            .corner_radius(8)
            .inner_margin(6)
            .show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                egui::ScrollArea::vertical().max_height(110.0).auto_shrink([false, true]).show(ui, |ui| {
                    if self.inputs.is_empty() {
                        ui.label(RichText::new("Drop files here or use the buttons above").color(theme::weak(ui)));
                    }
                    let mut remove = None;
                    for (i, path) in self.inputs.iter().enumerate() {
                        ui.horizontal(|ui| {
                            if ui.small_button("✖").on_hover_text("Remove").clicked() {
                                remove = Some(i);
                            }
                            ui.label(if path.is_dir() { "📁" } else { "📄" });
                            ui.add(egui::Label::new(path.display().to_string()).truncate())
                                .on_hover_text(path.display().to_string());
                        });
                    }
                    if let Some(i) = remove {
                        self.inputs.remove(i);
                    }
                });
            });
    }

    pub fn add_inputs(&mut self, paths: Vec<PathBuf>) {
        for p in paths {
            if !self.inputs.contains(&p) {
                self.inputs.push(p);
            }
        }
    }

    fn options(&self) -> CreateOptions {
        CreateOptions {
            format: Some(self.format),
            method: Some(self.method),
            level: self.method.level_range().map(|_| self.level),
            password: (!self.password.is_empty()).then(|| self.password.clone().into()),
            encryption: (!self.password.is_empty()).then_some(self.encryption),
            preserve_paths: self.preserve_paths,
            include_hidden: self.include_hidden,
            overwrite: self.overwrite,
            ..CreateOptions::default()
        }
    }
}
