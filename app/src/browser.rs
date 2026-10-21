//! The browser (left panel): search, favourites, and loading an instrument into a slot by
//! double-click or by dragging it onto a slot.

use egui::{Color32, RichText};

use crate::quirks;
use crate::session::Session;
use crate::ui::{Action, UiState, found_label};

pub fn panel(ui: &mut egui::Ui, s: &Session, st: &mut UiState, actions: &mut Vec<Action>) {
    egui::Panel::left("browser")
        .exact_size(235.0)
        .show(ui, |ui| {
            ui.add_space(4.0);
            ui.strong("BROWSER");
            ui.add(
                egui::TextEdit::singleline(&mut st.query)
                    .hint_text("search…")
                    .desired_width(f32::INFINITY),
            );
            ui.horizontal(|ui| {
                ui.selectable_value(&mut st.favs_only, false, "All");
                ui.selectable_value(&mut st.favs_only, true, "Favourites");
                if s.scanning() {
                    ui.label(RichText::new("scanning…").small().weak());
                }
            });
            ui.separator();

            let q = st.query.to_lowercase();
            let matches = |k: usize| {
                let f = &s.found[k];
                (q.is_empty()
                    || f.info.name.to_lowercase().contains(&q)
                    || f.info.vendor.to_lowercase().contains(&q)
                    || f.info
                        .features
                        .iter()
                        .any(|x| x.to_lowercase().contains(&q)))
                    && (!st.favs_only || s.is_favorite(f))
            };
            let shown: Vec<usize> = (0..s.found.len()).filter(|&k| matches(k)).collect();
            // Favourites first when not filtering to them.
            let (favs, others): (Vec<usize>, Vec<usize>) =
                shown.iter().partition(|&&k| s.is_favorite(&s.found[k]));

            egui::ScrollArea::vertical()
                .auto_shrink(false)
                .show(ui, |ui| {
                    if shown.is_empty() {
                        ui.label(RichText::new("nothing found").weak());
                    }
                    for (title, list) in [("Favourites", &favs), ("Instruments", &others)] {
                        if list.is_empty() {
                            continue;
                        }
                        ui.add_space(4.0);
                        ui.label(RichText::new(title).small().color(Color32::from_gray(150)));
                        for &k in list {
                            row(ui, s, k, actions);
                        }
                    }
                });
            ui.separator();
            ui.label(
                RichText::new("Double-click, or drag onto a slot.")
                    .small()
                    .weak(),
            );
        });
}

fn row(ui: &mut egui::Ui, s: &Session, k: usize, actions: &mut Vec<Action>) {
    let f = &s.found[k];
    let unsupported = quirks::lookup(&f.info.id, f.windows).unsupported;
    let crashed = s.quarantine.get(&f.path, &f.info.id).is_some();
    ui.horizontal(|ui| {
        let fav = s.is_favorite(f);
        if ui
            .small_button(RichText::new(if fav { "fav" } else { "+" }).color(if fav {
                Color32::from_rgb(0xe0, 0xb4, 0x4a)
            } else {
                Color32::from_gray(120)
            }))
            .on_hover_text("favourite")
            .clicked()
        {
            actions.push(Action::ToggleFavorite(k));
        }
        let mut text = RichText::new(found_label(f));
        if unsupported.is_some() {
            text = text.color(Color32::from_gray(100));
        } else if crashed {
            text = text.color(Color32::from_rgb(0xe0, 0x6c, 0x5a));
        }
        let drag = ui.dnd_drag_source(ui.make_persistent_id(("found", k)), k, |ui| {
            ui.add(egui::Label::new(text).sense(egui::Sense::click()))
        });
        let label = drag.inner;
        let hover = match (unsupported, crashed) {
            (Some(why), _) => format!("Not supported: {why}"),
            (None, true) => "Crashed the app before; loading it clears that.".to_string(),
            _ => format!(
                "{} · {}\n{}",
                f.info.vendor,
                f.info.version,
                f.path.display()
            ),
        };
        let double_clicked = label.double_clicked();
        label.on_hover_text(hover);
        if unsupported.is_none() && double_clicked {
            actions.push(Action::BrowserLoad(k));
        }
    });
}
