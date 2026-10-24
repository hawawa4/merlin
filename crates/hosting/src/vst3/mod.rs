//! VST3 hosting on the `vst3` crate (plan §4.5, decision from Phase 0: Rust only, no C++ shim).
//!
//! Library loading and class enumeration, plugin instances (component, controller, connection,
//! editor view), the host objects they call back into, and the audio-thread processor.
//!
//! Known gaps: only the first audio input/output bus and the first event input are used, no
//! `IUnitInfo`/programs, no sample-accurate parameter ramps, no `IProcessContextRequirements`.

mod fd;
mod host;
mod module;
mod processor;

use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::ptr::null_mut;
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result, anyhow, bail};
use merlin_model::Format;
use vst3::Steinberg::Linux::{IEventHandlerTrait, ITimerHandlerTrait};
use vst3::Steinberg::Vst::*;
use vst3::Steinberg::*;
use vst3::{ComPtr, ComWrapper, Interface};

use crate::{
    EditorSize, ParamInfo, PluginEditor, PluginHandle, PluginInfo, PluginProcessor, Waker,
};
use fd::FdPoller;
use host::{HostCtx, HostFrame, MemStream, pack_size, save_with};
pub use module::{ClassInfo, tuid_from_hex, tuid_to_hex};
use module::{Module, utf16};
use processor::{MAX_FRAMES, Vst3Processor};

// Enum values from the SDK headers (the bindings expose them in nested modules).
const K_AUDIO: i32 = 0;
const K_EVENT: i32 = 1;
const K_INPUT: i32 = 0;
const K_OUTPUT: i32 = 1;

/// Magic at the start of our saved state: component and controller state in one blob.
const STATE_MAGIC: &[u8; 4] = b"IV3S";

/// A loaded `.vst3` bundle.
pub struct Vst3Library {
    module: Arc<Module>,
    pub path: PathBuf,
}

/// What a class is, as the browser wants to know: instrument or effect, from the factory's
/// sub-categories (`"Instrument|Synth"`, `"Fx|Reverb"`), mapped to the CLAP feature names the
/// rest of the app uses.
fn features_of(subcategories: &str) -> Vec<String> {
    let mut features: Vec<String> = subcategories
        .split('|')
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty())
        .collect();
    if features.iter().any(|f| f == "fx") {
        features.push("audio-effect".into());
    }
    if features.iter().any(|f| f == "synth") {
        features.push("synthesizer".into());
    }
    features
}

impl Vst3Library {
    pub fn load(bundle: &Path) -> Result<Self> {
        Ok(Self {
            module: Arc::new(Module::load(bundle)?),
            path: bundle.to_owned(),
        })
    }

    fn audio_classes(&self) -> Vec<ClassInfo> {
        self.module
            .classes()
            .into_iter()
            .filter(|c| c.category == "Audio Module Class")
            .collect()
    }

    /// Every audio plugin in the bundle.
    pub fn plugins(&self) -> Vec<PluginInfo> {
        self.audio_classes()
            .into_iter()
            .map(|c| PluginInfo {
                format: Format::Vst3,
                id: c.id(),
                features: features_of(&c.subcategories),
                name: c.name,
                vendor: c.vendor,
                version: c.version,
            })
            .collect()
    }

    /// Creates an instance (main thread). The plugin is not activated yet.
    pub fn create(&self, class_id: &str, waker: Arc<dyn Waker>) -> Result<Vst3Plugin> {
        let class = self
            .audio_classes()
            .into_iter()
            .find(|c| c.id().eq_ignore_ascii_case(class_id))
            .with_context(|| format!("no VST3 class '{class_id}' in {}", self.path.display()))?;
        Vst3Plugin::new(self.module.clone(), &class, waker)
    }
}

struct Buses {
    /// Channel counts per audio bus.
    audio_in: Vec<i32>,
    audio_out: Vec<i32>,
    event_in: i32,
}

struct EditorState {
    view: Option<ComPtr<IPlugView>>,
    size: EditorSize,
    can_resize: bool,
}

pub struct Vst3Plugin {
    info: PluginInfo,
    fds: FdPoller,
    component: ComPtr<IComponent>,
    audio: ComPtr<IAudioProcessor>,
    controller: ComPtr<IEditController>,
    separate_controller: bool,
    connected: Option<(ComPtr<IConnectionPoint>, ComPtr<IConnectionPoint>)>,
    _host_app: ComPtr<IHostApplication>,
    host: ComWrapper<HostCtx>,
    frame: ComWrapper<HostFrame>,
    buses: Buses,
    midi_map: Box<[Option<u32>; 130]>,
    active: bool,
    reports: Option<rtrb::Consumer<(u32, f64)>>,
    editor: EditorState,
    // Last: the library must outlive every object created from it.
    module: Arc<Module>,
}

unsafe fn create<I: Interface>(factory: &ComPtr<IPluginFactory>, cid: &TUID) -> Result<ComPtr<I>> {
    let mut obj: *mut c_void = null_mut();
    // SAFETY: both ids are 16-byte TUIDs; obj receives an owned interface pointer.
    let r = unsafe { factory.createInstance(cid.as_ptr(), I::IID.as_ptr().cast(), &mut obj) };
    if r != kResultOk {
        bail!("createInstance failed: {r}");
    }
    // SAFETY: createInstance gave us an owned reference of the requested interface.
    unsafe { ComPtr::from_raw(obj.cast::<I>()) }.context("createInstance returned null")
}

unsafe fn bus_channels(component: &ComPtr<IComponent>, media: i32, dir: i32) -> Vec<i32> {
    // SAFETY: zeroed BusInfo is a valid out-struct.
    unsafe {
        (0..component.getBusCount(media, dir))
            .filter_map(|i| {
                let mut info: BusInfo = std::mem::zeroed();
                (component.getBusInfo(media, dir, i, &mut info) == kResultOk)
                    .then_some(info.channelCount)
            })
            .collect()
    }
}

impl Vst3Plugin {
    fn new(module: Arc<Module>, class: &ClassInfo, waker: Arc<dyn Waker>) -> Result<Self> {
        let host = ComWrapper::new(HostCtx::new(waker.clone()));
        let frame = ComWrapper::new(HostFrame::new(waker.clone()));
        let host_app = host.to_com_ptr::<IHostApplication>().unwrap();
        let ctx = host_app.as_ptr().cast::<FUnknown>();
        let factory = module.factory();
        // SAFETY: COM calls on objects we just created, in the order the VST3 docs prescribe.
        unsafe {
            if let Some(f3) = factory.cast::<IPluginFactory3>() {
                f3.setHostContext(ctx);
            }
            let component: ComPtr<IComponent> = create(factory, &class.cid)?;
            let r = component.initialize(ctx);
            if r != kResultOk {
                bail!("component.initialize failed: {r}");
            }
            let audio = component
                .cast::<IAudioProcessor>()
                .context("component is not an IAudioProcessor")?;

            // Single-component plugins implement the controller on the same object.
            let (controller, separate_controller) = match component.cast::<IEditController>() {
                Some(c) => (c, false),
                None => {
                    let mut cid: TUID = [0; 16];
                    if component.getControllerClassId(&mut cid) != kResultOk {
                        bail!("no controller class");
                    }
                    let c: ComPtr<IEditController> = create(factory, &cid)?;
                    let r = c.initialize(ctx);
                    if r != kResultOk {
                        bail!("controller.initialize failed: {r}");
                    }
                    (c, true)
                }
            };
            let connected = match (
                component.cast::<IConnectionPoint>(),
                controller.cast::<IConnectionPoint>(),
            ) {
                (Some(a), Some(b)) if separate_controller => {
                    a.connect(b.as_ptr());
                    b.connect(a.as_ptr());
                    Some((a, b))
                }
                _ => None,
            };
            let handler = host.to_com_ptr::<IComponentHandler>().unwrap();
            controller.setComponentHandler(handler.as_ptr());

            let buses = Buses {
                audio_in: bus_channels(&component, K_AUDIO, K_INPUT),
                audio_out: bus_channels(&component, K_AUDIO, K_OUTPUT),
                event_in: component.getBusCount(K_EVENT, K_INPUT),
            };
            let mut midi_map = Box::new([None; 130]);
            if let Some(map) = controller.cast::<IMidiMapping>() {
                for (cc, slot) in midi_map.iter_mut().enumerate() {
                    let mut id = 0;
                    if map.getMidiControllerAssignment(0, 0, cc as i16, &mut id) == kResultOk {
                        *slot = Some(id);
                    }
                }
            }
            Ok(Self {
                info: PluginInfo {
                    format: Format::Vst3,
                    id: class.id(),
                    name: class.name.clone(),
                    vendor: class.vendor.clone(),
                    version: class.version.clone(),
                    features: features_of(&class.subcategories),
                },
                fds: FdPoller::spawn(waker),
                component,
                audio,
                controller,
                separate_controller,
                connected,
                _host_app: host_app,
                host,
                frame,
                buses,
                midi_map,
                active: false,
                reports: None,
                editor: EditorState {
                    view: None,
                    size: EditorSize {
                        width: 0,
                        height: 0,
                    },
                    can_resize: false,
                },
                module,
            })
        }
    }

    /// Hands the processor's current state to the controller (`setComponentState`), which
    /// otherwise starts out knowing nothing of it.
    fn sync_controller(&self) {
        if let Some(comp) = save_with(|s| unsafe { self.component.getState(s) }) {
            let s = MemStream::with_data(&comp);
            let p = s.to_com_ptr::<IBStream>().unwrap();
            // SAFETY: the stream lives through the call.
            unsafe { self.controller.setComponentState(p.as_ptr()) };
        }
    }

    pub fn latency_samples(&self) -> u32 {
        // SAFETY: simple query.
        unsafe { self.audio.getLatencySamples() }
    }
}

fn pack_state(comp: &[u8], ctrl: Option<&[u8]>) -> Vec<u8> {
    let mut out = Vec::with_capacity(comp.len() + ctrl.map_or(0, <[u8]>::len) + 16);
    out.extend_from_slice(STATE_MAGIC);
    out.extend_from_slice(&(comp.len() as u32).to_le_bytes());
    out.extend_from_slice(comp);
    match ctrl {
        Some(c) => {
            out.extend_from_slice(&(c.len() as u32).to_le_bytes());
            out.extend_from_slice(c);
        }
        None => out.extend_from_slice(&u32::MAX.to_le_bytes()),
    }
    out
}

fn unpack_state(data: &[u8]) -> Option<(&[u8], Option<&[u8]>)> {
    let rest = data.strip_prefix(STATE_MAGIC)?;
    let (len, rest) = rest.split_first_chunk::<4>()?;
    let comp_len = u32::from_le_bytes(*len) as usize;
    let comp = rest.get(..comp_len)?;
    let rest = &rest[comp_len..];
    let (len, rest) = rest.split_first_chunk::<4>()?;
    let ctrl_len = u32::from_le_bytes(*len);
    let ctrl = (ctrl_len != u32::MAX)
        .then(|| rest.get(..ctrl_len as usize))
        .flatten();
    Some((comp, ctrl))
}

impl PluginHandle for Vst3Plugin {
    fn info(&self) -> &PluginInfo {
        &self.info
    }

    fn activate(&mut self, sample_rate: f64, max_block: u32) -> Result<Box<dyn PluginProcessor>> {
        if max_block as usize > MAX_FRAMES {
            return Err(anyhow!("block size {max_block} exceeds {MAX_FRAMES}"));
        }
        // SAFETY: setup calls on the main thread while not processing.
        unsafe {
            for dir in [K_INPUT, K_OUTPUT] {
                if self.component.getBusCount(K_AUDIO, dir) > 0 {
                    self.component.activateBus(K_AUDIO, dir, 0, 1);
                }
            }
            if self.buses.event_in > 0 {
                self.component.activateBus(K_EVENT, K_INPUT, 0, 1);
            }
            if self.audio.canProcessSampleSize(0) != kResultOk {
                bail!("plugin cannot process 32-bit float");
            }
            let mut setup = ProcessSetup {
                processMode: 0,        // kRealtime
                symbolicSampleSize: 0, // kSample32
                maxSamplesPerBlock: max_block as i32,
                sampleRate: sample_rate,
            };
            let r = self.audio.setupProcessing(&mut setup);
            if r != kResultOk {
                bail!("setupProcessing failed: {r}");
            }
            let r = self.component.setActive(1);
            if r != kResultOk {
                bail!("setActive failed: {r}");
            }
            self.audio.setProcessing(1);
        }
        self.active = true;

        let (edits_tx, edits_rx) = rtrb::RingBuffer::new(512);
        let (reports_tx, reports_rx) = rtrb::RingBuffer::new(512);
        *self.host.edits.lock().unwrap() = Some(edits_tx);
        self.reports = Some(reports_rx);
        self.sync_controller();
        // Only the first audio buses are activated, so only they get buffers.
        let first = |v: &[i32]| v.first().copied().into_iter().collect::<Vec<_>>();
        Ok(Box::new(Vst3Processor::new(
            self.audio.clone(),
            &first(&self.buses.audio_in),
            &first(&self.buses.audio_out),
            edits_rx,
            reports_tx,
            self.midi_map.clone(),
            sample_rate,
            self.module.clone(),
        )))
    }

    fn deactivate(&mut self) {
        if std::mem::take(&mut self.active) {
            // SAFETY: the audio thread has stopped calling process().
            unsafe {
                self.audio.setProcessing(0);
                self.component.setActive(0);
            }
        }
        *self.host.edits.lock().unwrap() = None;
        self.reports = None;
    }

    fn params(&mut self) -> Vec<ParamInfo> {
        // SAFETY: zeroed ParameterInfo is a valid out-struct.
        unsafe {
            (0..self.controller.getParameterCount())
                .filter_map(|i| {
                    let mut info: ParameterInfo = std::mem::zeroed();
                    (self.controller.getParameterInfo(i, &mut info) == kResultOk).then_some(info)
                })
                .map(|p| ParamInfo {
                    id: p.id,
                    name: utf16(&p.title),
                    module: String::new(),
                    // VST3 parameters are normalised.
                    min: 0.0,
                    max: 1.0,
                    default: p.defaultNormalizedValue,
                    // kCanAutomate = 1, kIsReadOnly = 2, kIsHidden = 16
                    automatable: p.flags & 1 != 0,
                    stepped: p.stepCount > 0,
                    hidden: p.flags & 16 != 0,
                    read_only: p.flags & 2 != 0,
                })
                .collect()
        }
    }

    fn param_value(&mut self, id: u32) -> Option<f64> {
        // SAFETY: a plain read on the main thread.
        Some(unsafe { self.controller.getParamNormalized(id) })
    }

    fn set_param(&mut self, id: u32, value: f64) {
        self.host.send_param(id, value);
        // SAFETY: simple controller call on the main thread; keeps the editor in step.
        unsafe { self.controller.setParamNormalized(id, value) };
    }

    fn save_state(&mut self) -> Result<Vec<u8>> {
        // SAFETY: the plugin writes into streams we own for the duration of the call.
        let comp = save_with(|s| unsafe { self.component.getState(s) })
            .context("component.getState failed")?;
        let ctrl = save_with(|s| unsafe { self.controller.getState(s) });
        Ok(pack_state(&comp, ctrl.as_deref()))
    }

    fn load_state(&mut self, data: &[u8]) -> Result<()> {
        let (comp, ctrl) = unpack_state(data).context("not a saved VST3 state")?;
        // SAFETY: the plugin reads from streams we own for the duration of each call.
        unsafe {
            let s = MemStream::with_data(comp);
            let p = s.to_com_ptr::<IBStream>().unwrap();
            if self.component.setState(p.as_ptr()) != kResultOk {
                bail!("component.setState failed");
            }
            // The controller must be told about the processor's state, or its UI is stale.
            let s = MemStream::with_data(comp);
            let p = s.to_com_ptr::<IBStream>().unwrap();
            self.controller.setComponentState(p.as_ptr());
            if let Some(ctrl) = ctrl {
                let s = MemStream::with_data(ctrl);
                let p = s.to_com_ptr::<IBStream>().unwrap();
                self.controller.setState(p.as_ptr());
            }
        }
        Ok(())
    }

    fn editor(&mut self) -> Option<&mut dyn PluginEditor> {
        Some(self)
    }

    fn on_main_thread(&mut self) {
        // `restartComponent` requests. Not acted on: setComponentState itself makes some
        // plugins (JUCE) ask for a restart with kParamValuesChanged, so re-syncing here
        // recurses forever (seen in S3).
        let _ = self
            .host
            .restart_flags
            .swap(0, std::sync::atomic::Ordering::AcqRel);
    }

    fn service(&mut self, now: Instant) {
        // Parameter changes the processor reported (state loads, internal automation) go to the
        // controller so the editor shows them.
        if let Some(rx) = self.reports.as_mut() {
            while let Ok((id, v)) = rx.pop() {
                // SAFETY: simple controller call on the main thread.
                unsafe { self.controller.setParamNormalized(id, v) };
            }
        }
        for handler in self.frame.due_timers(now) {
            // SAFETY: the handler is the plugin's live object.
            unsafe { handler.onTimer() };
        }
        if let Some(fds) = self.frame.take_fds_changed() {
            self.fds.set(&fds);
        }
    }

    fn next_deadline(&self) -> Option<Instant> {
        self.frame.next_timer_deadline()
    }

    fn on_fd_ready(&mut self) {
        let watched = self.frame.watched_fds();
        let ready = self.fds.ready(&watched);
        for (handler, fd) in self.frame.handlers_for(&ready) {
            // SAFETY: the handler is the plugin's live object.
            unsafe { handler.onFDIsSet(fd) };
        }
        self.fds.ack();
        self.service(Instant::now());
    }
}

fn rect_size(r: &ViewRect) -> EditorSize {
    EditorSize {
        width: (r.right - r.left).max(1) as u32,
        height: (r.bottom - r.top).max(1) as u32,
    }
}

impl Vst3Plugin {
    fn tell_view_size(&mut self, size: EditorSize) -> bool {
        let Some(view) = &self.editor.view else {
            return false;
        };
        let mut rect = ViewRect {
            left: 0,
            top: 0,
            right: size.width as i32,
            bottom: size.height as i32,
        };
        // The plugin echoes our own onSize back as a resize request; remember this size so the
        // echo is not forwarded as news.
        self.frame.known_size.store(
            pack_size(size.width, size.height),
            std::sync::atomic::Ordering::Release,
        );
        // SAFETY: valid rect for the call.
        let ok = unsafe { view.onSize(&mut rect) } == kResultOk;
        if ok {
            self.editor.size = size;
        }
        ok
    }
}

impl PluginEditor for Vst3Plugin {
    fn attach(&mut self, parent: u32, scale: f64) -> Result<EditorSize> {
        // SAFETY: COM calls on the plugin's view and our frame, main thread, in the order the
        // VST3 docs prescribe (createView, setFrame, attached).
        let view = unsafe {
            let raw = self.controller.createView(c"editor".as_ptr());
            ComPtr::from_raw(raw).context("plugin has no editor view")?
        };
        let size = unsafe {
            if view.isPlatformTypeSupported(kPlatformTypeX11EmbedWindowID) != kResultTrue {
                return Err(anyhow!("plugin cannot embed an X11 editor"));
            }
            if let Some(support) = view.cast::<IPlugViewContentScaleSupport>() {
                support.setContentScaleFactor(scale as f32);
            }
            // The frame carries IRunLoop: JUCE looks for it in setFrame/attached.
            let frame = self.frame.to_com_ptr::<IPlugFrame>().unwrap();
            view.setFrame(frame.as_ptr());
            let mut rect: ViewRect = std::mem::zeroed();
            view.getSize(&mut rect);
            self.editor.can_resize = view.canResize() == kResultTrue;
            let r = view.attached(
                parent as usize as *mut c_void,
                kPlatformTypeX11EmbedWindowID,
            );
            if r != kResultOk {
                view.setFrame(null_mut());
                return Err(anyhow!("IPlugView::attached failed: {r}"));
            }
            view.getSize(&mut rect);
            rect_size(&rect)
        };
        self.frame.known_size.store(
            pack_size(size.width, size.height),
            std::sync::atomic::Ordering::Release,
        );
        self.editor.size = size;
        self.editor.view = Some(view);
        Ok(size)
    }

    fn is_attached(&self) -> bool {
        self.editor.view.is_some()
    }

    // VST3 views cannot be hidden, only removed; the dock unmaps the window instead.
    fn show(&mut self) {}

    fn hide(&mut self) {}

    fn size(&self) -> EditorSize {
        self.editor.size
    }

    fn can_resize(&self) -> bool {
        self.editor.can_resize
    }

    fn adjust_size(&mut self, want: EditorSize) -> EditorSize {
        let Some(view) = &self.editor.view else {
            return want;
        };
        let mut rect = ViewRect {
            left: 0,
            top: 0,
            right: want.width as i32,
            bottom: want.height as i32,
        };
        // SAFETY: valid rect for the call.
        unsafe { view.checkSizeConstraint(&mut rect) };
        rect_size(&rect)
    }

    fn set_size(&mut self, size: EditorSize) -> bool {
        self.tell_view_size(size)
    }

    /// After `resizeView` the host has to tell the view the new size.
    fn note_size(&mut self, size: EditorSize) {
        self.tell_view_size(size);
    }

    fn set_zoom(&mut self, scale: f64) -> Option<EditorSize> {
        let view = self.editor.view.as_ref()?;
        // SAFETY: COM calls on the plugin's view, main thread.
        let size = unsafe {
            let support = view.cast::<IPlugViewContentScaleSupport>()?;
            if support.setContentScaleFactor(scale as f32) != kResultOk {
                return None;
            }
            let mut rect: ViewRect = std::mem::zeroed();
            view.getSize(&mut rect);
            rect_size(&rect)
        };
        self.frame.known_size.store(
            pack_size(size.width, size.height),
            std::sync::atomic::Ordering::Release,
        );
        self.editor.size = size;
        Some(size)
    }

    fn detach(&mut self) {
        if let Some(view) = self.editor.view.take() {
            // SAFETY: detach in the reverse order of attach.
            unsafe {
                view.removed();
                view.setFrame(null_mut());
            }
        }
        self.frame.clear();
        self.fds.set(&[]);
    }
}

impl Drop for Vst3Plugin {
    fn drop(&mut self) {
        self.detach();
        self.deactivate();
        // SAFETY: teardown in the reverse order of `new`.
        unsafe {
            self.controller.setComponentHandler(null_mut());
            if let Some((a, b)) = &self.connected {
                a.disconnect(b.as_ptr());
                b.disconnect(a.as_ptr());
            }
            if self.separate_controller {
                self.controller.terminate();
            }
            self.component.terminate();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_blobs_round_trip_and_reject_garbage() {
        let blob = pack_state(b"component", Some(b"controller"));
        assert_eq!(
            unpack_state(&blob),
            Some((&b"component"[..], Some(&b"controller"[..])))
        );
        let blob = pack_state(b"component", None);
        assert_eq!(unpack_state(&blob), Some((&b"component"[..], None)));
        assert_eq!(unpack_state(b"nope"), None);
        assert_eq!(unpack_state(&blob[..6]), None, "truncated");
    }

    #[test]
    fn sub_categories_become_features() {
        assert!(features_of("Instrument|Synth").contains(&"instrument".to_string()));
        assert!(features_of("Instrument|Synth").contains(&"synthesizer".to_string()));
        let fx = features_of("Fx|Reverb");
        assert!(fx.contains(&"audio-effect".to_string()));
        assert!(!fx.contains(&"instrument".to_string()));
    }
}
