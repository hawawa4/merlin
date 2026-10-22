//! The URID table: URIs and the numbers plugins use for them.

use std::collections::HashMap;
use std::ffi::{CStr, CString, c_char, c_void};
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct Table {
    by_uri: HashMap<String, u32>,
    uris: Vec<CString>,
}

/// Shared by every plugin and UI instance of one app.
#[derive(Default)]
pub struct Urids {
    table: Mutex<Table>,
}

impl Urids {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn map(&self, uri: &str) -> u32 {
        let mut t = self.table.lock().unwrap();
        if let Some(&id) = t.by_uri.get(uri) {
            return id;
        }
        let Ok(c) = CString::new(uri) else { return 0 };
        t.uris.push(c);
        let id = t.uris.len() as u32; // 0 is "no URID"
        t.by_uri.insert(uri.to_owned(), id);
        id
    }

    pub fn unmap(&self, id: u32) -> Option<String> {
        let t = self.table.lock().unwrap();
        let c = t.uris.get((id as usize).checked_sub(1)?)?;
        Some(c.to_string_lossy().into_owned())
    }

    /// A pointer to the URI that stays valid for as long as the table lives (the strings are
    /// never removed or moved: `CString` keeps its bytes on the heap).
    fn unmap_ptr(&self, id: u32) -> *const c_char {
        let t = self.table.lock().unwrap();
        (id as usize)
            .checked_sub(1)
            .and_then(|i| t.uris.get(i))
            .map_or(std::ptr::null(), |c| c.as_ptr())
    }
}

/// `LV2_URID_Map::map`.
///
/// # Safety
/// `handle` is the `Urids` the map was built from; `uri` is a C string.
pub unsafe extern "C" fn map_cb(handle: *mut c_void, uri: *const c_char) -> u32 {
    if handle.is_null() || uri.is_null() {
        return 0;
    }
    // SAFETY: see above.
    let (urids, uri) = unsafe { (&*(handle as *const Urids), CStr::from_ptr(uri)) };
    urids.map(&uri.to_string_lossy())
}

/// `LV2_URID_Unmap::unmap`.
///
/// # Safety
/// `handle` is the `Urids` the map was built from.
pub unsafe extern "C" fn unmap_cb(handle: *mut c_void, id: u32) -> *const c_char {
    if handle.is_null() {
        return std::ptr::null();
    }
    // SAFETY: see above.
    unsafe { &*(handle as *const Urids) }.unmap_ptr(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uris_get_stable_numbers_and_come_back() {
        let u = Urids::new();
        let a = u.map("urn:a");
        let b = u.map("urn:b");
        assert_ne!(a, 0);
        assert_ne!(a, b);
        assert_eq!(u.map("urn:a"), a);
        assert_eq!(u.unmap(b).as_deref(), Some("urn:b"));
        assert_eq!(u.unmap(0), None);
        assert_eq!(u.unmap(999), None);
    }
}
