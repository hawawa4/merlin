//! Plugin scanning in the background. The app starts from the cache; this thread probes the
//! files that are new or changed, one child process each, and reports back as it goes.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc;
use std::time::Duration;

use merlin_scanner::{Cache, Entry, run_isolated, scan_ignoring};
use winit::event_loop::EventLoopProxy;

use crate::session::UserEvent;

/// A native plugin should load in well under this; the first Wine start can take much longer.
const TIMEOUT_NATIVE: Duration = Duration::from_secs(20);
const TIMEOUT_WINDOWS: Duration = Duration::from_secs(90);

pub enum ScanMsg {
    /// About to probe this file.
    Started(PathBuf),
    /// A file's result (also for files answered from the cache).
    Entry(Entry),
    Finished,
}

/// The scanner executable that sits next to the app.
pub fn scanner_exe() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(|d| d.join("merlin-scan")))
        .unwrap_or_else(|| PathBuf::from("merlin-scan"))
}

fn probe(
    exe: &Path,
    file: &Path,
    windows: bool,
) -> anyhow::Result<Vec<merlin_hosting::PluginInfo>> {
    let mut cmd = Command::new(exe);
    cmd.arg(file);
    if windows {
        cmd.arg("--windows");
    }
    run_isolated(
        cmd,
        if windows {
            TIMEOUT_WINDOWS
        } else {
            TIMEOUT_NATIVE
        },
    )
}

/// Starts a scan of `dirs`, leaving the `ignored` files alone. Files in `forget` are probed again even if the cache has them.
pub fn spawn(
    dirs: Vec<PathBuf>,
    ignored: Vec<PathBuf>,
    cache_file: PathBuf,
    forget: Vec<PathBuf>,
    proxy: EventLoopProxy<UserEvent>,
) -> mpsc::Receiver<ScanMsg> {
    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new()
        .name("plugin-scan".into())
        .spawn(move || {
            let exe = scanner_exe();
            let mut cache = Cache::load(&cache_file);
            for f in &forget {
                cache.forget(f);
            }
            let send = |m: ScanMsg| {
                let _ = tx.send(m);
                let _ = proxy.send_event(UserEvent::Scan);
            };
            scan_ignoring(
                &dirs,
                &ignored,
                &mut cache,
                &|file, windows| probe(&exe, file, windows),
                |file| send(ScanMsg::Started(file.to_owned())),
                |entry| send(ScanMsg::Entry(entry.clone())),
            );
            if let Err(e) = cache.save(&cache_file) {
                eprintln!("could not save the plugin cache: {e:#}");
            }
            send(ScanMsg::Finished);
        })
        .expect("spawn scan thread");
    rx
}
