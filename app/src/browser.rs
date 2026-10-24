//! The browser (left panel): search, favourites, and loading an instrument into a slot by
//! double-click or by dragging it onto a slot.

use egui::{Color32, Popup, PopupCloseBehavior, RichText};

use crate::quirks;
use crate::session::Session;
use crate::tags::{self, Filter, Tag};
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
            tag_filter(ui, &mut st.tag_filter);
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
                        .any(|x| x.to_lowercase().contains(&q))
                    || s.tags_of(f)
                        .list
                        .iter()
                        .any(|t| t.label().to_lowercase().contains(&q)))
                    && (!st.favs_only || s.is_favorite(f))
                    && st.tag_filter.matches(s.tags_of(f).list)
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
                RichText::new("Double-click, or drag onto a slot. Right-click to tag.")
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
        let mut label = None;
        ui.vertical(|ui| {
            let drag = ui.dnd_drag_source(ui.make_persistent_id(("found", k)), k, |ui| {
                ui.add(egui::Label::new(text).sense(egui::Sense::click()))
            });
            label = Some(drag.inner);
            let tags = s.tags_of(f);
            if !tags.list.is_empty() {
                let line: Vec<String> = tags.list.iter().map(Tag::label).collect();
                let text = RichText::new(line.join(", ")).small().weak();
                if tags.automatic {
                    // Italic: the scanner's guess. Right-click to correct it.
                    ui.label(text.italics())
                        .on_hover_text("Tagged automatically. Right-click to change.");
                } else {
                    ui.label(text);
                }
            }
        });
        let label = label.expect("the label is always added");
        tag_menu(&label, s, k, actions);
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

/// The two lists under the search box: a family, then an instrument of it.
fn tag_filter(ui: &mut egui::Ui, filter: &mut Filter) {
    ui.horizontal(|ui| {
        let width = (ui.available_width() - ui.spacing().item_spacing.x) / 2.0;
        egui::ComboBox::from_id_salt("tag_family")
            .width(width - 18.0)
            .selected_text(match *filter {
                Filter::Any => "Any family".to_string(),
                f => f.family().map_or_else(|| f.label(), str::to_owned),
            })
            .show_ui(ui, |ui| {
                ui.selectable_value(filter, Filter::Any, "Any family");
                ui.selectable_value(filter, Filter::Untagged, "Untagged");
                ui.separator();
                for (family, _) in tags::TAXONOMY {
                    // Choosing the family it is already on keeps the instrument chosen.
                    let on = filter.family() == Some(family);
                    if ui.selectable_label(on, *family).clicked() && !on {
                        *filter = Filter::Family(family);
                    }
                }
            });
        let family = filter.family();
        ui.add_enabled_ui(family.is_some(), |ui| {
            egui::ComboBox::from_id_salt("tag_instrument")
                .width(width - 18.0)
                .selected_text(match *filter {
                    Filter::Instrument(_, i) => i,
                    _ => "Any instrument",
                })
                .show_ui(ui, |ui| {
                    let Some(family) = family else { return };
                    ui.selectable_value(filter, Filter::Family(family), "Any instrument");
                    for &i in tags::instruments(family) {
                        ui.selectable_value(filter, Filter::Instrument(family, i), i);
                    }
                });
        });
    });
}

/// Right-click on an entry: favourite it and tag it by family and instrument. The menu stays
/// open while tags are ticked, since a plugin can have several.
fn tag_menu(label: &egui::Response, s: &Session, k: usize, actions: &mut Vec<Action>) {
    let f = &s.found[k];
    let tags = s.tags_of(f);
    let current = tags.list;
    Popup::context_menu(label)
        .close_behavior(PopupCloseBehavior::CloseOnClickOutside)
        .show(|ui| {
            if ui.selectable_label(s.is_favorite(f), "Favourite").clicked() {
                actions.push(Action::ToggleFavorite(k));
            }
            ui.separator();
            ui.label(
                RichText::new(if tags.automatic {
                    "Tag as (now automatic; changing it makes it yours)"
                } else {
                    "Tag as"
                })
                .small()
                .weak(),
            );
            for (family, instruments) in tags::TAXONOMY {
                let in_family = current.iter().any(|t| t.family == *family);
                ui.menu_button(
                    RichText::new(*family).strong().color(if in_family {
                        Color32::from_rgb(0xe0, 0xb4, 0x4a)
                    } else {
                        ui.visuals().text_color()
                    }),
                    |ui| {
                        let general = Tag::new(family, None);
                        if ui
                            .selectable_label(current.contains(&general), "Just the family")
                            .clicked()
                        {
                            actions.push(Action::ToggleTag(k, general));
                        }
                        ui.separator();
                        for &i in *instruments {
                            let tag = Tag::new(family, Some(i));
                            if ui.selectable_label(current.contains(&tag), i).clicked() {
                                actions.push(Action::ToggleTag(k, tag));
                            }
                        }
                    },
                );
            }
            if !current.is_empty() || tags.manual {
                ui.separator();
            }
            if !current.is_empty() && ui.button("Clear tags").clicked() {
                actions.push(Action::ClearTags(k));
            }
            if tags.manual
                && ui
                    .button("Use automatic tags")
                    .on_hover_text("Forget your tags and let the scanner decide again")
                    .clicked()
            {
                actions.push(Action::ResetTags(k));
            }
        });
}
