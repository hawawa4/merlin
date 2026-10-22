//! The Macros tab: eight knobs per slot, each moving any number of parameters.

use egui::{Color32, RichText};
use merlin_hosting::ParamInfo;
use merlin_model::{ControlAction, MACROS_PER_SLOT, MacroTarget, SamplerField};

use crate::keyboard::COLORS;
use crate::session::{Session, UI_SLOTS};
use crate::ui::{Action, UiState};

/// Most parameters listed in the picker at once; the search box narrows the rest down.
const PICKER_ROWS: usize = 60;

pub fn show(
    ui: &mut egui::Ui,
    s: &mut Session,
    st: &mut UiState,
    slot: usize,
    actions: &mut Vec<Action>,
) {
    if slot >= UI_SLOTS || s.plugins[slot].is_none() {
        ui.label("Load an instrument into this slot to give it macros.");
        return;
    }
    ui.label(
        RichText::new(
            "A macro is one knob that moves several parameters at once, each over a range you choose (a range can run backwards). Move it here, map a controller knob to it in Settings > Controls, or let a snapshot set it.",
        )
        .small()
        .weak(),
    );
    ui.add_space(4.0);
    egui::ScrollArea::vertical().show(ui, |ui| {
        for k in 0..MACROS_PER_SLOT {
            card(ui, s, st, slot, k, actions);
            ui.add_space(4.0);
        }
    });
}

fn card(
    ui: &mut egui::Ui,
    s: &mut Session,
    st: &mut UiState,
    slot: usize,
    k: usize,
    actions: &mut Vec<Action>,
) {
    let color = COLORS[slot];
    let m = s.macros(slot).get(k).cloned().unwrap_or_default();
    egui::Frame::new()
        .fill(color.gamma_multiply(0.10))
        .stroke(egui::Stroke::new(0.5, color.gamma_multiply(0.5)))
        .inner_margin(egui::Margin::symmetric(8, 4))
        .corner_radius(3)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.colored_label(color, format!("{}", k + 1));
                let mut name = m.name.clone();
                if ui
                    .add(
                        egui::TextEdit::singleline(&mut name)
                            .hint_text(format!("Macro {}", k + 1))
                            .desired_width(150.0),
                    )
                    .changed()
                {
                    actions.push(Action::RenameMacro(slot, k, name));
                }
                let mut v = m.value;
                ui.spacing_mut().slider_width = 260.0;
                let r = ui.add(egui::Slider::new(&mut v, 0.0..=1.0).fixed_decimals(2));
                if r.changed() {
                    actions.push(Action::SetMacro(slot, k, v));
                }
                crate::map_mode::target(
                    ui,
                    s,
                    &r,
                    ControlAction::SlotMacro {
                        slot: slot as u8,
                        index: k as u8,
                    },
                    None,
                    actions,
                );
                picker(ui, s, st, slot, k, actions);
            });
            for (t, target) in m.targets.iter().enumerate() {
                ui.horizontal(|ui| {
                    ui.add_space(24.0);
                    ui.label(target_name(target));
                    let (from, to) = target.range_fractions();
                    let (mut f, mut o) = (from * 100.0, to * 100.0);
                    ui.label(RichText::new("from").small());
                    let a = ui.add(egui::DragValue::new(&mut f).range(0.0..=100.0).suffix("%"));
                    ui.label(RichText::new("to").small());
                    let b = ui.add(egui::DragValue::new(&mut o).range(0.0..=100.0).suffix("%"));
                    if a.changed() || b.changed() {
                        actions.push(Action::SetMacroRange(slot, k, t, f / 100.0, o / 100.0));
                    }
                    if ui
                        .small_button("x")
                        .on_hover_text("Stop driving this")
                        .clicked()
                    {
                        actions.push(Action::RemoveMacroTarget(slot, k, t));
                    }
                });
            }
        });
}

fn target_name(t: &MacroTarget) -> String {
    match t {
        MacroTarget::Param { insert, name, .. } => match insert {
            Some(i) => format!("{name}  (insert {})", i + 1),
            None => name.clone(),
        },
        MacroTarget::Sampler { field, .. } => format!("{}  (sound shaping)", field.name()),
    }
}

/// "+ parameter…": search the instrument's and inserts' parameters.
fn picker(
    ui: &mut egui::Ui,
    s: &mut Session,
    st: &mut UiState,
    slot: usize,
    k: usize,
    actions: &mut Vec<Action>,
) {
    ui.menu_button("+ parameter…", |ui| {
        ui.set_min_width(320.0);
        ui.add(
            egui::TextEdit::singleline(&mut st.macro_query)
                .hint_text("search")
                .desired_width(300.0),
        );
        let q = st.macro_query.to_lowercase();
        let mut shown = 0;
        egui::ScrollArea::vertical()
            .max_height(320.0)
            .show(ui, |ui| {
                if s.is_sampler(slot) {
                    for field in SamplerField::ALL {
                        if q.is_empty() || field.name().to_lowercase().contains(&q) {
                            if ui
                                .button(format!("{}  (sound shaping)", field.name()))
                                .clicked()
                            {
                                actions.push(Action::AddMacroSampler(slot, k, field));
                                ui.close();
                            }
                            shown += 1;
                        }
                    }
                }
                let mut sources: Vec<(Option<usize>, String)> =
                    vec![(None, "Instrument".to_owned())];
                for (i, ins) in s.rig.slots[slot].inserts.iter().enumerate() {
                    if let Some(ins) = ins {
                        sources.push((Some(i), format!("Insert {}: {}", i + 1, ins.plugin.name)));
                    }
                }
                for (insert, title) in sources {
                    let params: Vec<ParamInfo> = s
                        .plugin_params(slot, insert)
                        .iter()
                        .filter(|p| q.is_empty() || p.name.to_lowercase().contains(&q))
                        .take(PICKER_ROWS.saturating_sub(shown))
                        .cloned()
                        .collect();
                    if params.is_empty() {
                        continue;
                    }
                    ui.label(RichText::new(title).small().color(Color32::GRAY));
                    for p in params {
                        shown += 1;
                        let label = if p.module.is_empty() {
                            p.name.clone()
                        } else {
                            format!("{} · {}", p.module, p.name)
                        };
                        if ui.button(label).clicked() {
                            actions.push(Action::AddMacroParam(slot, k, insert, p));
                            ui.close();
                        }
                    }
                }
                if shown == 0 {
                    ui.label("nothing matches");
                } else if shown >= PICKER_ROWS {
                    ui.label(RichText::new("(type to narrow the list)").small().weak());
                }
            });
    });
}
