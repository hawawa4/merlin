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
        // A folder of SoundFonts plays as one instrument.
        label.push_str(if f.path.is_dir() {
            " (SoundFonts)"
        } else {
            " (SoundFont)"
        });
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
    /// Move an editor (by editor id) out into a window of its own, or back into the dock.
    PopOut(usize),
    PopIn(usize),
    /// Scale a floating editor's window (editor id, factor of its native size).
    FloatScale(usize, f32),
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
    /// The folder the Setlist view browses for backing tracks.
    SetBackingDir(Option<PathBuf>),
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
    /// Fit plugin editors to the dock automatically (or not).
    AutoFit(bool),
    /// Open or fold the browser.
    BrowserOpen(bool),
    /// Collapse the rack to the slot being edited (true) or show every slot (false).
    RackCollapsed(bool),
    /// Go to one of the app's views (the switch in the header).
    SetView(View),
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
    /// Take browser entry `k` out of the library for good.
    HidePlugin(usize),
    /// Bring a hidden plugin back (its favourite key).
    UnhidePlugin(String),
    /// Never load, list or report this plugin file again.
    IgnoreFile(PathBuf),
    UnignoreFile(PathBuf),
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
    /// Macro `k` of a slot drives the built-in effect knob with this id.
    AddMacroFx(usize, usize, u32),
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
    SetMetronome(bool),
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
    /// Resize a plugin's editor to fill the dock whenever the plugin allows it.
    pub auto_fit: bool,
    /// The editor and dock size (pixels) the last automatic fit was asked for, so a plugin that
    /// answers with another size is not asked again and again.
    pub last_fit: Option<(usize, u32, u32)>,
    /// A fit that waits for the dock to stop changing size: what it is for and since when.
    pub fit_wait: Option<((usize, u32, u32), std::time::Instant)>,
    /// The browser is open (else a narrow strip).
    pub browser_open: bool,
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
    /// The scene whose name is being edited in the strip, and the text so far.
    pub scene_edit: Option<(usize, String)>,
    /// The header's level meter.
    pub meter: hw::Meter,
    /// The backing-track library shown in the Setlist view.
    pub library: crate::library::Browsing,
    /// The file dialog that is open, if one is.
    pub picking: Option<crate::dialog::Open>,
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
            auto_fit: false,
            last_fit: None,
            fit_wait: None,
            browser_open: true,
            rack_collapsed: false,
            fx_height: 370.0,
            notice_count: 0,
            notice_at: None,
            keys_open: false,
            tag_filter: Default::default(),
            wine_text: String::new(),
            yab_text: String::new(),
            installer_text: String::new(),
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
            scene_edit: None,
            meter: hw::Meter::new(),
            library: Default::default(),
            picking: None,
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
    poll_dialog(ui, s, st, actions);
    // The level meter: fed every frame, and the frames keep coming while it is still moving.
    let (now, dt) = ui.input(|i| (i.time, i.unstable_dt.min(0.1)));
    st.meter.feed(s.take_meter(), now, dt);
    if st.meter.moving(now) {
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(33));
    }
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
        .show(ui, |ui| notice_bar(ui, s, st, actions));
    if st.perform {
        *dock_rect = egui::Rect::NOTHING;
        crate::perform::show(ui, s, st, actions);
        return;
    }
    // Scenes are made in the rack; the setlist only picks among them.
    if st.view() == View::Rack {
        egui::Panel::top("scenes")
            .frame(hw::chassis())
            .show(ui, |ui| scenes_bar(ui, s, st, actions));
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
        let avail = ui.available_rect_before_wrap();
        *dock_rect = avail;
        // A plugin's own window gets the room that is left once its scroll bars are placed.
        if matches!(st.tab, DockTab::Plugin | DockTab::Insert(_))
            && let Some(id) = s.view_id
            && s.editors.shown() == Some(id)
            && let Some(info) = s.editors.info(id)
        {
            *dock_rect = editor_view(ui, s, st, id, info, avail, actions);
        }
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
        if let Some(id) = s.view_id
            && matches!(st.tab, DockTab::Plugin | DockTab::Insert(_))
            && s.editors.is_floating(id)
        {
            ui.centered_and_justified(|ui| {
                ui.add(egui::Label::new("This plugin is open in its own window").selectable(false));
            });
            return;
        }
        let text = if s.viewing.is_none() {
            "Load an instrument into a slot, then select it"
        } else if st.popup_open {
            "(editor hidden while a menu is open)"
        } else if s.editors.shown() != s.view_id {
            note.unwrap_or("This plugin has no editor")
        } else {
            ""
        };
        // An empty label would still fill the panel, sit on top of the scroll bars drawn above
        // and take their mouse events (and show a text cursor over them).
        if !text.is_empty() {
            ui.centered_and_justified(|ui| {
                ui.add(egui::Label::new(text).selectable(false));
            });
        }
    });
}

/// Takes the answer of a file dialog when it comes: a folder goes to the list it was asked for,
/// a program or installer into its text box.
fn poll_dialog(ui: &egui::Ui, s: &mut Session, st: &mut UiState, actions: &mut Vec<Action>) {
    use crate::dialog::{Poll, Want};
    let Some(open) = &st.picking else { return };
    match open.poll() {
        Poll::Waiting => {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(100));
            return;
        }
        Poll::Chosen(want, paths) => {
            let path = paths[0].clone();
            let text = path.display().to_string();
            match want {
                Want::PluginDir => actions.push(Action::AddDir(text)),
                Want::SoundDir => actions.push(Action::AddSoundDir(text)),
                Want::CaptureDir => actions.push(Action::SetCaptureDir(Some(path))),
                Want::WineProgram => st.wine_text = text,
                Want::YabridgeDir => st.yab_text = text,
                Want::Installer => st.installer_text = text,
                Want::BackingDir => actions.push(Action::SetBackingDir(Some(path))),
                Want::BackingTrack(song) => {
                    for p in paths {
                        actions.push(Action::AddTrack(song, p));
                    }
                }
            }
        }
        Poll::Cancelled => {}
        Poll::Failed(e) => s.say(e),
    }
    st.picking = None;
}

/// A button that opens a file dialog for `want` (greyed out while one is open).
pub fn pick_button(
    ui: &mut egui::Ui,
    picking: &mut Option<crate::dialog::Open>,
    label: &str,
    want: crate::dialog::Want,
    start: Option<&std::path::Path>,
) -> bool {
    let free = picking.is_none();
    let clicked = ui.add_enabled(free, egui::Button::new(label)).clicked();
    if clicked {
        *picking = Some(crate::dialog::Open::start(want, start));
    }
    clicked
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
            .map_or_else(String::new, |p| match &p.file {
                Some(file) => format!("{file} \u{b7} {}", p.name),
                None => p.name.clone(),
            });
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
            // A folder of SoundFonts: each file's presets under its name.
            let mut file: Option<&str> = None;
            for p in presets {
                let file_name = p.file.as_deref();
                if !query.is_empty()
                    && !p.name.to_lowercase().contains(&query)
                    && !file_name.is_some_and(|f| f.to_lowercase().contains(&query))
                {
                    continue;
                }
                if let Some(name) = file_name.filter(|n| Some(*n) != file) {
                    ui.add_space(4.0);
                    ui.label(RichText::new(name).strong().color(hw::TEXT_DIM));
                }
                file = file_name.or(file);
                // In a folder the bank number carries the file's number too: show the file's own.
                let bank = if file_name.is_some() {
                    p.bank & 0xFF
                } else {
                    p.bank
                };
                let label = format!("{:>3}:{:<3}  {}", bank, p.program, p.name);
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
fn scenes_bar(ui: &mut egui::Ui, s: &mut Session, st: &mut UiState, actions: &mut Vec<Action>) {
    if st
        .scene_edit
        .as_ref()
        .is_some_and(|(n, _)| *n >= s.snapshots().len())
    {
        st.scene_edit = None;
    }
    ui.horizontal_wrapped(|ui| {
        hw::engraved(ui, "Scenes").on_hover_text(SCENES_HELP);
        let current = s.current_snapshot();
        for (n, snap) in s.snapshots().iter().enumerate() {
            if let Some((_, text)) = st.scene_edit.as_mut().filter(|(e, _)| *e == n) {
                // Renaming in place: Enter or a click elsewhere keeps the name, Escape drops it.
                let edit = ui.add(
                    egui::TextEdit::singleline(text)
                        .desired_width(110.0)
                        .margin(egui::Margin::symmetric(6, 4)),
                );
                if !edit.has_focus() && !edit.lost_focus() {
                    edit.request_focus();
                    // Select the old name so typing replaces it.
                    if let Some(mut state) = egui::TextEdit::load_state(ui.ctx(), edit.id) {
                        state.cursor.set_char_range(Some(egui::text::CCursorRange::two(
                            egui::text::CCursor::new(0),
                            egui::text::CCursor::new(text.chars().count()),
                        )));
                        state.store(ui.ctx(), edit.id);
                    }
                }
                let escaped = ui.input(|i| i.key_pressed(egui::Key::Escape));
                if escaped {
                    st.scene_edit = None;
                } else if edit.lost_focus() {
                    let name = text.trim().to_owned();
                    if !name.is_empty() && name != snap.name {
                        actions.push(Action::RenameSnapshot(n, name));
                    }
                    st.scene_edit = None;
                }
                continue;
            }
            let label = format!("{}  {}", n + 1, snap.name);
            let r = hw::Cap::new(label)
                .lit((current == Some(n)).then_some(hw::AMBER))
                .min_width(70.0)
                .show(ui)
                .on_hover_text(
                    "Click to bring this scene back. Double-click to rename it. Right-click to overwrite or remove it. The lit scene is also updated when you save the rig.",
                );
            if r.clicked() && !s.map_mode {
                actions.push(Action::RecallSnapshot(n));
            }
            if r.double_clicked() && !s.map_mode {
                st.scene_edit = Some((n, snap.name.clone()));
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
                if ui.button("Rename").clicked() {
                    st.scene_edit = Some((n, snap.name.clone()));
                    ui.close();
                }
                if ui.button("Overwrite with what is set now").clicked() {
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
            .on_hover_text("Remember the rack as it is now: which instruments play, their volumes, zones, effects and SoundFont sounds. Name it right away, then keep tuning: saving the rig updates the lit scene.")
            .clicked()
        {
            actions.push(Action::AddSnapshot);
            st.scene_edit = Some((s.snapshots().len(), format!("Scene {}", s.snapshots().len() + 1)));
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

const SCENES_HELP: &str = "A scene remembers which slots play, their volumes, zones, effects on or off and SoundFont sounds. Click one to bring it back (or map it to a pedal). Double-click to rename it; right-click to overwrite or remove it. Saving the rig also updates the lit scene with what is set now. Scenes are saved in the rig file.";

fn header(ui: &mut egui::Ui, s: &mut Session, st: &mut UiState, actions: &mut Vec<Action>) {
    // Everything has to fit on one line, so the fader, the meter and the rig's name shrink as the
    // window narrows (the header is the same in every view).
    let room = ui.available_width();
    let (fit, two_rows) = if room >= 1380.0 {
        (
            HeaderFit {
                readout: true,
                lamps: 24,
                name: 120.0,
            },
            false,
        )
    } else if room >= 1300.0 {
        (
            HeaderFit {
                readout: true,
                lamps: 20,
                name: 104.0,
            },
            false,
        )
    } else if room >= 1170.0 {
        (
            HeaderFit {
                readout: false,
                lamps: 16,
                name: 90.0,
            },
            false,
        )
    } else {
        // Too narrow for one line: the levels and tempo go on a second one.
        (
            HeaderFit {
                readout: true,
                lamps: 24,
                name: 104.0,
            },
            true,
        )
    };
    ui.vertical(|ui| {
        header_row(ui, s, st, fit, two_rows, actions);
        if two_rows {
            ui.add_space(4.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                header_levels(ui, s, st, fit, actions);
            });
        }
    });
}

fn header_row(
    ui: &mut egui::Ui,
    s: &mut Session,
    st: &mut UiState,
    fit: HeaderFit,
    two_rows: bool,
    actions: &mut Vec<Action>,
) {
    let perform = st.perform;
    // A locked stage view can only be left by holding Unlock.
    let locked = perform && st.locked;
    ui.horizontal(|ui| {
        ui.label(RichText::new("MERLIN").size(18.0).strong().color(hw::AMBER));
        ui.add_space(6.0);
        // The header looks the same in every view, so nothing moves when you switch. On stage the
        // file buttons are there but dead: no New / Open / Save while performing.
        ui.add_enabled_ui(!perform, |ui| file_plate(ui, s, fit.name, actions));
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
            if !two_rows {
                header_levels(ui, s, st, fit, actions);
                ui.add_space(6.0);
            }
            capture_button(ui, s, actions);
            ui.add_enabled_ui(!locked, |ui| crate::map_mode::toggle(ui, s, actions));
            let open = st.keys_open;
            ui.add_enabled_ui(!perform, |ui| {
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
            });
        });
    });
}

/// Master volume, the level meter and the tempo, right to left (so the fader is the rightmost).
fn header_levels(
    ui: &mut egui::Ui,
    s: &mut Session,
    st: &mut UiState,
    fit: HeaderFit,
    actions: &mut Vec<Action>,
) {
    let master = s.rig.master_volume;
    // Right to left: the reading first, so it ends up to the right of the fader.
    if fit.readout {
        ui.add_sized(
            [58.0, 20.0],
            egui::Label::new(
                RichText::new(hw::gain_text(master))
                    .monospace()
                    .color(hw::TEXT_DIM),
            )
            .selectable(false),
        );
    }
    let (m, new) = hw::meter_fader(
        ui,
        &st.meter,
        fit.lamps,
        ui.input(|i| i.time),
        master,
        !s.map_mode,
    );
    let m = m.on_hover_text(format!(
        "Master volume {} and what is going to your speakers, left and right. Drag across the lamps \
         to set the volume (Shift for fine steps), scroll over them, double-click for 0 dB. The round \
         lamp lights if the output clips.",
        hw::gain_text(master)
    ));
    if let Some(gain) = new {
        actions.push(Action::SetMaster(gain));
    }
    crate::map_mode::target(ui, s, &m, ControlAction::MasterVolume, None, actions);
    ui.add_space(6.0);
    tempo_plate(ui, s, actions);
}

/// How big the header's flexible parts are at this window width.
#[derive(Clone, Copy)]
struct HeaderFit {
    /// The master volume written out (in dB) beside the meter-fader.
    readout: bool,
    lamps: usize,
    name: f32,
}

/// The tempo, in the header so it is always in view: the metronome (lit while it clicks) and
/// the beats per minute beside it, to drag or type. Plugins that sync to the host see this tempo.
fn tempo_plate(ui: &mut egui::Ui, s: &Session, actions: &mut Vec<Action>) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        let on = s.metronome();
        let icon = hw::Cap::new("")
            .icon(hw::Icon::Metronome)
            .lit(on.then_some(hw::AMBER))
            .min_width(34.0)
            .show(ui)
            .on_hover_text(if on {
                "Metronome on: click to stop it"
            } else {
                "Metronome: click to hear the tempo as a click"
            });
        if icon.clicked() && !s.map_mode {
            actions.push(Action::SetMetronome(!on));
        }
        crate::map_mode::target(ui, s, &icon, ControlAction::MetronomeToggle, None, actions);
        let mut bpm = s.tempo();
        let number = ui
            .add(
                egui::DragValue::new(&mut bpm)
                    .range(merlin_model::MIN_TEMPO..=merlin_model::MAX_TEMPO)
                    .speed(0.25)
                    .max_decimals(1),
            )
            .on_hover_text(
                "Tempo in beats per minute: drag, scroll or type. The click, the count-in, \
                 MIDI clock and every plugin that syncs to the host follow it.",
            );
        if number.changed() {
            actions.push(Action::SetTempo(bpm));
        }
        if number.hovered() {
            let wheel = ui.input(|i| i.smooth_scroll_delta.y);
            if wheel != 0.0 {
                actions.push(Action::SetTempo(s.tempo() + wheel.signum()));
            }
        }
        crate::map_mode::target(ui, s, &number, ControlAction::Tempo, None, actions);
        // The time signature: the beat is a quarter note, so 6/8 is felt as 6 beats at the
        // eighth-note tempo.
        let mut beats = s.beats_per_bar();
        let signature = ui
            .add(
                egui::DragValue::new(&mut beats)
                    .range(1..=16)
                    .suffix("/4")
                    .speed(0.1),
            )
            .on_hover_text(
                "Time signature: beats per bar (over a quarter-note beat). The click accents \
                 the first beat of each bar. A song's own setting wins while it runs.",
            );
        if signature.changed() {
            actions.push(Action::SetBeatsPerBar(beats));
        }
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
fn notice_bar(ui: &mut egui::Ui, s: &Session, st: &mut UiState, actions: &mut Vec<Action>) {
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
                // Plugin files that failed to scan stay one click away: beside the scan's own
                // message, and beside "Ready" once that has gone.
                let failed = s.entries.iter().filter(|e| e.error.is_some()).count();
                let about_failures = failed > 0
                    && !s.scanning()
                    && (kind == Notice::Idle || text.contains("files failed"));
                let message = RichText::new(text).size(14.5).color(colour);
                ui.label(if kind == Notice::Prompt {
                    message.strong()
                } else {
                    message
                });
                if about_failures {
                    if kind == Notice::Idle {
                        ui.label(
                            RichText::new(format!(
                                "\u{b7} {failed} plugin file{} failed to scan",
                                if failed == 1 { "" } else { "s" }
                            ))
                            .size(14.5)
                            .color(hw::AMBER),
                        );
                    }
                    if ui
                        .link(RichText::new("VIEW").size(14.5).strong())
                        .on_hover_text("Settings > Plugins: see which files, retry or ignore them")
                        .clicked()
                    {
                        actions.push(Action::ShowSettingsTab(SettingsTab::Plugins));
                    }
                }
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
fn file_plate(ui: &mut egui::Ui, s: &Session, name_width: f32, actions: &mut Vec<Action>) {
    hw::plate().show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 5.0;
            hw::engraved(ui, "Rig");
            let mut name = s.rig.name.clone();
            if ui
                .add(egui::TextEdit::singleline(&mut name).desired_width(name_width))
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
        .add_enabled_ui(minutes > 0, |ui| hw::Cap::new("CAPTURE MIDI").icon(hw::Icon::Tape).show(ui))
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
        .icon(hw::Icon::Learn)
        .lit(active.then_some(hw::AMBER))
        .show(ui)
        .on_hover_text(
        "Map a knob of this plugin to a control on your device: click here, turn the knob in the plugin's own window, then move the control. Click again to cancel.",
    );
    if r.clicked() {
        actions.push(Action::TouchLearn(!active));
    }
}

/// The sizes a popped-out window can be: a row of caps in a recessed track, as on hardware.
fn float_scales(ui: &mut egui::Ui, s: &Session, id: usize, actions: &mut Vec<Action>) {
    let now = s.editors.float_scale(id);
    hw::inset().show(ui, |ui| {
        ui.spacing_mut().item_spacing.x = 3.0;
        for (k, label) in [
            (1.0_f32, "1\u{d7}"),
            (1.25, "1.25\u{d7}"),
            (1.5, "1.5\u{d7}"),
        ] {
            let on = (f64::from(k) - now).abs() < 0.01;
            let cap = hw::Cap::new(label)
                .lit(on.then_some(hw::AMBER))
                .min_width(44.0)
                .show(ui)
                .on_hover_text(
                    "How big the plugin's window is, compared to the size it was made for",
                );
            if cap.clicked() {
                actions.push(Action::FloatScale(id, k));
            }
        }
    });
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
        if s.view_id.and_then(|id| s.editors.info(id)).is_none() {
            return;
        }
        if let Some(id) = s.view_id
            && matches!(st.tab, DockTab::Plugin | DockTab::Insert(_))
        {
            let floating = s.editors.is_floating(id);
            let pop = hw::Cap::new(if floating { "BRING BACK" } else { "POP OUT" })
                .icon(if floating {
                    hw::Icon::PopIn
                } else {
                    hw::Icon::PopOut
                })
                .lit(floating.then_some(hw::AMBER))
                .show(ui)
                .on_hover_text(if floating {
                    "Bring the plugin's window back into the dock"
                } else {
                    "Pop out: open the plugin in a window of its own, at the size it was made for"
                });
            if pop.clicked() {
                actions.push(if floating {
                    Action::PopIn(id)
                } else {
                    Action::PopOut(id)
                });
            }
            if floating {
                float_scales(ui, s, id, actions);
            }
        }
        // Lit: the editor is scaled down to fit the dock, as far as the plugin allows.
        let fit = hw::Cap::new("FIT TO WINDOW")
            .icon(hw::Icon::FitHeight)
            .lit(st.auto_fit.then_some(hw::AMBER))
            .show(ui)
            .on_hover_text(
                "Scale the plugin's window down to fit the space, as far as the plugin lets itself be scaled or resized (lit: on). Otherwise there are scroll bars.",
            );
        if fit.clicked() {
            actions.push(Action::AutoFit(!st.auto_fit));
        }
    });
}

/// Which scroll bars an editor of `editor` size needs in `room` (width, height): a bar takes
/// room from the other direction, which can make that one needed too.
fn scroll_bars_needed(room: (f32, f32), editor: (f32, f32), bar: f32) -> (bool, bool) {
    let (mut need_x, mut need_y) = (false, false);
    for _ in 0..2 {
        let w = room.0 - if need_y { bar } else { 0.0 };
        let h = room.1 - if need_x { bar } else { 0.0 };
        // A few points over (rounding, a plugin's own minimum step) is not worth a bar.
        need_x = editor.0 > w + 4.0;
        need_y = editor.1 > h + 4.0;
    }
    (need_x, need_y)
}

/// Places a plugin's editor in `avail`: fits it to the room when it can be resized, and gives
/// the scroll bars (in strips beside it, since a native window cannot be drawn over) to one
/// that is bigger than the room. Returns where the dock goes.
fn editor_view(
    ui: &mut egui::Ui,
    s: &Session,
    st: &mut UiState,
    id: usize,
    info: merlin_embed::EditorInfo,
    avail: egui::Rect,
    actions: &mut Vec<Action>,
) -> egui::Rect {
    const BAR: f32 = 14.0;
    let ppp = ui.ctx().pixels_per_point();
    let (ew, eh) = (info.size.width as f32 / ppp, info.size.height as f32 / ppp);
    let (need_x, need_y) = scroll_bars_needed((avail.width(), avail.height()), (ew, eh), BAR);
    let view = egui::Rect::from_min_size(
        avail.min,
        egui::vec2(
            avail.width() - if need_y { BAR } else { 0.0 },
            avail.height() - if need_x { BAR } else { 0.0 },
        ),
    );
    // Asked once for each size of the dock, once that size has held still for a moment (making
    // some editors over is not instant, and a drag of the edge passes through many sizes). The
    // plugin may settle on another size, which is not asked about again.
    let dock = s.editors.dock_size();
    let key = (id, dock.0, dock.1);
    if st.auto_fit && (info.size.width, info.size.height) != dock && st.last_fit != Some(key) {
        const SETTLE: std::time::Duration = std::time::Duration::from_millis(300);
        match st.fit_wait {
            Some((k, since)) if k == key => {
                if since.elapsed() >= SETTLE {
                    st.last_fit = Some(key);
                    st.fit_wait = None;
                    actions.push(Action::Fit);
                } else {
                    ui.ctx().request_repaint_after(SETTLE);
                }
            }
            _ => {
                st.fit_wait = Some((key, std::time::Instant::now()));
                ui.ctx().request_repaint_after(SETTLE);
            }
        }
    }
    let (mut x, mut y) = (info.scroll.0 as f32 / ppp, info.scroll.1 as f32 / ppp);
    let before = (x, y);
    // The mouse wheel scrolls from anywhere over the editor's room (over the plugin itself the
    // plugin gets it): up and down move the picture, sideways too, or up and down when there is
    // no vertical bar. egui feeds a wheel notch out over several frames; without a frame asked
    // for until it is used up, the rest of each notch waits for the next event and the scroll
    // looks slow and skips.
    if (need_x || need_y)
        && ui.input(|i| i.is_scrolling() || i.smooth_scroll_delta != egui::Vec2::ZERO)
    {
        ui.ctx().request_repaint();
    }
    if (need_x || need_y)
        && ui
            .input(|i| i.pointer.hover_pos().or(i.pointer.latest_pos()))
            .is_some_and(|p| avail.contains(p))
    {
        let wheel = ui.input(|i| i.smooth_scroll_delta);
        if need_y {
            y -= wheel.y;
        } else {
            x -= wheel.y;
        }
        if need_x {
            x -= wheel.x;
        }
        x = x.clamp(0.0, (ew - view.width()).max(0.0));
        y = y.clamp(0.0, (eh - view.height()).max(0.0));
    }
    if need_x {
        let strip = egui::Rect::from_min_size(
            egui::pos2(view.left(), view.bottom()),
            egui::vec2(view.width(), BAR),
        );
        hw::scrollbar(ui, strip, hw::Axis::Horizontal, &mut x, view.width(), ew);
    } else {
        x = 0.0;
    }
    if need_y {
        let strip = egui::Rect::from_min_size(
            egui::pos2(view.right(), view.top()),
            egui::vec2(BAR, view.height()),
        );
        hw::scrollbar(ui, strip, hw::Axis::Vertical, &mut y, view.height(), eh);
    } else {
        y = 0.0;
    }
    if (x, y) != before {
        actions.push(Action::Scroll(
            (x * ppp).round() as u32,
            (y * ppp).round() as u32,
        ));
    }
    view
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
    fn an_editor_gets_scroll_bars_only_where_it_does_not_fit() {
        assert_eq!(
            scroll_bars_needed((800.0, 600.0), (700.0, 500.0), 14.0),
            (false, false)
        );
        assert_eq!(
            scroll_bars_needed((800.0, 600.0), (900.0, 500.0), 14.0),
            (true, false)
        );
        assert_eq!(
            scroll_bars_needed((800.0, 600.0), (700.0, 700.0), 14.0),
            (false, true)
        );
        // Exactly the room it has: no bars. A bar's own thickness can tip the other one over.
        assert_eq!(
            scroll_bars_needed((800.0, 600.0), (800.0, 600.0), 14.0),
            (false, false)
        );
        assert_eq!(
            scroll_bars_needed((800.0, 600.0), (800.0, 700.0), 14.0),
            (true, true)
        );
    }

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
