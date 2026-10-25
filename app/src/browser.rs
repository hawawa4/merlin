//! The browser (left panel): search, favourites, and loading an instrument into a slot by
//! double-click or by dragging it onto a slot.

use egui::{Color32, Popup, PopupCloseBehavior, RichText, vec2};

use crate::icons;
use crate::quirks;
use crate::session::Session;
use crate::tags::{self, Filter, Tag};
use crate::ui::{Action, UiState, found_label};

pub fn panel(ui: &mut egui::Ui, s: &Session, st: &mut UiState, actions: &mut Vec<Action>) {
    if !st.browser_open {
        // Folded: just a strip with a way back.
        egui::Panel::left("browser-folded")
            .exact_size(34.0)
            .show(ui, |ui| {
                ui.add_space(4.0);
                if crate::hw::Cap::new("")
                    .icon(crate::hw::Icon::ChevronRight)
                    .min_width(24.0)
                    .show(ui)
                    .on_hover_text("Show the browser")
                    .clicked()
                {
                    st.browser_open = true;
                }
            });
        return;
    }
    egui::Panel::left("browser")
        .resizable(true)
        .default_size(235.0)
        .size_range(170.0..=520.0)
        .show(ui, |ui| {
            ui.add_space(4.0);
            // Names are dragged, not selected: no text cursor, and a drag never sweeps up the
            // rows it passes over.
            ui.style_mut().interaction.selectable_labels = false;
            ui.horizontal(|ui| {
                ui.strong("BROWSER");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if crate::hw::Cap::new("")
                        .icon(crate::hw::Icon::ChevronLeft)
                        .min_width(30.0)
                        .show(ui)
                        .on_hover_text("Fold the browser away")
                        .clicked()
                    {
                        st.browser_open = false;
                    }
                });
            });
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
            let failed = s.entries.iter().filter(|e| e.error.is_some()).count();
            if failed > 0 && !s.scanning() {
                let text = format!(
                    "{failed} file{} could not be scanned",
                    if failed == 1 { "" } else { "s" }
                );
                if ui
                    .small_button(RichText::new(text).small())
                    .on_hover_text("Open Settings > Plugins to see which, retry or ignore them")
                    .clicked()
                {
                    actions.push(Action::ShowSettingsTab(
                        crate::settings_ui::SettingsTab::Plugins,
                    ));
                }
            }
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
            // Narrowed by a search, a filter or favourites: show everything that is left rather
            // than make the user open branches to find it.
            let force_open =
                (!q.is_empty() || st.favs_only || st.tag_filter != Filter::Any).then_some(true);
            let tree = build_tree(s, &shown);

            egui::ScrollArea::vertical()
                .auto_shrink(false)
                .show(ui, |ui| {
                    if shown.is_empty() {
                        ui.label(RichText::new("nothing found").weak());
                    }
                    // The Favourites view is a plain list; in All, a favourite stays where it is
                    // and only its star changes.
                    if st.favs_only {
                        for &k in &shown {
                            row(ui, s, k, true, actions);
                        }
                        return;
                    }
                    for family in &tree.families {
                        let salt = family.name;
                        branch(
                            ui,
                            icons::glyph_for(family.name, None),
                            family.name,
                            salt,
                            family.count(),
                            force_open,
                            |ui| {
                                for &k in &family.general {
                                    row(ui, s, k, false, actions);
                                }
                                for (instrument, list) in &family.instruments {
                                    let salt = format!("{}/{instrument}", family.name);
                                    branch(
                                        ui,
                                        icons::glyph_for(family.name, Some(instrument)),
                                        instrument,
                                        &salt,
                                        list.len(),
                                        force_open,
                                        |ui| {
                                            for &k in list {
                                                row(ui, s, k, false, actions);
                                            }
                                        },
                                    );
                                }
                            },
                        );
                    }
                    if !tree.untagged.is_empty() {
                        branch(
                            ui,
                            icons::Glyph::Untagged,
                            "Untagged",
                            "untagged",
                            tree.untagged.len(),
                            force_open,
                            |ui| {
                                for &k in &tree.untagged {
                                    row(ui, s, k, true, actions);
                                }
                            },
                        );
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

/// What the tree shows: the plugins under each family and instrument they are tagged with (a
/// plugin with two tags is under both), and those no tag places.
struct Tree {
    families: Vec<Family>,
    untagged: Vec<usize>,
}

struct Family {
    name: &'static str,
    /// Tagged with the family alone.
    general: Vec<usize>,
    instruments: Vec<(&'static str, Vec<usize>)>,
}

impl Family {
    /// How many different plugins are in the family.
    fn count(&self) -> usize {
        let mut all: Vec<usize> = self.general.clone();
        all.extend(self.instruments.iter().flat_map(|(_, l)| l.iter().copied()));
        all.sort_unstable();
        all.dedup();
        all.len()
    }
}

fn build_tree(s: &Session, shown: &[usize]) -> Tree {
    let mut placed = vec![false; s.found.len()];
    let mut families = Vec::new();
    for (name, instruments) in tags::TAXONOMY {
        let mut family = Family {
            name,
            general: Vec::new(),
            instruments: Vec::new(),
        };
        for &k in shown {
            let list = s.tags_of(&s.found[k]).list;
            if list
                .iter()
                .any(|t| t.family == *name && t.instrument.is_none())
            {
                family.general.push(k);
                placed[k] = true;
            }
        }
        for &instrument in *instruments {
            let here: Vec<usize> = shown
                .iter()
                .copied()
                .filter(|&k| {
                    s.tags_of(&s.found[k])
                        .list
                        .iter()
                        .any(|t| t.family == *name && t.instrument.as_deref() == Some(instrument))
                })
                .collect();
            if !here.is_empty() {
                for &k in &here {
                    placed[k] = true;
                }
                family.instruments.push((instrument, here));
            }
        }
        if !family.general.is_empty() || !family.instruments.is_empty() {
            families.push(family);
        }
    }
    let untagged = shown.iter().copied().filter(|&k| !placed[k]).collect();
    Tree { families, untagged }
}

/// One collapsible branch of the tree: a chevron, the picture of what is inside, its name.
/// The whole header line opens and closes it.
fn branch(
    ui: &mut egui::Ui,
    glyph: icons::Glyph,
    title: &str,
    id: &str,
    count: usize,
    force_open: Option<bool>,
    body: impl FnOnce(&mut egui::Ui),
) {
    let key = ui.make_persistent_id(("browser-branch", id));
    let stored = ui
        .data_mut(|d| d.get_persisted::<bool>(key))
        .unwrap_or(false);
    let open = force_open.unwrap_or(stored);
    let head = ui
        .horizontal(|ui| {
            let (chevron, _) = ui.allocate_exact_size(vec2(12.0, 16.0), egui::Sense::hover());
            let c = chevron.center();
            let points = if open {
                vec![
                    c + vec2(-4.0, -2.0),
                    c + vec2(4.0, -2.0),
                    c + vec2(0.0, 3.0),
                ]
            } else {
                vec![
                    c + vec2(-2.0, -4.0),
                    c + vec2(-2.0, 4.0),
                    c + vec2(3.0, 0.0),
                ]
            };
            ui.painter().add(egui::Shape::convex_polygon(
                points,
                crate::hw::TEXT_DIM,
                egui::Stroke::NONE,
            ));
            icons::show(ui, glyph, crate::hw::TEXT);
            ui.label(RichText::new(title).strong());
            ui.label(RichText::new(format!("({count})")).small().weak());
        })
        .response
        .interact(egui::Sense::click())
        .on_hover_cursor(egui::CursorIcon::PointingHand);
    if head.clicked() {
        ui.data_mut(|d| d.insert_persisted(key, !open));
    }
    if open {
        ui.indent(key, body);
    }
}

fn row(ui: &mut egui::Ui, s: &Session, k: usize, show_tags: bool, actions: &mut Vec<Action>) {
    let f = &s.found[k];
    let unsupported = quirks::lookup(&f.info.id, f.windows).unsupported;
    let crashed = s.quarantine.get(&f.path, &f.info.id).is_some();
    ui.horizontal(|ui| {
        let fav = s.is_favorite(f);
        if crate::hw::star(ui, fav)
            .on_hover_text(if fav {
                "Favourite (click to remove)"
            } else {
                "Add to favourites"
            })
            .clicked()
        {
            actions.push(Action::ToggleFavorite(k));
        }
        // The picture of the first tag, so a list of plugins says what each one is.
        match s.tags_of(f).list.first() {
            Some(tag) => {
                icons::show(
                    ui,
                    icons::glyph_for(&tag.family, tag.instrument.as_deref()),
                    crate::hw::TEXT_DIM,
                );
            }
            None => icons::blank(ui),
        }
        let mut text = RichText::new(found_label(f));
        if unsupported.is_some() {
            text = text.color(Color32::from_gray(100));
        } else if crashed {
            text = text.color(Color32::from_rgb(0xe0, 0x6c, 0x5a));
        }
        let mut label = None;
        ui.vertical(|ui| {
            // One widget that clicks and drags, so a right-click is never taken by a drag layer
            // laid over the name.
            let name = ui.add(egui::Label::new(text).sense(egui::Sense::click_and_drag()));
            if name.drag_started_by(egui::PointerButton::Primary) {
                name.dnd_set_drag_payload(k);
            }
            if ui.ctx().is_being_dragged(name.id) {
                ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
                drag_preview(ui.ctx(), name.id, &found_label(f));
            } else {
                name.clone().on_hover_cursor(egui::CursorIcon::Grab);
            }
            label = Some(name);
            let tags = s.tags_of(f);
            if show_tags && !tags.list.is_empty() {
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

/// The name of the plugin being dragged, following the pointer.
fn drag_preview(ctx: &egui::Context, id: egui::Id, name: &str) {
    let Some(pos) = ctx.pointer_interact_pos() else {
        return;
    };
    egui::Area::new(id.with("drag-preview"))
        .order(egui::Order::Tooltip)
        .fixed_pos(pos + vec2(14.0, 8.0))
        .interactable(false)
        .show(ctx, |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| ui.label(name));
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
            if ui
                .button("Hide from the library")
                .on_hover_text(
                    "Not an instrument, or not wanted: it is never shown again. Settings > Plugins lists what is hidden and brings it back.",
                )
                .clicked()
            {
                actions.push(Action::HidePlugin(k));
                ui.close();
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
