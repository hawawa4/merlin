//! `merlin-bridge`: makes Windows plugins loadable by any Linux plugin host (REAPER,
//! Bitwig, Ardour, ...), with the Wine and yabridge the app ships (docs/wine.md).
//!
//! It is `yabridgectl` with the runtime included. `runtime install` downloads Wine and yabridge,
//! `sync <folder>` makes a bridge for every 64-bit Windows CLAP, VST3 and VST2 plugin in a folder
//! (and in our Wine prefix) in `~/.clap`, `~/.vst3` and `~/.vst` below `merlin-bridges`.
//! The plugins themselves are never touched; a bridge is a link.

mod config;
mod status;
mod sync;

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use config::Config;
use merlin_wine::layout::{self, DawBridgeDirs};
use merlin_wine::runtime::{self, Progress};

const USAGE: &str = "\
merlin-bridge: load Windows plugins in any Linux plugin host

USAGE
    merlin-bridge [OPTIONS] <COMMAND>

COMMANDS
    runtime install [--url URL]   Download Wine and yabridge (about 40 MB)
    sync [FOLDER...]              Bridge the Windows plugins in the Wine prefix and in the folders
                                  (FOLDERs are remembered; plain `sync` repeats them all)
    forget FOLDER...              Stop bridging a folder and remove its bridges
    install FILE                  Run a Windows installer (.exe, .msi) in the prefix, then sync
    status                        What is installed and bridged, and what could be a problem
    clean                         Remove every bridge this tool made (plugins are not touched)

OPTIONS
    --prefix DIR          The Wine prefix to use (remembered). Default: the one the app uses
    --runtime-root DIR    Where the runtime is kept. Default: the one the app uses
    -h, --help

The bridges are in ~/.clap/merlin-bridges, ~/.vst3/merlin-bridges and
~/.vst/merlin-bridges. Rescan plugins in your host after a sync. Run `sync` again when
you install or remove Windows plugins; a runtime update needs no new sync.
";

/// Everything a command needs.
pub struct Ctx {
    pub runtime_root: PathBuf,
    pub prefix: PathBuf,
    pub dirs: DawBridgeDirs,
    pub config_file: PathBuf,
    pub config: Config,
}

fn main() -> ExitCode {
    match run(std::env::args().skip(1).collect()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("merlin-bridge: {e:#}");
            ExitCode::FAILURE
        }
    }
}

/// Takes `--name VALUE` out of `args`.
fn take_option(args: &mut Vec<String>, name: &str) -> Result<Option<String>> {
    let Some(i) = args.iter().position(|a| a == name) else {
        return Ok(None);
    };
    args.remove(i);
    if i >= args.len() {
        bail!("{name} needs a value");
    }
    Ok(Some(args.remove(i)))
}

fn run(mut args: Vec<String>) -> Result<()> {
    if args.is_empty() || args.iter().any(|a| a == "-h" || a == "--help") {
        print!("{USAGE}");
        return Ok(());
    }
    let prefix_arg = take_option(&mut args, "--prefix")?.map(PathBuf::from);
    let runtime_root = take_option(&mut args, "--runtime-root")?
        .map(PathBuf::from)
        .unwrap_or_else(layout::runtime_dir);
    let config_file = layout::config_dir().join("bridge.json");
    let mut config = Config::load(&config_file)?;
    // `--prefix` is remembered, but only by the commands that make something with it.
    if let Some(p) = prefix_arg {
        config.prefix = Some(config::absolute(&p)?);
        if matches!(
            args.first().map(String::as_str),
            Some("sync" | "install" | "forget" | "runtime")
        ) {
            config.save(&config_file)?;
        }
    }
    let ctx = Ctx {
        runtime_root: config::absolute(&runtime_root)?,
        prefix: config.prefix.clone().unwrap_or_else(layout::wine_prefix),
        dirs: DawBridgeDirs::standard(),
        config_file,
        config,
    };

    let (command, rest) = args.split_first().ok_or_else(|| anyhow!("no command"))?;
    match command.as_str() {
        "runtime" => match rest {
            [sub, rest @ ..] if sub == "install" => runtime_install(&ctx, rest.to_vec()),
            _ => bail!("usage: merlin-bridge runtime install [--url URL]"),
        },
        "sync" => sync_command(ctx, rest),
        "forget" => forget(ctx, rest),
        "install" => install(ctx, rest),
        "status" => status::run(&ctx),
        "clean" => clean(&ctx),
        other => bail!("unknown command `{other}` (try --help)"),
    }
}

fn runtime_install(ctx: &Ctx, mut rest: Vec<String>) -> Result<()> {
    let url =
        take_option(&mut rest, "--url")?.unwrap_or_else(|| layout::DEFAULT_RUNTIME_URL.to_owned());
    if let Some(extra) = rest.first() {
        bail!("unexpected `{extra}`");
    }
    std::fs::create_dir_all(&ctx.runtime_root)?;
    let last = std::cell::Cell::new(0u64);
    let installed = runtime::install(&url, &ctx.runtime_root, &|p| match p {
        Progress::Manifest => eprintln!("contacting the download server..."),
        Progress::Downloading { done, total } => {
            let mb = done >> 20;
            if mb != last.get() {
                last.set(mb);
                eprint!("\rdownloading: {mb} of {} MB   ", total >> 20);
            }
        }
        Progress::Verifying => eprintln!("\nchecking the download..."),
        Progress::Unpacking => eprintln!("unpacking..."),
    })?;
    installed.set_default_prefix(&ctx.prefix)?;
    println!("Installed {}.", installed.id);
    println!("Next: `merlin-bridge sync <folder of Windows plugins>`.");
    Ok(())
}

fn print_totals(ctx: &Ctx, t: &sync::Totals) {
    println!(
        "{} bridge(s) ready ({} new or repaired, {} removed).",
        t.bridges(),
        t.created,
        t.removed
    );
    for name in &t.pruned {
        println!("Removed the bridges of {name}.");
    }
    for (path, why) in &t.skipped {
        println!("Skipped {}: {why}", path.display());
    }
    for note in &t.notes {
        println!("Note: {note}");
    }
    for problem in &t.problems {
        eprintln!("Problem: {problem}");
    }
    if t.bridges() > 0 {
        println!("Bridges are in:");
        for d in ctx.dirs.all() {
            println!("  {}", d.display());
        }
        println!("Rescan plugins in your plugin host to see them.");
    }
}

fn sync_command(mut ctx: Ctx, folders: &[String]) -> Result<()> {
    let mut changed = false;
    for f in folders {
        let folder = config::absolute(&PathBuf::from(f))?;
        if !folder.is_dir() {
            bail!("{} is not a folder", folder.display());
        }
        changed |= ctx.config.add_folder(folder);
    }
    let totals = sync::run(&ctx)?;
    // Remembered only once the sync worked.
    if changed {
        ctx.config.save(&ctx.config_file)?;
    }
    print_totals(&ctx, &totals);
    if totals.bridges() == 0 && totals.problems.is_empty() {
        println!(
            "No Windows plugins found. Name a folder (`sync ~/Plugins/Windows`) or install some (`install setup.exe`)."
        );
    }
    if totals.problems.is_empty() {
        Ok(())
    } else {
        Err(anyhow!("{} problem(s) above", totals.problems.len()))
    }
}

fn forget(mut ctx: Ctx, folders: &[String]) -> Result<()> {
    if folders.is_empty() {
        bail!("usage: merlin-bridge forget FOLDER...");
    }
    for f in folders {
        let folder = config::absolute(&PathBuf::from(f))?;
        let before = ctx.config.folders.len();
        ctx.config.folders.retain(|x| *x != folder);
        if ctx.config.folders.len() == before {
            bail!(
                "{} is not one of the folders (see `status`)",
                folder.display()
            );
        }
    }
    ctx.config.save(&ctx.config_file)?;
    let totals = sync::run(&ctx)?;
    print_totals(&ctx, &totals);
    Ok(())
}

fn install(ctx: Ctx, rest: &[String]) -> Result<()> {
    let [file] = rest else {
        bail!("usage: merlin-bridge install FILE.exe|FILE.msi");
    };
    let (_, rt) = sync::runtime_for(&ctx)?;
    println!("Preparing the Wine prefix (the first time takes a minute)...");
    rt.ensure_prefix(Duration::from_secs(300))?;
    let file = config::absolute(&PathBuf::from(file))?;
    let mut cmd = rt.installer_command(&file)?;
    println!("Running the installer. Finish it in its window.");
    let status = cmd
        .status()
        .with_context(|| format!("cannot run {}", file.display()))?;
    if !status.success() {
        eprintln!("The installer exited with {status}; looking for plugins anyway.");
    }
    let totals = sync::run(&ctx)?;
    print_totals(&ctx, &totals);
    Ok(())
}

fn clean(ctx: &Ctx) -> Result<()> {
    for d in ctx.dirs.all() {
        if d.exists() {
            std::fs::remove_dir_all(d).with_context(|| format!("cannot remove {}", d.display()))?;
            println!("Removed {}", d.display());
        }
    }
    println!("Folders and the runtime are kept; `sync` makes the bridges again.");
    Ok(())
}
