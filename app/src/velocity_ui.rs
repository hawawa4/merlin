//! The velocity curve of a MIDI device: a screen with the curve on it that the user can draw on,
//! and the presets. Pure drawing: it reads the device's row and returns `Action`s.

use std::time::Duration;

use egui::{Align2, Color32, FontId, Mesh, Pos2, Sense, Shape, Stroke, Ui, pos2, vec2};
use merlin_audio_io::LastVelocity;
use merlin_control::velocity::{STEPS, draw_line};

use crate::hw;
use crate::session::MidiDeviceRow;
use crate::settings::VelocityCurve;
use crate::ui::Action;

/// Side of the curve's screen, in points.
const SCREEN: f32 = 240.0;
/// How long the dot of the latest note stays bright.
const FRESH: Duration = Duration::from_secs(2);

/// A stroke being drawn on a device's curve: the curve so far and where the pointer last was
/// (played, sent), so the next frame draws a line from there. Saved when the button comes up.
pub struct CurveStroke {
    device: String,
    points: Vec<u8>,
    last: Option<(u8, u8)>,
}

/// The editor of one device's curve. `last` is the latest note played on it.
pub fn editor(
    ui: &mut Ui,
    row: &MidiDeviceRow,
    last: Option<(LastVelocity, Duration)>,
    stroke: &mut Option<CurveStroke>,
    actions: &mut Vec<Action>,
) {
    hw::plate().show(ui, |ui| {
        ui.horizontal_top(|ui| {
            let shown = screen(ui, row, last, stroke, actions);
            ui.add_space(10.0);
            ui.vertical(|ui| {
                hw::engraved(ui, "Velocity curve");
                ui.label(egui::RichText::new(&row.name).strong());
                ui.add_space(6.0);
                let names: Vec<&str> = VelocityCurve::PRESETS.iter().map(|(_, n)| *n).collect();
                let lit = VelocityCurve::PRESETS
                    .iter()
                    .position(|(c, _)| *c == row.velocity)
                    .unwrap_or(names.len());
                if let Some(n) = hw::selector(ui, &names, lit, hw::AMBER) {
                    *stroke = None;
                    actions.push(Action::SetVelocityCurve(
                        row.name.clone(),
                        VelocityCurve::PRESETS[n].0.clone(),
                    ));
                }
                ui.add_space(6.0);
                ui.label(
                    egui::RichText::new(
                        "Soft: light playing already sounds loud. Hard: it takes a firm touch. \
                         Constant: every note at 64. Or draw your own on the screen. Left to \
                         right is how hard you play, bottom to top what the instruments get.\n\
                         Kept for this device in every rig.",
                    )
                    .small()
                    .weak(),
                );
                ui.add_space(6.0);
                let text = match shown.readout {
                    Some((played, sent)) => format!("PLAYED {played:>3}  SENT {sent:>3}"),
                    None if row.input.is_none() => "NOT CONNECTED".to_owned(),
                    None => "PLAY A NOTE".to_owned(),
                };
                hw::display(ui, &text, 190.0, hw::GREEN);
            });
        });
    });
}

/// What the screen showed: the readout for the display beside it.
struct Shown {
    readout: Option<(u8, u8)>,
}

/// The curve's screen, which takes strokes.
fn screen(
    ui: &mut Ui,
    row: &MidiDeviceRow,
    last: Option<(LastVelocity, Duration)>,
    stroke: &mut Option<CurveStroke>,
    actions: &mut Vec<Action>,
) -> Shown {
    let (rect, r) = ui.allocate_exact_size(vec2(SCREEN, SCREEN), Sense::click_and_drag());
    let plot = rect.shrink(10.0);
    let at = |played: f32, sent: f32| {
        pos2(
            plot.left() + played / 127.0 * plot.width(),
            plot.bottom() - sent / 127.0 * plot.height(),
        )
    };
    let under = |p: Pos2| {
        let played = ((p.x - plot.left()) / plot.width() * 127.0).round();
        let sent = ((plot.bottom() - p.y) / plot.height() * 127.0).round();
        (played.clamp(1.0, 127.0) as u8, sent.clamp(1.0, 127.0) as u8)
    };

    // A stroke for another device (its editor was swapped mid-stroke) is dropped.
    if stroke.as_ref().is_some_and(|s| s.device != row.name) {
        *stroke = None;
    }
    if r.is_pointer_button_down_on()
        && let Some(p) = r.interact_pointer_pos()
    {
        let now = under(p);
        let s = stroke.get_or_insert_with(|| CurveStroke {
            device: row.name.clone(),
            points: row.velocity.to_drawn(),
            last: None,
        });
        draw_line(&mut s.points, s.last.unwrap_or(now), now);
        s.last = Some(now);
    }
    // The button came up: the stroke is the device's curve now. It is still drawn this frame,
    // until the session has it.
    let finished = if r.is_pointer_button_down_on() {
        None
    } else {
        stroke.take()
    };
    let table: Vec<u8> = match (stroke.as_ref(), finished.as_ref()) {
        (Some(s), _) | (None, Some(s)) => s.points.clone(),
        (None, None) => row.velocity.table().to_vec(),
    };
    if let Some(s) = finished {
        actions.push(Action::SetVelocityCurve(
            row.name.clone(),
            VelocityCurve::Drawn(s.points),
        ));
    }
    let sent_at = |played: u8| table.get(played as usize).copied().unwrap_or(played);

    if !ui.is_rect_visible(rect) {
        return Shown { readout: None };
    }
    let p = ui.painter();
    p.rect_filled(rect, 6.0, hw::WELL);
    p.rect_stroke(
        rect,
        6.0,
        Stroke::new(1.0, Color32::from_rgb(0x0b, 0x0b, 0x0d)),
        egui::StrokeKind::Inside,
    );
    // The screen's grid, and as-played as a dashed diagonal.
    let grid = Stroke::new(1.0, Color32::from_gray(38));
    for q in 1..4 {
        let v = q as f32 * 127.0 / 4.0;
        p.line_segment([at(v, 0.0), at(v, 127.0)], grid);
        p.line_segment([at(0.0, v), at(127.0, v)], grid);
    }
    p.extend(Shape::dashed_line(
        &[at(0.0, 0.0), at(127.0, 127.0)],
        Stroke::new(1.0, Color32::from_gray(70)),
        4.0,
        4.0,
    ));
    let small = FontId::proportional(11.0);
    let label = Color32::from_gray(110);
    p.text(
        plot.left_top() + vec2(2.0, 0.0),
        Align2::LEFT_TOP,
        "SENT",
        small.clone(),
        label,
    );
    p.text(
        plot.right_bottom() - vec2(2.0, 2.0),
        Align2::RIGHT_BOTTOM,
        "PLAYED",
        small,
        label,
    );

    // The curve, with a glow under it, from velocity 1 (0 is the note-off).
    let line: Vec<Pos2> = (1..STEPS)
        .map(|v| at(v as f32, sent_at(v as u8) as f32))
        .collect();
    let mut fill = Mesh::default();
    let glow = hw::AMBER.gamma_multiply(0.10);
    for w in line.windows(2) {
        let i = fill.vertices.len() as u32;
        fill.colored_vertex(w[0], glow);
        fill.colored_vertex(w[1], glow);
        fill.colored_vertex(pos2(w[1].x, plot.bottom()), glow);
        fill.colored_vertex(pos2(w[0].x, plot.bottom()), glow);
        fill.add_triangle(i, i + 1, i + 2);
        fill.add_triangle(i, i + 2, i + 3);
    }
    p.add(fill);
    p.add(Shape::line(
        line.clone(),
        Stroke::new(5.0, hw::AMBER.gamma_multiply(0.18)),
    ));
    p.add(Shape::line(line, Stroke::new(2.0, hw::AMBER)));

    // Where the pointer is, or else the latest note played, on the curve.
    let hover = r
        .hover_pos()
        .filter(|_| stroke.is_none())
        .map(|p| under(p).0);
    let note = last.map(|(l, age)| (l.played.max(1), age < FRESH));
    let readout = match (hover, note) {
        (Some(played), _) => {
            let dot = at(played as f32, sent_at(played) as f32);
            p.line_segment(
                [pos2(dot.x, plot.bottom()), dot],
                Stroke::new(1.0, Color32::from_gray(90)),
            );
            p.circle_filled(dot, 3.5, Color32::from_gray(200));
            Some((played, sent_at(played)))
        }
        (None, Some((played, fresh))) => {
            let dot = at(played as f32, sent_at(played) as f32);
            let colour = if fresh {
                hw::GREEN
            } else {
                hw::GREEN.gamma_multiply(0.45)
            };
            p.line_segment(
                [pos2(dot.x, plot.bottom()), dot],
                Stroke::new(1.0, colour.gamma_multiply(0.5)),
            );
            if fresh {
                p.circle_filled(dot, 8.0, colour.gamma_multiply(0.2));
            }
            p.circle_filled(dot, 4.0, colour);
            Some((played, sent_at(played)))
        }
        (None, None) => None,
    };
    if r.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
    }
    Shown { readout }
}
