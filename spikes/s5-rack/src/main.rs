//! S5: a rack of 8 CLAP slots, one editor dock, a keyboard map.
//!
//! egui/wgpu on our own winit loop, forced to X11. Every slot holds a CLAP plugin (Linux or
//! a Windows plugin behind yabridge); one JACK callback runs all of them. The editor dock
//! shows the selected slot's editor; other editors stay alive, hidden.
//!
//! Usage: `cargo run -p s5-rack --release -- [dir-with-.clap-files ...]`
//! Env: `S5_PRELOAD="Surge XT; Six Sines (Windows)"` loads plugins into slots 0.. at start,
//! `S5_SCRIPT="2:select 1; 3:on 60; 4:peak; 5:quit"` runs timed actions, `S5_EDITOR_CACHE=n`
//! caps the number of live editors (default 8).

mod audio;
mod dock;
mod host;
mod keyboard;
mod slot;

use std::num::NonZeroU32;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use clack_extensions::gui::{GuiApiType, GuiConfiguration, GuiSize, Window as ClapWindow};
use egui::ViewportId;
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::platform::x11::EventLoopBuilderExtX11;
use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
use winit::window::{Window, WindowId};

use audio::{Audio, Cmd, SLOTS, SlotAudio};
use dock::Dock;
use host::UserEvent;
use keyboard::{COLORS, ZoneView};
use slot::{Found, Slot};

/// Destroying a bridged editor in the same turn as showing another one makes Wine abort the
/// plugin host with BadWindow (found in S5), so surplus editors are destroyed a little later.
const EVICT_DELAY: Duration = Duration::from_millis(300);

struct Running {
    window: Arc<Window>,
    egui: egui_winit::State,
    painter: egui_wgpu::winit::Painter,
    dock: Dock,
    // Field order matters on drop: audio first, then slots (instances), then the rest.
    audio: Option<Audio>,
    slots: [Option<Slot>; SLOTS],
    proxy: EventLoopProxy<UserEvent>,
    found: Vec<Found>,
    selected: Option<usize>,
    editor_cache: usize,
    held: Option<u8>,
    peaks_shown: [f32; SLOTS],
    peaks_max: [f32; SLOTS],
    log: Vec<String>,
    last_switch: Option<(usize, Duration)>,
    popup_open: bool,
    force_menu: Option<usize>,
    /// When to destroy surplus editors (never in the same turn as a switch, see `tick`).
    evict_at: Option<Instant>,
    frames: u64,
    wakeups: u64,
    started: Instant,
    next_status: Instant,
    status_period: Duration,
    repaint_at: Option<Instant>,
    cpu_mark: (Instant, u64),
    cpu_self: f32,
    cpu_script: Option<(Instant, u64, u64, Vec<ProcTicks>)>,
    script: Vec<(Duration, Action)>,
    quit: bool,
    requested: bool,
}

struct App {
    proxy: EventLoopProxy<UserEvent>,
    dirs: Vec<PathBuf>,
    run: Option<Running>,
}

fn x11_window_id(window: &Window) -> Result<u32> {
    match window.window_handle()?.as_raw() {
        RawWindowHandle::Xlib(h) => Ok(h.window as u32),
        RawWindowHandle::Xcb(h) => Ok(h.window.get()),
        other => Err(anyhow!("not an X11 window: {other:?}")),
    }
}

/// utime + stime (clock ticks) of one process, from /proc.
fn proc_ticks(stat: &str) -> Option<(String, u32, u64)> {
    let open = stat.find('(')?;
    let close = stat.rfind(')')?;
    let comm = stat[open + 1..close].to_string();
    let f: Vec<&str> = stat[close + 2..].split(' ').collect();
    // After the comm: state, ppid, ... utime is field 14 overall, i.e. index 11 here.
    let ppid = f.get(1)?.parse().ok()?;
    let ticks = f.get(11)?.parse::<u64>().ok()? + f.get(12)?.parse::<u64>().ok()?;
    Some((comm, ppid, ticks))
}

/// (pid, command name, CPU ticks) of one process.
type ProcTicks = (u32, String, u64);

/// CPU ticks of this process, and of it plus all its descendants and any wineserver.
fn cpu_ticks() -> (u64, u64) {
    let (own, tree, _) = cpu_snapshot();
    (own, tree)
}

/// Same, plus the per-process ticks of that family as (pid, command name, ticks).
fn cpu_snapshot() -> (u64, u64, Vec<ProcTicks>) {
    let me = std::process::id();
    let mut procs = Vec::new();
    if let Ok(rd) = std::fs::read_dir("/proc") {
        for e in rd.flatten() {
            let Ok(pid) = e.file_name().to_string_lossy().parse::<u32>() else {
                continue;
            };
            if let Ok(s) = std::fs::read_to_string(e.path().join("stat"))
                && let Some((comm, ppid, ticks)) = proc_ticks(&s)
            {
                procs.push((pid, ppid, comm, ticks));
            }
        }
    }
    let mut family = vec![me];
    loop {
        let before = family.len();
        for (pid, ppid, ..) in &procs {
            if family.contains(ppid) && !family.contains(pid) {
                family.push(*pid);
            }
        }
        if family.len() == before {
            break;
        }
    }
    let own = procs
        .iter()
        .find(|p| p.0 == me)
        .map(|p| p.3)
        .unwrap_or_default();
    let members: Vec<ProcTicks> = procs
        .iter()
        .filter(|(pid, _, comm, _)| family.contains(pid) || comm == "wineserver")
        .map(|p| (p.0, p.2.clone(), p.3))
        .collect();
    let tree = members.iter().map(|m| m.2).sum();
    (own, tree, members)
}

impl App {
    fn start(&mut self, event_loop: &ActiveEventLoop) -> Result<Running> {
        let window = Arc::new(
            event_loop.create_window(
                Window::default_attributes()
                    .with_title("Merlin S5: rack")
                    .with_inner_size(winit::dpi::LogicalSize::new(1280.0, 900.0)),
            )?,
        );
        let xid = x11_window_id(&window)?;
        println!(
            "main window X11 id {xid:#x}, scale factor {}",
            window.scale_factor()
        );

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

        let mut dock = Dock::new(xid)?;
        dock.set_visible(true)?;
        let audio = audio::start()?;
        let found = slot::discover(&self.dirs);
        println!("{} plugins found:", found.len());
        for f in &found {
            println!("  {}  ({})", f.label, f.path.display());
        }

        let mut run = Running {
            window,
            egui,
            painter,
            dock,
            audio: Some(audio),
            slots: std::array::from_fn(|_| None),
            proxy: self.proxy.clone(),
            found,
            selected: None,
            editor_cache: std::env::var("S5_EDITOR_CACHE")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(SLOTS),
            held: None,
            peaks_shown: [0.0; SLOTS],
            peaks_max: [0.0; SLOTS],
            log: Vec::new(),
            last_switch: None,
            popup_open: false,
            force_menu: None,
            evict_at: None,
            frames: 0,
            wakeups: 0,
            started: Instant::now(),
            next_status: Instant::now(),
            status_period: Duration::from_millis(500),
            repaint_at: None,
            cpu_mark: (Instant::now(), cpu_ticks().0),
            cpu_self: 0.0,
            cpu_script: None,
            script: parse_script(),
            quit: false,
            requested: false,
        };
        run.say(format!(
            "JACK at {} Hz",
            run.audio.as_ref().unwrap().sample_rate
        ));
        // Lay out once so the dock has its real rect before the first editor is created.
        run.redraw();
        if let Ok(list) = std::env::var("S5_PRELOAD") {
            for (i, query) in list.split(';').map(str::trim).enumerate().take(SLOTS) {
                match run.find(query) {
                    Some(k) => run.apply(Action::Load(i, k)),
                    None => run.say(format!("error: no plugin matches '{query}'")),
                }
            }
        }
        run.ask_redraw();
        Ok(run)
    }
}

impl Running {
    /// Log to the on-screen panel and stdout (scripted test runs sync on stdout).
    fn say(&mut self, line: impl Into<String>) {
        let line = line.into();
        println!("{line}");
        self.log.push(line);
    }

    fn ask_redraw(&mut self) {
        self.requested = true;
        self.window.request_redraw();
    }

    fn find(&self, query: &str) -> Option<usize> {
        let q = query.to_lowercase();
        self.found
            .iter()
            .position(|f| f.label.to_lowercase() == q)
            .or_else(|| {
                self.found
                    .iter()
                    .position(|f| f.label.to_lowercase().contains(&q))
            })
    }

    fn audio(&mut self) -> &mut Audio {
        self.audio.as_mut().expect("audio running")
    }

    fn push_route(&mut self, i: usize) {
        if let Some(route) = self.slots[i].as_ref().map(|s| s.route) {
            self.audio().send(Cmd::Route(i, route));
        }
    }

    // ---- slots ----------------------------------------------------------------------

    fn load(&mut self, i: usize, k: usize) -> Result<()> {
        if self.slots[i].is_some() {
            self.unload(i);
        }
        let (path, windows) = (self.found[k].path.clone(), self.found[k].windows);
        let mut slot = Slot::load(&self.proxy, i, &path, windows)?;
        let rate = self.audio().sample_rate as f64;
        let audio = SlotAudio::new(&mut slot.instance, rate)?;
        self.say(format!(
            "slot {} <- {}{}: loaded in {:?}",
            i + 1,
            slot.name,
            if windows { " (Windows)" } else { "" },
            slot.load_time
        ));
        self.slots[i] = Some(slot);
        self.audio().send(Cmd::Install(i, audio));
        self.select(i, false);
        Ok(())
    }

    fn unload(&mut self, i: usize) {
        if self.slots[i].is_none() {
            return;
        }
        self.destroy_editor(i);
        if let Some(a) = self.audio().remove(i)
            && let Some(slot) = self.slots[i].as_mut()
        {
            slot.instance.deactivate(a.processor.stop_processing());
        }
        if let Some(slot) = self.slots[i].take() {
            self.say(format!("slot {} unloaded ({})", i + 1, slot.name));
        }
        if self.selected == Some(i) {
            self.selected = None;
            let _ = self.dock.show(None);
        }
    }

    /// Exclusive selection (`layer` false) or toggling a layer (`layer` true).
    fn select(&mut self, i: usize, layer: bool) {
        if self.slots[i].is_none() {
            return;
        }
        if layer {
            let s = self.slots[i].as_mut().unwrap();
            s.route.active = !s.route.active;
            self.push_route(i);
        } else {
            for j in 0..SLOTS {
                if let Some(s) = self.slots[j].as_mut() {
                    s.route.active = j == i;
                    self.push_route(j);
                }
            }
        }
        self.show_editor(i);
    }

    // ---- editors --------------------------------------------------------------------

    /// Show this slot's editor in the dock, creating it on first use; hide the previous one.
    fn show_editor(&mut self, i: usize) {
        let t = Instant::now();
        if let Some(prev) = self.dock.shown().filter(|&p| p != i)
            && let Some(s) = self.slots[prev].as_mut()
            && let Some(gui) = s.gui()
        {
            let _ = gui.hide(&s.instance.plugin_handle());
        }
        let first = !self.slots[i].as_ref().is_some_and(|s| s.editor.created);
        if first && let Err(e) = self.create_editor(i) {
            self.say(format!("error: slot {} editor: {e:#}", i + 1));
            return;
        }
        let Some(s) = self.slots[i].as_mut() else {
            return;
        };
        if let Some(gui) = s.gui() {
            let _ = gui.show(&s.instance.plugin_handle());
        }
        s.editor.last_shown = Instant::now();
        let (w, h) = (s.editor.size.width, s.editor.size.height);
        let _ = self.dock.set_canvas_size(i, w, h);
        let _ = self.dock.scroll_to(i, s.editor.scroll.0, s.editor.scroll.1);
        let _ = self.dock.show(Some(i));
        self.selected = Some(i);
        self.last_switch = Some((i, t.elapsed()));
        self.say(format!(
            "editor {} {} in {:?}",
            i + 1,
            if first { "created+shown" } else { "shown" },
            t.elapsed()
        ));
        self.evict_at = Some(Instant::now() + EVICT_DELAY);
    }

    fn create_editor(&mut self, i: usize) -> Result<()> {
        let scale = self.window.scale_factor();
        let canvas = self.dock.canvas_window(i)?;
        let s = self.slots[i].as_mut().context("empty slot")?;
        let gui = s.gui().context("plugin has no GUI extension")?;
        let handle = s.instance.plugin_handle();
        let config = GuiConfiguration {
            api_type: GuiApiType::X11,
            is_floating: false,
        };
        if !gui.is_api_supported(&handle, config) {
            return Err(anyhow!("plugin cannot embed an X11 editor"));
        }
        gui.create(&handle, config)?;
        let _ = gui.set_scale(&handle, scale);
        s.editor.can_resize = gui.can_resize(&handle);
        s.editor.size = gui.get_size(&handle).unwrap_or(GuiSize {
            width: 800,
            height: 600,
        });
        // SAFETY: the canvas outlives the editor (destroyed in destroy_editor).
        unsafe { gui.set_parent(&handle, ClapWindow::from_x11_handle(canvas as _))? };
        s.editor.created = true;
        s.editor.scroll = (0, 0);
        // Size the canvas *after* set_parent: the ConfigureNotify is how a bridged editor
        // learns where it is on screen.
        let (w, h) = (s.editor.size.width, s.editor.size.height);
        self.dock.set_canvas_size(i, w, h)?;
        self.dock.announce(i)?;
        Ok(())
    }

    fn destroy_editor(&mut self, i: usize) {
        if let Some(s) = self.slots[i].as_mut()
            && s.editor.created
        {
            if let Some(gui) = s.gui() {
                gui.destroy(&s.instance.plugin_handle());
            }
            s.editor.created = false;
        }
        // The canvas stays (unmapped if it was showing) and is reused by the slot's next
        // editor: bridged editors tear down asynchronously, and destroying their parent
        // window under them makes Wine abort on BadWindow (found in S5).
        if self.dock.shown() == Some(i) {
            let _ = self.dock.show(None);
        }
    }

    /// Plan §4.6: keep recently used editors alive, destroy the least recently used beyond
    /// the cap.
    fn evict_editors(&mut self, keep: usize) {
        loop {
            let live: Vec<usize> = (0..SLOTS)
                .filter(|&j| self.slots[j].as_ref().is_some_and(|s| s.editor.created))
                .collect();
            if live.len() <= self.editor_cache {
                return;
            }
            let Some(&victim) = live
                .iter()
                .filter(|&&j| j != keep)
                .min_by_key(|&&j| self.slots[j].as_ref().unwrap().editor.last_shown)
            else {
                return;
            };
            self.say(format!("editor cache: destroying editor {}", victim + 1));
            self.destroy_editor(victim);
        }
    }

    fn fit_editor(&mut self) {
        let Some(i) = self.selected else { return };
        let (w, h) = self.dock.size();
        let Some(s) = self.slots[i].as_mut().filter(|s| s.editor.created) else {
            return;
        };
        let Some(gui) = s.gui() else { return };
        let handle = s.instance.plugin_handle();
        let want = GuiSize {
            width: w,
            height: h,
        };
        let size = gui.adjust_size(&handle, want).unwrap_or(want);
        if gui.set_size(&handle, size).is_ok() {
            s.editor.size = size;
            let _ = self.dock.set_canvas_size(i, size.width, size.height);
        }
        s.editor.scroll = (0, 0);
        let _ = self.dock.scroll_to(i, 0, 0);
    }

    // ---- main-thread service ---------------------------------------------------------

    fn service_plugins(&mut self) {
        let now = Instant::now();
        for s in self.slots.iter_mut().flatten() {
            s.service(now);
        }
    }

    fn next_deadline(&self) -> Option<Instant> {
        let timers = self
            .slots
            .iter()
            .flatten()
            .filter_map(|s| s.instance.access_handler(|h| h.next_timer_deadline()))
            .min();
        let script = self.script.first().map(|(at, _)| self.started + *at);
        [
            timers,
            self.repaint_at,
            self.evict_at,
            Some(self.next_status),
            script,
        ]
        .into_iter()
        .flatten()
        .min()
    }

    fn read_peaks(&mut self) {
        let Some(audio) = self.audio.as_ref() else {
            return;
        };
        for i in 0..SLOTS {
            let p = f32::from_bits(audio.shared.peaks[i].swap(0, Ordering::Relaxed));
            self.peaks_max[i] = self.peaks_max[i].max(p);
            self.peaks_shown[i] = p.max(self.peaks_shown[i] * 0.7);
        }
    }

    fn activity(&self) -> bool {
        let Some(audio) = self.audio.as_ref() else {
            return false;
        };
        self.peaks_shown.iter().any(|&p| p > 0.002)
            || self.held.is_some()
            || audio
                .shared
                .owners
                .iter()
                .any(|o| o.load(Ordering::Relaxed) != 0)
    }

    // ---- UI ---------------------------------------------------------------------------

    fn redraw(&mut self) {
        self.frames += 1;
        self.read_peaks();
        let raw = self.egui.take_egui_input(&self.window);
        let ctx = self.egui.egui_ctx().clone();
        let mut dock_rect = egui::Rect::NOTHING;
        let mut actions = Vec::new();
        let out = ctx.run_ui(raw, |ui| self.ui(ui, &mut dock_rect, &mut actions));
        self.egui
            .handle_platform_output(&self.window, out.platform_output);

        // Overlap rule (plan §4.6): native child windows draw above egui, so while an egui
        // popup is open the dock is unmapped and the popup is drawn over plain egui.
        self.popup_open = egui::Popup::is_any_open(&ctx);
        let ppp = out.pixels_per_point;
        let _ = self.dock.set_rect(
            (dock_rect.min.x * ppp).round() as i32,
            (dock_rect.min.y * ppp).round() as i32,
            (dock_rect.width() * ppp).round() as u32,
            (dock_rect.height() * ppp).round() as u32,
        );
        let _ = self
            .dock
            .set_visible(self.selected.is_some() && !self.popup_open);

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

    fn ui(&mut self, ui: &mut egui::Ui, dock_rect: &mut egui::Rect, actions: &mut Vec<Action>) {
        egui::Panel::top("status").show(ui, |ui| {
            let audio = self.audio.as_ref();
            let xruns = audio.map(|a| a.xruns.load(Ordering::Relaxed)).unwrap_or(0);
            let (bufsize, load) = audio
                .map(|a| (a.buffer_size(), a.cpu_load()))
                .unwrap_or_default();
            let loaded = self.slots.iter().flatten().count();
            let timers: usize = self
                .slots
                .iter()
                .flatten()
                .map(|s| s.instance.access_handler(|h| h.timers.borrow().len()))
                .sum();
            let fds: usize = self
                .slots
                .iter()
                .flatten()
                .map(|s| s.instance.access_handler(|h| h.fds.borrow().len()))
                .sum();
            let up = self.started.elapsed().as_secs_f64().max(0.001);
            ui.horizontal(|ui| {
                ui.strong("MERLIN S5");
                ui.label(format!(
                    "{loaded}/{SLOTS} slots · buffer {bufsize} · DSP {load:.1}% · xruns {xruns} · \
                     UI CPU {:.1}% · timers {timers} · fds {fds} · {:.1} frames/s · \
                     {:.1} wakeups/s{}",
                    self.cpu_self,
                    self.frames as f64 / up,
                    self.wakeups as f64 / up,
                    self.last_switch
                        .map(|(i, d)| format!(" · last editor switch (slot {}) {d:.1?}", i + 1))
                        .unwrap_or_default(),
                ));
            });
        });

        egui::Panel::top("rack").show(ui, |ui| {
            ui.add_space(2.0);
            for i in 0..SLOTS {
                self.unit(ui, i, actions);
            }
        });

        egui::Panel::bottom("keys").show(ui, |ui| {
            let zones: [ZoneView; SLOTS] = std::array::from_fn(|i| {
                self.slots[i]
                    .as_ref()
                    .map(|s| ZoneView {
                        loaded: true,
                        active: s.route.active && !s.route.mute,
                        lo: s.route.lo,
                        hi: s.route.hi,
                    })
                    .unwrap_or(ZoneView {
                        loaded: false,
                        active: false,
                        lo: 0,
                        hi: 0,
                    })
            });
            let pressed = match self.audio.as_ref() {
                Some(a) => keyboard::show(ui, &zones, &a.shared),
                None => None,
            };
            match (pressed, self.held) {
                (Some(k), None) => {
                    self.held = Some(k);
                    actions.push(Action::NoteOn(k));
                }
                (None, Some(k)) => {
                    self.held = None;
                    actions.push(Action::NoteOff(k));
                }
                _ => {}
            }
        });

        egui::Panel::top("dockbar").show(ui, |ui| {
            ui.horizontal(|ui| {
                match self
                    .selected
                    .and_then(|i| self.slots[i].as_ref().map(|s| (i, s)))
                {
                    Some((i, s)) => {
                        ui.colored_label(COLORS[i], format!("EDITOR · {} {}", i + 1, s.name));
                        let can_resize = s.editor.can_resize;
                        let (dw, dh) = self.dock.size();
                        let (ew, eh) = (s.editor.size.width, s.editor.size.height);
                        let (mut sx, mut sy) = s.editor.scroll;
                        if ui
                            .add_enabled(can_resize, egui::Button::new("Fit to dock"))
                            .clicked()
                        {
                            actions.push(Action::Fit);
                        }
                        ui.label(format!(
                            "{ew}x{eh} in {dw}x{dh} (X11 sees {:?})",
                            self.dock.plugin_window_size(i)
                        ));
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
                    }
                    None => {
                        ui.label("EDITOR · nothing selected");
                    }
                }
            });
        });

        egui::CentralPanel::default().show(ui, |ui| {
            *dock_rect = ui.available_rect_before_wrap();
            let text = if self.selected.is_none() {
                "Load a plugin into a slot"
            } else if self.popup_open {
                "(editor hidden while a menu is open)"
            } else {
                ""
            };
            ui.centered_and_justified(|ui| ui.label(text));
        });
    }

    fn unit(&mut self, ui: &mut egui::Ui, i: usize, actions: &mut Vec<Action>) {
        let selected = self.selected == Some(i);
        let color = COLORS[i];
        let (loaded, active, mute, mut lo, mut hi, label) = match self.slots[i].as_ref() {
            Some(s) => (
                true,
                s.route.active,
                s.route.mute,
                s.route.lo,
                s.route.hi,
                format!(
                    "{}  {}",
                    s.name,
                    if s.windows { "CLAP·WIN" } else { "CLAP" }
                ),
            ),
            None => (false, false, false, 0, 127, "— empty —".to_string()),
        };
        let frame = egui::Frame::new()
            .fill(if selected {
                color.gamma_multiply(0.30)
            } else {
                color.gamma_multiply(0.12)
            })
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
                let r = ui.add_sized(
                    [260.0, 20.0],
                    egui::Button::selectable(selected, label)
                        .frame(false)
                        .sense(egui::Sense::click()),
                );
                if loaded && r.clicked() {
                    actions.push(Action::Select(i, ui.input(|inp| inp.modifiers.shift)));
                }
                let led = if active && !mute {
                    color
                } else {
                    egui::Color32::from_gray(60)
                };
                let (led_rect, _) =
                    ui.allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::hover());
                ui.painter().circle_filled(led_rect.center(), 5.0, led);
                ui.add(
                    egui::ProgressBar::new(self.peaks_shown[i].clamp(0.0, 1.0).sqrt())
                        .desired_width(70.0),
                );
                if loaded {
                    if ui.selectable_label(mute, "M").clicked() {
                        actions.push(Action::Mute(i));
                    }
                    ui.label("zone");
                    let a = ui.add(egui::DragValue::new(&mut lo).range(0..=127));
                    ui.label("–");
                    let b = ui.add(egui::DragValue::new(&mut hi).range(0..=127));
                    if a.changed() || b.changed() {
                        actions.push(Action::Zone(i, lo, hi));
                    }
                }
                let combo = egui::ComboBox::from_id_salt(("load", i))
                    .selected_text(if loaded { "Replace…" } else { "Load…" })
                    .width(110.0);
                let menu = combo.show_ui(ui, |ui| {
                    for (k, f) in self.found.iter().enumerate() {
                        if ui.selectable_label(false, &f.label).clicked() {
                            actions.push(Action::Load(i, k));
                        }
                    }
                });
                if self.force_menu == Some(i) {
                    // Test hook (script `menu`): open this slot's menu without a click.
                    self.force_menu = None;
                    egui::Popup::open_id(ui.ctx(), menu.response.id.with("popup"));
                }
                if loaded && ui.small_button("x").clicked() {
                    actions.push(Action::Unload(i));
                }
            });
        });
    }

    // ---- actions ----------------------------------------------------------------------

    fn send_note(&mut self, bytes: [u8; 3]) {
        if let Some(a) = self.audio.as_mut() {
            let _ = a.ui_notes.push(bytes);
        }
    }

    fn apply(&mut self, action: Action) {
        let result: Result<()> = match action {
            Action::Load(i, k) => self.load(i, k),
            Action::LoadNamed(i, query) => match self.find(&query) {
                Some(k) => self.load(i, k),
                None => Err(anyhow!("no plugin matches '{query}'")),
            },
            Action::HideDock => self.dock.show(None),
            Action::Menu(i) => {
                self.force_menu = Some(i);
                Ok(())
            }
            Action::CloseMenu => {
                egui::Popup::close_all(self.egui.egui_ctx());
                Ok(())
            }
            Action::Destroy(i) => {
                self.destroy_editor(i);
                Ok(())
            }
            Action::Unload(i) => {
                self.unload(i);
                Ok(())
            }
            Action::Select(i, layer) => {
                self.select(i, layer);
                Ok(())
            }
            Action::Mute(i) => {
                if let Some(s) = self.slots[i].as_mut() {
                    s.route.mute = !s.route.mute;
                    self.push_route(i);
                }
                Ok(())
            }
            Action::Zone(i, lo, hi) => {
                if let Some(s) = self.slots[i].as_mut() {
                    s.route.lo = lo;
                    s.route.hi = hi;
                    self.push_route(i);
                }
                Ok(())
            }
            Action::Fit => {
                self.fit_editor();
                Ok(())
            }
            Action::Scroll(x, y) => {
                if let Some(i) = self.selected {
                    if let Some(s) = self.slots[i].as_mut() {
                        s.editor.scroll = (x, y);
                    }
                    self.dock.scroll_to(i, x, y)
                } else {
                    Ok(())
                }
            }
            Action::NoteOn(n) => {
                self.send_note([0x90, n, 100]);
                Ok(())
            }
            Action::NoteOff(n) => {
                self.send_note([0x80, n, 0]);
                Ok(())
            }
            Action::Peak => {
                self.read_peaks();
                let line = (0..SLOTS)
                    .filter(|&i| self.slots[i].is_some())
                    .map(|i| format!("{}:{:.3}", i + 1, self.peaks_max[i]))
                    .collect::<Vec<_>>()
                    .join(" ");
                self.say(format!("peaks since last check: {line}"));
                self.peaks_max = [0.0; SLOTS];
                Ok(())
            }
            Action::Cpu => {
                let (own, tree, members) = cpu_snapshot();
                let now = Instant::now();
                if let Some((t, o, tr, before)) = self.cpu_script.take() {
                    // SAFETY: sysconf is always safe to call.
                    let hz = unsafe { libc::sysconf(libc::_SC_CLK_TCK) } as f64;
                    let dt = now.duration_since(t).as_secs_f64();
                    self.say(format!(
                        "cpu over {dt:.1}s: this process {:.1}%, with children and wineserver {:.1}%",
                        (own - o) as f64 / hz / dt * 100.0,
                        (tree - tr) as f64 / hz / dt * 100.0
                    ));
                    for (pid, comm, ticks) in &members {
                        let old = before.iter().find(|b| b.0 == *pid).map_or(0, |b| b.2);
                        let pct = (ticks - old) as f64 / hz / dt * 100.0;
                        if pct >= 0.1 {
                            self.say(format!("  {comm} ({pid}): {pct:.1}%"));
                        }
                    }
                }
                self.cpu_script = Some((now, own, tree, members));
                Ok(())
            }
            Action::Report => {
                let list: Vec<String> = (0..SLOTS)
                    .filter_map(|i| {
                        self.slots[i].as_ref().map(|s| {
                            format!(
                                "{}:{}{}",
                                i + 1,
                                s.name,
                                if s.editor.created { "(editor)" } else { "" }
                            )
                        })
                    })
                    .collect();
                let (xruns, dsp) = self
                    .audio
                    .as_ref()
                    .map(|a| (a.xruns.load(Ordering::Relaxed), a.cpu_load()))
                    .unwrap_or_default();
                self.say(format!(
                    "state: xruns {xruns}, DSP {dsp:.1}%, selected {:?}, dock shows {:?}, popup open {}, slots {}",
                    self.selected.map(|i| i + 1),
                    self.dock.shown().map(|i| i + 1),
                    self.popup_open,
                    list.join(" ")
                ));
                Ok(())
            }
            Action::Quit => {
                self.quit = true;
                Ok(())
            }
        };
        if let Err(e) = result {
            self.say(format!("error: {e:#}"));
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

    /// Housekeeping after each wakeup: statistics and the status refresh rate.
    fn tick(&mut self, now: Instant) {
        if self.evict_at.is_some_and(|t| t <= now) {
            self.evict_at = None;
            if let Some(keep) = self.selected {
                self.evict_editors(keep);
            }
        }
        if now >= self.next_status {
            self.status_period = if self.activity() {
                Duration::from_millis(50)
            } else {
                Duration::from_millis(500)
            };
            self.next_status = now + self.status_period;
            if now.duration_since(self.cpu_mark.0) >= Duration::from_secs(2) {
                // SAFETY: sysconf is always safe to call.
                let hz = unsafe { libc::sysconf(libc::_SC_CLK_TCK) } as f32;
                let own = cpu_ticks().0;
                self.cpu_self = (own - self.cpu_mark.1) as f32
                    / hz
                    / now.duration_since(self.cpu_mark.0).as_secs_f32()
                    * 100.0;
                self.cpu_mark = (now, own);
            }
            self.ask_redraw();
        }
    }
}

#[derive(Debug)]
enum Action {
    /// Load found-plugin `k` into slot `i`.
    Load(usize, usize),
    Unload(usize),
    Select(usize, bool),
    Mute(usize),
    Zone(usize, u8, u8),
    Fit,
    Scroll(u32, u32),
    NoteOn(u8),
    NoteOff(u8),
    // Script-only.
    LoadNamed(usize, String),
    /// Destroy a slot's editor (closing it for good).
    Destroy(usize),
    /// Unmap the dock's canvas (nothing shown).
    HideDock,
    /// Open the load menu of a slot.
    Menu(usize),
    /// Close any open egui popup.
    CloseMenu,
    Peak,
    Cpu,
    Report,
    Quit,
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
        match event {
            UserEvent::MainThreadCallback(i) => {
                if let Some(s) = run.slots[i].as_mut() {
                    s.instance.call_on_main_thread_callback();
                }
            }
            UserEvent::FdReady(i) => {
                if let Some(s) = run.slots[i].as_mut() {
                    s.on_fd_ready();
                }
            }
            UserEvent::GuiResize(i, size) => {
                // Accept without calling set_size (JUCE logs HOST_MISBEHAVING for an
                // unadjusted size). The dock clips and scrolling covers anything larger.
                if let Some(s) = run.slots[i].as_mut().filter(|s| s.editor.created) {
                    s.editor.size = size;
                    let _ = run.dock.set_canvas_size(i, size.width, size.height);
                    run.ask_redraw();
                }
            }
            UserEvent::GuiClosed(i) => run.say(format!("slot {}: plugin closed its GUI", i + 1)),
        }
        run.service_plugins();
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
                for i in 0..SLOTS {
                    run.destroy_editor(i);
                }
                if let Some(audio) = run.audio.take() {
                    for (i, a) in audio.stop() {
                        if let Some(s) = run.slots[i].as_mut() {
                            s.instance.deactivate(a.processor.stop_processing());
                        }
                    }
                }
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
                std::mem::take(&mut run.requested);
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
        run.service_plugins();
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

/// `S5_SCRIPT="2:load 1 Six Sines; 3:select 1; 4:on 60; 5:peak; 6:quit"` (slots are 1-based;
/// times are seconds since start).
fn parse_script() -> Vec<(Duration, Action)> {
    let Ok(script) = std::env::var("S5_SCRIPT") else {
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
                "hidedock" => Action::HideDock,
                "menu" => Action::Menu(slot(words.next())?),
                "closemenu" => Action::CloseMenu,
                "destroy" => Action::Destroy(slot(words.next())?),
                "select" => Action::Select(slot(words.next())?, false),
                "layer" => Action::Select(slot(words.next())?, true),
                "mute" => Action::Mute(slot(words.next())?),
                "zone" => Action::Zone(
                    slot(words.next())?,
                    words.next()?.parse().ok()?,
                    words.next()?.parse().ok()?,
                ),
                "fit" => Action::Fit,
                "scroll" => {
                    Action::Scroll(words.next()?.parse().ok()?, words.next()?.parse().ok()?)
                }
                "on" => Action::NoteOn(words.next()?.parse().ok()?),
                "off" => Action::NoteOff(words.next()?.parse().ok()?),
                "peak" => Action::Peak,
                "cpu" => Action::Cpu,
                "report" => Action::Report,
                "quit" => Action::Quit,
                _ => return None,
            };
            Some((at, action))
        })
        .collect()
}

fn main() -> Result<()> {
    let mut dirs: Vec<PathBuf> = std::env::args().skip(1).map(PathBuf::from).collect();
    if dirs.is_empty() {
        dirs = vec![
            "test-assets/dl/surge-linux".into(),
            "test-assets/bridges-vblank/clap".into(),
        ];
    }
    let event_loop = EventLoop::<UserEvent>::with_user_event()
        .with_x11()
        .build()?;
    let mut app = App {
        proxy: event_loop.create_proxy(),
        dirs,
        run: None,
    };
    event_loop.run_app(&mut app)?;
    // Give plugin background threads (JUCE) a moment to wind down.
    std::thread::sleep(Duration::from_millis(100));
    Ok(())
}
