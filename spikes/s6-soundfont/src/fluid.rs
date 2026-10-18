//! Minimal FluidSynth binding, loaded at runtime from libfluidsynth.so.3. Runtime loading
//! means no -dev package to build, and the app can fall back gracefully (or to a bundled
//! copy) when the library is missing.

use std::ffi::{CStr, CString, c_char, c_double, c_int, c_void};
use std::path::Path;

use anyhow::{Result, anyhow};
use libloading::{Library, Symbol};

use crate::{Engine, PresetInfo, SR};

type Ptr = *mut c_void;

struct Api {
    _lib: Library,
    new_settings: unsafe extern "C" fn() -> Ptr,
    delete_settings: unsafe extern "C" fn(Ptr),
    setnum: unsafe extern "C" fn(Ptr, *const c_char, c_double) -> c_int,
    setint: unsafe extern "C" fn(Ptr, *const c_char, c_int) -> c_int,
    new_synth: unsafe extern "C" fn(Ptr) -> Ptr,
    delete_synth: unsafe extern "C" fn(Ptr),
    sfload: unsafe extern "C" fn(Ptr, *const c_char, c_int) -> c_int,
    get_sfont_by_id: unsafe extern "C" fn(Ptr, c_int) -> Ptr,
    sfont_iteration_start: unsafe extern "C" fn(Ptr),
    sfont_iteration_next: unsafe extern "C" fn(Ptr) -> Ptr,
    preset_get_name: unsafe extern "C" fn(Ptr) -> *const c_char,
    preset_get_banknum: unsafe extern "C" fn(Ptr) -> c_int,
    preset_get_num: unsafe extern "C" fn(Ptr) -> c_int,
    program_select: unsafe extern "C" fn(Ptr, c_int, c_int, c_int, c_int) -> c_int,
    noteon: unsafe extern "C" fn(Ptr, c_int, c_int, c_int) -> c_int,
    noteoff: unsafe extern "C" fn(Ptr, c_int, c_int) -> c_int,
    write_float:
        unsafe extern "C" fn(Ptr, c_int, *mut f32, c_int, c_int, *mut f32, c_int, c_int) -> c_int,
}

impl Api {
    fn load() -> Result<Self> {
        // SAFETY: loading a well-known system library; symbols are checked by name below and
        // their signatures match fluidsynth 2.x's public headers.
        unsafe {
            let lib = Library::new("libfluidsynth.so.3")?;
            macro_rules! sym {
                ($name:literal) => {{
                    let s: Symbol<_> = lib.get(concat!($name, "\0").as_bytes())?;
                    *s
                }};
            }
            Ok(Self {
                new_settings: sym!("new_fluid_settings"),
                delete_settings: sym!("delete_fluid_settings"),
                setnum: sym!("fluid_settings_setnum"),
                setint: sym!("fluid_settings_setint"),
                new_synth: sym!("new_fluid_synth"),
                delete_synth: sym!("delete_fluid_synth"),
                sfload: sym!("fluid_synth_sfload"),
                get_sfont_by_id: sym!("fluid_synth_get_sfont_by_id"),
                sfont_iteration_start: sym!("fluid_sfont_iteration_start"),
                sfont_iteration_next: sym!("fluid_sfont_iteration_next"),
                preset_get_name: sym!("fluid_preset_get_name"),
                preset_get_banknum: sym!("fluid_preset_get_banknum"),
                preset_get_num: sym!("fluid_preset_get_num"),
                program_select: sym!("fluid_synth_program_select"),
                noteon: sym!("fluid_synth_noteon"),
                noteoff: sym!("fluid_synth_noteoff"),
                write_float: sym!("fluid_synth_write_float"),
                _lib: lib,
            })
        }
    }
}

pub struct Fluid {
    api: Api,
    settings: Ptr,
    synth: Ptr,
    sfont_id: c_int,
}

impl Fluid {
    pub fn load(path: &Path) -> Result<Self> {
        let api = Api::load()?;
        let c = |s: &str| CString::new(s).unwrap();
        // SAFETY: FFI calls follow the fluidsynth API contract; pointers are checked.
        unsafe {
            let settings = (api.new_settings)();
            (api.setnum)(settings, c("synth.sample-rate").as_ptr(), SR as f64);
            // No worker threads: render happens on our (audio) thread only.
            (api.setint)(settings, c("synth.cpu-cores").as_ptr(), 1);
            (api.setint)(settings, c("synth.polyphony").as_ptr(), 256);
            // The engine calls the synth from the audio thread only, so drop the API mutex.
            let r = (api.setint)(settings, c("synth.threadsafe-api").as_ptr(), 0);
            debug_assert_eq!(r, 0, "synth.threadsafe-api rejected");
            let synth = (api.new_synth)(settings);
            if synth.is_null() {
                (api.delete_settings)(settings);
                return Err(anyhow!("new_fluid_synth failed"));
            }
            let file = c(path.to_str().ok_or_else(|| anyhow!("non-UTF-8 path"))?);
            let sfont_id = (api.sfload)(synth, file.as_ptr(), 1);
            if sfont_id < 0 {
                (api.delete_synth)(synth);
                (api.delete_settings)(settings);
                return Err(anyhow!("fluid_synth_sfload failed"));
            }
            Ok(Self {
                api,
                settings,
                synth,
                sfont_id,
            })
        }
    }
}

impl Drop for Fluid {
    fn drop(&mut self) {
        // SAFETY: both pointers were created by us and are deleted once.
        unsafe {
            (self.api.delete_synth)(self.synth);
            (self.api.delete_settings)(self.settings);
        }
    }
}

impl Engine for Fluid {
    fn name(&self) -> &'static str {
        "fluidsynth"
    }

    fn presets(&mut self) -> Vec<PresetInfo> {
        let mut out = Vec::new();
        // SAFETY: iteration over a loaded soundfont owned by our synth.
        unsafe {
            let sf = (self.api.get_sfont_by_id)(self.synth, self.sfont_id);
            if sf.is_null() {
                return out;
            }
            (self.api.sfont_iteration_start)(sf);
            loop {
                let p = (self.api.sfont_iteration_next)(sf);
                if p.is_null() {
                    break;
                }
                out.push(PresetInfo {
                    bank: (self.api.preset_get_banknum)(p),
                    program: (self.api.preset_get_num)(p),
                    name: CStr::from_ptr((self.api.preset_get_name)(p))
                        .to_string_lossy()
                        .into_owned(),
                });
            }
        }
        out
    }

    fn program(&mut self, ch: i32, bank: i32, program: i32) {
        // SAFETY: valid synth pointer.
        unsafe { (self.api.program_select)(self.synth, ch, self.sfont_id, bank, program) };
    }

    fn note_on(&mut self, ch: i32, key: i32, vel: i32) {
        // SAFETY: valid synth pointer.
        unsafe { (self.api.noteon)(self.synth, ch, key, vel) };
    }

    fn note_off(&mut self, ch: i32, key: i32) {
        // SAFETY: valid synth pointer.
        unsafe { (self.api.noteoff)(self.synth, ch, key) };
    }

    fn render(&mut self, l: &mut [f32], r: &mut [f32]) {
        let n = l.len().min(r.len()) as c_int;
        // SAFETY: both buffers hold at least n samples, written contiguously.
        unsafe {
            (self.api.write_float)(self.synth, n, l.as_mut_ptr(), 0, 1, r.as_mut_ptr(), 0, 1)
        };
    }
}
