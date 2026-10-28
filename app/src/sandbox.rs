//! Plugins run in processes of their own (see `merlin_sandbox`), so one that crashes or hangs
//! cannot take the app down.

use std::path::PathBuf;

/// The plugin host executable that sits next to the app (`MERLIN_PLUGIN_HOST` overrides it).
pub fn plugin_host_exe() -> PathBuf {
    if let Some(p) = std::env::var_os("MERLIN_PLUGIN_HOST").filter(|p| !p.is_empty()) {
        return p.into();
    }
    std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(|d| d.join("merlin-plugin-host")))
        .unwrap_or_else(|| PathBuf::from("merlin-plugin-host"))
}

/// Plugins load in-process only when this is set (for debugging a plugin with a debugger, or
/// to compare): a crash then takes the whole app down, as it used to.
pub fn disabled() -> bool {
    std::env::var_os("MERLIN_NO_SANDBOX").is_some_and(|v| !v.is_empty() && v != "0")
}
