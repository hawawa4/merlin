//! The look of the app's hardware: a dark chassis, raised key caps that light up, recessed
//! switches and plates. Everything on screen that is a control is built from these, so it reads
//! as a piece of gear rather than a form.

use egui::{
    Align2, Color32, CornerRadius, FontId, Margin, Rect, Response, Sense, Stroke, Ui, vec2,
};

pub const CHASSIS: Color32 = Color32::from_rgb(0x1c, 0x1d, 0x20);
pub const PLATE: Color32 = Color32::from_rgb(0x28, 0x2a, 0x2e);
pub const WELL: Color32 = Color32::from_rgb(0x11, 0x12, 0x14);
const CAP: Color32 = Color32::from_rgb(0x3a, 0x3d, 0x43);
const BEVEL_HI: Color32 = Color32::from_rgb(0x62, 0x66, 0x6e);
const BEVEL_LO: Color32 = Color32::from_rgb(0x0b, 0x0b, 0x0d);
pub const TEXT: Color32 = Color32::from_rgb(0xdc, 0xdd, 0xe0);
pub const TEXT_DIM: Color32 = Color32::from_rgb(0x8d, 0x90, 0x97);

pub const AMBER: Color32 = Color32::from_rgb(0xe8, 0xb4, 0x40);
pub const GREEN: Color32 = Color32::from_rgb(0x58, 0xd0, 0x70);
pub const BLUE: Color32 = Color32::from_rgb(0x4a, 0xa3, 0xe0);

/// A raised key cap with a label. `lit` backlights it in that colour (a switch that is on, the
/// view you are in); `base` paints the cap itself in a colour (a red panic button).
#[derive(Clone)]
pub struct Cap {
    label: String,
    lit: Option<Color32>,
    base: Option<Color32>,
    min_width: f32,
}

impl Cap {
    pub fn new(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            lit: None,
            base: None,
            min_width: 0.0,
        }
    }

    pub fn lit(mut self, colour: Option<Color32>) -> Self {
        self.lit = colour;
        self
    }

    pub fn base(mut self, colour: Color32) -> Self {
        self.base = Some(colour);
        self
    }

    pub fn min_width(mut self, w: f32) -> Self {
        self.min_width = w;
        self
    }

    pub fn show(self, ui: &mut Ui) -> Response {
        let font = FontId::proportional(13.0);
        let galley = ui
            .painter()
            .layout_no_wrap(self.label.clone(), font.clone(), TEXT);
        let size = vec2((galley.size().x + 22.0).max(self.min_width), 26.0);
        let (rect, r) = ui.allocate_exact_size(size, Sense::click());
        if !ui.is_rect_visible(rect) {
            return r;
        }
        let enabled = ui.is_enabled();
        let down = enabled && r.is_pointer_button_down_on();
        // A pressed cap sits lower and loses its sheen.
        let rect = if down {
            rect.translate(vec2(0.0, 1.0))
        } else {
            rect
        };
        let rounding = CornerRadius::same(5);
        let body = self.base.unwrap_or(CAP);
        let body = match (self.lit, down, r.hovered() && enabled) {
            (Some(c), _, _) => mix(body, c, 0.30),
            (None, true, _) => body.gamma_multiply(0.75),
            (None, false, true) => mix(body, Color32::WHITE, 0.07),
            _ => body,
        };
        let p = ui.painter();
        // The shadow it casts, then the cap, its sheen and its bevel.
        p.rect_filled(
            rect.translate(vec2(0.0, 2.0)).expand(0.5),
            rounding,
            BEVEL_LO,
        );
        p.rect_filled(rect, rounding, body.gamma_multiply(0.82));
        if !down {
            let sheen = Rect::from_min_size(rect.min, vec2(rect.width(), rect.height() * 0.5));
            p.rect_filled(
                sheen.shrink2(vec2(1.0, 1.0)),
                CornerRadius {
                    nw: 4,
                    ne: 4,
                    sw: 0,
                    se: 0,
                },
                mix(body, Color32::WHITE, 0.10),
            );
        }
        p.rect_stroke(
            rect,
            rounding,
            Stroke::new(
                1.0,
                if down {
                    BEVEL_LO
                } else {
                    BEVEL_HI.gamma_multiply(0.55)
                },
            ),
            egui::StrokeKind::Inside,
        );
        if let Some(c) = self.lit {
            // The backlight bleeding out under the cap.
            let glow = Rect::from_min_size(
                egui::pos2(rect.left() + 5.0, rect.bottom() - 3.0),
                vec2(rect.width() - 10.0, 2.0),
            );
            p.rect_filled(glow, 1.0, c);
        }
        let text = match (enabled, self.lit) {
            (false, _) => TEXT_DIM.gamma_multiply(0.6),
            (_, Some(c)) => mix(c, Color32::WHITE, 0.55),
            _ => TEXT,
        };
        p.text(
            rect.center(),
            Align2::CENTER_CENTER,
            &self.label,
            font,
            text,
        );
        r
    }
}

/// A star to mark favourites: an empty grey outline, or filled yellow when `on`.
pub fn star(ui: &mut Ui, on: bool) -> Response {
    let (rect, r) = ui.allocate_exact_size(vec2(18.0, 18.0), Sense::click());
    let (c, outer, inner) = (rect.center() + vec2(0.0, 0.5), 7.5, 3.1);
    // Ten points, alternately on the outer and the inner circle, starting at the top.
    let points: Vec<egui::Pos2> = (0..10)
        .map(|n| {
            let a = -std::f32::consts::FRAC_PI_2 + n as f32 * std::f32::consts::PI / 5.0;
            let radius = if n % 2 == 0 { outer } else { inner };
            c + vec2(a.cos(), a.sin()) * radius
        })
        .collect();
    let (fill, line) = match (on, r.hovered()) {
        (true, _) => (AMBER, mix(AMBER, Color32::BLACK, 0.45)),
        (false, true) => (Color32::TRANSPARENT, TEXT),
        (false, false) => (Color32::TRANSPARENT, TEXT_DIM),
    };
    ui.painter().add(egui::Shape::convex_polygon(
        points,
        fill,
        Stroke::new(1.3, line),
    ));
    r
}

/// A row of caps in a recessed track, one of which is lit: the way a hardware selector looks.
/// Returns the index of the cap clicked this frame.
pub fn selector(ui: &mut Ui, items: &[&str], selected: usize, colour: Color32) -> Option<usize> {
    let mut clicked = None;
    inset().show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing = vec2(3.0, 0.0);
            for (n, label) in items.iter().enumerate() {
                let lit = (n == selected).then_some(colour);
                if Cap::new(*label).lit(lit).min_width(70.0).show(ui).clicked() {
                    clicked = Some(n);
                }
            }
        });
    });
    clicked
}

/// A recessed well that controls sit in.
pub fn inset() -> egui::Frame {
    egui::Frame::new()
        .fill(WELL)
        .stroke(Stroke::new(1.0, BEVEL_LO))
        .corner_radius(7)
        .inner_margin(Margin::same(3))
}

/// A raised plate that groups controls that belong together.
pub fn plate() -> egui::Frame {
    egui::Frame::new()
        .fill(PLATE)
        .stroke(Stroke::new(1.0, BEVEL_HI.gamma_multiply(0.35)))
        .corner_radius(7)
        .inner_margin(Margin::symmetric(8, 4))
}

/// The frame of a panel that is part of the chassis.
pub fn chassis() -> egui::Frame {
    egui::Frame::new()
        .fill(CHASSIS)
        .stroke(Stroke::new(1.0, BEVEL_LO))
        .inner_margin(Margin::symmetric(10, 6))
}

/// A small engraved label, for naming a group of controls.
pub fn engraved(ui: &mut Ui, text: &str) -> Response {
    ui.label(
        egui::RichText::new(text.to_uppercase())
            .size(10.5)
            .strong()
            .color(TEXT_DIM),
    )
}

pub fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let l = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgb(l(a.r(), b.r()), l(a.g(), b.g()), l(a.b(), b.b()))
}
