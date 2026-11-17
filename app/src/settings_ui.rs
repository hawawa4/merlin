//! The settings panel: plugins, audio, MIDI, Windows plugins, performance check.

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use egui::{Color32, RichText};

use crate::dialog::Want;
use crate::hw;
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
    TransportStop,
    LoopRecord,
    LoopUndo,
    LoopPart,
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
    const ALL: [ActionKind; 32] = [
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
        Self::TransportStop,
        Self::LoopRecord,
        Self::LoopUndo,
        Self::LoopPart,
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
            Self::TransportStop => "Stop (transport and loops)",
            Self::LoopRecord => "Loop: record / overdub the slot that plays",
            Self::LoopUndo => "Loop: undo the last pass of the slot that plays",
            Self::LoopPart => "Loop: switch the slot that plays between part A and B",
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
            Self::TransportStop => A::TransportStop,
            Self::LoopRecord => A::LoopRecord,
            Self::LoopUndo => A::LoopUndo,
            Self::LoopPart => A::LoopPart,
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
        Status::Ok => hw::GREEN,
        Status::Warn => hw::AMBER,
        Status::Fail => Color32::from_rgb(0xe0, 0x6c, 0x5a),
    }
}

// ---- the pieces every tab is built from ----------------------------------------------------

/// How wide the settings column grows: lines of text much longer than this are hard to read.
const COLUMN: f32 = 900.0;

/// A titled group of settings on a raised plate: its name, what it is for, then its controls.
fn section(ui: &mut egui::Ui, title: &str, blurb: &str, body: impl FnOnce(&mut egui::Ui)) {
    hw::plate()
        .inner_margin(egui::Margin::symmetric(14, 10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 6.0;
            ui.label(
                RichText::new(title.to_uppercase())
                    .size(14.0)
                    .strong()
                    .color(hw::TEXT),
            );
            if !blurb.is_empty() {
                note(ui, blurb);
            }
            ui.add_space(4.0);
            body(ui);
        });
    ui.add_space(12.0);
}

/// A quieter heading inside a section, for a part of it.
fn subhead(ui: &mut egui::Ui, text: &str) {
    ui.add_space(6.0);
    hw::engraved(ui, text);
}

/// Explanatory text: dimmer than the settings themselves, but readable.
fn note(ui: &mut egui::Ui, text: impl Into<String>) {
    ui.label(RichText::new(text).size(13.0).color(hw::TEXT_DIM));
}

/// A light and a line of text: how something stands.
fn status_line(ui: &mut egui::Ui, status: Status, text: impl Into<String>) {
    ui.horizontal(|ui| {
        hw::led(ui, true, status_color(status));
        ui.label(RichText::new(text).color(hw::TEXT));
    });
}

/// Text from a running job (an install, a download), as it comes.
fn progress(ui: &mut egui::Ui, busy: bool, text: &str) {
    if text.is_empty() {
        return;
    }
    ui.horizontal(|ui| {
        if busy {
            hw::throbber(ui, hw::AMBER);
        }
        ui.label(RichText::new(text).color(hw::TEXT));
    });
}

/// One entry of a list in a recessed well: an optional light, the text (selectable, so a path
/// or an error can be copied), a dimmer second line, and keys at the right end. `keys` adds
/// them right to left.
fn row(
    ui: &mut egui::Ui,
    light: Option<Color32>,
    text: RichText,
    detail: Option<&str>,
    keys: impl FnOnce(&mut egui::Ui),
) {
    hw::inset()
        .inner_margin(egui::Margin::symmetric(8, 4))
        .show(ui, |ui| {
            egui::Sides::new().shrink_left().wrap().show(
                ui,
                |ui| {
                    if let Some(c) = light {
                        hw::led(ui, true, c);
                    }
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 1.0;
                        ui.label(text);
                        if let Some(d) = detail.filter(|d| !d.is_empty()) {
                            ui.label(RichText::new(d).size(12.5).color(hw::TEXT_DIM));
                        }
                    });
                },
                keys,
            );
        });
}

/// A path, as a row shows it.
fn path_text(path: &std::path::Path) -> RichText {
    RichText::new(path.display().to_string())
        .monospace()
        .color(hw::TEXT)
}

/// An engraved name in a column of its own, so the things it names line up.
fn label_cell(ui: &mut egui::Ui, text: &str, width: f32) {
    ui.allocate_ui_with_layout(
        egui::vec2(width, 20.0),
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| {
            ui.set_min_width(width);
            hw::engraved(ui, text);
        },
    );
}

/// A labelled readout: an engraved name and a selectable value beside it.
fn readout(ui: &mut egui::Ui, label: &str, value: &str, colour: Color32) {
    ui.horizontal(|ui| {
        label_cell(ui, label, 150.0);
        ui.label(RichText::new(value).monospace().color(colour));
    });
}

pub fn show(ui: &mut egui::Ui, s: &Session, st: &mut UiState, actions: &mut Vec<Action>) {
    const TABS: [(SettingsTab, &str); 7] = [
        (SettingsTab::Plugins, "Plugins"),
        (SettingsTab::Audio, "Audio"),
        (SettingsTab::Midi, "MIDI devices"),
        (SettingsTab::Controls, "Controls"),
        (SettingsTab::Windows, "Windows plugins"),
        (SettingsTab::Updates, "Updates"),
        (SettingsTab::Performance, "Performance"),
    ];
    ui.add_space(6.0);
    ui.horizontal_wrapped(|ui| {
        ui.heading("Settings");
        ui.add_space(12.0);
        let names = TABS.map(|(_, n)| n);
        let on = TABS
            .iter()
            .position(|(t, _)| *t == st.settings_tab)
            .unwrap_or(0);
        if let Some(n) = hw::selector(ui, &names, on, hw::AMBER) {
            st.settings_tab = TABS[n].0;
        }
    });
    ui.add_space(8.0);
    egui::ScrollArea::vertical()
        .auto_shrink(false)
        .show(ui, |ui| {
            ui.set_max_width(COLUMN.min(ui.available_width()));
            match st.settings_tab {
                SettingsTab::Plugins => plugins(ui, s, st, actions),
                SettingsTab::Audio => audio(ui, s, st, actions),
                SettingsTab::Midi => midi(ui, s, st, actions),
                SettingsTab::Controls => controls(ui, s, st, actions),
                SettingsTab::Windows => windows(ui, s, st, actions),
                SettingsTab::Updates => updates(ui, s, actions),
                SettingsTab::Performance => performance(ui, s, st),
            }
        });
}

// ---- Plugins ---------------------------------------------------------------------------------

fn plugins(ui: &mut egui::Ui, s: &Session, st: &mut UiState, actions: &mut Vec<Action>) {
    section(
        ui,
        "Plugin folders",
        "Add any folder with plugins: CLAP, VST3, VST2 or LV2, Linux or Windows, mixed or not, with subfolders. Merlin works out what each file is. Windows plugins also need Windows plugin support (the Windows plugins tab).",
        |ui| {
            for (i, d) in s.settings.plugin_dirs.iter().enumerate() {
                row(ui, None, path_text(d), None, |ui| {
                    if hw::Cap::new("Remove").show(ui).clicked() {
                        actions.push(Action::RemoveDir(i));
                    }
                });
            }
            crate::ui::pick_button(ui, &mut st.picking, "Add folder…", Want::PluginDir, None);
            egui::CollapsingHeader::new("Standard folders, always searched")
                .default_open(false)
                .show(ui, |ui| {
                    for d in merlin_scanner::default_plugin_dirs() {
                        ui.label(
                            RichText::new(d.display().to_string())
                                .monospace()
                                .color(hw::TEXT_DIM),
                        );
                    }
                });
            if !s.windows_status.is_empty() {
                note(ui, &s.windows_status);
            }
        },
    );
    sound_folders(ui, s, st, actions);
    library_scan(ui, s, actions);
    problem_files(ui, s, actions);
}

/// Where SoundFonts and SFZ instruments are looked for, with how many each folder holds.
fn sound_folders(ui: &mut egui::Ui, s: &Session, st: &mut UiState, actions: &mut Vec<Action>) {
    use merlin_model::Format;
    let count = |dir: &std::path::Path| {
        // Files, not browser entries: a folder of SoundFonts is one entry.
        let n = s
            .entries
            .iter()
            .filter(|e| {
                e.path.starts_with(dir)
                    && e.plugins
                        .iter()
                        .any(|p| matches!(p.format, Format::SoundFont | Format::Sfz))
            })
            .count();
        format!("{n} found")
    };
    section(
        ui,
        "Sound folders",
        "Every SoundFont (.sf2, .sf3) and SFZ instrument in these folders and their subfolders shows up in the browser. A folder with several of them directly inside it becomes one instrument named like the folder, with all its sounds in one list (the standard folders excepted). To add an SFZ library, unpack it anywhere and add its folder. You can also drop a file on the window.",
        |ui| {
            for (i, d) in s.settings.sound_dirs.iter().enumerate() {
                row(ui, None, path_text(d), Some(&count(d)), |ui| {
                    if hw::Cap::new("Remove").show(ui).clicked() {
                        actions.push(Action::RemoveSoundDir(i));
                    }
                });
            }
            crate::ui::pick_button(ui, &mut st.picking, "Add folder…", Want::SoundDir, None);
            egui::CollapsingHeader::new("Standard folders, always searched")
                .id_salt("standard-sound-folders")
                .default_open(false)
                .show(ui, |ui| {
                    egui::Grid::new("standard-sound-folders-grid")
                        .num_columns(2)
                        .spacing([16.0, 2.0])
                        .show(ui, |ui| {
                            for d in merlin_scanner::default_sound_dirs() {
                                ui.label(path_text(&d).color(hw::TEXT_DIM));
                                note(ui, count(&d));
                                ui.end_row();
                            }
                        });
                });
        },
    );
}
/// What the last scan found, and keys to scan again.
fn library_scan(ui: &mut egui::Ui, s: &Session, actions: &mut Vec<Action>) {
    section(
        ui,
        "Library",
        "Merlin looks at a file again only when it changed. Rescan everything after updating a plugin that keeps its file date.",
        |ui| {
            ui.horizontal(|ui| {
                ui.add_enabled_ui(!s.scanning(), |ui| {
                    if hw::Cap::new("Rescan changed files").show(ui).clicked() {
                        actions.push(Action::Rescan(false));
                    }
                    if hw::Cap::new("Rescan everything").show(ui).clicked() {
                        actions.push(Action::Rescan(true));
                    }
                });
                ui.add_space(8.0);
                if s.scanning() {
                    let file = s
                        .scan_current
                        .as_ref()
                        .and_then(|p| p.file_name())
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    hw::throbber(ui, hw::AMBER);
                    ui.label(format!("Scanning {file}"));
                } else {
                    hw::led(ui, true, hw::GREEN);
                    ui.label(format!(
                        "{} plugins in {} files",
                        s.found.len(),
                        s.entries.len()
                    ));
                }
            });
        },
    );
}

/// Files that failed to scan, files and plugins the user put aside, and quarantined plugins:
/// each a section only when it has something in it.
fn problem_files(ui: &mut egui::Ui, s: &Session, actions: &mut Vec<Action>) {
    let failed: Vec<_> = s.entries.iter().filter(|e| e.error.is_some()).collect();
    if !failed.is_empty() {
        section(
            ui,
            &format!("Files that could not be scanned ({})", failed.len()),
            "They are tried again when the file changes, or with Retry. Ignore a file you know is broken or not a plugin: it is never loaded or listed here again.",
            |ui| {
                for e in failed {
                    row(
                        ui,
                        Some(status_color(Status::Fail)),
                        path_text(&e.path),
                        e.error.as_deref(),
                        |ui| {
                            if hw::Cap::new("Ignore")
                                .show(ui)
                                .on_hover_text("Never scan this file again, and stop reporting it")
                                .clicked()
                            {
                                actions.push(Action::IgnoreFile(e.path.clone()));
                            }
                            if hw::Cap::new("Retry").show(ui).clicked() {
                                actions.push(Action::RetryFile(e.path.clone()));
                            }
                        },
                    );
                }
            },
        );
    }

    if !s.settings.ignored_files.is_empty() {
        section(ui, "Ignored files", "", |ui| {
            for p in &s.settings.ignored_files {
                row(ui, None, path_text(p), None, |ui| {
                    if hw::Cap::new("Stop ignoring").show(ui).clicked() {
                        actions.push(Action::UnignoreFile(p.clone()));
                    }
                });
            }
        });
    }

    if !s.hidden.is_empty() {
        section(
            ui,
            "Hidden plugins",
            "Hidden from the library with right-click > Hide. Rigs that use them still load.",
            |ui| {
                for f in &s.hidden {
                    let name = RichText::new(&f.info.name).strong().color(hw::TEXT);
                    row(ui, None, name, Some(&f.info.vendor), |ui| {
                        if hw::Cap::new("Show again").show(ui).clicked() {
                            actions.push(Action::UnhidePlugin(Session::favorite_key(f)));
                        }
                    });
                }
            },
        );
    }

    let quarantined = s.quarantine.entries();
    if !quarantined.is_empty() {
        section(
            ui,
            "Quarantined plugins",
            "These crashed the app. They are not loaded automatically.",
            |ui| {
                for q in quarantined {
                    let name = RichText::new(&q.name).strong().color(hw::TEXT);
                    row(
                        ui,
                        Some(status_color(Status::Warn)),
                        name,
                        Some(&q.reason),
                        |ui| {
                            if hw::Cap::new("Release")
                                .show(ui)
                                .on_hover_text("Take it out of quarantine, so it loads again")
                                .clicked()
                            {
                                actions.push(Action::Unquarantine(q.path.clone(), q.id.clone()));
                            }
                        },
                    );
                }
            },
        );
    }
}

// ---- Audio -----------------------------------------------------------------------------------

fn audio(ui: &mut egui::Ui, s: &Session, st: &mut UiState, actions: &mut Vec<Action>) {
    let Some(b) = s.backend() else {
        section(ui, "Audio", "", |ui| {
            status_line(ui, Status::Fail, "Audio is not running.");
        });
        return;
    };
    let frames = b.buffer_size();
    engine_panel(ui, s, b);
    ui.add_space(12.0);
    section(
        ui,
        "Buffer size",
        "Smaller is more responsive but needs a faster, well set up machine (see the Performance tab).",
        |ui| {
            const SIZES: [u32; 5] = [64, 128, 256, 512, 1024];
            ui.horizontal(|ui| {
                let labels = SIZES.map(|n| n.to_string());
                let names: Vec<&str> = labels.iter().map(String::as_str).collect();
                let on = SIZES
                    .iter()
                    .position(|n| *n == frames)
                    .unwrap_or(usize::MAX);
                if let Some(n) = hw::selector(ui, &names, on, hw::AMBER)
                    && SIZES[n] != frames
                {
                    actions.push(Action::SetBuffer(SIZES[n]));
                }
                ui.add_space(6.0);
                note(
                    ui,
                    format!(
                        "frames · {:.1} ms per buffer",
                        frames as f32 * 1000.0 / s.sample_rate as f32
                    ),
                );
            });
        },
    );
    // Listed once per frame: asking the server for its ports is not free.
    let pairs = s.output_pairs();
    main_output(ui, s, &pairs, actions);
    click_output(ui, s, &pairs, actions);
    backing_folder(ui, s, st, actions);
}

/// What the audio engine is doing, as one plate of recessed readouts: the server Merlin is
/// connected to, the format and what it costs in delay, the load, and where the sound goes.
fn engine_panel(ui: &mut egui::Ui, s: &Session, b: &merlin_audio_io::JackBackend) {
    let (frames, rate) = (b.buffer_size(), s.sample_rate);
    let one_way = frames as f32 * 1000.0 / rate as f32;
    let xruns = b.stats.xruns.load(Ordering::Relaxed);
    let devices = s.main_output_devices();
    let out = if devices.is_empty() {
        ("not connected".to_owned(), hw::RED)
    } else {
        (devices.join(", "), hw::AMBER)
    };
    let rows = [
        (
            "Server",
            merlin_audio_io::server_name().to_owned(),
            hw::AMBER,
        ),
        ("Format", format!("{rate} Hz · {frames} frames"), hw::AMBER),
        (
            "Latency",
            format!(
                "{one_way:.1} ms per buffer · about {:.1} ms round trip",
                one_way * 2.0
            ),
            hw::AMBER,
        ),
        (
            "Load",
            format!(
                "DSP {:.1}% · dropouts {xruns} · slowest callback {:.0} µs",
                b.cpu_load(),
                b.stats.max_ns.load(Ordering::Relaxed) as f64 / 1000.0,
            ),
            hw::AMBER,
        ),
        ("Output", out.0, out.1),
    ];
    // Every readout as wide as the longest text needs, so the plate is a tidy block.
    let font = egui::FontId::monospace(12.0);
    let width = rows
        .iter()
        .map(|(_, text, _)| {
            ui.painter()
                .layout_no_wrap(text.clone(), font.clone(), Color32::WHITE)
                .size()
                .x
        })
        .fold(300.0_f32, f32::max)
        + 24.0;
    hw::plate()
        .inner_margin(egui::Margin::symmetric(14, 10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(
                RichText::new("AUDIO ENGINE")
                    .size(14.0)
                    .strong()
                    .color(hw::TEXT),
            );
            ui.add_space(4.0);
            egui::Grid::new("audio-engine")
                .num_columns(3)
                .spacing([10.0, 6.0])
                .show(ui, |ui| {
                    for (label, text, colour) in &rows {
                        hw::engraved(ui, label);
                        hw::display(ui, text, width, *colour);
                        if *label == "Load" {
                            hw::led(ui, true, if xruns == 0 { hw::GREEN } else { hw::AMBER })
                                .on_hover_text(if xruns == 0 {
                                    "No dropouts since Merlin started."
                                } else {
                                    "The audio dropped out at least once since Merlin started."
                                });
                        }
                        ui.end_row();
                    }
                });
        });
}

/// Where the main mix is heard.
fn main_output(
    ui: &mut egui::Ui,
    s: &Session,
    pairs: &[merlin_audio_io::OutputPair],
    actions: &mut Vec<Action>,
) {
    section(
        ui,
        "Main output",
        "Where the mix is heard. Automatic follows the desktop's default output, as it is when Merlin starts or when you pick Automatic again. Pick an interface to stay on it whatever the desktop does: while it is unplugged, Merlin waits for it and plays through the default. Merlin goes through the audio server (PipeWire or JACK), so other programs keep their sound.",
        |ui| {
            if let Some(choice) = output_picker(
                ui,
                "main-output",
                pairs,
                &s.settings.main_output,
                "Automatic (system default)",
            ) {
                actions.push(Action::SetMainOutput(choice));
            }
            ui.add_space(2.0);
            match s.system_default_output() {
                Some(name) => readout(ui, "System default", name, hw::TEXT),
                None => {
                    readout(ui, "System default", "not known", hw::TEXT_DIM);
                    note(
                        ui,
                        "It is asked of WirePlumber (wpctl), which did not answer, so Automatic uses the first outputs the audio server lists.",
                    );
                }
            }
            if let Some(b) = s.backend() {
                readout(ui, "Merlin's ports", &b.out_names.join(", "), hw::TEXT);
            }
        },
    );
}

/// Choosing a pair of the interface's outputs from a dropdown. The first entry is the default
/// (`None`). A choice whose interface is not plugged in right now is kept, and shown as such.
/// Returns what was chosen this frame.
fn output_picker(
    ui: &mut egui::Ui,
    id: &str,
    pairs: &[merlin_audio_io::OutputPair],
    current: &Option<[String; 2]>,
    default_label: &str,
) -> Option<Option<[String; 2]>> {
    let shown = match current {
        None => default_label.to_owned(),
        Some(saved) => match pairs.iter().find(|p| &p.ports == saved) {
            Some(pair) => pair.label.clone(),
            None => format!(
                "{}  (not connected)",
                merlin_audio_io::device_label(&saved[0])
            ),
        },
    };
    let mut chosen = None;
    egui::ComboBox::from_id_salt(id)
        .selected_text(shown)
        .width(ui.available_width().min(460.0))
        .show_ui(ui, |ui| {
            if ui
                .selectable_label(current.is_none(), default_label)
                .clicked()
            {
                chosen = Some(None);
            }
            for pair in pairs {
                let on = current.as_ref() == Some(&pair.ports);
                let row = ui.selectable_label(on, &pair.label);
                if row.on_hover_text(pair.ports.join("  +  ")).clicked() {
                    chosen = Some(Some(pair.ports.clone()));
                }
            }
        });
    chosen
}

/// The folder the Setlist view lists backing tracks from.
fn backing_folder(ui: &mut egui::Ui, s: &Session, st: &mut UiState, actions: &mut Vec<Action>) {
    section(
        ui,
        "Backing tracks folder",
        "The Setlist view lists the audio files in this folder (and the folders inside it) beside the songs, so a backing track is a double-click away.",
        |ui| {
            let text = match s.backing_dir() {
                Some(d) => path_text(d),
                None => RichText::new("not chosen yet").color(hw::TEXT_DIM),
            };
            row(ui, None, text, None, |ui| {
                if s.backing_dir().is_some() && hw::Cap::new("Clear").show(ui).clicked() {
                    actions.push(Action::SetBackingDir(None));
                }
                crate::ui::pick_button(
                    ui,
                    &mut st.picking,
                    "Choose…",
                    Want::BackingDir,
                    s.backing_dir(),
                );
            });
        },
    );
}

/// The metronome: its bar length and where it is heard.
fn click_output(
    ui: &mut egui::Ui,
    s: &Session,
    pairs: &[merlin_audio_io::OutputPair],
    actions: &mut Vec<Action>,
) {
    section(
        ui,
        "Click",
        "The metronome is mixed into the main output unless you pick two of your interface's own outputs for it, to hear it in headphones only.",
        |ui| {
            ui.horizontal(|ui| {
                let mut beats = s.rig.transport.beats_per_bar;
                if ui
                    .add(egui::DragValue::new(&mut beats).range(1..=16))
                    .changed()
                {
                    actions.push(Action::SetBeatsPerBar(beats));
                }
                ui.label("beats per bar, when the song does not set it");
            });
            ui.horizontal(|ui| {
                label_cell(ui, "Heard on", 150.0);
                if let Some(choice) = output_picker(
                    ui,
                    "click-output",
                    pairs,
                    &s.settings.click_output,
                    "Main outputs (mixed in)",
                ) {
                    actions.push(Action::SetClickOutput(choice));
                }
            });
            note(
                ui,
                "The MIDI clock, and the program changes and controllers that song markers send, go out of Merlin's \"midi_out\" port: connect it in your patchbay.",
            );
        },
    );
}

// ---- Controls --------------------------------------------------------------------------------

/// What the MIDI controls do: the list of mappings, and teaching a new one by moving a control.
fn controls(ui: &mut egui::Ui, s: &Session, st: &mut UiState, actions: &mut Vec<Action>) {
    section(
        ui,
        "Teach a control",
        "Pick what a pedal, pad, knob or fader should do, press Learn, then press or move it. Mappings belong to the rig and are saved with it. A mapped control no longer plays an instrument.",
        |ui| {
            new_mapping(ui, s, st, actions);
            ui.add_space(2.0);
            match s.last_control() {
                Some(m) => readout(ui, "Last message", m, hw::AMBER),
                None => readout(
                    ui,
                    "Last message",
                    "nothing yet: press something on your device",
                    hw::TEXT_DIM,
                ),
            }
        },
    );

    // What connected controllers do without any mapping of yours.
    let built_in = s.profile_mappings();
    if !built_in.is_empty() {
        section(
            ui,
            &format!(
                "Built-in layout of connected controllers ({})",
                built_in.len()
            ),
            "These work until you map the same control yourself (below, or with MIDI map): yours then wins and the row is crossed out.",
            |ui| {
                egui::CollapsingHeader::new("Show the layout")
                    .id_salt("profile-mappings-open")
                    .show(ui, |ui| {
                        egui::Grid::new("profile-mappings")
                            .num_columns(3)
                            .spacing([14.0, 4.0])
                            .striped(true)
                            .show(ui, |ui| {
                                for (m, active) in &built_in {
                                    let text = |t: String| {
                                        let t = RichText::new(t);
                                        if *active { t } else { t.strikethrough().weak() }
                                    };
                                    ui.label(text(m.label.clone().unwrap_or_default()));
                                    ui.label(text(m.source.describe()).monospace());
                                    ui.label(text(format!("→  {}", m.action.describe())));
                                    ui.end_row();
                                }
                            });
                    });
            },
        );
    }

    section(
        ui,
        &format!("Your mappings ({})", s.mappings().len()),
        "",
        |ui| {
            if s.mappings().is_empty() {
                note(
                    ui,
                    "None yet: teach one above, or use MIDI map in the rack.",
                );
                return;
            }
            mapping_list(ui, s, actions);
        },
    );
}

/// The action picker, its slot/scene/macro number, and the Learn key.
fn new_mapping(ui: &mut egui::Ui, s: &Session, st: &mut UiState, actions: &mut Vec<Action>) {
    ui.horizontal_wrapped(|ui| {
        egui::ComboBox::from_id_salt("new-mapping-kind")
            .width(300.0)
            .selected_text(st.new_kind.name())
            .show_ui(ui, |ui| {
                for k in ActionKind::ALL {
                    ui.selectable_value(&mut st.new_kind, k, k.name());
                }
            });
        let number = |ui: &mut egui::Ui, label: &str, value: &mut usize, max: usize| {
            ui.label(label);
            let mut shown = *value + 1;
            ui.add(egui::DragValue::new(&mut shown).range(1..=max));
            *value = shown - 1;
        };
        if st.new_kind.needs_slot() {
            number(ui, "slot", &mut st.new_slot, crate::session::UI_SLOTS);
        }
        if st.new_kind == ActionKind::SelectByValue {
            number(ui, "first slot", &mut st.new_base, crate::session::UI_SLOTS);
        }
        if matches!(
            st.new_kind,
            ActionKind::SlotMacro | ActionKind::SelectedMacro
        ) {
            number(ui, "macro", &mut st.new_base, merlin_model::MACROS_PER_SLOT);
        }
        if st.new_kind == ActionKind::RecallSnapshot {
            number(ui, "scene", &mut st.new_slot, merlin_model::MAX_SNAPSHOTS);
        }
        if st.new_kind == ActionKind::RecallSnapshotByValue {
            number(
                ui,
                "first scene",
                &mut st.new_base,
                merlin_model::MAX_SNAPSHOTS,
            );
        }
        ui.add_space(6.0);
        match s.learning() {
            Some(_) => {
                if hw::Cap::new("Cancel").show(ui).clicked() {
                    actions.push(Action::CancelLearn);
                }
                hw::led(ui, true, hw::AMBER);
                ui.label(RichText::new("Press or move a control on your device…").color(hw::AMBER));
            }
            None => {
                if hw::Cap::new("Learn")
                    .flash(hw::AMBER)
                    .min_width(80.0)
                    .show(ui)
                    .clicked()
                {
                    actions.push(Action::LearnControl(
                        st.new_kind.build(st.new_slot, st.new_base),
                    ));
                }
            }
        }
    });
}

/// The rig's own mappings, each with how it behaves.
fn mapping_list(ui: &mut egui::Ui, s: &Session, actions: &mut Vec<Action>) {
    use merlin_model::Takeover;
    egui::Grid::new("mappings")
        .num_columns(5)
        .spacing([14.0, 6.0])
        .striped(true)
        .show(ui, |ui| {
            for (i, m) in s.mappings().iter().enumerate() {
                ui.label(
                    RichText::new(m.source.describe())
                        .monospace()
                        .color(hw::AMBER),
                );
                ui.label(match &m.label {
                    Some(name) => format!("→  {} ({name})", m.action.describe()),
                    None => format!("→  {}", m.action.describe()),
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
                if hw::Cap::new("Remove").show(ui).clicked() {
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
    ui.menu_button("Options…", |ui| {
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

// ---- MIDI devices ----------------------------------------------------------------------------

fn midi(ui: &mut egui::Ui, s: &Session, st: &mut UiState, actions: &mut Vec<Action>) {
    use crate::settings::DeviceRole;
    let devices = s.midi_devices();
    section(
        ui,
        "MIDI devices",
        "A keyboard plays the instruments. A controller (pads, knobs, faders) never does: its messages only control the app. Switch a device off to leave it alone. Velocity sets how hard you play for how loud. Devices are remembered by name, so plugging one in again brings back its role and curve.",
        |ui| {
            if devices.is_empty() {
                status_line(ui, Status::Warn, "No MIDI devices found.");
            }
            egui::Grid::new("midi-devices")
                .num_columns(5)
                .spacing([12.0, 8.0])
                .show(ui, |ui| {
                    for d in &devices {
                        hw::led(
                            ui,
                            d.active,
                            if d.active { hw::GREEN } else { hw::TEXT_DIM },
                        )
                        .on_hover_text(if d.active {
                            "Connected and in use"
                        } else {
                            "Not in use"
                        });
                        ui.vertical(|ui| {
                            ui.spacing_mut().item_spacing.y = 0.0;
                            ui.add(
                                egui::Label::new(RichText::new(&d.name).strong().color(hw::TEXT))
                                    .extend(),
                            );
                            ui.label(
                                RichText::new(match d.input {
                                    Some(n) => format!("input {n}"),
                                    None if !d.enabled => "off".to_owned(),
                                    None => "not connected (all inputs in use)".to_owned(),
                                })
                                .size(12.5)
                                .color(hw::TEXT_DIM),
                            );
                        });
                        let mut role = d.role;
                        let on = match role {
                            DeviceRole::Keyboard => 0,
                            DeviceRole::Controller => 1,
                        };
                        if let Some(n) =
                            hw::selector(ui, &["Keyboard", "Controller"], on, hw::AMBER)
                        {
                            role = [DeviceRole::Keyboard, DeviceRole::Controller][n];
                        }
                        let mut enabled = d.enabled;
                        if hw::Cap::new(if enabled { "On" } else { "Off" })
                            .lit(enabled.then_some(hw::GREEN))
                            .min_width(50.0)
                            .show(ui)
                            .on_hover_text("Use this device, or leave it alone completely")
                            .clicked()
                        {
                            enabled = !enabled;
                        }
                        if role != d.role || enabled != d.enabled {
                            actions.push(Action::SetMidiDevice(d.name.clone(), role, enabled));
                        }
                        let open = st.velocity_open.as_ref() == Some(&d.name);
                        let curve = crate::settings::VelocityCurve::PRESETS
                            .iter()
                            .find(|(c, _)| *c == d.velocity)
                            .map_or("Drawn", |(_, n)| *n);
                        if hw::Cap::new(format!("Velocity: {curve}"))
                            .lit(open.then_some(hw::AMBER))
                            .min_width(140.0)
                            .show(ui)
                            .on_hover_text("How hard you have to play for how loud")
                            .clicked()
                        {
                            st.velocity_open = (!open).then(|| d.name.clone());
                            st.velocity_stroke = None;
                        }
                        ui.end_row();
                    }
                });
            // The curve of the device picked above, while it is plugged in.
            if let Some(d) = devices
                .iter()
                .find(|d| st.velocity_open.as_ref() == Some(&d.name))
            {
                ui.add_space(8.0);
                let last = d.input.and_then(|n| s.last_velocity(n));
                crate::velocity_ui::editor(ui, d, last, &mut st.velocity_stroke, actions);
            }
            for (_, what) in s.surface_devices() {
                status_line(
                    ui,
                    Status::Ok,
                    format!("{what}: built-in layout, lights and screen are on (see Controls)"),
                );
            }
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                if hw::Cap::new("Look for new devices").show(ui).clicked() {
                    actions.push(Action::RefreshMidi);
                }
                note(ui, "Plugged-in devices are found by themselves, too.");
            });
        },
    );
    capture(ui, s, st, actions);
}

/// The MIDI capture: how much is kept, where it goes, and a button to save it now.
fn capture(ui: &mut egui::Ui, s: &Session, st: &mut UiState, actions: &mut Vec<Action>) {
    section(
        ui,
        "MIDI capture",
        "Merlin always keeps the last minutes of what you play on keyboards and the on-screen keys (not controllers, and not what is mapped to an action). Capture MIDI, or a control mapped to it, saves them as a .mid file with a track per keyboard. Nothing is written to disk until then.",
        |ui| {
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
                ui.add_space(8.0);
                let (listened, kept) = s.capture_status();
                if minutes == 0 {
                    hw::led(ui, false, hw::GREEN);
                    note(ui, "off");
                } else {
                    hw::led(ui, true, hw::GREEN);
                    note(
                        ui,
                        format!(
                            "listening for {} · {kept} messages kept",
                            crate::session::clock(listened)
                        ),
                    );
                }
            });
            subhead(ui, "Saved to");
            row(ui, None, path_text(&s.capture_dir()), None, |ui| {
                if hw::Cap::new("Default").show(ui).clicked() {
                    actions.push(Action::SetCaptureDir(None));
                }
                crate::ui::pick_button(
                    ui,
                    &mut st.picking,
                    "Choose…",
                    Want::CaptureDir,
                    Some(&s.capture_dir()),
                );
            });
            note(
                ui,
                "Audio recordings (Record in the Perform view) are saved here too.",
            );
            ui.add_enabled_ui(s.capture_minutes() > 0, |ui| {
                if hw::Cap::new("Save now").flash(hw::GREEN).show(ui).clicked() {
                    actions.push(Action::CaptureMidi);
                }
            });
        },
    );
}

// ---- Windows plugins -------------------------------------------------------------------------

fn windows(ui: &mut egui::Ui, s: &Session, st: &mut UiState, actions: &mut Vec<Action>) {
    windows_support(ui, s, actions);
    windows_plugins(ui, s, st, actions);
    section(
        ui,
        "Plugins from vendors' store apps",
        "Native Access (Kontakt), IK Product Manager, iZotope Product Portal, Spitfire Audio, Arturia Software Center and other store apps need fixes under Wine that this app does not have. Cabinet, a free app, installs them with those fixes; the plugins it installs show up here by themselves.",
        |ui| cabinet_keys(ui, s, actions),
    );
    section(
        ui,
        "In use right now",
        "Bridges other tools made (Cabinet, yabridgectl) run with their own Wine.",
        |ui| {
            let rt = s.windows_runtime();
            for (what, value) in [
                ("Wine", rt.as_ref().map(|r| r.wine.display().to_string())),
                (
                    "yabridge",
                    rt.as_ref().map(|r| r.yabridge_dir.display().to_string()),
                ),
            ] {
                match value {
                    Some(v) => readout(ui, what, &v, hw::TEXT),
                    None => readout(ui, what, "not set", hw::TEXT_DIM),
                }
            }
            egui::CollapsingHeader::new("Advanced: use my own Wine and yabridge")
                .default_open(false)
                .show(ui, |ui| own_wine(ui, st, actions));
        },
    );
}

/// "Open Cabinet", or "Install Cabinet" with what that runs; shared with the installer popup.
pub fn cabinet_keys(ui: &mut egui::Ui, s: &Session, actions: &mut Vec<Action>) {
    use crate::cabinet;
    ui.horizontal_wrapped(|ui| {
        if cabinet::installed() {
            hw::led(ui, true, hw::GREEN);
            ui.label("Cabinet is installed");
            ui.add_space(6.0);
            if hw::Cap::new("Open Cabinet").show(ui).clicked() {
                actions.push(Action::OpenCabinet);
            }
        } else if cabinet::flatpak_available() {
            ui.add_enabled_ui(!s.windows_busy(), |ui| {
                if hw::Cap::new("Install Cabinet")
                    .show(ui)
                    .on_hover_text("Installs Cabinet from its own Flatpak repository, for you only (no password asked)")
                    .clicked()
                {
                    actions.push(Action::InstallCabinet);
                }
            });
        } else {
            hw::led(ui, true, hw::AMBER);
            ui.label("Cabinet is a Flatpak: install Flatpak from your distribution first");
        }
        ui.add_space(6.0);
        ui.hyperlink_to("About Cabinet", cabinet::HOMEPAGE);
    });
    if !cabinet::installed() {
        note(
            ui,
            "Installing it runs these, which you can also run yourself:",
        );
        for line in cabinet::install_commands() {
            hw::command(ui, &line);
        }
    }
}

/// "Windows plugin support": the Wine + yabridge pair the app downloads and keeps itself.
fn windows_support(ui: &mut egui::Ui, s: &Session, actions: &mut Vec<Action>) {
    section(
        ui,
        "Windows plugin support",
        "Windows plugins run through Wine and yabridge. The app downloads a tested pair (about 70 MB) and keeps it in its own folder; your system Wine is never touched.",
        |ui| match (s.installed_runtime(), s.windows_runtime()) {
            (Some(r), _) => {
                status_line(ui, Status::Ok, format!("Installed: {}", r.id));
                if !r.runs_32_bit() {
                    // An early download: it cannot start 32-bit programs, which most installers are.
                    status_line(
                        ui,
                        Status::Fail,
                        "This version cannot run most plugin installers (they are 32-bit).",
                    );
                    ui.add_enabled_ui(!s.windows_busy(), |ui| {
                        if hw::Cap::new("Update").show(ui).clicked() {
                            actions.push(Action::InstallRuntime);
                        }
                    });
                    progress(ui, s.windows_busy(), &s.windows_status);
                }
            }
            (None, Some(_)) => {
                status_line(
                    ui,
                    Status::Ok,
                    "Using a Wine and yabridge that were already set up on this machine.",
                );
            }
            (None, None) => {
                status_line(ui, Status::Warn, "Not installed");
                ui.add_enabled_ui(!s.windows_busy(), |ui| {
                    if hw::Cap::new("Download Windows plugin support")
                        .show(ui)
                        .clicked()
                    {
                        actions.push(Action::InstallRuntime);
                    }
                });
                progress(ui, s.windows_busy(), &s.windows_status);
            }
        },
    );
}

/// The paths of a Wine and a yabridge the user set up themselves.
fn own_wine(ui: &mut egui::Ui, st: &mut UiState, actions: &mut Vec<Action>) {
    note(
        ui,
        "Overrides the downloaded pair. Changes apply after restarting the app.",
    );
    ui.add_space(4.0);
    let picking = &mut st.picking;
    for (label, text, browse) in [
        ("Wine program", &mut st.wine_text, &Want::WineProgram),
        ("yabridge folder", &mut st.yab_text, &Want::YabridgeDir),
    ] {
        ui.horizontal(|ui| {
            label_cell(ui, label, 130.0);
            ui.add(egui::TextEdit::singleline(text).desired_width(420.0));
            let path = std::path::Path::new(text.as_str());
            let start = if *browse == Want::WineProgram {
                path.parent()
            } else {
                Some(path)
            };
            crate::ui::pick_button(ui, picking, "Browse…", *browse, start);
            if text.is_empty() {
                note(ui, "not set");
            } else if path.exists() {
                hw::led(ui, true, hw::GREEN).on_hover_text("Found");
            } else {
                hw::led(ui, true, status_color(Status::Fail));
                ui.label(RichText::new("does not exist").color(status_color(Status::Fail)));
            }
        });
    }
    if hw::Cap::new("Save").show(ui).clicked() {
        actions.push(Action::SaveWine(st.wine_text.clone(), st.yab_text.clone()));
    }
}

/// Installing Windows plugins and what the Wine setup looks like (a small "Wine doctor").
fn windows_plugins(ui: &mut egui::Ui, s: &Session, st: &mut UiState, actions: &mut Vec<Action>) {
    section(
        ui,
        "Windows plugins",
        "Windows plugins in your folders (Plugins tab) are found and bridged by themselves. Only 64-bit CLAP, VST3 and VST2 plugins are picked up. Plugins with copy protection (iLok and the like) may not run. Prefer a native Linux version when there is one.",
        |ui| {
            let Some(rt) = s.windows_runtime() else {
                note(
                    ui,
                    "Download Windows plugin support above to use Windows plugins.",
                );
                return;
            };
            let ok = |good: bool| if good { Status::Ok } else { Status::Fail };
            status_line(
                ui,
                ok(rt.version().is_some()),
                format!(
                    "Wine: {}",
                    rt.version().unwrap_or_else(|| "does not run".into())
                ),
            );
            status_line(
                ui,
                ok(rt.chainloader().is_file()
                    && rt.vst3_chainloader().is_file()
                    && rt.vst2_chainloader().is_file()),
                "yabridge: CLAP, VST3 and VST2 chainloaders",
            );
            let ready = rt.prefix_ready();
            status_line(
                ui,
                if ready { Status::Ok } else { Status::Warn },
                if ready {
                    "Windows environment is ready"
                } else {
                    "Windows environment is made when you install the first plugin, or open it here"
                },
            );
            readout(
                ui,
                "Environment",
                &rt.prefix.display().to_string(),
                hw::TEXT,
            );
            ui.add_space(6.0);
            hw::inset()
                .inner_margin(egui::Margin::symmetric(12, 12))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.vertical_centered(|ui| {
                        ui.label(
                            RichText::new(
                                "Drop a Windows plugin installer (.exe or .msi) anywhere on this window",
                            )
                            .strong()
                            .color(hw::TEXT),
                        );
                        note(
                            ui,
                            "It runs in the app's own Windows environment, and the plugins it installs show up in the browser when it finishes.",
                        );
                    });
                });
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.add_enabled_ui(!s.windows_busy(), |ui| {
                    if hw::Cap::new("Look for installed plugins")
                        .show(ui)
                        .on_hover_text(
                            "Use this after installing a plugin by hand into the Windows environment",
                        )
                        .clicked()
                    {
                        actions.push(Action::SyncWindows);
                    }
                    if hw::Cap::new("Open Wine prefix")
                        .show(ui)
                        .on_hover_text(
                            "Show the Windows environment's folder in your file manager. It is made first when it does not exist yet.",
                        )
                        .clicked()
                    {
                        actions.push(Action::OpenWinePrefix);
                    }
                });
            });
            installed_apps(ui, s, actions);
            egui::CollapsingHeader::new("Install from a path instead")
                .default_open(false)
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.add(
                            egui::TextEdit::singleline(&mut st.installer_text)
                                .hint_text("/path/to/Plugin-Setup.exe")
                                .desired_width(420.0),
                        );
                        crate::ui::pick_button(
                            ui,
                            &mut st.picking,
                            "Browse…",
                            Want::Installer,
                            None,
                        );
                        let ready = !s.windows_busy() && !st.installer_text.trim().is_empty();
                        ui.add_enabled_ui(ready, |ui| {
                            if hw::Cap::new("Install").show(ui).clicked() {
                                actions.push(Action::InstallWindows(st.installer_text.clone()));
                            }
                        });
                    });
                });
            progress(ui, s.windows_busy(), &s.windows_status);
        },
    );
}

/// "Installed:" the programs in the Windows environment, one key cap each. Many vendors ship a
/// manager or downloader app (Native Access, Arturia Software Center, ...) rather than their
/// plugins, so it has to be possible to open it again after installing it.
fn installed_apps(ui: &mut egui::Ui, s: &Session, actions: &mut Vec<Action>) {
    subhead(ui, "Installed programs");
    if s.windows_apps.is_empty() {
        note(
            ui,
            "Nothing yet. A manager or downloader app you install here is listed, to open again.",
        );
        return;
    }
    ui.horizontal_wrapped(|ui| {
        ui.add_enabled_ui(!s.windows_busy(), |ui| {
            for app in &s.windows_apps {
                let open = hw::Cap::new(&app.name).show(ui).on_hover_text(format!(
                    "Open {} in the Windows environment. The plugins it installs are picked up when you close it.\n{}",
                    app.name,
                    app.exe.display()
                ));
                if open.clicked() {
                    actions.push(Action::LaunchWindowsApp(app.exe.display().to_string()));
                }
            }
        });
    });
}

// ---- Updates ---------------------------------------------------------------------------------

/// Looking for a newer release and installing it. Nothing here runs unless the user presses a
/// button: there is no check at startup and no popup.
fn updates(ui: &mut egui::Ui, s: &Session, actions: &mut Vec<Action>) {
    use crate::session::{APP_VERSION, UpdateBusy};
    use merlin_updater::Kind;

    let u = &s.update;
    section(
        ui,
        "Updates",
        "Merlin only looks for a new version when you ask here. It never checks in the background or at startup.",
        |ui| {
            readout(ui, "This version", APP_VERSION, hw::AMBER);
            ui.horizontal(|ui| {
                let checking = u.busy == UpdateBusy::Checking;
                ui.add_enabled_ui(u.busy == UpdateBusy::No, |ui| {
                    if hw::Cap::new("Check for updates").show(ui).clicked() {
                        actions.push(Action::CheckUpdate);
                    }
                });
                if checking {
                    hw::throbber(ui, hw::AMBER);
                    ui.label("Checking…");
                }
            });
            if !u.error.is_empty() {
                status_line(ui, Status::Fail, &u.error);
            }
            let Some(info) = &u.info else { return };
            ui.add_space(4.0);

            if u.installed {
                status_line(
                    ui,
                    Status::Ok,
                    format!(
                        "Version {} is installed. Restart Merlin to start using it.",
                        info.latest
                    ),
                );
                note(ui, "Your rig is saved and comes back as it was.");
                if hw::Cap::new("Restart now").show(ui).clicked() {
                    actions.push(Action::RestartForUpdate);
                }
                return;
            }
            if !info.available {
                status_line(ui, Status::Ok, "You're up to date.");
                return;
            }

            status_line(
                ui,
                Status::Warn,
                format!("A new version is available: {}", info.latest),
            );
            if u.busy == UpdateBusy::Installing {
                progress(ui, true, &u.status);
                if info.kind == Kind::Deb {
                    note(
                        ui,
                        "A password prompt from your desktop asks to install the package.",
                    );
                }
            } else if info.can_install {
                note(
                    ui,
                    match info.kind {
                        Kind::Deb => "Installs the package; your desktop asks for your password.",
                        Kind::AppImage => {
                            "Replaces this AppImage file; the running app keeps going until you restart."
                        }
                        _ => {
                            "Replaces the Merlin programs in this folder; the running app keeps going until you restart."
                        }
                    },
                );
            } else if !info.reason.is_empty() {
                note(ui, &info.reason);
            }
            ui.horizontal(|ui| {
                if info.can_install
                    && u.busy != UpdateBusy::Installing
                    && hw::Cap::new("Download and install")
                        .lit(Some(hw::GREEN))
                        .show(ui)
                        .clicked()
                {
                    actions.push(Action::InstallUpdate);
                }
                ui.add_enabled_ui(u.busy == UpdateBusy::No, |ui| {
                    if hw::Cap::new("Open release page").show(ui).clicked() {
                        actions.push(Action::OpenReleasePage);
                    }
                });
            });
            if !info.notes.trim().is_empty() {
                egui::CollapsingHeader::new("What's new")
                    .default_open(false)
                    .show(ui, |ui| {
                        ui.label(RichText::new(info.notes.trim()).color(hw::TEXT));
                    });
            }
        },
    );
}

// ---- Performance -----------------------------------------------------------------------------

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
            s.plugin_priorities(),
        );
        st.perf = Some((Instant::now(), perf::evaluate(&inputs)));
    }
    let Some((_, checks)) = &st.perf else {
        section(ui, "Performance check", "", |ui| {
            status_line(ui, Status::Fail, "Audio is not running.");
        });
        return;
    };
    let overall = perf::overall(checks);
    section(
        ui,
        "Performance check",
        "Is this machine set up for glitch-free live playing? Checked again every two seconds, so a change shows here as soon as it applies.",
        |ui| {
            ui.horizontal(|ui| {
                hw::led(ui, true, status_color(overall));
                ui.label(
                    RichText::new(match overall {
                        Status::Ok => "Ready for live playing",
                        Status::Warn => "Usable, with warnings",
                        Status::Fail => "Needs attention",
                    })
                    .size(17.0)
                    .strong()
                    .color(status_color(overall)),
                );
            });
            ui.add_space(4.0);
            for c in checks {
                hw::inset()
                    .inner_margin(egui::Margin::symmetric(10, 6))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.spacing_mut().item_spacing.y = 4.0;
                        ui.horizontal(|ui| {
                            hw::led(ui, true, status_color(c.status));
                            ui.label(RichText::new(c.name).strong().color(hw::TEXT));
                            ui.add_space(4.0);
                            ui.label(
                                RichText::new(match c.status {
                                    Status::Ok => "OK",
                                    Status::Warn => "WARNING",
                                    Status::Fail => "PROBLEM",
                                })
                                .size(11.5)
                                .strong()
                                .color(status_color(c.status)),
                            );
                        });
                        ui.label(RichText::new(&c.detail).color(hw::TEXT));
                        if let Some(fix) = &c.fix {
                            ui.label(RichText::new(format!("What to do: {fix}")).color(hw::mix(
                                hw::TEXT,
                                status_color(c.status),
                                0.35,
                            )));
                        }
                        for cmd in &c.commands {
                            hw::command(ui, cmd);
                        }
                    });
                ui.add_space(6.0);
            }
        },
    );
}
