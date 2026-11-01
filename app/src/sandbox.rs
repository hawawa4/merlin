//! Plugins run in processes of their own (see `merlin_sandbox`), so one that crashes or hangs
//! cannot take the app down.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

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

/// What the plugin hosts and the scanner get in their environment for the app's own bridges
/// (Wine, yabridge, DLL overrides: [`merlin_wine::Runtime::bridge_env`]). Every other plugin file,
/// including the bridges other tools made (Cabinet, `yabridgectl`), gets nothing added, so it runs
/// with its own tool's Wine, as it would in any other plugin host.
#[derive(Clone, Debug, Default)]
pub struct BridgeEnv {
    /// The folder the app's bridges are in.
    pub root: PathBuf,
    pub vars: Vec<(OsString, OsString)>,
}

impl BridgeEnv {
    /// The variables to add for loading `path`.
    pub fn for_file(&self, path: &Path) -> &[(OsString, OsString)] {
        if path.starts_with(&self.root) {
            &self.vars
        } else {
            &[]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_apps_own_bridges_get_its_wine() {
        let env = BridgeEnv {
            root: "/home/u/.local/share/merlin/bridges".into(),
            vars: vec![("WINELOADER".into(), "/runtime/wine/bin/wine".into())],
        };
        let ours = Path::new("/home/u/.local/share/merlin/bridges/vst3/Surge XT.vst3");
        assert_eq!(env.for_file(ours).len(), 1);
        for theirs in [
            "/home/u/.vst3/cabinet/windows/Kontakt 8.vst3",
            "/home/u/.vst3/yabridge/Serum.vst3",
            "/home/u/.local/share/merlin/bridges-old/x.clap",
        ] {
            assert!(env.for_file(Path::new(theirs)).is_empty(), "{theirs}");
        }
    }
}
