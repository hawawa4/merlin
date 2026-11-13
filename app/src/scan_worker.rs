//! Plugin scanning in the background. The app starts from the cache; this thread probes the
//! files that are new or changed, one child process each, and reports back as it goes.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc;
use std::time::Duration;

use merlin_scanner::{Cache, Entry, run_isolated, scan_ignoring};
use winit::event_loop::EventLoopProxy;

use crate::sandbox::BridgeEnv;
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
    env: &BridgeEnv,
) -> anyhow::Result<Vec<merlin_hosting::PluginInfo>> {
    let mut cmd = Command::new(exe);
    cmd.arg(file)
        .envs(env.for_file(file).iter().map(|(k, v)| (k, v)));
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
/// The app's own bridges are probed with `env`.
pub fn spawn(
    dirs: Vec<PathBuf>,
    ignored: Vec<PathBuf>,
    cache_file: PathBuf,
    forget: Vec<PathBuf>,
    env: BridgeEnv,
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
            // The main thread is woken for news it should show: a file about to be probed, a
            // probed file's result, the end. Files answered from the cache (nearly all of them,
            // usually) ride along with the next wake-up instead of waking it one by one.
            let wake = || {
                let _ = proxy.send_event(UserEvent::Scan);
            };
            let probing = std::cell::Cell::new(false);
            scan_ignoring(
                &dirs,
                &ignored,
                &mut cache,
                &|file, windows| probe(&exe, file, windows, &env),
                |file| {
                    probing.set(true);
                    let _ = tx.send(ScanMsg::Started(file.to_owned()));
                    wake();
                },
                |entry| {
                    let _ = tx.send(ScanMsg::Entry(entry.clone()));
                    if probing.replace(false) {
                        wake();
                    }
                },
            );
            if let Err(e) = cache.save(&cache_file) {
                eprintln!("could not save the plugin cache: {e:#}");
            }
            let _ = tx.send(ScanMsg::Finished);
            wake();
        })
        .expect("spawn scan thread");
    rx
}
