//! S3: VST3 hosting with the plugin editor embedded in the main window.
//!
//! The S2 shell (egui/wgpu on our own winit loop, forced to X11, editor in an X11 child
//! "dock") with the CLAP host swapped for a minimal VST3 host written on the `vst3` crate:
//! module loading, component/controller setup, `IPlugView` embedding with `IPlugFrame` and
//! `IRunLoop`, JACK processing, and state save/restore.
//!
//! Usage: `cargo run -p s3-vst3-embed --release -- [path/to/plugin.vst3]`

mod audio;
mod dock;
mod host;

use std::ffi::c_void;
use std::num::NonZeroU32;
use std::os::fd::RawFd;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use egui::ViewportId;
use vst3::ComPtr;
use vst3::Steinberg::Linux::{IEventHandlerTrait, ITimerHandlerTrait};
use vst3::Steinberg::Vst::IEditControllerTrait;
use vst3::Steinberg::*;
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::platform::x11::EventLoopBuilderExtX11;
use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
use winit::window::{Window, WindowId};

use audio::Audio;
use dock::Dock;
use host::{FdPoller, Module, ParamDesc, Plugin, UserEvent};

const DEFAULT_PLUGIN: &str = "test-assets/dl/surge-linux/Surge XT.vst3";

struct Editor {
    open: bool,
    size: (u32, u32),
    can_resize: bool,
    fit: bool,
    scroll: (u32, u32),
    resize_requests: u32,
}

type SavedState = (Vec<u8>, Option<Vec<u8>>);

struct Running {
    window: Arc<Window>,
    egui: egui_winit::State,
    painter: egui_wgpu::winit::Painter,
    dock: Dock,
    // Field order matters on drop: audio, view and plugin go before the module.
    audio: Option<Audio>,
    view: Option<ComPtr<IPlugView>>,
    editor: Editor,
    plugin: Plugin,
    _module: Module,
    params: Vec<ParamDesc>,
    reports: rtrb::Consumer<(u32, f64)>,
    fds: FdPoller,
    watched: Vec<RawFd>,
    held: Option<u8>,
    saved_state: Option<SavedState>,
    log: Vec<String>,
    frames: u64,
    wakeups: u64,
    started: Instant,
    next_status: Instant,
    repaint_at: Option<Instant>,
    script: Vec<(Duration, UiAction)>,
    quit: bool,
    requested: bool,
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
                    .with_title("Merlin S3: VST3 + embedded editor")
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

        // Load the module and pick the first audio class.
        let t = Instant::now();
        let module = Module::load(std::path::Path::new(&self.plugin_path))?;
        let classes = module.classes();
        for c in &classes {
            println!(
                "class '{}' [{}] sub '{}' vendor '{}'",
                c.name, c.category, c.subcategories, c.vendor
            );
        }
        let class = classes
            .iter()
            .find(|c| c.category == "Audio Module Class")
            .context("no 'Audio Module Class' in bundle")?;
        let (edits, edits_rx) = rtrb::RingBuffer::new(1024);
        let mut plugin = Plugin::new(&module, class, self.proxy.clone(), edits)?;
        let mut log = vec![format!("loaded {} in {:?}", plugin.name, t.elapsed())];
        println!("{}", log[0]);

        let (reports_tx, reports) = rtrb::RingBuffer::new(4096);
        let audio = audio::start(&mut plugin, edits_rx, reports_tx)?;
        log.push(format!(
            "JACK at {} Hz, plugin latency {} samples",
            audio.sample_rate,
            plugin.latency()
        ));
        let params = plugin.params();
        log.push(format!("{} automatable parameters", params.len()));
        let shown = if std::env::var_os("S3_PARAMS").is_some() {
            usize::MAX
        } else {
            12
        };
        for (i, p) in params.iter().take(shown).enumerate() {
            println!("  param #{i}: '{}' (id {})", p.title, p.id);
        }

        let fds = FdPoller::spawn(self.proxy.clone());
        let mut run = Running {
            window,
            egui,
            painter,
            dock,
            audio: Some(audio),
            view: None,
            editor: Editor {
                open: false,
                size: (0, 0),
                can_resize: false,
                fit: false,
                scroll: (0, 0),
                resize_requests: 0,
            },
            plugin,
            _module: module,
            params,
            reports,
            fds,
            watched: Vec::new(),
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
        };
        // Lay out once so the dock has its real rect before the editor is created.
        run.redraw();
        run.open_editor()?;
        run.service_plugin();
        run.ask_redraw();
        Ok(run)
    }
}

fn rect_size(r: &ViewRect) -> (u32, u32) {
    (
        (r.right - r.left).max(1) as u32,
        (r.bottom - r.top).max(1) as u32,
    )
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

    fn open_editor(&mut self) -> Result<()> {
        let t = Instant::now();
        // SAFETY: COM calls on the plugin's view and our frame, main thread, in the order
        // the VST3 docs prescribe (createView, setFrame, attached).
        let view = unsafe {
            let raw = self.plugin.controller.createView(c"editor".as_ptr());
            ComPtr::from_raw(raw).context("plugin has no editor view")?
        };
        unsafe {
            if view.isPlatformTypeSupported(kPlatformTypeX11EmbedWindowID) != kResultTrue {
                return Err(anyhow!("plugin cannot embed an X11 editor"));
            }
            if let Some(scale) = view.cast::<IPlugViewContentScaleSupport>() {
                scale.setContentScaleFactor(self.window.scale_factor() as f32);
            }
            // The frame carries IRunLoop: JUCE looks for it in setFrame/attached.
            let frame = self.plugin.frame.to_com_ptr::<IPlugFrame>().unwrap();
            view.setFrame(frame.as_ptr());
            let mut rect: ViewRect = std::mem::zeroed();
            view.getSize(&mut rect);
            self.editor.size = rect_size(&rect);
            self.editor.can_resize = view.canResize() == kResultTrue;
            let r = view.attached(
                self.dock.window as usize as *mut c_void,
                kPlatformTypeX11EmbedWindowID,
            );
            if r != kResultOk {
                return Err(anyhow!("IPlugView::attached failed: {r}"));
            }
            if view.getSize(&mut rect) == kResultOk {
                self.editor.size = rect_size(&rect);
            }
        }
        self.view = Some(view);
        // Size the canvas *after* attached: the resulting ConfigureNotify is how a bridged
        // editor learns where it is on screen (S4).
        let (w, h) = self.editor.size;
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
            self.editor.size.0,
            self.editor.size.1,
            self.editor.can_resize
        ));
        self.dock.set_visible(true)?;
        Ok(())
    }

    fn close_editor(&mut self) {
        if let Some(view) = self.view.take() {
            // SAFETY: detach in the reverse order of open_editor.
            unsafe {
                view.removed();
                view.setFrame(std::ptr::null_mut());
            }
            self.editor.open = false;
            let _ = self.dock.set_visible(false);
        }
    }

    /// Tell the plugin its view is now `w`×`h` (after the host decided, or after a request).
    fn resize_view(&mut self, w: u32, h: u32) {
        if let Some(view) = &self.view {
            let mut rect = ViewRect {
                left: 0,
                top: 0,
                right: w as i32,
                bottom: h as i32,
            };
            // SAFETY: valid rect for the call.
            unsafe { view.onSize(&mut rect) };
        }
    }

    fn fit_editor(&mut self) {
        let Some(view) = &self.view else { return };
        let (w, h) = self.dock.size();
        let mut rect = ViewRect {
            left: 0,
            top: 0,
            right: w as i32,
            bottom: h as i32,
        };
        // SAFETY: valid rect for both calls.
        let size = unsafe {
            view.checkSizeConstraint(&mut rect);
            view.onSize(&mut rect);
            rect_size(&rect)
        };
        self.editor.size = size;
        let _ = self.dock.set_canvas_size(size.0, size.1);
        // A fitted editor needs no scrolling; drop any old offset or its top stays hidden.
        self.editor.scroll = (0, 0);
        let _ = self.dock.scroll_to(0, 0);
    }

    fn save_state(&mut self) -> Result<SavedState> {
        self.plugin.save_state()
    }

    fn set_param(&mut self, n: u32, norm: f64) -> Result<()> {
        let p = self.params.get(n as usize).context("no such parameter")?;
        let (id, title) = (p.id, p.title.clone());
        // SAFETY: simple controller call on the main thread.
        unsafe { self.plugin.controller.setParamNormalized(id, norm) };
        // The controller does not reach the processor by itself: that is the host's job.
        self.plugin.host.send_param(id, norm);
        self.say(format!("param #{n} '{title}' (id {id}) -> {norm:.3}"));
        Ok(())
    }

    fn probe_param(&mut self, n: u32) -> Result<()> {
        let p = self.params.get(n as usize).context("no such parameter")?;
        let (id, title) = (p.id, p.title.clone());
        // SAFETY: simple controller query.
        let v = unsafe { self.plugin.controller.getParamNormalized(id) };
        self.say(format!("probe #{n} '{title}' (id {id}) = {v:.3}"));
        Ok(())
    }

    /// Main-thread housekeeping after any wakeup: run-loop timers and fd-set changes.
    fn service_plugin(&mut self) {
        // Parameter changes the processor reported go to the controller (and so the editor).
        // A real host wakes the main thread for this; the spike polls on every wakeup.
        while let Ok((id, value)) = self.reports.pop() {
            // SAFETY: simple controller call on the main thread.
            unsafe { self.plugin.controller.setParamNormalized(id, value) };
        }
        let frame = &self.plugin.frame;
        for handler in frame.due_timers(Instant::now()) {
            // SAFETY: the plugin's own timer handler, called on the main thread.
            unsafe { handler.onTimer() };
        }
        if let Some(fds) = frame.take_fds_changed() {
            self.fds.set(&fds);
            self.watched = fds;
        }
    }

    fn on_fd_ready(&mut self) {
        let ready = self.fds.ready(&self.watched);
        for (handler, fd) in self.plugin.frame.handlers_for(&ready) {
            // SAFETY: the plugin's own event handler, called on the main thread.
            unsafe { handler.onFDIsSet(fd) };
        }
        self.fds.ack();
        self.service_plugin();
    }

    fn next_deadline(&self) -> Option<Instant> {
        let timer = self.plugin.frame.next_timer_deadline();
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
                self.say(format!(
                    "state saved: component {} bytes, controller {} bytes",
                    s.0.len(),
                    s.1.as_ref().map_or(0, Vec::len)
                ));
                if let Some(path) = std::env::var_os("S3_DUMP") {
                    let n = self.log.len();
                    let _ = std::fs::write(format!("{}.{n}", path.to_string_lossy()), &s.0);
                }
                self.saved_state = Some(s);
            }),
            UiAction::LoadState => match self.saved_state.clone() {
                Some((comp, ctrl)) => self
                    .plugin
                    .load_state(&comp, ctrl.as_deref())
                    .map(|_| self.say("state restored")),
                None => Err(anyhow!("nothing saved yet")),
            },
            UiAction::Scroll(x, y) => {
                self.editor.scroll = (x, y);
                self.dock.scroll_to(x, y)
            }
            UiAction::NoteOn(n) => {
                self.plugin.host.send_midi([0x90, n, 100]);
                Ok(())
            }
            UiAction::NoteOff(n) => {
                self.plugin.host.send_midi([0x80, n, 0]);
                Ok(())
            }
            UiAction::Param(n, norm) => self.set_param(n, norm),
            UiAction::Probe(n) => self.probe_param(n),
            UiAction::Peak => {
                let peak = self.audio.as_ref().map_or(0.0, Audio::take_peak);
                self.say(format!("output peak {peak:.4}"));
                Ok(())
            }
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
                ui.heading(&self.plugin.name);
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
                if ui
                    .add_enabled(self.saved_state.is_some(), egui::Button::new("Restore state"))
                    .clicked()
                {
                    actions.push(UiAction::LoadState);
                }
                ui.separator();
                ui.label("Play:");
                for (label, note) in [("C", 60u8), ("E", 64), ("G", 67), ("C'", 72)] {
                    let r = ui.add(egui::Button::new(label).sense(egui::Sense::click_and_drag()));
                    let down = r.is_pointer_button_down_on();
                    match (down, self.held) {
                        (true, None) => {
                            self.held = Some(note);
                            actions.push(UiAction::NoteOn(note));
                        }
                        (false, Some(n)) if n == note => {
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
            let (timers, fds) = self.plugin.frame.counts();
            let up = self.started.elapsed().as_secs_f64().max(0.001);
            ui.label(format!(
                "buffer {bufsize} · DSP {load:.1}% · xruns {xruns} · editor {}x{} (X11 sees {:?}) · \
                 dock {:?} · timers {timers} · fds {fds} · {:.1} frames/s · {:.1} wakeups/s · \
                 focus {:?} · resize requests {}",
                self.editor.size.0,
                self.editor.size.1,
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
            let (ew, eh) = self.editor.size;
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
    Scroll(u32, u32),
    NoteOn(u8),
    NoteOff(u8),
    /// Set the n-th visible automatable parameter to a normalised value (0..1).
    Param(u32, f64),
    /// Log the controller's current value of the n-th parameter.
    Probe(u32),
    /// Log the output peak since the last check.
    Peak,
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
            UserEvent::FdReady => run.on_fd_ready(),
            UserEvent::ViewResize(w, h) => {
                run.editor.resize_requests += 1;
                // Plugins often echo our own onSize back; only act on a real change.
                if (w, h) != run.editor.size {
                    run.editor.size = (w, h);
                    let _ = run.dock.set_canvas_size(w, h);
                    // VST3: after a resizeView request the host tells the view the new size.
                    run.resize_view(w, h);
                    run.ask_redraw();
                }
            }
            UserEvent::Restart(flags) => {
                // Not acted on: setComponentState itself makes JUCE ask for a restart, so
                // re-syncing here recurses forever (seen: an endless 0x4 loop).
                run.say(format!("plugin asked for restart, flags {flags:#x}"));
            }
        }
        run.service_plugin();
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
        // redraw forever (~140 fps here). Only input events may ask for a repaint.
        let wants_repaint = response.repaint && !matches!(event, WindowEvent::RedrawRequested);
        match event {
            WindowEvent::CloseRequested => {
                run.close_editor();
                if let Some(audio) = run.audio.take() {
                    audio.stop(&mut run.plugin);
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
                run.requested = false;
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

/// `S3_SCRIPT="3:param 0 0.9; 4:save; 5:param 0 0.1; 6:load; 7:probe 0; 9:quit"`:
/// timed actions for repeatable tests (seconds since start).
fn parse_script() -> Vec<(Duration, UiAction)> {
    let Ok(script) = std::env::var("S3_SCRIPT") else {
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
                "quit" => UiAction::Quit,
                "scroll" => {
                    UiAction::Scroll(words.next()?.parse().ok()?, words.next()?.parse().ok()?)
                }
                "on" => UiAction::NoteOn(words.next()?.parse().ok()?),
                "off" => UiAction::NoteOff(words.next()?.parse().ok()?),
                "param" => {
                    UiAction::Param(words.next()?.parse().ok()?, words.next()?.parse().ok()?)
                }
                "peak" => UiAction::Peak,
                "probe" => UiAction::Probe(words.next()?.parse().ok()?),
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
