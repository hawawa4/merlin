//! A running LV2 plugin instance, its saved state, and the values of its control ports.

use std::ffi::{CString, c_void};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use anyhow::{Context, Result, anyhow, bail};
use libloading::os::unix::Library;

use super::abi::*;
use super::features::{FeatureSet, STATE_INTERFACE};
use super::urid::Urids;
use super::worker::{WORKER_INTERFACE, Worker, WorkerInterface};

/// A plugin instance. The library and the features outlive the handle; `cleanup` runs last.
pub struct Instance {
    pub desc: *const Descriptor,
    pub handle: Handle,
    pub state: Option<*const StateInterface>,
    /// The worker the plugin schedules work to, and its thread.
    pub worker: Option<Arc<Worker>>,
    worker_thread: Option<std::thread::JoinHandle<()>>,
    // Dropped after `cleanup` (see `Drop`).
    pub features: FeatureSet,
    pub lib: Arc<Library>,
}

// SAFETY: an instance is created on the main thread, run on the audio thread, and saved and
// restored on the main thread, as the LV2 threading rules for plugins prescribe.
unsafe impl Send for Instance {}
unsafe impl Sync for Instance {}

impl Instance {
    pub fn new(
        lib: Arc<Library>,
        uri: &str,
        sample_rate: f64,
        bundle: &Path,
        features: FeatureSet,
        worker: Option<Arc<Worker>>,
    ) -> Result<Self> {
        // SAFETY: the entry point every LV2 plugin library exports.
        let entry: DescriptorFn = unsafe {
            *lib.get::<DescriptorFn>(b"lv2_descriptor\0")
                .map_err(|_| anyhow!("not an LV2 plugin library (no lv2_descriptor)"))?
        };
        let mut desc = std::ptr::null();
        for i in 0..4096 {
            // SAFETY: the plugin answers with its i-th descriptor, or null past the last.
            let d = unsafe { entry(i) };
            if d.is_null() {
                break;
            }
            // SAFETY: a descriptor the plugin owns.
            if unsafe { super::features::text((*d).uri) } == uri {
                desc = d;
                break;
            }
        }
        if desc.is_null() {
            bail!("the library does not contain {uri}");
        }
        let mut bundle_path = bundle.to_string_lossy().into_owned();
        if !bundle_path.ends_with('/') {
            bundle_path.push('/');
        }
        let bundle_c = CString::new(bundle_path)?;
        // SAFETY: the arguments are what LV2 defines; the features outlive the instance.
        let handle = unsafe {
            ((*desc).instantiate)(desc, sample_rate, bundle_c.as_ptr(), features.as_ptr())
        };
        if handle.is_null() {
            bail!("the plugin refused to start (instantiate returned null)");
        }
        let ext = CString::new(STATE_INTERFACE).unwrap();
        // SAFETY: extension_data is optional and returns static data.
        let state = unsafe {
            (*desc)
                .extension_data
                .map(|f| f(ext.as_ptr()) as *const StateInterface)
                .filter(|p| !p.is_null())
        };
        // A plugin that offers a worker interface gets its thread now that it exists.
        let wext = CString::new(WORKER_INTERFACE).unwrap();
        // SAFETY: as above.
        let winterface = unsafe {
            (*desc)
                .extension_data
                .map(|f| f(wext.as_ptr()) as *const WorkerInterface)
                .filter(|p| !p.is_null())
        };
        let worker_thread = match (&worker, winterface) {
            (Some(w), Some(i)) => Some(w.start(handle, i)),
            _ => None,
        };
        Ok(Self {
            desc,
            handle,
            state,
            worker,
            worker_thread,
            features,
            lib,
        })
    }

    pub fn urids(&self) -> &Arc<Urids> {
        &self.features.urids
    }
}

impl Drop for Instance {
    fn drop(&mut self) {
        // The worker thread uses the handle: it stops first.
        if let Some(w) = &self.worker {
            w.stop();
        }
        if let Some(t) = self.worker_thread.take() {
            let _ = t.join();
        }
        // SAFETY: the last use of the handle.
        unsafe { ((*self.desc).cleanup)(self.handle) };
    }
}

/// The values of a plugin's control ports, shared between the main thread (parameters, the UI)
/// and the audio thread.
pub struct Bank {
    values: Vec<AtomicU32>,
}

impl Bank {
    pub fn new(ports: usize, defaults: impl Fn(usize) -> f32) -> Self {
        Self {
            values: (0..ports)
                .map(|i| AtomicU32::new(defaults(i).to_bits()))
                .collect(),
        }
    }

    pub fn get(&self, port: usize) -> f32 {
        self.values
            .get(port)
            .map_or(0.0, |v| f32::from_bits(v.load(Ordering::Relaxed)))
    }

    pub fn set(&self, port: usize, value: f32) {
        if let Some(v) = self.values.get(port) {
            v.store(value.to_bits(), Ordering::Relaxed);
        }
    }
}

/// One saved property of a plugin's state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Property {
    pub key: String,
    pub kind: String,
    pub flags: u32,
    pub data: Vec<u8>,
}

struct Collector<'a> {
    urids: &'a Urids,
    props: Vec<Property>,
}

unsafe extern "C" fn store(
    handle: *mut c_void,
    key: u32,
    value: *const c_void,
    size: usize,
    kind: u32,
    flags: u32,
) -> i32 {
    // SAFETY: `handle` is the collector we passed to `save`; the value is `size` bytes.
    let c = unsafe { &mut *(handle as *mut Collector) };
    let (Some(key), Some(kind)) = (c.urids.unmap(key), c.urids.unmap(kind)) else {
        return 1;
    };
    let data = if value.is_null() || size == 0 {
        Vec::new()
    } else {
        // SAFETY: see above.
        unsafe { std::slice::from_raw_parts(value as *const u8, size) }.to_vec()
    };
    c.props.push(Property {
        key,
        kind,
        flags,
        data,
    });
    0
}

struct Restorer<'a> {
    urids: &'a Urids,
    props: &'a [Property],
}

unsafe extern "C" fn retrieve(
    handle: *mut c_void,
    key: u32,
    size: *mut usize,
    kind: *mut u32,
    flags: *mut u32,
) -> *const c_void {
    // SAFETY: `handle` is the restorer we passed to `restore`.
    let r = unsafe { &*(handle as *const Restorer) };
    let Some(key) = r.urids.unmap(key) else {
        return std::ptr::null();
    };
    let Some(p) = r.props.iter().find(|p| p.key == key) else {
        return std::ptr::null();
    };
    // SAFETY: out-pointers the plugin gave, when it gave them.
    unsafe {
        if !size.is_null() {
            *size = p.data.len();
        }
        if !kind.is_null() {
            *kind = r.urids.map(&p.kind);
        }
        if !flags.is_null() {
            *flags = p.flags;
        }
    }
    p.data.as_ptr() as *const c_void
}

impl Instance {
    /// The plugin's own state (what its `state:interface` saves).
    pub fn save_properties(&self) -> Result<Vec<Property>> {
        let Some(iface) = self.state else {
            return Ok(Vec::new());
        };
        let mut c = Collector {
            urids: &self.features.urids,
            props: Vec::new(),
        };
        // SAFETY: the plugin calls `store` with the collector during the call.
        let status = unsafe {
            ((*iface).save)(
                self.handle,
                store,
                &mut c as *mut Collector as *mut c_void,
                STATE_IS_POD | STATE_IS_PORTABLE,
                self.features.as_ptr(),
            )
        };
        if status != 0 {
            bail!("the plugin could not save its state (error {status})");
        }
        Ok(c.props)
    }

    pub fn restore_properties(&self, props: &[Property]) -> Result<()> {
        let Some(iface) = self.state else {
            return Ok(());
        };
        let r = Restorer {
            urids: &self.features.urids,
            props,
        };
        // SAFETY: the plugin calls `retrieve` with the restorer during the call.
        let status = unsafe {
            ((*iface).restore)(
                self.handle,
                retrieve,
                &r as *const Restorer as *mut c_void,
                STATE_IS_POD | STATE_IS_PORTABLE,
                self.features.as_ptr(),
            )
        };
        if status != 0 {
            bail!("the plugin could not restore its state (error {status})");
        }
        Ok(())
    }
}

const STATE_MAGIC: &[u8; 4] = b"IL2S";

/// Control values and properties, in one blob.
pub fn pack_state(controls: &[(u32, f32)], props: &[Property]) -> Vec<u8> {
    let mut out = STATE_MAGIC.to_vec();
    out.extend_from_slice(&(controls.len() as u32).to_le_bytes());
    for (port, v) in controls {
        out.extend_from_slice(&port.to_le_bytes());
        out.extend_from_slice(&v.to_le_bytes());
    }
    out.extend_from_slice(&(props.len() as u32).to_le_bytes());
    for p in props {
        for s in [p.key.as_bytes(), p.kind.as_bytes()] {
            out.extend_from_slice(&(s.len() as u32).to_le_bytes());
            out.extend_from_slice(s);
        }
        out.extend_from_slice(&p.flags.to_le_bytes());
        out.extend_from_slice(&(p.data.len() as u32).to_le_bytes());
        out.extend_from_slice(&p.data);
    }
    out
}

/// Control port values (port, value) and the plugin's properties.
pub type SavedState = (Vec<(u32, f32)>, Vec<Property>);

pub fn unpack_state(data: &[u8]) -> Result<SavedState> {
    struct Reader<'a>(&'a [u8]);
    impl<'a> Reader<'a> {
        fn take(&mut self, n: usize) -> Result<&'a [u8]> {
            if self.0.len() < n {
                bail!("truncated");
            }
            let (a, b) = self.0.split_at(n);
            self.0 = b;
            Ok(a)
        }
        fn u32(&mut self) -> Result<u32> {
            Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
        }
        fn string(&mut self) -> Result<String> {
            let n = self.u32()? as usize;
            Ok(String::from_utf8_lossy(self.take(n)?).into_owned())
        }
    }
    let mut r = Reader(data);
    if r.take(4).context("not a saved LV2 state")? != STATE_MAGIC {
        bail!("not a saved LV2 state");
    }
    let n = r.u32()? as usize;
    let mut controls = Vec::with_capacity(n.min(4096));
    for _ in 0..n {
        let port = r.u32()?;
        let v = f32::from_bits(r.u32()?);
        controls.push((port, v));
    }
    let n = r.u32()? as usize;
    let mut props = Vec::with_capacity(n.min(4096));
    for _ in 0..n {
        let key = r.string()?;
        let kind = r.string()?;
        let flags = r.u32()?;
        let len = r.u32()? as usize;
        props.push(Property {
            key,
            kind,
            flags,
            data: r.take(len)?.to_vec(),
        });
    }
    Ok((controls, props))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saved_state_round_trips_and_rejects_garbage() {
        let props = vec![
            Property {
                key: "urn:k".into(),
                kind: "urn:t".into(),
                flags: 3,
                data: vec![1, 2, 3],
            },
            Property {
                key: "urn:empty".into(),
                kind: "urn:t".into(),
                flags: 0,
                data: vec![],
            },
        ];
        let blob = pack_state(&[(3, 0.5), (4, -1.0)], &props);
        let (controls, back) = unpack_state(&blob).unwrap();
        assert_eq!(controls, vec![(3, 0.5), (4, -1.0)]);
        assert_eq!(back, props);
        assert!(unpack_state(b"nope").is_err());
        assert!(unpack_state(&blob[..blob.len() - 2]).is_err(), "truncated");
    }

    #[test]
    fn the_control_bank_stores_floats_per_port() {
        let b = Bank::new(3, |i| i as f32);
        assert_eq!(b.get(2), 2.0);
        b.set(1, 9.5);
        assert_eq!(b.get(1), 9.5);
        assert_eq!(b.get(10), 0.0, "no such port");
    }
}
