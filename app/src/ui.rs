//! Drawing: header, rack, editor dock bar, keyboard map. Pure view code: it reads the
//! session and returns actions; the app applies them.

use std::path::PathBuf;
use std::sync::atomic::Ordering;

use egui::{Color32, RichText};

use crate::keyboard::{self, COLORS, ZoneView};
use crate::paths;
use crate::session::{Session, UI_SLOTS};

#[derive(Debug)]
pub enum Action {
    /// Load browser entry `k` into slot `i`.
    Load(usize, usize),
    Unload(usize),
    /// Try a missing or quarantined slot again.
    Retry(usize),
    Select(usize, bool),
    Mute(usize),
    Zone(usize, u8, u8),
    Volume(usize, f32),
    Transpose(usize, i8),
    Fit,
    Scroll(u32, u32),
    NoteOn(u8),
    NoteOff(u8),
    Panic,
    NewRig,
    SaveRig,
    OpenRig(PathBuf),
    RenameRig(String),
    // Script-only.
    LoadNamed(usize, String),
    SaveAs(PathBuf),
    OpenPath(PathBuf),
    Peak,
    Cpu,
    Report,
    Menu(usize),
    CloseMenu,
    Quit,
}

/// UI state that is not part of the session.
pub struct UiState {
    pub peaks_shown: [f32; UI_SLOTS],
    pub held_key: Option<u8>,
    /// Test hook: open this slot's load menu on the next frame.
    pub force_menu: Option<usize>,
    pub popup_open: bool,
    pub status: StatusInfo,
}

/// Numbers the app measures and the status line shows.
#[derive(Default, Clone, Copy)]
pub struct StatusInfo {
    pub cpu_self: f32,
    pub frames_per_s: f64,
    pub wakeups_per_s: f64,
}

impl UiState {
    pub fn new() -> Self {
        Self {
            peaks_shown: [0.0; UI_SLOTS],
            held_key: None,
            force_menu: None,
            popup_open: false,
            status: StatusInfo::default(),
        }
    }
}

pub fn draw(
    ui: &mut egui::Ui,
    s: &mut Session,
    st: &mut UiState,
    dock_rect: &mut egui::Rect,
    actions: &mut Vec<Action>,
) {
    egui::Panel::top("header").show(ui, |ui| header(ui, s, st, actions));
    egui::Panel::top("rack").show(ui, |ui| {
        ui.add_space(2.0);
        for i in 0..UI_SLOTS {
            unit(ui, s, st, i, actions);
        }
    });
    egui::Panel::bottom("keys").show(ui, |ui| keys(ui, s, st, actions));
    egui::Panel::top("dockbar").show(ui, |ui| dock_bar(ui, s, actions));
    egui::CentralPanel::default().show(ui, |ui| {
        *dock_rect = ui.available_rect_before_wrap();
        let note = s.viewing.and_then(|i| s.notes[i].as_deref());
        let text = if s.viewing.is_none() {
            "Load an instrument into a slot, then select it"
        } else if st.popup_open {
            "(editor hidden while a menu is open)"
        } else if s.editors.shown() != s.viewing {
            note.unwrap_or("This plugin has no editor")
        } else {
            ""
        };
        ui.centered_and_justified(|ui| ui.label(text));
    });
}

fn header(ui: &mut egui::Ui, s: &mut Session, st: &UiState, actions: &mut Vec<Action>) {
    ui.horizontal(|ui| {
        ui.strong("MERLIN");
        ui.separator();
        ui.label("Rig");
        let mut name = s.rig.name.clone();
        if ui
            .add(egui::TextEdit::singleline(&mut name).desired_width(160.0))
            .changed()
        {
            actions.push(Action::RenameRig(name));
        }
        if s.dirty {
            ui.label(RichText::new("modified").color(Color32::from_rgb(0xe0, 0xb4, 0x4a)));
        }
        if ui.button("New").clicked() {
            actions.push(Action::NewRig);
        }
        if ui.button("Save").clicked() {
            actions.push(Action::SaveRig);
        }
        egui::ComboBox::from_id_salt("open-rig")
            .selected_text("Open…")
            .width(90.0)
            .show_ui(ui, |ui| {
                let mut files: Vec<PathBuf> = std::fs::read_dir(paths::rigs_dir())
                    .into_iter()
                    .flatten()
                    .flatten()
                    .map(|e| e.path())
                    .filter(|p| p.extension().is_some_and(|e| e == "irig"))
                    .collect();
                files.sort();
                if files.is_empty() {
                    ui.label("no saved rigs");
                }
                for f in files {
                    let label = f.file_stem().unwrap().to_string_lossy().into_owned();
                    if ui.selectable_label(false, label).clicked() {
                        actions.push(Action::OpenRig(f));
                    }
                }
            });
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui
                .add(
                    egui::Button::new(RichText::new("PANIC").strong().color(Color32::WHITE))
                        .fill(Color32::from_rgb(0xa0, 0x30, 0x30)),
                )
                .clicked()
            {
                actions.push(Action::Panic);
            }
            let (xruns, dsp, buf) = s
                .backend()
                .map(|b| {
                    (
                        b.stats.xruns.load(Ordering::Relaxed),
                        b.cpu_load(),
                        b.buffer_size(),
                    )
                })
                .unwrap_or_default();
            ui.label(format!(
                "{buf} @ {} kHz · DSP {dsp:.1}% · xruns {xruns} · UI {:.1}% · {:.0} fps",
                s.sample_rate / 1000,
                st.status.cpu_self,
                st.status.frames_per_s
            ));
        });
    });
    if let Some(line) = s.log.last() {
        ui.label(RichText::new(line).small().weak());
    }
}

fn unit(ui: &mut egui::Ui, s: &mut Session, st: &mut UiState, i: usize, actions: &mut Vec<Action>) {
    let selected = s.viewing == Some(i);
    let color = COLORS[i];
    let loaded = s.plugins[i].is_some();
    let saved = &s.rig.slots[i];
    let missing = s.missing[i].clone();
    let (active, mute) = (s.targets.contains(&i), saved.mute);
    let (mut lo, mut hi, mut vol, mut tr) = (
        saved.filter.key_lo,
        saved.filter.key_hi,
        saved.volume,
        saved.filter.transpose,
    );
    let label = match (&saved.instrument, loaded) {
        (Some(r), true) => format!(
            "{}  {}",
            r.name,
            if r.windows { "CLAP·WIN" } else { "CLAP" }
        ),
        (Some(r), false) => format!(
            "{}  ({})",
            r.name,
            if missing
                .as_deref()
                .is_some_and(|m| m.starts_with("quarantined"))
            {
                "quarantined"
            } else {
                "missing"
            }
        ),
        (None, _) => "— empty —".to_string(),
    };
    let frame = egui::Frame::new()
        .fill(color.gamma_multiply(if selected { 0.30 } else { 0.12 }))
        .stroke(egui::Stroke::new(
            if selected { 1.5 } else { 0.5 },
            color.gamma_multiply(if selected { 1.0 } else { 0.5 }),
        ))
        .inner_margin(egui::Margin::symmetric(6, 2))
        .corner_radius(3);
    frame.show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            ui.colored_label(color, format!("{}", i + 1));
            let text = if missing.is_some() {
                RichText::new(label).color(Color32::from_rgb(0xe0, 0x6c, 0x5a))
            } else {
                RichText::new(label)
            };
            let r = ui.add_sized(
                [250.0, 20.0],
                egui::Button::selectable(selected, text)
                    .frame(false)
                    .sense(egui::Sense::click()),
            );
            let r = match &missing {
                Some(why) => r.on_hover_text(format!(
                    "Could not load: {why}\nThe saved plugin and state are kept."
                )),
                None => r,
            };
            if loaded && r.clicked() {
                actions.push(Action::Select(i, ui.input(|inp| inp.modifiers.shift)));
            }
            let led = if active && !mute {
                color
            } else {
                Color32::from_gray(60)
            };
            let (rect, _) = ui.allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::hover());
            ui.painter().circle_filled(rect.center(), 5.0, led);
            ui.add(
                egui::ProgressBar::new(st.peaks_shown[i].clamp(0.0, 1.0).sqrt())
                    .desired_width(60.0),
            );
            if loaded {
                if ui.selectable_label(mute, "M").clicked() {
                    actions.push(Action::Mute(i));
                }
                ui.label("vol");
                if ui
                    .add(
                        egui::DragValue::new(&mut vol)
                            .range(0.0..=2.0)
                            .speed(0.01)
                            .fixed_decimals(2),
                    )
                    .changed()
                {
                    actions.push(Action::Volume(i, vol));
                }
                ui.label("zone");
                let a = ui.add(egui::DragValue::new(&mut lo).range(0..=127));
                ui.label("–");
                let b = ui.add(egui::DragValue::new(&mut hi).range(0..=127));
                if a.changed() || b.changed() {
                    actions.push(Action::Zone(i, lo, hi));
                }
                ui.label("tr");
                if ui
                    .add(egui::DragValue::new(&mut tr).range(-48..=48))
                    .changed()
                {
                    actions.push(Action::Transpose(i, tr));
                }
            }
            let menu = egui::ComboBox::from_id_salt(("load", i))
                .selected_text(if saved.instrument.is_some() {
                    "Replace…"
                } else {
                    "Load…"
                })
                .width(100.0)
                .show_ui(ui, |ui| {
                    for (k, f) in s.found.iter().enumerate() {
                        let mut label = if f.windows {
                            format!("{} (Windows)", f.info.name)
                        } else {
                            f.info.name.clone()
                        };
                        let unsupported = crate::quirks::lookup(&f.info.id, f.windows).unsupported;
                        if unsupported.is_some() {
                            label.push_str("  (unsupported)");
                        } else if s.quarantine.get(&f.path, &f.info.id).is_some() {
                            label.push_str("  (crashed before)");
                        }
                        let r = ui.selectable_label(false, label);
                        if let Some(why) = unsupported {
                            r.on_hover_text(why);
                        } else if r.clicked() {
                            actions.push(Action::Load(i, k));
                        }
                    }
                    if s.found.is_empty() {
                        ui.label("no plugins found");
                    }
                });
            if st.force_menu == Some(i) {
                // Test hook (script `menu`): open this slot's menu without a click.
                st.force_menu = None;
                egui::Popup::open_id(ui.ctx(), menu.response.id.with("popup"));
            }
            if missing.is_some() && ui.small_button("Retry").clicked() {
                actions.push(Action::Retry(i));
            }
            if (loaded || saved.instrument.is_some()) && ui.small_button("x").clicked() {
                actions.push(Action::Unload(i));
            }
        });
    });
}

fn keys(ui: &mut egui::Ui, s: &Session, st: &mut UiState, actions: &mut Vec<Action>) {
    let zones: [ZoneView; UI_SLOTS] = std::array::from_fn(|i| {
        let slot = &s.rig.slots[i];
        ZoneView {
            loaded: s.plugins[i].is_some(),
            active: s.targets.contains(&i) && !slot.mute,
            lo: slot.filter.key_lo,
            hi: slot.filter.key_hi,
        }
    });
    let pressed = keyboard::show(ui, &zones, &s.engine.shared);
    match (pressed, st.held_key) {
        (Some(k), None) => {
            st.held_key = Some(k);
            actions.push(Action::NoteOn(k));
        }
        (None, Some(k)) => {
            st.held_key = None;
            actions.push(Action::NoteOff(k));
        }
        _ => {}
    }
}

fn dock_bar(ui: &mut egui::Ui, s: &Session, actions: &mut Vec<Action>) {
    ui.horizontal(|ui| {
        let Some(i) = s.viewing else {
            ui.label("EDITOR · nothing selected");
            return;
        };
        let name = s.rig.slots[i]
            .instrument
            .as_ref()
            .map(|r| r.name.as_str())
            .unwrap_or("");
        ui.colored_label(COLORS[i], format!("EDITOR · {} {name}", i + 1));
        let Some(info) = s.editors.info(i) else {
            return;
        };
        if ui
            .add_enabled(info.can_resize, egui::Button::new("Fit to dock"))
            .clicked()
        {
            actions.push(Action::Fit);
        }
        let (dw, dh) = s.editors.dock_size();
        let (ew, eh) = (info.size.width, info.size.height);
        ui.label(format!("{ew}x{eh} in {dw}x{dh}"));
        let (mut sx, mut sy) = info.scroll;
        ui.label("scroll x");
        let rx = ui.add_enabled(
            ew > dw,
            egui::Slider::new(&mut sx, 0..=ew.saturating_sub(dw)),
        );
        ui.label("y");
        let ry = ui.add_enabled(
            eh > dh,
            egui::Slider::new(&mut sy, 0..=eh.saturating_sub(dh)),
        );
        if rx.changed() || ry.changed() {
            actions.push(Action::Scroll(sx, sy));
        }
    });
}
