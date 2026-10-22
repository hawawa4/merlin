//! The Parameters tab: every parameter the instrument or an insert offers, each with a button
//! that maps it to a MIDI control (turn a knob and it follows).

use egui::{Color32, RichText};
use merlin_hosting::ParamInfo;
use merlin_model::{ControlAction, INSTRUMENT};

use crate::keyboard::COLORS;
use crate::session::{Session, UI_SLOTS};
use crate::ui::{Action, UiState};

pub fn show(
    ui: &mut egui::Ui,
    s: &mut Session,
    st: &mut UiState,
    slot: usize,
    actions: &mut Vec<Action>,
) {
    if slot >= UI_SLOTS || s.plugins[slot].is_none() {
        ui.label("Load an instrument into this slot to see its parameters.");
        return;
    }
    ui.label(
        RichText::new(
            "Any parameter can sit on a knob or fader of your controller: press Map, then move the control. The parameter follows it. (The plugin's own window shows the sound; its values are not read back here, so a knob that picks up starts from where this app last put the parameter.)",
        )
        .small()
        .weak(),
    );
    ui.add_space(4.0);
    // Which plugin: the instrument or one of its inserts.
    let mut sources: Vec<(Option<usize>, String)> = vec![(None, "Instrument".to_owned())];
    for (i, ins) in s.rig.slots[slot].inserts.iter().enumerate() {
        if let Some(ins) = ins {
            sources.push((Some(i), format!("Insert {}: {}", i + 1, ins.plugin.name)));
        }
    }
    if !sources.iter().any(|(k, _)| *k == st.param_insert) {
        st.param_insert = None;
    }
    ui.horizontal(|ui| {
        egui::ComboBox::from_id_salt("param-source")
            .selected_text(
                sources
                    .iter()
                    .find(|(k, _)| *k == st.param_insert)
                    .map_or("Instrument", |(_, t)| t.as_str()),
            )
            .show_ui(ui, |ui| {
                for (k, title) in &sources {
                    ui.selectable_value(&mut st.param_insert, *k, title);
                }
            });
        ui.add(
            egui::TextEdit::singleline(&mut st.param_query)
                .hint_text("search")
                .desired_width(220.0),
        );
    });
    ui.add_space(4.0);
    let insert = st.param_insert;
    let q = st.param_query.to_lowercase();
    let params: Vec<ParamInfo> = s
        .plugin_params(slot, insert)
        .iter()
        .filter(|p| q.is_empty() || p.name.to_lowercase().contains(&q))
        .cloned()
        .collect();
    if params.is_empty() {
        ui.label(if q.is_empty() {
            "This plugin lists no parameters (a SoundFont's sound shaping is under Macros)."
        } else {
            "nothing matches"
        });
        return;
    }
    let color = COLORS[slot];
    let row_h = ui.spacing().interact_size.y + 4.0;
    egui::ScrollArea::vertical().show_rows(ui, row_h, params.len(), |ui, range| {
        for p in &params[range] {
            let action = ControlAction::SlotParam {
                slot: slot as u8,
                insert: insert.map_or(INSTRUMENT, |i| i as u8),
                id: p.id,
            };
            ui.horizontal(|ui| {
                ui.set_height(row_h - 4.0);
                let learning = s.learning() == Some(action);
                let mapped = s.mapping_for(action);
                let label = if learning {
                    "move a control…".to_owned()
                } else {
                    mapped.map_or("Map".to_owned(), |(_, m)| m.source.short())
                };
                let b = egui::Button::new(label)
                    .min_size(egui::vec2(96.0, 0.0))
                    .fill(if learning {
                        Color32::from_rgb(0x8a, 0x6a, 0x20)
                    } else if mapped.is_some() {
                        Color32::from_rgb(0x2a, 0x6a, 0x3a)
                    } else {
                        ui.visuals().widgets.inactive.weak_bg_fill
                    });
                let r = ui.add(b);
                if r.clicked() {
                    actions.push(if learning {
                        Action::CancelLearn
                    } else {
                        Action::LearnTarget(action, Some(p.name.clone()))
                    });
                }
                if let Some((i, _)) = mapped {
                    if ui
                        .small_button("x")
                        .on_hover_text("Remove the mapping")
                        .clicked()
                    {
                        actions.push(Action::RemoveMapping(i));
                    }
                } else {
                    ui.add_space(24.0);
                }
                ui.colored_label(color, &p.name);
                if !p.module.is_empty() {
                    ui.label(RichText::new(&p.module).small().color(Color32::GRAY));
                }
            });
        }
    });
}
