//! The features a host hands to a plugin or its UI (`LV2_Feature` arrays), and what this host
//! can offer.

use std::ffi::{CStr, CString, c_char, c_void};
use std::sync::Arc;

use super::abi::*;
use super::urid::{Urids, map_cb, unmap_cb};

pub const URID_MAP: &str = "http://lv2plug.in/ns/ext/urid#map";
pub const URID_UNMAP: &str = "http://lv2plug.in/ns/ext/urid#unmap";
pub const OPTIONS: &str = "http://lv2plug.in/ns/ext/options#options";
pub const BOUNDED_BLOCK: &str = "http://lv2plug.in/ns/ext/buf-size#boundedBlockLength";
pub const LOG: &str = "http://lv2plug.in/ns/ext/log#log";
pub const MAP_PATH: &str = "http://lv2plug.in/ns/ext/state#mapPath";
pub const LOAD_DEFAULT_STATE: &str = "http://lv2plug.in/ns/ext/state#loadDefaultState";
pub const IS_LIVE: &str = "http://lv2plug.in/ns/lv2core#isLive";
pub const IN_PLACE_BROKEN: &str = "http://lv2plug.in/ns/lv2core#inPlaceBroken";
pub const HARD_RT: &str = "http://lv2plug.in/ns/lv2core#hardRTCapable";
pub const UI_PARENT: &str = "http://lv2plug.in/ns/extensions/ui#parent";
pub const UI_RESIZE: &str = "http://lv2plug.in/ns/extensions/ui#resize";
pub const UI_IDLE: &str = "http://lv2plug.in/ns/extensions/ui#idleInterface";
pub const INSTANCE_ACCESS: &str = "http://lv2plug.in/ns/ext/instance-access";
pub const UI_IDLE_EXT: &str = "http://lv2plug.in/ns/extensions/ui#idleInterface";
pub const STATE_INTERFACE: &str = "http://lv2plug.in/ns/ext/state#interface";

const PARAM_SAMPLE_RATE: &str = "http://lv2plug.in/ns/ext/parameters#sampleRate";
const BUFSZ_MIN: &str = "http://lv2plug.in/ns/ext/buf-size#minBlockLength";
const BUFSZ_MAX: &str = "http://lv2plug.in/ns/ext/buf-size#maxBlockLength";
const BUFSZ_NOMINAL: &str = "http://lv2plug.in/ns/ext/buf-size#nominalBlockLength";
const BUFSZ_SEQUENCE: &str = "http://lv2plug.in/ns/ext/buf-size#sequenceSize";
const UI_SCALE: &str = "http://lv2plug.in/ns/extensions/ui#scaleFactor";
const ATOM_FLOAT: &str = "http://lv2plug.in/ns/ext/atom#Float";
const ATOM_INT: &str = "http://lv2plug.in/ns/ext/atom#Int";
const OPTIONS_INSTANCE: &str = "http://lv2plug.in/ns/ext/options#Instance";

/// Whether a plugin that requires this feature can be hosted here.
pub fn supported(uri: &str) -> bool {
    matches!(
        uri,
        URID_MAP
            | URID_UNMAP
            | OPTIONS
            | BOUNDED_BLOCK
            | LOG
            | MAP_PATH
            | LOAD_DEFAULT_STATE
            | IS_LIVE
            | IN_PLACE_BROKEN
            | HARD_RT
            | super::worker::WORKER_SCHEDULE
    )
}

/// What a UI needs that this host provides (besides what the plugin features give).
pub fn ui_supported(uri: &str) -> bool {
    supported(uri) || matches!(uri, UI_PARENT | UI_RESIZE | UI_IDLE | INSTANCE_ACCESS)
}

// `printf` and `vprintf` are variadic in C. A function taking the fixed arguments only is
// called correctly by them on the platforms this runs on (the extra arguments are ignored),
// and a plugin's log text is not something this host shows.
unsafe extern "C" fn log_printf(_: *mut c_void, _: u32, _: *const c_char) -> i32 {
    0
}

unsafe extern "C" fn log_vprintf(_: *mut c_void, _: u32, _: *const c_char, _: *mut c_void) -> i32 {
    0
}

/// There are no files to move: an abstract path is the absolute one.
unsafe extern "C" fn same_path(_: *mut c_void, path: *const c_char) -> *mut c_char {
    if path.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: a C string from the plugin; the copy is freed by the plugin with `free`.
    unsafe { libc::strdup(path) }
}

/// A float or an int option value, kept alive with the feature set.
enum Value {
    Float(Box<f32>),
    Int(Box<i32>),
}

/// The features of one plugin or UI instance. Pointers into it stay valid while it lives.
pub struct FeatureSet {
    pub urids: Arc<Urids>,
    _map: Box<UridMap>,
    _unmap: Box<UridUnmap>,
    _log: Box<Log>,
    _map_path: Box<MapPath>,
    _options: Box<[OptionsOption]>,
    _values: Vec<Value>,
    _uris: Vec<CString>,
    _extra: Vec<Box<dyn std::any::Any>>,
    _features: Vec<Feature>,
    pointers: Vec<*const Feature>,
}

// SAFETY: the raw pointers point into the boxes this struct owns.
unsafe impl Send for FeatureSet {}

pub struct Audio {
    pub sample_rate: f64,
    pub min_block: i32,
    pub max_block: i32,
    pub scale: f32,
}

impl FeatureSet {
    /// The features for a plugin (`ui == false`) or a UI. `extra` are further features, as
    /// `(uri, data)`; the data must outlive the set.
    pub fn new(urids: Arc<Urids>, audio: &Audio, extra: Vec<(&str, *mut c_void)>) -> Self {
        let handle = Arc::as_ptr(&urids) as *mut c_void;
        let map = Box::new(UridMap {
            handle,
            map: map_cb,
        });
        let unmap = Box::new(UridUnmap {
            handle,
            unmap: unmap_cb,
        });
        let log = Box::new(Log {
            handle: std::ptr::null_mut(),
            printf: log_printf,
            vprintf: log_vprintf,
        });
        let map_path = Box::new(MapPath {
            handle: std::ptr::null_mut(),
            abstract_path: same_path,
            absolute_path: same_path,
        });

        let float = urids.map(ATOM_FLOAT);
        let int = urids.map(ATOM_INT);
        let ctx = urids.map(OPTIONS_INSTANCE);
        let mut values = Vec::new();
        let mut options: Vec<OptionsOption> = Vec::new();
        let mut add = |key: &str, value: Value, options: &mut Vec<OptionsOption>| {
            let (ptr, kind): (*const c_void, u32) = match &value {
                Value::Float(b) => (&**b as *const f32 as *const c_void, float),
                Value::Int(b) => (&**b as *const i32 as *const c_void, int),
            };
            options.push(OptionsOption {
                context: ctx,
                subject: 0,
                key: urids.map(key),
                size: 4,
                kind,
                value: ptr,
            });
            values.push(value);
        };
        add(
            PARAM_SAMPLE_RATE,
            Value::Float(Box::new(audio.sample_rate as f32)),
            &mut options,
        );
        add(
            BUFSZ_MIN,
            Value::Int(Box::new(audio.min_block)),
            &mut options,
        );
        add(
            BUFSZ_MAX,
            Value::Int(Box::new(audio.max_block)),
            &mut options,
        );
        add(
            BUFSZ_NOMINAL,
            Value::Int(Box::new(audio.max_block.min(1024))),
            &mut options,
        );
        add(BUFSZ_SEQUENCE, Value::Int(Box::new(8192)), &mut options);
        add(UI_SCALE, Value::Float(Box::new(audio.scale)), &mut options);
        // The list ends with an option whose key is 0.
        options.push(OptionsOption {
            context: 0,
            subject: 0,
            key: 0,
            size: 0,
            kind: 0,
            value: std::ptr::null(),
        });
        let options = options.into_boxed_slice();

        let mut uris = Vec::new();
        let mut features = Vec::new();
        let mut push = |uri: &str, data: *mut c_void| {
            let c = CString::new(uri).unwrap();
            features.push(Feature {
                uri: c.as_ptr(),
                data,
            });
            uris.push(c);
        };
        push(URID_MAP, &*map as *const UridMap as *mut c_void);
        push(URID_UNMAP, &*unmap as *const UridUnmap as *mut c_void);
        push(OPTIONS, options.as_ptr() as *mut c_void);
        push(BOUNDED_BLOCK, std::ptr::null_mut());
        push(LOG, &*log as *const Log as *mut c_void);
        push(MAP_PATH, &*map_path as *const MapPath as *mut c_void);
        push(LOAD_DEFAULT_STATE, std::ptr::null_mut());
        push(IS_LIVE, std::ptr::null_mut());
        push(IN_PLACE_BROKEN, std::ptr::null_mut());
        push(HARD_RT, std::ptr::null_mut());
        for (uri, data) in extra {
            push(uri, data);
        }
        let mut pointers: Vec<*const Feature> =
            features.iter().map(|f| f as *const Feature).collect();
        pointers.push(std::ptr::null());
        Self {
            urids,
            _map: map,
            _unmap: unmap,
            _log: log,
            _map_path: map_path,
            _options: options,
            _values: values,
            _uris: uris,
            _extra: Vec::new(),
            _features: features,
            pointers,
        }
    }

    pub fn as_ptr(&self) -> *const *const Feature {
        self.pointers.as_ptr()
    }
}

/// A C string as text.
///
/// # Safety
/// `p` is null or a NUL-terminated string.
pub unsafe fn text(p: *const c_char) -> String {
    if p.is_null() {
        return String::new();
    }
    // SAFETY: see above.
    unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
}
