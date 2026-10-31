//! Merlin: a live instrument host. One window: the rack on top, the selected
//! instrument's editor embedded below, a keyboard map at the bottom.
//!
//! egui/wgpu on our own winit loop, forced to X11 (XWayland on Wayland desktops), which is what
//! lets plugin editors embed as child windows (plan D7/D8).
//!
//! `merlin [--plugin-dir DIR]... [--rig FILE] [--fresh]`
//! Test hook: `MERLIN_SCRIPT="3:load 1 Surge XT; 5:on 60; 6:peak; 8:quit"`.

mod autotag;
mod browser;
mod dialog;
mod drivers;
mod guard;
mod hw;
mod icons;
mod keyboard;
mod library;
mod macro_panel;
mod map_mode;
mod marker;
mod param_panel;
mod paths;
mod perf;
mod perform;
mod quirks;
mod sandbox;
mod scan_worker;
mod session;
mod setlist_ui;
mod settings;
mod settings_ui;
mod system_audio;
mod tags;
mod touch;
mod ui;
mod zone_view;

use std::num::NonZeroU32;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};
use egui::ViewportId;
use merlin_model::FxSettings;
use merlin_scanner::Found;
use session::{Session, UI_SLOTS, UserEvent};
use settings::Settings;
use ui::{Action, UiState};
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::platform::x11::{EventLoopBuilderExtX11, WindowAttributesExtX11};
use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
use winit::window::{Window, WindowId};

struct Args {
    plugin_dirs: Vec<PathBuf>,
    rig: Option<PathBuf>,
    fresh: bool,
    /// Started by the crash watchdog: restore the autosave.
    recover: bool,
}

fn parse_args() -> Args {
    let mut a = Args {
        plugin_dirs: Vec::new(),
        rig: None,
        fresh: false,
        recover: false,
    };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--plugin-dir" => a.plugin_dirs.extend(it.next().map(PathBuf::from)),
            "--rig" => a.rig = it.next().map(PathBuf::from),
            "--fresh" => a.fresh = true,
            "--recover" => a.recover = true,
            other => eprintln!("ignoring unknown argument '{other}'"),
        }
    }
    a
}

struct Running {
    window: Arc<Window>,
    egui: egui_winit::State,
    painter: egui_wgpu::winit::Painter,
    session: Session,
    ui: UiState,
    settings: Settings,
    frames: u64,
    wakeups: u64,
    started: Instant,
    next_status: Instant,
    repaint_at: Option<Instant>,
    service_at: Option<Instant>,
    cpu_mark: (Instant, u64),
    cpu_script: Option<(Instant, u64)>,
    script: Vec<(Duration, Action)>,
    quit: bool,
    requested: bool,
}

struct App {
    proxy: EventLoopProxy<UserEvent>,
    args: Args,
    settings: Settings,
    cli_dirs: Vec<PathBuf>,
    guard: Arc<guard::Guard>,
    run: Option<Running>,
}

fn x11_window_id(window: &Window) -> Result<u32> {
    match window.window_handle()?.as_raw() {
        RawWindowHandle::Xlib(h) => Ok(h.window as u32),
        RawWindowHandle::Xcb(h) => Ok(h.window.get()),
        other => Err(anyhow!("not an X11 window: {other:?}")),
    }
}

/// CPU ticks (user + system) this process has used.
fn own_cpu_ticks() -> u64 {
    let Ok(stat) = std::fs::read_to_string("/proc/self/stat") else {
        return 0;
    };
    let Some(close) = stat.rfind(')') else {
        return 0;
    };
    let f: Vec<&str> = stat[close + 2..].split(' ').collect();
    let get = |i: usize| f.get(i).and_then(|v| v.parse::<u64>().ok()).unwrap_or(0);
    get(11) + get(12)
}

/// Resident memory in MB and the thread count, from `/proc/self/status` (soak runs watch these).
fn own_memory() -> (u64, u64) {
    let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    let field = |name: &str| {
        status
            .lines()
            .find_map(|l| l.strip_prefix(name))
            .and_then(|v| v.split_whitespace().next()?.parse::<u64>().ok())
            .unwrap_or(0)
    };
    (field("VmRSS:") / 1024, field("Threads:"))
}

fn ticks_per_second() -> f64 {
    // SAFETY: sysconf is always safe to call.
    unsafe { libc::sysconf(libc::_SC_CLK_TCK) as f64 }
}

/// The window icon (the pixel-art hat, `packaging/icons/merlin.png`; `tools/make-icon.py` produces the raw pixels).
fn window_icon() -> Option<winit::window::Icon> {
    winit::window::Icon::from_rgba(include_bytes!("../data/icon-128.rgba").to_vec(), 128, 128).ok()
}

impl App {
    fn start(&mut self, event_loop: &ActiveEventLoop) -> Result<Running> {
        let window = Arc::new(
            event_loop.create_window(
                Window::default_attributes()
                    .with_title("Merlin")
                    .with_window_icon(window_icon())
                    // The class the desktop entry's StartupWMClass names, so the shell pairs the window with its icon.
                    .with_name("merlin", "merlin")
                    .with_inner_size(winit::dpi::LogicalSize::new(1280.0, 900.0))
                    // Shown below, once the window manager has been told not to focus it.
                    .with_visible(false)
                    .with_active(false),
            )?,
        );
        let xid = x11_window_id(&window)?;
        // The window appears behind what the user is working in, without taking the keyboard.
        if let Err(e) = merlin_embed::show_without_focus(xid) {
            eprintln!("could not ask for a quiet start: {e:#}");
        }
        window.set_visible(true);
        println!("main window X11 id {xid:#x}");

        let ctx = egui::Context::default();
        hw::apply_style(&ctx);
        let config = egui_wgpu::WgpuConfiguration {
            wgpu_setup: egui_wgpu::WgpuSetup::from_display_handle(
                event_loop.owned_display_handle(),
            ),
            ..Default::default()
        };
        let mut painter = pollster::block_on(egui_wgpu::winit::Painter::new(
            ctx.clone(),
            config,
            false,
            Default::default(),
        ));
        pollster::block_on(painter.set_window(ViewportId::ROOT, Some(window.clone())))?;
        let egui = egui_winit::State::new(
            ctx,
            ViewportId::ROOT,
            &window,
            Some(window.scale_factor() as f32),
            None,
            painter.max_texture_side(),
        );

        // A plugin that took the app down while loading must not do it again.
        let interrupted = marker::quarantine_interrupted_load();
        let mut session = Session::new(
            xid,
            window.scale_factor(),
            self.proxy.clone(),
            &self.settings,
            self.cli_dirs.clone(),
            self.guard.clone(),
        )?;

        // Start from the rig asked for; after a crash, from the autosave.
        let marker = paths::state_dir().join("running");
        let crashed = marker.exists();
        let autosave = paths::autosave_file();
        if let Ok(name) = std::env::var("MERLIN_RECOVERED") {
            session.say(format!(
                "{name} crashed and was quarantined; the app restarted from the autosave. \
                 Use Retry on its slot to try it again."
            ));
        }
        if let Some(name) = interrupted {
            session.say(format!(
                "{name} crashed the app while loading and is quarantined; the rest of the rig \
                 is restored. Use Retry on its slot to try it again."
            ));
        }
        let start_rig = match (&self.args.rig, self.args.fresh) {
            (Some(p), _) => Some(p.clone()),
            (None, false) if (crashed || self.args.recover) && autosave.exists() => {
                if !self.args.recover {
                    session.say("the last session did not end cleanly: restoring the autosave");
                }
                Some(autosave)
            }
            _ => None,
        };
        if let Some(p) = start_rig
            && let Err(e) = session.open_rig(&p)
        {
            session.say(format!("cannot open {}: {e:#}", p.display()));
        }
        let _ = std::fs::create_dir_all(paths::state_dir());
        let _ = std::fs::write(&marker, std::process::id().to_string());

        let mut run = Running {
            window,
            egui,
            painter,
            session,
            ui: {
                let mut ui = UiState::new();
                ui.wine_text = self
                    .settings
                    .wine_loader
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default();
                ui.yab_text = self
                    .settings
                    .yabridge_dir
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default();
                ui
            },
            settings: self.settings.clone(),
            frames: 0,
            wakeups: 0,
            started: Instant::now(),
            next_status: Instant::now(),
            repaint_at: None,
            service_at: None,
            cpu_mark: (Instant::now(), own_cpu_ticks()),
            cpu_script: None,
            script: parse_script(),
            quit: false,
            requested: false,
        };
        // Lay out once so the dock has its real rect before the first editor is created.
        run.redraw();
        if let Some(i) = run.session.targets.first().copied() {
            run.session.show_editor(i);
        }
        run.ask_redraw();
        Ok(run)
    }
}

impl Running {
    fn ask_redraw(&mut self) {
        self.requested = true;
        self.window.request_redraw();
    }

    fn redraw(&mut self) {
        self.frames += 1;
        let peaks = self.session.take_peaks();
        for (shown, p) in self.ui.peaks_shown.iter_mut().zip(peaks) {
            *shown = p.max(*shown * 0.7);
        }
        let raw = self.egui.take_egui_input(&self.window);
        let ctx = self.egui.egui_ctx().clone();
        let mut dock_rect = egui::Rect::NOTHING;
        let mut actions = Vec::new();
        let out = ctx.run_ui(raw, |ui| {
            ui::draw(
                ui,
                &mut self.session,
                &mut self.ui,
                &mut dock_rect,
                &mut actions,
            )
        });
        self.egui
            .handle_platform_output(&self.window, out.platform_output);

        // Overlap rule (plan §4.6): native child windows draw above egui, so while an egui
        // popup is open the dock is unmapped.
        self.ui.popup_open = egui::Popup::is_any_open(&ctx);
        let ppp = out.pixels_per_point;
        let _ = self.session.editors.set_rect(
            (dock_rect.min.x * ppp).round() as i32,
            (dock_rect.min.y * ppp).round() as i32,
            (dock_rect.width() * ppp).round() as u32,
            (dock_rect.height() * ppp).round() as u32,
        );
        // The dock is unmapped while an egui popup is open, and while a tab that is drawn by the
        // app is shown in its place.
        let covered = self.ui.popup_open
            || self.ui.settings_open
            || self.ui.perform
            || self.ui.tab == ui::DockTab::Macros
            || self.ui.tab == ui::DockTab::Params
            || self.ui.tab == ui::DockTab::Zones;
        let _ = self.session.editors.set_popup_open(covered);

        let prims = ctx.tessellate(out.shapes, ppp);
        let mut textures = out.textures_delta;
        self.painter.paint_and_update_textures(
            ViewportId::ROOT,
            ppp,
            [0.08, 0.08, 0.09, 1.0],
            &prims,
            &mut textures,
            Vec::new(),
            &self.window,
        );

        let delay = out
            .viewport_output
            .get(&ViewportId::ROOT)
            .map(|v| v.repaint_delay)
            .unwrap_or(Duration::MAX);
        self.repaint_at = if delay == Duration::ZERO {
            self.ask_redraw();
            None
        } else {
            Instant::now().checked_add(delay)
        };
        for action in actions {
            self.apply(action);
        }
    }

    fn apply(&mut self, action: Action) {
        let s = &mut self.session;
        let result: Result<()> = match action {
            Action::Load(i, k) => match s.found.get(k).cloned() {
                Some(f) => s.load(i, &f),
                None => Err(anyhow!("no such plugin")),
            },
            Action::LoadNamed(i, query) => {
                let q = query.to_lowercase();
                let label = |f: &Found| ui::found_label(f).to_lowercase();
                let k = s
                    .found
                    .iter()
                    .position(|f| label(f) == q)
                    .or_else(|| s.found.iter().position(|f| label(f).contains(&q)));
                match k.and_then(|k| s.found.get(k).cloned()) {
                    Some(f) => s.load(i, &f),
                    None => Err(anyhow!("no plugin matches '{query}'")),
                }
            }
            Action::Unload(i) => {
                s.unload(i);
                Ok(())
            }
            Action::Retry(i) => s.retry(i),
            Action::Select(i, layer) => {
                s.select(i, layer);
                Ok(())
            }
            Action::Mute(i) => {
                s.toggle_mute(i);
                Ok(())
            }
            Action::Zone(i, lo, hi) => {
                s.set_zone(i, lo, hi);
                Ok(())
            }
            Action::Channel(i, channel) => {
                s.set_channel(i, channel);
                Ok(())
            }
            Action::Volume(i, v) => {
                s.set_volume(i, v);
                Ok(())
            }
            Action::SetPreset(i, bank, program) => {
                s.set_preset(i, bank, program);
                Ok(())
            }
            Action::SamplerTone(i, tone) => {
                s.set_sampler_tone(i, tone);
                Ok(())
            }
            Action::SamplerField(i, field, v) => {
                s.set_sampler_field(i, field, v);
                Ok(())
            }
            Action::SetView(view) => {
                // A locked stage view stays until it is unlocked.
                if !self.ui.locked || !self.ui.perform {
                    self.ui.perform = view == ui::View::Perform;
                    self.ui.settings_open = view == ui::View::Settings;
                    self.ui.setlist_open = view == ui::View::Setlist;
                }
                Ok(())
            }
            Action::TouchLearn(start) => {
                match (start, s.viewing) {
                    (true, Some(i)) => {
                        let insert = match self.ui.tab {
                            ui::DockTab::Insert(k) => Some(k),
                            _ => None,
                        };
                        s.start_touch_learn(i, insert);
                    }
                    _ => {
                        s.cancel_touch_learn();
                        s.cancel_learn();
                    }
                }
                Ok(())
            }
            Action::TouchStart(slot, insert) => {
                s.start_touch_learn(slot, insert);
                Ok(())
            }
            Action::FxDrawer(open) => {
                self.ui.fx_open = open;
                Ok(())
            }
            Action::RackCollapsed(collapsed) => {
                self.ui.rack_collapsed = collapsed;
                if !collapsed {
                    // Showing every slot means the effects drawer, which hides them, goes.
                    self.ui.fx_open = false;
                }
                Ok(())
            }
            Action::BrowserOpen(open) => {
                self.ui.browser_open = open;
                Ok(())
            }
            Action::AutoFit(on) => {
                self.ui.auto_fit = on;
                self.ui.last_fit = None;
                self.ui.fit_wait = None;
                if on {
                    Ok(())
                } else {
                    // Off puts every editor back at the size it was made.
                    let s = &mut self.session;
                    s.editors
                        .restore_native(&mut session::lookup(&mut s.plugins, &mut s.inserts))
                }
            }
            Action::KeysDrawer(open) => {
                self.ui.keys_open = open;
                Ok(())
            }
            Action::ToggleSettings => {
                self.ui.settings_open = !self.ui.settings_open;
                Ok(())
            }
            Action::ShowSettingsTab(tab) => {
                self.ui.settings_open = true;
                self.ui.settings_tab = tab;
                Ok(())
            }
            Action::AddDir(text) => {
                s.add_plugin_dir(PathBuf::from(text.trim()));
                Ok(())
            }
            Action::RemoveDir(i) => {
                s.remove_plugin_dir(i);
                Ok(())
            }
            Action::AddSoundDir(text) => {
                s.add_sound_dir(PathBuf::from(text.trim()));
                Ok(())
            }
            Action::RemoveSoundDir(i) => {
                s.remove_sound_dir(i);
                Ok(())
            }
            Action::Rescan(all) => {
                s.rescan(all);
                Ok(())
            }
            Action::RetryFile(p) => {
                s.retry_file(p);
                Ok(())
            }
            Action::Unquarantine(p, id) => {
                s.remove_from_quarantine(&p, &id);
                Ok(())
            }
            Action::SetBuffer(n) => s.set_buffer_size(n),
            Action::LearnControl(action) => {
                s.start_learn(action);
                Ok(())
            }
            Action::MapParamNamed(kind, slot, insert, query, pickup) => {
                s.map_param_named(kind, slot, insert, &query, pickup)
            }
            Action::LearnTarget(action, label) => {
                s.start_learn_labelled(action, label);
                Ok(())
            }
            Action::SetMapMode(on) => {
                s.map_mode = on;
                if !on {
                    s.cancel_learn();
                }
                Ok(())
            }
            Action::CancelLearn => {
                s.cancel_learn();
                Ok(())
            }
            Action::RemoveMapping(i) => {
                s.remove_mapping(i);
                Ok(())
            }
            Action::SetGlue(i, g) => {
                s.set_mapping_glue(i, g);
                Ok(())
            }
            Action::SetSwitch(i, sw) => {
                s.set_mapping_switch(i, sw);
                Ok(())
            }
            Action::SetTakeover(i, t) => {
                s.set_mapping_takeover(i, t);
                Ok(())
            }
            Action::SetPerform(on) => {
                // A locked stage view stays until it is unlocked.
                if on || !self.ui.locked {
                    self.ui.perform = on;
                    if on {
                        self.ui.settings_open = false;
                    }
                }
                Ok(())
            }
            Action::SetLock(on) => {
                self.ui.locked = on && self.ui.perform;
                self.ui.unlock_since = None;
                Ok(())
            }
            Action::ToggleFullscreen => {
                let full = self.window.fullscreen().is_some();
                self.window
                    .set_fullscreen((!full).then_some(winit::window::Fullscreen::Borderless(None)));
                Ok(())
            }
            Action::SetMacro(i, k, v) => {
                s.set_macro(i, k, v);
                Ok(())
            }
            Action::RenameMacro(i, k, name) => {
                s.rename_macro(i, k, name);
                Ok(())
            }
            Action::AddMacroParam(i, k, insert, info) => {
                s.add_macro_param(i, k, insert, &info);
                Ok(())
            }
            Action::AddMacroSampler(i, k, field) => {
                s.add_macro_sampler(i, k, field);
                Ok(())
            }
            Action::AddMacroFx(i, k, id) => {
                s.add_macro_fx(i, k, id);
                Ok(())
            }
            Action::RemoveMacroTarget(i, k, t) => {
                s.remove_macro_target(i, k, t);
                Ok(())
            }
            Action::SetMacroRange(i, k, t, from, to) => {
                s.set_macro_range(i, k, t, from, to);
                Ok(())
            }
            Action::AddMacroParamNamed(i, k, query) => {
                let q = query.to_lowercase();
                if let Some(field) = q.strip_prefix("sampler:") {
                    match merlin_model::SamplerField::ALL
                        .into_iter()
                        .find(|f| f.name().to_lowercase() == field)
                    {
                        Some(f) => {
                            s.add_macro_sampler(i, k, f);
                            Ok(())
                        }
                        None => Err(anyhow::anyhow!("no sampler field {field:?}")),
                    }
                } else {
                    let found = s
                        .plugin_params(i, None)
                        .iter()
                        .find(|p| p.name.to_lowercase().contains(&q))
                        .cloned();
                    match found {
                        Some(p) => {
                            s.say(format!("macro {} drives {:?}", k + 1, p.name));
                            s.add_macro_param(i, k, None, &p);
                            Ok(())
                        }
                        None => Err(anyhow::anyhow!("no parameter matching {query:?}")),
                    }
                }
            }
            Action::TransportToggle => {
                s.transport_toggle();
                Ok(())
            }
            Action::TransportStart => {
                if !s.transport_running() && !s.counting_in() {
                    s.transport_toggle();
                }
                Ok(())
            }
            Action::TransportStop => {
                if s.transport_running() || s.counting_in() {
                    s.transport_toggle();
                }
                Ok(())
            }
            Action::SetClickState(on) => {
                let v = s.rig.transport.click_volume;
                s.set_click(on, v);
                Ok(())
            }
            Action::ToggleClick => {
                s.toggle_click();
                Ok(())
            }
            Action::SetClickVolume(v) => {
                let on = s.rig.transport.click;
                s.set_click(on, v);
                Ok(())
            }
            Action::TapTempo => {
                s.tap_tempo();
                Ok(())
            }
            Action::SetTempo(v) => {
                s.set_tempo(v);
                Ok(())
            }
            Action::SetMetronome(on) => {
                s.set_metronome(on);
                Ok(())
            }
            Action::SetBeatsPerBar(n) => {
                s.set_beats_per_bar(n);
                Ok(())
            }
            Action::SetCountIn(n) => {
                s.set_count_in_bars(n);
                Ok(())
            }
            Action::SetMainOutput(p) => {
                s.set_main_output(p);
                Ok(())
            }
            Action::SetClickOutput(p) => {
                s.set_click_output(p);
                Ok(())
            }
            Action::AddMarker(n, m) => {
                s.add_marker(n, m);
                Ok(())
            }
            Action::RemoveMarker(n, k) => {
                s.remove_marker(n, k);
                Ok(())
            }
            Action::EditMarker(n, k, m) => {
                s.edit_marker(n, k, |old| *old = m);
                Ok(())
            }
            Action::AddTrack(n, path) => {
                s.add_track(n, path);
                Ok(())
            }
            Action::RemoveTrack(n, k) => {
                s.remove_track(n, k);
                Ok(())
            }
            Action::EditTrack(n, k, edit) => {
                s.edit_track(n, k, edit);
                Ok(())
            }
            Action::ToggleSetlist => {
                self.ui.setlist_open = !self.ui.setlist_open;
                if self.ui.setlist_open {
                    self.ui.settings_open = false;
                }
                Ok(())
            }
            Action::AddSong => {
                if let Some(n) = s.add_song() {
                    self.ui.edit_song = n;
                }
                Ok(())
            }
            Action::RemoveSong(n) => {
                s.remove_song(n);
                Ok(())
            }
            Action::MoveSong(n, down) => {
                s.move_song(n, down);
                if self.ui.edit_song == n {
                    self.ui.edit_song = if down { n + 1 } else { n.saturating_sub(1) };
                }
                Ok(())
            }
            Action::EditSong(n, edit) => {
                s.edit_song(n, |song| match edit {
                    ui::SongEdit::Name(v) => song.name = v,
                    ui::SongEdit::Tempo(v) => song.tempo = v,
                    ui::SongEdit::Beats(v) => song.beats_per_bar = v,
                    ui::SongEdit::Note(v) => song.note = v,
                });
                Ok(())
            }
            Action::AddPart(n, from_current) => {
                if from_current {
                    s.add_part_from_current(n);
                } else {
                    s.add_part(n, None);
                }
                Ok(())
            }
            Action::RemovePart(n, k) => {
                s.remove_part(n, k);
                Ok(())
            }
            Action::MovePart(n, k, down) => {
                s.move_part(n, k, down);
                Ok(())
            }
            Action::EditPart(n, k, edit) => {
                s.edit_part(n, k, |p| match edit {
                    ui::PartEdit::Name(v) => p.name = v,
                    ui::PartEdit::Snapshot(v) => p.snapshot = v,
                    ui::PartEdit::Tempo(v) => p.tempo = v,
                    ui::PartEdit::Note(v) => p.note = v,
                });
                Ok(())
            }
            Action::GoTo(n, k) => {
                s.go_to(n, k);
                Ok(())
            }
            Action::NextPart => {
                s.next_part();
                Ok(())
            }
            Action::PrevPart => {
                s.prev_part();
                Ok(())
            }
            Action::NextSong => {
                s.next_song();
                Ok(())
            }
            Action::PrevSong => {
                s.prev_song();
                Ok(())
            }
            Action::AddSnapshot => {
                s.add_snapshot();
                Ok(())
            }
            Action::RecallSnapshot(n) => {
                s.recall_snapshot(n);
                Ok(())
            }
            Action::UpdateSnapshot(n) => {
                s.update_snapshot(n);
                Ok(())
            }
            Action::RenameSnapshot(n, name) => {
                s.rename_snapshot(n, name);
                Ok(())
            }
            Action::RemoveSnapshot(n) => {
                s.remove_snapshot(n);
                Ok(())
            }
            Action::AddMapping(m) => {
                s.add_mapping(m);
                Ok(())
            }
            Action::SetMidiDevice(name, role, on) => {
                s.set_midi_device(&name, role, on);
                Ok(())
            }
            Action::RefreshMidi => {
                s.refresh_midi();
                Ok(())
            }
            Action::SaveWine(wine, yab) => {
                let opt = |t: String| {
                    let t = t.trim().to_owned();
                    (!t.is_empty()).then(|| PathBuf::from(t))
                };
                s.settings.wine_loader = opt(wine);
                s.settings.yabridge_dir = opt(yab);
                s.save_settings();
                s.say("saved; restart the app to use the new Wine settings");
                Ok(())
            }
            Action::InstallWindows(path) => {
                let path = PathBuf::from(path.trim().trim_matches('"'));
                s.install_windows_plugin(path)
            }
            Action::LaunchWindowsApp(which) => s.launch_windows_app(&which),
            Action::SyncWindows => s.sync_windows_plugins(),
            Action::OpenWinePrefix => s.open_wine_prefix(),
            Action::DropFile(path) => {
                self.ui.drag_hint = None;
                if self.ui.locked {
                    self.session.say("locked: ignoring the dropped file");
                    return;
                }
                let ext = path
                    .extension()
                    .and_then(|e| e.to_str())
                    .map(str::to_ascii_lowercase);
                match ext.as_deref() {
                    // Audio: a backing track for the song being edited in the Setlist screen.
                    Some("wav" | "flac" | "mp3" | "ogg" | "m4a" | "aac") => {
                        if self.ui.setlist_open && !self.session.songs().is_empty() {
                            self.session.add_track(self.ui.edit_song, path);
                            Ok(())
                        } else {
                            Err(anyhow::anyhow!(
                                "open the Setlist screen first to add a backing track to a song"
                            ))
                        }
                    }
                    Some("exe" | "msi") => {
                        // Show the progress where it is written.
                        self.ui.settings_open = true;
                        self.ui.settings_tab = settings_ui::SettingsTab::Windows;
                        if s.windows_runtime().is_none() {
                            Err(anyhow!(
                                "Windows plugin support is not installed yet: press \"Download \
                                 Windows plugin support\" here, then drop the installer again"
                            ))
                        } else {
                            s.install_windows_plugin(path)
                        }
                    }
                    Some("sf2" | "sf3") => s.load_soundfont_file(&path),
                    _ if crate::paths::is_rig_file(&path) => s.open_rig(&path),
                    _ => Err(anyhow!(
                        "don't know what to do with {}",
                        path.file_name().map_or_else(
                            || path.display().to_string(),
                            |n| n.to_string_lossy().into_owned()
                        )
                    )),
                }
            }
            Action::InstallRuntime => s.install_runtime(),
            Action::CheckUpdate => s.check_for_update(),
            Action::InstallUpdate => s.install_update(),
            Action::RestartForUpdate => s.restart_for_update(),
            Action::OpenReleasePage => match s.update.info.as_ref().map(|i| i.page_url.clone()) {
                Some(url) => crate::dialog::open_folder(std::path::Path::new(&url))
                    .map_err(|e| anyhow::anyhow!(e)),
                None => Err(anyhow::anyhow!("no release page to open")),
            },
            Action::ToggleFavorite(k) => {
                if let Some(f) = s.found.get(k).cloned() {
                    s.toggle_favorite(&f);
                }
                Ok(())
            }
            Action::HidePlugin(k) => {
                if let Some(f) = s.found.get(k).cloned() {
                    s.hide_plugin(&f);
                }
                Ok(())
            }
            Action::UnhidePlugin(key) => {
                s.unhide_plugin(&key);
                Ok(())
            }
            Action::IgnoreFile(path) => {
                s.ignore_file(path);
                Ok(())
            }
            Action::UnignoreFile(path) => {
                s.unignore_file(&path);
                Ok(())
            }
            Action::ToggleTag(k, tag) => {
                if let Some(f) = s.found.get(k).cloned() {
                    s.toggle_tag(&f, tag);
                }
                Ok(())
            }
            Action::ResetTags(k) => {
                if let Some(f) = s.found.get(k).cloned() {
                    s.reset_tags(&f);
                }
                Ok(())
            }
            Action::ClearTags(k) => {
                if let Some(f) = s.found.get(k).cloned() {
                    s.clear_tags(&f);
                }
                Ok(())
            }
            Action::BrowserLoad(k) => s.load_into_free_slot(k),
            Action::DropLoad(i, k) => match s.found.get(k).cloned() {
                Some(f) => s.load(i, &f),
                None => Err(anyhow!("no such plugin")),
            },
            Action::SetFx(i, fx) => {
                s.set_fx(i, fx);
                Ok(())
            }
            Action::FxPreset(i, name) => {
                let mut fx = s.rig.slots[i].fx;
                match name.as_str() {
                    "reverb" => fx.reverb.enabled = true,
                    "delay" => fx.delay.enabled = true,
                    "comp" => fx.comp.enabled = true,
                    "eq" => {
                        fx.eq.enabled = true;
                        fx.eq.low_db = 5.0;
                        fx.eq.high_db = -4.0;
                    }
                    _ => fx = FxSettings::default(),
                }
                s.set_fx(i, fx);
                Ok(())
            }
            Action::SetMaster(v) => {
                s.set_master_volume(v);
                Ok(())
            }
            Action::Tab(tab) => {
                self.ui.tab = tab;
                if let Some(i) = s.viewing {
                    match tab {
                        ui::DockTab::Plugin => s.show_editor(i),
                        ui::DockTab::Insert(k) => s.show_insert_editor(i, k),
                        ui::DockTab::Macros | ui::DockTab::Params | ui::DockTab::Zones => {}
                    }
                }
                Ok(())
            }
            Action::LoadInsert(i, k, e) => match s.effects.get(e).cloned() {
                Some(f) => s.load_insert(i, k, &f),
                None => Err(anyhow::anyhow!("no such effect plugin")),
            },
            Action::LoadInsertNamed(i, k, query) => {
                let q = query.to_lowercase();
                let label = |f: &Found| ui::found_label(f).to_lowercase();
                match s
                    .effects
                    .iter()
                    .find(|f| label(f) == q)
                    .or_else(|| s.effects.iter().find(|f| label(f).contains(&q)))
                    .cloned()
                {
                    Some(f) => s.load_insert(i, k, &f),
                    None => Err(anyhow::anyhow!("no effect plugin matching {query:?}")),
                }
            }
            Action::UnloadInsert(i, k) => {
                s.unload_insert(i, k);
                if self.ui.tab == ui::DockTab::Insert(k) {
                    self.ui.tab = ui::DockTab::Plugin;
                }
                Ok(())
            }
            Action::BypassInsert(i, k) => {
                s.toggle_insert_bypass(i, k);
                Ok(())
            }
            Action::Transpose(i, t) => {
                s.set_transpose(i, t);
                Ok(())
            }
            Action::PopOut(id) => {
                s.pop_out_editor(id);
                Ok(())
            }
            Action::PopIn(id) => {
                self.ui.last_fit = None;
                s.pop_in_editor(id);
                Ok(())
            }
            Action::FloatScale(id, k) => {
                s.set_float_scale(id, f64::from(k));
                Ok(())
            }
            Action::Fit => s
                .editors
                .fit(&mut session::lookup(&mut s.plugins, &mut s.inserts)),
            Action::Scroll(x, y) => s.editors.scroll(x, y),
            Action::NoteOn(k) => {
                s.note(k, true);
                Ok(())
            }
            Action::NoteOff(k) => {
                s.note(k, false);
                Ok(())
            }
            Action::Panic => {
                s.panic();
                Ok(())
            }
            Action::CaptureMidi => {
                s.capture_midi();
                Ok(())
            }
            Action::SetCaptureMinutes(m) => {
                s.set_capture_minutes(m);
                Ok(())
            }
            Action::SetBackingDir(d) => {
                s.set_backing_dir(d);
                Ok(())
            }
            Action::SetCaptureDir(d) => {
                s.set_capture_dir(d);
                Ok(())
            }
            Action::NewRig => {
                s.new_rig();
                Ok(())
            }
            Action::RenameRig(name) => {
                s.rig.name = name;
                s.dirty = true;
                Ok(())
            }
            Action::SaveRig => {
                s.save_current_scene();
                let path = s
                    .rig_path
                    .clone()
                    .unwrap_or_else(|| paths::rigs_dir().join(paths::rig_file_name(&s.rig.name)));
                s.save_rig(&path)
            }
            Action::SaveAs(path) => {
                s.save_current_scene();
                s.save_rig(&path)
            }
            Action::OpenRig(path) | Action::OpenPath(path) => s.open_rig(&path),
            Action::Peak => {
                let peaks = s.take_peaks();
                let line = (0..UI_SLOTS)
                    .filter(|&i| s.plugins[i].is_some())
                    .map(|i| format!("{}:{:.3}", i + 1, peaks[i]))
                    .collect::<Vec<_>>()
                    .join(" ");
                let master = f32::from_bits(
                    s.engine
                        .shared
                        .master_peak
                        .swap(0, std::sync::atomic::Ordering::Relaxed),
                );
                s.say(format!("peaks since last check: {line} master:{master:.3}"));
                Ok(())
            }
            Action::Cpu => {
                let (now, ticks) = (Instant::now(), own_cpu_ticks());
                if let Some((t, o)) = self.cpu_script {
                    let dt = now.duration_since(t).as_secs_f64();
                    let pct = (ticks - o) as f64 / ticks_per_second() / dt * 100.0;
                    s.say(format!("cpu over {dt:.1}s: this process {pct:.1}%"));
                }
                self.cpu_script = Some((now, ticks));
                Ok(())
            }
            Action::Report => {
                let loaded: Vec<String> = (0..UI_SLOTS)
                    .filter_map(|i| {
                        s.rig.slots[i].instrument.as_ref().map(|r| {
                            format!(
                                "{}:{}{}{}",
                                i + 1,
                                r.name,
                                if s.plugins[i].is_some() {
                                    ""
                                } else {
                                    "(missing)"
                                },
                                if s.editors.has_editor(i) {
                                    "(editor)"
                                } else {
                                    ""
                                }
                            )
                        })
                    })
                    .collect();
                let (xruns, dsp) = s
                    .backend()
                    .map(|b| {
                        (
                            b.stats.xruns.load(std::sync::atomic::Ordering::Relaxed),
                            b.cpu_load(),
                        )
                    })
                    .unwrap_or_default();
                let held = s
                    .engine
                    .shared
                    .held
                    .iter()
                    .filter(|h| h.load(std::sync::atomic::Ordering::Relaxed) != 0)
                    .count();
                let midi = s
                    .midi_devices()
                    .iter()
                    .map(|d| {
                        format!(
                            "{}[{}{}]",
                            d.name,
                            match (d.input, d.enabled) {
                                (Some(n), _) => format!("in{n} "),
                                (None, false) => "off ".to_owned(),
                                (None, true) => "unconnected ".to_owned(),
                            },
                            match d.role {
                                settings::DeviceRole::Keyboard => "keyboard",
                                settings::DeviceRole::Controller => "controller",
                            }
                        )
                    })
                    .collect::<Vec<_>>()
                    .join(" ");
                let (rss_mb, threads) = own_memory();
                s.say(format!(
                    "state: rig '{}'{}, xruns {xruns}, DSP {dsp:.1}%, held keys {held}, \
                     RSS {rss_mb} MB, threads {threads}, midi {midi}, master {:.2}, vol1 {:.2}, targets {:?}, dock shows {:?}, \
                     popup {}, slots {}, perform {}, locked {}, macro1 {:.2}, gain1 {:.2}, position {:?}, tempo {:.0}, transport {} bar {} beat {} beats {}, run {:.1}s, underruns {}, stems {}, cue {:?}, pan1 {:.2}, fx1 {}, bypass1 {}, click {} level {:.2}, mapmode {}, mappings {}, params {:?}, reverbmix1 {:.2}, bright1 {:.2}",
                    s.rig.name,
                    if s.dirty { " (modified)" } else { "" },
                    s.rig.master_volume,
                    s.rig.slots[0].volume,
                    s.targets.iter().map(|i| i + 1).collect::<Vec<_>>(),
                    s.editors.shown().map(|i| i + 1),
                    self.ui.popup_open,
                    loaded.join(" "),
                    self.ui.perform,
                    self.ui.locked,
                    s.macro_value(0, 0),
                    s.sampler_gain(0).unwrap_or(-1.0),
                    s.position().map(|(a, b)| (a + 1, b + 1)),
                    s.current_tempo(),
                    if s.transport_running() { "running" } else if s.counting_in() { "counting" } else { "stopped" },
                    s.engine.shared.transport.position().0,
                    s.engine.shared.transport.position().1,
                    s.engine.shared.transport.beats_total(),
                    s.run_seconds(),
                    s.engine.shared.backing_underruns.load(std::sync::atomic::Ordering::Relaxed),
                    s.engine.shared.stems_playing.load(std::sync::atomic::Ordering::Relaxed),
                    s.cue(),
                    s.rig.slots[0].pan,
                    {
                        let f = &s.rig.slots[0].fx;
                        format!(
                            "{}{}{}{}",
                            u8::from(f.eq.enabled),
                            u8::from(f.comp.enabled),
                            u8::from(f.delay.enabled),
                            u8::from(f.reverb.enabled)
                        )
                    },
                    s.rig.slots[0]
                        .inserts
                        .iter()
                        .map(|i| i.as_ref().map_or('-', |i| if i.bypass { 'b' } else { 'o' }))
                        .collect::<String>(),
                    s.rig.transport.click,
                    s.rig.transport.click_volume,
                    s.map_mode,
                    s.mappings().len(),
                    s.param_report(),
                    s.rig.slots[0].fx.reverb.mix,
                    s.sampler_field(0, merlin_model::SamplerField::Brightness)
                        .unwrap_or(-9.0)
                ));
                Ok(())
            }
            Action::Menu(i) => {
                self.ui.force_menu = Some(i);
                Ok(())
            }
            Action::CloseMenu => {
                egui::Popup::close_all(self.egui.egui_ctx());
                Ok(())
            }
            Action::Quit => {
                self.quit = true;
                Ok(())
            }
        };
        if let Err(e) = result {
            self.session.say(format!("error: {e:#}"));
        }
    }

    fn run_script(&mut self) {
        while self
            .script
            .first()
            .is_some_and(|(at, _)| self.started.elapsed() >= *at)
        {
            let (at, action) = self.script.remove(0);
            println!("script @{:.1}s: {action:?}", at.as_secs_f64());
            self.apply(action);
            self.ask_redraw();
        }
    }

    fn tick(&mut self, now: Instant) {
        if now >= self.next_status {
            let active = self.ui.peaks_shown.iter().any(|&p| p > 0.002)
                || self.session.midi_active()
                || self.session.transport_running()
                || self.session.counting_in()
                || self.ui.held_key.is_some()
                || self.session.touch_learning().is_some()
                || self
                    .session
                    .engine
                    .shared
                    .held
                    .iter()
                    .any(|h| h.load(std::sync::atomic::Ordering::Relaxed) != 0);
            self.next_status = now
                + if active {
                    Duration::from_millis(50)
                } else {
                    Duration::from_millis(500)
                };
            let secs = now.duration_since(self.cpu_mark.0);
            if secs >= Duration::from_secs(2) {
                let ticks = own_cpu_ticks();
                self.ui.status.cpu_self =
                    ((ticks - self.cpu_mark.1) as f64 / ticks_per_second() / secs.as_secs_f64()
                        * 100.0) as f32;
                self.cpu_mark = (now, ticks);
                let up = self.started.elapsed().as_secs_f64().max(0.001);
                self.ui.status.frames_per_s = self.frames as f64 / up;
                self.ui.status.wakeups_per_s = self.wakeups as f64 / up;
            }
            self.ask_redraw();
        }
        self.session.poll_midi_hotplug();
        self.session.poll_touch_learn(now);
        self.session.poll_floating_editors();
        self.session.update_surfaces();
        self.session.update_loads();
        if self.session.restart_requested {
            // New Windows plugin support is read at startup: come back with it, same rig.
            self.session.restart_requested = false;
            self.session.shutdown();
            let err = guard::restart_from_autosave(None);
            self.session.say(format!("could not restart: {err}"));
        }
        self.session
            .autosave(Duration::from_secs(self.settings.autosave_secs), false);
    }

    fn next_deadline(&self) -> Option<Instant> {
        let script = self.script.first().map(|(at, _)| self.started + *at);
        [
            self.service_at,
            self.repaint_at,
            Some(self.next_status),
            self.session
                .editors
                .any_floating()
                .then(|| Instant::now() + Duration::from_millis(200)),
            script,
        ]
        .into_iter()
        .flatten()
        .min()
    }
}

impl ApplicationHandler<UserEvent> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.run.is_none() {
            match self.start(event_loop) {
                Ok(run) => self.run = Some(run),
                Err(e) => {
                    eprintln!("startup failed: {e:#}");
                    event_loop.exit();
                }
            }
        }
    }

    fn user_event(&mut self, _: &ActiveEventLoop, event: UserEvent) {
        let Some(run) = self.run.as_mut() else { return };
        run.wakeups += 1;
        run.session.handle_event(event);
        run.service_at = run.session.service();
        run.ask_redraw();
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _: WindowId, event: WindowEvent) {
        let Some(run) = self.run.as_mut() else { return };
        let response = run.egui.on_window_event(&run.window, &event);
        let event = if run.quit {
            WindowEvent::CloseRequested
        } else {
            event
        };
        // egui_winit reports repaint=true for RedrawRequested itself; honouring that would
        // redraw forever. Only input events may ask for a repaint.
        let wants_repaint = response.repaint && !matches!(event, WindowEvent::RedrawRequested);
        match event {
            WindowEvent::CloseRequested => {
                let mut run = self.run.take().unwrap();
                run.session.shutdown();
                let _ = std::fs::remove_file(paths::state_dir().join("running"));
                drop(run);
                event_loop.exit();
                return;
            }
            WindowEvent::Resized(size) => {
                if let (Some(w), Some(h)) =
                    (NonZeroU32::new(size.width), NonZeroU32::new(size.height))
                {
                    run.painter.on_window_resized(ViewportId::ROOT, w, h);
                }
                run.ask_redraw();
            }
            WindowEvent::RedrawRequested => {
                run.requested = false;
                run.redraw();
            }
            WindowEvent::HoveredFile(path) => {
                run.ui.drag_hint = Some(ui::drop_hint(&path));
                run.ask_redraw();
            }
            WindowEvent::HoveredFileCancelled => {
                run.ui.drag_hint = None;
                run.ask_redraw();
            }
            WindowEvent::DroppedFile(path) => {
                run.apply(Action::DropFile(path));
                run.ask_redraw();
            }
            _ => {}
        }
        if wants_repaint {
            run.ask_redraw();
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let Some(run) = self.run.as_mut() else { return };
        let now = Instant::now();
        run.service_at = run.session.service();
        run.run_script();
        if run.quit {
            run.ask_redraw();
            event_loop.set_control_flow(ControlFlow::Poll);
            return;
        }
        run.tick(now);
        if run.repaint_at.is_some_and(|t| t <= now) {
            run.repaint_at = None;
            run.ask_redraw();
        }
        event_loop.set_control_flow(match run.next_deadline() {
            Some(t) => ControlFlow::WaitUntil(t),
            None => ControlFlow::Wait,
        });
    }
}

/// `MERLIN_SCRIPT="2:load 1 Surge XT; 3:select 1; 4:on 60; 5:peak; 6:quit"` (slots are
/// 1-based, times are seconds since start).
fn parse_script() -> Vec<(Duration, Action)> {
    let Ok(script) = std::env::var("MERLIN_SCRIPT") else {
        return Vec::new();
    };
    let mut steps = script
        .split(';')
        .filter_map(|item| {
            let (at, cmd) = item.trim().split_once(':')?;
            let at = Duration::from_secs_f64(at.trim().parse().ok()?);
            let mut words = cmd.split_whitespace();
            let verb = words.next()?;
            let slot = |w: Option<&str>| w?.parse::<usize>().ok()?.checked_sub(1);
            let action = match verb {
                "load" => {
                    let i = slot(words.next())?;
                    Action::LoadNamed(i, words.collect::<Vec<_>>().join(" "))
                }
                "unload" => Action::Unload(slot(words.next())?),
                "retry" => Action::Retry(slot(words.next())?),
                "select" => Action::Select(slot(words.next())?, false),
                "layer" => Action::Select(slot(words.next())?, true),
                "mute" => Action::Mute(slot(words.next())?),
                "zone" => Action::Zone(
                    slot(words.next())?,
                    words.next()?.parse().ok()?,
                    words.next()?.parse().ok()?,
                ),
                // channel <slot> <1-16|omni>
                "channel" => Action::Channel(
                    slot(words.next())?,
                    match words.next()? {
                        "omni" => None,
                        n => Some(n.parse::<u8>().ok()?.checked_sub(1)?),
                    },
                ),
                "volume" => Action::Volume(slot(words.next())?, words.next()?.parse().ok()?),
                "transpose" => Action::Transpose(slot(words.next())?, words.next()?.parse().ok()?),
                "preset" => Action::SetPreset(
                    slot(words.next())?,
                    words.next()?.parse().ok()?,
                    words.next()?.parse().ok()?,
                ),
                // mainout auto | mainout <left port>|<right port>
                "mainout" => {
                    let ports = words.collect::<Vec<_>>().join(" ");
                    Action::SetMainOutput(
                        ports
                            .split_once('|')
                            .map(|(l, r)| [l.trim().to_owned(), r.trim().to_owned()]),
                    )
                }
                "settings" => match words.next() {
                    None => Action::ToggleSettings,
                    Some(t) => Action::ShowSettingsTab(match t {
                        "audio" => settings_ui::SettingsTab::Audio,
                        "midi" => settings_ui::SettingsTab::Midi,
                        "controls" => settings_ui::SettingsTab::Controls,
                        "windows" => settings_ui::SettingsTab::Windows,
                        "updates" => settings_ui::SettingsTab::Updates,
                        "perf" => settings_ui::SettingsTab::Performance,
                        _ => settings_ui::SettingsTab::Plugins,
                    }),
                },
                // touch <slot> [insertK]: start learning by touch (the test moves a parameter)
                "touch" => {
                    let slot = slot(words.next())?;
                    let insert = match words.next() {
                        Some(t) if t.starts_with("insert") => {
                            Some(t["insert".len()..].parse::<usize>().ok()?.checked_sub(1)?)
                        }
                        _ => None,
                    };
                    Action::TouchStart(slot, insert)
                }
                "drawer" => match (words.next()?, words.next()? == "on") {
                    ("fx", open) => Action::FxDrawer(open),
                    ("keys", open) => Action::KeysDrawer(open),
                    ("rack", open) => Action::RackCollapsed(!open),
                    ("autofit", on) => Action::AutoFit(on),
                    ("browser", open) => Action::BrowserOpen(open),
                    _ => return None,
                },
                "tab" => Action::Tab(match words.next()? {
                    "macros" => ui::DockTab::Macros,
                    "params" => ui::DockTab::Params,
                    "zones" => ui::DockTab::Zones,
                    t if t.starts_with("insert") => ui::DockTab::Insert(
                        t["insert".len()..].parse::<usize>().ok()?.checked_sub(1)?,
                    ),
                    _ => ui::DockTab::Plugin,
                }),
                "insert" => {
                    let i = slot(words.next())?;
                    let k = words.next()?.parse::<usize>().ok()?.checked_sub(1)?;
                    Action::LoadInsertNamed(i, k, words.collect::<Vec<_>>().join(" "))
                }
                "uninsert" => Action::UnloadInsert(
                    slot(words.next())?,
                    words.next()?.parse::<usize>().ok()?.checked_sub(1)?,
                ),
                "bypass" => Action::BypassInsert(
                    slot(words.next())?,
                    words.next()?.parse::<usize>().ok()?.checked_sub(1)?,
                ),
                "master" => Action::SetMaster(words.next()?.parse().ok()?),
                "fx" => Action::FxPreset(slot(words.next())?, words.next()?.to_string()),
                "install" => Action::InstallWindows(words.collect::<Vec<_>>().join(" ")),
                "launch" => Action::LaunchWindowsApp(words.collect::<Vec<_>>().join(" ")),
                "sync" => Action::SyncWindows,
                // map <cc N | note N | program> <action> [pickup]: any device, any channel
                "map" => {
                    use merlin_model::{Mapping, Source, SourceKind, Takeover};
                    let kind = match words.next()? {
                        "cc" => SourceKind::Cc {
                            number: words.next()?.parse().ok()?,
                        },
                        "note" => SourceKind::Note {
                            key: words.next()?.parse().ok()?,
                        },
                        "program" => SourceKind::Program,
                        _ => return None,
                    };
                    let action = parse_control_action(&mut words)?;
                    // Then any of: pickup scale invert rel2c relsb reloff step=0.1 min=0.2 max=0.8 soft hard
                    let mut takeover = Takeover::Jump;
                    let mut glue = merlin_model::Glue::default();
                    let mut switch = merlin_model::Switch::Press;
                    for w in words.by_ref() {
                        use merlin_model::{Curve, Relative};
                        match w {
                            "latch" => switch = merlin_model::Switch::Level,
                            "pickup" => takeover = Takeover::Pickup,
                            "scale" => takeover = Takeover::Scale,
                            "invert" => glue.invert = true,
                            "rel2c" => glue.relative = Relative::TwosComplement,
                            "relsb" => glue.relative = Relative::SignedBit,
                            "reloff" => glue.relative = Relative::BinaryOffset,
                            "soft" => glue.curve = Curve::Soft,
                            "hard" => glue.curve = Curve::Hard,
                            _ => match w.split_once('=') {
                                Some(("step", v)) => glue.step = v.parse().ok()?,
                                Some(("min", v)) => glue.min = v.parse().ok()?,
                                Some(("max", v)) => glue.max = v.parse().ok()?,
                                _ => return None,
                            },
                        }
                    }
                    let mut mapping = Mapping::new(
                        Source {
                            device: None,
                            channel: None,
                            kind,
                        },
                        action,
                    );
                    mapping.takeover = takeover;
                    mapping.glue = glue;
                    mapping.switch = switch;
                    Action::AddMapping(mapping)
                }
                "mapmode" => Action::SetMapMode(words.next()? == "on"),
                // mapparam <cc N> <slot> <inst|insertK> <name part> [pickup]: a plugin parameter
                // on a control; the name is the first parameter that contains the text.
                "mapparam" => {
                    let kind = match words.next()? {
                        "cc" => merlin_model::SourceKind::Cc {
                            number: words.next()?.parse().ok()?,
                        },
                        _ => return None,
                    };
                    let slot = slot(words.next())?;
                    let insert = match words.next()? {
                        "inst" => None,
                        t if t.starts_with("insert") => {
                            Some(t["insert".len()..].parse::<usize>().ok()?.checked_sub(1)?)
                        }
                        _ => return None,
                    };
                    let mut rest: Vec<&str> = words.collect();
                    let pickup = rest.last() == Some(&"pickup");
                    if pickup {
                        rest.pop();
                    }
                    Action::MapParamNamed(kind, slot, insert, rest.join(" "), pickup)
                }
                // learn <action>: the next control the user (or the test controller) sends
                "learn" => Action::LearnControl(parse_control_action(&mut words)?),
                // midi <keyboard|controller|off> <device name>
                "midi" => {
                    let role = words.next()?;
                    let name = words.collect::<Vec<_>>().join(" ");
                    let (role, on) = match role {
                        "keyboard" => (settings::DeviceRole::Keyboard, true),
                        "controller" => (settings::DeviceRole::Controller, true),
                        "off" => (settings::DeviceRole::Keyboard, false),
                        _ => return None,
                    };
                    Action::SetMidiDevice(name, role, on)
                }
                "drop" => Action::DropFile(PathBuf::from(words.collect::<Vec<_>>().join(" "))),
                "runtime" => Action::InstallRuntime,
                "checkupdate" => Action::CheckUpdate,
                // macro <slot> <n> <0..1>; macrotarget <slot> <n> <param name part | sampler:level>
                "macro" => Action::SetMacro(
                    slot(words.next())?,
                    slot(words.next())?,
                    words.next()?.parse().ok()?,
                ),
                "macrotarget" => Action::AddMacroParamNamed(
                    slot(words.next())?,
                    slot(words.next())?,
                    words.collect::<Vec<_>>().join(" "),
                ),
                // song add | song name N text | song tempo N bpm; part add N | part snapshot N K S;
                // go S P; nextpart prevpart nextsong prevsong
                "song" => match words.next()? {
                    "add" => Action::AddSong,
                    "name" => {
                        let n = slot(words.next())?;
                        Action::EditSong(n, ui::SongEdit::Name(words.collect::<Vec<_>>().join(" ")))
                    }
                    "tempo" => Action::EditSong(
                        slot(words.next())?,
                        ui::SongEdit::Tempo(words.next()?.parse().ok()?),
                    ),
                    _ => return None,
                },
                "part" => match words.next()? {
                    "add" => Action::AddPart(slot(words.next())?, true),
                    "empty" => Action::AddPart(slot(words.next())?, false),
                    "snapshot" => Action::EditPart(
                        slot(words.next())?,
                        slot(words.next())?,
                        ui::PartEdit::Snapshot(Some(slot(words.next())?)),
                    ),
                    "tempo" => Action::EditPart(
                        slot(words.next())?,
                        slot(words.next())?,
                        ui::PartEdit::Tempo(Some(words.next()?.parse().ok()?)),
                    ),
                    _ => return None,
                },
                "setlist" => Action::ToggleSetlist,
                // transport (start|stop) ; tempo N ; click on|off ; tap ; countin N
                "transport" => match words.next()? {
                    "start" => Action::TransportStart,
                    "stop" => Action::TransportStop,
                    _ => return None,
                },
                "tempo" => Action::SetTempo(words.next()?.parse().ok()?),
                "metro" => Action::SetMetronome(words.next()? == "on"),
                "click" => Action::SetClickState(words.next()? == "on"),
                "tap" => Action::TapTempo,
                "countin" => Action::SetCountIn(words.next()?.parse().ok()?),
                // marker S bar beat part N | next | stop | cue text | pc ch prog | cc ch num val
                "marker" => {
                    use merlin_model::{Marker, MarkerAction as M};
                    let n = slot(words.next())?;
                    let bar = words.next()?.parse().ok()?;
                    let beat = words.next()?.parse().ok()?;
                    let action = match words.next()? {
                        "part" => M::GoToPart {
                            part: slot(words.next())?.try_into().ok()?,
                        },
                        "next" => M::NextPart,
                        "stop" => M::Stop,
                        "cue" => M::Cue {
                            text: words.collect::<Vec<_>>().join(" "),
                        },
                        "pc" => M::ProgramChange {
                            channel: words.next()?.parse().ok()?,
                            program: words.next()?.parse().ok()?,
                        },
                        "cc" => M::ControlChange {
                            channel: words.next()?.parse().ok()?,
                            number: words.next()?.parse().ok()?,
                            value: words.next()?.parse().ok()?,
                        },
                        _ => return None,
                    };
                    Action::AddMarker(n, Marker { bar, beat, action })
                }
                // track add S path | track gain S K v | track cue S K on|off | track mute S K on|off
                "track" => match words.next()? {
                    "add" => {
                        let n = slot(words.next())?;
                        Action::AddTrack(n, PathBuf::from(words.collect::<Vec<_>>().join(" ")))
                    }
                    "gain" => Action::EditTrack(
                        slot(words.next())?,
                        slot(words.next())?,
                        session::TrackEdit::Gain(words.next()?.parse().ok()?),
                    ),
                    "cue" => Action::EditTrack(
                        slot(words.next())?,
                        slot(words.next())?,
                        session::TrackEdit::Cue(words.next()? == "on"),
                    ),
                    "mute" => Action::EditTrack(
                        slot(words.next())?,
                        slot(words.next())?,
                        session::TrackEdit::Mute(words.next()? == "on"),
                    ),
                    _ => return None,
                },
                "go" => Action::GoTo(slot(words.next())?, slot(words.next())?),
                "nextpart" => Action::NextPart,
                "prevpart" => Action::PrevPart,
                "nextsong" => Action::NextSong,
                "prevsong" => Action::PrevSong,
                // perform on|off, lock on|off, fullscreen
                "perform" => Action::SetPerform(words.next()? == "on"),
                "lock" => Action::SetLock(words.next()? == "on"),
                "fullscreen" => Action::ToggleFullscreen,
                // snapshot save | recall N | update N | remove N (N counted from 1)
                "snapshot" => {
                    let verb = words.next()?;
                    let n = |w: Option<&str>| w?.parse::<usize>().ok()?.checked_sub(1);
                    match verb {
                        "save" => Action::AddSnapshot,
                        "recall" => Action::RecallSnapshot(n(words.next())?),
                        "update" => Action::UpdateSnapshot(n(words.next())?),
                        "remove" => Action::RemoveSnapshot(n(words.next())?),
                        _ => return None,
                    }
                }
                "fit" => Action::Fit,
                "scroll" => {
                    Action::Scroll(words.next()?.parse().ok()?, words.next()?.parse().ok()?)
                }
                "popout" => Action::PopOut(words.next()?.parse().ok()?),
                "floatscale" => {
                    Action::FloatScale(words.next()?.parse().ok()?, words.next()?.parse().ok()?)
                }
                "popin" => Action::PopIn(words.next()?.parse().ok()?),
                "on" => Action::NoteOn(words.next()?.parse().ok()?),
                "off" => Action::NoteOff(words.next()?.parse().ok()?),
                "panic" => Action::Panic,
                "capture" => Action::CaptureMidi,
                "new" => Action::NewRig,
                "save" => Action::SaveAs(PathBuf::from(words.collect::<Vec<_>>().join(" "))),
                "open" => Action::OpenPath(PathBuf::from(words.collect::<Vec<_>>().join(" "))),
                "peak" => Action::Peak,
                "cpu" => Action::Cpu,
                "report" => Action::Report,
                "menu" => Action::Menu(slot(words.next())?),
                "closemenu" => Action::CloseMenu,
                "quit" => Action::Quit,
                _ => return None,
            };
            Some((at, action))
        })
        .collect::<Vec<_>>();
    // Run in time order whatever order they were written in (the sort keeps ties in order).
    steps.sort_by_key(|(at, _)| *at);
    steps
}

/// `pan 1`, `fx 1 reverb`, `insert 1 2`, `clicklevel`, `tempo`, `select 2`, `layer 3`, `mute 1`, `volume 1`, `byvalue 1`, `master`, `macro 1 3`, `selmacro 2`, `selvolume`, `snap 2`, `snapbyvalue 1`, `snapnext`, `snapprev`, `next`, `prev`, `panic`
/// (slots counted from 1), for the script's `map` and `learn`.
fn parse_control_action(
    words: &mut std::str::SplitWhitespace<'_>,
) -> Option<merlin_model::ControlAction> {
    use merlin_model::{ControlAction as A, FxKind};
    let num = |w: Option<&str>| w?.parse::<u8>().ok()?.checked_sub(1);
    Some(match words.next()? {
        "select" => A::SelectSlot {
            slot: num(words.next())?,
        },
        "layer" => A::ToggleLayer {
            slot: num(words.next())?,
        },
        "mute" => A::ToggleMute {
            slot: num(words.next())?,
        },
        "volume" => A::SlotVolume {
            slot: num(words.next())?,
        },
        "byvalue" => A::SelectByValue {
            base: num(words.next())?,
        },
        "master" => A::MasterVolume,
        "selmacro" => A::SelectedMacro {
            index: num(words.next())?,
        },
        "selvolume" => A::SelectedVolume,
        "selknob" => A::SelectedKnob {
            index: num(words.next())?,
        },
        "macro" => A::SlotMacro {
            slot: num(words.next())?,
            index: num(words.next())?,
        },
        "snap" => A::RecallSnapshot {
            snapshot: num(words.next())?,
        },
        "snapbyvalue" => A::RecallSnapshotByValue {
            base: num(words.next())?,
        },
        "snapnext" => A::NextSnapshot,
        "transport" => A::TransportToggle,
        "click" => A::ClickToggle,
        "metronome" => A::MetronomeToggle,
        "tap" => A::TapTempo,
        "nextpart" => A::NextPart,
        "prevpart" => A::PrevPart,
        "nextsong" => A::NextSong,
        "prevsong" => A::PrevSong,
        "snapprev" => A::PrevSnapshot,
        "next" => A::NextSlot,
        "prev" => A::PrevSlot,
        "panic" => A::Panic,
        "capture" => A::CaptureMidi,
        "pan" => A::SlotPan {
            slot: num(words.next())?,
        },
        "fx" => A::ToggleFx {
            slot: num(words.next())?,
            fx: match words.next()? {
                "eq" => FxKind::Eq,
                "comp" => FxKind::Comp,
                "delay" => FxKind::Delay,
                "reverb" => FxKind::Reverb,
                _ => return None,
            },
        },
        "insert" => A::ToggleInsertBypass {
            slot: num(words.next())?,
            insert: num(words.next())?,
        },
        "clicklevel" => A::ClickLevel,
        "tempo" => A::Tempo,
        // fxknob <slot> <eq|compressor|delay|reverb> <knob name, spaces as _>
        "fxknob" => {
            let slot = num(words.next())?;
            let (group, name) = (words.next()?, words.next()?);
            let knob = merlin_model::EQ_KNOBS
                .iter()
                .chain(&merlin_model::COMP_KNOBS)
                .chain(&merlin_model::DELAY_KNOBS)
                .chain(&merlin_model::REVERB_KNOBS)
                .find(|k| {
                    k.group.eq_ignore_ascii_case(group) && k.name.replace(' ', "_") == name
                })?;
            A::SlotParam {
                slot,
                insert: merlin_model::FX_KNOBS,
                id: knob.id,
            }
        }
        // soundknob <slot> <level|attack|release|brightness|resonance|tuning|voices>
        "soundknob" => {
            let slot = num(words.next())?;
            let field = words.next()?;
            let id = merlin_model::SamplerField::ALL
                .iter()
                .position(|f| f.name().eq_ignore_ascii_case(field))?;
            A::SlotParam {
                slot,
                insert: merlin_model::SOUND_KNOBS,
                id: id as u32,
            }
        }
        _ => return None,
    })
}

/// Set (to the runtime's yabridge folder) when this process pointed `WINELOADER` and `PATH` at
/// the downloaded Wine and yabridge, so that a restart can tell them from the user's own.
const MANAGED_RUNTIME: &str = "MERLIN_MANAGED_RUNTIME";

fn main() -> Result<()> {
    let args = parse_args();
    paths::migrate_legacy_rigs();
    let settings_file = paths::settings_file();
    let settings = match Settings::load(&settings_file) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("{}: {e:#}; using defaults", settings_file.display());
            Settings::default()
        }
    };
    // Windows plugins: yabridge's chainloader finds Wine and its host binary through these.
    // SAFETY: set before any other thread exists.
    unsafe {
        // A restart keeps the environment. What the previous run set for the downloaded runtime
        // is taken back, or it would pass for the user's own Wine and a newer runtime would
        // never be picked up (the app would restart into the old one forever).
        if let Some(yabridge) = std::env::var_os(MANAGED_RUNTIME) {
            std::env::remove_var(MANAGED_RUNTIME);
            std::env::remove_var("WINELOADER");
            let path = std::env::var_os("PATH").unwrap_or_default();
            let kept: Vec<_> = std::env::split_paths(&path)
                .filter(|dir| dir.as_os_str() != yabridge)
                .collect();
            if let Ok(joined) = std::env::join_paths(kept) {
                std::env::set_var("PATH", joined);
            }
        }
    }
    // The plugin hosts start Wine themselves, from this environment: no Mono/Gecko dialogs there
    // either (a prefix that a newer Wine has to upgrade would open one).
    // SAFETY: still before any other thread exists.
    unsafe {
        std::env::set_var(
            "WINEDLLOVERRIDES",
            merlin_wine::dll_overrides(std::env::var("WINEDLLOVERRIDES").ok().as_deref()),
        );
    }
    // The downloaded runtime fills in what neither the settings nor the environment name.
    let managed = (settings.wine_loader.is_none() && std::env::var_os("WINELOADER").is_none())
        .then(|| merlin_wine::runtime::installed(&paths::runtime_dir()))
        .flatten();
    unsafe {
        if let Some(w) = &settings.wine_loader {
            std::env::set_var("WINELOADER", w);
        }
        if let Some(y) = &settings.yabridge_dir {
            let path = std::env::var("PATH").unwrap_or_default();
            std::env::set_var("PATH", format!("{}:{path}", y.display()));
        }
        if let Some(m) = &managed {
            let path = std::env::var("PATH").unwrap_or_default();
            std::env::set_var("WINELOADER", &m.wine);
            std::env::set_var("PATH", format!("{}:{path}", m.yabridge_dir.display()));
            std::env::set_var(MANAGED_RUNTIME, &m.yabridge_dir);
        }
    }
    merlin_audio_io::preload_pipewire_jack();
    let guard = guard::Guard::new();
    let cli_dirs = args.plugin_dirs.clone();
    let event_loop = EventLoop::<UserEvent>::with_user_event()
        .with_x11()
        .build()?;
    let mut app = App {
        proxy: event_loop.create_proxy(),
        args,
        settings,
        cli_dirs,
        guard,
        run: None,
    };
    app.guard.spawn_watchdog();
    event_loop.run_app(&mut app)?;
    // Give plugin background threads (JUCE) a moment to wind down.
    std::thread::sleep(Duration::from_millis(100));
    // Leave without running the plugins' static destructors: yabridge's (C++) cancel threads
    // there, and about one exit in fifteen died with "exception not rethrown". Everything that
    // matters (the autosave, the marker, the plugins) was dealt with by the shutdown above.
    {
        use std::io::Write;
        let _ = std::io::stdout().flush();
        let _ = std::io::stderr().flush();
    }
    // SAFETY: _exit never returns and nothing is left to clean up.
    unsafe { libc::_exit(0) }
}
