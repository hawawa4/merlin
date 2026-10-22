//! The VST2 host against a tiny plugin built from C at test time (`tests/fixtures`).

use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;

use merlin_hosting::vst2::Vst2Library;
use merlin_hosting::{
    HostEvent, Io, MidiEvent, PluginHandle, ProcessStatus, Waker, host_time,
};

struct Nobody;
impl Waker for Nobody {
    fn wake(&self, _: HostEvent) {}
}

/// Builds the fixture plugin; `None` (and the test is skipped) without a C compiler.
fn build() -> Option<PathBuf> {
    let dir = std::env::temp_dir().join(format!("irig-vst2-fixture-{}", std::process::id()));
    std::fs::create_dir_all(&dir).ok()?;
    let out = dir.join("test_synth.so");
    let src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/test_synth_vst2.c");
    let status = Command::new("cc")
        .args(["-shared", "-fPIC", "-O1", "-o"])
        .arg(&out)
        .arg(&src)
        .arg("-lm")
        .status()
        .ok()?;
    status.success().then_some(out)
}

fn block(proc: &mut dyn merlin_hosting::PluginProcessor, events: &[MidiEvent]) -> Vec<f32> {
    let (mut l, mut r) = (vec![0.0; 512], vec![0.0; 512]);
    let status = proc.process(
        &mut Io {
            input: None,
            out_l: &mut l,
            out_r: &mut r,
        },
        events,
    );
    assert_eq!(status, ProcessStatus::Continue);
    assert_eq!(l, r);
    l
}

fn peak(v: &[f32]) -> f32 {
    v.iter().fold(0.0, |m, x| m.max(x.abs()))
}

#[test]
fn a_vst2_synth_is_found_played_and_saved() {
    let Some(path) = build() else {
        eprintln!("no C compiler: skipping");
        return;
    };
    let lib = Vst2Library::load(&path).unwrap();
    let found = lib.plugins().unwrap();
    assert_eq!(found.len(), 1);
    let info = &found[0];
    assert_eq!(info.name, "Test Synth");
    assert_eq!(info.vendor, "Merlin tests");
    assert_eq!(info.id, "54535931");
    assert!(info.features.iter().any(|f| f == "instrument"));

    let mut plugin = lib.create(&info.id, Arc::new(Nobody)).unwrap();
    let mut proc = plugin.activate(48_000.0, 512).unwrap();

    // Silent until a note is played.
    assert_eq!(peak(&block(proc.as_mut(), &[])), 0.0);
    let loud = block(proc.as_mut(), &[MidiEvent::note_on(0, 0, 69, 127)]);
    // Gain 0.5 of a full-velocity sine.
    assert!((peak(&loud) - 0.5).abs() < 0.02, "{}", peak(&loud));
    // 440 Hz at 48 kHz: about 4.7 periods in 512 frames.
    let crossings = loud
        .windows(2)
        .filter(|w| w[0] <= 0.0 && w[1] > 0.0)
        .count();
    assert!((4..=5).contains(&crossings), "{crossings}");

    // A parameter set from the main thread reaches the plugin at the next block.
    plugin.set_param(0, 0.25);
    let quieter = block(proc.as_mut(), &[]);
    assert!((peak(&quieter) - 0.25).abs() < 0.02, "{}", peak(&quieter));
    let params = plugin.params();
    assert_eq!(params.len(), 2);
    assert_eq!(params[0].name, "Gain");

    // The host's tempo reaches the plugin through getTime.
    host_time::set_tempo(150.0);
    block(proc.as_mut(), &[]);
    let after = plugin.params();
    assert!(
        (after[1].default - 0.5).abs() < 0.01,
        "{}",
        after[1].default
    );

    // State is the plugin's chunk, and loads back.
    let state = plugin.save_state().unwrap();
    plugin.set_param(0, 1.0);
    block(proc.as_mut(), &[]);
    plugin.load_state(&state).unwrap();
    let restored = block(proc.as_mut(), &[]);
    assert!((peak(&restored) - 0.25).abs() < 0.02, "{}", peak(&restored));
    assert!(plugin.load_state(b"junk").is_err());

    // The editor opens into a parent window and reports its size.
    let ed = plugin.editor().expect("the plugin has an editor");
    let size = ed.attach(1234, 1.0).unwrap();
    assert_eq!((size.width, size.height), (320, 200));
    assert!(ed.is_attached());
    ed.detach();
    assert!(!ed.is_attached());

    drop(proc);
    plugin.deactivate();
}

#[test]
fn a_file_that_is_not_a_vst2_plugin_is_refused() {
    let Some(path) = build() else { return };
    // Any other shared library has no VSTPluginMain.
    let libm = std::path::Path::new("/lib/x86_64-linux-gnu/libm.so.6");
    if libm.exists() {
        assert!(Vst2Library::load(libm).unwrap().plugins().is_err());
    }
    let _ = path;
}
