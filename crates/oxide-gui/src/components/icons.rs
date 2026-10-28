//! A small set of line icons drawn with the painter, so they follow the theme's
//! text colour, scale crisply with DPI, and dim correctly when disabled
//! (colour emoji do none of these).

use std::f32::consts::{PI, TAU};

use eframe::egui::{self, Color32, Painter, Pos2, Rect, Response, Sense, Shape, Stroke, StrokeKind, Vec2};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Icon {
    Open,
    Create,
    Extract,
    Add,
    Test,
    Back,
    Up,
    Search,
    Preview,
    Close,
    Folder,
    File,
    Link,
    Lock,
    Archive,
    Sun,
    Moon,
    System,
}

/// Draws `icon` into `rect` (designed on a 16×16 grid).
pub fn paint(painter: &Painter, rect: Rect, icon: Icon, color: Color32) {
    let s = rect.width().min(rect.height()) / 16.0;
    let o = rect.center() - Vec2::splat(8.0 * s);
    let p = |x: f32, y: f32| Pos2::new(o.x + x * s, o.y + y * s);
    let r = |x0: f32, y0: f32, x1: f32, y1: f32| Rect::from_min_max(p(x0, y0), p(x1, y1));
    let stroke = Stroke::new(1.4 * s.max(0.75), color);
    let line = |pts: &[(f32, f32)]| {
        painter.add(Shape::line(pts.iter().map(|&(x, y)| p(x, y)).collect(), stroke));
    };
    let closed = |pts: &[(f32, f32)]| {
        painter.add(Shape::closed_line(pts.iter().map(|&(x, y)| p(x, y)).collect(), stroke));
    };
    let arc = |cx: f32, cy: f32, rad: f32, from: f32, to: f32| -> Vec<Pos2> {
        (0..=16)
            .map(|i| from + (to - from) * i as f32 / 16.0)
            .map(|a| p(cx + rad * a.cos(), cy + rad * a.sin()))
            .collect()
    };

    match icon {
        Icon::Preview => {
            closed(&[
                (1.0, 8.0),
                (3.5, 4.5),
                (8.0, 2.5),
                (12.5, 4.5),
                (15.0, 8.0),
                (12.5, 11.5),
                (8.0, 13.5),
                (3.5, 11.5),
            ]);
            painter.circle_stroke(p(8.0, 8.0), 2.5 * s, stroke);
        }
        Icon::Open => {
            line(&[(1.5, 13.0), (1.5, 3.0), (6.0, 3.0), (7.5, 4.5), (13.0, 4.5), (13.0, 7.0)]);
            closed(&[(1.5, 13.0), (4.0, 7.0), (15.0, 7.0), (12.5, 13.0)]);
        }
        Icon::Create | Icon::Archive => {
            painter.rect_stroke(r(2.0, 6.0, 14.0, 14.0), 1.5 * s, stroke, StrokeKind::Middle);
            painter.rect_stroke(r(1.0, 2.5, 15.0, 6.0), 1.5 * s, stroke, StrokeKind::Middle);
            line(&[(6.0, 9.0), (10.0, 9.0)]);
        }
        Icon::Extract => {
            line(&[(8.0, 1.5), (8.0, 10.0)]);
            line(&[(4.5, 6.5), (8.0, 10.0), (11.5, 6.5)]);
            line(&[(2.0, 10.5), (2.0, 14.0), (14.0, 14.0), (14.0, 10.5)]);
        }
        Icon::Add => {
            line(&[(8.0, 2.5), (8.0, 13.5)]);
            line(&[(2.5, 8.0), (13.5, 8.0)]);
        }
        Icon::Test => {
            painter.circle_stroke(p(8.0, 8.0), 6.5 * s, stroke);
            line(&[(5.0, 8.3), (7.2, 10.5), (11.2, 5.8)]);
        }
        Icon::Back => line(&[(10.0, 3.0), (5.0, 8.0), (10.0, 13.0)]),
        Icon::Up => {
            line(&[(8.0, 13.5), (8.0, 3.0)]);
            line(&[(3.5, 7.5), (8.0, 3.0), (12.5, 7.5)]);
        }
        Icon::Search => {
            painter.circle_stroke(p(7.0, 7.0), 4.5 * s, stroke);
            line(&[(10.4, 10.4), (14.0, 14.0)]);
        }
        Icon::Close => {
            line(&[(4.0, 4.0), (12.0, 12.0)]);
            line(&[(12.0, 4.0), (4.0, 12.0)]);
        }
        Icon::Folder => {
            painter.rect_filled(r(1.0, 2.5, 7.0, 6.0), 1.2 * s, color);
            painter.rect_filled(r(1.0, 4.5, 15.0, 13.5), 1.5 * s, color);
        }
        Icon::File => {
            closed(&[(3.5, 1.5), (9.5, 1.5), (12.5, 4.5), (12.5, 14.5), (3.5, 14.5)]);
            line(&[(9.5, 1.5), (9.5, 4.5), (12.5, 4.5)]);
        }
        Icon::Link => {
            closed(&[(3.5, 1.5), (9.5, 1.5), (12.5, 4.5), (12.5, 14.5), (3.5, 14.5)]);
            line(&[(6.0, 11.5), (10.0, 7.5)]);
            line(&[(7.0, 7.5), (10.0, 7.5), (10.0, 10.5)]);
        }
        Icon::Lock => {
            painter.rect_filled(r(3.0, 7.0, 13.0, 14.5), 1.5 * s, color);
            painter.add(Shape::line(arc(8.0, 7.0, 3.2, PI, TAU), stroke));
        }
        Icon::Sun => {
            painter.circle_stroke(p(8.0, 8.0), 3.0 * s, stroke);
            for i in 0..8 {
                let a = i as f32 * TAU / 8.0;
                line(&[(8.0 + 5.0 * a.cos(), 8.0 + 5.0 * a.sin()), (8.0 + 6.8 * a.cos(), 8.0 + 6.8 * a.sin())]);
            }
        }
        Icon::Moon => {
            // Crescent: the part of circle A not covered by circle B.
            let (a, ra, b, rb) = ((8.0f32, 8.5f32), 6.0f32, (11.5f32, 5.0f32), 5.0f32);
            let away = (a.1 - b.1).atan2(a.0 - b.0);
            let outside_b = |x: f32, y: f32| (x - b.0).hypot(y - b.1) > rb;
            let inside_a = |x: f32, y: f32| (x - a.0).hypot(y - a.1) < ra;
            let mut pts: Vec<(f32, f32)> = (0..=64)
                .map(|i| away - PI + TAU * i as f32 / 64.0)
                .map(|t| (a.0 + ra * t.cos(), a.1 + ra * t.sin()))
                .filter(|&(x, y)| outside_b(x, y))
                .collect();
            pts.extend(
                (0..=64)
                    .map(|i| away + PI - TAU * i as f32 / 64.0)
                    .map(|t| (b.0 + rb * t.cos(), b.1 + rb * t.sin()))
                    .filter(|&(x, y)| inside_a(x, y)),
            );
            closed(&pts);
        }
        Icon::System => {
            painter.rect_stroke(r(1.5, 2.5, 14.5, 11.0), 1.5 * s, stroke, StrokeKind::Middle);
            line(&[(8.0, 11.0), (8.0, 14.0)]);
            line(&[(5.0, 14.0), (11.0, 14.0)]);
        }
    }
}

/// Allocates a 16-point square and draws `icon` in it.
pub fn show(ui: &mut egui::Ui, icon: Icon, color: Color32) -> Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(16.0), Sense::hover());
    if ui.is_rect_visible(rect) {
        paint(ui.painter(), rect, icon, color);
    }
    response
}

/// A button showing an icon and optional label. Frameless buttons only show
/// a background on hover (used for navigation and the theme switch).
pub fn button(ui: &mut egui::Ui, icon: Icon, label: &str, enabled: bool, framed: bool) -> Response {
    let font = egui::TextStyle::Button.resolve(ui.style());
    let galley = (!label.is_empty()).then(|| ui.painter().layout_no_wrap(label.to_owned(), font, Color32::PLACEHOLDER));
    let pad = if galley.is_some() { 11.0 } else { 7.0 };
    let text_w = galley.as_ref().map_or(0.0, |g| 7.0 + g.size().x);
    let size = egui::vec2(pad * 2.0 + 16.0 + text_w, 30.0);
    let sense = if enabled { Sense::click() } else { Sense::hover() };
    let (rect, response) = ui.allocate_exact_size(size, sense);
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, enabled, label));

    if ui.is_rect_visible(rect) {
        let visuals = if enabled { ui.style().interact(&response) } else { &ui.visuals().widgets.inactive };
        let painter = ui.painter();
        if framed || (enabled && response.hovered()) {
            let fill = if framed { visuals.weak_bg_fill } else { ui.visuals().widgets.hovered.weak_bg_fill };
            let stroke = if framed { visuals.bg_stroke } else { Stroke::NONE };
            painter.rect(rect, visuals.corner_radius, fill, stroke, StrokeKind::Inside);
        }
        let color = if enabled { visuals.text_color() } else { ui.visuals().weak_text_color().gamma_multiply(0.7) };
        let icon_rect = Rect::from_center_size(Pos2::new(rect.left() + pad + 8.0, rect.center().y), Vec2::splat(16.0));
        paint(painter, icon_rect, icon, color);
        if let Some(g) = galley {
            let pos = Pos2::new(icon_rect.right() + 7.0, rect.center().y - g.size().y / 2.0);
            painter.galley(pos, g, color);
        }
    }
    response
}
