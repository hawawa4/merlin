//! Finding plugins and remembering what was found (plan §4.7).
//!
//! This is the scanning library: discovery of `.clap` files and `.vst3` bundles, a cache keyed by path + mtime +
//! size so only changed files are looked at again, and a loader that reads a file's plugin
//! descriptors. It runs the loader in-process for now; the child-process scanner with
//! timeouts and quarantine goes on top of the same cache (a file that crashed the scanner is
//! recorded like a failed load).

mod isolated;
mod quarantine;

pub use isolated::{RESULT_PREFIX, parse_child_output, run_isolated};
pub use quarantine::{Quarantine, Quarantined};

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use anyhow::{Context, Result};
use merlin_hosting::PluginInfo;
use merlin_hosting::clap::ClapLibrary;
use merlin_hosting::vst3::Vst3Library;
use serde::{Deserialize, Serialize};

/// How deep below a scan folder `.clap` files are looked for (vendors use subfolders).
const MAX_DEPTH: usize = 3;

/// One plugin file and what was found inside it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub path: PathBuf,
    /// Modification time, seconds since the epoch.
    pub mtime: u64,
    pub size: u64,
    /// A yabridge bridge to a Windows plugin.
    pub windows: bool,
    pub plugins: Vec<PluginInfo>,
    /// Why loading failed. Failed files are cached too, so they are not retried at every
    /// start; they are tried again when the file changes or on an explicit rescan.
    pub error: Option<String>,
}

/// The remembered scan results.
#[derive(Default, Serialize, Deserialize)]
pub struct Cache {
    entries: HashMap<PathBuf, Entry>,
}

impl Cache {
    /// Loads the cache; a missing or damaged file is an empty cache.
    pub fn load(path: &Path) -> Self {
        std::fs::read(path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        // Write beside the target and rename, so a crash never leaves half a file.
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self)?)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Forgets one file so the next scan loads it again (the "retry" button).
    pub fn forget(&mut self, path: &Path) {
        self.entries.remove(path);
    }
}

/// Where plugins usually live (CLAP and VST3).
pub fn default_plugin_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(home) = std::env::var_os("HOME") {
        dirs.push(Path::new(&home).join(".clap"));
        dirs.push(Path::new(&home).join(".vst3"));
    }
    dirs.extend(
        [
            "/usr/lib/clap",
            "/usr/lib64/clap",
            "/usr/local/lib/clap",
            "/usr/lib/vst3",
            "/usr/lib64/vst3",
            "/usr/local/lib/vst3",
        ]
        .into_iter()
        .map(PathBuf::from),
    );
    dirs
}

/// `.clap` files and `.vst3` bundles in the folders (and a few levels below), with whether each
/// is a yabridge bridge (CLAP only). Bundle directories are plugins, not folders to search.
pub fn find_clap_files(dirs: &[PathBuf]) -> Vec<(PathBuf, bool)> {
    fn walk(dir: &Path, depth: usize, out: &mut Vec<(PathBuf, bool)>) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        let mut paths: Vec<PathBuf> = rd.flatten().map(|e| e.path()).collect();
        paths.sort();
        for p in paths {
            if p.extension().is_some_and(|e| e == "clap") {
                // A chainloader named like the plugin, with a `.clap-win` link beside it.
                let windows = p.with_extension("clap-win").exists();
                out.push((p, windows));
            } else if p.extension().is_some_and(|e| e == "vst3") {
                out.push((p, false));
            } else if p.is_dir() && depth < MAX_DEPTH {
                walk(&p, depth + 1, out);
            }
        }
    }
    let mut out = Vec::new();
    for d in dirs {
        walk(d, 0, &mut out);
    }
    out.dedup_by(|a, b| a.0 == b.0);
    out
}

/// Reads a CLAP file's plugin descriptors by loading it. For a bridge this starts Wine, which
/// takes seconds.
pub fn load_clap(path: &Path, _windows: bool) -> Result<Vec<PluginInfo>> {
    ClapLibrary::load(path)
        .and_then(|lib| lib.plugins())
        .with_context(|| format!("scanning {}", path.display()))
}

/// Reads a VST3 bundle's audio classes by loading it.
pub fn load_vst3(path: &Path) -> Result<Vec<PluginInfo>> {
    Vst3Library::load(path)
        .map(|lib| lib.plugins())
        .with_context(|| format!("scanning {}", path.display()))
}

/// Reads whatever kind of plugin file this is.
pub fn load_any(path: &Path, windows: bool) -> Result<Vec<PluginInfo>> {
    if path.extension().is_some_and(|e| e == "vst3") {
        load_vst3(path)
    } else {
        load_clap(path, windows)
    }
}

fn stamp(path: &Path) -> Option<(u64, u64)> {
    let m = std::fs::metadata(path).ok()?;
    // For a bridge, the Windows plugin behind it is what changes; its link target's metadata
    // follows symlinks automatically via `metadata`, and the chainloader is the file itself.
    let mtime = m
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_secs();
    Some((mtime, m.len()))
}

/// Scans the folders, loading only files that are new or changed since the cache entry.
/// `progress` is called with each file about to be loaded (not for cache hits) and `done` with
/// each entry as soon as it is known (cache hits included, so a UI can fill up incrementally). Entries for
/// files that no longer exist are dropped from the cache.
pub fn scan_with(
    dirs: &[PathBuf],
    cache: &mut Cache,
    load: &dyn Fn(&Path, bool) -> Result<Vec<PluginInfo>>,
    mut progress: impl FnMut(&Path),
    mut done: impl FnMut(&Entry),
) -> Vec<Entry> {
    let files = find_clap_files(dirs);
    let mut result = Vec::with_capacity(files.len());
    let mut seen = std::collections::HashSet::new();
    for (path, windows) in files {
        let Some((mtime, size)) = stamp(&path) else {
            continue;
        };
        seen.insert(path.clone());
        if let Some(e) = cache.entries.get(&path)
            && e.mtime == mtime
            && e.size == size
            && e.windows == windows
        {
            done(e);
            result.push(e.clone());
            continue;
        }
        progress(&path);
        let (plugins, error) = match load(&path, windows) {
            Ok(p) => (p, None),
            Err(e) => (Vec::new(), Some(format!("{e:#}"))),
        };
        let entry = Entry {
            path: path.clone(),
            mtime,
            size,
            windows,
            plugins,
            error,
        };
        cache.entries.insert(path, entry.clone());
        done(&entry);
        result.push(entry);
    }
    // Drop vanished files, but only those under the folders we scanned.
    cache
        .entries
        .retain(|p, _| seen.contains(p) || !dirs.iter().any(|d| p.starts_with(d)));
    result
}

/// What the cache already knows about the plugin files in these folders, without loading
/// anything (stale or unknown files are left out).
pub fn cached_entries(dirs: &[PathBuf], cache: &Cache) -> Vec<Entry> {
    find_clap_files(dirs)
        .into_iter()
        .filter_map(|(path, windows)| {
            let (mtime, size) = stamp(&path)?;
            cache
                .entries
                .get(&path)
                .filter(|e| e.mtime == mtime && e.size == size && e.windows == windows)
                .cloned()
        })
        .collect()
}

/// [`scan_with`] using the real loaders.
pub fn scan(dirs: &[PathBuf], cache: &mut Cache, progress: impl FnMut(&Path)) -> Vec<Entry> {
    scan_with(dirs, cache, &load_any, progress, |_| {})
}

/// A plugin ready to be offered in the browser.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Found {
    pub path: PathBuf,
    pub windows: bool,
    pub info: PluginInfo,
}

/// Every plugin of the entries that has the given feature (`"instrument"`, `"audio-effect"`),
/// sorted by name.
pub fn with_feature(entries: &[Entry], feature: &str) -> Vec<Found> {
    let mut v: Vec<Found> = entries
        .iter()
        .flat_map(|e| {
            e.plugins
                .iter()
                .filter(|p| p.features.iter().any(|f| f == feature))
                .map(|p| Found {
                    path: e.path.clone(),
                    windows: e.windows,
                    info: p.clone(),
                })
        })
        .collect();
    v.sort_by(|a, b| {
        a.info
            .name
            .to_lowercase()
            .cmp(&b.info.name.to_lowercase())
            .then(a.windows.cmp(&b.windows))
    });
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use merlin_model::Format;
    use std::cell::RefCell;

    fn info(id: &str) -> PluginInfo {
        PluginInfo {
            format: Format::Clap,
            id: id.into(),
            name: id.to_uppercase(),
            vendor: "v".into(),
            version: "1".into(),
            features: vec!["instrument".into()],
        }
    }

    fn tempdir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "merlin-scanner-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn finds_plugins_bridges_and_nested_vendor_folders() {
        let d = tempdir("find");
        std::fs::write(d.join("a.clap"), b"x").unwrap();
        std::fs::write(d.join("b.clap"), b"x").unwrap();
        std::fs::write(d.join("b.clap-win"), b"x").unwrap();
        std::fs::create_dir_all(d.join("vendor/deeper")).unwrap();
        std::fs::write(d.join("vendor/c.clap"), b"x").unwrap();
        std::fs::write(d.join("vendor/deeper/d.clap"), b"x").unwrap();
        std::fs::write(d.join("notes.txt"), b"x").unwrap();
        // A bundle directory is a plugin, not a folder to search.
        std::fs::create_dir_all(d.join("bundle.clap/inner")).unwrap();
        std::fs::write(d.join("bundle.clap/inner/e.clap"), b"x").unwrap();
        std::fs::create_dir_all(d.join("Synth.vst3/Contents/x86_64-linux")).unwrap();
        std::fs::write(d.join("Synth.vst3/Contents/x86_64-linux/inner.clap"), b"x").unwrap();

        let files: Vec<(String, bool)> = find_clap_files(std::slice::from_ref(&d))
            .into_iter()
            .map(|(p, w)| {
                (
                    p.strip_prefix(&d).unwrap().to_string_lossy().into_owned(),
                    w,
                )
            })
            .collect();
        assert_eq!(
            files,
            vec![
                ("Synth.vst3".into(), false),
                ("a.clap".into(), false),
                ("b.clap".into(), true),
                ("bundle.clap".into(), false),
                ("vendor/c.clap".into(), false),
                ("vendor/deeper/d.clap".into(), false),
            ]
        );
    }

    #[test]
    fn only_changed_files_are_loaded_again() {
        let d = tempdir("cache");
        std::fs::write(d.join("good.clap"), b"1").unwrap();
        std::fs::write(d.join("bad.clap"), b"1").unwrap();
        let calls = RefCell::new(Vec::<String>::new());
        let loader = |p: &Path, _w: bool| -> Result<Vec<PluginInfo>> {
            let name = p.file_stem().unwrap().to_string_lossy().into_owned();
            calls.borrow_mut().push(name.clone());
            if name == "bad" {
                anyhow::bail!("crashed")
            }
            Ok(vec![info(&name)])
        };
        let dirs = [d.clone()];
        let mut cache = Cache::default();

        let first = scan_with(&dirs, &mut cache, &loader, |_| {}, |_| {});
        assert_eq!(calls.borrow().len(), 2);
        assert_eq!(first.len(), 2);
        let bad = first.iter().find(|e| e.path.ends_with("bad.clap")).unwrap();
        assert!(bad.error.as_deref().unwrap().contains("crashed"));
        assert!(bad.plugins.is_empty());

        // Nothing changed: nothing is loaded, failed files included.
        calls.borrow_mut().clear();
        let second = scan_with(&dirs, &mut cache, &loader, |_| {}, |_| {});
        assert!(calls.borrow().is_empty());
        assert_eq!(second, first);

        // One file changes: only it is loaded.
        std::fs::write(d.join("good.clap"), b"22").unwrap();
        scan_with(&dirs, &mut cache, &loader, |_| {}, |_| {});
        assert_eq!(*calls.borrow(), vec!["good".to_string()]);

        // Retry forgets one file.
        calls.borrow_mut().clear();
        cache.forget(&d.join("bad.clap"));
        scan_with(&dirs, &mut cache, &loader, |_| {}, |_| {});
        assert_eq!(*calls.borrow(), vec!["bad".to_string()]);

        // A removed file leaves the cache.
        std::fs::remove_file(d.join("bad.clap")).unwrap();
        let third = scan_with(&dirs, &mut cache, &loader, |_| {}, |_| {});
        assert_eq!(third.len(), 1);
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn the_cache_survives_a_restart_and_damage() {
        let d = tempdir("persist");
        std::fs::write(d.join("a.clap"), b"1").unwrap();
        let loader = |_: &Path, _| Ok(vec![info("a")]);
        let mut cache = Cache::default();
        let entries = scan_with(
            std::slice::from_ref(&d),
            &mut cache,
            &loader,
            |_| {},
            |_| {},
        );
        let file = d.join("cache/plugins.json");
        cache.save(&file).unwrap();

        let mut again = Cache::load(&file);
        let calls = RefCell::new(0);
        let counting = |_: &Path, _| {
            *calls.borrow_mut() += 1;
            Ok(vec![info("a")])
        };
        assert_eq!(
            scan_with(
                std::slice::from_ref(&d),
                &mut again,
                &counting,
                |_| {},
                |_| {}
            ),
            entries
        );
        assert_eq!(*calls.borrow(), 0, "restored from disk");

        std::fs::write(&file, b"{ not json").unwrap();
        assert!(
            Cache::load(&file).is_empty(),
            "a damaged cache is just empty"
        );
    }

    #[test]
    fn instruments_are_listed_by_name_with_windows_variants_after() {
        let mk = |id: &str, windows: bool, features: &[&str]| Entry {
            path: format!("/p/{id}.clap").into(),
            mtime: 0,
            size: 0,
            windows,
            plugins: vec![PluginInfo {
                features: features.iter().map(|s| s.to_string()).collect(),
                ..info(id)
            }],
            error: None,
        };
        let entries = [
            mk("surge", true, &["instrument"]),
            mk("surge", false, &["instrument"]),
            mk("reverb", false, &["audio-effect"]),
            mk("odin", false, &["instrument", "synthesizer"]),
        ];
        let names: Vec<(String, bool)> = with_feature(&entries, "instrument")
            .into_iter()
            .map(|f| (f.info.name, f.windows))
            .collect();
        assert_eq!(
            names,
            vec![
                ("ODIN".into(), false),
                ("SURGE".into(), false),
                ("SURGE".into(), true)
            ]
        );
        assert_eq!(with_feature(&entries, "audio-effect").len(), 1);
    }

    #[test]
    fn reads_real_plugin_descriptors() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../test-assets/dl/surge-linux/Surge XT.clap");
        if !path.exists() {
            eprintln!("skipped: Surge XT not installed");
            return;
        }
        let plugins = load_clap(&path, false).unwrap();
        assert_eq!(plugins[0].id, "org.surge-synth-team.surge-xt");
        assert!(plugins[0].is_instrument());
        assert!(load_clap(Path::new("/nonexistent.clap"), false).is_err());
    }

    #[test]
    fn reads_real_vst3_classes() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../test-assets/dl/surge-linux/Surge XT.vst3");
        if !path.exists() {
            eprintln!("skipped: Surge XT VST3 not installed");
            return;
        }
        let plugins = load_any(&path, false).unwrap();
        let synth = plugins.iter().find(|p| p.name == "Surge XT").unwrap();
        assert!(synth.is_instrument());
        assert_eq!(synth.format, Format::Vst3);
        assert!(load_any(Path::new("/nonexistent.vst3"), false).is_err());
    }
}
