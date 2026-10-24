//! MIDI map mode (the way Ableton does it): switch it on, click any control on screen, then move
//! or press the control on your device. Controls that can be mapped are tinted; mapped ones show
//! their source, and a right-click takes the mapping away.

use egui::{Color32, Response, Sense, Ui};
use merlin_model::ControlAction;

use crate::session::Session;
use crate::ui::Action;

const FREE: Color32 = Color32::from_rgb(0x4a, 0x7a, 0xd0);
const MAPPED: Color32 = Color32::from_rgb(0x3f, 0xb0, 0x6a);
const LEARNING: Color32 = Color32::from_rgb(0xe0, 0xb4, 0x4a);

/// Marks the control `r` as something `action` can be mapped to, while map mode is on. Call it
/// right after adding the widget; a click on the tint teaches the control instead of using it.
pub fn target(
    ui: &mut Ui,
    s: &Session,
    r: &Response,
    action: ControlAction,
    label: Option<&str>,
    actions: &mut Vec<Action>,
) {
    if !s.map_mode {
        return;
    }
    let mapped = s.mapping_for(action);
    let learning = s.learning() == Some(action);
    let colour = if learning {
        LEARNING
    } else if mapped.is_some() {
        MAPPED
    } else {
        FREE
    };
    let rect = r.rect.expand(1.0);
    let hit = ui.interact(rect, r.id.with("map-mode"), Sense::click());
    let painter = ui.painter();
    painter.rect_filled(
        rect,
        3.0,
        colour.gamma_multiply(if hit.hovered() || learning {
            0.55
        } else {
            0.30
        }),
    );
    painter.rect_stroke(
        rect,
        3.0,
        egui::Stroke::new(1.5, colour),
        egui::StrokeKind::Inside,
    );
    let text = if learning {
        Some("move a control…".to_owned())
    } else {
        mapped.map(|(_, m)| m.source.short())
    };
    if let Some(text) = text {
        let galley = painter.layout_no_wrap(text, egui::FontId::proportional(10.0), Color32::WHITE);
        let at = egui::pos2(rect.right() - galley.size().x - 3.0, rect.top() - 1.0);
        let back = egui::Rect::from_min_size(at, galley.size()).expand2(egui::vec2(2.0, 0.0));
        painter.rect_filled(back, 2.0, colour.gamma_multiply(0.9));
        painter.galley(at, galley, Color32::WHITE);
    }
    if hit.clicked() {
        actions.push(if learning {
            Action::CancelLearn
        } else {
            Action::LearnTarget(action, label.map(str::to_owned))
        });
    }
    if let Some((i, _)) = mapped {
        hit.context_menu(|ui| {
            if ui.button("Remove the mapping").clicked() {
                actions.push(Action::RemoveMapping(i));
                ui.close();
            }
        });
        hit.on_hover_text("Click to map another control here; right-click to remove");
    } else {
        hit.on_hover_text("Click, then move or press a control on your device");
    }
}

/// The header button that switches map mode on and off: lit while on.
pub fn toggle(ui: &mut Ui, s: &Session, actions: &mut Vec<Action>) {
    if crate::hw::Cap::new("MIDI MAP")
        .lit(s.map_mode.then_some(LEARNING))
        .show(ui)
        .on_hover_text(
            "Map mode (Ctrl+M): click a control on screen, then move or press a control on your \
             MIDI device. Right-click a mapped control to remove it. Escape leaves.",
        )
        .clicked()
    {
        actions.push(Action::SetMapMode(!s.map_mode));
    }
}
