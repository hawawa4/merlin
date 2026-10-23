//! `status`: what is installed and what is bridged, and what could surprise the user.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::Result;
use merlin_wine::runtime::{self, CURRENT_LINK, DEFAULT_PREFIX_FILE};

use crate::Ctx;

/// The plugin names (file stems, lower case) in `dir` with extension `ext`, a few folders deep.
/// A `.vst3` is a folder and is not searched further.
fn plugin_names(dir: &Path, ext: &str, depth: usize, out: &mut BTreeSet<String>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.extension().is_some_and(|x| x == ext) {
            if let Some(stem) = p.file_stem() {
                out.insert(stem.to_string_lossy().to_lowercase());
            }
        } else if p.is_dir() && depth < 4 {
            plugin_names(&p, ext, depth + 1, out);
        }
    }
}

/// What `status` says about one plugin format.
fn format_line(label: &str, dir: &Path, ext: &str) -> (String, BTreeSet<String>) {
    let mut names = BTreeSet::new();
    plugin_names(dir, ext, 0, &mut names);
    (
        format!("  {label:<5} {:>3} in {}", names.len(), dir.display()),
        names,
    )
}

/// `~/.clap/yabridge` and friends: where `yabridgectl` puts its bridges.
fn yabridgectl_dir(ours: &Path) -> Option<PathBuf> {
    ours.parent()
        .map(|p| p.join("yabridge"))
        .filter(|p| p.is_dir())
}

pub fn run(ctx: &Ctx) -> Result<()> {
    println!("Runtime");
    match runtime::installed(&ctx.runtime_root) {
        None => println!(
            "  not installed in {} (run `merlin-bridge runtime install`)",
            ctx.runtime_root.display()
        ),
        Some(rt) => {
            println!("  {}", rt.id);
            let version = Command::new(&rt.wine)
                .arg("--version")
                .output()
                .ok()
                .filter(|o| o.status.success())
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned());
            match version {
                Some(v) => println!("  Wine runs: {v}"),
                None => println!("  WARNING: {} does not run", rt.wine.display()),
            }
            let linked = std::fs::read_link(ctx.runtime_root.join(CURRENT_LINK))
                .is_ok_and(|t| t == Path::new(&rt.id));
            if !linked {
                println!("  the `current` link is missing; `sync` repairs it");
            }
            let want = format!("{}\n", ctx.prefix.display());
            let have = std::fs::read_to_string(rt.yabridge_dir.join(DEFAULT_PREFIX_FILE)).ok();
            if have.as_deref() != Some(want.as_str()) {
                println!("  plugins outside every prefix would use ~/.wine; `sync` fixes that");
            }
        }
    }

    println!("Wine prefix");
    let ready = ctx.prefix.join("system.reg").exists();
    println!(
        "  {} ({})",
        ctx.prefix.display(),
        if ready { "ready" } else { "not created yet" }
    );

    println!("Folders of Windows plugins");
    if ctx.config.folders.is_empty() {
        println!("  none (`merlin-bridge sync <folder>` adds one)");
    }
    for f in &ctx.config.folders {
        println!(
            "  {}{}",
            f.display(),
            if f.is_dir() { "" } else { "   (not there)" }
        );
    }

    println!("Bridges for other plugin hosts");
    let (clap, clap_names) = format_line("CLAP", &ctx.dirs.clap, "clap");
    let (vst3, vst3_names) = format_line("VST3", &ctx.dirs.vst3, "vst3");
    let (vst2, vst2_names) = format_line("VST2", &ctx.dirs.vst2, "so");
    println!("{clap}\n{vst3}\n{vst2}");

    // The same plugin bridged by `yabridgectl` shows up twice in a plugin host.
    let ours = [
        (&ctx.dirs.clap, "clap", &clap_names),
        (&ctx.dirs.vst3, "vst3", &vst3_names),
        (&ctx.dirs.vst2, "so", &vst2_names),
    ];
    for (dir, ext, names) in ours {
        let Some(theirs_dir) = yabridgectl_dir(dir) else {
            continue;
        };
        let mut theirs = BTreeSet::new();
        plugin_names(&theirs_dir, ext, 0, &mut theirs);
        let twice: Vec<_> = names.intersection(&theirs).cloned().collect();
        if !twice.is_empty() {
            println!(
                "WARNING: {} plugin(s) are also bridged by yabridgectl in {}, so a plugin host will list them twice: {}{}",
                twice.len(),
                theirs_dir.display(),
                twice.iter().take(5).cloned().collect::<Vec<_>>().join(", "),
                if twice.len() > 5 { ", ..." } else { "" }
            );
        }
    }
    Ok(())
}
