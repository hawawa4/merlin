//! Copies of rig files, kept so that nothing the app does can lose a rig: not a new version, not
//! a save over the wrong file, not an autosave over the last session.
//!
//! - `~/.local/share/merlin/backups/updates/<when> <old> to <new>/`: every saved rig and the
//!   autosave as they were when a new version of the app first started.
//! - `~/.local/share/merlin/backups/saves/<rig>/`: what a rig file held before a save replaced it.
//! - `~/.local/state/merlin/backups/`: the autosave as each start found it.
//!
//! Each folder keeps its newest few copies. Failing to make a copy never stops the app.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::paths;

/// Copies kept of each saved rig's earlier contents.
const KEEP_SAVES: usize = 10;
/// Copies kept of the autosave as each start found it.
const KEEP_AUTOSAVES: usize = 20;
/// Snapshots kept from version changes.
const KEEP_UPDATES: usize = 10;

/// `~/.local/share/merlin/backups`.
pub fn backups_dir() -> PathBuf {
    paths::data_dir().join("backups")
}

/// Where the autosave's copies go: beside it, `~/.local/state/merlin/backups`.
pub fn autosave_backups_dir() -> PathBuf {
    paths::state_dir().join("backups")
}

/// Run once at startup, before anything can write a rig.
pub fn on_start(version: &str) {
    let autosave = paths::autosave_file();
    let _ = keep_copy(
        &autosave,
        &autosave_backups_dir(),
        "autosave",
        KEEP_AUTOSAVES,
    );
    let _ = snapshot_on_new_version(&backups_dir(), version, &[paths::rigs_dir()], &[autosave]);
}

/// Keeps a copy of a rig file that a save is about to replace. Returns where it went.
pub fn before_overwrite(path: &Path) -> Option<PathBuf> {
    let stem = path.file_stem()?.to_string_lossy().into_owned();
    let dir = backups_dir().join("saves").join(&stem);
    keep_copy(path, &dir, &stem, KEEP_SAVES).ok().flatten()
}

/// Keeps `bytes` (a file's contents before something rewrote it) with the rig's other copies.
pub fn keep_bytes(path: &Path, bytes: &[u8], label: &str) -> Option<PathBuf> {
    let stem = path.file_stem()?.to_string_lossy().into_owned();
    let dir = backups_dir().join("saves").join(&stem);
    write_copy(&dir, &format!("{stem} {label}"), bytes, KEEP_SAVES).ok()
}

/// Copies `src` into `dir` as `<prefix> <date time>.mrig`, unless the newest copy there already
/// holds the same bytes, and keeps the newest `keep` copies with that prefix.
fn keep_copy(
    src: &Path,
    dir: &Path,
    prefix: &str,
    keep: usize,
) -> std::io::Result<Option<PathBuf>> {
    let bytes = match std::fs::read(src) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    if let Some(newest) = copies(dir, prefix).last()
        && std::fs::read(newest).is_ok_and(|b| b == bytes)
    {
        return Ok(None);
    }
    write_copy(dir, prefix, &bytes, keep).map(Some)
}

fn write_copy(dir: &Path, prefix: &str, bytes: &[u8], keep: usize) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let stamp = crate::session::stamp();
    let mut path = dir.join(format!("{prefix} {stamp}.{}", paths::RIG_EXT));
    for n in 2.. {
        if !path.exists() {
            break;
        }
        path = dir.join(format!("{prefix} {stamp} ({n}).{}", paths::RIG_EXT));
    }
    std::fs::write(&path, bytes)?;
    let all = copies(dir, prefix);
    for old in all.iter().take(all.len().saturating_sub(keep)) {
        let _ = std::fs::remove_file(old);
    }
    Ok(path)
}

/// The copies in `dir` made with `prefix`, oldest first.
fn copies(dir: &Path, prefix: &str) -> Vec<PathBuf> {
    let start = format!("{prefix} ");
    oldest_first(dir, |p| {
        p.is_file()
            && paths::is_rig_file(p)
            && p.file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with(&start))
    })
}

/// By modification time, then by name without the extension (so `x` comes before `x (2)`, made
/// in the same second).
fn oldest_first(dir: &Path, keep: impl Fn(&Path) -> bool) -> Vec<PathBuf> {
    let mut found: Vec<(SystemTime, String, PathBuf)> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| keep(p))
        .map(|p| {
            let when = std::fs::metadata(&p)
                .and_then(|m| m.modified())
                .unwrap_or(SystemTime::UNIX_EPOCH);
            let stem = p
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            (when, stem, p)
        })
        .collect();
    found.sort();
    found.into_iter().map(|(_, _, p)| p).collect()
}

/// When the app's version differs from the one that last ran (or none is recorded), copies every
/// rig in `rig_dirs` and each of `files` into `<root>/updates/<when> <old> to <new>/`. The
/// version is recorded only once the copy is made, so a failed copy is tried again next start.
fn snapshot_on_new_version(
    root: &Path,
    version: &str,
    rig_dirs: &[PathBuf],
    files: &[PathBuf],
) -> std::io::Result<Option<PathBuf>> {
    let mark = root.join("version");
    let last = std::fs::read_to_string(&mark).ok();
    let last = last.as_deref().map(str::trim);
    if last == Some(version) {
        return Ok(None);
    }
    let mut sources: Vec<PathBuf> = rig_dirs
        .iter()
        .flat_map(|d| std::fs::read_dir(d).into_iter().flatten().flatten())
        .map(|e| e.path())
        .filter(|p| p.is_file() && paths::is_rig_file(p))
        .collect();
    sources.extend(files.iter().filter(|p| p.is_file()).cloned());
    let mut made = None;
    if !sources.is_empty() {
        let updates = root.join("updates");
        let name = format!(
            "{} {} to {version}",
            crate::session::stamp(),
            last.unwrap_or("earlier")
        );
        let dir = updates.join(name.replace('/', "_"));
        std::fs::create_dir_all(&dir)?;
        for src in &sources {
            if let Some(name) = src.file_name() {
                std::fs::copy(src, dir.join(name))?;
            }
        }
        let all = oldest_first(&updates, Path::is_dir);
        for old in all.iter().take(all.len().saturating_sub(KEEP_UPDATES)) {
            let _ = std::fs::remove_dir_all(old);
        }
        made = Some(dir);
    }
    std::fs::create_dir_all(root)?;
    std::fs::write(&mark, version)?;
    Ok(made)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("merlin-backup-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn copies_skip_unchanged_contents_and_keep_the_newest() {
        let d = tmp("copies");
        let src = d.join("autosave.mrig");
        let out = d.join("out");
        assert_eq!(
            keep_copy(&src, &out, "autosave", 3).unwrap(),
            None,
            "no file"
        );
        std::fs::write(&src, "one").unwrap();
        assert!(keep_copy(&src, &out, "autosave", 3).unwrap().is_some());
        assert_eq!(
            keep_copy(&src, &out, "autosave", 3).unwrap(),
            None,
            "the same again"
        );
        for text in ["two", "three", "four", "five"] {
            std::fs::write(&src, text).unwrap();
            keep_copy(&src, &out, "autosave", 3).unwrap().unwrap();
        }
        let kept: Vec<String> = copies(&out, "autosave")
            .iter()
            .map(|p| std::fs::read_to_string(p).unwrap())
            .collect();
        assert_eq!(kept, ["three", "four", "five"]);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_new_version_snapshots_every_rig_once() {
        let d = tmp("versions");
        let rigs = d.join("rigs");
        std::fs::create_dir_all(&rigs).unwrap();
        std::fs::write(rigs.join("Gig.mrig"), "gig").unwrap();
        std::fs::write(rigs.join("Old.irig"), "old").unwrap();
        std::fs::write(rigs.join("notes.txt"), "not a rig").unwrap();
        let autosave = d.join("autosave.mrig");
        std::fs::write(&autosave, "auto").unwrap();
        let root = d.join("backups");
        let snap = |v: &str| {
            snapshot_on_new_version(
                &root,
                v,
                std::slice::from_ref(&rigs),
                std::slice::from_ref(&autosave),
            )
            .unwrap()
        };

        let first = snap("0.13.1").expect("no version recorded yet: snapshot");
        assert!(
            first
                .file_name()
                .unwrap()
                .to_string_lossy()
                .ends_with("earlier to 0.13.1")
        );
        assert_eq!(
            std::fs::read_to_string(first.join("Gig.mrig")).unwrap(),
            "gig"
        );
        assert_eq!(
            std::fs::read_to_string(first.join("Old.irig")).unwrap(),
            "old"
        );
        assert_eq!(
            std::fs::read_to_string(first.join("autosave.mrig")).unwrap(),
            "auto"
        );
        assert!(!first.join("notes.txt").exists());

        assert_eq!(snap("0.13.1"), None, "same version: nothing to do");
        let second = snap("0.14.0").expect("new version");
        assert!(
            second
                .file_name()
                .unwrap()
                .to_string_lossy()
                .ends_with("0.13.1 to 0.14.0")
        );
        assert!(second.join("Gig.mrig").exists());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn nothing_to_copy_still_records_the_version() {
        let d = tmp("empty");
        let root = d.join("backups");
        assert_eq!(
            snapshot_on_new_version(&root, "1.0.0", &[d.join("none")], &[]).unwrap(),
            None
        );
        assert_eq!(
            std::fs::read_to_string(root.join("version")).unwrap(),
            "1.0.0"
        );
        let _ = std::fs::remove_dir_all(&d);
    }
}
