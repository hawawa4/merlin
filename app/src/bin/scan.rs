//! `merlin-scan <plugin file> [--windows]`: loads one plugin file and prints its plugins
//! as a tagged JSON line. Runs in its own process so that a crashing or hanging plugin cannot
//! take the app down; the app kills it after a timeout.

use std::io::Write;

use merlin_scanner::{RESULT_PREFIX, load_any};

fn main() {
    let mut args = std::env::args().skip(1);
    let Some(path) = args.next() else {
        eprintln!("usage: merlin-scan <plugin file> [--windows]");
        std::process::exit(64);
    };
    let windows = args.next().is_some_and(|a| a == "--windows");
    let code = match load_any(std::path::Path::new(&path), windows) {
        Ok(plugins) => {
            // A newline first: a plugin may have left a line unfinished.
            let json = serde_json::to_string(&plugins).unwrap_or_else(|_| "[]".into());
            let mut out = std::io::stdout().lock();
            let _ = writeln!(out, "\n{RESULT_PREFIX}{json}");
            let _ = out.flush();
            0
        }
        Err(e) => {
            eprintln!("{e:#}");
            2
        }
    };
    // Leave without running the plugin's static destructors or waiting for its threads: they
    // are a common place for hangs and crashes, and the result is already out.
    // SAFETY: _exit never returns.
    unsafe { libc::_exit(code) }
}
