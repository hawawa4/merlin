//! The Wine + yabridge pair we ship (plan D6): fetched once, kept in our own folder.
//!
//! A *manifest* (a small JSON file) names a pinned archive and its checksum. [`install`] fetches
//! the manifest, downloads the archive (resuming an interrupted download), checks the SHA-256,
//! unpacks it beside the final place and renames it into position, so a failure at any step
//! leaves the previous runtime untouched. The archive is `tar` + `zstd`; both are read in
//! Rust, so nothing but `curl` has to exist on the machine.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::Runtime;

/// What a release publishes next to the archive.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    /// Names the Wine and yabridge builds, e.g. `wine-11.20-vblank_yabridge-b580a9f7`.
    pub id: String,
    /// The archive, relative to the manifest's own URL (or absolute).
    pub file: String,
    pub sha256: String,
    /// Bytes to download.
    pub size: u64,
    /// Bytes on disk once unpacked, for the "needs N MB" line.
    pub unpacked_size: u64,
}

impl Manifest {
    pub fn parse(json: &str) -> Result<Self> {
        let m: Manifest = serde_json::from_str(json).context("not a runtime manifest")?;
        if m.id.is_empty() || m.id.contains(['/', '\\']) || m.id.starts_with('.') {
            bail!("the manifest has an unusable id: {:?}", m.id);
        }
        if m.sha256.len() != 64 || !m.sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
            bail!("the manifest has no valid SHA-256");
        }
        Ok(m)
    }

    /// Where the archive is, given where the manifest came from.
    pub fn archive_url(&self, manifest_url: &str) -> String {
        if self.file.contains("://") {
            return self.file.clone();
        }
        match manifest_url.rfind('/') {
            Some(i) => format!("{}/{}", &manifest_url[..i], self.file),
            None => self.file.clone(),
        }
    }
}

/// An unpacked runtime on disk.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Installed {
    pub id: String,
    pub wine: PathBuf,
    pub yabridge_dir: PathBuf,
}

impl Installed {
    /// A [`Runtime`] using this Wine and yabridge with `prefix`.
    pub fn runtime(&self, prefix: PathBuf) -> Runtime {
        Runtime {
            wine: self.wine.clone(),
            yabridge_dir: self.yabridge_dir.clone(),
            prefix,
        }
    }
}

fn describe(root: &Path, id: &str) -> Option<Installed> {
    let dir = root.join(id);
    let wine = dir.join("wine/bin/wine");
    let yabridge_dir = dir.join("yabridge");
    (wine.is_file()
        && yabridge_dir
            .join("libyabridge-chainloader-clap.so")
            .is_file())
    .then(|| Installed {
        id: id.to_owned(),
        wine,
        yabridge_dir,
    })
}

/// The runtime in `root` that is current, if there is a complete one.
pub fn installed(root: &Path) -> Option<Installed> {
    let id = std::fs::read_to_string(root.join("CURRENT")).ok()?;
    describe(root, id.trim())
}

/// The stable name of the current runtime: `<root>/current` is a link to `<root>/<id>`. Bridges
/// made for other plugin hosts link through it, so a new runtime reaches them without a new sync.
pub const CURRENT_LINK: &str = "current";

/// Name of the file in `yabridge/` that tells bridges which Wine prefix a plugin outside every
/// prefix uses (see `vendor/yabridge/PATCHES.md`).
pub const DEFAULT_PREFIX_FILE: &str = "default-wineprefix";

impl Installed {
    /// Where `<root>/current` reaches this runtime's yabridge folder.
    pub fn stable_yabridge_dir(root: &Path) -> PathBuf {
        root.join(CURRENT_LINK).join("yabridge")
    }

    /// Writes `yabridge/default-wineprefix`: the prefix a plugin that is in no prefix runs in
    /// (rather than `~/.wine`). Needed again after each runtime update: the file lives in the
    /// runtime's own folder.
    pub fn set_default_prefix(&self, prefix: &Path) -> Result<()> {
        let file = self.yabridge_dir.join(DEFAULT_PREFIX_FILE);
        let text = format!("{}\n", prefix.display());
        if std::fs::read_to_string(&file).is_ok_and(|t| t == text) {
            return Ok(());
        }
        std::fs::write(&file, text).with_context(|| format!("cannot write {}", file.display()))
    }
}

/// Points `<root>/current` at runtime `id`, atomically.
fn point_current(root: &Path, id: &str) -> Result<()> {
    let link = root.join(CURRENT_LINK);
    if std::fs::read_link(&link).is_ok_and(|t| t == Path::new(id)) {
        return Ok(());
    }
    let tmp = root.join(format!("{CURRENT_LINK}.tmp"));
    let _ = std::fs::remove_file(&tmp);
    std::os::unix::fs::symlink(id, &tmp)?;
    std::fs::rename(&tmp, &link).context("cannot update the `current` link")
}

/// [`installed`], and makes sure `<root>/current` reaches it (a runtime installed before that
/// link existed gets one).
pub fn ensure_current(root: &Path) -> Result<Option<Installed>> {
    let Some(have) = installed(root) else {
        return Ok(None);
    };
    point_current(root, &have.id)?;
    Ok(Some(have))
}

/// How far an install is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Progress {
    Manifest,
    Downloading { done: u64, total: u64 },
    Verifying,
    Unpacking,
}

fn curl(url: &str) -> Command {
    let mut c = Command::new("curl");
    c.args([
        "--fail",
        "--location",
        "--silent",
        "--show-error",
        "--retry",
        "3",
    ])
    .arg(url)
    .stdin(Stdio::null());
    c
}

/// Fetches a small text file.
pub fn fetch_text(url: &str) -> Result<String> {
    let out = curl(url)
        .arg("--max-time")
        .arg("60")
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| curl_missing(&e))?;
    if !out.status.success() {
        bail!(
            "cannot fetch {url}: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    String::from_utf8(out.stdout).context("the manifest is not text")
}

fn curl_missing(e: &std::io::Error) -> anyhow::Error {
    if e.kind() == std::io::ErrorKind::NotFound {
        anyhow!("`curl` is needed to download the Windows plugin support and was not found")
    } else {
        anyhow!("cannot run curl: {e}")
    }
}

/// Downloads `url` to `part`, continuing what is already there, and reports progress.
fn download(url: &str, part: &Path, total: u64, progress: &dyn Fn(Progress)) -> Result<()> {
    let mut cmd = curl(url);
    cmd.arg("-C")
        .arg("-")
        .arg("-o")
        .arg(part)
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().map_err(|e| curl_missing(&e))?;
    loop {
        if let Some(status) = child.try_wait()? {
            if !status.success() {
                let mut err = String::new();
                if let Some(mut e) = child.stderr.take() {
                    let _ = e.read_to_string(&mut err);
                }
                // A finished file makes curl's resume request fail with 416; that is success.
                if std::fs::metadata(part).is_ok_and(|m| m.len() == total && total > 0) {
                    return Ok(());
                }
                bail!("download failed: {}", err.trim());
            }
            return Ok(());
        }
        let done = std::fs::metadata(part).map_or(0, |m| m.len());
        progress(Progress::Downloading { done, total });
        std::thread::sleep(Duration::from_millis(200));
    }
}

fn sha256_file(path: &Path) -> Result<String> {
    let mut f = std::fs::File::open(path)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

/// Unpacks a `tar.zst` into `dest`. Only paths inside `dest` are written.
pub fn unpack(archive: &Path, dest: &Path) -> Result<()> {
    let file = std::io::BufReader::with_capacity(1 << 20, std::fs::File::open(archive)?);
    let decoder = ruzstd::decoding::StreamingDecoder::new(file)
        .map_err(|e| anyhow!("not a zstd archive: {e}"))?;
    let mut tar = tar::Archive::new(decoder);
    tar.set_preserve_permissions(true);
    tar.set_overwrite(true);
    // `unpack` refuses entries that would land outside `dest` (`..`, absolute paths).
    tar.unpack(dest).context("unpacking the archive")
}

/// Installs the runtime described by the manifest at `manifest_url` into `root`, unless that
/// exact runtime is already there. Blocks for as long as the download takes.
pub fn install(manifest_url: &str, root: &Path, progress: &dyn Fn(Progress)) -> Result<Installed> {
    progress(Progress::Manifest);
    let manifest = Manifest::parse(&fetch_text(manifest_url)?)?;
    let previous = installed(root);
    if let Some(have) = describe(root, &manifest.id) {
        std::fs::write(root.join("CURRENT"), &manifest.id)?;
        point_current(root, &manifest.id)?;
        return Ok(have);
    }
    let work = root.join("download");
    std::fs::create_dir_all(&work)?;
    let part = work.join(format!("{}.part", manifest.id));
    download(
        &manifest.archive_url(manifest_url),
        &part,
        manifest.size,
        progress,
    )?;

    progress(Progress::Verifying);
    let got = sha256_file(&part)?;
    if !got.eq_ignore_ascii_case(&manifest.sha256) {
        let _ = std::fs::remove_file(&part);
        bail!(
            "the download is damaged (checksum {got}, expected {})",
            manifest.sha256
        );
    }

    progress(Progress::Unpacking);
    let staging = root.join(format!("{}.unpacking", manifest.id));
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging)?;
    if let Err(e) = unpack(&part, &staging) {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(e);
    }
    let target = root.join(&manifest.id);
    let _ = std::fs::remove_dir_all(&target);
    std::fs::rename(&staging, &target)?;
    let _ = std::fs::remove_file(&part);
    let Some(have) = describe(root, &manifest.id) else {
        bail!("the archive did not contain wine/bin/wine and yabridge/");
    };
    // The new runtime keeps the prefix the old one named for plugins outside every prefix.
    if let Some(prefix) = previous
        .as_ref()
        .and_then(|p| std::fs::read_to_string(p.yabridge_dir.join(DEFAULT_PREFIX_FILE)).ok())
    {
        let _ = std::fs::write(have.yabridge_dir.join(DEFAULT_PREFIX_FILE), prefix);
    }
    // Last: only now does the new runtime become the current one.
    let tmp = root.join("CURRENT.tmp");
    std::fs::File::create(&tmp)?.write_all(manifest.id.as_bytes())?;
    std::fs::rename(&tmp, root.join("CURRENT"))?;
    point_current(root, &manifest.id)?;
    remove_old(root, &manifest.id);
    Ok(have)
}

/// Deletes runtimes other than `keep` (they are hundreds of megabytes each).
fn remove_old(root: &Path, keep: &str) {
    let Ok(rd) = std::fs::read_dir(root) else {
        return;
    };
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if e.path().is_dir()
            && name != keep
            && name != "download"
            && (name.starts_with("wine-") || name.ends_with(".unpacking"))
        {
            let _ = std::fs::remove_dir_all(e.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tempdir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("merlin-rt-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// A tar.zst with a tiny fake runtime inside, and its manifest, in `dir`.
    fn publish(dir: &Path, id: &str, extra: &[(&str, &[u8])]) -> PathBuf {
        let mut tar_bytes = Vec::new();
        {
            let mut b = tar::Builder::new(&mut tar_bytes);
            let mut add = |path: &str, data: &[u8], mode: u32| {
                let mut h = tar::Header::new_gnu();
                h.set_size(data.len() as u64);
                h.set_mode(mode);
                h.set_cksum();
                b.append_data(&mut h, path, data).unwrap();
            };
            add("wine/bin/wine", b"#!/bin/sh\necho wine-test\n", 0o755);
            add("yabridge/libyabridge-chainloader-clap.so", b"ELF", 0o755);
            add("yabridge/libyabridge-chainloader-vst3.so", b"ELF", 0o755);
            for (p, d) in extra {
                add(p, d, 0o644);
            }
            b.finish().unwrap();
        }
        let zst = ruzstd::encoding::compress_to_vec(
            &tar_bytes[..],
            ruzstd::encoding::CompressionLevel::Fastest,
        );
        let file = format!("{id}.tar.zst");
        std::fs::write(dir.join(&file), &zst).unwrap();
        let sha: String = Sha256::digest(&zst)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let m = Manifest {
            id: id.into(),
            file,
            sha256: sha,
            size: zst.len() as u64,
            unpacked_size: tar_bytes.len() as u64,
        };
        let path = dir.join("runtime-manifest.json");
        std::fs::write(&path, serde_json::to_string_pretty(&m).unwrap()).unwrap();
        path
    }

    fn have_curl() -> bool {
        Command::new("curl").arg("--version").output().is_ok()
    }

    fn url(p: &Path) -> String {
        format!("file://{}", p.display())
    }

    #[test]
    fn manifests_are_checked_and_resolve_the_archive_beside_them() {
        let ok = r#"{"id":"wine-1_y","file":"a.tar.zst","sha256":"0000000000000000000000000000000000000000000000000000000000000000","size":1,"unpacked_size":2}"#;
        let m = Manifest::parse(ok).unwrap();
        assert_eq!(
            m.archive_url("https://h/rel/runtime-manifest.json"),
            "https://h/rel/a.tar.zst"
        );
        assert_eq!(
            m.archive_url("file:///tmp/x/m.json"),
            "file:///tmp/x/a.tar.zst"
        );
        assert!(Manifest::parse(&ok.replace("wine-1_y", "../evil")).is_err());
        assert!(Manifest::parse(&ok.replace(&"0".repeat(64), "abc")).is_err());
        assert!(Manifest::parse("{}").is_err());
        let abs = ok.replace("a.tar.zst", "https://cdn/b.tar.zst");
        assert_eq!(
            Manifest::parse(&abs)
                .unwrap()
                .archive_url("https://h/m.json"),
            "https://cdn/b.tar.zst"
        );
    }

    #[test]
    fn a_runtime_is_downloaded_verified_unpacked_and_becomes_current() {
        if !have_curl() {
            return;
        }
        let (site, root) = (tempdir("site"), tempdir("root"));
        let manifest = publish(&site, "wine-1_y", &[]);
        assert!(installed(&root).is_none());
        let steps = std::sync::Mutex::new(Vec::new());
        let got = install(&url(&manifest), &root, &|p| {
            let mut s = steps.lock().unwrap();
            let tag = match p {
                Progress::Manifest => "manifest",
                Progress::Downloading { .. } => "download",
                Progress::Verifying => "verify",
                Progress::Unpacking => "unpack",
            };
            if s.last() != Some(&tag) {
                s.push(tag);
            }
        })
        .unwrap();
        assert_eq!(got.id, "wine-1_y");
        assert_eq!(installed(&root), Some(got.clone()));
        assert_eq!(
            std::fs::read_link(root.join(CURRENT_LINK)).unwrap(),
            Path::new("wine-1_y")
        );
        assert!(root.join(CURRENT_LINK).join("wine/bin/wine").is_file());
        let steps = steps.lock().unwrap().clone();
        assert_eq!(steps.first(), Some(&"manifest"));
        assert_eq!(&steps[steps.len() - 2..], ["verify", "unpack"]);
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&got.wine).unwrap().permissions().mode();
        assert_eq!(mode & 0o111, 0o111, "programs stay executable");
        assert!(
            !root.join("download/wine-1_y.part").exists(),
            "the archive is cleaned up"
        );
        // Installing the same runtime again changes nothing and needs no download.
        std::fs::remove_file(site.join("wine-1_y.tar.zst")).unwrap();
        assert_eq!(
            install(&url(&manifest), &root, &|_| {}).unwrap().id,
            "wine-1_y"
        );
    }

    #[test]
    fn a_runtime_from_before_the_current_link_gets_one() {
        let root = tempdir("nolink");
        let id = "wine-0_y";
        std::fs::create_dir_all(root.join(id).join("wine/bin")).unwrap();
        std::fs::create_dir_all(root.join(id).join("yabridge")).unwrap();
        std::fs::write(root.join(id).join("wine/bin/wine"), b"x").unwrap();
        std::fs::write(
            root.join(id)
                .join("yabridge/libyabridge-chainloader-clap.so"),
            b"x",
        )
        .unwrap();
        assert!(
            ensure_current(&root).unwrap().is_none(),
            "no CURRENT file yet"
        );
        std::fs::write(root.join("CURRENT"), id).unwrap();
        assert_eq!(ensure_current(&root).unwrap().unwrap().id, id);
        assert!(root.join(CURRENT_LINK).join("yabridge").is_dir());
        // Again: nothing to do.
        assert!(ensure_current(&root).unwrap().is_some());
    }

    #[test]
    fn a_damaged_download_is_refused_and_the_old_runtime_stays() {
        if !have_curl() {
            return;
        }
        let (site, root) = (tempdir("badsite"), tempdir("badroot"));
        let manifest = publish(&site, "wine-1_y", &[]);
        install(&url(&manifest), &root, &|_| {}).unwrap();

        let site2 = tempdir("badsite2");
        let manifest2 = publish(&site2, "wine-2_y", &[]);
        // Same size, different bytes: the checksum has to catch it.
        let archive = site2.join("wine-2_y.tar.zst");
        let mut bytes = std::fs::read(&archive).unwrap();
        let mid = bytes.len() / 2;
        bytes[mid] ^= 0xff;
        std::fs::write(&archive, bytes).unwrap();
        let err = install(&url(&manifest2), &root, &|_| {})
            .unwrap_err()
            .to_string();
        assert!(err.contains("damaged"), "{err}");
        assert_eq!(
            installed(&root).unwrap().id,
            "wine-1_y",
            "the working runtime is untouched"
        );
        assert!(!root.join("wine-2_y").exists());
    }

    #[test]
    fn an_archive_without_wine_is_refused() {
        if !have_curl() {
            return;
        }
        let (site, root) = (tempdir("emptysite"), tempdir("emptyroot"));
        // Hand-made manifest for an archive that holds something else.
        let zst = ruzstd::encoding::compress_to_vec(
            &{
                let mut v = Vec::new();
                let mut b = tar::Builder::new(&mut v);
                let mut h = tar::Header::new_gnu();
                h.set_size(1);
                h.set_mode(0o644);
                h.set_cksum();
                b.append_data(&mut h, "readme", &b"x"[..]).unwrap();
                b.finish().unwrap();
                drop(b);
                v
            }[..],
            ruzstd::encoding::CompressionLevel::Fastest,
        );
        std::fs::write(site.join("x.tar.zst"), &zst).unwrap();
        let sha: String = Sha256::digest(&zst)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let m = Manifest {
            id: "wine-3".into(),
            file: "x.tar.zst".into(),
            sha256: sha,
            size: zst.len() as u64,
            unpacked_size: 1,
        };
        std::fs::write(site.join("m.json"), serde_json::to_string(&m).unwrap()).unwrap();
        let err = install(&url(&site.join("m.json")), &root, &|_| {})
            .unwrap_err()
            .to_string();
        assert!(err.contains("did not contain"), "{err}");
        assert!(installed(&root).is_none());
    }

    #[test]
    fn a_newer_runtime_replaces_the_old_one() {
        if !have_curl() {
            return;
        }
        let (site, root) = (tempdir("upsite"), tempdir("uproot"));
        let first = install(&url(&publish(&site, "wine-1_y", &[])), &root, &|_| {}).unwrap();
        first.set_default_prefix(Path::new("/some/prefix")).unwrap();
        let site2 = tempdir("upsite2");
        install(&url(&publish(&site2, "wine-2_y", &[])), &root, &|_| {}).unwrap();
        assert_eq!(installed(&root).unwrap().id, "wine-2_y");
        assert!(!root.join("wine-1_y").exists(), "old runtimes are removed");
        // The link bridges go through follows, and the prefix they use survives the update.
        assert_eq!(
            std::fs::read_link(root.join(CURRENT_LINK)).unwrap(),
            Path::new("wine-2_y")
        );
        assert_eq!(
            std::fs::read_to_string(
                Installed::stable_yabridge_dir(&root).join(DEFAULT_PREFIX_FILE)
            )
            .unwrap(),
            "/some/prefix\n"
        );
    }

    #[test]
    fn unpacking_refuses_paths_that_escape() {
        let dir = tempdir("escape");
        // A tar whose entry climbs out of the destination.
        let mut v = Vec::new();
        {
            let mut b = tar::Builder::new(&mut v);
            let mut h = tar::Header::new_gnu();
            let data = b"pwned";
            h.set_size(data.len() as u64);
            h.set_mode(0o644);
            let name = b"../escaped.txt";
            h.as_old_mut().name[..name.len()].copy_from_slice(name);
            h.set_cksum();
            b.append(&h, &data[..]).unwrap();
            b.finish().unwrap();
        }
        let zst =
            ruzstd::encoding::compress_to_vec(&v[..], ruzstd::encoding::CompressionLevel::Fastest);
        let archive = dir.join("a.tar.zst");
        std::fs::write(&archive, zst).unwrap();
        let dest = dir.join("out");
        std::fs::create_dir_all(&dest).unwrap();
        let _ = unpack(&archive, &dest);
        assert!(
            !dir.join("escaped.txt").exists(),
            "nothing may be written outside the destination"
        );
    }

    #[test]
    fn a_missing_manifest_says_what_failed() {
        if !have_curl() {
            return;
        }
        let err = install(
            "file:///nonexistent/runtime-manifest.json",
            &tempdir("none"),
            &|_| {},
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("cannot fetch"), "{err}");
    }
}
