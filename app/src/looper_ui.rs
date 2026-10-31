//! The Looper view: a loop station for the rack. Every instrument gets a lane with one REC
//! button, two parts (A and B) and a row of blocks, one per measure, lit where the part holds
//! MIDI. One playhead runs across all of them. The loops themselves are on the audio thread
//! (see `merlin_engine::looper`); this only draws them and sends the buttons' presses.

use std::time::Duration;

use egui::{Align2, Color32, FontId, Rect, Sense, Stroke, StrokeKind, Ui, pos2, vec2};
use merlin_engine::looper::{LoopCommand, LoopMode};

use crate::hw::{self, AMBER, BLUE, Cap, GREEN, Icon, RED, TEXT, TEXT_DIM, WELL};
use crate::keyboard::COLORS;
use crate::session::{LoopLane, LoopView, Session, UI_SLOTS};
use crate::ui::Action;

/// What a press in this view asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoopAct {
    Rec(usize),
    Part(usize, usize),
    Mute(usize, bool),
    Undo(usize),
    Clear(usize),
    AllParts(usize),
    ClearAll,
    Bars(u8),
}

pub fn run(s: &mut Session, act: LoopAct) {
    match act {
        LoopAct::Rec(i) => s.loop_rec(i),
        LoopAct::Part(i, p) => s.loop_cmd(LoopCommand::Part(i, p)),
        LoopAct::Mute(i, m) => s.loop_cmd(LoopCommand::Mute(i, m)),
        LoopAct::Undo(i) => s.loop_cmd(LoopCommand::Undo(i)),
        LoopAct::Clear(i) => s.loop_cmd(LoopCommand::Clear(i)),
        LoopAct::AllParts(p) => s.loop_all(p),
        LoopAct::ClearAll => s.loop_clear_all(),
        LoopAct::Bars(n) => s.loop_set_bars(n),
    }
}

/// Loop lengths on offer, in measures.
const BARS: [u8; 4] = [1, 2, 4, 8];
/// The instrument's name plate, and the gap before the blocks.
const LABEL_W: f32 = 318.0;
const GAP: f32 = 12.0;
const LANE_H: f32 = 66.0;
const RULER_H: f32 = 34.0;

pub fn show(ui: &mut Ui, s: &Session, actions: &mut Vec<Action>) {
    let view = s.loop_view();
    // The playhead moves smoothly while the loop runs; the blinking lamps need a slower beat.
    if view.running {
        ui.ctx().request_repaint_after(Duration::from_millis(16));
    } else if view
        .lanes
        .iter()
        .any(|l| l.mode == LoopMode::Armed || l.pending.is_some())
    {
        ui.ctx().request_repaint_after(Duration::from_millis(100));
    }
    egui::ScrollArea::vertical().show(ui, |ui| {
        ui.add_space(6.0);
        controls(ui, s, &view, actions);
        ui.add_space(10.0);
        stage(ui, s, &view, actions);
    });
}

fn blink(ui: &Ui) -> bool {
    ((ui.input(|i| i.time) * 2.5) as u64).is_multiple_of(2)
}

fn controls(ui: &mut Ui, s: &Session, view: &LoopView, actions: &mut Vec<Action>) {
    ui.horizontal_top(|ui| {
        hw::plate().show(ui, |ui| {
            hw::engraved(ui, "Transport");
            ui.horizontal(|ui| {
                let counting = s.counting_in();
                let running = view.running || counting;
                let cap = Cap::new(if running { "STOP" } else { "PLAY" })
                    .lit(running.then_some(GREEN))
                    .min_width(84.0);
                if cap.show(ui).clicked() {
                    actions.push(Action::TransportToggle);
                }
                let text = if counting {
                    let (_, beat) = s.engine_position();
                    let bpb = u32::from(view.beats_per_bar.max(1));
                    // The click's beat counter runs one ahead during the count-in.
                    format!("COUNT {}", if beat <= 1 { bpb } else { beat - 1 })
                } else if view.running {
                    let (bar, beat) = view.position();
                    format!("{bar} . {beat}")
                } else {
                    "1 . 1".to_owned()
                };
                hw::display(ui, &text, 92.0, AMBER);
            });
        });
        hw::plate().show(ui, |ui| {
            // The tempo and the time signature are the header's; the loop's length is here.
            hw::engraved(ui, "Loop length");
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("MEASURES").size(11.0).color(TEXT_DIM));
                let now = BARS.iter().position(|b| *b == view.bars).unwrap_or(2);
                let names = BARS.map(|b| b.to_string());
                let names: Vec<&str> = names.iter().map(String::as_str).collect();
                if let Some(n) = hw::selector(ui, &names, now, BLUE)
                    && BARS[n] != view.bars
                {
                    actions.push(Action::Loop(LoopAct::Bars(BARS[n])));
                }
            });
        });
        hw::plate().show(ui, |ui| {
            hw::engraved(ui, "Helpers");
            ui.horizontal(|ui| {
                let count_in = s.rig.transport.count_in_bars > 0;
                if Cap::new("")
                    .icon(Icon::CountIn)
                    .lit(count_in.then_some(GREEN))
                    .show(ui)
                    .on_hover_text(if count_in {
                        "Count-in on: a bar of clicks before the loop begins. Click to turn it off"
                    } else {
                        "Count-in off. Click to start with a bar of clicks before the loop begins"
                    })
                    .clicked()
                {
                    actions.push(Action::SetCountIn(u8::from(!count_in)));
                }
                ui.add_space(6.0);
                // Lit while every instrument is on that part.
                let on_rack = || {
                    (0..UI_SLOTS)
                        .filter(|&i| s.plugins[i].is_some())
                        .map(|i| view.lanes[i])
                };
                for part in 0..2 {
                    let all = on_rack().count() > 0 && on_rack().all(|l| l.part == part);
                    let (icon, name) = if part == 0 {
                        (Icon::AllA, "A")
                    } else {
                        (Icon::AllB, "B")
                    };
                    if Cap::new("")
                        .icon(icon)
                        .lit(all.then_some(BLUE))
                        .flash(BLUE)
                        .show(ui)
                        .on_hover_text(format!(
                            "Every instrument to part {name}, when its loop comes round"
                        ))
                        .clicked()
                    {
                        actions.push(Action::Loop(LoopAct::AllParts(part)));
                    }
                }
                ui.add_space(6.0);
                if Cap::new("")
                    .icon(Icon::TrashAll)
                    .min_width(48.0)
                    .flash(RED)
                    .show(ui)
                    .on_hover_text("Empty every loop, both parts")
                    .clicked()
                {
                    actions.push(Action::Loop(LoopAct::ClearAll));
                }
            });
        });
    });
}

fn stage(ui: &mut Ui, s: &Session, view: &LoopView, actions: &mut Vec<Action>) {
    let slots: Vec<usize> = (0..UI_SLOTS).filter(|&i| s.plugins[i].is_some()).collect();
    if slots.is_empty() {
        ui.add_space(30.0);
        ui.vertical_centered(|ui| {
            ui.label(
                egui::RichText::new("No instruments in the rack yet. Load one in the Rack view, then come back to loop it.")
                    .size(15.0)
                    .color(TEXT_DIM),
            );
        });
        return;
    }
    let width = ui.available_width();
    let left = ui.cursor().left();
    let (x0, x1) = (left + LABEL_W + GAP, left + width - 4.0);
    let top = ui.cursor().top();
    ruler(ui, view, x0, x1);
    for &i in &slots {
        lane(ui, s, view, i, x0, x1, actions);
        ui.add_space(8.0);
    }
    // One playhead across the ruler and every lane.
    if view.running {
        let bottom = ui.cursor().top() - 8.0;
        let total = f32::from(view.bars) * f32::from(view.beats_per_bar);
        let x = x0 + (view.beat / total.max(1.0)).clamp(0.0, 1.0) * (x1 - x0);
        let p = ui.painter();
        let (a, b) = (pos2(x, top + RULER_H * 0.7), pos2(x, bottom));
        p.line_segment([a, b], Stroke::new(6.0, Color32::from_white_alpha(22)));
        p.line_segment([a, b], Stroke::new(2.0, Color32::WHITE));
    }
    ui.add_space(8.0);
    ui.label(
        egui::RichText::new(
            "REC records one pass, tap again to overdub  \u{b7}  A / B swap the part when the loop comes round  \u{b7}  the curved arrow takes off the last pass  \u{b7}  the bin empties the part",
        )
        .size(12.0)
        .color(TEXT_DIM),
    );
}

/// The beats along the top, the current one lit; bar numbers on their first beats.
fn ruler(ui: &mut Ui, view: &LoopView, x0: f32, x1: f32) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), RULER_H), Sense::hover());
    let bar = Rect::from_min_max(pos2(x0, rect.top()), pos2(x1, rect.bottom()));
    let p = ui.painter();
    p.rect_filled(bar, 5.0, WELL);
    p.rect_stroke(
        bar,
        5.0,
        Stroke::new(1.0, Color32::BLACK),
        StrokeKind::Inside,
    );
    let bpb = usize::from(view.beats_per_bar.max(1));
    let total = usize::from(view.bars) * bpb;
    let w = bar.width() / total as f32;
    for b in 0..total {
        let cell = Rect::from_min_size(
            pos2(bar.left() + b as f32 * w, bar.top()),
            vec2(w, bar.height()),
        );
        let first = b % bpb == 0;
        let lit = view.running && b == view.beat_index();
        if lit {
            p.rect_filled(cell.shrink(1.0), 2.0, AMBER);
        }
        if b > 0 {
            let c = if first {
                Color32::from_gray(0x5b)
            } else {
                Color32::from_gray(0x2a)
            };
            // Narrow cells (12/8) leave the bar's label clear of the beat lines.
            let top = if first || w >= 30.0 { 0.0 } else { 14.0 };
            p.line_segment(
                [cell.left_top() + vec2(0.0, top), cell.left_bottom()],
                Stroke::new(if first { 2.0 } else { 1.0 }, c),
            );
        }
        let ink = if lit {
            Color32::from_rgb(0x1a, 0x12, 0x00)
        } else {
            TEXT_DIM
        };
        if first {
            p.text(
                cell.left_top() + vec2(5.0, 3.0),
                Align2::LEFT_TOP,
                format!("BAR {}", b / bpb + 1),
                FontId::proportional(10.0),
                ink,
            );
        }
        // 12/8 over eight bars is 96 narrow cells: the numbers shrink to fit, then give way.
        if w >= 11.0 {
            p.text(
                pos2(cell.center().x, cell.bottom() - 3.0),
                Align2::CENTER_BOTTOM,
                (b % bpb + 1).to_string(),
                FontId::monospace((w * 0.62).clamp(8.0, 12.5)),
                ink,
            );
        }
    }
}

fn lane(
    ui: &mut Ui,
    s: &Session,
    view: &LoopView,
    slot: usize,
    x0: f32,
    x1: f32,
    actions: &mut Vec<Action>,
) {
    let l = view.lanes[slot];
    let colour = COLORS[slot];
    let name = s.rig.slots[slot].title();
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), LANE_H), Sense::hover());
    // The name plate, with the lane's colour down its edge.
    let plate = Rect::from_min_size(rect.min, vec2(LABEL_W, LANE_H));
    ui.painter().rect_filled(plate, 7.0, hw::PLATE);
    ui.painter().rect_stroke(
        plate,
        7.0,
        Stroke::new(1.0, Color32::BLACK),
        StrokeKind::Inside,
    );
    ui.painter().rect_filled(
        Rect::from_min_size(plate.min + vec2(5.0, 6.0), vec2(5.0, LANE_H - 12.0)),
        2.0,
        colour,
    );
    ui.painter().text(
        plate.min + vec2(20.0, 7.0),
        Align2::LEFT_TOP,
        format!("{}  {name}", slot + 1),
        FontId::proportional(14.5),
        TEXT,
    );
    let mut buttons = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(Rect::from_min_size(
                plate.min + vec2(18.0, 31.0),
                vec2(LABEL_W - 24.0, 28.0),
            ))
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
    );
    buttons_row(&mut buttons, slot, &l, colour, actions);
    blocks(
        ui,
        view,
        &l,
        colour,
        Rect::from_min_max(pos2(x0, rect.top()), pos2(x1, rect.bottom())),
    );
}

fn buttons_row(ui: &mut Ui, slot: usize, l: &LoopLane, colour: Color32, actions: &mut Vec<Action>) {
    ui.spacing_mut().item_spacing = vec2(4.0, 0.0);
    let on = blink(ui);
    let (label, lit, base) = match l.mode {
        LoopMode::Idle => ("REC", None, None),
        LoopMode::Armed => ("ARMED", on.then_some(RED), None),
        LoopMode::Rec => (
            "RECORDING",
            Some(RED),
            Some(Color32::from_rgb(0x9a, 0x2a, 0x2a)),
        ),
        LoopMode::Play => ("PLAYING", Some(GREEN), None),
        LoopMode::Over => ("OVERDUB", Some(AMBER), None),
    };
    let mut rec = Cap::new(label).min_width(92.0);
    if let Some(b) = base {
        rec = rec.base(b);
    }
    if rec
        .lit(lit)
        .show(ui)
        .on_hover_text(match l.mode {
            LoopMode::Idle => {
                "Record: starts at the next beat 1, plays the keyboard into this instrument"
            }
            LoopMode::Armed => "Waiting for beat 1. Click to cancel",
            LoopMode::Rec => "Recording one pass. Click to stop now",
            LoopMode::Play => "Click to record more on top (overdub)",
            LoopMode::Over => "Click to stop overdubbing",
        })
        .clicked()
    {
        actions.push(Action::Loop(LoopAct::Rec(slot)));
    }
    hw::inset().show(ui, |ui| {
        ui.spacing_mut().item_spacing = vec2(2.0, 0.0);
        for p in 0..2 {
            let waiting = l.pending == Some(p);
            let lit = if waiting {
                blink(ui).then_some(AMBER)
            } else {
                (l.part == p).then_some(BLUE)
            };
            let r = Cap::new(if p == 0 { "A" } else { "B" })
                .lit(lit)
                .min_width(30.0)
                .show(ui)
                .on_hover_text("Switch to this part when the loop comes round");
            if l.filled[p] != 0 {
                // A dot: this part has something in it.
                ui.painter()
                    .circle_filled(r.rect.right_top() + vec2(-5.0, 5.0), 2.5, colour);
            }
            if r.clicked() {
                actions.push(Action::Loop(LoopAct::Part(slot, p)));
            }
        }
    });
    if Cap::new("M")
        .lit(l.muted.then_some(AMBER))
        .min_width(26.0)
        .show(ui)
        .on_hover_text("Mute this loop")
        .clicked()
    {
        actions.push(Action::Loop(LoopAct::Mute(slot, !l.muted)));
    }
    if Cap::new("")
        .icon(Icon::Undo)
        .min_width(30.0)
        .flash(AMBER)
        .show(ui)
        .on_hover_text("Take off the last pass")
        .clicked()
    {
        actions.push(Action::Loop(LoopAct::Undo(slot)));
    }
    if Cap::new("")
        .icon(Icon::Trash)
        .min_width(30.0)
        .flash(RED)
        .show(ui)
        .on_hover_text("Empty this part")
        .clicked()
    {
        actions.push(Action::Loop(LoopAct::Clear(slot)));
    }
}

/// One block per measure: lit where the part holds MIDI, red while it is being recorded.
fn blocks(ui: &mut Ui, view: &LoopView, l: &LoopLane, colour: Color32, area: Rect) {
    let p = ui.painter();
    p.rect_filled(area, 6.0, WELL);
    p.rect_stroke(
        area,
        6.0,
        Stroke::new(1.0, Color32::BLACK),
        StrokeKind::Inside,
    );
    let bars = usize::from(view.bars);
    let w = area.width() / bars as f32;
    let now = (view.running).then(|| view.beat_index() / usize::from(view.beats_per_bar.max(1)));
    let on = blink(ui);
    let p = ui.painter();
    for b in 0..bars {
        let cell = Rect::from_min_size(
            pos2(area.left() + b as f32 * w, area.top()),
            vec2(w, area.height()),
        )
        .shrink2(vec2(3.0, 7.0));
        let full = l.filled[l.part] & (1 << b) != 0;
        let here = now == Some(b);
        let recording = here && l.mode == LoopMode::Rec;
        let over = here && l.mode == LoopMode::Over;
        let mut fill = if recording {
            Color32::from_rgb(0xd8, 0x30, 0x24)
        } else if full {
            hw::mix(colour, Color32::BLACK, 0.18)
        } else {
            Color32::from_rgb(0x17, 0x18, 0x1b)
        };
        if full || recording {
            if here {
                fill = hw::mix(fill, Color32::WHITE, 0.22);
            }
            if l.muted {
                fill = hw::mix(fill, Color32::from_rgb(0x17, 0x18, 0x1b), 0.6);
            }
            p.rect_filled(cell.expand(3.0), 8.0, fill.gamma_multiply(0.25));
        }
        p.rect_filled(cell, 6.0, fill);
        if full || recording {
            // The sheen of a lit cap.
            let sheen = Rect::from_min_size(cell.min, vec2(cell.width(), cell.height() * 0.45));
            p.rect_filled(
                sheen.shrink2(vec2(1.0, 1.0)),
                egui::CornerRadius {
                    nw: 5,
                    ne: 5,
                    sw: 0,
                    se: 0,
                },
                Color32::from_white_alpha(28),
            );
        }
        let ring = if over {
            Some(AMBER)
        } else if l.mode == LoopMode::Armed && b == 0 && on {
            Some(RED)
        } else {
            None
        };
        p.rect_stroke(
            cell,
            6.0,
            match ring {
                Some(c) => Stroke::new(2.0, c),
                None => Stroke::new(1.0, Color32::from_gray(if full { 0x10 } else { 0x2c })),
            },
            StrokeKind::Inside,
        );
    }
}
