//! The LV2 host against a real plugin, when one is at hand (Surge XT's LV2 from the test
//! assets). Skipped where it is not.

use std::path::PathBuf;
use std::sync::Arc;

use merlin_hosting::lv2::Lv2Library;
use merlin_hosting::{HostEvent, Io, MidiEvent, PluginHandle, Waker};

struct Nobody;
impl Waker for Nobody {
    fn wake(&self, _: HostEvent) {}
}

fn surge() -> Option<PathBuf> {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../test-assets/dl/surge-linux/Surge XT.lv2");
    p.join("manifest.ttl").exists().then_some(p)
}

fn render(
    proc: &mut dyn merlin_hosting::PluginProcessor,
    events: &[MidiEvent],
) -> Vec<f32> {
    let (mut l, mut r) = (vec![0.0; 128], vec![0.0; 128]);
    proc.process(
        &mut Io {
            input: None,
            out_l: &mut l,
            out_r: &mut r,
        },
        events,
    );
    l
}

#[test]
fn surge_xt_lv2_is_found_plays_and_saves_its_state() {
    let Some(bundle) = surge() else {
        eprintln!("no Surge XT LV2 in the test assets: skipping");
        return;
    };
    let lib = Lv2Library::open(&bundle).unwrap();
    let found = lib.plugins();
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].name, "Surge XT");
    assert!(found[0].features.iter().any(|f| f == "instrument"));

    let mut plugin = lib
        .create(&found[0].id, Arc::new(Nobody), 48_000.0, 512)
        .unwrap();
    let mut proc = plugin.activate(48_000.0, 512).unwrap();
    // Silence first, then sound after a note.
    for _ in 0..10 {
        render(proc.as_mut(), &[]);
    }
    let mut peak = 0f32;
    let mut first = true;
    for _ in 0..200 {
        let events = if first {
            vec![MidiEvent::note_on(0, 0, 60, 100)]
        } else {
            vec![]
        };
        first = false;
        for s in render(proc.as_mut(), &events) {
            peak = peak.max(s.abs());
        }
    }
    assert!(peak > 0.01, "Surge XT made no sound: {peak}");

    // The plugin's own state goes out and comes back.
    let state = plugin.save_state().unwrap();
    assert!(state.len() > 100, "{}", state.len());
    plugin.load_state(&state).unwrap();
    assert!(plugin.load_state(b"junk").is_err());
    drop(proc);
    plugin.deactivate();
}

/// Builds the fixture bundle (TTL files plus a C plugin compiled now); `None` without a compiler.
fn fixture() -> Option<PathBuf> {
    let src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let dir = std::env::temp_dir()
        .join(format!("irig-lv2-fixture-{}", std::process::id()))
        .join("test_lv2.lv2");
    std::fs::create_dir_all(&dir).ok()?;
    for f in ["manifest.ttl", "plugin.ttl"] {
        std::fs::copy(src.join("test_lv2.lv2").join(f), dir.join(f)).ok()?;
    }
    let ok = std::process::Command::new("cc")
        .args(["-shared", "-fPIC", "-O1", "-o"])
        .arg(dir.join("test_lv2.so"))
        .arg(src.join("test_lv2.c"))
        .arg("-lm")
        .status()
        .ok()?
        .success();
    ok.then_some(dir)
}

#[test]
fn a_c_lv2_plugin_gets_midi_a_worker_and_state() {
    let Some(bundle) = fixture() else {
        eprintln!("no C compiler: skipping");
        return;
    };
    let lib = Lv2Library::open(&bundle).unwrap();
    let found = lib.plugins();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].name, "Test LV2 Synth");
    assert_eq!(found[0].id, "urn:merlin:test-lv2");
    let mut plugin = lib
        .create(&found[0].id, Arc::new(Nobody), 48_000.0, 512)
        .unwrap();
    // Only the control input is a parameter.
    let params = plugin.params();
    assert_eq!(params.len(), 1);
    assert_eq!(
        (
            params[0].name.as_str(),
            params[0].min,
            params[0].max,
            params[0].default
        ),
        ("Gain", 0.0, 1.0, 0.5)
    );
    let mut proc = plugin.activate(48_000.0, 512).unwrap();

    let quiet = render(proc.as_mut(), &[]);
    assert!(quiet.iter().all(|&x| x == 0.0));
    let loud = render(proc.as_mut(), &[MidiEvent::note_on(0, 0, 69, 127)]);
    let peak = |v: &[f32]| v.iter().fold(0f32, |m, x| m.max(x.abs()));
    assert!((peak(&loud) - 0.5).abs() < 0.02, "{}", peak(&loud));

    // A parameter set from the main thread reaches the plugin's control port.
    plugin.set_param(3, 0.25);
    let softer = render(proc.as_mut(), &[]);
    assert!((peak(&softer) - 0.25).abs() < 0.02, "{}", peak(&softer));
    plugin.set_param(3, 5.0); // clamped to the port's range
    let max = render(proc.as_mut(), &[]);
    assert!((peak(&max) - 1.0).abs() < 0.02, "{}", peak(&max));

    // The worker did its job off the audio thread and the answer came back into a later run:
    // the plugin shows it on an output port (21 * 2).
    let mut worked = 0.0;
    for _ in 0..500 {
        render(proc.as_mut(), &[]);
        worked = plugin.port_value(4);
        if worked != 0.0 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    assert_eq!(worked, 42.0, "the worker's answer never arrived");
    assert_eq!(plugin.port_value(5), 1234.0);

    // State: the plugin's property goes out; restoring changes it back.
    let state = plugin.save_state().unwrap();
    plugin.load_state(&state).unwrap();
    drop(proc);
    plugin.deactivate();
}
