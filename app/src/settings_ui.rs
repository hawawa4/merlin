//! The settings panel: plugins, audio, MIDI, Windows plugins, performance check.

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use egui::{Color32, RichText};

use crate::dialog::Want;
use crate::perf::{self, Status};
use crate::session::Session;
use crate::ui::{Action, UiState};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettingsTab {
    Plugins,
    Audio,
    Midi,
    Controls,
    Windows,
    Updates,
    Performance,
}

/// The kinds of action a mapping can have, for the picker.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ActionKind {
    #[default]
    SelectSlot,
    ToggleLayer,
    ToggleMute,
    NextSlot,
    PrevSlot,
    SelectByValue,
    SlotVolume,
    MasterVolume,
    SlotMacro,
    SelectedMacro,
    SelectedVolume,
    TransportToggle,
    ClickToggle,
    MetronomeToggle,
    TapTempo,
    NextPart,
    PrevPart,
    NextSong,
    PrevSong,
    RecallSnapshot,
    NextSnapshot,
    PrevSnapshot,
    RecallSnapshotByValue,
    Panic,
    CaptureMidi,
    SlotPan,
    ClickLevel,
    Tempo,
}

impl ActionKind {
    const ALL: [ActionKind; 28] = [
        Self::SelectSlot,
        Self::ToggleLayer,
        Self::ToggleMute,
        Self::NextSlot,
        Self::PrevSlot,
        Self::SelectByValue,
        Self::SlotVolume,
        Self::MasterVolume,
        Self::SlotMacro,
        Self::SelectedMacro,
        Self::SelectedVolume,
        Self::TransportToggle,
        Self::ClickToggle,
        Self::MetronomeToggle,
        Self::TapTempo,
        Self::NextPart,
        Self::PrevPart,
        Self::NextSong,
        Self::PrevSong,
        Self::RecallSnapshot,
        Self::NextSnapshot,
        Self::PrevSnapshot,
        Self::RecallSnapshotByValue,
        Self::Panic,
        Self::CaptureMidi,
        Self::SlotPan,
        Self::ClickLevel,
        Self::Tempo,
    ];

    fn name(self) -> &'static str {
        match self {
            Self::SelectSlot => "Select a slot",
            Self::ToggleLayer => "Layer a slot on or off",
            Self::ToggleMute => "Mute a slot on or off",
            Self::NextSlot => "Next slot (foot switch)",
            Self::PrevSlot => "Previous slot (foot switch)",
            Self::SelectByValue => "Select the slot given by the number sent",
            Self::SlotVolume => "Volume of a slot (fader or knob)",
            Self::MasterVolume => "Master volume (fader or knob)",
            Self::SlotMacro => "Macro of a slot (knob or fader)",
            Self::SelectedMacro => "Macro of the slot that plays (knob or fader)",
            Self::SelectedVolume => "Volume of the slot that plays (fader or knob)",
            Self::TransportToggle => "Start / stop (count-in, click, MIDI clock)",
            Self::ClickToggle => "Click on / off (during songs)",
            Self::MetronomeToggle => "Metronome on / off",
            Self::TapTempo => "Tap tempo",
            Self::NextPart => "Next part (foot switch)",
            Self::PrevPart => "Previous part (foot switch)",
            Self::NextSong => "Next song",
            Self::PrevSong => "Previous song",
            Self::RecallSnapshot => "Recall a scene",
            Self::NextSnapshot => "Next scene (foot switch)",
            Self::PrevSnapshot => "Previous scene (foot switch)",
            Self::RecallSnapshotByValue => "Recall the scene given by the number sent",
            Self::Panic => "Panic (all notes off)",
            Self::CaptureMidi => "Save the last minutes of MIDI (capture)",
            Self::SlotPan => "Pan of a slot (knob)",
            Self::ClickLevel => "Click level (knob or fader)",
            Self::Tempo => "Tempo (knob or fader)",
        }
    }

    fn needs_slot(self) -> bool {
        matches!(
            self,
            Self::SelectSlot
                | Self::ToggleLayer
                | Self::ToggleMute
                | Self::SlotVolume
                | Self::SlotMacro
                | Self::SlotPan
        )
    }

    /// The action with the numbers the user picked (slots from 1 on screen, 0-based inside).
    fn build(self, slot: usize, base: usize) -> merlin_model::ControlAction {
        use merlin_model::ControlAction as A;
        let (slot, base) = (slot.min(255) as u8, base.min(255) as u8);
        match self {
            Self::SelectSlot => A::SelectSlot { slot },
            Self::ToggleLayer => A::ToggleLayer { slot },
            Self::ToggleMute => A::ToggleMute { slot },
            Self::NextSlot => A::NextSlot,
            Self::PrevSlot => A::PrevSlot,
            Self::SelectByValue => A::SelectByValue { base },
            Self::SlotVolume => A::SlotVolume { slot },
            Self::MasterVolume => A::MasterVolume,
            Self::SlotMacro => A::SlotMacro { slot, index: base },
            Self::SelectedMacro => A::SelectedMacro { index: base },
            Self::SelectedVolume => A::SelectedVolume,
            Self::TransportToggle => A::TransportToggle,
            Self::ClickToggle => A::ClickToggle,
            Self::MetronomeToggle => A::MetronomeToggle,
            Self::TapTempo => A::TapTempo,
            Self::NextPart => A::NextPart,
            Self::PrevPart => A::PrevPart,
            Self::NextSong => A::NextSong,
            Self::PrevSong => A::PrevSong,
            Self::RecallSnapshot => A::RecallSnapshot { snapshot: slot },
            Self::NextSnapshot => A::NextSnapshot,
            Self::PrevSnapshot => A::PrevSnapshot,
            Self::RecallSnapshotByValue => A::RecallSnapshotByValue { base },
            Self::Panic => A::Panic,
            Self::CaptureMidi => A::CaptureMidi,
            Self::SlotPan => A::SlotPan { slot },
            Self::ClickLevel => A::ClickLevel,
            Self::Tempo => A::Tempo,
        }
    }
}

fn status_color(s: Status) -> Color32 {
    match s {
        Status::Ok => Color32::from_rgb(0x6c, 0xc9, 0x6a),
        Status::Warn => Color32::from_rgb(0xe0, 0xb4, 0x4a),
        Status::Fail => Color32::from_rgb(0xe0, 0x6c, 0x5a),
    }
}

pub fn show(ui: &mut egui::Ui, s: &Session, st: &mut UiState, actions: &mut Vec<Action>) {
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.heading("Settings");
        ui.separator();
        for (tab, name) in [
            (SettingsTab::Plugins, "Plugins"),
            (SettingsTab::Audio, "Audio"),
            (SettingsTab::Midi, "MIDI devices"),
            (SettingsTab::Controls, "Controls"),
            (SettingsTab::Windows, "Windows plugins"),
            (SettingsTab::Updates, "Updates"),
            (SettingsTab::Performance, "Performance check"),
        ] {
            ui.selectable_value(&mut st.settings_tab, tab, name);
        }
    });
    ui.separator();
    egui::ScrollArea::vertical()
        .auto_shrink(false)
        .show(ui, |ui| match st.settings_tab {
            SettingsTab::Plugins => plugins(ui, s, st, actions),
            SettingsTab::Audio => audio(ui, s, st, actions),
            SettingsTab::Midi => midi(ui, s, st, actions),
            SettingsTab::Controls => controls(ui, s, st, actions),
            SettingsTab::Windows => windows(ui, s, st, actions),
            SettingsTab::Updates => updates(ui, s, actions),
            SettingsTab::Performance => performance(ui, s, st),
        });
}

fn plugins(ui: &mut egui::Ui, s: &Session, st: &mut UiState, actions: &mut Vec<Action>) {
    ui.strong("Plugin folders");
    ui.label(
        RichText::new(
            "Add any folder with plugins: CLAP, VST3, VST2, LV2, Linux or Windows, mixed or not, with subfolders. Merlin works out what each file is. Windows plugins need Windows plugin support (the Windows plugins tab).",
        )
        .small()
        .weak(),
    );
    for (i, d) in s.settings.plugin_dirs.iter().enumerate() {
        ui.horizontal(|ui| {
            ui.label(d.display().to_string());
            if ui.small_button("remove").clicked() {
                actions.push(Action::RemoveDir(i));
            }
        });
    }
    crate::ui::pick_button(ui, &mut st.picking, "Add folder…", Want::PluginDir, None);
    egui::CollapsingHeader::new("Standard folders, always searched")
        .default_open(false)
        .show(ui, |ui| {
            for d in merlin_scanner::default_plugin_dirs() {
                ui.label(RichText::new(d.display().to_string()).small().weak());
            }
        });
    if !s.windows_status.is_empty() {
        ui.label(RichText::new(&s.windows_status).small().weak());
    }
    ui.add_space(12.0);
    sound_folders(ui, s, st, actions);
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        ui.add_enabled_ui(!s.scanning(), |ui| {
            if ui.button("Rescan changed files").clicked() {
                actions.push(Action::Rescan(false));
            }
            if ui.button("Rescan everything").clicked() {
                actions.push(Action::Rescan(true));
            }
        });
        if s.scanning() {
            let file = s
                .scan_current
                .as_ref()
                .and_then(|p| p.file_name())
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            ui.label(format!("scanning… {file}"));
        } else {
            ui.label(format!(
                "{} plugins in {} files",
                s.found.len(),
                s.entries.len()
            ));
        }
    });

    let failed: Vec<_> = s.entries.iter().filter(|e| e.error.is_some()).collect();
    if !failed.is_empty() {
        ui.add_space(8.0);
        ui.strong(format!(
            "Files that could not be scanned ({})",
            failed.len()
        ));
        ui.label(
            RichText::new(
                "They are tried again when the file changes, or with Retry. Ignore a file you know is broken or not a plugin: it is never loaded or listed here again.",
            )
            .small()
            .weak(),
        );
        for e in failed {
            ui.horizontal_wrapped(|ui| {
                if ui.small_button("Retry").clicked() {
                    actions.push(Action::RetryFile(e.path.clone()));
                }
                if ui
                    .small_button("Ignore")
                    .on_hover_text("Never scan this file again, and stop reporting it")
                    .clicked()
                {
                    actions.push(Action::IgnoreFile(e.path.clone()));
                }
                ui.colored_label(status_color(Status::Fail), e.path.display().to_string());
                ui.label(e.error.as_deref().unwrap_or_default());
            });
        }
    }

    if !s.settings.ignored_files.is_empty() {
        ui.add_space(8.0);
        ui.strong("Ignored files");
        for p in &s.settings.ignored_files {
            ui.horizontal_wrapped(|ui| {
                if ui.small_button("Stop ignoring").clicked() {
                    actions.push(Action::UnignoreFile(p.clone()));
                }
                ui.label(p.display().to_string());
            });
        }
    }

    if !s.hidden.is_empty() {
        ui.add_space(8.0);
        ui.strong("Hidden plugins");
        ui.label(
            RichText::new(
                "Hidden from the library with right-click > Hide. Rigs that use them still load.",
            )
            .small()
            .weak(),
        );
        for f in &s.hidden {
            ui.horizontal_wrapped(|ui| {
                if ui.small_button("Show again").clicked() {
                    actions.push(Action::UnhidePlugin(Session::favorite_key(f)));
                }
                ui.label(format!("{} ({})", f.info.name, f.info.vendor));
            });
        }
    }

    let quarantined = s.quarantine.entries();
    if !quarantined.is_empty() {
        ui.add_space(8.0);
        ui.strong("Quarantined plugins");
        ui.label(
            RichText::new("These crashed the app. They are not loaded automatically.")
                .small()
                .weak(),
        );
        for q in quarantined {
            ui.horizontal_wrapped(|ui| {
                ui.colored_label(status_color(Status::Warn), &q.name);
                ui.label(&q.reason);
                if ui.small_button("Remove from quarantine").clicked() {
                    actions.push(Action::Unquarantine(q.path.clone(), q.id.clone()));
                }
            });
        }
    }
}

fn audio(ui: &mut egui::Ui, s: &Session, st: &mut UiState, actions: &mut Vec<Action>) {
    let Some(b) = s.backend() else {
        ui.label("Audio is not running.");
        return;
    };
    let (frames, rate) = (b.buffer_size(), s.sample_rate);
    ui.strong("Buffer size");
    ui.horizontal(|ui| {
        for n in [64u32, 128, 256, 512, 1024] {
            if ui.selectable_label(frames == n, format!("{n}")).clicked() && frames != n {
                actions.push(Action::SetBuffer(n));
            }
        }
        ui.label("frames");
    });
    let one_way = frames as f32 * 1000.0 / rate as f32;
    ui.label(format!(
        "{frames} frames at {rate} Hz: {one_way:.1} ms per buffer, about {:.1} ms round trip.",
        one_way * 2.0
    ));
    ui.label(
        RichText::new("Smaller is more responsive but needs a faster, well-set-up machine (see the performance check).")
            .small()
            .weak(),
    );
    ui.add_space(8.0);
    ui.strong("Now");
    ui.label(format!(
        "DSP load {:.1}%   dropouts {}   slowest callback {:.0} µs",
        b.cpu_load(),
        b.stats.xruns.load(Ordering::Relaxed),
        b.stats.max_ns.load(Ordering::Relaxed) as f64 / 1000.0,
    ));
    ui.label(format!("Outputs: {}", b.out_names.join(", ")));
    ui.add_space(8.0);
    click_output(ui, s, actions);
    ui.add_space(8.0);
    backing_folder(ui, s, st, actions);
}

/// The folder the Setlist view lists backing tracks from.
fn backing_folder(ui: &mut egui::Ui, s: &Session, st: &mut UiState, actions: &mut Vec<Action>) {
    ui.strong("Backing tracks folder");
    ui.label(
        RichText::new("The Setlist view shows this folder (and the folders inside it) beside the songs, so a backing track is a double-click away. Only audio files are listed.")
            .small()
            .weak(),
    );
    ui.horizontal(|ui| {
        match s.backing_dir() {
            Some(d) => ui.label(RichText::new(d.display().to_string()).monospace()),
            None => ui.label(RichText::new("not chosen yet").weak()),
        };
        crate::ui::pick_button(
            ui,
            &mut st.picking,
            "Choose…",
            Want::BackingDir,
            s.backing_dir(),
        );
        if s.backing_dir().is_some() && ui.button("Clear").clicked() {
            actions.push(Action::SetBackingDir(None));
        }
    });
}

/// Where the click is heard.
fn click_output(ui: &mut egui::Ui, s: &Session, actions: &mut Vec<Action>) {
    ui.strong("Click");
    ui.horizontal(|ui| {
        ui.label("beats per bar when no song sets it");
        let mut beats = s.rig.transport.beats_per_bar;
        if ui
            .add(egui::DragValue::new(&mut beats).range(1..=16))
            .changed()
        {
            actions.push(Action::SetBeatsPerBar(beats));
        }
    });
    ui.strong("Click output");
    ui.label(
        RichText::new("The metronome. Mixed into the main outputs by default; pick two of your interface's own outputs to hear it in headphones only. The MIDI clock, and the program changes and controllers that song markers send, go out of the \"midi_out\" port (connect it in your patchbay).")
            .small()
            .weak(),
    );
    let ports = s.playback_ports();
    let current = s.settings.click_output.clone();
    let text = match &current {
        Some([l, r]) => format!("{l} + {r}"),
        None => "Main outputs (mixed in)".to_owned(),
    };
    egui::ComboBox::from_id_salt("click-output")
        .selected_text(text)
        .width(420.0)
        .show_ui(ui, |ui| {
            if ui
                .selectable_label(current.is_none(), "Main outputs (mixed in)")
                .clicked()
            {
                actions.push(Action::SetClickOutput(None));
            }
            // Left and right of a pair are neighbouring ports.
            for pair in ports.chunks(2).filter(|p| p.len() == 2) {
                let label = format!("{} + {}", pair[0], pair[1]);
                let chosen = current
                    .as_ref()
                    .is_some_and(|c| c[0] == pair[0] && c[1] == pair[1]);
                if ui.selectable_label(chosen, label).clicked() {
                    actions.push(Action::SetClickOutput(Some([
                        pair[0].clone(),
                        pair[1].clone(),
                    ])));
                }
            }
        });
}

/// Where SoundFonts are looked for, with how many each folder holds.
fn sound_folders(ui: &mut egui::Ui, s: &Session, st: &mut UiState, actions: &mut Vec<Action>) {
    use merlin_model::Format;
    let count = |dir: &std::path::Path| {
        // Files, not browser entries: a folder of SoundFonts is one entry.
        s.entries
            .iter()
            .filter(|e| {
                e.path.starts_with(dir) && e.plugins.iter().any(|p| p.format == Format::SoundFont)
            })
            .count()
    };
    ui.strong("Sound folders (SoundFonts)");
    ui.label(
        RichText::new("Every .sf2 and .sf3 in these folders appears in the browser as an instrument. A folder with several SoundFonts directly inside it (other than the standard folders below) shows up as one instrument named like the folder, with all its sounds in one list. You can also drop a SoundFont file on the window.")
            .small()
            .weak(),
    );
    for d in merlin_scanner::default_sound_dirs() {
        ui.label(
            RichText::new(format!("{}  ({} found)", d.display(), count(&d)))
                .small()
                .weak(),
        );
    }
    ui.add_space(4.0);
    for (i, d) in s.settings.sound_dirs.iter().enumerate() {
        ui.horizontal(|ui| {
            ui.label(format!("{}  ({} found)", d.display(), count(d)));
            if ui.small_button("remove").clicked() {
                actions.push(Action::RemoveSoundDir(i));
            }
        });
    }
    crate::ui::pick_button(ui, &mut st.picking, "Add folder…", Want::SoundDir, None);
}

/// What the MIDI controls do: the list of mappings, and teaching a new one by moving a control.
fn controls(ui: &mut egui::Ui, s: &Session, st: &mut UiState, actions: &mut Vec<Action>) {
    use merlin_model::Takeover;
    ui.strong("Controls");
    ui.label(
        RichText::new(
            "Teach the app what your pedals, pads, knobs and faders do: pick an action, press Learn, then press or move the control. Mappings belong to the rig and are saved with it. A control that is mapped no longer plays an instrument.",
        )
        .small()
        .weak(),
    );
    ui.add_space(6.0);

    // Composing a new mapping.
    ui.horizontal(|ui| {
        egui::ComboBox::from_id_salt("new-mapping-kind")
            .width(300.0)
            .selected_text(st.new_kind.name())
            .show_ui(ui, |ui| {
                for k in ActionKind::ALL {
                    ui.selectable_value(&mut st.new_kind, k, k.name());
                }
            });
        if st.new_kind.needs_slot() {
            ui.label("slot");
            let mut shown = st.new_slot + 1;
            ui.add(egui::DragValue::new(&mut shown).range(1..=crate::session::UI_SLOTS));
            st.new_slot = shown - 1;
        }
        if st.new_kind == ActionKind::SelectByValue {
            ui.label("first slot");
            let mut shown = st.new_base + 1;
            ui.add(egui::DragValue::new(&mut shown).range(1..=crate::session::UI_SLOTS));
            st.new_base = shown - 1;
        }
        if matches!(
            st.new_kind,
            ActionKind::SlotMacro | ActionKind::SelectedMacro
        ) {
            ui.label("macro");
            let mut shown = st.new_base + 1;
            ui.add(egui::DragValue::new(&mut shown).range(1..=merlin_model::MACROS_PER_SLOT));
            st.new_base = shown - 1;
        }
        if st.new_kind == ActionKind::RecallSnapshot {
            ui.label("scene");
            let mut shown = st.new_slot + 1;
            ui.add(egui::DragValue::new(&mut shown).range(1..=merlin_model::MAX_SNAPSHOTS));
            st.new_slot = shown - 1;
        }
        if st.new_kind == ActionKind::RecallSnapshotByValue {
            ui.label("first scene");
            let mut shown = st.new_base + 1;
            ui.add(egui::DragValue::new(&mut shown).range(1..=merlin_model::MAX_SNAPSHOTS));
            st.new_base = shown - 1;
        }
        match s.learning() {
            Some(_) => {
                ui.colored_label(
                    status_color(Status::Warn),
                    "Press or move a control on your device…",
                );
                if ui.button("Cancel").clicked() {
                    actions.push(Action::CancelLearn);
                }
            }
            None => {
                if ui.button("Learn…").clicked() {
                    actions.push(Action::LearnControl(
                        st.new_kind.build(st.new_slot, st.new_base),
                    ));
                }
            }
        }
    });
    ui.add_space(2.0);
    ui.label(
        RichText::new(match s.last_control() {
            Some(m) => format!("Last message received: {m}"),
            None => "No control messages yet: press something on your device.".to_owned(),
        })
        .small()
        .weak(),
    );
    ui.add_space(8.0);

    // What connected controllers do without any mapping of yours.
    let built_in = s.profile_mappings();
    if !built_in.is_empty() {
        egui::CollapsingHeader::new(format!(
            "Built-in layout of connected controllers ({})",
            built_in.len()
        ))
        .show(ui, |ui| {
            ui.label(
                RichText::new(
                    "These work until you map the same control yourself (in the list below or with MIDI map): yours then wins and the row is crossed out.",
                )
                .small()
                .weak(),
            );
            egui::Grid::new("profile-mappings")
                .num_columns(3)
                .spacing([14.0, 2.0])
                .show(ui, |ui| {
                    for (m, active) in &built_in {
                        let text = |t: String| {
                            let t = RichText::new(t);
                            if *active { t } else { t.strikethrough().weak() }
                        };
                        ui.label(text(m.label.clone().unwrap_or_default()));
                        ui.label(text(m.source.describe()));
                        ui.label(text(format!("-> {}", m.action.describe())));
                        ui.end_row();
                    }
                });
        });
        ui.add_space(8.0);
    }

    // The list.
    if s.mappings().is_empty() {
        ui.label("No mappings yet.");
    }
    egui::Grid::new("mappings")
        .num_columns(5)
        .spacing([14.0, 6.0])
        .show(ui, |ui| {
            for (i, m) in s.mappings().iter().enumerate() {
                ui.label(RichText::new(m.source.describe()).strong());
                ui.label(match &m.label {
                    Some(name) => format!("-> {} ({name})", m.action.describe()),
                    None => format!("-> {}", m.action.describe()),
                });
                if m.action.is_continuous() {
                    let mut t = m.takeover;
                    egui::ComboBox::from_id_salt(("takeover", i))
                        .selected_text(match t {
                            Takeover::Jump => "Jump",
                            Takeover::Pickup => "Pick up",
                            Takeover::Scale => "Scale",
                        })
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut t, Takeover::Jump, "Jump")
                                .on_hover_text("The value jumps to wherever the knob is");
                            ui.selectable_value(&mut t, Takeover::Scale, "Scale")
                                .on_hover_text("The value moves with the knob, scaled so both reach the end of their range together: no jump and no dead stretch");
                            ui.selectable_value(&mut t, Takeover::Pickup, "Pick up")
                                .on_hover_text("The knob does nothing until it reaches the current value, so it never makes a jump");
                        });
                    if t != m.takeover {
                        actions.push(Action::SetTakeover(i, t));
                    }
                    glue_options(ui, i, m, actions);
                } else if m.action.is_switch() {
                    use merlin_model::Switch;
                    let mut sw = m.switch;
                    egui::ComboBox::from_id_salt(("switch", i))
                        .selected_text(match sw {
                            Switch::Press => "Each press flips",
                            Switch::Level => "Latching switch",
                        })
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut sw, Switch::Press, "Each press flips")
                                .on_hover_text("A pad or a momentary foot switch: every press toggles it, the release does nothing");
                            ui.selectable_value(&mut sw, Switch::Level, "Latching switch")
                                .on_hover_text("A switch that stays down (sends 127, later 0): on when it sends 64 or more, off below, whatever the state was");
                        });
                    if sw != m.switch {
                        actions.push(Action::SetSwitch(i, sw));
                    }
                    ui.label("");
                } else {
                    ui.label("");
                    ui.label("");
                }
                if ui.small_button("remove").clicked() {
                    actions.push(Action::RemoveMapping(i));
                }
                ui.end_row();
            }
        });
}

/// Range, direction, curve and encoder settings of a knob mapping.
fn glue_options(
    ui: &mut egui::Ui,
    index: usize,
    m: &merlin_model::Mapping,
    actions: &mut Vec<Action>,
) {
    use merlin_model::{Curve, Relative};
    ui.menu_button("options", |ui| {
        let mut g = m.glue;
        ui.checkbox(&mut g.invert, "Invert (turn up = lower)");
        ui.horizontal(|ui| {
            ui.label("range");
            let (mut lo, mut hi) = (g.min * 100.0, g.max * 100.0);
            ui.add(egui::DragValue::new(&mut lo).range(0.0..=100.0).suffix("%"));
            ui.label("to");
            ui.add(egui::DragValue::new(&mut hi).range(0.0..=100.0).suffix("%"));
            (g.min, g.max) = (lo / 100.0, hi / 100.0);
        });
        egui::ComboBox::from_label("response")
            .selected_text(match g.curve {
                Curve::Linear => "Linear",
                Curve::Soft => "Soft (fine at the bottom)",
                Curve::Hard => "Hard (fine at the top)",
            })
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut g.curve, Curve::Linear, "Linear");
                ui.selectable_value(&mut g.curve, Curve::Soft, "Soft (fine at the bottom)");
                ui.selectable_value(&mut g.curve, Curve::Hard, "Hard (fine at the top)");
            });
        egui::ComboBox::from_label("kind")
            .selected_text(match g.relative {
                Relative::Off => "Knob or fader",
                Relative::TwosComplement => "Endless encoder (two's complement)",
                Relative::SignedBit => "Endless encoder (sign bit)",
                Relative::BinaryOffset => "Endless encoder (offset 64)",
            })
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut g.relative, Relative::Off, "Knob or fader");
                ui.selectable_value(
                    &mut g.relative,
                    Relative::TwosComplement,
                    "Endless encoder (two's complement)",
                )
                .on_hover_text("1..63 up, 127..65 down: most encoders");
                ui.selectable_value(
                    &mut g.relative,
                    Relative::SignedBit,
                    "Endless encoder (sign bit)",
                )
                .on_hover_text("1..63 up, 65..127 down");
                ui.selectable_value(
                    &mut g.relative,
                    Relative::BinaryOffset,
                    "Endless encoder (offset 64)",
                )
                .on_hover_text("64 is still, above is up, below is down");
            });
        if g.relative != Relative::Off {
            ui.horizontal(|ui| {
                ui.label("step");
                let mut pct = g.step * 100.0;
                ui.add(
                    egui::DragValue::new(&mut pct)
                        .range(0.1..=100.0)
                        .speed(0.1)
                        .suffix("%"),
                );
                g.step = pct / 100.0;
            });
        }
        if g != m.glue {
            actions.push(Action::SetGlue(index, g));
        }
    });
}

fn midi(ui: &mut egui::Ui, s: &Session, st: &mut UiState, actions: &mut Vec<Action>) {
    use crate::settings::DeviceRole;
    ui.strong("MIDI devices");
    ui.label(
        RichText::new(
            "A keyboard plays the instruments. A controller (pads, knobs, faders) never does: its messages are kept for controlling the app. Turn a device off to leave it alone completely. Devices are remembered by name, so plugging one in again restores its role.",
        )
        .small()
        .weak(),
    );
    ui.add_space(4.0);
    let devices = s.midi_devices();
    if devices.is_empty() {
        ui.label("No MIDI devices found.");
    }
    egui::Grid::new("midi-devices")
        .num_columns(5)
        .spacing([14.0, 6.0])
        .show(ui, |ui| {
            for d in &devices {
                let (rect, _) =
                    ui.allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::hover());
                let led = if d.active {
                    status_color(Status::Ok)
                } else if d.input.is_some() {
                    Color32::from_gray(90)
                } else {
                    Color32::from_gray(45)
                };
                ui.painter().circle_filled(rect.center(), 5.0, led);
                ui.label(RichText::new(&d.name).strong());
                ui.label(match d.input {
                    Some(n) => format!("input {n}"),
                    None if !d.enabled => "off".to_owned(),
                    None => "not connected (all inputs in use)".to_owned(),
                });
                let mut role = d.role;
                egui::ComboBox::from_id_salt(("midi-role", &d.name))
                    .selected_text(match role {
                        DeviceRole::Keyboard => "Keyboard",
                        DeviceRole::Controller => "Controller",
                    })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut role, DeviceRole::Keyboard, "Keyboard");
                        ui.selectable_value(&mut role, DeviceRole::Controller, "Controller");
                    });
                let mut on = d.enabled;
                ui.checkbox(&mut on, "use");
                if role != d.role || on != d.enabled {
                    actions.push(Action::SetMidiDevice(d.name.clone(), role, on));
                }
                ui.end_row();
            }
        });
    for (_, what) in s.surface_devices() {
        ui.colored_label(
            status_color(Status::Ok),
            format!("{what}: built-in layout, lights and screen are on (see Controls)"),
        );
    }
    ui.add_space(6.0);
    if ui.button("Look for new devices").clicked() {
        actions.push(Action::RefreshMidi);
    }
    ui.label(
        RichText::new("New devices are looked for automatically when they are plugged in.")
            .small()
            .weak(),
    );
    ui.add_space(12.0);
    capture(ui, s, st, actions);
}

/// The MIDI capture: how much is kept, where it goes, and a button to save it now.
fn capture(ui: &mut egui::Ui, s: &Session, st: &mut UiState, actions: &mut Vec<Action>) {
    ui.strong("MIDI capture");
    ui.label(
        RichText::new(
            "Merlin always keeps the last minutes of what you play from keyboards and the on-screen keys (not from controllers, and not what is mapped to an action). The \"Capture MIDI\" button, or a MIDI control mapped to it, saves them as a .mid file with a track per keyboard. Nothing is written to disk until then.",
        )
        .small()
        .weak(),
    );
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        let mut minutes = s.capture_minutes();
        ui.label("Keep the last");
        let changed = ui
            .add(
                egui::DragValue::new(&mut minutes)
                    .range(0..=crate::session::MAX_CAPTURE_MINUTES)
                    .suffix(" min"),
            )
            .changed();
        if changed {
            actions.push(Action::SetCaptureMinutes(minutes));
        }
        ui.label(
            RichText::new(if minutes == 0 { "(off)" } else { "" })
                .small()
                .weak(),
        );
    });
    let (listened, kept) = s.capture_status();
    if s.capture_minutes() > 0 {
        ui.label(format!(
            "Listening for {} · {kept} messages kept",
            crate::session::clock(listened)
        ));
    }
    ui.horizontal(|ui| {
        ui.label("Save to");
        ui.label(RichText::new(s.capture_dir().display().to_string()).monospace());
        crate::ui::pick_button(
            ui,
            &mut st.picking,
            "Choose…",
            Want::CaptureDir,
            Some(&s.capture_dir()),
        );
        if ui.button("Default").clicked() {
            actions.push(Action::SetCaptureDir(None));
        }
    });
    if ui
        .add_enabled(s.capture_minutes() > 0, egui::Button::new("Save now"))
        .clicked()
    {
        actions.push(Action::CaptureMidi);
    }
}

fn windows(ui: &mut egui::Ui, s: &Session, st: &mut UiState, actions: &mut Vec<Action>) {
    windows_support(ui, s, actions);
    ui.add_space(8.0);
    egui::CollapsingHeader::new("Advanced: use my own Wine and yabridge")
        .default_open(false)
        .show(ui, |ui| own_wine(ui, st, actions));
    ui.add_space(12.0);
    windows_plugins(ui, s, st, actions);
    ui.add_space(8.0);
    ui.strong("In use right now");
    for var in ["WINELOADER", "PATH"] {
        let value = std::env::var(var).unwrap_or_default();
        let shown = if var == "PATH" {
            value
                .split(':')
                .filter(|p| p.contains("yabridge"))
                .collect::<Vec<_>>()
                .join(", ")
        } else {
            value
        };
        ui.label(format!(
            "{var}: {}",
            if shown.is_empty() {
                "(not set)"
            } else {
                &shown
            }
        ));
    }
}

/// "Windows plugin support": the Wine + yabridge pair the app downloads and keeps itself.
fn windows_support(ui: &mut egui::Ui, s: &Session, actions: &mut Vec<Action>) {
    ui.strong("Windows plugin support");
    match (s.installed_runtime(), s.windows_runtime()) {
        (Some(r), _) => {
            ui.horizontal(|ui| {
                ui.colored_label(status_color(Status::Ok), "installed");
                ui.label(&r.id);
            });
        }
        (None, Some(_)) => {
            ui.label("Using a Wine and yabridge that were already set up on this machine.");
        }
        (None, None) => {
            ui.label(
                RichText::new(
                    "Windows plugins need Wine and yabridge. The app downloads a tested pair (about 40 MB) and keeps it in its own folder; your system Wine is never touched.",
                )
                .small()
                .weak(),
            );
            let busy = s.windows_busy();
            if ui
                .add_enabled(!busy, egui::Button::new("Download Windows plugin support"))
                .clicked()
            {
                actions.push(Action::InstallRuntime);
            }
            if !s.windows_status.is_empty() {
                ui.label(&s.windows_status);
            }
        }
    }
}

/// The paths of a Wine and a yabridge the user set up themselves.
fn own_wine(ui: &mut egui::Ui, st: &mut UiState, actions: &mut Vec<Action>) {
    ui.label(
        RichText::new("Overrides the downloaded pair. Changes apply after restarting the app.")
            .small()
            .weak(),
    );
    ui.add_space(6.0);
    let picking = &mut st.picking;
    for (label, text, browse) in [
        ("Wine program", &mut st.wine_text, &Want::WineProgram),
        ("yabridge folder", &mut st.yab_text, &Want::YabridgeDir),
    ] {
        ui.horizontal(|ui| {
            ui.label(label);
            ui.add(egui::TextEdit::singleline(text).desired_width(460.0));
            let path = std::path::Path::new(text.as_str());
            let start = if *browse == Want::WineProgram {
                path.parent()
            } else {
                Some(path)
            };
            crate::ui::pick_button(ui, picking, "Browse…", *browse, start);
            if text.is_empty() {
                ui.label(RichText::new("not set").weak());
            } else if path.exists() {
                ui.colored_label(status_color(Status::Ok), "found");
            } else {
                ui.colored_label(status_color(Status::Fail), "does not exist");
            }
        });
    }
    if ui.button("Save").clicked() {
        actions.push(Action::SaveWine(st.wine_text.clone(), st.yab_text.clone()));
    }
}

/// Installing Windows plugins and what the Wine setup looks like (a small "Wine doctor").
fn windows_plugins(ui: &mut egui::Ui, s: &Session, st: &mut UiState, actions: &mut Vec<Action>) {
    ui.strong("Windows plugins");
    let Some(rt) = s.windows_runtime() else {
        ui.label(
            RichText::new("Download Windows plugin support above to use Windows plugins.")
                .small()
                .weak(),
        );
        return;
    };
    let check = |ui: &mut egui::Ui, ok: bool, text: String| {
        ui.horizontal(|ui| {
            ui.colored_label(
                status_color(if ok { Status::Ok } else { Status::Fail }),
                if ok { "OK  " } else { "FAIL" },
            );
            ui.label(text);
        });
    };
    check(
        ui,
        rt.version().is_some(),
        format!(
            "Wine: {}",
            rt.version().unwrap_or_else(|| "does not run".into())
        ),
    );
    check(
        ui,
        rt.chainloader().is_file()
            && rt.vst3_chainloader().is_file()
            && rt.vst2_chainloader().is_file(),
        "yabridge: CLAP, VST3 and VST2 chainloaders".to_owned(),
    );
    ui.horizontal(|ui| {
        let ready = rt.prefix_ready();
        ui.colored_label(
            status_color(if ready { Status::Ok } else { Status::Warn }),
            if ready { "OK  " } else { "WAIT" },
        );
        ui.label(format!(
            "Windows environment: {}{}",
            rt.prefix.display(),
            if ready {
                ""
            } else {
                " (made when you install the first plugin, or open it here)"
            }
        ));
    });
    ui.add_space(6.0);
    ui.add_space(6.0);
    egui::Frame::group(ui.style()).show(ui, |ui| {
        ui.set_width(ui.available_width().min(620.0));
        ui.vertical_centered(|ui| {
            ui.add_space(6.0);
            ui.strong("Drop a Windows plugin installer (.exe or .msi) anywhere on this window");
            ui.label(
                RichText::new("It runs in the app's own Windows environment, and the plugins it installs show up in the browser when it finishes.")
                    .small()
                    .weak(),
            );
            ui.add_space(6.0);
        });
    });
    ui.horizontal(|ui| {
        let busy = s.windows_busy();
        if ui
            .add_enabled(!busy, egui::Button::new("Look for installed plugins"))
            .on_hover_text(
                "Use this after installing a plugin by hand into the Windows environment",
            )
            .clicked()
        {
            actions.push(Action::SyncWindows);
        }
        if ui
            .add_enabled(!s.windows_busy(), egui::Button::new("Open Wine prefix"))
            .on_hover_text(
                "Show the Windows environment's folder in your file manager. It is made first when it does not exist yet.",
            )
            .clicked()
        {
            actions.push(Action::OpenWinePrefix);
        }
    });
    egui::CollapsingHeader::new("Install from a path instead")
        .default_open(false)
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut st.installer_text)
                        .hint_text("/path/to/Plugin-Setup.exe")
                        .desired_width(460.0),
                );
                crate::ui::pick_button(ui, &mut st.picking, "Browse…", Want::Installer, None);
                if ui
                    .add_enabled(
                        !s.windows_busy() && !st.installer_text.trim().is_empty(),
                        egui::Button::new("Install"),
                    )
                    .clicked()
                {
                    actions.push(Action::InstallWindows(st.installer_text.clone()));
                }
            });
        });
    if !s.windows_status.is_empty() {
        ui.label(&s.windows_status);
    }
    ui.add_space(4.0);
    ui.label(
        RichText::new(
            "Windows plugins in your folders (Plugins tab) are found and bridged by themselves. Only 64-bit CLAP, VST3 and VST2 plugins are picked up. Plugins with copy protection (iLok and the like) may not run. Prefer a native Linux version when there is one.",
        )
        .small()
        .weak(),
    );
}

/// Looking for a newer release and installing it. Nothing here runs unless the user presses a
/// button: there is no check at startup and no popup.
fn updates(ui: &mut egui::Ui, s: &Session, actions: &mut Vec<Action>) {
    use crate::session::{APP_VERSION, UpdateBusy};
    use merlin_updater::Kind;

    let u = &s.update;
    ui.strong("Updates");
    ui.label(
        RichText::new(
            "Merlin only looks for a new version when you ask here. It never checks in the background or at startup.",
        )
        .small()
        .weak(),
    );
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        ui.label(format!("Version {APP_VERSION}"));
        let checking = u.busy == UpdateBusy::Checking;
        if ui
            .add_enabled(
                u.busy == UpdateBusy::No,
                egui::Button::new(if checking {
                    "Checking..."
                } else {
                    "Check for updates"
                }),
            )
            .clicked()
        {
            actions.push(Action::CheckUpdate);
        }
    });
    if !u.error.is_empty() {
        ui.colored_label(status_color(Status::Fail), &u.error);
    }
    let Some(info) = &u.info else { return };
    ui.add_space(6.0);

    if u.installed {
        ui.colored_label(
            status_color(Status::Ok),
            format!(
                "Version {} is installed. Restart Merlin to start using it.",
                info.latest
            ),
        );
        ui.label(
            RichText::new("Your rig is saved and comes back as it was.")
                .small()
                .weak(),
        );
        if ui.button("Restart now").clicked() {
            actions.push(Action::RestartForUpdate);
        }
        return;
    }
    if !info.available {
        ui.colored_label(status_color(Status::Ok), "You're up to date.");
        return;
    }

    ui.colored_label(
        status_color(Status::Warn),
        format!("A new version is available: {}", info.latest),
    );
    if u.busy == UpdateBusy::Installing {
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label(&u.status);
        });
        if info.kind == Kind::Deb {
            ui.label(
                RichText::new("A password prompt from your desktop asks to install the package.")
                    .small()
                    .weak(),
            );
        }
    } else if info.can_install {
        ui.label(
            RichText::new(match info.kind {
                Kind::Deb => "Installs the package; your desktop asks for your password.",
                Kind::AppImage => "Replaces this AppImage file; the running app keeps going until you restart.",
                _ => "Replaces the Merlin programs in this folder; the running app keeps going until you restart.",
            })
            .small()
            .weak(),
        );
        if ui.button("Download and install").clicked() {
            actions.push(Action::InstallUpdate);
        }
    } else if !info.reason.is_empty() {
        ui.label(&info.reason);
    }
    if ui
        .add_enabled(
            u.busy == UpdateBusy::No,
            egui::Button::new("Open release page"),
        )
        .clicked()
    {
        actions.push(Action::OpenReleasePage);
    }
    if !info.notes.trim().is_empty() {
        egui::CollapsingHeader::new("What's new")
            .default_open(false)
            .show(ui, |ui| {
                ui.label(RichText::new(info.notes.trim()).small());
            });
    }
}

fn performance(ui: &mut egui::Ui, s: &Session, st: &mut UiState) {
    let stale = st
        .perf
        .as_ref()
        .is_none_or(|(t, _)| t.elapsed() > Duration::from_secs(2));
    if stale && let Some(b) = s.backend() {
        let inputs = perf::gather(
            b.buffer_size(),
            s.sample_rate,
            b.stats.xruns.load(Ordering::Relaxed),
            b.cpu_load(),
        );
        st.perf = Some((Instant::now(), perf::evaluate(&inputs)));
    }
    let Some((_, checks)) = &st.perf else {
        ui.label("Audio is not running.");
        return;
    };
    let overall = perf::overall(checks);
    ui.horizontal(|ui| {
        ui.heading("Performance check");
        ui.colored_label(
            status_color(overall),
            match overall {
                Status::Ok => "ready for live playing",
                Status::Warn => "usable, with warnings",
                Status::Fail => "needs attention",
            },
        );
    });
    ui.add_space(6.0);
    for c in checks {
        ui.horizontal(|ui| {
            ui.colored_label(
                status_color(c.status),
                match c.status {
                    Status::Ok => "OK  ",
                    Status::Warn => "WARN",
                    Status::Fail => "FAIL",
                },
            );
            ui.strong(c.name);
            ui.label(&c.detail);
        });
        if let Some(fix) = &c.fix {
            ui.horizontal(|ui| {
                ui.add_space(52.0);
                ui.label(RichText::new(fix).color(Color32::from_gray(190)));
            });
        }
        ui.add_space(2.0);
    }
}
