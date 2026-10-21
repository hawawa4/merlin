//! The keyboard map: zones coloured by slot, live note dots, clickable keys.

use std::sync::atomic::Ordering;

use egui::{Color32, CornerRadius, Pos2, Rect, Sense, Stroke, StrokeKind, Ui, vec2};

use crate::audio::{SLOTS, Shared};

pub const COLORS: [Color32; SLOTS] = [
    Color32::from_rgb(0xe0, 0x6c, 0x5a),
    Color32::from_rgb(0x5a, 0xa9, 0xe0),
    Color32::from_rgb(0x6c, 0xc9, 0x6a),
    Color32::from_rgb(0xe0, 0xb4, 0x4a),
    Color32::from_rgb(0xb0, 0x7a, 0xe0),
    Color32::from_rgb(0x4a, 0xd0, 0xc4),
    Color32::from_rgb(0xe0, 0x7a, 0xb8),
    Color32::from_rgb(0xa0, 0xa8, 0x60),
];

const FIRST: u8 = 36; // C2
const LAST: u8 = 96; // C7

/// What the map needs to know about a slot.
#[derive(Clone, Copy)]
pub struct ZoneView {
    pub loaded: bool,
    pub active: bool,
    pub lo: u8,
    pub hi: u8,
}

fn is_black(key: u8) -> bool {
    matches!(key % 12, 1 | 3 | 6 | 8 | 10)
}

/// Draws the map; returns the key under a pressed pointer, if any.
pub fn show(ui: &mut Ui, zones: &[ZoneView; SLOTS], shared: &Shared) -> Option<u8> {
    let width = ui.available_width();
    let whites: Vec<u8> = (FIRST..=LAST).filter(|k| !is_black(*k)).collect();
    let key_w = width / whites.len() as f32;
    let key_h = 64.0;
    let strip_h = 4.0;
    let (resp, painter) = ui.allocate_painter(
        vec2(width, key_h + strip_h * SLOTS as f32 + 4.0),
        Sense::click_and_drag(),
    );
    let origin = resp.rect.min;

    let mut rects: Vec<(u8, Rect)> = Vec::new();
    for (i, &k) in whites.iter().enumerate() {
        let x = origin.x + i as f32 * key_w;
        rects.push((
            k,
            Rect::from_min_size(Pos2::new(x, origin.y), vec2(key_w, key_h)),
        ));
    }
    let black_w = key_w * 0.62;
    for k in FIRST..=LAST {
        if is_black(k) {
            let left = whites.iter().position(|&w| w == k - 1).unwrap();
            let x = origin.x + (left as f32 + 1.0) * key_w - black_w / 2.0;
            rects.push((
                k,
                Rect::from_min_size(Pos2::new(x, origin.y), vec2(black_w, key_h * 0.6)),
            ));
        }
    }

    let tint = |key: u8, base: Color32| {
        let mut c = base;
        let mut n = 0;
        let (mut r, mut g, mut b) = (0u32, 0u32, 0u32);
        for (i, z) in zones.iter().enumerate() {
            if z.loaded && z.active && (z.lo..=z.hi).contains(&key) {
                r += COLORS[i].r() as u32;
                g += COLORS[i].g() as u32;
                b += COLORS[i].b() as u32;
                n += 1;
            }
        }
        if n > 0 {
            let mix = |base: u8, sum: u32| ((base as u32 * 45 + sum / n * 55) / 100) as u8;
            c = Color32::from_rgb(mix(base.r(), r), mix(base.g(), g), mix(base.b(), b));
        }
        c
    };
    for black in [false, true] {
        for (k, rect) in rects.iter().filter(|(k, _)| is_black(*k) == black) {
            let base = if black {
                Color32::from_gray(30)
            } else {
                Color32::from_gray(235)
            };
            painter.rect_filled(*rect, CornerRadius::same(2), tint(*k, base));
            painter.rect_stroke(
                *rect,
                CornerRadius::same(2),
                Stroke::new(1.0, Color32::from_gray(70)),
                StrokeKind::Inside,
            );
            let owners = shared.owners[*k as usize].load(Ordering::Relaxed);
            if owners != 0 {
                let slot = owners.trailing_zeros() as usize;
                painter.circle_filled(
                    Pos2::new(rect.center().x, rect.max.y - 9.0),
                    4.5,
                    COLORS[slot],
                );
                painter.circle_stroke(
                    Pos2::new(rect.center().x, rect.max.y - 9.0),
                    4.5,
                    Stroke::new(1.5, Color32::WHITE),
                );
            }
        }
    }

    // Zone bars: one thin strip per loaded slot, spanning its range.
    let x_of = |key: u8| {
        let below = whites.iter().filter(|&&w| w < key).count() as f32;
        origin.x + below * key_w
    };
    for (i, z) in zones.iter().enumerate().filter(|(_, z)| z.loaded) {
        let (lo, hi) = (z.lo.max(FIRST), z.hi.min(LAST));
        if lo > hi {
            continue;
        }
        let y = origin.y + key_h + 3.0 + i as f32 * strip_h;
        let rect = Rect::from_min_max(
            Pos2::new(x_of(lo), y),
            Pos2::new(x_of(hi) + key_w, y + strip_h - 1.0),
        );
        let c = if z.active {
            COLORS[i]
        } else {
            COLORS[i].gamma_multiply(0.3)
        };
        painter.rect_filled(rect, CornerRadius::ZERO, c);
    }

    if resp.is_pointer_button_down_on()
        && let Some(p) = resp.interact_pointer_pos()
    {
        // Black keys sit on top, so test them first.
        let hit = rects
            .iter()
            .filter(|(k, _)| is_black(*k))
            .chain(rects.iter().filter(|(k, _)| !is_black(*k)))
            .find(|(_, r)| r.contains(p));
        return hit.map(|(k, _)| *k);
    }
    None
}
