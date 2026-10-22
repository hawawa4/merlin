//! Merlin: a live instrument host. One window: the rack on top, the selected
//! instrument's editor embedded below, a keyboard map at the bottom.
//!
//! egui/wgpu on our own winit loop, forced to X11 (XWayland on Wayland desktops), which is what
//! lets plugin editors embed as child windows (plan D7/D8).
//!
//! `merlin [--plugin-dir DIR]... [--rig FILE] [--fresh]`
//! Test hook: `MERLIN_SCRIPT="3:load 1 Surge XT; 5:on 60; 6:peak; 8:quit"`.

mod browser;
mod guard;
mod keyboard;
mod marker;
mod paths;
mod perf;
mod quirks;
mod scan_worker;
mod session;
mod settings;
mod settings_ui;
mod ui;

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
use winit::platform::x11::EventLoopBuilderExtX11;
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

impl App {
    fn start(&mut self, event_loop: &ActiveEventLoop) -> Result<Running> {
        let window = Arc::new(
            event_loop.create_window(
                Window::default_attributes()
                    .with_title("Merlin")
                    .with_inner_size(winit::dpi::LogicalSize::new(1280.0, 900.0)),
            )?,
        );
        let xid = x11_window_id(&window)?;
        println!("main window X11 id {xid:#x}");

        let ctx = egui::Context::default();
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
        // The dock is unmapped while an egui popup is open, and while the effects tab is shown
        // in its place.
        let covered =
            self.ui.popup_open || self.ui.settings_open || self.ui.tab == ui::DockTab::Effects;
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
            Action::Volume(i, v) => {
                s.set_volume(i, v);
                Ok(())
            }
            Action::SetPreset(i, bank, program) => {
                s.set_preset(i, bank, program);
                Ok(())
            }
            Action::SamplerGain(i, g) => {
                s.set_sampler_gain(i, g);
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
            Action::SyncWindows => s.sync_windows_plugins(),
            Action::InstallRuntime => s.install_runtime(),
            Action::ToggleFavorite(k) => {
                if let Some(f) = s.found.get(k).cloned() {
                    s.toggle_favorite(&f);
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
                        ui::DockTab::Effects => {}
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
                match s
                    .effects
                    .iter()
                    .find(|f| f.info.name.to_lowercase().contains(&q))
                    .cloned()
                {
                    Some(f) => s.load_insert(i, k, &f),
                    None => Err(anyhow::anyhow!("no effect plugin matching {query:?}")),
                }
            }
            Action::UnloadInsert(i, k) => {
                s.unload_insert(i, k);
                if self.ui.tab == ui::DockTab::Insert(k) {
                    self.ui.tab = ui::DockTab::Effects;
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
                let path = s
                    .rig_path
                    .clone()
                    .unwrap_or_else(|| paths::rigs_dir().join(paths::rig_file_name(&s.rig.name)));
                s.save_rig(&path)
            }
            Action::SaveAs(path) => s.save_rig(&path),
            Action::OpenRig(path) | Action::OpenPath(path) => s.open_rig(&path),
            Action::Peak => {
                let peaks = s.take_peaks();
                let line = (0..UI_SLOTS)
                    .filter(|&i| s.plugins[i].is_some())
                    .map(|i| format!("{}:{:.3}", i + 1, peaks[i]))
                    .collect::<Vec<_>>()
                    .join(" ");
                s.say(format!("peaks since last check: {line}"));
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
                let (rss_mb, threads) = own_memory();
                s.say(format!(
                    "state: rig '{}'{}, xruns {xruns}, DSP {dsp:.1}%, held keys {held}, \
                     RSS {rss_mb} MB, threads {threads}, targets {:?}, dock shows {:?}, \
                     popup {}, slots {}",
                    s.rig.name,
                    if s.dirty { " (modified)" } else { "" },
                    s.targets.iter().map(|i| i + 1).collect::<Vec<_>>(),
                    s.editors.shown().map(|i| i + 1),
                    self.ui.popup_open,
                    loaded.join(" ")
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
                || self.ui.held_key.is_some()
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
    script
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
                "select" => Action::Select(slot(words.next())?, false),
                "layer" => Action::Select(slot(words.next())?, true),
                "mute" => Action::Mute(slot(words.next())?),
                "zone" => Action::Zone(
                    slot(words.next())?,
                    words.next()?.parse().ok()?,
                    words.next()?.parse().ok()?,
                ),
                "volume" => Action::Volume(slot(words.next())?, words.next()?.parse().ok()?),
                "transpose" => Action::Transpose(slot(words.next())?, words.next()?.parse().ok()?),
                "preset" => Action::SetPreset(
                    slot(words.next())?,
                    words.next()?.parse().ok()?,
                    words.next()?.parse().ok()?,
                ),
                "settings" => match words.next() {
                    None => Action::ToggleSettings,
                    Some(t) => Action::ShowSettingsTab(match t {
                        "audio" => settings_ui::SettingsTab::Audio,
                        "midi" => settings_ui::SettingsTab::Midi,
                        "windows" => settings_ui::SettingsTab::Windows,
                        "perf" => settings_ui::SettingsTab::Performance,
                        _ => settings_ui::SettingsTab::Plugins,
                    }),
                },
                "tab" => Action::Tab(match words.next()? {
                    "fx" => ui::DockTab::Effects,
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
                "sync" => Action::SyncWindows,
                "runtime" => Action::InstallRuntime,
                "fit" => Action::Fit,
                "on" => Action::NoteOn(words.next()?.parse().ok()?),
                "off" => Action::NoteOff(words.next()?.parse().ok()?),
                "panic" => Action::Panic,
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
        .collect()
}

fn main() -> Result<()> {
    let args = parse_args();
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
    Ok(())
}
