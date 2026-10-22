//! The LV2 plugin and UI ABI: the C structures of the core specification and the extensions a
//! host here supports (urid, options, atom, state, ui). Written from the specifications.
#![allow(dead_code)]

use std::ffi::{c_char, c_void};

pub type Handle = *mut c_void;

#[repr(C)]
pub struct Feature {
    pub uri: *const c_char,
    pub data: *mut c_void,
}

#[repr(C)]
pub struct Descriptor {
    pub uri: *const c_char,
    pub instantiate: unsafe extern "C" fn(
        *const Descriptor,
        f64,
        *const c_char,
        *const *const Feature,
    ) -> Handle,
    pub connect_port: unsafe extern "C" fn(Handle, u32, *mut c_void),
    pub activate: Option<unsafe extern "C" fn(Handle)>,
    pub run: unsafe extern "C" fn(Handle, u32),
    pub deactivate: Option<unsafe extern "C" fn(Handle)>,
    pub cleanup: unsafe extern "C" fn(Handle),
    pub extension_data: Option<unsafe extern "C" fn(*const c_char) -> *const c_void>,
}

pub type DescriptorFn = unsafe extern "C" fn(u32) -> *const Descriptor;

#[repr(C)]
pub struct UridMap {
    pub handle: *mut c_void,
    pub map: unsafe extern "C" fn(*mut c_void, *const c_char) -> u32,
}

#[repr(C)]
pub struct UridUnmap {
    pub handle: *mut c_void,
    pub unmap: unsafe extern "C" fn(*mut c_void, u32) -> *const c_char,
}

#[repr(C)]
pub struct OptionsOption {
    pub context: u32,
    pub subject: u32,
    pub key: u32,
    pub size: u32,
    pub kind: u32,
    pub value: *const c_void,
}

/// `LV2_Log_Log`.
#[repr(C)]
pub struct Log {
    pub handle: *mut c_void,
    pub printf: unsafe extern "C" fn(*mut c_void, u32, *const c_char) -> i32,
    pub vprintf: unsafe extern "C" fn(*mut c_void, u32, *const c_char, *mut c_void) -> i32,
}

#[repr(C)]
pub struct Atom {
    pub size: u32,
    pub kind: u32,
}

#[repr(C)]
pub struct SequenceBody {
    pub unit: u32,
    pub pad: u32,
}

#[repr(C)]
pub struct AtomEvent {
    pub frames: i64,
    pub body: Atom,
}

#[repr(C)]
pub struct MapPath {
    pub handle: *mut c_void,
    pub abstract_path: unsafe extern "C" fn(*mut c_void, *const c_char) -> *mut c_char,
    pub absolute_path: unsafe extern "C" fn(*mut c_void, *const c_char) -> *mut c_char,
}

pub type StateStore = unsafe extern "C" fn(*mut c_void, u32, *const c_void, usize, u32, u32) -> i32;
pub type StateRetrieve =
    unsafe extern "C" fn(*mut c_void, u32, *mut usize, *mut u32, *mut u32) -> *const c_void;

#[repr(C)]
pub struct StateInterface {
    pub save:
        unsafe extern "C" fn(Handle, StateStore, *mut c_void, u32, *const *const Feature) -> i32,
    pub restore:
        unsafe extern "C" fn(Handle, StateRetrieve, *mut c_void, u32, *const *const Feature) -> i32,
}

pub const STATE_IS_POD: u32 = 1;
pub const STATE_IS_PORTABLE: u32 = 2;

// ---- UI ----

pub type UiWrite = unsafe extern "C" fn(*mut c_void, u32, u32, u32, *const c_void);

#[repr(C)]
pub struct UiDescriptor {
    pub uri: *const c_char,
    pub instantiate: unsafe extern "C" fn(
        *const UiDescriptor,
        *const c_char,
        *const c_char,
        UiWrite,
        *mut c_void,
        *mut *mut c_void,
        *const *const Feature,
    ) -> Handle,
    pub cleanup: unsafe extern "C" fn(Handle),
    pub port_event: unsafe extern "C" fn(Handle, u32, u32, u32, *const c_void),
    pub extension_data: Option<unsafe extern "C" fn(*const c_char) -> *const c_void>,
}

pub type UiDescriptorFn = unsafe extern "C" fn(u32) -> *const UiDescriptor;

#[repr(C)]
pub struct UiResize {
    pub handle: *mut c_void,
    pub ui_resize: unsafe extern "C" fn(*mut c_void, i32, i32) -> i32,
}

#[repr(C)]
pub struct UiIdle {
    pub idle: unsafe extern "C" fn(Handle) -> i32,
}
