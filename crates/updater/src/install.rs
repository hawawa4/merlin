//! Downloads a release, checks it and puts it in place.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};

use anyhow::{Context, Result, anyhow, bail};

use crate::http;
use crate::kind::{Install, Kind, deb_prerequisites};
use crate::release::{Asset, Release, checksum_for};

/// How far an install is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Progress {
    Downloading {
        done: u64,
        total: u64,
    },
    /// Unpacking and replacing files, or (for a `.deb`) waiting for the package installer.
    Installing,
}

/// The programs of the tarball. The app looks for the first two next to itself, so all of them
/// are replaced together; the bridge is only replaced where it was already put.
const HELPERS: [(&str, bool); 3] = [
    ("merlin-scan", true),
    ("merlin-plugin-host", true),
    ("merlin-bridge", false),
];

/// Installs `rel` for `inst`. On success the new version is on disk; the running process keeps
/// going until the app restarts.
pub fn install(rel: &Release, inst: &Install, report: &dyn Fn(Progress)) -> Result<()> {
    let name = inst
        .asset_name(&rel.version)
        .ok_or_else(|| anyhow!("self-update isn't available on this platform"))?;
    let asset = rel
        .asset(&name)
        .ok_or_else(|| anyhow!("release {} has no {name} file", rel.version))?;
    let want = checksum_for(rel, &name)?;
    match inst.kind {
        Kind::AppImage => install_file(asset, &want, &inst.path, report),
        Kind::Binary => install_tarball(asset, &want, &inst.path, &rel.version, report),
        Kind::Deb => install_deb(asset, &want, report),
        Kind::Unsupported => unreachable!("asset_name is None for it"),
    }
}

/// A fresh directory in the system temp folder.
fn scratch_dir() -> Result<PathBuf> {
    static N: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "merlin-update-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Removes the directory when dropped.
struct Cleanup(PathBuf);
impl Drop for Cleanup {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Downloads `asset` to `dest` and checks its SHA-256 against `want`.
fn fetch(asset: &Asset, dest: &Path, want: &str, report: &dyn Fn(Progress)) -> Result<()> {
    let total = asset.size;
    http::download(&asset.url, dest, &|done| {
        report(Progress::Downloading { done, total })
    })?;
    let got = http::sha256_file(dest)?;
    if got != want {
        let _ = std::fs::remove_file(dest);
        bail!(
            "checksum mismatch for {} (expected {want}, got {got})",
            asset.name
        );
    }
    Ok(())
}

fn install_deb(asset: &Asset, want: &str, report: &dyn Fn(Progress)) -> Result<()> {
    if let Some(why) = deb_prerequisites() {
        bail!(why);
    }
    let dir = scratch_dir()?;
    let _cleanup = Cleanup(dir.clone());
    // apt's sandbox user needs to read the package; otherwise it only warns.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755));
    }
    let deb = dir.join(&asset.name);
    fetch(asset, &deb, want, report)?;
    report(Progress::Installing);
    run_apt(&deb)
}

/// apt through pkexec: polkit shows the desktop's own password dialog. pkexec clears the
/// environment, so apt is told not to ask questions.
fn run_apt(deb: &Path) -> Result<()> {
    let out = Command::new("pkexec")
        .args([
            "env",
            "DEBIAN_FRONTEND=noninteractive",
            "apt-get",
            "install",
            "-y",
        ])
        .arg(deb)
        .stdin(std::process::Stdio::null())
        .output()
        .context("cannot run pkexec")?;
    match out.status.code() {
        Some(0) => Ok(()),
        Some(126) => bail!("the password prompt was cancelled"),
        Some(127) => bail!("not authorised to install packages"),
        _ => {
            let text = String::from_utf8_lossy(&out.stdout).into_owned()
                + &String::from_utf8_lossy(&out.stderr);
            bail!("apt-get failed ({}): {}", out.status, tail(&text, 400))
        }
    }
}

/// At most `n` trailing bytes of `s`, trimmed (never cutting a character in half).
fn tail(s: &str, n: usize) -> String {
    let s = s.trim();
    if s.len() <= n {
        return s.to_owned();
    }
    let mut start = s.len() - n;
    while !s.is_char_boundary(start) {
        start += 1;
    }
    format!("…{}", &s[start..])
}

/// Downloads the AppImage next to the running one and renames it over it.
fn install_file(asset: &Asset, want: &str, target: &Path, report: &dyn Fn(Progress)) -> Result<()> {
    let staged = Staged::beside(target)?;
    fetch(asset, &staged.path, want, report)?;
    report(Progress::Installing);
    staged.replace(target)
}

fn install_tarball(
    asset: &Asset,
    want: &str,
    exe: &Path,
    version: &str,
    report: &dyn Fn(Progress),
) -> Result<()> {
    let dir = exe
        .parent()
        .ok_or_else(|| anyhow!("cannot tell where the app is installed"))?;
    // What gets replaced: the files in the archive and where each goes.
    let mut targets: Vec<(&str, PathBuf)> = HELPERS
        .iter()
        .filter(|(name, always)| *always || dir.join(name).exists())
        .map(|(name, _)| (*name, dir.join(name)))
        .collect();
    targets.push(("merlin", exe.to_owned()));

    // Fail early, before downloading, if the install folder isn't writable.
    let staged: Vec<Staged> = targets
        .iter()
        .map(|(_, to)| Staged::beside(to))
        .collect::<Result<_>>()?;

    let work = scratch_dir()?;
    let _cleanup = Cleanup(work.clone());
    let archive = work.join(&asset.name);
    fetch(asset, &archive, want, report)?;
    report(Progress::Installing);
    let unpacked = work.join("unpacked");
    unpack(&archive, &unpacked)?;

    // Everything is staged before anything is replaced, so a bad archive changes nothing.
    for ((name, _), stage) in targets.iter().zip(&staged) {
        let from = find_file(&unpacked, name, 2)
            .ok_or_else(|| anyhow!("the archive for {version} has no {name}"))?;
        std::fs::copy(&from, &stage.path).with_context(|| format!("copying {name}"))?;
    }
    // The helpers go first and the app last: if this is interrupted the old app is still whole.
    for ((_, to), stage) in targets.iter().zip(&staged) {
        stage.replace(to)?;
    }
    Ok(())
}

/// Unpacks a `.tar.gz` with the system `tar` (GNU and BSD tar refuse paths that leave `dest`).
fn unpack(archive: &Path, dest: &Path) -> Result<()> {
    std::fs::create_dir_all(dest)?;
    let out = Command::new("tar")
        .arg("-xzf")
        .arg(archive)
        .arg("-C")
        .arg(dest)
        .output()
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => anyhow!("`tar` is needed to unpack the update"),
            _ => anyhow!("cannot run tar: {e}"),
        })?;
    if !out.status.success() {
        bail!(
            "cannot unpack the download: {}",
            tail(&String::from_utf8_lossy(&out.stderr), 300)
        );
    }
    Ok(())
}

/// The regular file called `name` in `dir` or up to `depth` folders below it.
fn find_file(dir: &Path, name: &str, depth: u32) -> Option<PathBuf> {
    let mut subdirs = Vec::new();
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let Ok(t) = entry.file_type() else { continue };
        if t.is_file() && entry.file_name() == name {
            return Some(entry.path());
        }
        if t.is_dir() {
            subdirs.push(entry.path());
        }
    }
    if depth == 0 {
        return None;
    }
    subdirs
        .into_iter()
        .find_map(|d| find_file(&d, name, depth - 1))
}

/// An empty file next to its target, so the final rename never crosses a filesystem. Creating
/// it fails when the folder isn't writable, which is the "can't self-update here" case.
struct Staged {
    path: PathBuf,
}

impl Staged {
    fn beside(target: &Path) -> Result<Self> {
        static N: AtomicU32 = AtomicU32::new(0);
        let dir = target.parent().unwrap_or(Path::new("."));
        let path = dir.join(format!(
            ".merlin-update-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .with_context(|| format!("can't write next to {}", target.display()))?;
        Ok(Self { path })
    }

    /// Swaps the staged file in for `target`. The old file stays valid for a process that has it
    /// open, which keeps its own inode.
    fn replace(&self, target: &Path) -> Result<()> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode =
                std::fs::metadata(target).map_or(0o755, |m| m.permissions().mode() & 0o777) | 0o111;
            std::fs::set_permissions(&self.path, std::fs::Permissions::from_mode(mode))?;
        }
        std::fs::rename(&self.path, target)
            .with_context(|| format!("cannot replace {}", target.display()))
    }
}

impl Drop for Staged {
    fn drop(&mut self) {
        // Whatever was not renamed into place (a failure part-way) is not left lying around.
        let _ = std::fs::remove_file(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn have_tar() -> bool {
        Command::new("tar").arg("--version").output().is_ok()
    }

    #[test]
    fn tail_keeps_the_end_and_whole_characters() {
        assert_eq!(tail("  short ", 10), "short");
        assert_eq!(tail("abcdef", 3), "…def");
        assert_eq!(tail("aéé", 3), "…é");
    }

    #[test]
    fn staging_fails_where_the_folder_is_missing() {
        assert!(Staged::beside(Path::new("/nonexistent-dir-for-merlin/merlin")).is_err());
    }

    #[test]
    fn replaces_the_programs_and_leaves_nothing_behind() {
        if !have_tar() {
            return;
        }
        let root = scratch_dir().unwrap();
        let _cleanup = Cleanup(root.clone());
        // An archive laid out like the release's: merlin-V-linux-amd64/<programs>.
        let src = root.join("src").join("merlin-9.9.9-linux-amd64");
        std::fs::create_dir_all(&src).unwrap();
        for n in [
            "merlin",
            "merlin-scan",
            "merlin-plugin-host",
            "merlin-bridge",
        ] {
            std::fs::write(src.join(n), format!("new {n}")).unwrap();
        }
        let archive = root.join("a.tar.gz");
        assert!(
            Command::new("tar")
                .arg("-czf")
                .arg(&archive)
                .arg("-C")
                .arg(root.join("src"))
                .arg("merlin-9.9.9-linux-amd64")
                .status()
                .unwrap()
                .success()
        );

        let live = root.join("live");
        std::fs::create_dir_all(&live).unwrap();
        for n in ["merlin", "merlin-scan", "merlin-plugin-host"] {
            std::fs::write(live.join(n), format!("old {n}")).unwrap();
        }
        let size = std::fs::metadata(&archive).unwrap().len();
        let sum = http::sha256_file(&archive).unwrap();
        let asset = Asset {
            name: "a.tar.gz".into(),
            url: format!("file://{}", archive.display()),
            size,
        };
        install_tarball(&asset, &sum, &live.join("merlin"), "9.9.9", &|_| {}).unwrap();

        for n in ["merlin", "merlin-scan", "merlin-plugin-host"] {
            assert_eq!(
                std::fs::read_to_string(live.join(n)).unwrap(),
                format!("new {n}")
            );
        }
        // The bridge was not there, so it is not added.
        assert!(!live.join("merlin-bridge").exists());
        // No staging files left.
        assert_eq!(std::fs::read_dir(&live).unwrap().count(), 3);
    }

    #[test]
    fn replaces_an_appimage_in_place() {
        let root = scratch_dir().unwrap();
        let _cleanup = Cleanup(root.clone());
        let new = root.join("new.AppImage");
        std::fs::write(&new, "new image").unwrap();
        let live = root.join("live");
        std::fs::create_dir_all(&live).unwrap();
        let target = live.join("Merlin.AppImage");
        std::fs::write(&target, "old image").unwrap();
        let sum = http::sha256_file(&new).unwrap();
        let asset = Asset {
            name: "new.AppImage".into(),
            url: format!("file://{}", new.display()),
            size: 9,
        };

        install_file(&asset, &"1".repeat(64), &target, &|_| {}).unwrap_err();
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "old image");
        install_file(&asset, &sum, &target, &|_| {}).unwrap();
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "new image");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_ne!(
                std::fs::metadata(&target).unwrap().permissions().mode() & 0o111,
                0
            );
        }
        assert_eq!(std::fs::read_dir(&live).unwrap().count(), 1);
    }

    #[test]
    fn a_wrong_checksum_changes_nothing() {
        if !have_tar() {
            return;
        }
        let root = scratch_dir().unwrap();
        let _cleanup = Cleanup(root.clone());
        let archive = root.join("a.tar.gz");
        std::fs::write(&archive, b"not an archive").unwrap();
        let live = root.join("live");
        std::fs::create_dir_all(&live).unwrap();
        std::fs::write(live.join("merlin"), "old").unwrap();
        let asset = Asset {
            name: "a.tar.gz".into(),
            url: format!("file://{}", archive.display()),
            size: 14,
        };
        let err = install_tarball(
            &asset,
            &"0".repeat(64),
            &live.join("merlin"),
            "9.9.9",
            &|_| {},
        )
        .unwrap_err();
        assert!(err.to_string().contains("checksum mismatch"), "{err}");
        assert_eq!(std::fs::read_to_string(live.join("merlin")).unwrap(), "old");
        assert_eq!(std::fs::read_dir(&live).unwrap().count(), 1);
    }
}
