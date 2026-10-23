//! `sync`: makes the bridges other plugin hosts load, and tidies the ones that are not wanted.

use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow};
use merlin_wine::runtime::{self, CURRENT_LINK, Installed};
use merlin_wine::{Placement, Runtime, SyncReport};

use crate::Ctx;

/// The folder (below each format's bridge folder) for the managed prefix's plugins.
pub const PREFIX_TAG: &str = "wine-prefix";

/// What a sync did, over every source and format.
#[derive(Debug, Default)]
pub struct Totals {
    pub created: usize,
    pub unchanged: usize,
    pub removed: usize,
    /// Sources (folders) whose bridges were removed because nobody asks for them any more.
    pub pruned: Vec<String>,
    pub skipped: Vec<(PathBuf, String)>,
    pub problems: Vec<String>,
    pub notes: Vec<String>,
}

impl Totals {
    fn add(&mut self, reports: Vec<Result<SyncReport>>) {
        for r in reports {
            match r {
                Ok(r) => {
                    self.created += r.created.len();
                    self.unchanged += r.unchanged;
                    self.removed += r.removed.len();
                    self.skipped.extend(r.skipped);
                }
                Err(e) => self.problems.push(format!("{e:#}")),
            }
        }
    }

    pub fn bridges(&self) -> usize {
        self.created + self.unchanged
    }
}

/// A short folder name for a source folder: its name plus a few letters of a hash of its path,
/// so two folders called `VST` do not meet.
pub fn folder_tag(folder: &Path) -> String {
    let name: String = folder
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "plugins".into())
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || "-_.".contains(c) {
                c
            } else {
                '_'
            }
        })
        .collect();
    // FNV-1a: stable across runs and Rust versions, unlike `DefaultHasher`.
    let mut h: u32 = 0x811c_9dc5;
    for b in folder.as_os_str().as_encoded_bytes() {
        h = (h ^ u32::from(*b)).wrapping_mul(0x0100_0193);
    }
    format!("{}-{:04x}", name.trim_matches('.'), h & 0xffff)
}

/// The Wine prefix a path lives in, if it is inside one (a folder with `dosdevices` above it).
pub fn prefix_containing(path: &Path) -> Option<PathBuf> {
    path.ancestors()
        .skip(1)
        .find(|a| a.join("dosdevices").is_dir() && a.join("drive_c").is_dir())
        .map(Path::to_path_buf)
}

/// The runtime to make bridges for. Bridges link through `<runtime root>/current`, so a runtime
/// update reaches them without another sync.
pub fn runtime_for(ctx: &Ctx) -> Result<(Installed, Runtime)> {
    let root = &ctx.runtime_root;
    let installed = runtime::ensure_current(root)?.ok_or_else(|| {
        anyhow!(
            "Wine and yabridge are not installed in {}. Run `merlin-bridge runtime install` first.",
            root.display()
        )
    })?;
    let rt = Runtime {
        wine: root.join(CURRENT_LINK).join("wine/bin/wine"),
        yabridge_dir: Installed::stable_yabridge_dir(root),
        prefix: ctx.prefix.clone(),
    };
    Ok((installed, rt))
}

pub fn run(ctx: &Ctx) -> Result<Totals> {
    let (installed, rt) = runtime_for(ctx)?;
    installed.set_default_prefix(&ctx.prefix)?;
    let dirs = &ctx.dirs;
    let mut totals = Totals::default();
    let mut wanted: Vec<String> = Vec::new();

    if ctx.prefix.join("drive_c").is_dir() {
        wanted.push(PREFIX_TAG.to_owned());
        totals.add(vec![
            rt.sync_clap_bridges_with(&dirs.clap.join(PREFIX_TAG), Placement::Link),
            rt.sync_vst3_bridges_with(&dirs.vst3.join(PREFIX_TAG), Placement::Link),
            rt.sync_vst2_bridges_with(&[], &dirs.vst2.join(PREFIX_TAG), Placement::Link),
        ]);
    }

    for folder in &ctx.config.folders {
        let tag = folder_tag(folder);
        wanted.push(tag.clone());
        if !folder.is_dir() {
            totals.notes.push(format!(
                "{} is not there (a disk that is not mounted?); its bridges are kept",
                folder.display()
            ));
            continue;
        }
        if let Some(p) = prefix_containing(folder).filter(|p| *p != ctx.prefix) {
            totals.notes.push(format!(
                "{} is inside the Wine prefix {}: its plugins will run there, with our Wine",
                folder.display(),
                p.display()
            ));
        }
        let one = std::slice::from_ref(folder);
        totals.add(vec![
            merlin_wine::sync_clap_bridges_with(
                one,
                &rt.chainloader(),
                &dirs.clap.join(&tag),
                Placement::Link,
            ),
            merlin_wine::sync_vst3_bridges_with(
                one,
                &rt.vst3_chainloader(),
                &dirs.vst3.join(&tag),
                Placement::Link,
            ),
            merlin_wine::sync_vst2_bridges_with(
                one,
                &rt.vst2_chainloader(),
                &dirs.vst2.join(&tag),
                Placement::Link,
            ),
        ]);
    }

    for root in dirs.all() {
        prune(root, &wanted, &mut totals);
    }
    Ok(totals)
}

/// Removes the sources below `root` that are no longer wanted (a forgotten folder, a prefix that
/// is gone). Only folders made of links are touched: anything else in there is not ours.
fn prune(root: &Path, wanted: &[String], totals: &mut Totals) {
    let Ok(rd) = std::fs::read_dir(root) else {
        return;
    };
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if wanted.contains(&name) || !e.path().is_dir() {
            continue;
        }
        if only_links(&e.path()) {
            if std::fs::remove_dir_all(e.path()).is_ok() {
                totals.pruned.push(name);
            }
        } else {
            totals.notes.push(format!(
                "{} holds files that are not bridges; left alone",
                e.path().display()
            ));
        }
    }
}

fn only_links(dir: &Path) -> bool {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return false;
    };
    rd.flatten().all(|e| match e.file_type() {
        Ok(t) if t.is_symlink() => true,
        Ok(t) if t.is_dir() => only_links(&e.path()),
        _ => false,
    })
}
