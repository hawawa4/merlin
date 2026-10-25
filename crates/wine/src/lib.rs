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

pub mod layout;
pub mod runtime;

use anyhow::{Context, Result, anyhow, bail};

/// Folders (relative to a prefix's `drive_c`) where Windows installers put CLAP plugins.
const CLAP_DIRS: &[&str] = &["Program Files/Common Files/CLAP"];
/// Per-user install locations, below `drive_c/users/<name>/`.
const USER_CLAP_DIRS: &[&str] = &["AppData/Local/Programs/Common/CLAP"];
/// The same for VST3 plugins.
const VST3_DIRS: &[&str] = &["Program Files/Common Files/VST3"];
const USER_VST3_DIRS: &[&str] = &["AppData/Local/Programs/Common/VST3"];
/// Where Windows installers put VST2 plugins (`.dll` files).
const VST2_DIRS: &[&str] = &[
    "Program Files/VstPlugins",
    "Program Files/Steinberg/VstPlugins",
    "Program Files/Common Files/VST2",
    "Program Files/Common Files/Steinberg/VST2",
];
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
        self.plugin_dirs(CLAP_DIRS, USER_CLAP_DIRS)
    }

    /// Where Windows VST3 plugins land in this prefix.
    pub fn vst3_dirs(&self) -> Vec<PathBuf> {
        self.plugin_dirs(VST3_DIRS, USER_VST3_DIRS)
    }

    fn plugin_dirs(&self, system: &[&str], per_user: &[&str]) -> Vec<PathBuf> {
        let c = self.prefix.join("drive_c");
        let mut dirs: Vec<PathBuf> = system.iter().map(|d| c.join(d)).collect();
        if let Ok(users) = std::fs::read_dir(c.join("users")) {
            for u in users.flatten() {
                dirs.extend(per_user.iter().map(|d| u.path().join(d)));
            }
        }
        dirs.retain(|d| d.is_dir());
        dirs
    }

    /// Where Windows VST2 plugins land in this prefix.
    pub fn vst2_dirs(&self) -> Vec<PathBuf> {
        self.plugin_dirs(VST2_DIRS, &[])
    }

    pub fn vst2_chainloader(&self) -> PathBuf {
        self.yabridge_dir.join("libyabridge-chainloader-vst2.so")
    }

    /// Makes (and tidies) the bridges for this prefix's Windows VST2 plugins, and those in
    /// `extra_dirs` (folders the user pointed at), in `out_dir`.
    pub fn sync_vst2_bridges(&self, extra_dirs: &[PathBuf], out_dir: &Path) -> Result<SyncReport> {
        self.sync_vst2_bridges_with(extra_dirs, out_dir, Placement::Copy)
    }

    pub fn sync_vst2_bridges_with(
        &self,
        extra_dirs: &[PathBuf],
        out_dir: &Path,
        placement: Placement,
    ) -> Result<SyncReport> {
        let mut dirs = self.vst2_dirs();
        dirs.extend(extra_dirs.iter().filter(|d| d.is_dir()).cloned());
        sync_vst2_bridges_with(&dirs, &self.vst2_chainloader(), out_dir, placement)
    }

    pub fn vst3_chainloader(&self) -> PathBuf {
        self.yabridge_dir.join("libyabridge-chainloader-vst3.so")
    }

    /// Makes (and tidies) the bridges for this prefix's Windows VST3 plugins in `out_dir`.
    pub fn sync_vst3_bridges(&self, out_dir: &Path) -> Result<SyncReport> {
        self.sync_vst3_bridges_with(out_dir, Placement::Copy)
    }

    pub fn sync_vst3_bridges_with(
        &self,
        out_dir: &Path,
        placement: Placement,
    ) -> Result<SyncReport> {
        self.sync_vst3_bridges_in(&[], out_dir, placement)
    }

    /// [`Self::sync_vst3_bridges_with`], and the Windows VST3 plugins in `extra_dirs` too.
    pub fn sync_vst3_bridges_in(
        &self,
        extra_dirs: &[PathBuf],
        out_dir: &Path,
        placement: Placement,
    ) -> Result<SyncReport> {
        let mut dirs = self.vst3_dirs();
        dirs.extend(extra_dirs.iter().filter(|d| d.is_dir()).cloned());
        sync_vst3_bridges_with(&dirs, &self.vst3_chainloader(), out_dir, placement)
    }

    /// Makes (and tidies) the bridges for this prefix's Windows CLAP plugins in `out_dir`.
    pub fn sync_clap_bridges(&self, out_dir: &Path) -> Result<SyncReport> {
        self.sync_clap_bridges_with(out_dir, Placement::Copy)
    }

    pub fn sync_clap_bridges_with(
        &self,
        out_dir: &Path,
        placement: Placement,
    ) -> Result<SyncReport> {
        self.sync_clap_bridges_in(&[], out_dir, placement)
    }

    /// [`Self::sync_clap_bridges_with`], and the Windows CLAP plugins in `extra_dirs` too.
    pub fn sync_clap_bridges_in(
        &self,
        extra_dirs: &[PathBuf],
        out_dir: &Path,
        placement: Placement,
    ) -> Result<SyncReport> {
        let mut dirs = self.clap_dirs();
        dirs.extend(extra_dirs.iter().filter(|d| d.is_dir()).cloned());
        sync_clap_bridges_with(&dirs, &self.chainloader(), out_dir, placement)
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

/// How a bridge gets its yabridge chainloader.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Placement {
    /// A copy of the chainloader. It does not know where its runtime is, so whoever loads it
    /// must put yabridge on `PATH` and set `WINELOADER` (the app does).
    #[default]
    Copy,
    /// A symlink to the chainloader. The bridge finds its runtime from there (see
    /// `vendor/yabridge/PATCHES.md`), so any plugin host can load it as it is, and a runtime
    /// update reaches it without a new sync when the link goes through a stable path.
    Link,
}

/// The chainloader a sync puts in place, as a copy or as a link.
struct Chainloader<'a> {
    path: &'a Path,
    bytes: Vec<u8>,
    placement: Placement,
}

impl<'a> Chainloader<'a> {
    fn new(path: &'a Path, placement: Placement, what: &str) -> Result<Self> {
        if !path.is_file() {
            bail!(
                "yabridge's {what} chainloader is missing: {}",
                path.display()
            );
        }
        let bytes = match placement {
            Placement::Copy => std::fs::read(path)?,
            Placement::Link => Vec::new(),
        };
        Ok(Self {
            path,
            bytes,
            placement,
        })
    }

    /// Is `bridge` already what this sync would make?
    fn is_placed(&self, bridge: &Path) -> bool {
        match self.placement {
            Placement::Copy => {
                // A link left from an earlier sync in the other mode is not a copy.
                std::fs::symlink_metadata(bridge).is_ok_and(|m| m.is_file())
                    && std::fs::read(bridge).is_ok_and(|b| b == self.bytes)
            }
            Placement::Link => std::fs::read_link(bridge).is_ok_and(|t| t == self.path),
        }
    }

    /// Makes `bridge`, beside it and then renamed, so a running host never sees half of one.
    fn place(&self, bridge: &Path) -> Result<()> {
        let tmp = bridge.with_extension("tmp");
        let _ = std::fs::remove_file(&tmp);
        match self.placement {
            Placement::Copy => {
                std::fs::write(&tmp, &self.bytes)?;
                set_executable(&tmp)?;
            }
            Placement::Link => std::os::unix::fs::symlink(self.path, &tmp)
                .with_context(|| format!("cannot link {}", bridge.display()))?,
        }
        std::fs::rename(&tmp, bridge)?;
        Ok(())
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
    /// Windows plugins that cannot be bridged, with the reason.
    pub skipped: Skipped,
}

/// The 64-bit Windows `.clap` files below `dir`. Other files named `.clap` (Linux plugins in a
/// folder of mixed plugins) are left alone; 32-bit Windows ones are reported as skipped.
fn find_windows_claps(
    dir: &Path,
    depth: usize,
    out: &mut Vec<PathBuf>,
    skipped: &mut Vec<(PathBuf, String)>,
) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut paths: Vec<PathBuf> = rd.flatten().map(|e| e.path()).collect();
    paths.sort();
    for p in paths {
        if p.extension().is_some_and(|e| e == "clap") && p.is_file() {
            match pe_arch(&p) {
                Some(PeArch::X64) => out.push(p),
                Some(PeArch::X86) => {
                    skipped.push((p, "32-bit (yabridge here runs 64-bit plugins)".into()))
                }
                _ => {}
            }
        } else if p.is_dir() && depth < MAX_DEPTH {
            find_windows_claps(&p, depth + 1, out, skipped);
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
    sync_clap_bridges_with(plugin_dirs, chainloader, out_dir, Placement::Copy)
}

/// [`sync_clap_bridges`] with a choice of how the chainloader is placed.
pub fn sync_clap_bridges_with(
    plugin_dirs: &[PathBuf],
    chainloader: &Path,
    out_dir: &Path,
    placement: Placement,
) -> Result<SyncReport> {
    let chainloader = Chainloader::new(chainloader, placement, "CLAP")?;
    let mut report = SyncReport::default();
    let mut wanted: Vec<PathBuf> = Vec::new();

    for dir in plugin_dirs {
        let (mut files, mut skipped) = (Vec::new(), Vec::new());
        find_windows_claps(dir, 0, &mut files, &mut skipped);
        report.skipped.extend(skipped);
        for real in files {
            let rel = real.strip_prefix(dir).unwrap_or(&real);
            let bridge = out_dir.join(rel);
            let link = bridge.with_extension(WIN_LINK);
            wanted.push(bridge.clone());
            if let Some(parent) = bridge.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let same_link = std::fs::read_link(&link).is_ok_and(|t| t == real);
            if chainloader.is_placed(&bridge) && same_link {
                report.unchanged += 1;
                continue;
            }
            chainloader.place(&bridge)?;
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

/// Which kind of Windows binary a file is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PeArch {
    X64,
    X86,
    Other,
}

/// Reads the machine type from a PE file's header. `None` if it is not a PE file.
pub fn pe_arch(path: &Path) -> Option<PeArch> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(path).ok()?;
    let mut dos = [0u8; 64];
    f.read_exact(&mut dos).ok()?;
    if &dos[..2] != b"MZ" {
        return None;
    }
    let pe_at = u32::from_le_bytes(dos[60..64].try_into().ok()?) as u64;
    f.seek(SeekFrom::Start(pe_at)).ok()?;
    let mut hdr = [0u8; 6];
    f.read_exact(&mut hdr).ok()?;
    if &hdr[..4] != b"PE\0\0" {
        return None;
    }
    Some(match u16::from_le_bytes([hdr[4], hdr[5]]) {
        0x8664 => PeArch::X64,
        0x014c => PeArch::X86,
        _ => PeArch::Other,
    })
}

/// A Windows VST3 plugin found in a prefix.
struct Vst3Module {
    /// The `X.vst3` file or bundle folder, as installed.
    installed: PathBuf,
    /// The DLL: the file itself, or `X.vst3/Contents/x86_64-win/X.vst3` inside a bundle.
    module: PathBuf,
    /// The folder it was in, below the search folder (vendors group their plugins).
    subdir: PathBuf,
    resources: Option<PathBuf>,
}

fn find_windows_vst3(
    root: &Path,
    dir: &Path,
    depth: usize,
    out: &mut Vec<Vst3Module>,
    skipped: &mut Vec<(PathBuf, String)>,
) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut paths: Vec<PathBuf> = rd.flatten().map(|e| e.path()).collect();
    paths.sort();
    for p in paths {
        let is_vst3 = p.extension().is_some_and(|e| e == "vst3");
        if is_vst3 {
            let name = p.file_name().map(PathBuf::from).unwrap_or_default();
            let subdir = dir.strip_prefix(root).unwrap_or(Path::new("")).to_owned();
            let (module, resources) = if p.is_dir() {
                // A Linux plugin, or a bridge that is already made (yabridge's own, or ours):
                // folders of plugins can hold anything, so these are not Windows plugins to
                // report on.
                if p.join("Contents/x86_64-linux").exists() {
                    continue;
                }
                let win64 = p.join("Contents/x86_64-win").join(&name);
                if !win64.exists() {
                    if p.join("Contents/x86-win").exists() {
                        skipped.push((
                            p.clone(),
                            "32-bit only (yabridge here runs 64-bit plugins)".into(),
                        ));
                    }
                    continue;
                }
                let res = p.join("Contents/Resources");
                (win64, res.is_dir().then_some(res))
            } else {
                (p.clone(), None)
            };
            match pe_arch(&module) {
                Some(PeArch::X64) => out.push(Vst3Module {
                    installed: p,
                    module,
                    subdir,
                    resources,
                }),
                Some(PeArch::X86) => {
                    skipped.push((p, "32-bit (yabridge here runs 64-bit plugins)".into()))
                }
                _ => skipped.push((p, "not a Windows plugin".into())),
            }
        } else if p.is_dir() && depth < MAX_DEPTH {
            find_windows_vst3(root, &p, depth + 1, out, skipped);
        }
    }
}

/// What [`sync_vst3_bridges`] could not use, with the reason.
pub type Skipped = Vec<(PathBuf, String)>;

/// For every 64-bit Windows VST3 plugin below `plugin_dirs`, makes a merged bundle in `out_dir`
/// the way `yabridgectl sync` does: `X.vst3/Contents/x86_64-linux/X.so` (a copy of yabridge's
/// VST3 chainloader) and `X.vst3/Contents/x86_64-win/X.vst3` (a link to the Windows plugin).
/// Bundles whose Windows plugin has gone are removed.
pub fn sync_vst3_bridges(
    plugin_dirs: &[PathBuf],
    chainloader: &Path,
    out_dir: &Path,
) -> Result<SyncReport> {
    sync_vst3_bridges_with(plugin_dirs, chainloader, out_dir, Placement::Copy)
}

/// [`sync_vst3_bridges`] with a choice of how the chainloader is placed.
pub fn sync_vst3_bridges_with(
    plugin_dirs: &[PathBuf],
    chainloader: &Path,
    out_dir: &Path,
    placement: Placement,
) -> Result<SyncReport> {
    let chainloader = Chainloader::new(chainloader, placement, "VST3")?;
    let mut report = SyncReport::default();
    let mut wanted: Vec<PathBuf> = Vec::new();

    for dir in plugin_dirs {
        let (mut found, mut skipped) = (Vec::new(), Vec::new());
        find_windows_vst3(dir, dir, 0, &mut found, &mut skipped);
        report.skipped.extend(skipped);
        for m in found {
            let name = m
                .installed
                .file_name()
                .map(PathBuf::from)
                .unwrap_or_default();
            let bundle = out_dir.join(&m.subdir).join(&name);
            let so = bundle
                .join("Contents/x86_64-linux")
                .join(name.with_extension("so"));
            let link = bundle.join("Contents/x86_64-win").join(&name);
            wanted.push(bundle.clone());
            std::fs::create_dir_all(so.parent().unwrap())?;
            std::fs::create_dir_all(link.parent().unwrap())?;
            let same_link = std::fs::read_link(&link).is_ok_and(|t| t == m.module);
            if let Some(res) = &m.resources {
                let rl = bundle.join("Contents/Resources");
                if std::fs::read_link(&rl).is_ok_and(|t| &t == res) {
                } else {
                    let _ = std::fs::remove_file(&rl);
                    let _ = std::os::unix::fs::symlink(res, &rl);
                }
            }
            if chainloader.is_placed(&so) && same_link {
                report.unchanged += 1;
                continue;
            }
            chainloader.place(&so)?;
            let _ = std::fs::remove_file(&link);
            std::os::unix::fs::symlink(&m.module, &link)
                .with_context(|| format!("cannot link {}", link.display()))?;
            report.created.push(bundle);
        }
    }

    // Our bundles whose Windows plugin is gone: the `x86_64-win` link points nowhere.
    let mut stale = Vec::new();
    collect_bundles(out_dir, 0, &mut stale);
    for bundle in stale {
        if wanted.contains(&bundle) {
            continue;
        }
        let dangling = std::fs::read_dir(bundle.join("Contents/x86_64-win"))
            .map(|rd| {
                rd.flatten()
                    .any(|e| e.file_type().is_ok_and(|t| t.is_symlink()) && !e.path().exists())
            })
            .unwrap_or(false);
        if dangling {
            let _ = std::fs::remove_dir_all(&bundle);
            report.removed.push(bundle);
        }
    }
    Ok(report)
}

/// Does the file contain this text? (A Windows DLL lists its exported names in the clear, so
/// looking for `VSTPluginMain` tells a VST2 plugin from any other DLL without loading it.)
fn file_contains(path: &Path, needle: &[u8]) -> bool {
    use std::io::Read;
    let Ok(mut f) = std::fs::File::open(path) else {
        return false;
    };
    let mut buf = vec![0u8; 1 << 20];
    let mut kept = 0;
    loop {
        let Ok(n) = f.read(&mut buf[kept..]) else {
            return false;
        };
        if n == 0 {
            return false;
        }
        let end = kept + n;
        if buf[..end].windows(needle.len()).any(|w| w == needle) {
            return true;
        }
        // Keep the tail so a name split across two reads is still found.
        kept = (needle.len() - 1).min(end);
        buf.copy_within(end - kept..end, 0);
    }
}

/// Windows VST2 plugins: 64-bit PE files that export `VSTPluginMain`.
fn find_windows_vst2(
    root: &Path,
    dir: &Path,
    depth: usize,
    out: &mut Vec<(PathBuf, PathBuf)>,
    skipped: &mut Vec<(PathBuf, String)>,
) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut paths: Vec<PathBuf> = rd.flatten().map(|e| e.path()).collect();
    paths.sort();
    for p in paths {
        let is_dll = p.extension().is_some_and(|e| e.eq_ignore_ascii_case("dll"));
        if is_dll && p.is_file() {
            // `X.so` beside `X.dll` is a bridge made already (yabridge's, or ours).
            if p.with_extension("so").is_file() {
                continue;
            }
            match pe_arch(&p) {
                Some(PeArch::X64) => {
                    if file_contains(&p, b"VSTPluginMain") {
                        out.push((p.clone(), p.strip_prefix(root).unwrap_or(&p).to_owned()));
                    }
                }
                Some(PeArch::X86) if file_contains(&p, b"VSTPluginMain") => {
                    skipped.push((p, "32-bit (yabridge here runs 64-bit plugins)".into()))
                }
                _ => {}
            }
        } else if p.is_dir() && depth < MAX_DEPTH {
            find_windows_vst2(root, &p, depth + 1, out, skipped);
        }
    }
}

/// For every 64-bit Windows VST2 plugin below `plugin_dirs`, makes `out_dir/<folder>/<rel>.so`
/// (a copy of yabridge's VST2 chainloader) and `<rel>.dll` beside it (a link to the real file),
/// which is how yabridge finds the plugin. Bridges whose DLL has gone are removed.
pub fn sync_vst2_bridges(
    plugin_dirs: &[PathBuf],
    chainloader: &Path,
    out_dir: &Path,
) -> Result<SyncReport> {
    sync_vst2_bridges_with(plugin_dirs, chainloader, out_dir, Placement::Copy)
}

/// [`sync_vst2_bridges`] with a choice of how the chainloader is placed.
pub fn sync_vst2_bridges_with(
    plugin_dirs: &[PathBuf],
    chainloader: &Path,
    out_dir: &Path,
    placement: Placement,
) -> Result<SyncReport> {
    let chainloader = Chainloader::new(chainloader, placement, "VST2")?;
    let mut report = SyncReport::default();
    let mut wanted: Vec<PathBuf> = Vec::new();
    for dir in plugin_dirs {
        let (mut found, mut skipped) = (Vec::new(), Vec::new());
        find_windows_vst2(dir, dir, 0, &mut found, &mut skipped);
        report.skipped.extend(skipped);
        let folder = dir.file_name().map(PathBuf::from).unwrap_or_default();
        for (real, rel) in found {
            let link = out_dir.join(&folder).join(&rel);
            let bridge = link.with_extension("so");
            wanted.push(bridge.clone());
            if let Some(parent) = bridge.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let same_link = std::fs::read_link(&link).is_ok_and(|t| t == real);
            if chainloader.is_placed(&bridge) && same_link {
                report.unchanged += 1;
                continue;
            }
            chainloader.place(&bridge)?;
            let _ = std::fs::remove_file(&link);
            std::os::unix::fs::symlink(&real, &link)
                .with_context(|| format!("cannot link {}", link.display()))?;
            report.created.push(bridge);
        }
    }
    // A `.dll` link in our folder that points nowhere, with its chainloader copy.
    let mut stale = Vec::new();
    collect_dll_links(out_dir, 0, &mut stale);
    for link in stale {
        let bridge = link.with_extension("so");
        if wanted.contains(&bridge) || link.exists() {
            continue;
        }
        let _ = std::fs::remove_file(&link);
        let _ = std::fs::remove_file(&bridge);
        report.removed.push(bridge);
    }
    Ok(report)
}

fn collect_dll_links(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        let is_link = e.file_type().is_ok_and(|t| t.is_symlink());
        if is_link && p.extension().is_some_and(|x| x == "dll") {
            out.push(p);
        } else if !is_link && p.is_dir() && depth <= MAX_DEPTH {
            collect_dll_links(&p, depth + 1, out);
        }
    }
}

fn collect_bundles(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        if e.file_type().is_ok_and(|t| t.is_dir()) {
            if p.extension().is_some_and(|x| x == "vst3") {
                out.push(p);
            } else if depth < MAX_DEPTH {
                collect_bundles(&p, depth + 1, out);
            }
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
        let d = std::env::temp_dir().join(format!("merlin-wine-{name}-{}", std::process::id()));
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
        std::fs::write(claps.join("Six Sines.clap"), pe(0x8664)).unwrap();
        std::fs::write(claps.join("u-he/Diva.clap"), pe(0x8664)).unwrap();
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
        std::fs::write(user.join("Odin.clap"), pe(0x8664)).unwrap();
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

    /// A minimal PE file with the given machine type.
    fn pe(machine: u16) -> Vec<u8> {
        let mut v = vec![0u8; 0x80];
        v[0] = b'M';
        v[1] = b'Z';
        v[60..64].copy_from_slice(&0x40u32.to_le_bytes());
        v[0x40..0x44].copy_from_slice(b"PE\0\0");
        v[0x44..0x46].copy_from_slice(&machine.to_le_bytes());
        v
    }

    fn vst3_fixture(name: &str) -> (Runtime, PathBuf) {
        let (rt, out) = fixture(name);
        std::fs::write(rt.vst3_chainloader(), b"ELF vst3 chainloader").unwrap();
        let v = rt.prefix.join("drive_c/Program Files/Common Files/VST3");
        std::fs::create_dir_all(v.join("u-he")).unwrap();
        // A single-file (legacy) module, one in a vendor folder, a bundle, a 32-bit one, junk.
        std::fs::write(v.join("Podolski(x64).vst3"), pe(0x8664)).unwrap();
        std::fs::write(v.join("u-he/Diva.vst3"), pe(0x8664)).unwrap();
        std::fs::create_dir_all(v.join("Bundle.vst3/Contents/x86_64-win")).unwrap();
        std::fs::write(
            v.join("Bundle.vst3/Contents/x86_64-win/Bundle.vst3"),
            pe(0x8664),
        )
        .unwrap();
        std::fs::create_dir_all(v.join("Bundle.vst3/Contents/Resources")).unwrap();
        std::fs::write(v.join("Old32.vst3"), pe(0x014c)).unwrap();
        std::fs::write(v.join("Fake.vst3"), b"not a dll").unwrap();
        std::fs::write(v.join("Podolski.data.lnk"), b"x").unwrap();
        (rt, out.join("vst3"))
    }

    #[test]
    fn windows_vst3_plugins_get_merged_bundles() {
        let (rt, out) = vst3_fixture("vst3");
        let report = rt.sync_vst3_bridges(&out).unwrap();
        assert_eq!(report.created.len(), 3, "{report:?}");
        let so = out.join("Podolski(x64).vst3/Contents/x86_64-linux/Podolski(x64).so");
        assert_eq!(std::fs::read(&so).unwrap(), b"ELF vst3 chainloader");
        let link = out.join("Podolski(x64).vst3/Contents/x86_64-win/Podolski(x64).vst3");
        assert!(
            std::fs::read_link(&link)
                .unwrap()
                .starts_with(rt.prefix.join("drive_c"))
        );
        assert!(link.exists());
        // Vendor folders are kept, bundles link their DLL and their Resources.
        assert!(
            out.join("u-he/Diva.vst3/Contents/x86_64-linux/Diva.so")
                .exists()
        );
        let b = out.join("Bundle.vst3/Contents");
        assert!(b.join("x86_64-win/Bundle.vst3").exists());
        assert!(b.join("Resources").exists());
        // Not bridged, and why.
        let names: Vec<_> = report
            .skipped
            .iter()
            .map(|(p, _)| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["Fake.vst3", "Old32.vst3"]);
        assert!(report.skipped[1].1.contains("32-bit"));
        assert!(!out.join("Old32.vst3").exists());
    }

    #[test]
    fn vst3_sync_is_repeatable_and_tidies_up() {
        let (rt, out) = vst3_fixture("vst3again");
        rt.sync_vst3_bridges(&out).unwrap();
        let again = rt.sync_vst3_bridges(&out).unwrap();
        assert_eq!((again.created.len(), again.unchanged), (0, 3));
        std::fs::remove_file(
            rt.prefix
                .join("drive_c/Program Files/Common Files/VST3/u-he/Diva.vst3"),
        )
        .unwrap();
        let after = rt.sync_vst3_bridges(&out).unwrap();
        assert_eq!(after.removed, vec![out.join("u-he/Diva.vst3")]);
        assert!(!out.join("u-he/Diva.vst3").exists());
        assert!(out.join("Podolski(x64).vst3").exists());
    }

    #[test]
    fn reads_the_machine_type_of_pe_files() {
        let d = tempdir("pe");
        for (name, machine, want) in [
            ("a", 0x8664, PeArch::X64),
            ("b", 0x014c, PeArch::X86),
            ("c", 0xaa64, PeArch::Other),
        ] {
            std::fs::write(d.join(name), pe(machine)).unwrap();
            assert_eq!(pe_arch(&d.join(name)), Some(want));
        }
        std::fs::write(d.join("elf"), b"\x7fELF....").unwrap();
        assert_eq!(pe_arch(&d.join("elf")), None);
        assert_eq!(pe_arch(&d.join("missing")), None);
    }

    #[test]
    fn one_folder_of_mixed_plugins_is_bridged_in_every_format() {
        let (rt, out) = vst3_fixture("mixed-formats");
        std::fs::write(rt.vst2_chainloader(), b"ELF vst2 chainloader").unwrap();
        let mine = rt.prefix.parent().unwrap().join("my plugins");
        std::fs::create_dir_all(mine.join("Native.vst3/Contents/x86_64-linux")).unwrap();
        std::fs::create_dir_all(mine.join("Merged.vst3/Contents/x86_64-linux")).unwrap();
        std::fs::create_dir_all(mine.join("Merged.vst3/Contents/x86_64-win")).unwrap();
        std::fs::write(mine.join("Odd.clap"), pe(0x8664)).unwrap();
        std::fs::write(mine.join("Native.clap"), b"\x7fELF").unwrap();
        std::fs::write(mine.join("Fresh.vst3"), pe(0x8664)).unwrap();
        let mut dll = pe(0x8664);
        dll.extend_from_slice(b"VSTPluginMain");
        std::fs::write(mine.join("Old.dll"), &dll).unwrap();
        std::fs::write(mine.join("Bridged.dll"), &dll).unwrap();
        std::fs::write(mine.join("Bridged.so"), b"ELF").unwrap();
        let extra = std::slice::from_ref(&mine);
        let copy = Placement::Copy;

        let claps = rt
            .sync_clap_bridges_in(extra, &out.join("clap"), copy)
            .unwrap();
        assert!(out.join("clap/Odd.clap").exists());
        assert!(!out.join("clap/Native.clap").exists());
        assert!(claps.skipped.is_empty(), "{claps:?}");

        let vst3 = rt.sync_vst3_bridges_in(extra, &out, copy).unwrap();
        assert!(
            out.join("Fresh.vst3/Contents/x86_64-win/Fresh.vst3")
                .exists()
        );
        assert!(!out.join("Native.vst3").exists() && !out.join("Merged.vst3").exists());
        let skipped: Vec<_> = vst3
            .skipped
            .iter()
            .map(|(p, _)| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            skipped,
            ["Fake.vst3", "Old32.vst3"],
            "native bundles are not reported"
        );

        let vst2 = rt.sync_vst2_bridges(extra, &out.join("vst2")).unwrap();
        assert_eq!(vst2.created.len(), 1, "{vst2:?}");
        assert!(out.join("vst2/my plugins/Old.so").exists());
    }

    #[test]
    fn vst2_dlls_are_told_from_other_dlls_and_bridged_beside_a_link() {
        let (rt, out) = fixture("vst2");
        std::fs::write(rt.vst2_chainloader(), b"ELF vst2 chainloader").unwrap();
        let v = rt.prefix.join("drive_c/Program Files/VstPlugins");
        std::fs::create_dir_all(v.join("ReaPlugs")).unwrap();
        let plugin = |name: &str, machine: u16, exports: bool| {
            let mut bytes = pe(machine);
            if exports {
                bytes.extend_from_slice(b"..VSTPluginMain..");
            }
            std::fs::write(v.join(name), bytes).unwrap();
        };
        plugin("Synth.dll", 0x8664, true);
        plugin("ReaPlugs/reaeq.dll", 0x8664, true);
        plugin("helper.dll", 0x8664, false); // a DLL that is not a plugin
        plugin("Old.dll", 0x14c, true); // 32-bit
        let report = rt.sync_vst2_bridges(&[], &out).unwrap();
        assert_eq!(report.created.len(), 2, "{report:?}");
        assert_eq!(report.skipped.len(), 1);
        let synth = out.join("VstPlugins/Synth.so");
        assert_eq!(std::fs::read(&synth).unwrap(), b"ELF vst2 chainloader");
        assert_eq!(
            std::fs::read_link(out.join("VstPlugins/Synth.dll")).unwrap(),
            v.join("Synth.dll")
        );
        assert!(out.join("VstPlugins/ReaPlugs/reaeq.so").exists());
        assert!(!out.join("VstPlugins/helper.so").exists());
        // Again: nothing to do. Remove a DLL: its bridge goes.
        let again = rt.sync_vst2_bridges(&[], &out).unwrap();
        assert_eq!((again.created.len(), again.unchanged), (0, 2));
        std::fs::remove_file(v.join("Synth.dll")).unwrap();
        let after = rt.sync_vst2_bridges(&[], &out).unwrap();
        assert_eq!(after.removed.len(), 1);
        assert!(!synth.exists());
    }

    #[test]
    fn linked_bridges_point_at_the_chainloader_and_follow_it() {
        let (rt, out) = fixture("linked");
        let report = rt.sync_clap_bridges_with(&out, Placement::Link).unwrap();
        assert_eq!(report.created.len(), 2);
        for name in ["Six Sines", "u-he/Diva"] {
            let bridge = out.join(format!("{name}.clap"));
            assert_eq!(std::fs::read_link(&bridge).unwrap(), rt.chainloader());
            assert!(out.join(format!("{name}.clap-win")).exists());
        }
        // Nothing to do the second time, and the chainloader itself is not rewritten.
        let again = rt.sync_clap_bridges_with(&out, Placement::Link).unwrap();
        assert_eq!((again.created.len(), again.unchanged), (0, 2));
        // Copies from an earlier sync become links, and links become copies.
        let back = rt.sync_clap_bridges(&out).unwrap();
        assert_eq!(back.created.len(), 2);
        assert!(
            !std::fs::symlink_metadata(out.join("Six Sines.clap"))
                .unwrap()
                .is_symlink()
        );
        let relinked = rt.sync_clap_bridges_with(&out, Placement::Link).unwrap();
        assert_eq!(relinked.created.len(), 2);
        // A bridge whose plugin is gone is removed, link and all.
        std::fs::remove_file(
            rt.prefix
                .join("drive_c/Program Files/Common Files/CLAP/u-he/Diva.clap"),
        )
        .unwrap();
        let report = rt.sync_clap_bridges_with(&out, Placement::Link).unwrap();
        assert_eq!(report.removed, vec![out.join("u-he/Diva.clap")]);
        assert!(std::fs::symlink_metadata(out.join("u-he/Diva.clap")).is_err());
    }

    #[test]
    fn linked_vst3_and_vst2_bridges_work_the_same_way() {
        let (rt, out) = vst3_fixture("linked3");
        let report = rt.sync_vst3_bridges_with(&out, Placement::Link).unwrap();
        assert_eq!(report.created.len(), 3);
        let so = out.join("Podolski(x64).vst3/Contents/x86_64-linux/Podolski(x64).so");
        assert_eq!(std::fs::read_link(&so).unwrap(), rt.vst3_chainloader());
        let again = rt.sync_vst3_bridges_with(&out, Placement::Link).unwrap();
        assert_eq!((again.created.len(), again.unchanged), (0, 3));

        let dll_dir = rt.prefix.join("drive_c/Program Files/VstPlugins");
        std::fs::create_dir_all(&dll_dir).unwrap();
        std::fs::write(
            dll_dir.join("Synth.dll"),
            [pe(0x8664), b"VSTPluginMain".to_vec()].concat(),
        )
        .unwrap();
        std::fs::write(rt.vst2_chainloader(), b"ELF vst2").unwrap();
        let vst2_out = out.join("vst2");
        let report = rt
            .sync_vst2_bridges_with(&[], &vst2_out, Placement::Link)
            .unwrap();
        assert_eq!(report.created.len(), 1);
        let bridge = vst2_out.join("VstPlugins/Synth.so");
        assert_eq!(std::fs::read_link(&bridge).unwrap(), rt.vst2_chainloader());
    }

    #[test]
    fn a_linux_clap_in_a_mixed_folder_is_not_bridged_and_a_32_bit_one_is_reported() {
        let (rt, out) = fixture("mixed");
        let dir = rt.prefix.join("drive_c/Program Files/Common Files/CLAP");
        std::fs::write(dir.join("Native.clap"), b"\x7fELF native plugin").unwrap();
        std::fs::write(dir.join("Old32.clap"), pe(0x014c)).unwrap();
        let report = rt.sync_clap_bridges(&out).unwrap();
        assert_eq!(
            report.created.len(),
            2,
            "only the two 64-bit Windows plugins"
        );
        assert!(!out.join("Native.clap").exists() && !out.join("Old32.clap").exists());
        let names: Vec<_> = report
            .skipped
            .iter()
            .map(|(p, _)| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["Old32.clap"]);
    }
}
