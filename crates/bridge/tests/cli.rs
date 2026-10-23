//! Runs the real binary against a fake home folder, with a fake runtime and fake Windows plugins.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn pe() -> Vec<u8> {
    let mut v = vec![0u8; 0x80];
    v[..2].copy_from_slice(b"MZ");
    v[60..64].copy_from_slice(&0x40u32.to_le_bytes());
    v[0x40..0x44].copy_from_slice(b"PE\0\0");
    v[0x44..0x46].copy_from_slice(&0x8664u16.to_le_bytes());
    v
}

struct Home(PathBuf);

impl Home {
    fn new(name: &str) -> Self {
        let d = std::env::temp_dir().join(format!("merlin-bridge-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        Home(d)
    }

    fn data(&self) -> PathBuf {
        self.0.join(".local/share/merlin")
    }

    /// A runtime laid out like the one the app downloads, with programs that do nothing.
    fn install_runtime(&self, id: &str) {
        let root = self.data().join("runtime");
        let d = root.join(id);
        std::fs::create_dir_all(d.join("wine/bin")).unwrap();
        std::fs::create_dir_all(d.join("yabridge")).unwrap();
        let wine = d.join("wine/bin/wine");
        std::fs::write(&wine, "#!/bin/sh\necho wine-test\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&wine, std::fs::Permissions::from_mode(0o755)).unwrap();
        for f in ["clap", "vst3", "vst2"] {
            std::fs::write(
                d.join(format!("yabridge/libyabridge-chainloader-{f}.so")),
                "ELF",
            )
            .unwrap();
        }
        std::fs::write(root.join("CURRENT"), id).unwrap();
    }

    /// A Wine prefix with one Windows CLAP in it.
    fn prefix_with_clap(&self, name: &str) {
        let claps = self
            .data()
            .join("wineprefix/drive_c/Program Files/Common Files/CLAP");
        std::fs::create_dir_all(&claps).unwrap();
        std::fs::create_dir_all(self.data().join("wineprefix/dosdevices")).unwrap();
        std::fs::write(claps.join(format!("{name}.clap")), pe()).unwrap();
    }

    fn folder_with(&self, folder: &str, files: &[(&str, Vec<u8>)]) -> PathBuf {
        let d = self.0.join(folder);
        for (name, bytes) in files {
            let p = d.join(name);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, bytes).unwrap();
        }
        d
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_merlin-bridge"))
            .args(args)
            .env_clear()
            .env("HOME", &self.0)
            .env("PATH", "/usr/bin:/bin")
            .current_dir(&self.0)
            .output()
            .unwrap()
    }

    fn ok(&self, args: &[&str]) -> String {
        let out = self.run(args);
        let text = String::from_utf8_lossy(&out.stdout).into_owned();
        assert!(
            out.status.success(),
            "{args:?} failed: {}\n{text}",
            String::from_utf8_lossy(&out.stderr)
        );
        text
    }

    fn clap_dir(&self) -> PathBuf {
        self.0.join(".clap/merlin-bridges")
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn link(p: &Path) -> PathBuf {
    std::fs::read_link(p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

#[test]
fn without_a_runtime_it_says_what_to_do() {
    let h = Home::new("noruntime");
    let out = h.run(&["sync"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("runtime install"));
}

#[test]
fn the_prefix_is_bridged_with_links_through_the_stable_runtime_path() {
    let h = Home::new("prefix");
    h.install_runtime("wine-1_y");
    h.prefix_with_clap("Synth");
    let text = h.ok(&["sync"]);
    assert!(text.contains("1 bridge(s) ready (1 new"), "{text}");

    let bridge = h.clap_dir().join("wine-prefix/Synth.clap");
    let runtime = h.data().join("runtime");
    assert_eq!(
        link(&bridge),
        runtime.join("current/yabridge/libyabridge-chainloader-clap.so")
    );
    assert!(h.clap_dir().join("wine-prefix/Synth.clap-win").exists());
    // `current` reaches the real runtime, and plugins outside every prefix get the prefix.
    assert_eq!(link(&runtime.join("current")), Path::new("wine-1_y"));
    assert_eq!(
        std::fs::read_to_string(runtime.join("wine-1_y/yabridge/default-wineprefix")).unwrap(),
        format!("{}\n", h.data().join("wineprefix").display())
    );

    // Again: nothing to do. And the plugin's own file is untouched.
    let again = h.ok(&["sync"]);
    assert!(again.contains("1 bridge(s) ready (0 new"), "{again}");
    assert_eq!(
        std::fs::read(
            h.data()
                .join("wineprefix/drive_c/Program Files/Common Files/CLAP/Synth.clap")
        )
        .unwrap(),
        pe()
    );
}

#[test]
fn a_folder_is_remembered_and_forgotten_and_a_linux_plugin_in_it_is_left_alone() {
    let h = Home::new("folder");
    h.install_runtime("wine-1_y");
    let folder = h.folder_with(
        "Plugins/Win",
        &[
            ("Lead.clap", pe()),
            ("Native.clap", b"\x7fELF native".to_vec()),
            ("Pad.vst3", pe()),
        ],
    );
    let text = h.ok(&["sync", "Plugins/Win"]);
    assert!(text.contains("2 bridge(s) ready"), "{text}");
    let tag = std::fs::read_dir(h.clap_dir())
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .file_name();
    let tag = tag.to_string_lossy().into_owned();
    assert!(tag.starts_with("Win-"), "{tag}");
    assert!(h.clap_dir().join(&tag).join("Lead.clap").is_symlink());
    assert!(!h.clap_dir().join(&tag).join("Native.clap").exists());
    assert!(
        h.0.join(".vst3/merlin-bridges")
            .join(&tag)
            .join("Pad.vst3/Contents/x86_64-linux/Pad.so")
            .is_symlink()
    );

    // A new plugin shows up on the next plain `sync`, which remembers the folder.
    std::fs::write(folder.join("Bass.clap"), pe()).unwrap();
    let again = h.ok(&["sync"]);
    assert!(again.contains("3 bridge(s) ready (1 new"), "{again}");
    assert!(h.ok(&["status"]).contains(&folder.display().to_string()));

    // Forgetting removes its bridges, and only those.
    let forgot = h.ok(&["forget", "Plugins/Win"]);
    assert!(forgot.contains("Removed the bridges of Win-"), "{forgot}");
    assert!(!h.clap_dir().join(&tag).exists());
    assert!(
        folder.join("Lead.clap").exists(),
        "the plugins are never touched"
    );
}

#[test]
fn a_folder_that_is_gone_keeps_its_bridges() {
    let h = Home::new("gone");
    h.install_runtime("wine-1_y");
    let folder = h.folder_with("Disk/Plugins", &[("Lead.clap", pe())]);
    h.ok(&["sync", "Disk/Plugins"]);
    std::fs::remove_dir_all(h.0.join("Disk")).unwrap();
    let text = h.ok(&["sync"]);
    assert!(text.contains("is not there"), "{text}");
    let tag = std::fs::read_dir(h.clap_dir())
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    assert!(tag.join("Lead.clap").is_symlink(), "{}", folder.display());
}

#[test]
fn a_runtime_update_keeps_working_bridges_and_clean_removes_only_ours() {
    let h = Home::new("update");
    h.install_runtime("wine-1_y");
    h.prefix_with_clap("Synth");
    h.ok(&["sync"]);
    // The app (or `runtime install`) switches to a newer runtime and deletes the old one.
    h.install_runtime("wine-2_y");
    std::fs::remove_dir_all(h.data().join("runtime/wine-1_y")).unwrap();
    let bridge = h.clap_dir().join("wine-prefix/Synth.clap");
    // `current` still names the deleted runtime; a sync repoints it and nothing else changes.
    let text = h.ok(&["sync"]);
    assert!(text.contains("1 bridge(s) ready (0 new"), "{text}");
    assert!(
        std::fs::metadata(&bridge).is_ok(),
        "the bridge reaches the new runtime"
    );

    std::fs::write(h.0.join(".clap/Mine.clap"), b"a native plugin").unwrap();
    h.ok(&["clean"]);
    assert!(!h.clap_dir().exists());
    assert!(h.0.join(".clap/Mine.clap").exists());
}

#[test]
fn yabridgectl_duplicates_are_pointed_out() {
    let h = Home::new("dupes");
    h.install_runtime("wine-1_y");
    h.prefix_with_clap("Synth");
    h.prefix_with_clap("Other");
    h.folder_with(".clap/yabridge", &[("Synth.clap", b"copy".to_vec())]);
    h.ok(&["sync"]);
    let text = h.ok(&["status"]);
    assert!(
        text.contains("WARNING: 1 plugin(s) are also bridged by yabridgectl"),
        "{text}"
    );
    assert!(text.contains("synth"), "{text}");
}
