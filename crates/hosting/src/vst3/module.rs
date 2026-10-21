//! A loaded `.vst3` bundle and the classes in its factory.

use std::ffi::{c_char, c_void};
use std::path::Path;

use anyhow::{Context, Result, anyhow, bail};
use libloading::os::unix::Library;
use vst3::ComPtr;
use vst3::Steinberg::*;

pub fn cstr(chars: &[c_char]) -> String {
    let bytes: Vec<u8> = chars
        .iter()
        .take_while(|&&c| c != 0)
        .map(|&c| c as u8)
        .collect();
    String::from_utf8_lossy(&bytes).into_owned()
}

pub fn utf16(chars: &[u16]) -> String {
    let end = chars.iter().position(|&c| c == 0).unwrap_or(chars.len());
    String::from_utf16_lossy(&chars[..end])
}

#[derive(Clone, Debug)]
pub struct ClassInfo {
    pub cid: TUID,
    pub name: String,
    pub category: String,
    pub subcategories: String,
    pub vendor: String,
    pub version: String,
}

impl ClassInfo {
    /// The class id as 32 hex digits (what `PluginRef::id` stores).
    pub fn id(&self) -> String {
        tuid_to_hex(&self.cid)
    }
}

pub fn tuid_to_hex(tuid: &TUID) -> String {
    tuid.iter().map(|b| format!("{:02X}", *b as u8)).collect()
}

pub fn tuid_from_hex(hex: &str) -> Option<TUID> {
    if hex.len() != 32 {
        return None;
    }
    let mut out: TUID = [0; 16];
    for (i, slot) in out.iter_mut().enumerate() {
        *slot = u8::from_str_radix(hex.get(i * 2..i * 2 + 2)?, 16).ok()? as c_char;
    }
    Some(out)
}

/// A loaded bundle: `ModuleEntry` has run, `ModuleExit` runs on drop (after every plugin object
/// is released, which is why plugins keep an `Arc<Module>`).
pub struct Module {
    factory: Option<ComPtr<IPluginFactory>>,
    exit: Option<unsafe extern "C" fn() -> bool>,
    _lib: Library,
}

// SAFETY: the module is only a handle to a loaded library and its factory; the plugin objects
// created from it follow the VST3 threading rules in our own code.
unsafe impl Send for Module {}
unsafe impl Sync for Module {}

impl Module {
    pub fn load(bundle: &Path) -> Result<Self> {
        let stem = bundle
            .file_stem()
            .and_then(|s| s.to_str())
            .context("bundle path has no name")?;
        let binary = bundle
            .join("Contents/x86_64-linux")
            .join(format!("{stem}.so"));
        // SAFETY: loading a plugin binary runs its code; that is the point of a host.
        unsafe {
            let lib = Library::open(Some(&binary), libc::RTLD_NOW | libc::RTLD_LOCAL)
                .map_err(|e| anyhow!("dlopen {}: {e}", binary.display()))?;
            // The SDK passes the dlopen handle to ModuleEntry.
            let raw = lib.into_raw();
            let lib = Library::from_raw(raw);
            if let Ok(entry) =
                lib.get::<unsafe extern "C" fn(*mut c_void) -> bool>(b"ModuleEntry\0")
                && !entry(raw)
            {
                bail!("ModuleEntry returned false");
            }
            let exit = lib
                .get::<unsafe extern "C" fn() -> bool>(b"ModuleExit\0")
                .ok()
                .map(|s| *s);
            let get_factory = lib
                .get::<unsafe extern "C" fn() -> *mut IPluginFactory>(b"GetPluginFactory\0")
                .map_err(|e| anyhow!("no GetPluginFactory: {e}"))?;
            let factory = ComPtr::from_raw(get_factory()).context("GetPluginFactory gave null")?;
            Ok(Self {
                factory: Some(factory),
                exit,
                _lib: lib,
            })
        }
    }

    pub fn factory(&self) -> &ComPtr<IPluginFactory> {
        self.factory.as_ref().unwrap()
    }

    /// Every class in the factory (audio modules and controllers).
    pub fn classes(&self) -> Vec<ClassInfo> {
        let f = self.factory();
        let f2 = f.cast::<IPluginFactory2>();
        // SAFETY: plain factory calls with zeroed out-structs.
        unsafe {
            (0..f.countClasses())
                .filter_map(|i| {
                    if let Some(f2) = &f2 {
                        let mut info: PClassInfo2 = std::mem::zeroed();
                        (f2.getClassInfo2(i, &mut info) == kResultOk).then(|| ClassInfo {
                            cid: info.cid,
                            name: cstr(&info.name),
                            category: cstr(&info.category),
                            subcategories: cstr(&info.subCategories),
                            vendor: cstr(&info.vendor),
                            version: cstr(&info.version),
                        })
                    } else {
                        let mut info: PClassInfo = std::mem::zeroed();
                        (f.getClassInfo(i, &mut info) == kResultOk).then(|| ClassInfo {
                            cid: info.cid,
                            name: cstr(&info.name),
                            category: cstr(&info.category),
                            subcategories: String::new(),
                            vendor: String::new(),
                            version: String::new(),
                        })
                    }
                })
                .collect()
        }
    }
}

impl Drop for Module {
    fn drop(&mut self) {
        self.factory = None;
        if let Some(exit) = self.exit {
            // SAFETY: the SDK contract: called once, after all objects are released.
            unsafe { exit() };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn class_ids_round_trip_through_hex() {
        let tuid: TUID = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, -1];
        let hex = tuid_to_hex(&tuid);
        assert_eq!(hex, "0102030405060708090A0B0C0D0E0FFF");
        assert_eq!(tuid_from_hex(&hex), Some(tuid));
        assert_eq!(tuid_from_hex("short"), None);
        assert_eq!(tuid_from_hex("ZZ02030405060708090A0B0C0D0E0FFF"), None);
    }
}
