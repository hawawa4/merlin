//! The look of the app's hardware: a dark chassis, raised key caps that light up, recessed
//! switches and plates. Everything on screen that is a control is built from these, so it reads
//! as a piece of gear rather than a form.

use egui::{
    Align2, Color32, CornerRadius, FontId, Margin, Painter, Pos2, Rect, Response, Sense, Shape,
    Stroke, Ui, Vec2, pos2, vec2,
};

pub const CHASSIS: Color32 = Color32::from_rgb(0x1c, 0x1d, 0x20);
pub const PLATE: Color32 = Color32::from_rgb(0x28, 0x2a, 0x2e);
pub const WELL: Color32 = Color32::from_rgb(0x11, 0x12, 0x14);
const CAP: Color32 = Color32::from_rgb(0x3a, 0x3d, 0x43);
const BEVEL_HI: Color32 = Color32::from_rgb(0x62, 0x66, 0x6e);
const BEVEL_LO: Color32 = Color32::from_rgb(0x0b, 0x0b, 0x0d);
pub const TEXT: Color32 = Color32::from_rgb(0xdc, 0xdd, 0xe0);
pub const TEXT_DIM: Color32 = Color32::from_rgb(0xae, 0xb1, 0xb8);

pub const AMBER: Color32 = Color32::from_rgb(0xe8, 0xb4, 0x40);
pub const GREEN: Color32 = Color32::from_rgb(0x58, 0xd0, 0x70);
pub const RED: Color32 = Color32::from_rgb(0xff, 0x2a, 0x1e);
pub const BLUE: Color32 = Color32::from_rgb(0x4a, 0xa3, 0xe0);

/// The app-wide text: egui's defaults (9 pt small print, grey on grey) are unreadable on a stage
/// monitor, and the settings and setlist are full of small, weak text.
pub fn apply_style(ctx: &egui::Context) {
    use egui::{FontFamily, TextStyle};
    ctx.global_style_mut(|style| {
        let sizes = [
            (TextStyle::Small, 12.5, FontFamily::Proportional),
            (TextStyle::Body, 14.5, FontFamily::Proportional),
            (TextStyle::Button, 14.5, FontFamily::Proportional),
            (TextStyle::Heading, 22.0, FontFamily::Proportional),
            (TextStyle::Monospace, 14.0, FontFamily::Monospace),
        ];
        for (name, size, family) in sizes {
            style.text_styles.insert(name, FontId::new(size, family));
        }
        // `.weak()` and plain labels: brighter, so they pass for contrast against the chassis.
        style.visuals.weak_text_color = Some(Color32::from_rgb(0xb4, 0xb7, 0xbe));
        style.visuals.widgets.noninteractive.fg_stroke.color = Color32::from_rgb(0xdc, 0xdd, 0xe0);
        style.visuals.widgets.inactive.fg_stroke.color = Color32::from_rgb(0xe4, 0xe5, 0xe8);
        style.visuals.widgets.hovered.fg_stroke.color = Color32::WHITE;
        style.visuals.widgets.active.fg_stroke.color = Color32::WHITE;
        style.spacing.interact_size.y = 24.0;
    });
}

/// A raised key cap with a label. `lit` backlights it in that colour (a switch that is on, the
/// view you are in); `base` paints the cap itself in a colour (a red panic button).
#[derive(Clone)]
pub struct Cap {
    label: String,
    icon: Option<Icon>,
    lit: Option<Color32>,
    base: Option<Color32>,
    min_width: f32,
}

impl Cap {
    pub fn new(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            icon: None,
            lit: None,
            base: None,
            min_width: 0.0,
        }
    }

    /// Draws a small picture on the cap instead of a label.
    pub fn icon(mut self, icon: Icon) -> Self {
        self.icon = Some(icon);
        self.min_width = self.min_width.max(icon.width());
        self
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
        // A cap with a picture is as wide as the picture needs, not as its label.
        let wide = if self.icon.is_some() {
            0.0
        } else {
            galley.size().x + 22.0
        };
        let size = vec2(wide.max(self.min_width), 26.0);
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
        match self.icon {
            Some(icon) => icon.paint(p, rect.center(), text),
            None => {
                p.text(
                    rect.center(),
                    Align2::CENTER_CENTER,
                    &self.label,
                    font,
                    text,
                );
            }
        }
        r
    }
}

/// Pictures for caps.
#[derive(Clone, Copy)]
pub enum Icon {
    Keyboard,
    ChevronLeft,
    ChevronRight,
    /// A window with an arrow leaving it: open in a window of its own.
    PopOut,
    /// The same with the arrow going in: bring it back.
    PopIn,
    /// An up-and-down arrow between two bars: fit the height.
    FitHeight,
    /// A keyboard under a graduation cap: learn (a control for a knob).
    Learn,
    /// A keyboard beside a globe: the MIDI map.
    Map,
    /// A cassette: record what was played.
    Tape,
    /// A pyramid metronome with its swinging weight.
    Metronome,
}

impl Icon {
    fn width(self) -> f32 {
        match self {
            Self::Map => 54.0,
            _ => 40.0,
        }
    }

    fn paint(self, p: &Painter, c: Pos2, colour: Color32) {
        let line = Stroke::new(1.5, colour);
        let at = |x: f32, y: f32| c + vec2(x, y);
        match self {
            Self::ChevronLeft | Self::ChevronRight => {
                let tip = if matches!(self, Self::ChevronLeft) {
                    -3.5
                } else {
                    3.5
                };
                p.add(Shape::line(
                    vec![
                        c + vec2(-tip, -6.0),
                        c + vec2(tip, 0.0),
                        c + vec2(-tip, 6.0),
                    ],
                    Stroke::new(2.0, colour),
                ));
            }
            Self::Keyboard => keyboard(p, c, vec2(22.0, 13.0), colour),
            Self::PopOut | Self::PopIn => {
                // The window, open at the top right where the arrow goes through.
                p.add(Shape::line(
                    vec![
                        at(-2.0, -5.0),
                        at(-8.0, -5.0),
                        at(-8.0, 7.0),
                        at(6.0, 7.0),
                        at(6.0, 2.0),
                    ],
                    line,
                ));
                if matches!(self, Self::PopOut) {
                    p.line_segment([at(-1.0, 2.0), at(8.0, -7.0)], line);
                    p.add(Shape::line(
                        vec![at(3.0, -7.5), at(8.5, -7.5), at(8.5, -2.0)],
                        line,
                    ));
                } else {
                    p.line_segment([at(8.0, -7.0), at(0.0, 1.0)], line);
                    p.add(Shape::line(
                        vec![at(0.0, -4.0), at(0.0, 1.0), at(5.0, 1.0)],
                        line,
                    ));
                }
            }
            Self::FitHeight => {
                p.line_segment([at(-7.0, -9.0), at(7.0, -9.0)], line);
                p.line_segment([at(-7.0, 9.0), at(7.0, 9.0)], line);
                p.line_segment([at(0.0, -6.0), at(0.0, 6.0)], line);
                p.add(Shape::line(
                    vec![at(-4.0, -3.0), at(0.0, -7.0), at(4.0, -3.0)],
                    line,
                ));
                p.add(Shape::line(
                    vec![at(-4.0, 3.0), at(0.0, 7.0), at(4.0, 3.0)],
                    line,
                ));
            }
            Self::Learn => {
                // The mortarboard: a flat diamond on a cap, with a tassel.
                p.add(Shape::convex_polygon(
                    vec![
                        at(-10.0, -6.0),
                        at(0.0, -10.5),
                        at(10.0, -6.0),
                        at(0.0, -1.5),
                    ],
                    colour,
                    Stroke::NONE,
                ));
                p.add(Shape::line(
                    vec![
                        at(-5.0, -3.5),
                        at(-5.0, -0.5),
                        at(0.0, 1.5),
                        at(5.0, -0.5),
                        at(5.0, -3.5),
                    ],
                    Stroke::new(1.3, colour),
                ));
                p.line_segment([at(9.0, -6.0), at(9.0, -0.5)], Stroke::new(1.2, colour));
                p.circle_filled(at(9.0, 0.5), 1.3, colour);
                keyboard(p, at(0.0, 7.0), vec2(20.0, 9.0), colour);
            }
            Self::Map => {
                keyboard(p, at(-11.0, 0.0), vec2(20.0, 12.0), colour);
                // A globe: the outline, a meridian, the equator and two parallels.
                let g = at(12.0, 0.0);
                let r = 8.0;
                p.circle_stroke(g, r, Stroke::new(1.3, colour));
                let meridian: Vec<Pos2> = (0..=24)
                    .map(|n| {
                        let a = n as f32 / 24.0 * std::f32::consts::TAU;
                        g + vec2(a.cos() * 3.6, a.sin() * r)
                    })
                    .collect();
                p.add(Shape::line(meridian, Stroke::new(1.0, colour)));
                p.line_segment(
                    [g + vec2(-r, 0.0), g + vec2(r, 0.0)],
                    Stroke::new(1.0, colour),
                );
                for y in [-4.0_f32, 4.0] {
                    let half = (r * r - y * y).sqrt();
                    p.line_segment(
                        [g + vec2(-half, y), g + vec2(half, y)],
                        Stroke::new(1.0, colour),
                    );
                }
            }
            Self::Metronome => {
                // The case, the arm and its sliding weight, and the base.
                p.add(Shape::closed_line(
                    vec![at(-7.0, 9.0), at(-3.5, -9.0), at(3.5, -9.0), at(7.0, 9.0)],
                    line,
                ));
                p.line_segment([at(-1.0, 6.0), at(5.0, -11.0)], Stroke::new(1.3, colour));
                p.rect_filled(
                    Rect::from_center_size(at(2.4, -4.0), vec2(5.0, 3.5)),
                    1.0,
                    colour,
                );
                p.line_segment([at(-5.0, 9.0), at(5.0, 9.0)], Stroke::new(2.0, colour));
            }
            Self::Tape => {
                let body = Rect::from_center_size(c, vec2(26.0, 17.0));
                p.rect_stroke(
                    body,
                    2.5,
                    Stroke::new(1.4, colour),
                    egui::StrokeKind::Inside,
                );
                // Two reels, and the window the tape runs past.
                for x in [-6.0, 6.0] {
                    p.circle_stroke(at(x, -2.0), 3.2, Stroke::new(1.2, colour));
                    p.circle_filled(at(x, -2.0), 1.0, colour);
                }
                p.add(Shape::line(
                    vec![at(-8.0, 8.5), at(-5.0, 3.5), at(5.0, 3.5), at(8.0, 8.5)],
                    Stroke::new(1.2, colour),
                ));
            }
        }
    }
}

/// A little keyboard of five white keys and three black ones, outlined.
fn keyboard(p: &Painter, c: Pos2, size: Vec2, colour: Color32) {
    let body = Rect::from_center_size(c, size);
    p.rect_stroke(
        body,
        2.0,
        Stroke::new(1.3, colour),
        egui::StrokeKind::Inside,
    );
    let black = size.y * 0.5;
    for n in 1..5 {
        let x = body.left() + body.width() * n as f32 / 5.0;
        p.line_segment(
            [pos2(x, body.top() + black - 1.0), pos2(x, body.bottom())],
            Stroke::new(1.0, colour),
        );
    }
    for n in [1, 2, 4] {
        let x = body.left() + body.width() * n as f32 / 5.0;
        p.rect_filled(
            Rect::from_min_size(pos2(x - 1.6, body.top() + 1.0), vec2(3.2, black)),
            0.5,
            colour,
        );
    }
}

/// What the header's level meter remembers between frames: how high each channel stands (dB,
/// falling at a steady rate after a peak), where its peak was held, and a clip lamp.
#[derive(Clone, Copy, Default)]
pub struct Meter {
    level: [f32; 2],
    hold: [f32; 2],
    hold_at: [f64; 2],
    clip_at: f64,
}

/// The bottom of the scale, in dB.
const METER_FLOOR: f32 = -60.0;

impl Meter {
    pub fn new() -> Self {
        Self {
            level: [METER_FLOOR; 2],
            hold: [METER_FLOOR; 2],
            hold_at: [0.0; 2],
            clip_at: f64::NEG_INFINITY,
        }
    }

    /// Takes the peaks (linear, 0..1) the audio thread saw since the last frame. `now` is in
    /// seconds, `dt` the time since the last frame.
    pub fn feed(&mut self, peaks: (f32, f32), now: f64, dt: f32) {
        for (c, peak) in [peaks.0, peaks.1].into_iter().enumerate() {
            let db = (20.0 * peak.max(1e-6).log10()).max(METER_FLOOR);
            // Up at once, down at a steady 30 dB a second.
            self.level[c] = db.max(self.level[c] - 30.0 * dt);
            if db >= self.hold[c] {
                self.hold[c] = db;
                self.hold_at[c] = now;
            } else if now - self.hold_at[c] > 1.2 {
                self.hold[c] = db.max(self.hold[c] - 24.0 * dt);
            }
            if peak >= 0.999 {
                self.clip_at = now;
            }
        }
    }

    /// Whether anything is still moving (the header keeps repainting while it is).
    pub fn moving(&self, now: f64) -> bool {
        self.level
            .iter()
            .chain(&self.hold)
            .any(|&d| d > METER_FLOOR + 0.5)
            || now - self.clip_at < 2.0
    }
}

/// The top of the meter-fader's scale, in dB: a little above full scale, where the master volume
/// can be turned up to (its gain goes to 1.5, +3.5 dB). No lamp ever lights up there.
const FADER_TOP: f32 = 4.0;

/// The dB a master gain sits at on the scale; silence is the left end.
fn gain_to_db(gain: f32) -> f32 {
    if gain <= 0.0 {
        METER_FLOOR
    } else {
        (20.0 * gain.log10()).clamp(METER_FLOOR, FADER_TOP)
    }
}

/// The master gain a scale position means: the bottom of the scale is silence.
fn db_to_gain(db: f32) -> f32 {
    if db <= METER_FLOOR + 1.0 {
        0.0
    } else {
        10f32.powf(db / 20.0).min(1.5)
    }
}

/// How a master gain reads: `-6.0 dB`, or `off` at the bottom of the scale.
pub fn gain_text(gain: f32) -> String {
    if gain <= 0.0 {
        "off".to_owned()
    } else {
        format!("{:+.1} dB", 20.0 * gain.log10())
    }
}

/// The stereo level meter and the master fader in one: two rows of lamps (green into amber into
/// red) show what leaves the app, a lamp held at each channel's peak and a clip lamp at the end
/// that stays lit for two seconds. An amber marker across both rows is the master volume on the
/// same dB scale: drag anywhere on the lamps to move it (Shift for fine steps), scroll the wheel
/// over it, double-click to put it at 0 dB. Returns the new gain when it was changed.
pub fn meter_fader(
    ui: &mut Ui,
    meter: &Meter,
    lamps: usize,
    now: f64,
    gain: f32,
    interactive: bool,
) -> (Response, Option<f32>) {
    let lamps = lamps.max(4);
    const LAMP: Vec2 = vec2(4.0, 6.0);
    const GAP: f32 = 1.0;
    const LEFT: f32 = 16.0;
    let width = lamps as f32 * (LAMP.x + GAP) + LEFT + 8.0 + 16.0;
    let sense = if interactive {
        Sense::click_and_drag()
    } else {
        Sense::hover()
    };
    let (rect, r) = ui.allocate_exact_size(vec2(width, 26.0), sense);
    // The scale runs along the lamps.
    let x0 = rect.left() + LEFT;
    let x1 = x0 + lamps as f32 * (LAMP.x + GAP) - GAP;
    let at_db = |db: f32| x0 + (db - METER_FLOOR) / (FADER_TOP - METER_FLOOR) * (x1 - x0);
    let db_at = |x: f32| METER_FLOOR + (x - x0) / (x1 - x0) * (FADER_TOP - METER_FLOOR);

    let mut changed = None;
    if interactive {
        let id = r.id.with("fader");
        let db_now = gain_to_db(gain);
        if r.drag_started()
            && let Some(p) = r.interact_pointer_pos()
        {
            // Fine steps are relative to where the drag began; otherwise the marker jumps to
            // the pointer.
            ui.data_mut(|d| d.insert_temp(id, (p.x, db_now)));
        }
        if r.dragged()
            && let Some(p) = r.interact_pointer_pos()
        {
            let fine = ui.input(|i| i.modifiers.shift);
            let (start_x, start_db) = ui
                .data(|d| d.get_temp::<(f32, f32)>(id))
                .unwrap_or((p.x, db_now));
            let db = if fine {
                start_db + (db_at(p.x) - db_at(start_x)) * 0.2
            } else {
                db_at(p.x)
            };
            changed = Some(db_to_gain(db.clamp(METER_FLOOR, FADER_TOP)));
        } else if r.clicked()
            && !r.double_clicked()
            && let Some(p) = r.interact_pointer_pos()
        {
            changed = Some(db_to_gain(db_at(p.x).clamp(METER_FLOOR, FADER_TOP)));
        }
        if r.double_clicked() {
            changed = Some(1.0);
        }
        if r.hovered() {
            let wheel = ui.input(|i| i.smooth_scroll_delta.y);
            if wheel != 0.0 {
                // A 50-point notch is half a dB.
                changed = Some(db_to_gain(
                    (db_now + wheel * 0.01).clamp(METER_FLOOR, FADER_TOP),
                ));
            }
        }
        if changed.is_some() {
            ui.ctx().request_repaint();
        }
    }
    if !ui.is_rect_visible(rect) {
        return (r, changed);
    }
    let shown = changed.unwrap_or(gain);
    let active = interactive && (r.hovered() || r.dragged());
    let p = ui.painter();
    p.rect_filled(rect, 5.0, WELL);
    p.rect_stroke(
        rect,
        5.0,
        Stroke::new(1.0, BEVEL_LO),
        egui::StrokeKind::Inside,
    );
    // The light along the top edge of the recess.
    p.line_segment(
        [
            rect.left_top() + vec2(5.0, 0.5),
            rect.right_top() + vec2(-5.0, 0.5),
        ],
        Stroke::new(1.0, BEVEL_HI.gamma_multiply(0.25)),
    );
    let step = (FADER_TOP - METER_FLOOR) / lamps as f32;
    for (c, label) in ["L", "R"].into_iter().enumerate() {
        let top = rect.top() + 5.0 + c as f32 * (LAMP.y + 3.0);
        p.text(
            pos2(rect.left() + 8.0, top + LAMP.y / 2.0),
            Align2::CENTER_CENTER,
            label,
            FontId::proportional(8.5),
            TEXT_DIM,
        );
        for n in 0..lamps {
            // The lamp's own level: its top edge on the scale.
            let db = METER_FLOOR + (n as f32 + 1.0) * step;
            let colour = if db > -3.0 {
                RED
            } else if db > -12.0 {
                AMBER
            } else {
                GREEN
            };
            // Lit once the level reaches the middle of the lamp's range; the held peak is the
            // one lamp whose range holds it.
            let held = meter.hold[c] > db - step && meter.hold[c] <= db;
            let lit = (meter.level[c] >= db - step * 0.5 || held) && db - step <= 0.0;
            let x = x0 + n as f32 * (LAMP.x + GAP);
            let lamp = Rect::from_min_size(pos2(x, top), LAMP);
            if lit {
                p.rect_filled(lamp.expand(1.2), 1.5, colour.gamma_multiply(0.18));
                p.rect_filled(lamp, 1.0, colour);
            } else {
                p.rect_filled(lamp, 1.0, mix(WELL, colour, 0.16));
            }
        }
    }
    // The master volume: a marker across both rows, with a pointer at each end.
    let mx = at_db(gain_to_db(shown)).clamp(x0, x1);
    let mark = if active { Color32::WHITE } else { AMBER };
    p.line_segment(
        [pos2(mx, rect.top() + 2.0), pos2(mx, rect.bottom() - 2.0)],
        Stroke::new(2.0, mark),
    );
    for (y, sign) in [(rect.top() + 1.0, 1.0), (rect.bottom() - 1.0, -1.0)] {
        p.add(Shape::convex_polygon(
            vec![
                pos2(mx - 3.5, y),
                pos2(mx + 3.5, y),
                pos2(mx, y + 4.0 * sign),
            ],
            mark,
            Stroke::NONE,
        ));
    }
    // The clip lamp.
    let lamp = pos2(rect.right() - 9.0, rect.center().y);
    if now - meter.clip_at < 2.0 {
        p.circle_filled(lamp, 5.0, RED.gamma_multiply(0.25));
        p.circle_filled(lamp, 3.0, RED);
    } else {
        p.circle_filled(lamp, 3.0, mix(WELL, RED, 0.2));
    }
    (r, changed)
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
                if Cap::new(*label).lit(lit).min_width(60.0).show(ui).clicked() {
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
            .size(12.0)
            .strong()
            .color(TEXT_DIM),
    )
}

pub fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let l = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgb(l(a.r(), b.r()), l(a.g(), b.g()), l(a.b(), b.b()))
}

/// A small indicator light.
pub fn led(ui: &mut Ui, on: bool, colour: Color32) -> Response {
    let (rect, r) = ui.allocate_exact_size(vec2(12.0, 12.0), Sense::hover());
    let (c, radius) = (rect.center(), 4.0);
    let p = ui.painter();
    p.circle_filled(c, radius + 1.5, BEVEL_LO);
    if on {
        p.circle_filled(c, radius + 3.5, colour.gamma_multiply(0.18));
        p.circle_filled(c, radius, colour);
        p.circle_filled(c - vec2(1.2, 1.2), 1.4, mix(colour, Color32::WHITE, 0.7));
    } else {
        // Off is dark glass with only a hint of its colour.
        p.circle_filled(c, radius, mix(Color32::BLACK, colour, 0.10));
    }
    r
}

/// A screw head, for the corners of panels.
pub fn screw(p: &Painter, at: Pos2) {
    p.circle_filled(at + vec2(0.0, 1.0), 4.0, BEVEL_LO);
    p.circle_filled(at, 3.8, Color32::from_rgb(0x8a, 0x8d, 0x94));
    p.circle_filled(
        at - vec2(0.8, 0.8),
        2.6,
        Color32::from_rgb(0xa8, 0xab, 0xb2),
    );
    // The slot, turned a little differently on each screw.
    let a = (at.x * 7.0 + at.y * 3.0).sin() * 1.2;
    let d = vec2(a.cos(), a.sin()) * 2.8;
    p.line_segment(
        [at - d, at + d],
        Stroke::new(1.3, Color32::from_rgb(0x3a, 0x3c, 0x42)),
    );
}

/// 225 degrees (bottom left) to 45 degrees (bottom right) the long way round, in screen angles.
const KNOB_START: f32 = std::f32::consts::PI * 0.75;
const KNOB_SWEEP: f32 = std::f32::consts::PI * 1.5;

fn arc(p: &Painter, c: Pos2, radius: f32, from: f32, to: f32, stroke: Stroke) {
    let steps = (((to - from).abs() / 0.12).ceil() as usize).max(1);
    let points: Vec<Pos2> = (0..=steps)
        .map(|n| {
            let a = from + (to - from) * n as f32 / steps as f32;
            c + vec2(a.cos(), a.sin()) * radius
        })
        .collect();
    p.add(Shape::line(points, stroke));
}

/// What a knob does with the mouse: drag up and down (Shift for fine), the wheel, double-click
/// for `default`. Marks the response changed when the position moved.
fn turn(ui: &mut Ui, r: &mut Response, position: &mut f32, default: f32) {
    let before = *position;
    if r.dragged() {
        let fine = ui.input(|i| i.modifiers.shift);
        *position -= r.drag_delta().y * if fine { 0.0007 } else { 0.007 };
    }
    if r.hovered() {
        let scroll = ui.input(|i| i.smooth_scroll_delta.y);
        if scroll != 0.0 {
            *position += scroll * 0.002;
            ui.input_mut(|i| i.smooth_scroll_delta = Vec2::ZERO);
        }
    }
    if r.double_clicked() {
        *position = default;
    }
    *position = position.clamp(0.0, 1.0);
    if *position != before {
        r.mark_changed();
    }
}

/// A rotary knob. `position` is where it points, 0..=1; the caller turns that into a value.
/// Drag it up and down (Shift for fine), use the scroll wheel, double-click to go back to
/// `default`. `origin` is where the lit ring starts: 0 for most knobs, the middle for ones that
/// go both ways. `readout` writes the value under the knob.
pub fn knob(
    ui: &mut Ui,
    position: &mut f32,
    label: &str,
    readout: &dyn Fn(f32) -> String,
    default: f32,
    origin: f32,
    accent: Color32,
) -> Response {
    let (rect, mut r) = ui.allocate_exact_size(vec2(64.0, 78.0), Sense::click_and_drag());
    turn(ui, &mut r, position, default);
    if !ui.is_rect_visible(rect) {
        return r;
    }
    let enabled = ui.is_enabled();
    let accent = if enabled { accent } else { TEXT_DIM };
    let c = pos2(rect.center().x, rect.top() + 26.0);
    let p = ui.painter();
    let angle = |q: f32| KNOB_START + KNOB_SWEEP * q;
    // The ring: its track, and lit from the origin to where the knob points.
    arc(p, c, 22.0, angle(0.0), angle(1.0), Stroke::new(3.0, WELL));
    let (lo, hi) = (origin.min(*position), origin.max(*position));
    if hi - lo > 0.002 {
        arc(p, c, 22.0, angle(lo), angle(hi), Stroke::new(3.0, accent));
    }
    // The knob itself: shadow, bezel, cap, and a highlight where the light falls.
    p.circle_filled(c + vec2(0.0, 2.5), 19.5, BEVEL_LO);
    p.circle_filled(c, 19.0, Color32::from_rgb(0x55, 0x58, 0x60));
    p.circle_filled(c, 17.2, Color32::from_rgb(0x2b, 0x2d, 0x32));
    p.circle_filled(c - vec2(1.5, 2.5), 12.0, Color32::from_white_alpha(10));
    let a = angle(*position);
    let d = vec2(a.cos(), a.sin());
    p.line_segment(
        [c + d * 5.0, c + d * 15.0],
        Stroke::new(2.6, mix(accent, Color32::WHITE, 0.55)),
    );
    let active = r.dragged() || r.hovered();
    p.text(
        pos2(c.x, rect.top() + 54.0),
        Align2::CENTER_TOP,
        label.to_uppercase(),
        FontId::proportional(11.5),
        TEXT_DIM,
    );
    p.text(
        pos2(c.x, rect.top() + 65.0),
        Align2::CENTER_TOP,
        readout(*position),
        FontId::proportional(11.0),
        if active {
            mix(accent, Color32::WHITE, 0.4)
        } else {
            TEXT
        },
    );
    r
}

/// A knob small enough for a row of the rack, without a label: the same moves as [`knob`], and a
/// notch where `default` is.
pub fn mini_knob(ui: &mut Ui, position: &mut f32, default: f32, accent: Color32) -> Response {
    let (rect, mut r) = ui.allocate_exact_size(vec2(28.0, 28.0), Sense::click_and_drag());
    turn(ui, &mut r, position, default);
    if !ui.is_rect_visible(rect) {
        return r;
    }
    let accent = if ui.is_enabled() { accent } else { TEXT_DIM };
    let c = rect.center();
    let p = ui.painter();
    let angle = |q: f32| KNOB_START + KNOB_SWEEP * q;
    arc(p, c, 12.5, angle(0.0), angle(1.0), Stroke::new(2.5, WELL));
    // Lit from the notch (a gain of 1.0) to where the knob points, so "louder" and "quieter"
    // read apart.
    let (lo, hi) = (default.min(*position), default.max(*position));
    if hi - lo > 0.002 {
        arc(p, c, 12.5, angle(lo), angle(hi), Stroke::new(2.5, accent));
    }
    p.circle_filled(c + vec2(0.0, 1.5), 10.0, BEVEL_LO);
    p.circle_filled(c, 9.5, Color32::from_rgb(0x55, 0x58, 0x60));
    p.circle_filled(c, 8.3, Color32::from_rgb(0x2b, 0x2d, 0x32));
    p.circle_filled(c - vec2(1.0, 1.5), 6.0, Color32::from_white_alpha(10));
    let a = angle(*position);
    let d = vec2(a.cos(), a.sin());
    p.line_segment(
        [c + d * 2.5, c + d * 8.0],
        Stroke::new(2.0, mix(accent, Color32::WHITE, 0.55)),
    );
    r
}

/// A recessed display with a short text, for a number or name beside a control. Clickable.
pub fn display(ui: &mut Ui, text: &str, width: f32, colour: Color32) -> Response {
    let (rect, r) = ui.allocate_exact_size(vec2(width, 20.0), Sense::click());
    if ui.is_rect_visible(rect) {
        let p = ui.painter();
        p.rect_filled(rect, 4.0, WELL);
        p.rect_stroke(
            rect,
            4.0,
            Stroke::new(1.0, BEVEL_LO),
            egui::StrokeKind::Inside,
        );
        p.text(
            rect.center(),
            Align2::CENTER_CENTER,
            text,
            FontId::monospace(12.0),
            if r.hovered() {
                mix(colour, Color32::WHITE, 0.4)
            } else {
                colour
            },
        );
    }
    r
}

/// The stomp button of a pedal.
pub fn footswitch(ui: &mut Ui) -> Response {
    let (rect, r) = ui.allocate_exact_size(vec2(46.0, 46.0), Sense::click());
    let down = r.is_pointer_button_down_on();
    let c = rect.center() + if down { vec2(0.0, 1.0) } else { Vec2::ZERO };
    let p = ui.painter();
    p.circle_filled(c + vec2(0.0, 2.5), 21.0, BEVEL_LO);
    p.circle_filled(c, 20.0, Color32::from_rgb(0x9b, 0x9f, 0xa7));
    p.circle_filled(c, 17.5, Color32::from_rgb(0x4d, 0x50, 0x57));
    let dome = if down { 0x6a } else { 0x84 };
    p.circle_filled(c, 14.5, Color32::from_rgb(dome, dome + 3, dome + 8));
    p.circle_filled(
        c - vec2(2.5, 3.5),
        7.0,
        Color32::from_white_alpha(if down { 14 } else { 34 }),
    );
    r
}

/// A guitar-pedal enclosure: the effect's name and light, its knobs, and the footswitch that turns
/// it on. Returns the footswitch's response (clicked = toggled).
pub fn pedal(
    ui: &mut Ui,
    title: &str,
    colour: Color32,
    on: &mut bool,
    body: impl FnOnce(&mut Ui),
) -> Response {
    let paint = mix(colour, Color32::BLACK, if *on { 0.50 } else { 0.70 });
    let mut switch = None;
    let framed = egui::Frame::new()
        .fill(paint)
        .stroke(Stroke::new(1.5, mix(colour, Color32::BLACK, 0.25)))
        .corner_radius(10)
        .inner_margin(Margin::same(12))
        .show(ui, |ui| {
            ui.vertical(|ui| {
                ui.set_width(4.0 * 64.0 + 3.0 * 4.0);
                ui.label(egui::RichText::new(title).size(14.0).strong().color(mix(
                    colour,
                    Color32::WHITE,
                    0.75,
                )));
                ui.add_space(4.0);
                body(ui);
                ui.add_space(8.0);
                ui.vertical_centered(|ui| {
                    // Red when the pedal is on, black when it is off, like the real thing.
                    led(ui, *on, RED);
                    let mut r = footswitch(ui);
                    if r.clicked() {
                        *on = !*on;
                        r.mark_changed();
                    }
                    switch = Some(r);
                });
            });
        });
    let rect = framed.response.rect;
    for corner in [
        rect.left_top(),
        rect.right_top(),
        rect.left_bottom(),
        rect.right_bottom(),
    ] {
        let inward = vec2(
            if corner.x == rect.left() { 8.0 } else { -8.0 },
            if corner.y == rect.top() { 8.0 } else { -8.0 },
        );
        screw(ui.painter(), corner + inward);
    }
    switch.expect("the footswitch is always drawn")
}

/// A 19-inch rack unit: a wide faceplate with mounting ears and a title.
pub fn rack(ui: &mut Ui, title: &str, subtitle: &str, body: impl FnOnce(&mut Ui)) {
    let width = ui.available_width();
    let framed = egui::Frame::new()
        .fill(Color32::from_rgb(0x2d, 0x2f, 0x34))
        .stroke(Stroke::new(1.0, BEVEL_HI.gamma_multiply(0.4)))
        .corner_radius(3)
        .inner_margin(Margin {
            left: 40,
            right: 40,
            top: 8,
            bottom: 8,
        })
        .show(ui, |ui| {
            ui.set_min_width(width - 82.0);
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new(title.to_uppercase())
                        .size(13.0)
                        .strong()
                        .color(TEXT),
                );
                ui.label(egui::RichText::new(subtitle).size(11.0).color(TEXT_DIM));
            });
            ui.add_space(2.0);
            body(ui);
        });
    let rect = framed.response.rect;
    let p = ui.painter();
    for (x0, x1) in [
        (rect.left(), rect.left() + 28.0),
        (rect.right() - 28.0, rect.right()),
    ] {
        let ear = Rect::from_min_max(pos2(x0, rect.top()), pos2(x1, rect.bottom()));
        p.rect_filled(ear, 0.0, Color32::from_rgb(0x1f, 0x20, 0x24));
        let x = (x0 + x1) / 2.0;
        screw(p, pos2(x, rect.top() + 14.0));
        screw(p, pos2(x, rect.bottom() - 14.0));
    }
}

/// A row of lamps that fill up with `fraction` (0..=1): a level or gain-reduction meter.
pub fn meter(ui: &mut Ui, fraction: f32, colour: Color32, label: &str) {
    const LAMPS: usize = 14;
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(label).size(11.5).color(TEXT_DIM));
        let (rect, _) =
            ui.allocate_exact_size(vec2(LAMPS as f32 * 9.0 + 4.0, 12.0), Sense::hover());
        let p = ui.painter();
        p.rect_filled(rect, 3.0, WELL);
        let lit = (fraction.clamp(0.0, 1.0) * LAMPS as f32).round() as usize;
        for n in 0..LAMPS {
            let at = Rect::from_min_size(
                pos2(rect.left() + 3.0 + n as f32 * 9.0, rect.top() + 3.0),
                vec2(7.0, 6.0),
            );
            p.rect_filled(
                at,
                1.0,
                if n < lit {
                    colour
                } else {
                    colour.gamma_multiply(0.18)
                },
            );
        }
    });
}

/// The pull of a drawer: a slim bar across the width with a chevron and a name. `chevron_up` says
/// which way the drawer would move if pulled.
pub fn drawer_handle(ui: &mut Ui, label: &str, detail: &str, chevron_up: bool) -> Response {
    let (rect, r) = ui.allocate_exact_size(vec2(ui.available_width(), 24.0), Sense::click());
    let p = ui.painter();
    let body = if r.hovered() {
        mix(PLATE, Color32::WHITE, 0.06)
    } else {
        PLATE
    };
    p.rect_filled(rect, 4.0, body);
    p.line_segment(
        [
            rect.left_top() + vec2(4.0, 0.5),
            rect.right_top() + vec2(-4.0, 0.5),
        ],
        Stroke::new(1.0, BEVEL_HI.gamma_multiply(0.45)),
    );
    p.line_segment(
        [
            rect.left_bottom() + vec2(4.0, -0.5),
            rect.right_bottom() + vec2(-4.0, -0.5),
        ],
        Stroke::new(1.0, BEVEL_LO),
    );
    // The chevron.
    let c = pos2(rect.left() + 18.0, rect.center().y);
    let tip = if chevron_up { -3.0 } else { 3.0 };
    p.add(Shape::line(
        vec![
            c + vec2(-5.0, -tip),
            c + vec2(0.0, tip),
            c + vec2(5.0, -tip),
        ],
        Stroke::new(2.0, TEXT),
    ));
    p.text(
        pos2(rect.left() + 34.0, rect.center().y),
        Align2::LEFT_CENTER,
        label.to_uppercase(),
        FontId::proportional(12.0),
        TEXT,
    );
    p.text(
        pos2(rect.right() - 12.0, rect.center().y),
        Align2::RIGHT_CENTER,
        detail,
        FontId::proportional(11.0),
        TEXT_DIM,
    );
    r
}

/// A strip to drag to change the height of what is above it. Returns how far it was dragged this
/// frame (positive: down).
pub fn grip(ui: &mut Ui) -> f32 {
    let (rect, r) = ui.allocate_exact_size(vec2(ui.available_width(), 10.0), Sense::drag());
    let r = r
        .on_hover_cursor(egui::CursorIcon::ResizeVertical)
        .on_hover_text("Drag to change the height");
    let lit = r.hovered() || r.dragged();
    let c = rect.center();
    let p = ui.painter();
    p.rect_filled(
        Rect::from_center_size(c, vec2(rect.width(), 4.0)),
        2.0,
        if lit {
            mix(WELL, Color32::WHITE, 0.10)
        } else {
            WELL
        },
    );
    for dx in [-14.0, 0.0, 14.0] {
        p.circle_filled(c + vec2(dx, 0.0), 1.6, if lit { TEXT } else { TEXT_DIM });
    }
    r.drag_delta().y
}

/// Which way a [`scrollbar`] runs.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Axis {
    Horizontal,
    Vertical,
}

/// A scroll bar drawn in `rect`: a recessed track with a raised thumb. `offset`, `view` and
/// `content` are in the same units; returns whether `offset` changed.
pub fn scrollbar(
    ui: &mut Ui,
    rect: Rect,
    axis: Axis,
    offset: &mut f32,
    view: f32,
    content: f32,
) -> bool {
    let along = |v: Vec2| if axis == Axis::Horizontal { v.x } else { v.y };
    let length = along(rect.size());
    let range = (content - view).max(0.0);
    let thumb = (length * view / content.max(1.0)).clamp(28.0_f32.min(length), length);
    let travel = (length - thumb).max(1.0);
    let id = ui.make_persistent_id(("editor-scrollbar", axis == Axis::Horizontal));
    let r = ui.interact(rect, id, Sense::click_and_drag());
    let before = *offset;
    if let Some(pointer) = r.interact_pointer_pos()
        && range > 0.0
    {
        // The thumb's middle follows the pointer.
        let from_start = along(pointer - rect.min) - thumb / 2.0;
        *offset = (from_start / travel * range).clamp(0.0, range);
    }
    *offset = offset.clamp(0.0, range);
    let at = if range > 0.0 {
        *offset / range * travel
    } else {
        0.0
    };
    let thumb_rect = match axis {
        Axis::Horizontal => Rect::from_min_size(
            pos2(rect.left() + at, rect.top()),
            vec2(thumb, rect.height()),
        ),
        Axis::Vertical => Rect::from_min_size(
            pos2(rect.left(), rect.top() + at),
            vec2(rect.width(), thumb),
        ),
    };
    let p = ui.painter();
    p.rect_filled(rect, 4.0, WELL);
    let lit = r.hovered() || r.dragged();
    let body = if lit {
        mix(CAP, Color32::WHITE, 0.10)
    } else {
        CAP
    };
    let thumb_rect = thumb_rect.shrink(1.5);
    p.rect_filled(thumb_rect, 4.0, body);
    p.rect_stroke(
        thumb_rect,
        4.0,
        Stroke::new(1.0, BEVEL_HI.gamma_multiply(0.5)),
        egui::StrokeKind::Inside,
    );
    // Three ridges to grip.
    let c = thumb_rect.center();
    for d in [-4.0, 0.0, 4.0] {
        let (a, b) = match axis {
            Axis::Horizontal => (c + vec2(d, -3.0), c + vec2(d, 3.0)),
            Axis::Vertical => (c + vec2(-3.0, d), c + vec2(3.0, d)),
        };
        p.line_segment([a, b], Stroke::new(1.0, TEXT_DIM));
    }
    *offset != before
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_master_gain_and_its_place_on_the_scale_agree() {
        // Unity is 0 dB; the top of the knob's range is +3.5 dB, inside the scale.
        assert!(gain_to_db(1.0).abs() < 1e-4);
        assert!((gain_to_db(1.5) - 3.52).abs() < 0.01 && gain_to_db(1.5) < FADER_TOP);
        // Silence is the left end, and the left end is silence.
        assert_eq!(gain_to_db(0.0), METER_FLOOR);
        assert_eq!(db_to_gain(METER_FLOOR), 0.0);
        assert_eq!(db_to_gain(METER_FLOOR + 0.5), 0.0);
        // The top of the scale does not go past what the engine takes.
        assert_eq!(db_to_gain(FADER_TOP), 1.5);
        for g in [0.01_f32, 0.1, 0.5, 0.707, 1.0, 1.4] {
            assert!((db_to_gain(gain_to_db(g)) - g).abs() < 1e-3, "{g}");
        }
    }

    #[test]
    fn the_gain_reads_in_db() {
        assert_eq!(gain_text(1.0), "+0.0 dB");
        assert_eq!(gain_text(0.5), "-6.0 dB");
        assert_eq!(gain_text(0.0), "off");
    }

    #[test]
    fn the_meter_rises_at_once_and_falls_steadily() {
        let mut m = Meter::new();
        m.feed((1.0, 0.0), 0.0, 0.03);
        assert!(m.level[0] > -0.1 && m.level[1] <= METER_FLOOR + 0.1);
        m.feed((0.0, 0.0), 0.5, 0.5);
        assert!(m.level[0] < -10.0 && m.level[0] > -20.0, "{}", m.level[0]);
        // The peak lamp stays for a moment, then falls too.
        assert!(m.hold[0] > -0.1);
        m.feed((0.0, 0.0), 2.0, 0.5);
        assert!(m.hold[0] < -5.0);
        assert!(m.moving(2.0));
    }
}
