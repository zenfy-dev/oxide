//! "Extract" dialog.

use std::path::{Path, PathBuf};

use eframe::egui::{self, RichText};
use oxide_core::{ArchiveInfo, ExtractLimits, ExtractOptions, OverwritePolicy, Password, SymlinkPolicy};

use super::{folder_field, form_label};
use crate::theme;

pub struct ExtractDialog {
    pub archive: PathBuf,
    pub dest: String,
    pub subfolder: bool,
    pub folder_name: String,
    pub selection: Vec<String>,
    pub only_selected: bool,
    pub overwrite: OverwritePolicy,
    pub password: String,
    pub encrypted: bool,
    pub protect: bool,
    pub symlinks: bool,
}

pub enum Outcome {
    Open,
    Cancel,
    Extract { archive: PathBuf, dest: PathBuf, options: ExtractOptions },
}

/// "photos.tar.gz" → "photos".
fn stem_without_archive_ext(info: &ArchiveInfo) -> String {
    let name = info.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let lower = name.to_ascii_lowercase();
    for ext in info.format.extensions() {
        let dotted = format!(".{ext}");
        if lower.ends_with(&dotted) && name.len() > dotted.len() {
            return name[..name.len() - dotted.len()].to_owned();
        }
    }
    Path::new(&name).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or(name)
}

impl ExtractDialog {
    pub fn new(info: &ArchiveInfo, selection: Vec<String>, only_selected: bool, password: Option<&Password>) -> Self {
        let parent = info.path.parent().map(Path::to_path_buf).unwrap_or_default();
        ExtractDialog {
            archive: info.path.clone(),
            dest: parent.display().to_string(),
            subfolder: true,
            folder_name: stem_without_archive_ext(info),
            only_selected: only_selected && !selection.is_empty(),
            selection,
            overwrite: OverwritePolicy::Rename,
            password: password.map(|p| p.expose().to_owned()).unwrap_or_default(),
            encrypted: info.is_encrypted(),
            protect: true,
            symlinks: true,
        }
    }

    fn destination(&self) -> PathBuf {
        let base = PathBuf::from(&self.dest);
        if self.subfolder && !self.folder_name.trim().is_empty() { base.join(self.folder_name.trim()) } else { base }
    }

    pub fn show(&mut self, ctx: &egui::Context) -> Outcome {
        let mut outcome = Outcome::Open;
        let response = egui::Modal::new(egui::Id::unique("extract_dialog")).show(ctx, |ui| {
            ui.set_width(500.0);
            ui.heading("Extract");
            ui.add_space(8.0);
            egui::Grid::new("extract_grid").num_columns(2).spacing([14.0, 10.0]).show(ui, |ui| {
                form_label(ui, "Destination");
                folder_field(ui, "extract_dest", &mut self.dest);
                ui.end_row();

                form_label(ui, "");
                ui.horizontal(|ui| {
                    ui.checkbox(&mut self.subfolder, "Into a new folder");
                    ui.add_enabled(
                        self.subfolder,
                        egui::TextEdit::singleline(&mut self.folder_name).desired_width(200.0),
                    );
                });
                ui.end_row();

                form_label(ui, "Files");
                ui.horizontal(|ui| {
                    ui.radio_value(&mut self.only_selected, false, "Everything");
                    ui.add_enabled_ui(!self.selection.is_empty(), |ui| {
                        ui.radio_value(&mut self.only_selected, true, format!("Selected ({})", self.selection.len()));
                    });
                });
                ui.end_row();

                form_label(ui, "If a file exists");
                egui::ComboBox::from_id_salt("overwrite").selected_text(policy_label(self.overwrite)).show_ui(
                    ui,
                    |ui| {
                        for p in [
                            OverwritePolicy::Rename,
                            OverwritePolicy::Skip,
                            OverwritePolicy::Overwrite,
                            OverwritePolicy::Error,
                        ] {
                            ui.selectable_value(&mut self.overwrite, p, policy_label(p));
                        }
                    },
                );
                ui.end_row();

                if self.encrypted || !self.password.is_empty() {
                    form_label(ui, "Password");
                    ui.add(egui::TextEdit::singleline(&mut self.password).password(true).desired_width(240.0));
                    ui.end_row();
                }

                form_label(ui, "Safety");
                ui.vertical(|ui| {
                    ui.checkbox(&mut self.protect, "Stop on suspicious size or file count").on_hover_text(
                        "Guards against decompression bombs: 64 GB total, 1 million files, 1000:1 ratio",
                    );
                    ui.checkbox(&mut self.symlinks, "Restore symbolic links (inside the destination only)");
                });
                ui.end_row();
            });

            ui.add_space(6.0);
            ui.label(RichText::new(format!("→ {}", self.destination().display())).small().color(theme::weak(ui)));
            ui.add_space(10.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let ready = !self.dest.trim().is_empty();
                if ui.add_enabled(ready, theme::primary_button("Extract")).clicked() {
                    outcome = Outcome::Extract {
                        archive: self.archive.clone(),
                        dest: self.destination(),
                        options: self.options(),
                    };
                }
                if ui.button("Cancel").clicked() {
                    outcome = Outcome::Cancel;
                }
            });
        });
        if response.should_close() && matches!(outcome, Outcome::Open) {
            outcome = Outcome::Cancel;
        }
        outcome
    }

    fn options(&self) -> ExtractOptions {
        ExtractOptions {
            overwrite: self.overwrite,
            symlinks: if self.symlinks { SymlinkPolicy::Contained } else { SymlinkPolicy::Skip },
            limits: if self.protect { ExtractLimits::default() } else { ExtractLimits::unlimited() },
            selection: self.only_selected.then(|| self.selection.clone()),
            password: (!self.password.is_empty()).then(|| self.password.clone().into()),
            ..ExtractOptions::default()
        }
    }
}

fn policy_label(p: OverwritePolicy) -> &'static str {
    match p {
        OverwritePolicy::Rename => "Keep both (rename new)",
        OverwritePolicy::Skip => "Skip it",
        OverwritePolicy::Overwrite => "Replace it",
        OverwritePolicy::Error => "Stop extracting",
    }
}
