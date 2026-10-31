//! The zones view: the whole keyboard (A0 to C8) with one lane above it for each instrument in
//! the rack. A lane's coloured band is the keys that instrument plays: paint it with the mouse,
//! drag it to move it, drag its ends to resize. The tag on the band is the MIDI channel.

use std::sync::atomic::Ordering;

use egui::{
    Align, Align2, Color32, CornerRadius, CursorIcon, FontId, Layout, Pos2, Rect, Response, Sense,
    Stroke, StrokeKind, Ui, UiBuilder, pos2, vec2,
};

use crate::hw;
use crate::keyboard::{COLORS, is_black};
use crate::session::{Session, UI_SLOTS};
use crate::ui::{Action, UiState};

/// The keys of an 88-key piano: A0 to C8.
pub const FIRST: u8 = 21;
pub const LAST: u8 = 108;

const LANE_H: f32 = 30.0;
const LANE_GAP: f32 = 5.0;
/// Room under the lanes for the octave names and the dots of held notes.
const KEY_FOOT: f32 = 42.0;
const MIN_KEY_H: f32 = 120.0;
/// The column on the left of the lanes: the instrument's name and its transpose.
const GUTTER: f32 = 244.0;
const TAG_W: f32 = 42.0;
/// How near to the end of a band (points) a press takes hold of the end.
const EDGE: f32 = 7.0;

// ---- the keys ---------------------------------------------------------------------------

/// Where the 52 white keys of the piano sit across `width`, starting at `x0`.
struct Geometry {
    x0: f32,
    key_w: f32,
    whites: Vec<u8>,
}

impl Geometry {
    fn new(x0: f32, width: f32) -> Self {
        let whites: Vec<u8> = (FIRST..=LAST).filter(|k| !is_black(*k)).collect();
        Self {
            x0,
            key_w: width / whites.len() as f32,
            whites,
        }
    }

    fn black_w(&self) -> f32 {
        self.key_w * 0.62
    }

    /// How many white keys lie below `key`.
    fn whites_below(&self, key: u8) -> usize {
        self.whites.partition_point(|&w| w < key)
    }

    /// The left and right edge of a key. A black key sits across the line between two white ones.
    fn span(&self, key: u8) -> (f32, f32) {
        let key = key.clamp(FIRST, LAST);
        if is_black(key) {
            let line = self.x0 + self.whites_below(key) as f32 * self.key_w;
            (line - self.black_w() / 2.0, line + self.black_w() / 2.0)
        } else {
            let left = self.x0 + self.whites_below(key) as f32 * self.key_w;
            (left, left + self.key_w)
        }
    }

    /// The key under `x`; a black key wins where it overlaps its white neighbours.
    fn key_at(&self, x: f32) -> u8 {
        let w = ((x - self.x0) / self.key_w).floor();
        let white = self.whites[(w.max(0.0) as usize).min(self.whites.len() - 1)];
        [white.wrapping_add(1), white.wrapping_sub(1)]
            .into_iter()
            .filter(|k| (FIRST..=LAST).contains(k) && is_black(*k))
            .find(|&k| {
                let (l, r) = self.span(k);
                (l..r).contains(&x)
            })
            .unwrap_or(white)
    }
}

// ---- painting a zone --------------------------------------------------------------------

/// What a drag in a lane is doing to the zone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Grab {
    /// Painting a new zone from the key where the press began.
    Paint {
        anchor: u8,
    },
    /// Carrying the band: the key that was grabbed and the zone as it was.
    Move {
        from: u8,
        lo: u8,
        hi: u8,
    },
    /// Pulling one end of the band.
    Lo,
    Hi,
}

/// The piano ends at A0 and C8, but MIDI goes on: a zone that reaches the end of the piano
/// reaches the end of the keys.
fn snap_lo(lo: u8) -> u8 {
    if lo <= FIRST { 0 } else { lo }
}

fn snap_hi(hi: u8) -> u8 {
    if hi >= LAST { 127 } else { hi }
}

/// The zone after a drag has brought the pointer to `key`.
fn dragged(grab: Grab, key: u8, (lo, hi): (u8, u8)) -> (u8, u8) {
    match grab {
        Grab::Paint { anchor } => (snap_lo(anchor.min(key)), snap_hi(anchor.max(key))),
        Grab::Lo => (snap_lo(key.min(hi)), hi),
        Grab::Hi => (lo, snap_hi(key.max(lo))),
        Grab::Move { from, lo, hi } => {
            let by = (i16::from(key) - i16::from(from)).clamp(-i16::from(lo), 127 - i16::from(hi));
            ((i16::from(lo) + by) as u8, (i16::from(hi) + by) as u8)
        }
    }
}

// ---- names ------------------------------------------------------------------------------

/// `C4` for middle C (MIDI 60).
pub fn note_name(key: u8) -> String {
    const NAMES: [&str; 12] = [
        "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
    ];
    format!(
        "{}{}",
        NAMES[usize::from(key % 12)],
        i16::from(key / 12) - 1
    )
}

/// `C2\u{2013}B4`, or `ALL KEYS` when the zone is the whole keyboard.
pub fn range_text((lo, hi): (u8, u8)) -> String {
    if (lo, hi) == (0, 127) {
        "ALL KEYS".to_owned()
    } else if lo == hi {
        note_name(lo)
    } else {
        format!("{}\u{2013}{}", note_name(lo), note_name(hi))
    }
}

/// `OMNI` or `CH 3`.
pub fn channel_text(channel: Option<u8>) -> String {
    match channel {
        None => "OMNI".to_owned(),
        Some(c) => format!("CH {}", c + 1),
    }
}

/// What a rack unit says about a slot's mapping: `C2\u{2013}B4 \u{b7} CH 3 \u{b7} +12`.
pub fn summary(f: &merlin_model::MidiFilter) -> String {
    let mut text = format!(
        "{} \u{b7} {}",
        range_text((f.key_lo, f.key_hi)),
        channel_text(f.channel)
    );
    if f.transpose != 0 {
        text.push_str(&format!(" \u{b7} {:+}", f.transpose));
    }
    text
}

// ---- the view ---------------------------------------------------------------------------

/// Draws the view: the piano, long enough to hold a lane for each instrument across its keys.
/// It sets zones and channels; it does not play, so a drag never sounds a note.
pub fn show(ui: &mut Ui, s: &Session, st: &mut UiState, actions: &mut Vec<Action>) {
    let slots: Vec<usize> = (0..UI_SLOTS)
        .filter(|&i| s.rig.slots[i].instrument.is_some())
        .collect();
    let width = ui.available_width();
    let geo = Geometry::new(
        ui.available_rect_before_wrap().left() + GUTTER,
        (width - GUTTER - 6.0).max(52.0 * 8.0),
    );

    // The toolbar is laid out last, once the key under the pointer is known.
    let (bar, _) = ui.allocate_exact_size(vec2(width, 28.0), Sense::hover());
    ui.add_space(4.0);

    // The keys are as tall as the lanes laid across them need, plus a strip below for the octave
    // names and the dots of notes being played.
    let lanes_h = slots.len() as f32 * (LANE_H + LANE_GAP);
    let key_h = (lanes_h + LANE_GAP + KEY_FOOT).max(MIN_KEY_H);
    let (rect, _) = ui.allocate_exact_size(vec2(width, key_h + 12.0), Sense::hover());
    let top = rect.top() + 6.0;
    paint_keys(ui, &geo, rect, top, key_h);

    for (n, &i) in slots.iter().enumerate() {
        let row = Rect::from_min_size(
            pos2(rect.left(), top + LANE_GAP + n as f32 * (LANE_H + LANE_GAP)),
            vec2(width, LANE_H),
        );
        lane(ui, s, st, i, &geo, row, actions);
    }
    if slots.is_empty() {
        ui.painter().text(
            pos2(geo.x0 + 20.0, top + 24.0),
            Align2::LEFT_CENTER,
            "Load an instrument into the rack to give it keys to play.",
            FontId::proportional(14.0),
            Color32::from_gray(90),
        );
    }
    paint_held(ui, s, &geo, top, key_h);

    let keys = Rect::from_min_max(pos2(geo.x0, top), pos2(geo.span(LAST).1, top + key_h));
    let hover = ui
        .input(|i| i.pointer.hover_pos())
        .filter(|p| keys.contains(*p))
        .map(|p| geo.key_at(p.x));
    ui.scope_builder(
        UiBuilder::new()
            .max_rect(bar)
            .layout(Layout::left_to_right(Align::Center)),
        |ui| toolbar(ui, s, &slots, hover, actions),
    );
}

fn toolbar(
    ui: &mut Ui,
    s: &Session,
    slots: &[usize],
    hover: Option<u8>,
    actions: &mut Vec<Action>,
) {
    if hw::Cap::new("ALL KEYS")
        .show(ui)
        .on_hover_text("Every instrument plays the whole keyboard again")
        .clicked()
    {
        for &i in slots {
            actions.push(Action::Zone(i, 0, 127));
        }
    }
    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
        if hw::Cap::new("OMNI")
            .show(ui)
            .on_hover_text("Every instrument listens to all 16 MIDI channels again")
            .clicked()
        {
            for &i in slots {
                actions.push(Action::Channel(i, None));
            }
        }
        // What is under the pointer, or how the view works.
        ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
            ui.add_space(8.0);
            let text = match hover {
                Some(k) => format!("{} \u{b7} MIDI {k}", note_name(k)),
                None if s.map_mode => "MIDI map mode is on".to_owned(),
                None => "Drag across a row to paint the keys it plays \u{b7} drag a band to move it, its ends to resize it \u{b7} click the tag for the MIDI channel".to_owned(),
            };
            ui.add(
                egui::Label::new(egui::RichText::new(text).size(12.5).color(hw::TEXT_DIM))
                    .truncate(),
            );
        });
    });
}

/// One instrument's lane: its name and transpose in the gutter, its band on the track.
fn lane(
    ui: &mut Ui,
    s: &Session,
    st: &mut UiState,
    i: usize,
    geo: &Geometry,
    row: Rect,
    actions: &mut Vec<Action>,
) {
    let slot = &s.rig.slots[i];
    let colour = COLORS[i];
    let live = s.plugins[i].is_some() && s.targets.contains(&i) && !slot.mute;
    let zone = (slot.filter.key_lo, slot.filter.key_hi);
    let channel = slot.filter.channel;

    let track = Rect::from_min_max(
        pos2(geo.x0, row.top()),
        pos2(geo.span(LAST).1, row.bottom()),
    );
    gutter(
        ui,
        s,
        i,
        Rect::from_min_max(row.min, pos2(geo.x0 - 8.0, row.bottom())),
        live,
        actions,
    );

    let band = band_rect(geo, track, zone);
    let tag = tag_rect(band, track);

    let (r, changed) = lane_input(ui, i, geo, track, band, tag, zone, &mut st.zone_drag);
    if let Some(at) = changed {
        actions.push(Action::Zone(i, at.0, at.1));
    }
    paint_lane(
        ui,
        track,
        band,
        tag,
        colour,
        live,
        zone,
        channel,
        r.hovered(),
    );

    // The tag opens the channel picker; right-click anywhere on the lane has the resets.
    let tag_r = ui.interact(tag, ui.scope_id().with(("zone-tag", i)), Sense::click());
    tag_r.clone().on_hover_text(match channel {
        None => "MIDI channel: all of them (omni). Click to listen to one channel only".to_owned(),
        Some(c) => format!("Listens to MIDI channel {} only. Click to change", c + 1),
    });
    egui::Popup::menu(&tag_r).show(|ui| {
        hw::engraved(ui, "MIDI channel");
        if hw::Cap::new("OMNI \u{b7} all channels")
            .lit(channel.is_none().then_some(hw::AMBER))
            .min_width(4.0 * 44.0 + 3.0 * 4.0)
            .show(ui)
            .clicked()
        {
            actions.push(Action::Channel(i, None));
        }
        for row in 0..4u8 {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                for col in 0..4u8 {
                    let c = row * 4 + col;
                    if hw::Cap::new((c + 1).to_string())
                        .lit((channel == Some(c)).then_some(colour))
                        .min_width(44.0)
                        .show(ui)
                        .clicked()
                    {
                        actions.push(Action::Channel(i, Some(c)));
                    }
                }
            });
        }
    });
    r.context_menu(|ui| {
        if ui.button("Play the whole keyboard").clicked() {
            actions.push(Action::Zone(i, 0, 127));
            ui.close();
        }
        if ui.button("Listen to every channel").clicked() {
            actions.push(Action::Channel(i, None));
            ui.close();
        }
        if ui.button("No transpose").clicked() {
            actions.push(Action::Transpose(i, 0));
            ui.close();
        }
    });
}

/// What the pointer does to lane `i`: it starts, goes on with and ends a drag, and a click on
/// bare track. Returns the lane's response and the zone it is to become, if the pointer changed it.
#[allow(clippy::too_many_arguments)]
fn lane_input(
    ui: &mut Ui,
    i: usize,
    geo: &Geometry,
    track: Rect,
    band: Rect,
    tag: Rect,
    zone: (u8, u8),
    drag: &mut Option<(usize, Grab)>,
) -> (Response, Option<(u8, u8)>) {
    let (band_l, band_r) = (band.left(), band.right());
    let r = ui.interact(
        track,
        ui.scope_id().with(("zone-lane", i)),
        Sense::click_and_drag(),
    );
    let press = ui.input(|inp| inp.pointer.press_origin());
    let at = r.interact_pointer_pos().or(r.hover_pos());
    let mut changed = None;

    // What a press would take hold of, for the cursor and for starting a drag.
    let grab_at = |p: Pos2| {
        let key = geo.key_at(p.x);
        let (near_l, near_r) = ((p.x - band_l).abs(), (p.x - band_r).abs());
        if near_l <= EDGE && near_l <= near_r {
            Grab::Lo
        } else if near_r <= EDGE {
            Grab::Hi
        } else if band.contains(p) {
            Grab::Move {
                from: key,
                lo: zone.0,
                hi: zone.1,
            }
        } else {
            Grab::Paint { anchor: key }
        }
    };
    if r.drag_started()
        && let Some(p) = press.filter(|p| !tag.contains(*p))
    {
        *drag = Some((i, grab_at(p)));
    }
    if let Some((lane, grab)) = *drag
        && lane == i
    {
        if r.dragged()
            && let Some(p) = at
        {
            let now = dragged(grab, geo.key_at(p.x), zone);
            if now != zone {
                changed = Some(now);
            }
        }
        if r.drag_stopped() {
            *drag = None;
        }
    }
    // A click on bare track is one key; a click on the band does nothing (the tag has its own).
    if r.clicked()
        && !tag.contains(press.unwrap_or(Pos2::ZERO))
        && let Some(p) = r.interact_pointer_pos().filter(|p| !band.contains(*p))
    {
        let key = geo.key_at(p.x);
        changed = Some((key, key));
    }
    if r.hovered() || r.dragged() {
        let grab = match *drag {
            Some((lane, g)) if lane == i => Some(g),
            _ => r.hover_pos().filter(|p| !tag.contains(*p)).map(grab_at),
        };
        ui.ctx().set_cursor_icon(match grab {
            Some(Grab::Lo | Grab::Hi) => CursorIcon::ResizeHorizontal,
            Some(Grab::Move { .. }) => {
                if r.dragged() {
                    CursorIcon::Grabbing
                } else {
                    CursorIcon::Grab
                }
            }
            Some(Grab::Paint { .. }) => CursorIcon::Crosshair,
            None => CursorIcon::Default,
        });
    }
    (r, changed)
}

/// The band of a zone on its track: from the left edge of its first key to the right edge of its
/// last.
fn band_rect(geo: &Geometry, track: Rect, (lo, hi): (u8, u8)) -> Rect {
    Rect::from_min_max(
        pos2(geo.span(lo.clamp(FIRST, LAST)).0, track.top() + 3.0),
        pos2(geo.span(hi.clamp(FIRST, LAST)).1, track.bottom() - 3.0),
    )
}

/// Where the channel tag sits: at the start of the band, or beside it when the band is too short.
fn tag_rect(band: Rect, track: Rect) -> Rect {
    let h = band.height() - 6.0;
    let x = if band.width() >= TAG_W + 8.0 {
        band.left() + 4.0
    } else if band.left() - TAG_W - 4.0 >= track.left() {
        band.left() - TAG_W - 4.0
    } else {
        band.right() + 4.0
    };
    Rect::from_min_size(pos2(x, band.center().y - h / 2.0), vec2(TAG_W, h))
}

#[allow(clippy::too_many_arguments)]
fn paint_lane(
    ui: &Ui,
    track: Rect,
    band: Rect,
    tag: Rect,
    colour: Color32,
    live: bool,
    zone: (u8, u8),
    channel: Option<u8>,
    hovered: bool,
) {
    let p = ui.painter();
    // The lane is a smoked strip across the keys; the band is lit glass over them.
    p.rect_filled(
        track,
        4.0,
        Color32::from_black_alpha(if hovered { 105 } else { 80 }),
    );
    p.rect_stroke(
        track,
        4.0,
        Stroke::new(1.0, Color32::from_black_alpha(150)),
        StrokeKind::Inside,
    );
    // The band lights up like a lamp while its instrument takes the keyboard, and sits dull
    // when it is not selected or muted.
    let (r, g, b) = if live {
        (colour.r(), colour.g(), colour.b())
    } else {
        let d = hw::mix(Color32::BLACK, colour, 0.45);
        (d.r(), d.g(), d.b())
    };
    let body = Color32::from_rgba_unmultiplied(r, g, b, if live { 215 } else { 195 });
    p.rect_filled(band, 5.0, body);
    p.rect_filled(
        Rect::from_min_size(band.min, vec2(band.width(), band.height() * 0.45))
            .shrink2(vec2(1.0, 1.0)),
        CornerRadius {
            nw: 4,
            ne: 4,
            sw: 0,
            se: 0,
        },
        Color32::from_white_alpha(if live { 38 } else { 16 }),
    );
    p.rect_stroke(
        band,
        5.0,
        Stroke::new(
            1.0,
            hw::mix(colour, Color32::WHITE, if live { 0.55 } else { 0.15 }),
        ),
        StrokeKind::Inside,
    );
    // The ends say there is more of the zone beyond the piano.
    for (open, edge, dir) in [
        (zone.0 < FIRST, band.left(), 1.0),
        (zone.1 > LAST, band.right(), -1.0),
    ] {
        if open {
            for n in 0..2 {
                let x = edge + dir * (3.0 + n as f32 * 4.0);
                p.line_segment(
                    [pos2(x, band.top() + 4.0), pos2(x, band.bottom() - 4.0)],
                    Stroke::new(1.0, Color32::from_black_alpha(90)),
                );
            }
        }
    }
    // The zone in words, when the band is wide enough to hold it beside the tag.
    let text = range_text(zone);
    let free = if tag.left() >= band.left() && tag.right() <= band.right() {
        Rect::from_min_max(pos2(tag.right() + 6.0, band.top()), band.right_bottom())
    } else {
        band
    };
    if free.width() >= 64.0 {
        p.text(
            pos2(free.right() - 8.0, band.center().y),
            Align2::RIGHT_CENTER,
            text,
            FontId::proportional(12.0),
            Color32::WHITE,
        );
    }
    // The tag: an engraved plate.
    p.rect_filled(
        tag.translate(vec2(0.0, 1.0)),
        4.0,
        Color32::from_black_alpha(120),
    );
    p.rect_filled(tag, 4.0, hw::mix(Color32::BLACK, colour, 0.35));
    p.rect_stroke(
        tag,
        4.0,
        Stroke::new(1.0, hw::mix(colour, Color32::WHITE, 0.35)),
        StrokeKind::Inside,
    );
    p.text(
        tag.center(),
        Align2::CENTER_CENTER,
        channel_text(channel),
        FontId::proportional(11.5),
        Color32::WHITE,
    );
}

/// The left of a lane: the slot's light, number and name, and its transpose.
fn gutter(ui: &mut Ui, s: &Session, i: usize, rect: Rect, live: bool, actions: &mut Vec<Action>) {
    let slot = &s.rig.slots[i];
    let colour = COLORS[i];
    ui.scope_builder(
        UiBuilder::new()
            .max_rect(rect)
            .layout(Layout::left_to_right(Align::Center)),
        |ui| {
            ui.spacing_mut().item_spacing.x = 5.0;
            hw::led(ui, live, hw::RED).on_hover_text(if live {
                "Receives the keyboard"
            } else {
                "Not selected, or muted: it does not play the keyboard"
            });
            ui.colored_label(colour, (i + 1).to_string());
            let name = slot.title();
            let r = ui
                .add_sized(
                    [92.0, 20.0],
                    egui::Button::selectable(s.targets.contains(&i), name)
                        .frame(false)
                        .truncate(),
                )
                .on_hover_text(format!(
                    "{name}\nClick to play it from the keyboard (Shift: together with the others)"
                ));
            if r.clicked() {
                actions.push(Action::Select(i, ui.input(|inp| inp.modifiers.shift)));
            }
            let tr = slot.filter.transpose;
            let step = |ui: &Ui| {
                if ui.input(|inp| inp.modifiers.shift) {
                    12
                } else {
                    1
                }
            };
            if hw::Cap::new("\u{2212}")
                .min_width(22.0)
                .show(ui)
                .on_hover_text("Transpose down a semitone (Shift: an octave)")
                .clicked()
            {
                actions.push(Action::Transpose(
                    i,
                    (i32::from(tr) - step(ui)).max(-48) as i8,
                ));
            }
            if hw::display(
                ui,
                &if tr == 0 {
                    "0".to_owned()
                } else {
                    format!("{tr:+}")
                },
                38.0,
                if tr == 0 { hw::TEXT_DIM } else { hw::AMBER },
            )
            .on_hover_text("Transpose in semitones. Click to put it back to 0")
            .clicked()
            {
                actions.push(Action::Transpose(i, 0));
            }
            if hw::Cap::new("+")
                .min_width(22.0)
                .show(ui)
                .on_hover_text("Transpose up a semitone (Shift: an octave)")
                .clicked()
            {
                actions.push(Action::Transpose(
                    i,
                    (i32::from(tr) + step(ui)).min(48) as i8,
                ));
            }
        },
    );
}

/// The piano: long keys on a bed under a red rail. The lanes are drawn over them.
fn paint_keys(ui: &Ui, geo: &Geometry, rect: Rect, top: f32, key_h: f32) {
    let width = geo.span(LAST).1 - geo.x0;
    let p = ui.painter();
    let bed = Rect::from_min_size(
        pos2(geo.x0 - 3.0, rect.top()),
        vec2(width + 6.0, key_h + 12.0),
    );
    p.rect_filled(bed, 3.0, Color32::from_rgb(0x0b, 0x0b, 0x0d));
    p.rect_filled(
        Rect::from_min_size(bed.min, vec2(bed.width(), 5.0)),
        CornerRadius {
            nw: 3,
            ne: 3,
            sw: 0,
            se: 0,
        },
        Color32::from_rgb(0x4a, 0x1c, 0x22),
    );

    for &k in geo.whites.iter() {
        let (l, r) = geo.span(k);
        let key = Rect::from_min_max(pos2(l + 0.5, top), pos2(r - 0.5, top + key_h));
        let face = Color32::from_gray(236);
        let round = CornerRadius {
            nw: 0,
            ne: 0,
            sw: 4,
            se: 4,
        };
        // The key's lower lip is shaded, as on a real one.
        p.rect_filled(key, round, hw::mix(face, Color32::BLACK, 0.18));
        p.rect_filled(
            Rect::from_min_max(key.min, pos2(key.right(), key.bottom() - 5.0)),
            round,
            face,
        );
        if k % 12 == 0 {
            p.text(
                pos2(key.center().x, key.bottom() - 12.0),
                Align2::CENTER_CENTER,
                note_name(k),
                FontId::proportional(10.5),
                Color32::from_gray(90),
            );
        }
    }
    for k in (FIRST..=LAST).filter(|k| is_black(*k)) {
        let (l, r) = geo.span(k);
        let key = Rect::from_min_max(pos2(l, top), pos2(r, top + key_h * 0.62));
        let face = Color32::from_gray(28);
        p.rect_filled(
            key.translate(vec2(0.0, 1.5)),
            2.0,
            Color32::from_black_alpha(140),
        );
        p.rect_filled(key, 2.0, face);
        p.rect_filled(
            Rect::from_min_max(
                pos2(key.left() + 2.0, key.top()),
                pos2(key.right() - 2.0, key.bottom() - 7.0),
            ),
            2.0,
            hw::mix(face, Color32::WHITE, 0.10),
        );
    }
}

/// A dot in the strip under the keys for each note being played (from a MIDI keyboard), in the
/// colour of the instrument that has it.
fn paint_held(ui: &Ui, s: &Session, geo: &Geometry, top: f32, key_h: f32) {
    let p = ui.painter();
    for k in FIRST..=LAST {
        let owners = s.engine.shared.held[usize::from(k)].load(Ordering::Relaxed);
        if owners != 0 {
            let (l, r) = geo.span(k);
            let slot = (owners.trailing_zeros() as usize).min(UI_SLOTS - 1);
            let at = pos2((l + r) / 2.0, top + key_h - 28.0);
            p.circle_filled(at, 5.0, COLORS[slot]);
            p.circle_stroke(at, 5.0, Stroke::new(1.5, Color32::WHITE));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_key_is_found_where_it_is_drawn() {
        let geo = Geometry::new(100.0, 1040.0);
        for k in FIRST..=LAST {
            let (l, r) = geo.span(k);
            assert_eq!(geo.key_at((l + r) / 2.0), k, "key {k}");
        }
        assert_eq!(geo.key_at(0.0), FIRST);
        assert_eq!(geo.key_at(5000.0), LAST);
    }

    #[test]
    fn painting_reaches_past_the_piano_when_it_reaches_its_end() {
        let from = |anchor, key| dragged(Grab::Paint { anchor }, key, (0, 127));
        assert_eq!(from(60, 71), (60, 71));
        assert_eq!(from(71, 60), (60, 71), "either way round");
        assert_eq!(from(60, 60), (60, 60), "a single key");
        assert_eq!(from(FIRST, 71), (0, 71));
        assert_eq!(from(60, LAST), (60, 127));
    }

    #[test]
    fn ends_stay_in_order() {
        assert_eq!(dragged(Grab::Lo, 90, (40, 60)), (60, 60));
        assert_eq!(dragged(Grab::Hi, 30, (40, 60)), (40, 40));
        assert_eq!(dragged(Grab::Lo, FIRST, (40, 60)), (0, 60));
        assert_eq!(dragged(Grab::Hi, LAST, (40, 60)), (40, 127));
    }

    #[test]
    fn moving_keeps_the_width_and_the_ends_stop_the_move() {
        let grab = Grab::Move {
            from: 50,
            lo: 40,
            hi: 60,
        };
        assert_eq!(dragged(grab, 55, (40, 60)), (45, 65));
        assert_eq!(dragged(grab, 0, (40, 60)), (0, 20));
        assert_eq!(dragged(grab, 120, (40, 60)), (107, 127));
    }

    #[test]
    fn names() {
        assert_eq!(note_name(60), "C4");
        assert_eq!(note_name(21), "A0");
        assert_eq!(note_name(108), "C8");
        assert_eq!(note_name(61), "C#4");
        assert_eq!(range_text((0, 127)), "ALL KEYS");
        assert_eq!(range_text((36, 59)), "C2\u{2013}B3");
        let f = merlin_model::MidiFilter {
            channel: Some(2),
            transpose: -12,
            key_lo: 60,
            key_hi: 60,
            ..Default::default()
        };
        assert_eq!(summary(&f), "C4 \u{b7} CH 3 \u{b7} -12");
    }

    /// A lane in a window of its own, driven by pointer events a frame at a time.
    struct Bench {
        ctx: egui::Context,
        geo: Geometry,
        time: f64,
        zone: (u8, u8),
        drag: Option<(usize, Grab)>,
    }

    impl Bench {
        fn new(zone: (u8, u8)) -> Self {
            let mut bench = Self {
                ctx: egui::Context::default(),
                geo: Geometry::new(100.0, 1040.0),
                time: 0.0,
                zone,
                drag: None,
            };
            bench.frame(vec![]);
            bench
        }

        fn track(&self) -> Rect {
            Rect::from_min_max(pos2(self.geo.x0, 10.0), pos2(self.geo.span(LAST).1, 42.0))
        }

        fn tag(&self) -> Rect {
            tag_rect(band_rect(&self.geo, self.track(), self.zone), self.track())
        }

        /// The middle of a key's column, halfway down the lane.
        fn at(&self, key: u8) -> Pos2 {
            let (l, r) = self.geo.span(key);
            pos2((l + r) / 2.0, 26.0)
        }

        /// Runs a frame; the zone the lane asked for is taken as the zone from then on.
        fn frame(&mut self, events: Vec<egui::Event>) {
            let raw = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(1400.0, 300.0))),
                time: Some(self.time),
                events,
                ..Default::default()
            };
            self.time += 0.05;
            let (track, zone) = (self.track(), self.zone);
            let band = band_rect(&self.geo, track, zone);
            let tag = tag_rect(band, track);
            let (mut asked, geo, drag) = (None, &self.geo, &mut self.drag);
            let mut out = self.ctx.run_ui(raw, |ui| {
                asked = lane_input(ui, 0, geo, track, band, tag, zone, drag).1;
            });
            out.textures_delta.clear();
            if let Some(z) = asked {
                self.zone = z;
            }
        }

        fn mouse(&mut self, to: Pos2) {
            self.frame(vec![egui::Event::PointerMoved(to)]);
        }

        fn button(&mut self, at: Pos2, pressed: bool) {
            self.frame(vec![egui::Event::PointerButton {
                pos: at,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: Default::default(),
            }]);
        }

        /// Presses at `from`, drags through `via` to `to`, lets go.
        fn drag(&mut self, from: Pos2, to: Pos2) {
            self.mouse(from);
            self.button(from, true);
            for n in 1..=4 {
                self.mouse(from + (to - from) * (n as f32 / 4.0));
            }
            self.button(to, false);
            self.frame(vec![]);
        }
    }

    #[test]
    fn dragging_across_bare_track_paints_a_zone() {
        let mut b = Bench::new((60, 71));
        b.drag(b.at(40), b.at(52));
        assert_eq!(b.zone, (40, 52));
        assert_eq!(b.drag, None, "the drag ended with the button");
    }

    #[test]
    fn dragging_an_end_resizes_and_the_middle_moves() {
        let mut b = Bench::new((60, 71));
        let right = pos2(b.geo.span(71).1, 26.0);
        b.drag(right, pos2(b.geo.span(76).1 - 1.0, 26.0));
        assert_eq!(b.zone, (60, 76));

        let mut b = Bench::new((60, 71));
        b.drag(b.at(65), b.at(70));
        assert_eq!(b.zone, (65, 76), "moved five keys, same width");
    }

    #[test]
    fn a_click_on_bare_track_is_one_key_and_on_the_band_nothing() {
        let mut b = Bench::new((60, 71));
        b.mouse(b.at(40));
        b.button(b.at(40), true);
        b.button(b.at(40), false);
        assert_eq!(b.zone, (40, 40));

        let mut b = Bench::new((40, 80));
        let mid = b.at(60);
        b.mouse(mid);
        b.button(mid, true);
        b.button(mid, false);
        assert_eq!(b.zone, (40, 80));
    }

    #[test]
    fn the_tag_is_not_dragged() {
        let mut b = Bench::new((40, 80));
        let tag = b.tag().center();
        b.drag(tag, tag + vec2(150.0, 0.0));
        assert_eq!(b.zone, (40, 80));
    }
}
