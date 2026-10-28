//! The modal shown while a background job runs.

use eframe::egui::{self, RichText};

use super::{human_bytes, human_duration};
use crate::app::{Job, JobKind};

/// Shows progress for `job`. Returns true if the user asked to cancel.
pub fn show(ctx: &egui::Context, job: &Job) -> bool {
    let update = job.shared().update.clone();
    let mut cancel = false;
    egui::Modal::new(egui::Id::unique("progress")).show(ctx, |ui| {
        ui.set_width(440.0);
        ui.heading(&job.title);
        ui.add_space(10.0);

        let fraction = update.as_ref().and_then(|u| u.fraction);
        let bar = match fraction {
            Some(f) => egui::ProgressBar::new(f as f32).show_percentage(),
            None => egui::ProgressBar::new(0.0).animate(true),
        };
        ui.add(bar.desired_height(18.0).corner_radius(6));
        ui.add_space(8.0);

        let weak = crate::theme::weak(ui);
        let current = update.as_ref().and_then(|u| u.current.clone()).unwrap_or_else(|| match job.kind {
            JobKind::Open => "Reading archive index…".into(),
            _ => "Preparing…".into(),
        });
        ui.add(egui::Label::new(RichText::new(current).color(weak)).truncate());
        ui.add_space(8.0);

        // Listing only knows how far through the file it is; counters would read zero.
        if let Some(u) = update.as_ref().filter(|_| job.kind != JobKind::Open) {
            egui::Grid::new("progress_stats").num_columns(4).spacing([16.0, 4.0]).show(ui, |ui| {
                let files = match u.entries_total {
                    Some(t) => format!("{} of {}", u.entries_done, t),
                    None => u.entries_done.to_string(),
                };
                let bytes = match u.bytes_total {
                    Some(t) if t > 0 => format!("{} of {}", human_bytes(u.bytes_done), human_bytes(t)),
                    _ => human_bytes(u.bytes_done),
                };
                stat(ui, "Items", &files);
                stat(ui, "Data", &bytes);
                ui.end_row();
                stat(ui, "Speed", &format!("{}/s", human_bytes(u.bytes_per_second() as u64)));
                stat(ui, "Elapsed", &human_duration(job.started.elapsed()));
                ui.end_row();
                if let Some(eta) = u.eta() {
                    stat(ui, "Remaining", &format!("about {}", human_duration(eta)));
                    ui.end_row();
                }
            });
        }

        ui.add_space(12.0);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let cancelling = job.cancel.is_cancelled();
            let label = if cancelling { "Cancelling…" } else { "Cancel" };
            if ui.add_enabled(!cancelling, egui::Button::new(label)).clicked() {
                cancel = true;
            }
        });
    });
    // Keep the elapsed clock ticking even without progress callbacks.
    ctx.request_repaint_after(std::time::Duration::from_millis(250));
    cancel
}

fn stat(ui: &mut egui::Ui, label: &str, value: &str) {
    ui.label(RichText::new(label).color(crate::theme::weak(ui)));
    ui.label(value);
}
