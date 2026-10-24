//! Drawing: header, rack, editor dock bar, keyboard map. Pure view code: it reads the
//! session and returns actions; the app applies them.

use std::path::PathBuf;
use std::sync::atomic::Ordering;

use egui::{Color32, RichText};

use merlin_model::{
    COMP_KNOBS, ControlAction, DELAY_KNOBS, EQ_KNOBS, FX_KNOBS, Format, FxKind, FxKnob, FxSettings,
    REVERB_KNOBS, SOUND_KNOBS, SamplerField,
};

use crate::browser;
use crate::hw;
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
        _ if paths::is_rig_file(path) => ("Drop to open this rig".into(), true),
        _ => ("This kind of file can't be used here".into(), false),
    }
}

/// How a plugin is named in the browser: `Name`, `Name (VST3)`, `Name (Windows)`.
pub fn found_label(f: &merlin_scanner::Found) -> String {
    let mut label = f.info.name.clone();
    if f.info.format == Format::Vst3 {
        label.push_str(" (VST3)");
    }
    if f.info.format == Format::Vst2 {
        label.push_str(" (VST2)");
    }
    if f.info.format == Format::Lv2 {
        label.push_str(" (LV2)");
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
    /// Set one of a SoundFont slot's sound-shaping fields (in the field's own units).
    SamplerField(usize, merlin_model::SamplerField, f32),
    SamplerTone(usize, merlin_hosting::SamplerTone),
    Transpose(usize, i8),
    Fit,
    Scroll(u32, u32),
    NoteOn(u8),
    NoteOff(u8),
    Panic,
    /// Save the last minutes of MIDI as a file.
    CaptureMidi,
    /// Minutes of MIDI the capture keeps (0: off).
    SetCaptureMinutes(u32),
    /// Where captures are saved (`None`: the default folder).
    SetCaptureDir(Option<PathBuf>),
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
    /// Open or close the effects drawer (it takes the place of the rack, except for the slot
    /// being edited).
    FxDrawer(bool),
    /// Start (true) or cancel (false) learning a plugin knob by touch: the user turns a knob in
    /// the plugin's window of the slot being edited, then moves a control on their device.
    TouchLearn(bool),
    /// As above for a given slot and insert (for the test script).
    TouchStart(usize, Option<usize>),
    /// Open or close the keyboard drawer.
    KeysDrawer(bool),
    /// Collapse the rack to the slot being edited (true) or show every slot (false).
    RackCollapsed(bool),
    /// Go to one of the app's views (the switch in the header).
    SetView(View),
    /// Script-only: show a settings tab.
    ShowSettingsTab(SettingsTab),
    AddDir(String),
    RemoveDir(usize),
    AddSoundDir(String),
    AddWindowsVst2Dir(String),
    RemoveWindowsVst2Dir(usize),
    RemoveSoundDir(usize),
    Rescan(bool),
    RetryFile(PathBuf),
    Unquarantine(PathBuf, String),
    SetBuffer(u32),
    RefreshMidi,
    /// Teach a control: the next one the user moves or presses is mapped to this.
    LearnControl(merlin_model::ControlAction),
    /// The same, from map mode, with a name to keep with the mapping.
    LearnTarget(merlin_model::ControlAction, Option<String>),
    /// Switch MIDI map mode on or off.
    SetMapMode(bool),
    /// Script-only: map a control to the first parameter of a plugin whose name has this text.
    MapParamNamed(merlin_model::SourceKind, usize, Option<usize>, String, bool),
    CancelLearn,
    RemoveMapping(usize),
    SetTakeover(usize, merlin_model::Takeover),
    SetSwitch(usize, merlin_model::Switch),
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
    /// Add the tag to browser entry `k`, or take it off if it has it.
    ToggleTag(usize, crate::tags::Tag),
    ClearTags(usize),
    /// Back to the scanner's automatic tags.
    ResetTags(usize),
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
    AddMacroParam(usize, usize, Option<usize>, merlin_hosting::ParamInfo),
    AddMacroSampler(usize, usize, merlin_model::SamplerField),
    RemoveMacroTarget(usize, usize, usize),
    SetMacroRange(usize, usize, usize, f32, f32),
    /// Script-only: make macro `k` of a slot drive the first parameter whose name contains the
    /// text (or `sampler:level`, `sampler:brightness`, ... for a SoundFont).
    AddMacroParamNamed(usize, usize, String),
    ToggleSetlist,
    AddMarker(usize, merlin_model::Marker),
    RemoveMarker(usize, usize),
    EditMarker(usize, usize, merlin_model::Marker),
    AddTrack(usize, PathBuf),
    RemoveTrack(usize, usize),
    EditTrack(usize, usize, crate::session::TrackEdit),
    TransportToggle,
    TransportStart,
    TransportStop,
    SetClickState(bool),
    ToggleClick,
    SetClickVolume(f32),
    TapTempo,
    SetTempo(f32),
    SetCountIn(u8),
    SetBeatsPerBar(u8),
    SetClickOutput(Option<[String; 2]>),
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

/// The app's views: one is on screen at a time, chosen with the switch in the header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum View {
    Rack,
    Setlist,
    /// The stage view.
    Perform,
    Settings,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DockTab {
    Plugin,
    /// The editor of insert `n`.
    Insert(usize),
    Macros,
    /// Every parameter of the instrument and its inserts, to map to MIDI controls.
    Params,
}

/// UI state that is not part of the session.
pub struct UiState {
    pub tab: DockTab,
    pub settings_open: bool,
    /// The Setlist screen is showing, and the song being edited in it.
    pub setlist_open: bool,
    /// The path being typed for a new backing track.
    pub track_text: String,
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
    /// The Parameters tab: which plugin it lists (`None` = the instrument) and its search box.
    pub param_insert: Option<usize>,
    pub param_query: String,
    pub favs_only: bool,
    /// The effects drawer is open.
    pub fx_open: bool,
    /// The rack shows only the slot being edited.
    pub rack_collapsed: bool,
    /// How tall the pedals' area of the effects drawer is, in points.
    pub fx_height: f32,
    /// How many lines the app had said when the notice area last looked, and when it did.
    pub notice_count: u64,
    pub notice_at: Option<std::time::Instant>,
    /// The keyboard drawer is open.
    pub keys_open: bool,
    /// The browser shows only plugins with this tag (or none yet).
    pub tag_filter: crate::tags::Filter,
    pub dir_text: String,
    pub sound_dir_text: String,
    pub vst2_dir_text: String,
    pub wine_text: String,
    pub yab_text: String,
    pub installer_text: String,
    /// The folder being typed for MIDI captures.
    pub capture_dir_text: String,
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
    pub fn view(&self) -> View {
        if self.perform {
            View::Perform
        } else if self.settings_open {
            View::Settings
        } else if self.setlist_open {
            View::Setlist
        } else {
            View::Rack
        }
    }

    pub fn new() -> Self {
        Self {
            tab: DockTab::Plugin,
            settings_open: false,
            setlist_open: false,
            track_text: String::new(),
            edit_song: 0,
            perform: false,
            locked: false,
            unlock_since: None,
            settings_tab: SettingsTab::Plugins,
            // Test hook: start with this in the browser's search box.
            query: std::env::var("MERLIN_QUERY").unwrap_or_default(),
            preset_query: String::new(),
            macro_query: String::new(),
            favs_only: false,
            fx_open: false,
            rack_collapsed: false,
            fx_height: 370.0,
            notice_count: 0,
            notice_at: None,
            keys_open: true,
            tag_filter: Default::default(),
            dir_text: String::new(),
            sound_dir_text: String::new(),
            vst2_dir_text: String::new(),
            wine_text: String::new(),
            yab_text: String::new(),
            installer_text: String::new(),
            capture_dir_text: String::new(),
            param_insert: None,
            param_query: String::new(),
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
    if ui.input(|i| i.modifiers.command && i.key_pressed(egui::Key::M)) {
        actions.push(Action::SetMapMode(!s.map_mode));
    }
    if s.map_mode && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
        actions.push(Action::SetMapMode(false));
    }
    if ui.input(|i| i.key_pressed(egui::Key::F11)) && !(st.perform && st.locked) {
        actions.push(Action::SetPerform(!st.perform));
    }
    egui::Panel::top("header")
        .frame(hw::chassis())
        .show(ui, |ui| header(ui, s, st, actions));
    egui::Panel::top("notice")
        .frame(egui::Frame::new().inner_margin(egui::Margin::symmetric(10, 4)))
        .show(ui, |ui| notice_bar(ui, s, st));
    if st.perform {
        *dock_rect = egui::Rect::NOTHING;
        crate::perform::show(ui, s, st, actions);
        return;
    }
    // Scenes belong to the rack and the setlist, not to the settings.
    if st.view() != View::Settings {
        egui::Panel::top("scenes")
            .frame(hw::chassis())
            .show(ui, |ui| scenes_bar(ui, s, actions));
    }
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
        // With the effects drawer open, or the rack collapsed, only the slot being edited shows.
        let drawer = st.fx_open && s.viewing.is_some();
        let collapsed = st.rack_collapsed || drawer;
        let in_use = (0..UI_SLOTS).filter(|&i| s.plugins[i].is_some()).count();
        let handle = hw::drawer_handle(
            ui,
            "Rack",
            &format!("{in_use} of {UI_SLOTS} slots in use"),
            !collapsed,
        )
        .on_hover_text(if collapsed {
            "Show every slot"
        } else {
            "Fold the rack down to the slot you are editing, to make room"
        });
        if handle.clicked() {
            actions.push(Action::RackCollapsed(!collapsed));
        }
        for i in 0..UI_SLOTS {
            if !collapsed || s.viewing == Some(i) {
                unit(ui, s, st, i, actions);
            }
        }
        if let Some(i) = s.viewing {
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
            let handle = hw::drawer_handle(
                ui,
                "Effects",
                &format!("slot {} \u{b7} {on} of 4 on", i + 1),
                drawer,
            )
            .on_hover_text(if drawer {
                "Close the effects drawer"
            } else {
                "Open the effects drawer: pedals for this slot, with the rest of the rack folded away"
            });
            if handle.clicked() {
                actions.push(Action::FxDrawer(!st.fx_open));
            }
            if drawer {
                fx_drawer(ui, s, i, &mut st.fx_height, actions);
            }
        }
    });
    // The keyboard is switched on and off from the header and takes no room while it is off.
    if st.keys_open {
        egui::Panel::bottom("keys").show(ui, |ui| keys(ui, s, st, actions));
    }
    egui::Panel::top("dockbar").show(ui, |ui| dock_bar(ui, s, st, actions));
    egui::CentralPanel::default().show(ui, |ui| {
        *dock_rect = ui.available_rect_before_wrap();
        if st.tab == DockTab::Params
            && let Some(i) = s.viewing
        {
            crate::param_panel::show(ui, s, st, i, actions);
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

/// How a sound-shaping field's value reads under its knob.
fn sound_readout(field: SamplerField, v: f32) -> String {
    match field {
        SamplerField::Level => format!("{v:.2}"),
        SamplerField::Attack | SamplerField::Release => format!("x{:.2}", 2f32.powf(v)),
        SamplerField::Brightness => format!("{v:+.1} oct"),
        SamplerField::Resonance => format!("{v:.0} dB"),
        SamplerField::Tuning => format!("{v:+.0} ct"),
        SamplerField::Voices => format!("{v:.0}"),
    }
}

/// A SoundFont slot's "Shape the sound" rack unit: one knob per field, each mappable to MIDI.
fn sound_rack(ui: &mut egui::Ui, s: &Session, i: usize, actions: &mut Vec<Action>) {
    let rest = merlin_hosting::SamplerTone::default();
    hw::rack(ui, "Shape the sound", &format!("slot {}", i + 1), |ui| {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            for (n, field) in SamplerField::ALL.into_iter().enumerate() {
                let (lo, hi) = field.range();
                let at = |v: f32| ((v - lo) / (hi - lo)).clamp(0.0, 1.0);
                let default = match field {
                    SamplerField::Level => 1.0,
                    SamplerField::Attack => rest.attack,
                    SamplerField::Release => rest.release,
                    SamplerField::Brightness => rest.cutoff,
                    SamplerField::Resonance => rest.resonance,
                    SamplerField::Tuning => rest.fine_tune,
                    SamplerField::Voices => f32::from(rest.polyphony),
                };
                let mut position = at(s.sampler_field(i, field).unwrap_or(default));
                // Fields that go both ways around their resting point light up from there.
                let origin = if lo < 0.0 { at(0.0) } else { 0.0 };
                let r = hw::knob(
                    ui,
                    &mut position,
                    field.name(),
                    &|p| sound_readout(field, lo + (hi - lo) * p),
                    at(default),
                    origin,
                    COLORS[i],
                );
                if r.changed() {
                    actions.push(Action::SamplerField(i, field, lo + (hi - lo) * position));
                }
                crate::map_mode::target(
                    ui,
                    s,
                    &r,
                    ControlAction::SlotParam {
                        slot: i as u8,
                        insert: SOUND_KNOBS,
                        id: n as u32,
                    },
                    Some(field.name()),
                    actions,
                );
            }
            ui.add_space(8.0);
            if hw::Cap::new("RESET")
                .show(ui)
                .on_hover_text("Put the attack, release, brightness, resonance, tuning and voices back as the SoundFont made them")
                .clicked()
            {
                actions.push(Action::SamplerTone(i, rest));
            }
        });
    });
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
        let now = presets
            .iter()
            .find(|p| (p.bank, p.program) == current)
            .map_or("", |p| p.name.as_str());
        ui.add_space(12.0);
        ui.colored_label(COLORS[i], format!("now: {now}"));
    });
    if s.sampler_tone(i).is_some() {
        sound_rack(ui, s, i, actions);
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
/// Scenes (what the code calls snapshots): saved rack set-ups to recall with a click or a pedal.
fn scenes_bar(ui: &mut egui::Ui, s: &mut Session, actions: &mut Vec<Action>) {
    ui.horizontal_wrapped(|ui| {
        hw::engraved(ui, "Scenes").on_hover_text(SCENES_HELP);
        let current = s.current_snapshot();
        for (n, snap) in s.snapshots().iter().enumerate() {
            let label = format!("{}  {}", n + 1, snap.name);
            let r = hw::Cap::new(label)
                .lit((current == Some(n)).then_some(hw::AMBER))
                .min_width(70.0)
                .show(ui)
                .on_hover_text("Click to bring this scene back. Right-click to rename, update or remove it.");
            if r.clicked() && !s.map_mode {
                actions.push(Action::RecallSnapshot(n));
            }
            crate::map_mode::target(
                ui,
                s,
                &r,
                ControlAction::RecallSnapshot { snapshot: n as u8 },
                None,
                actions,
            );
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
        if hw::Cap::new("+ Store scene")
            .show(ui)
            .on_hover_text("Remember the rack as it is now: which instruments play, their volumes, zones, effects and SoundFont sounds")
            .clicked()
        {
            actions.push(Action::AddSnapshot);
        }
        if s.snapshots().is_empty() {
            ui.label(
                RichText::new("A scene is a saved rack set-up, to recall in one click or with a foot switch. Set the rack up, then press Store scene.")
                    .small()
                    .weak(),
            );
        }
    });
}

const SCENES_HELP: &str = "A scene remembers which slots play, their volumes, zones, effects on or off and SoundFont sounds. Click one to bring it back (or map it to a pedal). Right-click to rename, update or remove it. Scenes are saved in the rig file.";

fn header(ui: &mut egui::Ui, s: &mut Session, st: &mut UiState, actions: &mut Vec<Action>) {
    let perform = st.perform;
    // A locked stage view can only be left by holding Unlock.
    let locked = perform && st.locked;
    ui.horizontal(|ui| {
        ui.label(RichText::new("MERLIN").size(18.0).strong().color(hw::AMBER));
        ui.add_space(6.0);
        if perform {
            // No New / Open / Save on stage: only the name of what is loaded.
            hw::plate().show(ui, |ui| {
                ui.label(
                    RichText::new(&s.rig.name)
                        .size(15.0)
                        .strong()
                        .color(hw::TEXT),
                );
            });
        } else {
            file_plate(ui, s, actions);
        }
        ui.add_space(10.0);
        let views = [View::Rack, View::Setlist, View::Perform, View::Settings];
        let names = ["RACK", "SETLIST", "PERFORM", "SETTINGS"];
        let now = views.iter().position(|v| *v == st.view()).unwrap_or(0);
        ui.add_enabled_ui(!locked, |ui| {
            if let Some(n) = hw::selector(ui, &names, now, hw::BLUE) {
                actions.push(Action::SetView(views[n]));
            }
        });
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let p = hw::Cap::new("PANIC")
                .base(Color32::from_rgb(0x9a, 0x2a, 0x2a))
                .min_width(70.0)
                .show(ui)
                .on_hover_text("Silences everything: all notes off on every slot");
            if p.clicked() && !s.map_mode {
                actions.push(Action::Panic);
            }
            crate::map_mode::target(ui, s, &p, ControlAction::Panic, None, actions);
            let mut master = s.rig.master_volume;
            ui.spacing_mut().slider_width = 80.0;
            let m = ui.add(
                egui::Slider::new(&mut master, 0.0..=1.5)
                    .fixed_decimals(2)
                    .text("master"),
            );
            if m.changed() {
                actions.push(Action::SetMaster(master));
            }
            crate::map_mode::target(ui, s, &m, ControlAction::MasterVolume, None, actions);
            ui.add_space(6.0);
            capture_button(ui, s, actions);
            if !locked {
                crate::map_mode::toggle(ui, s, actions);
            }
            if perform {
                crate::perform::lock_cap(ui, st, actions);
                if hw::Cap::new("FULL SCREEN").show(ui).clicked() {
                    actions.push(Action::ToggleFullscreen);
                }
            } else {
                let open = st.keys_open;
                if hw::Cap::new("")
                    .icon(hw::Icon::Keyboard)
                    .lit(open.then_some(hw::AMBER))
                    .show(ui)
                    .on_hover_text(if open {
                        "Hide the on-screen keyboard"
                    } else {
                        "Show the on-screen keyboard"
                    })
                    .clicked()
                {
                    actions.push(Action::KeysDrawer(!open));
                }
            }
        });
    });
}

/// How a line in the notice area looks.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Notice {
    /// Nothing to say.
    Idle,
    Info,
    Success,
    /// The app is waiting for the user to do something.
    Prompt,
    Error,
}

/// What the app says to the user right now: what it is waiting for, else what is going on, else
/// the last thing it did while that is still fresh.
fn notice_text(s: &Session, st: &mut UiState, ctx: &egui::Context) -> (String, Notice) {
    if s.touch_learning().is_some() {
        return (
            "Turn a knob in the plugin's own window\u{2026}".into(),
            Notice::Prompt,
        );
    }
    if s.learning().is_some() {
        return (
            match s.learn_label() {
                Some(name) => {
                    format!("Now move a control on your device to map \u{201c}{name}\u{201d}")
                }
                None => "Move a control on your device to map it".into(),
            },
            Notice::Prompt,
        );
    }
    if s.scanning() {
        let file = s
            .scan_current
            .as_ref()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        return (format!("Scanning plugins\u{2026} {file}"), Notice::Info);
    }
    if let Some(note) = s.capture_notice() {
        return (note.to_string(), Notice::Success);
    }
    if s.log_count != st.notice_count {
        st.notice_count = s.log_count;
        st.notice_at = Some(std::time::Instant::now());
    }
    let fresh = st
        .notice_at
        .is_some_and(|t| t.elapsed() < std::time::Duration::from_secs(12));
    match s.log.last() {
        Some(line) if fresh => {
            ctx.request_repaint_after(std::time::Duration::from_secs(1));
            let l = line.to_lowercase();
            let kind = if l.starts_with("error") || l.contains("could not") || l.contains("failed")
            {
                Notice::Error
            } else {
                Notice::Info
            };
            (line.clone(), kind)
        }
        _ => ("Ready".into(), Notice::Idle),
    }
}

/// The strip under the header where the app talks back: prompts, progress, results and errors,
/// in a frame of their own, with the engine's numbers at the far end.
fn notice_bar(ui: &mut egui::Ui, s: &Session, st: &mut UiState) {
    let (text, kind) = notice_text(s, st, ui.ctx());
    let (tint, colour) = match kind {
        Notice::Idle => (hw::WELL, hw::TEXT_DIM),
        Notice::Info => (hw::WELL, hw::TEXT),
        Notice::Success => (hw::mix(hw::WELL, hw::GREEN, 0.14), hw::GREEN),
        Notice::Prompt => (hw::mix(hw::WELL, hw::AMBER, 0.20), hw::AMBER),
        Notice::Error => (
            hw::mix(hw::WELL, hw::RED, 0.22),
            hw::mix(hw::RED, Color32::WHITE, 0.35),
        ),
    };
    let edge = if kind == Notice::Idle {
        hw::PLATE
    } else {
        colour
    };
    egui::Frame::new()
        .fill(tint)
        .stroke(egui::Stroke::new(1.0, edge))
        .corner_radius(6)
        .inner_margin(egui::Margin::symmetric(10, 4))
        .show(ui, |ui| {
            ui.set_min_height(22.0);
            ui.horizontal(|ui| {
                let message = RichText::new(text).size(14.5).color(colour);
                ui.label(if kind == Notice::Prompt {
                    message.strong()
                } else {
                    message
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
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
                    // The UI's own cost is for development: it goes first when the row is narrow.
                    let mut line = format!(
                        "{buf} @ {} kHz \u{b7} DSP {dsp:.1}% \u{b7} xruns {xruns}",
                        s.sample_rate / 1000
                    );
                    let room = ui.available_width();
                    if room > 700.0 {
                        line.push_str(&format!(
                            " \u{b7} UI {:.1}% \u{b7} {:.0} fps",
                            st.status.cpu_self, st.status.frames_per_s
                        ));
                    } else if room < 380.0 {
                        line = format!("DSP {dsp:.0}% \u{b7} xruns {xruns}");
                    }
                    ui.label(RichText::new(line).size(11.0).color(hw::TEXT_DIM));
                    let limiting = f32::from_bits(
                        s.engine.shared.limiter_reduction.swap(0, Ordering::Relaxed),
                    );
                    if limiting > 0.5 {
                        ui.label(
                            RichText::new(format!("LIMIT {limiting:.1} dB"))
                                .size(11.0)
                                .color(Color32::from_rgb(0xe0, 0x6c, 0x5a)),
                        );
                    }
                });
            });
        });
}

/// The rig file's controls together: its name, and New, Open, Save beside it.
fn file_plate(ui: &mut egui::Ui, s: &Session, actions: &mut Vec<Action>) {
    hw::plate().show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 5.0;
            hw::engraved(ui, "Rig");
            let mut name = s.rig.name.clone();
            if ui
                .add(egui::TextEdit::singleline(&mut name).desired_width(120.0))
                .changed()
            {
                actions.push(Action::RenameRig(name));
            }
            if hw::Cap::new("New").show(ui).clicked() {
                actions.push(Action::NewRig);
            }
            let open = hw::Cap::new("Open…").show(ui);
            egui::Popup::menu(&open).show(|ui| {
                let mut files: Vec<PathBuf> = std::fs::read_dir(paths::rigs_dir())
                    .into_iter()
                    .flatten()
                    .flatten()
                    .map(|e| e.path())
                    .filter(|p| paths::is_rig_file(p))
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
            // Lit while there is something to save.
            let save = hw::Cap::new("Save")
                .lit(s.dirty.then_some(hw::AMBER))
                .show(ui)
                .on_hover_text(if s.dirty {
                    "The rig has changes that are not saved"
                } else {
                    "Save the rig"
                });
            if save.clicked() {
                actions.push(Action::SaveRig);
            }
        });
    });
}

/// "Capture MIDI": saves the last minutes of what was played (the app is always listening).
fn capture_button(ui: &mut egui::Ui, s: &Session, actions: &mut Vec<Action>) {
    let minutes = s.capture_minutes();
    let (listened, kept) = s.capture_status();
    let r = ui
        .add_enabled_ui(minutes > 0, |ui| hw::Cap::new("CAPTURE MIDI").show(ui))
        .inner
        .on_hover_text(if minutes == 0 {
            "MIDI capture is off (Settings > MIDI)".to_owned()
        } else {
            format!(
                "Save the last {minutes} minutes of what you played as a MIDI file. The app is always listening: {} so far, {kept} messages kept.",
                crate::session::clock(listened)
            )
        });
    crate::map_mode::target(ui, s, &r, ControlAction::CaptureMidi, None, actions);
    if r.clicked() && !s.map_mode {
        actions.push(Action::CaptureMidi);
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
                Format::Vst2 => "VST2",
                Format::Lv2 => "LV2",
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
            if loaded && r.clicked() && !s.map_mode {
                actions.push(Action::Select(i, ui.input(|inp| inp.modifiers.shift)));
            }
            if loaded {
                let a = ControlAction::SelectSlot { slot: i as u8 };
                crate::map_mode::target(ui, s, &r, a, None, actions);
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
                let m = ui
                    .selectable_label(mute, "M")
                    .on_hover_text("Mute this slot");
                if m.clicked() && !s.map_mode {
                    actions.push(Action::Mute(i));
                }
                crate::map_mode::target(
                    ui,
                    s,
                    &m,
                    ControlAction::ToggleMute { slot: i as u8 },
                    None,
                    actions,
                );
                ui.label("vol");
                let v = ui
                    .add(
                        egui::DragValue::new(&mut vol)
                            .range(0.0..=2.0)
                            .speed(0.01)
                            .fixed_decimals(2),
                    )
                    .on_hover_text("Slot volume (1.00 = unchanged). Drag to change.");
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

/// "MIDI LEARN" for the plugin in the dock: click, turn a knob in the plugin's window, then move
/// a control on the device. The app finds the knob by watching which parameter moved.
fn touch_learn_cap(ui: &mut egui::Ui, s: &Session, actions: &mut Vec<Action>) {
    // What to do next is said in the notice area; the button only shows that it is on.
    let touching = s.touch_learning().is_some();
    let waiting = matches!(s.learning(), Some(ControlAction::SlotParam { .. }));
    let active = touching || waiting;
    let r = hw::Cap::new("MIDI LEARN")
        .lit(active.then_some(hw::AMBER))
        .show(ui)
        .on_hover_text(
        "Map a knob of this plugin to a control on your device: click here, turn the knob in the plugin's own window, then move the control. Click again to cancel.",
    );
    if r.clicked() {
        actions.push(Action::TouchLearn(!active));
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
        tabs.push((DockTab::Macros, "Macros".to_owned()));
        tabs.push((DockTab::Params, "Parameter list".to_owned()));
        for (tab, label) in tabs {
            if ui.selectable_label(st.tab == tab, label).clicked() {
                actions.push(Action::Tab(tab));
            }
        }
        ui.separator();
        if matches!(st.tab, DockTab::Plugin | DockTab::Insert(_)) {
            touch_learn_cap(ui, s, actions);
        }
        if st.tab == DockTab::Params {
            ui.colored_label(COLORS[i], format!("PARAMETERS · {}", i + 1));
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

/// The knobs of one built-in effect, drawn from the shared table. In map mode each is something a
/// MIDI control can be mapped to.
fn fx_knobs(
    ui: &mut egui::Ui,
    s: &Session,
    slot: usize,
    fx: &mut FxSettings,
    knobs: &[FxKnob],
    accent: Color32,
    actions: &mut Vec<Action>,
) {
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = egui::vec2(4.0, 4.0);
        for k in knobs {
            let (Some(now), Some(rest)) =
                (fx.knob_value(k.id), FxSettings::default().knob_value(k.id))
            else {
                continue;
            };
            let mut position = k.position(now);
            // Knobs that go both ways light up from the middle.
            let origin = if k.min < 0.0 && k.max > 0.0 {
                k.position(0.0)
            } else {
                0.0
            };
            let r = hw::knob(
                ui,
                &mut position,
                k.name,
                &|p| k.show(k.value(p)),
                k.position(rest),
                origin,
                accent,
            );
            if r.changed() {
                fx.set_knob_position(k.id, position);
            }
            crate::map_mode::target(
                ui,
                s,
                &r,
                ControlAction::SlotParam {
                    slot: slot as u8,
                    insert: FX_KNOBS,
                    id: k.id,
                },
                Some(&format!("{} {}", k.group, k.name)),
                actions,
            );
        }
    });
}

/// The effects drawer: which slot it is for, its plugin inserts, and the built-in effects as four
/// pedals in signal order (EQ, compressor, delay, reverb).
fn fx_drawer(
    ui: &mut egui::Ui,
    s: &Session,
    i: usize,
    height: &mut f32,
    actions: &mut Vec<Action>,
) {
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        hw::engraved(ui, "Slot");
        for k in (0..UI_SLOTS).filter(|&k| s.plugins[k].is_some()) {
            let lit = (k == i).then_some(COLORS[k]);
            if hw::Cap::new((k + 1).to_string())
                .lit(lit)
                .min_width(30.0)
                .show(ui)
                .on_hover_text(
                    s.rig.slots[k]
                        .instrument
                        .as_ref()
                        .map_or(String::new(), |r| r.name.clone()),
                )
                .clicked()
            {
                actions.push(Action::Select(k, false));
            }
        }
    });
    inserts_strip(ui, s, i, actions);
    let before = s.rig.slots[i].fx;
    let mut fx = before;
    let reduction = f32::from_bits(s.engine.shared.comp_reduction[i].load(Ordering::Relaxed));
    ui.add_space(4.0);
    // The pedals have a height of their own, set by the strip below them; what does not fit
    // scrolls. (The height is given, not measured: a scroll area cannot grow to fit its content
    // inside a panel that sizes itself to what is in it.)
    let h = *height;
    ui.allocate_ui(egui::vec2(ui.available_width(), h), |ui| {
        ui.set_min_height(h);
        egui::ScrollArea::both()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing = egui::vec2(14.0, 14.0);
                    let blue = Color32::from_rgb(0x5a, 0xa9, 0xe0);
                    let mut on = fx.eq.enabled;
                    let switch = hw::pedal(ui, "EQ", blue, &mut on, |ui| {
                        fx_knobs(ui, s, i, &mut fx, &EQ_KNOBS, blue, actions);
                    });
                    fx.eq.enabled = on;
                    fx_switch_target(ui, s, &switch, i, FxKind::Eq, actions);

                    let amber = hw::AMBER;
                    let mut on = fx.comp.enabled;
                    let switch = hw::pedal(ui, "COMPRESSOR", amber, &mut on, |ui| {
                        fx_knobs(ui, s, i, &mut fx, &COMP_KNOBS, amber, actions);
                        hw::meter(ui, reduction / 24.0, amber, "GAIN REDUCTION");
                    });
                    fx.comp.enabled = on;
                    fx_switch_target(ui, s, &switch, i, FxKind::Comp, actions);

                    let green = hw::GREEN;
                    let mut on = fx.delay.enabled;
                    let switch = hw::pedal(ui, "DELAY", green, &mut on, |ui| {
                        fx_knobs(ui, s, i, &mut fx, &DELAY_KNOBS, green, actions);
                        let ping_pong = fx.delay.ping_pong;
                        if hw::Cap::new("PING-PONG")
                            .lit(ping_pong.then_some(green))
                            .show(ui)
                            .clicked()
                        {
                            fx.delay.ping_pong = !ping_pong;
                        }
                    });
                    fx.delay.enabled = on;
                    fx_switch_target(ui, s, &switch, i, FxKind::Delay, actions);

                    let purple = Color32::from_rgb(0xb0, 0x7a, 0xe0);
                    let mut on = fx.reverb.enabled;
                    let switch = hw::pedal(ui, "REVERB", purple, &mut on, |ui| {
                        fx_knobs(ui, s, i, &mut fx, &REVERB_KNOBS, purple, actions);
                    });
                    fx.reverb.enabled = on;
                    fx_switch_target(ui, s, &switch, i, FxKind::Reverb, actions);
                });
            });
    });
    *height = (h + hw::grip(ui)).clamp(140.0, 800.0);
    if fx != before {
        actions.push(Action::SetFx(i, fx));
    }
}

/// Marks a pedal's footswitch as something a MIDI control can be mapped to.
fn fx_switch_target(
    ui: &mut egui::Ui,
    s: &Session,
    switch: &egui::Response,
    slot: usize,
    fx: FxKind,
    actions: &mut Vec<Action>,
) {
    crate::map_mode::target(
        ui,
        s,
        switch,
        ControlAction::ToggleFx {
            slot: slot as u8,
            fx,
        },
        None,
        actions,
    );
}

/// The slot's insert chain, in order: effect plugins between the instrument and the built-in
/// effects.
fn inserts_strip(ui: &mut egui::Ui, s: &Session, i: usize, actions: &mut Vec<Action>) {
    ui.horizontal_wrapped(|ui| {
        hw::engraved(ui, "Inserts").on_hover_text(
            "Plugin effects, in order, between the instrument and the built-in effects",
        );
        for k in 0..merlin_model::MAX_INSERTS {
            hw::plate().show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 5.0;
                    hw::engraved(ui, &(k + 1).to_string());
                    match s.rig.slots[i].inserts.get(k).and_then(Option::as_ref) {
                        Some(ins) => {
                            ui.label(&ins.plugin.name);
                            let on = hw::Cap::new("ON")
                                .lit((!ins.bypass).then_some(hw::GREEN))
                                .show(ui)
                                .on_hover_text(
                                    "Lit: the effect is on. Click to bypass it (the audio passes through untouched)",
                                );
                            if on.clicked() && !s.map_mode {
                                actions.push(Action::BypassInsert(i, k));
                            }
                            crate::map_mode::target(
                                ui,
                                s,
                                &on,
                                ControlAction::ToggleInsertBypass {
                                    slot: i as u8,
                                    insert: k as u8,
                                },
                                None,
                                actions,
                            );
                            if hw::Cap::new("EDIT").show(ui).clicked() {
                                actions.push(Action::Tab(DockTab::Insert(k)));
                            }
                            if hw::Cap::new("REMOVE").show(ui).clicked() {
                                actions.push(Action::UnloadInsert(i, k));
                            }
                        }
                        None => {
                            let load = hw::Cap::new("+ Load effect").show(ui);
                            egui::Popup::menu(&load).show(|ui| {
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
            });
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_view_follows_the_flags_with_the_stage_view_first() {
        let mut st = UiState::new();
        assert_eq!(st.view(), View::Rack);
        st.setlist_open = true;
        assert_eq!(st.view(), View::Setlist);
        st.settings_open = true;
        assert_eq!(
            st.view(),
            View::Settings,
            "settings is drawn over the setlist"
        );
        st.perform = true;
        assert_eq!(st.view(), View::Perform);
    }
}
