//! The Perform view (plan §4.6, Phase 3): a stage screen with big targets and nothing that can
//! break the rig by accident. Snapshots across the top, one tile per instrument with its level
//! and a mute, the master fader at the bottom. With the lock on, the view cannot be left and
//! the app ignores dropped files until the Unlock button has been held for a moment.

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use egui::{Color32, RichText};
use merlin_model::ControlAction;

use crate::keyboard::COLORS;
use crate::session::{Session, TrackState, UI_SLOTS};
use crate::ui::{Action, UiState};

/// How long Unlock has to be held: long enough that a brush against a touch screen does nothing.
pub const UNLOCK_HOLD: Duration = Duration::from_millis(1200);

pub fn show(ui: &mut egui::Ui, s: &Session, st: &mut UiState, actions: &mut Vec<Action>) {
    egui::Panel::top("perform-stage").show(ui, |ui| stage_strip(ui, st, actions));
    egui::Panel::top("perform-transport").show(ui, |ui| transport(ui, s, actions));
    egui::Panel::bottom("perform-master").show(ui, |ui| master(ui, s, actions));
    egui::CentralPanel::default().show(ui, |ui| {
        egui::ScrollArea::vertical().show(ui, |ui| {
            setlist(ui, s, actions);
            snapshots(ui, s, actions);
            ui.add_space(14.0);
            tiles(ui, s, st, actions);
        });
    });
    if !st.locked && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
        actions.push(Action::SetPerform(false));
    }
}

/// The stage view's own controls, so the header can stay the same as in every other view.
fn stage_strip(ui: &mut egui::Ui, st: &mut UiState, actions: &mut Vec<Action>) {
    ui.horizontal(|ui| {
        crate::hw::engraved(ui, "Stage");
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            lock_cap(ui, st, actions);
            if crate::hw::Cap::new("FULL SCREEN").show(ui).clicked() {
                actions.push(Action::ToggleFullscreen);
            }
        });
    });
}

/// Lock, or (while locked) Unlock that has to be held.
pub fn lock_cap(ui: &mut egui::Ui, st: &mut UiState, actions: &mut Vec<Action>) {
    if !st.locked {
        if crate::hw::Cap::new("LOCK")
            .show(ui)
            .on_hover_text(
                "Stops this view from being left, and ignores dropped files, until unlocked",
            )
            .clicked()
        {
            actions.push(Action::SetLock(true));
        }
        return;
    }
    let held = st.unlock_since.map_or(0.0, |t| {
        t.elapsed().as_secs_f32() / UNLOCK_HOLD.as_secs_f32()
    });
    let r = crate::hw::Cap::new("LOCKED \u{b7} HOLD TO UNLOCK")
        .lit(Some(crate::hw::AMBER))
        .show(ui)
        .on_hover_text("Hold the button down for a moment to unlock");
    if held > 0.0 {
        let bar = egui::Rect::from_min_size(
            egui::pos2(r.rect.left() + 4.0, r.rect.bottom() - 6.0),
            egui::vec2((r.rect.width() - 8.0) * held.min(1.0), 4.0),
        );
        ui.painter().rect_filled(bar, 1.0, Color32::WHITE);
    }
    // egui stops calling a press a click after 0.8 s, which is before the hold is over, so once
    // the press has started it counts for as long as the button stays down on the cap.
    let pressed = r.is_pointer_button_down_on()
        || (st.unlock_since.is_some()
            && r.contains_pointer()
            && ui.input(|i| i.pointer.primary_down()));
    if pressed {
        let since = *st.unlock_since.get_or_insert_with(Instant::now);
        if since.elapsed() >= UNLOCK_HOLD {
            st.unlock_since = None;
            actions.push(Action::SetLock(false));
        } else {
            ui.ctx().request_repaint();
        }
    } else {
        st.unlock_since = None;
    }
}

/// Start / stop, the bar and beat, the tempo with tap, and the click.
fn transport(ui: &mut egui::Ui, s: &Session, actions: &mut Vec<Action>) {
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        let running = s.transport_running();
        let counting = s.counting_in();
        let label = if running || counting { "STOP" } else { "START" };
        let b = egui::Button::new(RichText::new(label).size(22.0).strong())
            .min_size(egui::vec2(130.0, 52.0))
            .fill(if running || counting {
                Color32::from_rgb(0x7a, 0x2a, 0x2a)
            } else {
                Color32::from_rgb(0x2a, 0x6a, 0x3a)
            });
        let b = ui.add(b);
        crate::map_mode::target(ui, s, &b, ControlAction::TransportToggle, None, actions);
        if b.clicked() && !s.map_mode {
            actions.push(Action::TransportToggle);
        }
        let (bar, beat) = s.engine.shared.transport.position();
        let position = if counting {
            format!("count-in {beat}")
        } else if running {
            format!("{bar} . {beat}")
        } else {
            "stopped".to_owned()
        };
        ui.label(RichText::new(position).size(24.0).monospace());
        ui.add_space(16.0);
        if ui
            .add(egui::Button::new("-").min_size(egui::vec2(40.0, 40.0)))
            .clicked()
        {
            actions.push(Action::SetTempo(s.tempo() - 1.0));
        }
        let bpm = ui.label(RichText::new(format!("{:.0} bpm", s.tempo())).size(24.0));
        crate::map_mode::target(ui, s, &bpm, ControlAction::Tempo, None, actions);
        if ui
            .add(egui::Button::new("+").min_size(egui::vec2(40.0, 40.0)))
            .clicked()
        {
            actions.push(Action::SetTempo(s.tempo() + 1.0));
        }
        let tap = ui
            .add(
                egui::Button::new(RichText::new("TAP").size(18.0)).min_size(egui::vec2(70.0, 40.0)),
            )
            .on_hover_text("Tap the beat to set the tempo");
        crate::map_mode::target(ui, s, &tap, ControlAction::TapTempo, None, actions);
        if tap.clicked() && !s.map_mode {
            actions.push(Action::TapTempo);
        }
        ui.add_space(16.0);
        let t = s.rig.transport;
        let click = egui::Button::new(RichText::new("CLICK").size(16.0))
            .selected(t.click)
            .min_size(egui::vec2(80.0, 40.0));
        let click = ui.add(click);
        crate::map_mode::target(ui, s, &click, ControlAction::ClickToggle, None, actions);
        if click.clicked() && !s.map_mode {
            actions.push(Action::ToggleClick);
        }
        let mut volume = t.click_volume;
        ui.spacing_mut().slider_width = 100.0;
        let level = ui.add(egui::Slider::new(&mut volume, 0.0..=1.0).show_value(false));
        if level.changed() {
            actions.push(Action::SetClickVolume(volume));
        }
        crate::map_mode::target(ui, s, &level, ControlAction::ClickLevel, None, actions);
        ui.label("count-in");
        let mut bars = t.count_in_bars;
        egui::ComboBox::from_id_salt("count-in")
            .selected_text(match bars {
                0 => "off".to_owned(),
                1 => "1 bar".to_owned(),
                n => format!("{n} bars"),
            })
            .width(80.0)
            .show_ui(ui, |ui| {
                for n in 0..=merlin_model::MAX_COUNT_IN_BARS {
                    let text = match n {
                        0 => "off".to_owned(),
                        1 => "1 bar".to_owned(),
                        n => format!("{n} bars"),
                    };
                    ui.selectable_value(&mut bars, n, text);
                }
            });
        if bars != t.count_in_bars {
            actions.push(Action::SetCountIn(bars));
        }
    });
    backing_row(ui, s, actions);
    ui.add_space(4.0);
}

/// The song's backing tracks: the clock and a mute for each.
fn backing_row(ui: &mut egui::Ui, s: &Session, actions: &mut Vec<Action>) {
    let Some(n) = s.armed_song() else { return };
    let Some(song) = s.songs().get(n) else { return };
    if song.tracks.is_empty() {
        return;
    }
    ui.horizontal_wrapped(|ui| {
        let longest = (0..song.tracks.len())
            .filter_map(|k| s.track_seconds(n, k))
            .fold(0.0, f64::max);
        let now = if s.transport_running() {
            s.run_seconds()
        } else {
            0.0
        };
        ui.label(
            RichText::new(format!(
                "{} / {}",
                crate::setlist_ui::clock(now),
                crate::setlist_ui::clock(longest)
            ))
            .size(18.0)
            .monospace(),
        );
        for (k, track) in song.tracks.iter().enumerate() {
            let text = match s.track_state(n, k) {
                TrackState::Failed(_) => format!("{}  (cannot play)", track.label()),
                TrackState::Loading => format!("{}  ...", track.label()),
                _ => track.label(),
            };
            let b = egui::Button::new(RichText::new(text).size(14.0))
                .selected(!track.mute)
                .min_size(egui::vec2(90.0, 30.0));
            if ui
                .add(b)
                .on_hover_text(if track.cue {
                    "Cue output (headphones)"
                } else {
                    "Main mix"
                })
                .clicked()
            {
                actions.push(Action::EditTrack(
                    n,
                    k,
                    crate::session::TrackEdit::Mute(!track.mute),
                ));
            }
        }
    });
}

/// Where the setlist is: the song and its parts, with Previous / Next part and the other songs.
fn setlist(ui: &mut egui::Ui, s: &Session, actions: &mut Vec<Action>) {
    if let Some(cue) = s.cue() {
        ui.label(
            RichText::new(cue)
                .size(40.0)
                .strong()
                .color(Color32::from_rgb(0xe0, 0xb4, 0x4a)),
        );
        ui.add_space(6.0);
    }
    if s.songs().is_empty() {
        return;
    }
    let here = s.position();
    let big = |text: &str| {
        egui::Button::new(RichText::new(text).size(20.0).strong()).min_size(egui::vec2(150.0, 64.0))
    };
    ui.horizontal(|ui| {
        let prev = ui.add(big("< Prev part"));
        crate::map_mode::target(ui, s, &prev, ControlAction::PrevPart, None, actions);
        if prev.clicked() && !s.map_mode {
            actions.push(Action::PrevPart);
        }
        let next = ui.add(big("Next part >"));
        crate::map_mode::target(ui, s, &next, ControlAction::NextPart, None, actions);
        if next.clicked() && !s.map_mode {
            actions.push(Action::NextPart);
        }
        ui.add_space(10.0);
        match here.and_then(|(n, k)| Some((n, k, s.songs().get(n)?))) {
            Some((n, k, song)) => {
                ui.vertical(|ui| {
                    ui.label(RichText::new(song.label(n)).size(28.0).strong());
                    if let Some(part) = song.parts.get(k) {
                        ui.label(
                            RichText::new(format!(
                                "{}  ·  {:.0} bpm",
                                part.label(k),
                                song.tempo_in(k)
                            ))
                            .size(18.0),
                        );
                        if !part.note.trim().is_empty() {
                            ui.label(
                                RichText::new(&part.note)
                                    .size(20.0)
                                    .color(Color32::from_rgb(0xe0, 0xb4, 0x4a)),
                            );
                        }
                    }
                });
            }
            None => {
                ui.label(RichText::new("Setlist not started").size(20.0).weak());
            }
        }
    });
    if let Some((n, k)) = here
        && let Some(song) = s.songs().get(n)
        && !song.parts.is_empty()
    {
        ui.add_space(8.0);
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = egui::vec2(8.0, 8.0);
            for (i, part) in song.parts.iter().enumerate() {
                let b = egui::Button::new(RichText::new(part.label(i)).size(18.0))
                    .min_size(egui::vec2(130.0, 52.0))
                    .selected(i == k);
                if ui.add(b).clicked() {
                    actions.push(Action::GoTo(n, i));
                }
            }
        });
    }
    ui.add_space(8.0);
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = egui::vec2(6.0, 6.0);
        for (i, song) in s.songs().iter().enumerate() {
            let b = egui::Button::new(
                RichText::new(format!("{}  {}", i + 1, song.label(i))).size(15.0),
            )
            .min_size(egui::vec2(110.0, 36.0))
            .selected(here.is_some_and(|(n, _)| n == i));
            if ui.add(b).clicked() {
                actions.push(Action::GoTo(i, 0));
            }
        }
    });
    ui.add_space(14.0);
}

fn snapshots(ui: &mut egui::Ui, s: &Session, actions: &mut Vec<Action>) {
    if s.snapshots().is_empty() {
        ui.label(
            RichText::new("No scenes yet. Leave this view, set the rig up the way you want it and press \"+ Store scene\".")
                .size(16.0)
                .weak(),
        );
        return;
    }
    let current = s.current_snapshot();
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = egui::vec2(10.0, 10.0);
        for (n, snap) in s.snapshots().iter().enumerate() {
            let here = current == Some(n);
            let text = RichText::new(format!("{}\n{}", n + 1, snap.name))
                .size(20.0)
                .strong();
            let b = egui::Button::new(text)
                .min_size(egui::vec2(170.0, 76.0))
                .fill(if here {
                    Color32::from_rgb(0x1f, 0x4d, 0x7a)
                } else {
                    Color32::from_gray(38)
                })
                .stroke(if here {
                    egui::Stroke::new(2.5, Color32::from_rgb(0x6c, 0xc9, 0xe0))
                } else {
                    egui::Stroke::new(1.0, Color32::from_gray(70))
                });
            let b = ui.add(b);
            crate::map_mode::target(
                ui,
                s,
                &b,
                ControlAction::RecallSnapshot { snapshot: n as u8 },
                None,
                actions,
            );
            if b.clicked() && !s.map_mode {
                actions.push(Action::RecallSnapshot(n));
            }
        }
    });
}

fn tiles(ui: &mut egui::Ui, s: &Session, st: &UiState, actions: &mut Vec<Action>) {
    let shown: Vec<usize> = (0..UI_SLOTS)
        .filter(|&i| s.rig.slots[i].instrument.is_some())
        .collect();
    if shown.is_empty() {
        ui.label(RichText::new("No instruments loaded.").size(16.0).weak());
        return;
    }
    // Two tiles to a row.
    for pair in shown.chunks(2) {
        ui.columns(2, |cols| {
            for (col, &i) in cols.iter_mut().zip(pair) {
                tile(col, s, st, i, actions);
            }
        });
        ui.add_space(6.0);
    }
}

fn tile(ui: &mut egui::Ui, s: &Session, st: &UiState, i: usize, actions: &mut Vec<Action>) {
    let color = COLORS[i];
    let width = ui.available_width() - 4.0;
    let slot = &s.rig.slots[i];
    let loaded = s.plugins[i].is_some();
    let playing = s.targets.contains(&i) && !slot.mute;
    let name = slot.title();
    // The whole tile selects the instrument; the buttons and sliders on it keep their own clicks.
    let tile = ui.scope_builder(egui::UiBuilder::new().sense(egui::Sense::click()), |ui| {
        egui::Frame::new()
            .fill(color.gamma_multiply(if playing { 0.28 } else { 0.10 }))
            .stroke(egui::Stroke::new(
                if playing { 3.0 } else { 1.0 },
                color.gamma_multiply(if playing { 1.0 } else { 0.5 }),
            ))
            .inner_margin(egui::Margin::same(12))
            .corner_radius(6)
            .show(ui, |ui| {
                ui.set_width(width - 24.0);
                ui.horizontal(|ui| {
                    ui.label(RichText::new(format!("{}", i + 1)).size(22.0).color(color));
                    let title = RichText::new(name).size(22.0).strong();
                    let title = if loaded {
                        title
                    } else {
                        title.color(Color32::from_rgb(0xe0, 0x6c, 0x5a))
                    };
                    let r = ui.add(
                        egui::Button::new(title)
                            .frame(false)
                            .min_size(egui::vec2(0.0, 40.0)),
                    );
                    if loaded {
                        let a = ControlAction::SelectSlot { slot: i as u8 };
                        crate::map_mode::target(ui, s, &r, a, None, actions);
                    }
                    if loaded && r.clicked() && !s.map_mode {
                        actions.push(Action::Select(i, false));
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let mute = egui::Button::new(RichText::new("MUTE").size(16.0).strong())
                            .selected(slot.mute)
                            .min_size(egui::vec2(80.0, 40.0));
                        let mute = ui.add_enabled(loaded, mute);
                        let a = ControlAction::ToggleMute { slot: i as u8 };
                        crate::map_mode::target(ui, s, &mute, a, None, actions);
                        if mute.clicked() && !s.map_mode {
                            actions.push(Action::Mute(i));
                        }
                        let layer = egui::Button::new(RichText::new("+").size(18.0).strong())
                            .selected(playing)
                            .min_size(egui::vec2(48.0, 40.0));
                        if ui
                            .add_enabled(loaded, layer)
                            .on_hover_text("Play together with the others (layer)")
                            .clicked()
                        {
                            actions.push(Action::Select(i, true));
                        }
                    });
                });
                if !loaded {
                    ui.label(RichText::new("not loaded").weak());
                    return;
                }
                // Level meter and volume.
                let (meter, _) = ui.allocate_exact_size(
                    egui::vec2(ui.available_width(), 10.0),
                    egui::Sense::hover(),
                );
                ui.painter().rect_filled(meter, 2.0, Color32::from_gray(35));
                let fill = st.peaks_shown[i].clamp(0.0, 1.0).sqrt();
                if fill > 0.01 {
                    let mut bar = meter;
                    bar.set_width(meter.width() * fill);
                    ui.painter().rect_filled(bar, 2.0, color);
                }
                ui.add_space(6.0);
                let mut vol = slot.volume;
                ui.spacing_mut().slider_width = ui.available_width() - 80.0;
                let v = ui.add(
                    egui::Slider::new(&mut vol, 0.0..=1.5)
                        .fixed_decimals(2)
                        .text("vol"),
                );
                if v.changed() {
                    actions.push(Action::Volume(i, vol));
                }
                crate::map_mode::target(
                    ui,
                    s,
                    &v,
                    ControlAction::SlotVolume { slot: i as u8 },
                    None,
                    actions,
                );
                // The slot's macros that do something.
                for (k, m) in s.macros(i).iter().enumerate() {
                    if m.targets.is_empty() {
                        continue;
                    }
                    let mut v = m.value;
                    let r = ui.add(
                        egui::Slider::new(&mut v, 0.0..=1.0)
                            .fixed_decimals(2)
                            .text(m.label(k)),
                    );
                    if r.changed() {
                        actions.push(Action::SetMacro(i, k, v));
                    }
                    let a = ControlAction::SlotMacro {
                        slot: i as u8,
                        index: k as u8,
                    };
                    crate::map_mode::target(ui, s, &r, a, None, actions);
                }
            });
    });
    if loaded && !s.map_mode && tile.response.clicked() {
        actions.push(Action::Select(i, false));
    }
}

fn master(ui: &mut egui::Ui, s: &Session, actions: &mut Vec<Action>) {
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        let mut master = s.rig.master_volume;
        ui.spacing_mut().slider_width = (ui.available_width() - 420.0).max(200.0);
        let m = ui.add(
            egui::Slider::new(&mut master, 0.0..=1.5)
                .fixed_decimals(2)
                .text(RichText::new("master").size(18.0)),
        );
        if m.changed() {
            actions.push(Action::SetMaster(master));
        }
        crate::map_mode::target(ui, s, &m, ControlAction::MasterVolume, None, actions);
        let limiting = f32::from_bits(s.engine.shared.limiter_reduction.swap(0, Ordering::Relaxed));
        if limiting > 0.5 {
            ui.label(
                RichText::new(format!("LIMIT {limiting:.1} dB"))
                    .color(Color32::from_rgb(0xe0, 0x6c, 0x5a)),
            );
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let (xruns, dsp) = s
                .backend()
                .map(|b| (b.stats.xruns.load(Ordering::Relaxed), b.cpu_load()))
                .unwrap_or_default();
            let bad = xruns > 0;
            ui.label(
                RichText::new(format!("DSP {dsp:.0}% · xruns {xruns}")).color(if bad {
                    Color32::from_rgb(0xe0, 0x6c, 0x5a)
                } else {
                    Color32::GRAY
                }),
            );
        });
    });
    ui.add_space(6.0);
}
