//! Reusable pieces of the interface.

pub mod create_dialog;
pub mod extract_dialog;
pub mod file_table;
pub mod icons;
pub mod progress;

use std::path::PathBuf;
use std::time::Duration;

use chrono::{Local, TimeZone};
use eframe::egui;

pub fn human_bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = n as f64;
    let mut unit = 0;
    while v >= 1000.0 && unit < UNITS.len() - 1 {
        v /= 1000.0;
        unit += 1;
    }
    match unit {
        0 => format!("{n} B"),
        _ if v >= 100.0 => format!("{v:.0} {}", UNITS[unit]),
        _ => format!("{v:.1} {}", UNITS[unit]),
    }
}

pub fn human_duration(d: Duration) -> String {
    let s = d.as_secs();
    match s {
        0..=59 => format!("{s}s"),
        60..=3599 => format!("{}m {:02}s", s / 60, s % 60),
        _ => format!("{}h {:02}m", s / 3600, (s % 3600) / 60),
    }
}

pub fn format_time(secs: Option<i64>) -> String {
    secs.and_then(|s| Local.timestamp_opt(s, 0).single())
        .map(|t| t.format("%Y-%m-%d  %H:%M").to_string())
        .unwrap_or_default()
}

/// A text field for a folder path with a "Choose…" button that opens a native picker.
pub fn folder_field(ui: &mut egui::Ui, id: &str, value: &mut String) {
    ui.horizontal(|ui| {
        let width = (ui.available_width() - 90.0).max(120.0);
        ui.add(egui::TextEdit::singleline(value).id_salt(id).desired_width(width));
        if ui.button("Choose…").clicked() {
            let mut dialog = rfd::FileDialog::new();
            if !value.is_empty() {
                dialog = dialog.set_directory(PathBuf::from(value.as_str()));
            }
            if let Some(dir) = dialog.pick_folder() {
                *value = dir.display().to_string();
            }
        }
    });
}

/// A form label in the left column of a dialog grid.
pub fn form_label(ui: &mut egui::Ui, text: &str) {
    ui.label(egui::RichText::new(text).color(crate::theme::weak(ui)));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_sizes_and_durations() {
        assert_eq!(human_bytes(999), "999 B");
        assert_eq!(human_bytes(1_500), "1.5 KB");
        assert_eq!(human_bytes(250_000_000), "250 MB");
        assert_eq!(human_duration(Duration::from_secs(75)), "1m 15s");
    }
}
