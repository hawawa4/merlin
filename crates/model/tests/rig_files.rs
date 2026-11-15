//! Rig files saved by released versions must keep opening. Each `tests/rigs/formatN.mrig` is a
//! frozen file of format N (a real rig with its plugin states shortened). Never edit one: a change
//! to the model that breaks it needs a migration step (`crates/model/src/migrate.rs`), and a new
//! format version adds its own fixture.

use std::path::{Path, PathBuf};

use merlin_model::{RIG_VERSION, Rig};

fn fixture(version: u32) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("tests/rigs/format{version}.mrig"))
}

fn load(version: u32) -> Rig {
    let path = fixture(version);
    let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let (rig, migrated) =
        Rig::from_json(&bytes).unwrap_or_else(|e| panic!("format {version} no longer opens: {e}"));
    assert_eq!(migrated, version != RIG_VERSION, "format {version}");
    rig
}

#[test]
fn every_format_has_a_fixture() {
    for v in 1..=RIG_VERSION {
        assert!(
            fixture(v).is_file(),
            "add tests/rigs/format{v}.mrig: a rig saved by format {v}, to keep it opening"
        );
    }
}

#[test]
fn format_1_keeps_what_it_held() {
    let rig = load(1);
    assert_eq!(rig.version, RIG_VERSION);
    assert_eq!(rig.name, "format 1 fixture");
    assert_eq!(rig.slots.len(), 8);
    let instruments = rig.slots.iter().filter(|s| s.instrument.is_some()).count();
    assert_eq!(instruments, 6);
    assert!(
        rig.slots
            .iter()
            .filter(|s| s.instrument.is_some())
            .all(|s| s.state.as_deref() == Some(b"plugin state".as_slice())),
        "plugin states survive"
    );
    assert_eq!(rig.slots.iter().map(|s| s.macros.len()).sum::<usize>(), 2);
    assert_eq!(rig.mappings.len(), 1);
    assert_eq!(rig.snapshots.len(), 1);
    assert_eq!(rig.songs.len(), 1);
    assert_eq!(rig.selected.len(), 1);
}

#[test]
fn every_fixture_survives_a_save_and_reload() {
    for v in 1..=RIG_VERSION {
        let rig = load(v);
        let bytes = serde_json::to_vec_pretty(&rig).unwrap();
        let (again, migrated) = Rig::from_json(&bytes).unwrap();
        assert!(!migrated);
        assert_eq!(again, rig, "format {v}");
    }
}
