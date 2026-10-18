//! S2: CLAP hosting with the plugin editor embedded in the main window.
//!
//! An egui/wgpu window on our own winit loop, forced to X11 (XWayland on Wayland desktops).
//! A Surge XT CLAP instance plays through JACK; its editor is embedded in an X11 child
//! window ("dock") inside the main window, with scroll/clip, resize, focus experiments and
//! state save/restore.
//!
//! Usage: `cargo run -p s2-clap-embed --release -- [path/to/plugin.clap]`

mod audio;
mod dock;
mod host;

use std::ffi::CString;
use std::num::NonZeroU32;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use clack_extensions::gui::{GuiApiType, GuiConfiguration, GuiSize, Window as ClapWindow};
use clack_host::prelude::*;
use egui::ViewportId;
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::platform::x11::EventLoopBuilderExtX11;
use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
use winit::window::{Window, WindowId};

use audio::{Audio, UiNote};
use dock::Dock;
use host::{FdPoller, MainThread, S2Host, Shared, UserEvent};

const DEFAULT_PLUGIN: &str = "test-assets/dl/surge-linux/Surge XT.clap";

struct Editor {
    open: bool,
    size: GuiSize,
    can_resize: bool,
    fit: bool,
    scroll: (u32, u32),
    resize_requests: u32,
}

struct Running {
    window: Arc<Window>,
    egui: egui_winit::State,
    painter: egui_wgpu::winit::Painter,
    dock: Dock,
    // Field order matters on drop: audio and editor go before the instance.
    audio: Option<Audio>,
    editor: Editor,
    instance: PluginInstance<S2Host>,
    _entry: PluginEntry,
    plugin_name: String,
    fds: FdPoller,
    notes: rtrb::Producer<UiNote>,
    held: Option<[u8; 3]>,
    saved_state: Option<Vec<u8>>,
    log: Vec<String>,
    frames: u64,
    wakeups: u64,
    started: Instant,
    next_status: Instant,
    repaint_at: Option<Instant>,
    script: Vec<(Duration, UiAction)>,
    quit: bool,
    requested: bool,
    spontaneous: u64,
}

struct App {
    proxy: EventLoopProxy<UserEvent>,
    plugin_path: String,
    run: Option<Running>,
}

fn x11_window_id(window: &Window) -> Result<u32> {
    match window.window_handle()?.as_raw() {
        RawWindowHandle::Xlib(h) => Ok(h.window as u32),
        RawWindowHandle::Xcb(h) => Ok(h.window.get()),
        other => Err(anyhow!("not an X11 window: {other:?}")),
    }
}

impl App {
    fn start(&mut self, event_loop: &ActiveEventLoop) -> Result<Running> {
        let window = Arc::new(
            event_loop.create_window(
                Window::default_attributes()
                    .with_title("Merlin S2: CLAP + embedded editor")
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

        // Load the plugin.
        let t = Instant::now();
        // SAFETY: loading a plugin binary runs its code; that is the point of a host.
        let entry = unsafe { PluginEntry::load(&self.plugin_path) }
            .map_err(|e| anyhow!("loading {}: {e:?}", self.plugin_path))?;
        let factory = entry.get_plugin_factory().context("no plugin factory")?;
        let desc = factory
            .plugin_descriptors()
            .next()
            .context("no plugins in bundle")?;
        let id = CString::from(desc.id().context("no id")?);
        let plugin_name = desc
            .name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let host_info = HostInfo::new("Merlin S2", "Merlin", "", "0.0.0")?;
        let proxy = self.proxy.clone();
        let mut instance = PluginInstance::<S2Host>::new(
            |_| Shared::new(proxy),
            |shared| MainThread::new(shared),
            &entry,
            &id,
            &host_info,
        )?;
        let mut log = vec![format!("loaded {plugin_name} in {:?}", t.elapsed())];
        println!("{}", log[0]);

        let (notes, notes_rx) = rtrb::RingBuffer::new(256);
        let audio = audio::start(&mut instance, notes_rx)?;
        log.push(format!("JACK at {} Hz", audio.sample_rate));

        let fds = FdPoller::spawn(self.proxy.clone());
        let mut run = Running {
            window,
            egui,
            painter,
            dock,
            audio: Some(audio),
            editor: Editor {
                open: false,
                size: GuiSize {
                    width: 0,
                    height: 0,
                },
                can_resize: false,
                fit: false,
                scroll: (0, 0),
                resize_requests: 0,
            },
            instance,
            _entry: entry,
            plugin_name,
            fds,
            notes,
            held: None,
            saved_state: None,
            log,
            frames: 0,
            wakeups: 0,
            started: Instant::now(),
            next_status: Instant::now(),
            repaint_at: None,
            script: parse_script(),
            quit: false,
            requested: false,
            spontaneous: 0,
        };
        // Lay out once so the dock has its real rect before the editor is created.
        run.redraw();
        run.open_editor()?;
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

    fn gui(&mut self) -> Option<clack_extensions::gui::PluginGui> {
        self.instance.access_handler(|h| h.gui.get())
    }

    fn open_editor(&mut self) -> Result<()> {
        let gui = self.gui().context("plugin has no GUI extension")?;
        let handle = self.instance.plugin_handle();
        let config = GuiConfiguration {
            api_type: GuiApiType::X11,
            is_floating: false,
        };
        if !gui.is_api_supported(&handle, config) {
            return Err(anyhow!("plugin cannot embed an X11 editor"));
        }
        let t = Instant::now();
        gui.create(&handle, config)?;
        let _ = gui.set_scale(&handle, self.window.scale_factor());
        self.editor.can_resize = gui.can_resize(&handle);
        self.editor.size = gui.get_size(&handle).unwrap_or(GuiSize {
            width: 800,
            height: 600,
        });
        // SAFETY: the dock window outlives the editor (destroyed in close_editor/drop).
        unsafe { gui.set_parent(&handle, ClapWindow::from_x11_handle(self.dock.window as _))? };
        let _ = gui.show(&handle);
        // Size the canvas *after* set_parent: the resulting ConfigureNotify is how a bridged
        // (yabridge/Wine) editor learns where it is on screen. Without it Wine maps every
        // click to the wrong place.
        let (w, h) = (self.editor.size.width, self.editor.size.height);
        self.editor.scroll = (0, 0);
        self.dock.scroll_to(0, 0)?;
        self.dock.set_canvas_size(w, h)?;
        self.dock.announce()?;
        if let Some(w) = self.dock.plugin_window() {
            println!("plugin_window: {w}");
        }
        self.editor.open = true;
        self.say(format!(
            "editor embedded in {:?}: {}x{}, resizable {}",
            t.elapsed(),
            self.editor.size.width,
            self.editor.size.height,
            self.editor.can_resize
        ));
        self.dock.set_visible(true)?;
        Ok(())
    }

    fn close_editor(&mut self) {
        if let Some(gui) = self.gui().filter(|_| self.editor.open) {
            gui.destroy(&self.instance.plugin_handle());
            self.editor.open = false;
            let _ = self.dock.set_visible(false);
        }
    }

    fn fit_editor(&mut self) {
        let Some(gui) = self.gui() else { return };
        let (w, h) = self.dock.size();
        let handle = self.instance.plugin_handle();
        let want = GuiSize {
            width: w,
            height: h,
        };
        let size = gui.adjust_size(&handle, want).unwrap_or(want);
        if gui.set_size(&handle, size).is_ok() {
            self.editor.size = size;
            let _ = self.dock.set_canvas_size(size.width, size.height);
        }
        // A fitted editor needs no scrolling; drop any old offset or its top stays hidden.
        self.editor.scroll = (0, 0);
        let _ = self.dock.scroll_to(0, 0);
    }

    fn save_state(&mut self) -> Result<Vec<u8>> {
        let ext = self
            .instance
            .access_handler(|h| h.state_ext.get())
            .context("no state ext")?;
        let mut buf = Vec::new();
        ext.save(&self.instance.plugin_handle(), &mut buf)?;
        Ok(buf)
    }

    fn load_state(&mut self, data: &[u8]) -> Result<()> {
        let ext = self
            .instance
            .access_handler(|h| h.state_ext.get())
            .context("no state ext")?;
        ext.load(
            &self.instance.plugin_handle(),
            &mut std::io::Cursor::new(data),
        )?;
        Ok(())
    }

    fn set_param(&mut self, n: u32, norm: f64) -> Result<()> {
        use clack_extensions::params::{ParamInfoBuffer, ParamInfoFlags, PluginParams};
        let handle = self.instance.plugin_handle();
        let ext = handle
            .get_extension::<PluginParams>()
            .context("no params extension")?;
        let mut buf = ParamInfoBuffer::new();
        let info = (0..ext.count(&handle))
            .filter_map(|i| {
                ext.get_info(&handle, i, &mut buf).map(|p| {
                    (
                        p.id,
                        p.flags,
                        p.min_value,
                        p.max_value,
                        String::from_utf8_lossy(p.name).into_owned(),
                    )
                })
            })
            .filter(|(_, f, ..)| {
                f.contains(ParamInfoFlags::IS_AUTOMATABLE)
                    && !f.intersects(
                        ParamInfoFlags::IS_HIDDEN
                            | ParamInfoFlags::IS_READONLY
                            | ParamInfoFlags::IS_BYPASS,
                    )
            })
            .nth(n as usize)
            .context("no such parameter")?;
        let (id, _, min, max, name) = info;
        let value = min + (max - min) * norm;
        let _ = self.notes.push(UiNote::Param(id.get(), value));
        self.say(format!(
            "param #{n} '{name}' (id {}) -> {value:.3}",
            id.get()
        ));
        Ok(())
    }

    fn send_note(&mut self, bytes: [u8; 3]) {
        let _ = self.notes.push(UiNote::Midi(bytes));
    }

    /// Main-thread housekeeping after any wakeup: timers and fd-set changes.
    fn service_plugin(&mut self) {
        let now = Instant::now();
        let (timer_ext, due) = self
            .instance
            .access_handler(|h| (h.timer_ext.get(), h.due_timers(now)));
        if let Some(ext) = timer_ext {
            for id in due {
                ext.on_timer(&self.instance.plugin_handle(), id);
            }
        }
        if let Some(fds) = self
            .instance
            .access_handler(|h| h.fds_changed.replace(false).then(|| h.fds.borrow().clone()))
        {
            self.fds.set(&fds);
        }
    }

    fn on_fd_ready(&mut self) {
        let (ext, fds) = self
            .instance
            .access_handler(|h| (h.fd_ext.get(), h.fds.borrow().clone()));
        if let Some(ext) = ext {
            for (fd, flags) in self.fds.ready(&fds) {
                ext.on_fd(&self.instance.plugin_handle(), fd, flags);
            }
        }
        self.fds.ack();
        self.service_plugin();
    }

    fn next_deadline(&self) -> Option<Instant> {
        let timer = self.instance.access_handler(|h| h.next_timer_deadline());
        let script = self.script.first().map(|(at, _)| self.started + *at);
        [timer, self.repaint_at, Some(self.next_status), script]
            .into_iter()
            .flatten()
            .min()
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

    fn redraw(&mut self) {
        self.frames += 1;
        let raw = self.egui.take_egui_input(&self.window);
        let ctx = self.egui.egui_ctx().clone();
        let mut dock_rect = egui::Rect::NOTHING;
        let mut actions = Vec::new();
        let out = ctx.run_ui(raw, |ui| self.ui(ui, &mut dock_rect, &mut actions));
        self.egui
            .handle_platform_output(&self.window, out.platform_output);

        let ppp = out.pixels_per_point;
        let _ = self.dock.set_rect(
            (dock_rect.min.x * ppp).round() as i32,
            (dock_rect.min.y * ppp).round() as i32,
            (dock_rect.width() * ppp).round() as u32,
            (dock_rect.height() * ppp).round() as u32,
        );

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

        if std::env::var_os("S2_DEBUG_REPAINT").is_some() && self.frames.is_multiple_of(100) {
            println!(
                "frame {} repaint causes: {:?} delay {:?}",
                self.frames,
                ctx.repaint_causes(),
                out.viewport_output
                    .get(&ViewportId::ROOT)
                    .map(|v| v.repaint_delay)
            );
        }
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

    fn apply(&mut self, action: UiAction) {
        let result: Result<()> = match action {
            UiAction::ToggleEditor => {
                if self.editor.open {
                    self.close_editor();
                    Ok(())
                } else {
                    self.open_editor()
                }
            }
            UiAction::Fit => {
                self.fit_editor();
                Ok(())
            }
            UiAction::SaveState => self.save_state().map(|s| {
                self.say(format!("state saved: {} bytes", s.len()));
                self.saved_state = Some(s);
            }),
            UiAction::LoadState => match self.saved_state.clone() {
                Some(s) => self.load_state(&s).map(|_| self.say("state restored")),
                None => Err(anyhow!("nothing saved yet")),
            },
            UiAction::FocusPlugin => self.dock.focus_plugin(),
            UiAction::Scroll(x, y) => {
                self.editor.scroll = (x, y);
                self.dock.scroll_to(x, y)
            }
            UiAction::NoteOn(n) => {
                self.send_note([0x90, n, 100]);
                Ok(())
            }
            UiAction::NoteOff(n) => {
                self.send_note([0x80, n, 0]);
                Ok(())
            }
            UiAction::Param(n, norm) => self.set_param(n, norm),
            UiAction::Quit => {
                self.quit = true;
                Ok(())
            }
        };
        if let Err(e) = result {
            self.say(format!("error: {e:#}"));
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, dock_rect: &mut egui::Rect, actions: &mut Vec<UiAction>) {
        egui::Panel::top("rack").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.heading(&self.plugin_name);
                ui.separator();
                let label = if self.editor.open { "Close editor" } else { "Open editor" };
                if ui.button(label).clicked() {
                    actions.push(UiAction::ToggleEditor);
                }
                if ui
                    .add_enabled(self.editor.can_resize, egui::Button::new("Fit editor to dock"))
                    .clicked()
                {
                    actions.push(UiAction::Fit);
                }
                if ui.button("Save state").clicked() {
                    actions.push(UiAction::SaveState);
                }
                if ui.add_enabled(self.saved_state.is_some(), egui::Button::new("Restore state")).clicked() {
                    actions.push(UiAction::LoadState);
                }
                if ui.button("Focus plugin").clicked() {
                    actions.push(UiAction::FocusPlugin);
                }
                ui.separator();
                ui.label("Play:");
                for (label, note) in [("C", 60u8), ("E", 64), ("G", 67), ("C'", 72)] {
                    let r = ui.add(egui::Button::new(label).sense(egui::Sense::click_and_drag()));
                    let down = r.is_pointer_button_down_on();
                    match (down, self.held) {
                        (true, None) => {
                            self.held = Some([note, 0, 0]);
                            actions.push(UiAction::NoteOn(note));
                        }
                        (false, Some([n, ..])) if n == note => {
                            self.held = None;
                            actions.push(UiAction::NoteOff(note));
                        }
                        _ => {}
                    }
                }
            });
            let audio = self.audio.as_ref();
            let xruns = audio.map(|a| a.xruns.load(Ordering::Relaxed)).unwrap_or(0);
            let (bufsize, load) = audio
                .map(|a| (a.client.as_client().buffer_size(), a.client.as_client().cpu_load()))
                .unwrap_or_default();
            let (timers, fds) = self
                .instance
                .access_handler(|h| (h.timers.borrow().len(), h.fds.borrow().len()));
            let up = self.started.elapsed().as_secs_f64().max(0.001);
            ui.label(format!(
                "buffer {bufsize} · DSP {load:.1}% · xruns {xruns} · editor {}x{} (X11 sees {:?}) · \
                 dock {:?} · timers {timers} · fds {fds} · {:.1} frames/s · {:.1} wakeups/s · \
                 focus {:?} · resize requests {}",
                self.editor.size.width,
                self.editor.size.height,
                self.dock.plugin_window_size(),
                self.dock.size(),
                self.frames as f64 / up,
                self.wakeups as f64 / up,
                self.dock.focus_owner(),
                self.editor.resize_requests,
            ));
        });

        egui::Panel::bottom("log").exact_size(110.0).show(ui, |ui| {
            let (dw, dh) = self.dock.size();
            let (ew, eh) = (self.editor.size.width, self.editor.size.height);
            let (mut sx, mut sy) = self.editor.scroll;
            ui.horizontal(|ui| {
                ui.label("Scroll x");
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
                    actions.push(UiAction::Scroll(sx, sy));
                }
                ui.checkbox(&mut self.editor.fit, "Auto-fit on resize");
            });
            egui::ScrollArea::vertical()
                .stick_to_bottom(true)
                .show(ui, |ui| {
                    for line in &self.log {
                        ui.monospace(line);
                    }
                });
        });

        egui::CentralPanel::default().show(ui, |ui| {
            *dock_rect = ui.available_rect_before_wrap();
            if !self.editor.open {
                ui.centered_and_justified(|ui| ui.label("Editor closed"));
            }
        });
    }
}

#[derive(Debug)]
enum UiAction {
    ToggleEditor,
    Fit,
    SaveState,
    LoadState,
    FocusPlugin,
    Scroll(u32, u32),
    NoteOn(u8),
    NoteOff(u8),
    /// Set the n-th visible automatable parameter to a normalised value (0..1).
    Param(u32, f64),
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
            UserEvent::MainThreadCallback => run.instance.call_on_main_thread_callback(),
            UserEvent::FdReady => run.on_fd_ready(),
            UserEvent::GuiResize(size) => {
                // Accept without calling set_size (CLAP: the host doesn't have to; calling it
                // with an unadjusted size makes JUCE log HOST_MISBEHAVING). The dock clips,
                // and scrolling covers anything larger.
                run.editor.size = size;
                let _ = run.dock.set_canvas_size(size.width, size.height);
                run.editor.resize_requests += 1;
                run.ask_redraw();
            }
            UserEvent::GuiClosed => run.say("plugin closed its GUI"),
        }
        run.service_plugin();
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _: WindowId, event: WindowEvent) {
        let Some(run) = self.run.as_mut() else { return };
        let response = run.egui.on_window_event(&run.window, &event);
        if std::env::var_os("S2_DEBUG_REPAINT").is_some()
            && !matches!(event, WindowEvent::RedrawRequested)
        {
            println!("event {event:?} repaint={}", response.repaint);
        }
        let event = if run.quit {
            WindowEvent::CloseRequested
        } else {
            event
        };
        // egui_winit reports repaint=true for RedrawRequested itself; honouring that would
        // redraw forever (~140 fps here). Only input events may ask for a repaint.
        let wants_repaint = response.repaint && !matches!(event, WindowEvent::RedrawRequested);
        match event {
            WindowEvent::CloseRequested => {
                run.close_editor();
                if let Some(audio) = run.audio.take() {
                    audio.stop(&mut run.instance);
                }
                self.run = None;
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
                if !std::mem::take(&mut run.requested) {
                    run.spontaneous += 1;
                    if std::env::var_os("S2_DEBUG_REPAINT").is_some() && run.spontaneous % 100 == 0
                    {
                        println!(
                            "{} spontaneous redraws of {} frames",
                            run.spontaneous, run.frames
                        );
                    }
                }
                let before = run.dock.size();
                run.redraw();
                if run.editor.fit && run.editor.can_resize && run.dock.size() != before {
                    run.fit_editor();
                }
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
        run.service_plugin();
        run.run_script();
        if run.quit {
            run.ask_redraw();
            event_loop.set_control_flow(ControlFlow::Poll);
            return;
        }
        if now >= run.next_status {
            // Refresh the status line twice a second.
            run.next_status = now + Duration::from_millis(500);
            run.ask_redraw();
        }
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

/// `S2_SCRIPT="3:save; 5:scroll 0 200; 7:load; 9:fit; 11:toggle; 13:toggle; 15:quit"`:
/// timed actions for repeatable tests (seconds since start).
fn parse_script() -> Vec<(Duration, UiAction)> {
    let Ok(script) = std::env::var("S2_SCRIPT") else {
        return Vec::new();
    };
    script
        .split(';')
        .filter_map(|item| {
            let (at, cmd) = item.trim().split_once(':')?;
            let at = Duration::from_secs_f64(at.trim().parse().ok()?);
            let mut words = cmd.split_whitespace();
            let action = match words.next()? {
                "save" => UiAction::SaveState,
                "load" => UiAction::LoadState,
                "fit" => UiAction::Fit,
                "toggle" => UiAction::ToggleEditor,
                "focus" => UiAction::FocusPlugin,
                "quit" => UiAction::Quit,
                "scroll" => {
                    UiAction::Scroll(words.next()?.parse().ok()?, words.next()?.parse().ok()?)
                }
                "on" => UiAction::NoteOn(words.next()?.parse().ok()?),
                "off" => UiAction::NoteOff(words.next()?.parse().ok()?),
                "param" => {
                    UiAction::Param(words.next()?.parse().ok()?, words.next()?.parse().ok()?)
                }
                _ => return None,
            };
            Some((at, action))
        })
        .collect()
}

fn main() -> Result<()> {
    let plugin_path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| DEFAULT_PLUGIN.into());
    let event_loop = EventLoop::<UserEvent>::with_user_event()
        .with_x11()
        .build()?;
    let mut app = App {
        proxy: event_loop.create_proxy(),
        plugin_path,
        run: None,
    };
    event_loop.run_app(&mut app)?;
    // Give plugin background threads (JUCE) a moment to wind down.
    std::thread::sleep(Duration::from_millis(100));
    Ok(())
}
