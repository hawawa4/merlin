//! Little pictures for the browser: one per family (a keyboard for Keys, a trumpet for Brass...)
//! and a few for instruments that look nothing like their family. Drawn with lines, so they
//! take the text colour and stay sharp at any scale.

use egui::{Color32, Painter, Pos2, Rect, Response, Sense, Shape, Stroke, Ui, pos2, vec2};

/// The room an icon takes, in points.
pub const SIZE: f32 = 18.0;

/// Which picture a name gets: an instrument's own if it has one, else its family's.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Glyph {
    Keyboard,
    Pipes,
    Violin,
    Harp,
    Guitar,
    Woodwind,
    Saxophone,
    Trumpet,
    Horn,
    Mic,
    Drum,
    Wave,
    Globe,
    Rack,
    Sparkle,
    Untagged,
    /// A folder, and an audio file (a waveform): for the backing-track library.
    Folder,
    AudioFile,
}

/// The picture for a family, or for one of its instruments when that has its own.
pub fn glyph_for(family: &str, instrument: Option<&str>) -> Glyph {
    match instrument {
        Some("Organ") => return Glyph::Pipes,
        Some("Harp") => return Glyph::Harp,
        Some("Saxophone") => return Glyph::Saxophone,
        Some("French Horn") => return Glyph::Horn,
        Some("Accordion") => return Glyph::Rack,
        _ => {}
    }
    match family {
        "Keys" => Glyph::Keyboard,
        "Strings" => Glyph::Violin,
        "Guitars & Plucked" => Glyph::Guitar,
        "Woodwinds" => Glyph::Woodwind,
        "Brass" => Glyph::Trumpet,
        "Voices" => Glyph::Mic,
        "Percussion" => Glyph::Drum,
        "Synth" => Glyph::Wave,
        "World" => Glyph::Globe,
        "Multi-instrument" => Glyph::Rack,
        "Other" => Glyph::Sparkle,
        _ => Glyph::Untagged,
    }
}

/// Draws the icon in a square of [`SIZE`] and returns its response (hover only).
pub fn show(ui: &mut Ui, glyph: Glyph, colour: Color32) -> Response {
    let (rect, r) = ui.allocate_exact_size(vec2(SIZE, SIZE), Sense::hover());
    if ui.is_rect_visible(rect) {
        paint(ui.painter(), glyph, rect.center(), colour);
    }
    r
}

/// Leaves the room an icon would take, so rows with and without one line up.
pub fn blank(ui: &mut Ui) {
    ui.allocate_exact_size(vec2(SIZE, SIZE), Sense::hover());
}

fn paint(p: &Painter, glyph: Glyph, c: Pos2, colour: Color32) {
    let line = Stroke::new(1.4, colour);
    let thin = Stroke::new(1.0, colour);
    let at = |x: f32, y: f32| c + vec2(x, y);
    let path = |points: &[(f32, f32)], stroke: Stroke| {
        p.add(Shape::line(
            points.iter().map(|&(x, y)| at(x, y)).collect(),
            stroke,
        ));
    };
    match glyph {
        Glyph::Keyboard => {
            let body = Rect::from_center_size(c, vec2(17.0, 11.0));
            p.rect_stroke(body, 1.5, line, egui::StrokeKind::Inside);
            for n in 1..4 {
                let x = body.left() + body.width() * n as f32 / 4.0;
                p.line_segment([pos2(x, body.top() + 5.0), pos2(x, body.bottom())], thin);
            }
            for n in [1, 3] {
                let x = body.left() + body.width() * n as f32 / 4.0;
                p.rect_filled(
                    Rect::from_min_size(pos2(x - 1.5, body.top() + 1.0), vec2(3.0, 5.0)),
                    0.5,
                    colour,
                );
            }
        }
        Glyph::Pipes => {
            for (n, h) in [5.0_f32, 9.0, 12.0, 9.0, 5.0].into_iter().enumerate() {
                let x = -7.0 + n as f32 * 3.5;
                let pipe = Rect::from_min_max(at(x - 1.3, 7.0 - h), at(x + 1.3, 7.0));
                p.rect_stroke(pipe, 0.5, thin, egui::StrokeKind::Inside);
            }
        }
        Glyph::Violin => {
            // A figure-eight body, a neck with its scroll, and the bow across it.
            p.circle_stroke(at(0.0, 3.5), 4.6, line);
            p.circle_stroke(at(0.0, -1.2), 3.2, line);
            path(&[(0.0, -4.4), (0.0, -8.5)], line);
            p.circle_stroke(at(0.0, -8.6), 1.0, thin);
            path(&[(-7.0, 7.5), (7.0, -2.0)], thin);
        }
        Glyph::Harp => {
            path(&[(-5.0, 8.0), (-5.0, -7.0), (5.5, -2.0), (5.5, 8.0)], line);
            for n in 0..4 {
                let x = -2.5 + n as f32 * 2.4;
                path(&[(x, -5.5 + n as f32 * 1.2), (x, 7.5)], thin);
            }
        }
        Glyph::Guitar => {
            p.circle_stroke(at(-2.5, 3.5), 4.7, line);
            p.circle_stroke(at(0.5, 0.2), 3.0, line);
            path(&[(1.8, -1.3), (7.0, -6.5)], line);
            path(&[(6.0, -8.0), (8.5, -5.5)], Stroke::new(2.4, colour));
            p.circle_filled(at(-2.5, 3.5), 1.1, colour);
        }
        Glyph::Woodwind => {
            // A slanted tube with its finger holes and a bell.
            path(&[(-8.0, 7.0), (6.0, -7.0)], Stroke::new(3.2, colour));
            for n in 0..3 {
                let t = 0.25 + n as f32 * 0.22;
                let (x, y) = (-8.0 + 14.0 * t, 7.0 - 14.0 * t);
                p.circle_filled(at(x, y), 1.0, Color32::from_black_alpha(200));
            }
            path(&[(5.0, -4.5), (8.5, -8.0)], Stroke::new(2.0, colour));
        }
        Glyph::Saxophone => {
            path(
                &[
                    (-3.5, -8.0),
                    (-1.0, -6.5),
                    (-1.0, 3.5),
                    (2.0, 7.0),
                    (6.0, 5.5),
                    (6.5, 1.0),
                ],
                Stroke::new(2.4, colour),
            );
            path(&[(4.0, 0.0), (8.5, 0.0)], line);
        }
        Glyph::Trumpet => {
            // Mouthpiece, tubing, three valves and the bell.
            path(&[(-8.5, -1.0), (-6.0, -1.0)], line);
            path(&[(-6.0, -1.0), (2.0, -1.0)], line);
            path(&[(-5.0, 1.5), (2.0, 1.5)], thin);
            path(&[(-6.0, -1.0), (-6.0, 1.5), (-5.0, 1.5)], thin);
            p.add(Shape::convex_polygon(
                vec![at(2.0, -1.0), at(8.5, -5.5), at(8.5, 5.5), at(2.0, 1.5)],
                Color32::TRANSPARENT,
                line,
            ));
            for x in [-3.0_f32, -1.0, 1.0] {
                path(&[(x, -1.0), (x, -4.5)], thin);
                path(&[(x - 0.8, -4.5), (x + 0.8, -4.5)], line);
            }
        }
        Glyph::Horn => {
            p.circle_stroke(at(-1.5, 0.0), 6.5, line);
            p.circle_stroke(at(-1.5, 0.0), 3.2, thin);
            p.add(Shape::convex_polygon(
                vec![at(4.0, -4.5), at(8.5, -8.0), at(8.5, 2.0), at(4.5, 1.0)],
                Color32::TRANSPARENT,
                line,
            ));
        }
        Glyph::Mic => {
            p.rect_stroke(
                Rect::from_center_size(at(0.0, -3.0), vec2(6.0, 9.5)),
                3.0,
                line,
                egui::StrokeKind::Inside,
            );
            path(
                &[
                    (-5.0, -1.0),
                    (-5.0, 2.0),
                    (-2.5, 4.5),
                    (2.5, 4.5),
                    (5.0, 2.0),
                    (5.0, -1.0),
                ],
                thin,
            );
            path(&[(0.0, 4.5), (0.0, 8.0)], line);
            path(&[(-3.0, 8.0), (3.0, 8.0)], line);
        }
        Glyph::Drum => {
            p.add(Shape::ellipse_stroke(at(0.0, -2.5), vec2(7.0, 2.6), line));
            path(&[(-7.0, -2.5), (-7.0, 5.0)], line);
            path(&[(7.0, -2.5), (7.0, 5.0)], line);
            path(&[(-7.0, 5.0), (-4.0, 7.2), (4.0, 7.2), (7.0, 5.0)], line);
            path(&[(-3.5, 0.0), (-1.5, 6.5)], thin);
            path(&[(3.5, 0.0), (1.5, 6.5)], thin);
            path(&[(-2.0, -9.0), (3.0, -4.0)], thin);
            path(&[(2.0, -9.0), (-3.0, -4.0)], thin);
        }
        Glyph::Wave => {
            path(
                &[
                    (-8.5, 4.5),
                    (-3.0, -5.0),
                    (-3.0, 4.5),
                    (2.5, -5.0),
                    (2.5, 4.5),
                    (8.0, -3.0),
                ],
                line,
            );
        }
        Glyph::Globe => {
            p.circle_stroke(c, 7.5, line);
            let meridian: Vec<Pos2> = (0..=24)
                .map(|n| {
                    let a = n as f32 / 24.0 * std::f32::consts::TAU;
                    c + vec2(a.cos() * 3.4, a.sin() * 7.5)
                })
                .collect();
            p.add(Shape::line(meridian, thin));
            p.line_segment([at(-7.5, 0.0), at(7.5, 0.0)], thin);
            for y in [-3.8_f32, 3.8] {
                let half = (7.5_f32 * 7.5 - y * y).sqrt();
                p.line_segment([at(-half, y), at(half, y)], thin);
            }
        }
        Glyph::Rack => {
            for y in [-6.5_f32, -1.5, 3.5] {
                let unit = Rect::from_min_size(at(-8.0, y), vec2(16.0, 4.0));
                p.rect_stroke(unit, 1.0, thin, egui::StrokeKind::Inside);
                p.circle_filled(at(-5.5, y + 2.0), 0.9, colour);
                p.line_segment([at(-2.0, y + 2.0), at(5.5, y + 2.0)], thin);
            }
        }
        Glyph::Sparkle => {
            p.add(Shape::convex_polygon(
                vec![
                    at(0.0, -8.5),
                    at(2.0, -2.0),
                    at(8.5, 0.0),
                    at(2.0, 2.0),
                    at(0.0, 8.5),
                    at(-2.0, 2.0),
                    at(-8.5, 0.0),
                    at(-2.0, -2.0),
                ],
                Color32::TRANSPARENT,
                line,
            ));
        }
        Glyph::Folder => {
            // The body with the tab at the top left.
            path(
                &[
                    (-8.0, -6.0),
                    (-3.0, -6.0),
                    (-1.0, -3.5),
                    (8.0, -3.5),
                    (8.0, 7.0),
                    (-8.0, 7.0),
                ],
                line,
            );
            p.line_segment([at(-8.0, -6.0), at(-8.0, 7.0)], line);
        }
        Glyph::AudioFile => {
            for (n, h) in [3.0_f32, 7.0, 11.0, 6.0, 9.0, 4.0].into_iter().enumerate() {
                let x = -7.0 + n as f32 * 2.8;
                p.line_segment([at(x, -h / 2.0), at(x, h / 2.0)], Stroke::new(1.6, colour));
            }
        }
        Glyph::Untagged => {
            p.circle_stroke(c, 7.0, thin);
            p.text(
                c,
                egui::Align2::CENTER_CENTER,
                "?",
                egui::FontId::proportional(11.0),
                colour,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_family_has_a_picture_and_instruments_may_have_their_own() {
        for (family, instruments) in crate::tags::TAXONOMY {
            assert_ne!(glyph_for(family, None), Glyph::Untagged, "{family}");
            for i in *instruments {
                // Never panics, and an instrument without a picture of its own uses its family's.
                let _ = glyph_for(family, Some(i));
            }
        }
        assert_eq!(glyph_for("Brass", Some("Trumpet")), Glyph::Trumpet);
        assert_eq!(glyph_for("Keys", Some("Organ")), Glyph::Pipes);
    }
}
