//! Windows plugins: the Wine prefix, installers, and yabridge bridges (plan §4.8).
//!
//! The app never asks the user to run `yabridgectl` or `winecfg`. This crate does the same
//! work directly: it prepares a prefix, runs a plugin's installer in it, and then makes a
//! *bridge* for every Windows CLAP plugin it finds there: yabridge's chainloader copied to a
//! `.clap` file named like the plugin, plus a `.clap-win` link to the real Windows file.
//! The scanner and the host already understand that pair.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};

/// Folders (relative to a prefix's `drive_c`) where Windows installers put CLAP plugins.
const CLAP_DIRS: &[&str] = &["Program Files/Common Files/CLAP"];
/// Per-user install locations, below `drive_c/users/<name>/`.
const USER_CLAP_DIRS: &[&str] = &["AppData/Local/Programs/Common/CLAP"];
/// How deep below a plugin folder `.clap` files are looked for (vendors use subfolders).
const MAX_DEPTH: usize = 3;
/// Suffix of the link that names the real Windows plugin beside a bridge.
const WIN_LINK: &str = "clap-win";

/// A Wine plus a yabridge, and the prefix our Windows plugins live in.
#[derive(Clone, Debug)]
pub struct Runtime {
    /// The `wine` program (a patched build, or the system's).
    pub wine: PathBuf,
    /// yabridge's build folder: chainloaders and `yabridge-host.exe`.
    pub yabridge_dir: PathBuf,
    pub prefix: PathBuf,
}

impl Runtime {
    /// A command that runs `program` under Wine with our prefix and yabridge on the path.
    pub fn command(&self, program: impl Into<OsString>) -> Command {
        let mut cmd = Command::new(&self.wine);
        cmd.arg(program.into())
            .env("WINEPREFIX", &self.prefix)
            .env("WINELOADER", &self.wine)
            // Wine's own chatter is noise here, and the Mono/Gecko installers are a dialog the
            // user never asked for.
            .env("WINEDEBUG", "-all")
            .env("WINEDLLOVERRIDES", "mscoree,mshtml=d");
        let path = std::env::var_os("PATH").unwrap_or_default();
        let mut dirs = vec![self.yabridge_dir.clone()];
        dirs.extend(std::env::split_paths(&path));
        if let Ok(joined) = std::env::join_paths(dirs) {
            cmd.env("PATH", joined);
        }
        cmd
    }

    /// `wine --version`, e.g. `wine-11.20 (Staging)`.
    pub fn version(&self) -> Option<String> {
        let out = Command::new(&self.wine).arg("--version").output().ok()?;
        let text = String::from_utf8_lossy(&out.stdout).trim().to_owned();
        (out.status.success() && !text.is_empty()).then_some(text)
    }

    /// Has the prefix been created (`wineboot` has run in it)?
    pub fn prefix_ready(&self) -> bool {
        self.prefix.join("system.reg").exists() && self.prefix.join("drive_c").is_dir()
    }

    pub fn chainloader(&self) -> PathBuf {
        self.yabridge_dir.join("libyabridge-chainloader-clap.so")
    }

    /// Creates the prefix if it does not exist yet. Takes tens of seconds the first time.
    pub fn ensure_prefix(&self, timeout: Duration) -> Result<()> {
        if self.prefix_ready() {
            return Ok(());
        }
        std::fs::create_dir_all(&self.prefix)
            .with_context(|| format!("cannot create {}", self.prefix.display()))?;
        let mut cmd = self.command("wineboot");
        cmd.arg("-u");
        let start = Instant::now();
        run_with_timeout(cmd, timeout).context("creating the Wine prefix")?;
        // wineboot can return before the registry is written: wineserver saves it when it
        // shuts down, so wait for that (it lingers a few seconds after the last client).
        if let Some(server) = self
            .wine
            .parent()
            .map(|d| d.join("wineserver"))
            .filter(|p| p.is_file())
        {
            let mut wait = Command::new(server);
            wait.arg("-w")
                .env("WINEPREFIX", &self.prefix)
                .env("WINEDEBUG", "-all");
            let left = timeout
                .saturating_sub(start.elapsed())
                .max(Duration::from_secs(10));
            run_with_timeout(wait, left).context("waiting for Wine to settle")?;
        }
        if !self.prefix_ready() {
            bail!(
                "wineboot finished but {} is not a prefix",
                self.prefix.display()
            );
        }
        Ok(())
    }

    /// The command that runs a plugin installer (`.exe`, or `.msi` through `msiexec`).
    pub fn installer_command(&self, file: &Path) -> Result<Command> {
        if !file.is_file() {
            bail!("{} is not a file", file.display());
        }
        let ext = file
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase);
        match ext.as_deref() {
            Some("exe") => {
                let mut cmd = self.command(file);
                // Installers look for their own files beside themselves.
                if let Some(dir) = file.parent() {
                    cmd.current_dir(dir);
                }
                Ok(cmd)
            }
            Some("msi") => {
                let mut cmd = self.command("msiexec");
                cmd.arg("/i").arg(file);
                Ok(cmd)
            }
            _ => bail!(
                "{} is not a Windows installer (.exe or .msi)",
                file.display()
            ),
        }
    }

    /// Where Windows CLAP plugins land in this prefix.
    pub fn clap_dirs(&self) -> Vec<PathBuf> {
        let c = self.prefix.join("drive_c");
        let mut dirs: Vec<PathBuf> = CLAP_DIRS.iter().map(|d| c.join(d)).collect();
        if let Ok(users) = std::fs::read_dir(c.join("users")) {
            for u in users.flatten() {
                dirs.extend(USER_CLAP_DIRS.iter().map(|d| u.path().join(d)));
            }
        }
        dirs.retain(|d| d.is_dir());
        dirs
    }

    /// Makes (and tidies) the bridges for this prefix's Windows CLAP plugins in `out_dir`.
    pub fn sync_clap_bridges(&self, out_dir: &Path) -> Result<SyncReport> {
        sync_clap_bridges(&self.clap_dirs(), &self.chainloader(), out_dir)
    }
}

/// Runs `cmd` to the end, or kills it after `timeout`.
fn run_with_timeout(mut cmd: Command, timeout: Duration) -> Result<()> {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut child = cmd.spawn().context("cannot start Wine")?;
    let start = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            return if status.success() {
                Ok(())
            } else {
                Err(anyhow!("exited with {status}"))
            };
        }
        if start.elapsed() > timeout {
            let _ = child.kill();
            let _ = child.wait();
            bail!("timed out after {}s", timeout.as_secs());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// What a sync did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct SyncReport {
    /// Bridges made or repaired.
    pub created: Vec<PathBuf>,
    /// Bridges that were already right.
    pub unchanged: usize,
    /// Bridges removed because their Windows plugin is gone.
    pub removed: Vec<PathBuf>,
}

fn find_windows_claps(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut paths: Vec<PathBuf> = rd.flatten().map(|e| e.path()).collect();
    paths.sort();
    for p in paths {
        if p.extension().is_some_and(|e| e == "clap") {
            out.push(p);
        } else if p.is_dir() && depth < MAX_DEPTH {
            find_windows_claps(&p, depth + 1, out);
        }
    }
}

/// For every Windows `.clap` below `plugin_dirs`, makes `out_dir/<same relative path>` (a copy of
/// the chainloader) and `<that>.clap-win` (a link to the Windows file). Bridges whose Windows
/// plugin has disappeared are removed; files that are not our bridges are never touched.
pub fn sync_clap_bridges(
    plugin_dirs: &[PathBuf],
    chainloader: &Path,
    out_dir: &Path,
) -> Result<SyncReport> {
    if !chainloader.is_file() {
        bail!(
            "yabridge's CLAP chainloader is missing: {}",
            chainloader.display()
        );
    }
    let wanted_bytes = std::fs::read(chainloader)?;
    let mut report = SyncReport::default();
    let mut wanted: Vec<PathBuf> = Vec::new();

    for dir in plugin_dirs {
        let mut files = Vec::new();
        find_windows_claps(dir, 0, &mut files);
        for real in files {
            let rel = real.strip_prefix(dir).unwrap_or(&real);
            let bridge = out_dir.join(rel);
            let link = bridge.with_extension(WIN_LINK);
            wanted.push(bridge.clone());
            if let Some(parent) = bridge.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let same_bridge = std::fs::read(&bridge).is_ok_and(|b| b == wanted_bytes);
            let same_link = std::fs::read_link(&link).is_ok_and(|t| t == real);
            if same_bridge && same_link {
                report.unchanged += 1;
                continue;
            }
            // Write beside and rename, so a running app never sees half a chainloader.
            let tmp = bridge.with_extension("tmp");
            std::fs::write(&tmp, &wanted_bytes)?;
            set_executable(&tmp)?;
            std::fs::rename(&tmp, &bridge)?;
            let _ = std::fs::remove_file(&link);
            std::os::unix::fs::symlink(&real, &link)
                .with_context(|| format!("cannot link {}", link.display()))?;
            report.created.push(bridge);
        }
    }

    // Remove bridges whose Windows plugin is gone: a `.clap-win` link in our folder that points
    // nowhere, with its chainloader copy.
    let mut stale = Vec::new();
    collect_links(out_dir, 0, &mut stale);
    for link in stale {
        let bridge = link.with_extension("clap");
        if wanted.contains(&bridge) || link.exists() {
            continue;
        }
        let _ = std::fs::remove_file(&link);
        let _ = std::fs::remove_file(&bridge);
        report.removed.push(bridge);
    }
    Ok(report)
}

fn collect_links(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        let is_link = e.file_type().is_ok_and(|t| t.is_symlink());
        if is_link && p.extension().is_some_and(|x| x == WIN_LINK) {
            out.push(p);
        } else if !is_link && p.is_dir() && depth < MAX_DEPTH {
            collect_links(&p, depth + 1, out);
        }
    }
}

fn set_executable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tempdir(name: &str) -> PathBuf {
        let d =
            std::env::temp_dir().join(format!("merlin-wine-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// A fake prefix with two Windows plugins (one in a vendor folder) and a fake chainloader.
    fn fixture(name: &str) -> (Runtime, PathBuf) {
        let root = tempdir(name);
        let prefix = root.join("prefix");
        let claps = prefix.join("drive_c/Program Files/Common Files/CLAP");
        std::fs::create_dir_all(claps.join("u-he")).unwrap();
        std::fs::write(claps.join("Six Sines.clap"), b"MZ six").unwrap();
        std::fs::write(claps.join("u-he/Diva.clap"), b"MZ diva").unwrap();
        std::fs::write(claps.join("readme.txt"), b"x").unwrap();
        let yab = root.join("yabridge");
        std::fs::create_dir_all(&yab).unwrap();
        std::fs::write(
            yab.join("libyabridge-chainloader-clap.so"),
            b"ELF chainloader",
        )
        .unwrap();
        let rt = Runtime {
            wine: root.join("wine"),
            yabridge_dir: yab,
            prefix,
        };
        (rt, root.join("bridges"))
    }

    #[test]
    fn every_windows_clap_gets_a_bridge_and_a_link() {
        let (rt, out) = fixture("sync");
        let report = rt.sync_clap_bridges(&out).unwrap();
        assert_eq!(report.created.len(), 2);
        assert_eq!(report.unchanged, 0);
        for name in ["Six Sines", "u-he/Diva"] {
            let bridge = out.join(format!("{name}.clap"));
            assert_eq!(std::fs::read(&bridge).unwrap(), b"ELF chainloader");
            let target = std::fs::read_link(out.join(format!("{name}.clap-win"))).unwrap();
            assert!(
                target.starts_with(rt.prefix.join("drive_c")) && target.exists(),
                "{target:?}"
            );
        }
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(out.join("Six Sines.clap"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o111, 0o111, "the chainloader must be executable");
    }

    #[test]
    fn a_second_sync_changes_nothing_and_a_new_chainloader_is_picked_up() {
        let (rt, out) = fixture("again");
        rt.sync_clap_bridges(&out).unwrap();
        let again = rt.sync_clap_bridges(&out).unwrap();
        assert_eq!((again.created.len(), again.unchanged), (0, 2));

        std::fs::write(rt.chainloader(), b"ELF newer").unwrap();
        let upgraded = rt.sync_clap_bridges(&out).unwrap();
        assert_eq!(
            upgraded.created.len(),
            2,
            "bridges follow the yabridge build"
        );
        assert_eq!(
            std::fs::read(out.join("Six Sines.clap")).unwrap(),
            b"ELF newer"
        );
    }

    #[test]
    fn uninstalled_plugins_lose_their_bridge_but_foreign_files_stay() {
        let (rt, out) = fixture("stale");
        rt.sync_clap_bridges(&out).unwrap();
        // Something of the user's, not ours.
        std::fs::write(out.join("Mine.clap"), b"native plugin").unwrap();
        std::fs::remove_file(
            rt.prefix
                .join("drive_c/Program Files/Common Files/CLAP/u-he/Diva.clap"),
        )
        .unwrap();
        let report = rt.sync_clap_bridges(&out).unwrap();
        assert_eq!(report.removed, vec![out.join("u-he/Diva.clap")]);
        assert!(!out.join("u-he/Diva.clap").exists() && !out.join("u-he/Diva.clap-win").exists());
        assert!(out.join("Six Sines.clap").exists());
        assert!(
            out.join("Mine.clap").exists(),
            "files that are not bridges are left alone"
        );
    }

    #[test]
    fn a_missing_chainloader_is_an_error_not_empty_bridges() {
        let (rt, out) = fixture("nochain");
        std::fs::remove_file(rt.chainloader()).unwrap();
        let err = rt.sync_clap_bridges(&out).unwrap_err().to_string();
        assert!(err.contains("chainloader"), "{err}");
        assert!(!out.exists() || std::fs::read_dir(&out).unwrap().next().is_none());
    }

    #[test]
    fn user_install_folders_are_found_too() {
        let (rt, out) = fixture("users");
        let user = rt
            .prefix
            .join("drive_c/users/hawa/AppData/Local/Programs/Common/CLAP");
        std::fs::create_dir_all(&user).unwrap();
        std::fs::write(user.join("Odin.clap"), b"MZ").unwrap();
        assert_eq!(rt.clap_dirs().len(), 2);
        assert_eq!(rt.sync_clap_bridges(&out).unwrap().created.len(), 3);
    }

    #[test]
    fn installers_are_run_the_right_way() {
        let (rt, root) = fixture("inst");
        let exe = root.join("Setup.exe");
        let msi = root.join("Plugin.MSI");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(&exe, b"MZ").unwrap();
        std::fs::write(&msi, b"x").unwrap();
        let args = |c: &Command| {
            c.get_args()
                .map(|a| a.to_string_lossy().into_owned())
                .collect::<Vec<_>>()
        };
        let c = rt.installer_command(&exe).unwrap();
        assert_eq!(args(&c), vec![exe.to_string_lossy().into_owned()]);
        let c = rt.installer_command(&msi).unwrap();
        assert_eq!(args(&c)[..2], ["msiexec", "/i"]);
        assert!(rt.installer_command(&root.join("notes.txt")).is_err());
        let txt = root.join("notes.txt");
        std::fs::write(&txt, b"x").unwrap();
        assert!(
            rt.installer_command(&txt)
                .unwrap_err()
                .to_string()
                .contains("not a Windows installer")
        );
        let env: Vec<_> = rt
            .installer_command(&exe)
            .unwrap()
            .get_envs()
            .map(|(k, _)| k.to_string_lossy().into_owned())
            .collect();
        assert!(env.contains(&"WINEPREFIX".to_string()) && env.contains(&"PATH".to_string()));
    }

    #[test]
    fn the_prefix_is_ready_only_when_wineboot_has_run() {
        let (rt, _) = fixture("ready");
        assert!(!rt.prefix_ready());
        std::fs::write(rt.prefix.join("system.reg"), b"WINE REGISTRY").unwrap();
        assert!(rt.prefix_ready());
        assert!(
            rt.ensure_prefix(Duration::from_secs(1)).is_ok(),
            "an existing prefix is left alone"
        );
    }
}
