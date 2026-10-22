//! Drawing: header, rack, editor dock bar, keyboard map. Pure view code: it reads the
//! session and returns actions; the app applies them.

use std::path::PathBuf;
use std::sync::atomic::Ordering;

use egui::{Color32, RichText};

use merlin_model::{Format, FxSettings};

use crate::browser;
use crate::keyboard::{self, COLORS, ZoneView};
use crate::paths;
use crate::perf;
use crate::session::{Session, UI_SLOTS};
use crate::settings_ui::{self, SettingsTab};

/// What dropping this file on the window does, and whether it does anything.
pub fn drop_hint(path: &std::path::Path) -> (String, bool) {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase);
    match ext.as_deref() {
        Some("exe" | "msi") => ("Drop to install this Windows plugin".into(), true),
        Some("sf2" | "sf3") => ("Drop to load this SoundFont into a free slot".into(), true),
        Some("irig") => ("Drop to open this rig".into(), true),
        _ => ("This kind of file can't be used here".into(), false),
    }
}

/// How a plugin is named in the browser: `Name`, `Name (VST3)`, `Name (Windows)`.
pub fn found_label(f: &merlin_scanner::Found) -> String {
    let mut label = f.info.name.clone();
    if f.info.format == Format::Vst3 {
        label.push_str(" (VST3)");
    }
    if f.info.format == Format::SoundFont {
        label.push_str(" (SoundFont)");
    }
    if f.windows {
        label.push_str(" (Windows)");
    }
    label
}

/// What can change about a song.
#[derive(Debug)]
pub enum SongEdit {
    Name(String),
    Tempo(f32),
    Beats(u8),
    Note(String),
}

/// What can change about a part.
#[derive(Debug)]
pub enum PartEdit {
    Name(String),
    Snapshot(Option<usize>),
    Tempo(Option<f32>),
    Note(String),
}

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
    /// Pick a SoundFont slot's preset (bank, program).
    SetPreset(usize, u16, u8),
    SamplerGain(usize, f32),
    SamplerTone(usize, merlin_hosting::SamplerTone),
    Transpose(usize, i8),
    Fit,
    Scroll(u32, u32),
    NoteOn(u8),
    NoteOff(u8),
    Panic,
    SetFx(usize, FxSettings),
    SetMaster(f32),
    /// Script-only: switch a named preset of effects on (`reverb`, `delay`, `comp`, `eq`, `off`).
    FxPreset(usize, String),
    /// Switch the dock between the plugin's editor and the slot's effects.
    Tab(DockTab),
    /// Put effect-plugin entry `k` into insert position `index` of slot `i`.
    LoadInsert(usize, usize, usize),
    UnloadInsert(usize, usize),
    BypassInsert(usize, usize),
    /// Script-only: load the named effect plugin as an insert.
    LoadInsertNamed(usize, usize, String),
    ToggleSettings,
    /// Script-only: show a settings tab.
    ShowSettingsTab(SettingsTab),
    AddDir(String),
    RemoveDir(usize),
    AddSoundDir(String),
    RemoveSoundDir(usize),
    Rescan(bool),
    RetryFile(PathBuf),
    Unquarantine(PathBuf, String),
    SetBuffer(u32),
    RefreshMidi,
    /// Teach a control: the next one the user moves or presses is mapped to this.
    LearnControl(merlin_model::ControlAction),
    CancelLearn,
    RemoveMapping(usize),
    SetTakeover(usize, merlin_model::Takeover),
    SetGlue(usize, merlin_model::Glue),
    /// Script-only: add a mapping without a device (`map cc 20 select 2`).
    AddMapping(merlin_model::Mapping),
    /// What a MIDI device is for, and whether it is used (device name, role, on).
    SetMidiDevice(String, crate::settings::DeviceRole, bool),
    SaveWine(String, String),
    /// A file was dropped on the window (or the script's `drop`): install, load or open it.
    DropFile(PathBuf),
    /// Run a Windows plugin installer (path) in the managed prefix, then pick up its plugins.
    InstallWindows(String),
    /// Make bridges for the Windows plugins already in the prefix.
    SyncWindows,
    /// Download and install the Wine + yabridge runtime, then restart.
    InstallRuntime,
    ToggleFavorite(usize),
    /// Load browser entry `k` into a free slot.
    BrowserLoad(usize),
    /// A browser entry was dropped onto slot `i`.
    DropLoad(usize, usize),
    NewRig,
    SaveRig,
    OpenRig(PathBuf),
    RenameRig(String),
    SetMacro(usize, usize, f32),
    RenameMacro(usize, usize, String),
    AddMacroParam(
        usize,
        usize,
        Option<usize>,
        merlin_hosting::ParamInfo,
    ),
    AddMacroSampler(usize, usize, merlin_model::SamplerField),
    RemoveMacroTarget(usize, usize, usize),
    SetMacroRange(usize, usize, usize, f32, f32),
    /// Script-only: make macro `k` of a slot drive the first parameter whose name contains the
    /// text (or `sampler:level`, `sampler:brightness`, ... for a SoundFont).
    AddMacroParamNamed(usize, usize, String),
    ToggleSetlist,
    AddSong,
    RemoveSong(usize),
    MoveSong(usize, bool),
    EditSong(usize, SongEdit),
    /// Add a part to a song: from the sound as it is now (a new snapshot), or empty.
    AddPart(usize, bool),
    RemovePart(usize, usize),
    MovePart(usize, usize, bool),
    EditPart(usize, usize, PartEdit),
    /// Enter part `k` of song `n`.
    GoTo(usize, usize),
    NextPart,
    PrevPart,
    NextSong,
    PrevSong,
    /// Show or leave the Perform view.
    SetPerform(bool),
    /// Lock the Perform view (it cannot be left) or unlock it.
    SetLock(bool),
    ToggleFullscreen,
    /// Save the current state as a new snapshot.
    AddSnapshot,
    RecallSnapshot(usize),
    UpdateSnapshot(usize),
    RenameSnapshot(usize, String),
    RemoveSnapshot(usize),
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DockTab {
    Plugin,
    /// The editor of insert `n`.
    Insert(usize),
    Effects,
    Macros,
}

/// UI state that is not part of the session.
pub struct UiState {
    pub tab: DockTab,
    pub settings_open: bool,
    /// The Setlist screen is showing, and the song being edited in it.
    pub setlist_open: bool,
    pub edit_song: usize,
    /// The Perform view is showing.
    pub perform: bool,
    /// Perform view locked: it cannot be left and dropped files are ignored.
    pub locked: bool,
    /// When the Unlock button was pressed down, while it is.
    pub unlock_since: Option<std::time::Instant>,
    pub settings_tab: SettingsTab,
    pub query: String,
    /// Search box of the preset list in a SoundFont slot.
    pub preset_query: String,
    /// Search box of the macro parameter picker.
    pub macro_query: String,
    pub favs_only: bool,
    pub dir_text: String,
    pub sound_dir_text: String,
    pub wine_text: String,
    pub yab_text: String,
    pub installer_text: String,
    /// The mapping being composed on the Controls tab: what kind of action, and its numbers.
    pub new_kind: crate::settings_ui::ActionKind,
    pub new_slot: usize,
    pub new_base: usize,
    /// What a file being dragged over the window would do, while one is.
    pub drag_hint: Option<(String, bool)>,
    pub perf: Option<(std::time::Instant, Vec<perf::Check>)>,
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
            tab: DockTab::Plugin,
            settings_open: false,
            setlist_open: false,
            edit_song: 0,
            perform: false,
            locked: false,
            unlock_since: None,
            settings_tab: SettingsTab::Plugins,
            query: String::new(),
            preset_query: String::new(),
            macro_query: String::new(),
            favs_only: false,
            dir_text: String::new(),
            sound_dir_text: String::new(),
            wine_text: String::new(),
            yab_text: String::new(),
            installer_text: String::new(),
            new_kind: Default::default(),
            new_slot: 0,
            new_base: 0,
            drag_hint: None,
            perf: None,
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
    drag_overlay(ui, st);
    if ui.input(|i| i.key_pressed(egui::Key::F11)) && !(st.perform && st.locked) {
        actions.push(Action::SetPerform(!st.perform));
    }
    if st.perform {
        *dock_rect = egui::Rect::NOTHING;
        crate::perform::show(ui, s, st, actions);
        return;
    }
    egui::Panel::top("header").show(ui, |ui| header(ui, s, st, actions));
    egui::Panel::top("snapshots").show(ui, |ui| snapshot_bar(ui, s, actions));
    if st.setlist_open && !st.settings_open {
        egui::CentralPanel::default().show(ui, |ui| {
            *dock_rect = egui::Rect::NOTHING;
            crate::setlist_ui::show(ui, s, st, actions);
        });
        return;
    }
    if st.settings_open {
        egui::CentralPanel::default().show(ui, |ui| {
            *dock_rect = egui::Rect::NOTHING;
            settings_ui::show(ui, s, st, actions);
        });
        return;
    }
    browser::panel(ui, s, st, actions);
    egui::Panel::top("rack").show(ui, |ui| {
        ui.add_space(2.0);
        for i in 0..UI_SLOTS {
            unit(ui, s, st, i, actions);
        }
    });
    egui::Panel::bottom("keys").show(ui, |ui| keys(ui, s, st, actions));
    egui::Panel::top("dockbar").show(ui, |ui| dock_bar(ui, s, st, actions));
    egui::CentralPanel::default().show(ui, |ui| {
        *dock_rect = ui.available_rect_before_wrap();
        if st.tab == DockTab::Effects
            && let Some(i) = s.viewing
        {
            fx_panel(ui, s, i, actions);
            return;
        }
        if st.tab == DockTab::Macros
            && let Some(i) = s.viewing
        {
            crate::macro_panel::show(ui, s, st, i, actions);
            return;
        }
        if st.tab == DockTab::Plugin
            && let Some(i) = s.viewing
            && s.is_sampler(i)
        {
            sampler_panel(ui, s, st, i, actions);
            return;
        }
        let note = s.viewing.and_then(|i| s.notes[i].as_deref());
        let text = if s.viewing.is_none() {
            "Load an instrument into a slot, then select it"
        } else if st.popup_open {
            "(editor hidden while a menu is open)"
        } else if s.editors.shown() != s.view_id {
            note.unwrap_or("This plugin has no editor")
        } else {
            ""
        };
        ui.centered_and_justified(|ui| ui.label(text));
    });
}

/// The sliders of a sampler's tone. Returns the new tone when one was touched.
fn tone_controls(
    ui: &mut egui::Ui,
    tone: merlin_hosting::SamplerTone,
) -> Option<merlin_hosting::SamplerTone> {
    let mut t = tone;
    let times = |v: f64| format!("x{:.2}", 2f64.powf(v));
    egui::Grid::new("sampler-tone")
        .num_columns(2)
        .spacing([12.0, 4.0])
        .show(ui, |ui| {
            ui.label("Attack");
            ui.add(
                egui::Slider::new(&mut t.attack, -3.0..=8.0)
                    .custom_formatter(|v, _| times(v))
                    .custom_parser(|s| {
                        s.trim_start_matches('x')
                            .parse::<f64>()
                            .ok()
                            .map(|m| m.log2())
                    }),
            )
            .on_hover_text(
                "How slowly notes fade in, relative to the sound's own attack (x1 = as made)",
            );
            ui.end_row();
            ui.label("Release");
            ui.add(
                egui::Slider::new(&mut t.release, -3.0..=4.0)
                    .custom_formatter(|v, _| times(v))
                    .custom_parser(|s| {
                        s.trim_start_matches('x')
                            .parse::<f64>()
                            .ok()
                            .map(|m| m.log2())
                    }),
            )
            .on_hover_text("How long notes ring after the key comes up (x1 = as made)");
            ui.end_row();
            ui.label("Brightness");
            ui.add(
                egui::Slider::new(&mut t.cutoff, -3.0..=3.0)
                    .suffix(" oct")
                    .fixed_decimals(1),
            )
            .on_hover_text("Filter cutoff in octaves: lower is darker, higher is brighter");
            ui.end_row();
            ui.label("Resonance");
            ui.add(
                egui::Slider::new(&mut t.resonance, 0.0..=24.0)
                    .suffix(" dB")
                    .fixed_decimals(0),
            )
            .on_hover_text("Emphasis at the filter cutoff");
            ui.end_row();
            ui.label("Tuning");
            ui.add(
                egui::Slider::new(&mut t.fine_tune, -100.0..=100.0)
                    .suffix(" cents")
                    .fixed_decimals(0),
            )
            .on_hover_text(
                "Fine tuning, 100 cents is a semitone (use the slot's tr for whole semitones)",
            );
            ui.end_row();
            ui.label("Voices");
            let mut voices = t.polyphony as u32;
            ui.add(egui::Slider::new(&mut voices, 4..=128))
                .on_hover_text(
                    "How many notes can sound at once; the oldest are cut when it is exceeded",
                );
            t.polyphony = voices as u16;
            ui.end_row();
        });
    if ui.small_button("Reset").clicked() {
        t = merlin_hosting::SamplerTone::default();
    }
    (t != tone).then_some(t)
}

/// A SoundFont slot's panel: the preset list (bank and program), a search box, the level.
fn sampler_panel(
    ui: &mut egui::Ui,
    s: &Session,
    st: &mut UiState,
    i: usize,
    actions: &mut Vec<Action>,
) {
    let (Some(presets), Some(current)) = (s.presets(i), s.preset(i)) else {
        return;
    };
    ui.horizontal(|ui| {
        ui.label("Search");
        ui.add(egui::TextEdit::singleline(&mut st.preset_query).desired_width(220.0));
        if let Some(mut gain) = s.sampler_gain(i) {
            ui.add_space(12.0);
            ui.label("level");
            if ui
                .add(egui::Slider::new(&mut gain, 0.0..=2.0).fixed_decimals(2))
                .on_hover_text("Output level of this SoundFont (1.00 = the file's own level)")
                .changed()
            {
                actions.push(Action::SamplerGain(i, gain));
            }
        }
        let now = presets
            .iter()
            .find(|p| (p.bank, p.program) == current)
            .map_or("", |p| p.name.as_str());
        ui.add_space(12.0);
        ui.colored_label(COLORS[i], format!("now: {now}"));
    });
    if let Some(tone) = s.sampler_tone(i) {
        egui::CollapsingHeader::new("Shape the sound")
            .default_open(false)
            .show(ui, |ui| {
                if let Some(new) = tone_controls(ui, tone)
                    && new != tone
                {
                    actions.push(Action::SamplerTone(i, new));
                }
            });
    }
    ui.separator();
    let query = st.preset_query.trim().to_lowercase();
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for p in presets {
                if !query.is_empty() && !p.name.to_lowercase().contains(&query) {
                    continue;
                }
                let label = format!("{:>3}:{:<3}  {}", p.bank, p.program, p.name);
                if ui
                    .selectable_label((p.bank, p.program) == current, label)
                    .clicked()
                {
                    actions.push(Action::SetPreset(i, p.bank, p.program));
                }
            }
        });
}

/// A banner over everything while a file is dragged across the window.
fn drag_overlay(ui: &mut egui::Ui, st: &UiState) {
    let Some((text, ok)) = &st.drag_hint else {
        return;
    };
    let screen = ui.ctx().content_rect();
    egui::Area::new(egui::Id::unique("drag-hint"))
        .order(egui::Order::Foreground)
        .fixed_pos(egui::pos2(screen.center().x - 200.0, screen.top() + 90.0))
        .show(ui.ctx(), |ui| {
            egui::Frame::popup(ui.style())
                .fill(if *ok {
                    Color32::from_rgb(0x1f, 0x4d, 0x7a)
                } else {
                    Color32::from_rgb(0x6a, 0x2a, 0x2a)
                })
                .show(ui, |ui| {
                    ui.set_width(400.0);
                    ui.vertical_centered(|ui| {
                        ui.add_space(8.0);
                        ui.heading(text);
                        ui.add_space(8.0);
                    });
                });
        });
}

/// The saved performance states: click one to bring it back; right-click for more.
fn snapshot_bar(ui: &mut egui::Ui, s: &Session, actions: &mut Vec<Action>) {
    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new("Snapshots").strong()).on_hover_text(
            "A snapshot remembers which slots play, their volumes, zones, effects on or off and SoundFont presets. Click one to bring it back; right-click to rename, update or remove it. They are saved in the rig file.",
        );
        let current = s.current_snapshot();
        for (n, snap) in s.snapshots().iter().enumerate() {
            let label = format!("{}  {}", n + 1, snap.name);
            let r = ui.selectable_label(current == Some(n), label);
            if r.clicked() {
                actions.push(Action::RecallSnapshot(n));
            }
            r.context_menu(|ui| {
                let mut name = snap.name.clone();
                if ui
                    .add(egui::TextEdit::singleline(&mut name).desired_width(160.0))
                    .changed()
                {
                    actions.push(Action::RenameSnapshot(n, name));
                }
                if ui.button("Update with what is set now").clicked() {
                    actions.push(Action::UpdateSnapshot(n));
                    ui.close();
                }
                if ui.button("Remove").clicked() {
                    actions.push(Action::RemoveSnapshot(n));
                    ui.close();
                }
            });
        }
        if ui
            .button("+ Save snapshot")
            .on_hover_text("Remember the current state")
            .clicked()
        {
            actions.push(Action::AddSnapshot);
        }
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
        if ui.button("Settings").clicked() {
            actions.push(Action::ToggleSettings);
        }
        if ui
            .button("Setlist")
            .on_hover_text("Songs and their parts, in playing order")
            .clicked()
        {
            actions.push(Action::ToggleSetlist);
        }
        if ui
            .button("Perform")
            .on_hover_text("Stage view: big buttons for snapshots, instruments and volume (F11 also, Escape leaves)")
            .clicked()
        {
            actions.push(Action::SetPerform(true));
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
            let mut master = s.rig.master_volume;
            if ui
                .add(
                    egui::Slider::new(&mut master, 0.0..=1.5)
                        .fixed_decimals(2)
                        .text("master"),
                )
                .changed()
            {
                actions.push(Action::SetMaster(master));
            }
            let limiting =
                f32::from_bits(s.engine.shared.limiter_reduction.swap(0, Ordering::Relaxed));
            if limiting > 0.5 {
                ui.label(
                    RichText::new(format!("LIMIT {limiting:.1} dB"))
                        .color(Color32::from_rgb(0xe0, 0x6c, 0x5a)),
                );
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
    if s.scanning() {
        let file = s
            .scan_current
            .as_ref()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        ui.label(
            RichText::new(format!("scanning plugins… {file}"))
                .small()
                .color(Color32::from_rgb(0xe0, 0xb4, 0x4a)),
        );
    } else if let Some(line) = s.log.last() {
        ui.label(RichText::new(line).small().weak());
    }
}

/// The slot's cost next to its meter: audio-thread share of the block, and for a Windows plugin
/// the CPU of its Wine host process. Turns orange above half a block, red close to dropouts.
fn slot_cpu_label(ui: &mut egui::Ui, c: crate::session::SlotCpu) {
    let pct = c.audio * 100.0;
    let colour = if c.audio_worst > 0.8 {
        Color32::from_rgb(0xe0, 0x6c, 0x5a)
    } else if c.audio_worst > 0.5 {
        Color32::from_rgb(0xe0, 0xa8, 0x4a)
    } else {
        Color32::from_gray(150)
    };
    let mut text = format!("{pct:>4.1}%");
    if let Some(w) = c.wine {
        text.push_str(&format!(" wine {w:.0}%"));
    }
    ui.label(RichText::new(text).monospace().size(11.5).color(colour))
        .on_hover_text(format!(
            "Audio thread: {:.1}% of the time available per block (worst recent block {:.0}%). \
             Above 100% the slot alone would cause a dropout.{}",
            pct,
            c.audio_worst * 100.0,
            match c.wine {
                Some(w) => format!(
                    "\nWine host process: {w:.0}% of one CPU core (the plugin's own threads and \
                     its editor), not part of the audio number."
                ),
                None => String::new(),
            }
        ));
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
            "{}  {}{}",
            r.name,
            match r.format {
                Format::Vst3 => "VST3",
                Format::SoundFont => "SF",
                _ => "CLAP",
            },
            if r.windows { "·WIN" } else { "" }
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
        (None, _) => match &s.loading[i] {
            Some((_, name)) => format!("{name}  (loading…)"),
            None => "— empty —".to_string(),
        },
    };
    let frame = egui::Frame::new()
        .fill(color.gamma_multiply(if selected { 0.30 } else { 0.12 }))
        .stroke(egui::Stroke::new(
            if selected { 1.5 } else { 0.5 },
            color.gamma_multiply(if selected { 1.0 } else { 0.5 }),
        ))
        .inner_margin(egui::Margin::symmetric(6, 2))
        .corner_radius(3);
    let unit_response = frame.show(ui, |ui| {
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
            // Level meter (not a switch): a flat bar that stays empty when there is no signal.
            let (meter, level) = ui.allocate_exact_size(egui::vec2(60.0, 8.0), egui::Sense::hover());
            ui.painter().rect_filled(meter, 1.0, Color32::from_gray(35));
            let fill = st.peaks_shown[i].clamp(0.0, 1.0).sqrt();
            if fill > 0.01 {
                let mut bar = meter;
                bar.set_width(meter.width() * fill);
                ui.painter().rect_filled(bar, 1.0, color);
            }
            level.on_hover_text("Output level of this slot");
            if loaded {
                slot_cpu_label(ui, s.slot_cpu[i]);
            }
            if loaded {
                if ui
                    .selectable_label(mute, "M")
                    .on_hover_text("Mute this slot")
                    .clicked()
                {
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
                    .on_hover_text("Slot volume (1.00 = unchanged). Drag to change.")
                    .changed()
                {
                    actions.push(Action::Volume(i, vol));
                }
                ui.label("zone")
                    .on_hover_text("Key range this slot plays, as MIDI notes (0-127; 60 = middle C). Notes outside it are ignored, so two slots can split the keyboard.");
                let a = ui.add(egui::DragValue::new(&mut lo).range(0..=127));
                ui.label("–");
                let b = ui.add(egui::DragValue::new(&mut hi).range(0..=127));
                if a.changed() || b.changed() {
                    actions.push(Action::Zone(i, lo, hi));
                }
                ui.label("tr")
                    .on_hover_text("Transpose in semitones (-48 to +48)");
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
                        let mut label = found_label(f);
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
    // A browser entry dragged onto this unit.
    let drop = &unit_response.response;
    if drop.dnd_hover_payload::<usize>().is_some() {
        ui.painter().rect_stroke(
            drop.rect,
            3,
            egui::Stroke::new(2.0, Color32::WHITE),
            egui::StrokeKind::Inside,
        );
    }
    if let Some(k) = drop.dnd_release_payload::<usize>() {
        actions.push(Action::DropLoad(i, *k));
    }
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

fn dock_bar(ui: &mut egui::Ui, s: &Session, st: &UiState, actions: &mut Vec<Action>) {
    ui.horizontal(|ui| {
        let Some(i) = s.viewing else {
            ui.label("EDITOR · nothing selected");
            return;
        };
        let mut tabs = vec![(DockTab::Plugin, "Plugin".to_owned())];
        for (k, ins) in s.rig.slots[i].inserts.iter().enumerate() {
            if let Some(ins) = ins {
                tabs.push((
                    DockTab::Insert(k),
                    format!("Insert {}: {}", k + 1, ins.plugin.name),
                ));
            }
        }
        tabs.push((DockTab::Effects, "Effects".to_owned()));
        tabs.push((DockTab::Macros, "Macros".to_owned()));
        for (tab, label) in tabs {
            if ui.selectable_label(st.tab == tab, label).clicked() {
                actions.push(Action::Tab(tab));
            }
        }
        ui.separator();
        if st.tab == DockTab::Effects {
            let fx = &s.rig.slots[i].fx;
            let on = [
                fx.eq.enabled,
                fx.comp.enabled,
                fx.delay.enabled,
                fx.reverb.enabled,
            ]
            .iter()
            .filter(|&&e| e)
            .count();
            ui.colored_label(COLORS[i], format!("EFFECTS · {} · {on}/4 on", i + 1));
            return;
        }
        if st.tab == DockTab::Macros {
            let used = s.macros(i).iter().filter(|m| !m.targets.is_empty()).count();
            ui.colored_label(COLORS[i], format!("MACROS · {} · {used}/8 in use", i + 1));
            return;
        }
        let name = match st.tab {
            DockTab::Insert(k) => s.rig.slots[i]
                .inserts
                .get(k)
                .and_then(Option::as_ref)
                .map(|n| n.plugin.name.as_str()),
            _ => s.rig.slots[i].instrument.as_ref().map(|r| r.name.as_str()),
        }
        .unwrap_or("");
        ui.colored_label(COLORS[i], format!("EDITOR · {} {name}", i + 1));
        let Some(info) = s.view_id.and_then(|id| s.editors.info(id)) else {
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

fn knob(
    ui: &mut egui::Ui,
    value: &mut f32,
    range: std::ops::RangeInclusive<f32>,
    name: &str,
    unit: &str,
    log: bool,
    decimals: usize,
) {
    ui.add(
        egui::Slider::new(value, range)
            .logarithmic(log)
            .fixed_decimals(decimals)
            .suffix(unit)
            .text(name),
    );
}

/// One effect "pedal": a framed section with an enable toggle.
fn pedal(
    ui: &mut egui::Ui,
    title: &str,
    color: Color32,
    enabled: &mut bool,
    body: impl FnOnce(&mut egui::Ui),
) {
    egui::Frame::new()
        .fill(color.gamma_multiply(if *enabled { 0.22 } else { 0.08 }))
        .stroke(egui::Stroke::new(
            1.0,
            color.gamma_multiply(if *enabled { 1.0 } else { 0.4 }),
        ))
        .inner_margin(egui::Margin::same(8))
        .corner_radius(4)
        .show(ui, |ui| {
            // Children of a wrapped row would inherit its horizontal layout.
            ui.vertical(|ui| {
                ui.set_width(285.0);
                ui.spacing_mut().slider_width = 100.0;
                ui.horizontal(|ui| {
                    ui.checkbox(enabled, "");
                    ui.label(RichText::new(title).strong().color(color));
                });
                ui.add_enabled_ui(*enabled, body);
            });
        });
}

/// The slot's built-in effects: EQ, compressor, delay, reverb, in signal order.
fn fx_panel(ui: &mut egui::Ui, s: &Session, i: usize, actions: &mut Vec<Action>) {
    inserts_section(ui, s, i, actions);
    let before = s.rig.slots[i].fx;
    let mut fx = before;
    let reduction = f32::from_bits(s.engine.shared.comp_reduction[i].load(Ordering::Relaxed));
    ui.add_space(6.0);
    egui::ScrollArea::vertical().show(ui, |ui| {
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = egui::vec2(10.0, 10.0);
            pedal(
                ui,
                "EQ",
                Color32::from_rgb(0x5a, 0xa9, 0xe0),
                &mut fx.eq.enabled,
                |ui| {
                    knob(ui, &mut fx.eq.low_db, -18.0..=18.0, "low", " dB", false, 1);
                    knob(
                        ui,
                        &mut fx.eq.low_hz,
                        20.0..=800.0,
                        "low freq",
                        " Hz",
                        true,
                        0,
                    );
                    knob(ui, &mut fx.eq.mid_db, -18.0..=18.0, "mid", " dB", false, 1);
                    knob(
                        ui,
                        &mut fx.eq.mid_hz,
                        100.0..=10000.0,
                        "mid freq",
                        " Hz",
                        true,
                        0,
                    );
                    knob(ui, &mut fx.eq.mid_q, 0.2..=8.0, "mid Q", "", true, 2);
                    knob(
                        ui,
                        &mut fx.eq.high_db,
                        -18.0..=18.0,
                        "high",
                        " dB",
                        false,
                        1,
                    );
                    knob(
                        ui,
                        &mut fx.eq.high_hz,
                        1500.0..=16000.0,
                        "high freq",
                        " Hz",
                        true,
                        0,
                    );
                },
            );
            pedal(
                ui,
                "COMPRESSOR",
                Color32::from_rgb(0xe0, 0xb4, 0x4a),
                &mut fx.comp.enabled,
                |ui| {
                    knob(
                        ui,
                        &mut fx.comp.threshold_db,
                        -60.0..=0.0,
                        "threshold",
                        " dB",
                        false,
                        1,
                    );
                    knob(ui, &mut fx.comp.ratio, 1.0..=20.0, "ratio", ":1", true, 1);
                    knob(
                        ui,
                        &mut fx.comp.attack_ms,
                        0.1..=200.0,
                        "attack",
                        " ms",
                        true,
                        1,
                    );
                    knob(
                        ui,
                        &mut fx.comp.release_ms,
                        5.0..=2000.0,
                        "release",
                        " ms",
                        true,
                        0,
                    );
                    knob(
                        ui,
                        &mut fx.comp.knee_db,
                        0.0..=24.0,
                        "knee",
                        " dB",
                        false,
                        1,
                    );
                    knob(
                        ui,
                        &mut fx.comp.makeup_db,
                        0.0..=24.0,
                        "makeup",
                        " dB",
                        false,
                        1,
                    );
                    ui.add(
                        egui::ProgressBar::new((reduction / 24.0).clamp(0.0, 1.0))
                            .text(format!("reduction {reduction:.1} dB")),
                    );
                },
            );
            pedal(
                ui,
                "DELAY",
                Color32::from_rgb(0x6c, 0xc9, 0x6a),
                &mut fx.delay.enabled,
                |ui| {
                    knob(
                        ui,
                        &mut fx.delay.time_ms,
                        1.0..=1500.0,
                        "time",
                        " ms",
                        true,
                        0,
                    );
                    knob(
                        ui,
                        &mut fx.delay.feedback,
                        0.0..=0.95,
                        "feedback",
                        "",
                        false,
                        2,
                    );
                    knob(ui, &mut fx.delay.mix, 0.0..=1.0, "mix", "", false, 2);
                    knob(
                        ui,
                        &mut fx.delay.damping_hz,
                        500.0..=18000.0,
                        "damping",
                        " Hz",
                        true,
                        0,
                    );
                    ui.checkbox(&mut fx.delay.ping_pong, "ping-pong");
                },
            );
            pedal(
                ui,
                "REVERB",
                Color32::from_rgb(0xb0, 0x7a, 0xe0),
                &mut fx.reverb.enabled,
                |ui| {
                    knob(ui, &mut fx.reverb.room, 0.0..=1.0, "room", "", false, 2);
                    knob(
                        ui,
                        &mut fx.reverb.damping,
                        0.0..=1.0,
                        "damping",
                        "",
                        false,
                        2,
                    );
                    knob(ui, &mut fx.reverb.width, 0.0..=1.0, "width", "", false, 2);
                    knob(ui, &mut fx.reverb.mix, 0.0..=1.0, "mix", "", false, 2);
                },
            );
        });
    });
    if fx != before {
        actions.push(Action::SetFx(i, fx));
    }
}

/// The slot's insert chain: effect plugins between the instrument and the built-in effects.
fn inserts_section(ui: &mut egui::Ui, s: &Session, i: usize, actions: &mut Vec<Action>) {
    ui.add_space(6.0);
    ui.label(
        RichText::new("INSERTS · plugin effects, in order, before the built-in effects").strong(),
    );
    for k in 0..merlin_model::MAX_INSERTS {
        ui.horizontal(|ui| {
            ui.label(format!("{}.", k + 1));
            match s.rig.slots[i].inserts.get(k).and_then(Option::as_ref) {
                Some(ins) => {
                    ui.label(&ins.plugin.name);
                    let mut on = !ins.bypass;
                    if ui
                        .checkbox(&mut on, "on")
                        .on_hover_text(
                            "Untick to bypass this effect (audio passes through untouched)",
                        )
                        .changed()
                    {
                        actions.push(Action::BypassInsert(i, k));
                    }
                    if ui.small_button("Edit").clicked() {
                        actions.push(Action::Tab(DockTab::Insert(k)));
                    }
                    if ui.small_button("Remove").clicked() {
                        actions.push(Action::UnloadInsert(i, k));
                    }
                }
                None => {
                    ui.menu_button("Load effect…", |ui| {
                        if s.effects.is_empty() {
                            ui.label("No effect plugins found");
                        }
                        egui::ScrollArea::vertical()
                            .max_height(300.0)
                            .show(ui, |ui| {
                                for (e, f) in s.effects.iter().enumerate() {
                                    if ui.button(found_label(f)).clicked() {
                                        actions.push(Action::LoadInsert(i, k, e));
                                        ui.close();
                                    }
                                }
                            });
                    });
                }
            }
        });
    }
    ui.separator();
}
