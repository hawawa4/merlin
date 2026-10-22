//! Survives crashing Windows plugins (plan: crash marker, quarantine).
//!
//! A bridged plugin runs in its own `yabridge-host` process under Wine. When that process
//! dies (a Wine stack overflow, a plugin bug) the next call across the bridge either blocks
//! forever (the socket stays open) and freezes the app, or throws a C++ exception out of the
//! audio callback, which aborts a Rust process. Neither can be recovered from in place, so we
//! restart: the culprit goes on the quarantine list, the other children are killed, and the
//! new process starts from the last autosave without loading the culprit.
//!
//! Two detectors: a watchdog thread polling the registered host processes (the hang case), and
//! a hook in our patched yabridge that runs when a socket fails (the exception case).

use std::collections::{HashMap, HashSet};
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use merlin_scanner::{Quarantine, Quarantined};

use crate::paths;

/// Which plugin a host process belongs to.
#[derive(Clone, Debug)]
pub struct HostInfo {
    pub slot: usize,
    pub path: PathBuf,
    pub id: String,
    pub name: String,
}

pub struct Guard {
    hosts: Mutex<HashMap<u32, HostInfo>>,
    /// Plugins being removed on purpose right now: their sockets closing is expected.
    closing: AtomicUsize,
    /// Shutting down: nothing is a crash any more.
    disarmed: AtomicBool,
}

/// `(pid, parent pid, command name, state)` of every process.
fn processes() -> Vec<(u32, u32, String, char)> {
    let mut v = Vec::new();
    let Ok(rd) = std::fs::read_dir("/proc") else {
        return v;
    };
    for e in rd.flatten() {
        let Ok(pid) = e.file_name().to_string_lossy().parse::<u32>() else {
            continue;
        };
        let Ok(stat) = std::fs::read_to_string(e.path().join("stat")) else {
            continue;
        };
        let (Some(open), Some(close)) = (stat.find('('), stat.rfind(')')) else {
            continue;
        };
        let rest: Vec<&str> = stat[close + 2..].split(' ').collect();
        let (Some(state), Some(ppid)) = (
            rest.first().and_then(|s| s.chars().next()),
            rest.get(1).and_then(|s| s.parse().ok()),
        ) else {
            continue;
        };
        v.push((pid, ppid, stat[open + 1..close].to_string(), state));
    }
    v
}

/// Direct children of this process that are yabridge plugin hosts.
/// Clock ticks of CPU time (user + system, all threads) a process has used so far.
pub fn cpu_ticks(pid: u32) -> Option<u64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let rest: Vec<&str> = stat[stat.rfind(')')? + 2..].split(' ').collect();
    // After the command name: state is field 0, utime 11, stime 12.
    Some(rest.get(11)?.parse::<u64>().ok()? + rest.get(12)?.parse::<u64>().ok()?)
}

pub fn host_children() -> HashSet<u32> {
    let me = std::process::id();
    processes()
        .into_iter()
        .filter(|(_, ppid, comm, _)| *ppid == me && comm.starts_with("yabridge-host"))
        .map(|(pid, ..)| pid)
        .collect()
}

fn kill_children() {
    let me = std::process::id();
    for (pid, ppid, ..) in processes() {
        if ppid == me {
            // SAFETY: signalling our own child process.
            unsafe { libc::kill(pid as i32, libc::SIGKILL) };
        }
    }
}

/// True for a host that exited or was left a zombie.
fn is_dead(pid: u32, all: &[(u32, u32, String, char)]) -> bool {
    all.iter()
        .find(|p| p.0 == pid)
        .is_none_or(|p| matches!(p.3, 'Z' | 'X'))
}

impl Guard {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            hosts: Mutex::new(HashMap::new()),
            closing: AtomicUsize::new(0),
            disarmed: AtomicBool::new(false),
        })
    }

    pub fn register(&self, pid: u32, info: HostInfo) {
        self.hosts.lock().unwrap().insert(pid, info);
    }

    /// `(slot or insert id, pid)` of every running Wine host.
    pub fn host_pids(&self) -> Vec<(usize, u32)> {
        self.hosts
            .lock()
            .unwrap()
            .iter()
            .map(|(&pid, h)| (h.slot, pid))
            .collect()
    }

    /// Call before a plugin is removed on purpose, so its host exiting is not a crash.
    pub fn unregister_slot(&self, slot: usize) {
        self.hosts.lock().unwrap().retain(|_, h| h.slot != slot);
    }

    /// Brackets the deliberate removal of a plugin.
    pub fn begin_close(&self) {
        self.closing.fetch_add(1, Ordering::SeqCst);
    }

    pub fn end_close(&self) {
        self.closing.fetch_sub(1, Ordering::SeqCst);
    }

    /// The app is shutting down: hosts dying is no longer a crash.
    pub fn disarm(&self) {
        self.disarmed.store(true, Ordering::SeqCst);
    }

    /// Restarts the app if a registered host is dead. Returns if none is.
    fn recover_if_dead(&self) {
        if self.disarmed.load(Ordering::SeqCst) {
            return;
        }
        let dead = {
            let hosts = self.hosts.lock().unwrap();
            if hosts.is_empty() {
                return;
            }
            let all = processes();
            hosts
                .iter()
                .find(|(pid, _)| is_dead(**pid, &all))
                .map(|(_, info)| info.clone())
        };
        if let Some(info) = dead {
            recover(&info);
        }
    }

    /// Starts the watchdog thread.
    pub fn spawn_watchdog(self: &Arc<Self>) {
        let _ = GUARD.set(self.clone());
        let guard = self.clone();
        std::thread::Builder::new()
            .name("guard".into())
            .spawn(move || {
                loop {
                    std::thread::sleep(Duration::from_millis(250));
                    guard.recover_if_dead();
                }
            })
            .expect("spawn guard thread");
    }
}

/// Quarantines the plugin and restarts the app. Only returns if the restart failed.
fn recover(info: &HostInfo) {
    // The watchdog thread and the terminate hook can both get here; the first one restarts.
    static RECOVERING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if RECOVERING.swap(true, std::sync::atomic::Ordering::SeqCst) {
        std::thread::sleep(Duration::from_secs(30));
        return;
    }
    eprintln!(
        "{} (slot {}) crashed its Wine host: quarantining it and restarting from the autosave",
        info.name,
        info.slot + 1
    );
    let file = paths::quarantine_file();
    let mut q = Quarantine::load(&file);
    q.add(Quarantined {
        path: info.path.clone(),
        id: info.id.clone(),
        name: info.name.clone(),
        reason: "its Wine host process died while the app was running".into(),
        when: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs()),
    });
    if let Err(e) = q.save(&file) {
        eprintln!("cannot save the quarantine list: {e:#}");
    }
    // Other bridged plugins' hosts would otherwise outlive us (the PID stays the same after exec).
    kill_children();

    let err = restart_from_autosave(Some(&info.name));
    eprintln!("restart failed: {err}");
    std::process::exit(1);
}

/// Replaces this process with a fresh one that restores the autosave. Only returns (with the
/// error) if that failed. `recovered` names a plugin that was just quarantined.
pub fn restart_from_autosave(recovered: Option<&str>) -> std::io::Error {
    let exe = match std::env::current_exe() {
        Ok(e) => e,
        Err(e) => return e,
    };
    // The saved rig named on the command line is older than the autosave: drop it.
    let mut args: Vec<String> = Vec::new();
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--rig" => {
                it.next();
            }
            "--fresh" | "--recover" => {}
            _ => args.push(a),
        }
    }
    args.push("--recover".into());
    let mut cmd = std::process::Command::new(exe);
    cmd.args(args);
    if let Some(name) = recovered {
        cmd.env("MERLIN_RECOVERED", name);
    }
    cmd.exec()
}

static GUARD: OnceLock<Arc<Guard>> = OnceLock::new();

/// Called by our patched yabridge (`yabridge_socket_failed_hook`) on the thread where a socket
/// to a Wine host just failed, before the C++ exception propagates. That exception would
/// unwind through Rust frames (the JACK callback) and abort the process, so this must not
/// return when the cause is a dead host: it quarantines the plugin and restarts the app.
extern "C" fn on_socket_failed() {
    let Some(g) = GUARD.get() else { return };
    if g.disarmed.load(Ordering::SeqCst) || g.closing.load(Ordering::SeqCst) > 0 {
        // Shutdown or a deliberate unload: sockets closing is expected.
        return;
    }
    // The socket closes while the Wine host is still tearing down its threads, a few
    // milliseconds before the process is gone: give it a moment.
    for _ in 0..100 {
        g.recover_if_dead();
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Path of the loaded yabridge plugin library (`libyabridge-clap.so`), if any.
fn loaded_yabridge_library() -> Option<std::ffi::CString> {
    unsafe extern "C" fn visit(
        info: *mut libc::dl_phdr_info,
        _size: usize,
        data: *mut libc::c_void,
    ) -> libc::c_int {
        // SAFETY: dl_iterate_phdr hands us a valid info struct; `data` is our `Option<CString>`.
        unsafe {
            let name = std::ffi::CStr::from_ptr((*info).dlpi_name);
            if name.to_bytes().ends_with(b"libyabridge-clap.so") {
                *(data as *mut Option<std::ffi::CString>) = Some(name.to_owned());
                return 1;
            }
        }
        0
    }
    let mut found: Option<std::ffi::CString> = None;
    // SAFETY: the callback only writes through `data`, which points at `found`.
    unsafe { libc::dl_iterate_phdr(Some(visit), &mut found as *mut _ as *mut libc::c_void) };
    found
}

/// Registers [`on_socket_failed`] with the yabridge library, once it is loaded. Call after a
/// bridged plugin was created; harmless to repeat. (yabridge's chainloader loads the library
/// with local visibility, so the symbol is looked up through the library's own handle.)
pub fn install_socket_hook() {
    let Some(path) = loaded_yabridge_library() else {
        return;
    };
    // SAFETY: RTLD_NOLOAD only returns a handle for a library that is already loaded; the
    // symbol is a `void (*)(void)` variable exported by our patched yabridge, and we only write
    // a function pointer of that type into it.
    unsafe {
        let lib = libc::dlopen(path.as_ptr(), libc::RTLD_NOW | libc::RTLD_NOLOAD);
        if lib.is_null() {
            return;
        }
        let sym = libc::dlsym(lib, c"yabridge_socket_failed_hook".as_ptr());
        if sym.is_null() {
            eprintln!("this yabridge build has no crash hook (run `just yabridge-build`)");
            return;
        }
        *(sym as *mut Option<extern "C" fn()>) = Some(on_socket_failed);
    }
}
