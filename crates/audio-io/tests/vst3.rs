//! Hosts a real VST3 plugin (Surge XT) through the hosting traits and renders it with the
//! engine. Skipped when the test plugin is not installed (`just setup` downloads it).

use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, mpsc};

use merlin_audio_io::{Item, OfflineRenderer};
use merlin_engine::{Command, SelectMode};
use merlin_hosting::vst3::Vst3Library;
use merlin_hosting::{HostEvent, MidiEvent, PluginHandle, Waker};

const SURGE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../test-assets/dl/surge-linux/Surge XT.vst3"
);

struct ChannelWaker(Mutex<mpsc::Sender<HostEvent>>);

impl Waker for ChannelWaker {
    fn wake(&self, event: HostEvent) {
        let _ = self.0.lock().unwrap().send(event);
    }
}

fn surge() -> Option<(Vst3Library, Arc<ChannelWaker>, mpsc::Receiver<HostEvent>)> {
    let path = PathBuf::from(SURGE);
    if !path.exists() {
        eprintln!("skipped: {SURGE} not installed");
        return None;
    }
    let (tx, rx) = mpsc::channel();
    Some((
        Vst3Library::load(&path).expect("load Surge VST3"),
        Arc::new(ChannelWaker(Mutex::new(tx))),
        rx,
    ))
}

fn pump(plugin: &mut dyn PluginHandle, rx: &mpsc::Receiver<HostEvent>) {
    while let Ok(ev) = rx.try_recv() {
        if ev == HostEvent::MainThreadCallback {
            plugin.on_main_thread();
        }
    }
    plugin.service(std::time::Instant::now());
}

fn on(key: u8) -> Item {
    Item::Midi(MidiEvent::note_on(0, 0, key, 110))
}

fn instrument_id(lib: &Vst3Library) -> String {
    let plugins = lib.plugins();
    plugins
        .iter()
        .find(|p| p.name == "Surge XT")
        .unwrap_or_else(|| {
            panic!(
                "Surge XT not among {:?}",
                plugins.iter().map(|p| &p.name).collect::<Vec<_>>()
            )
        })
        .id
        .clone()
}

#[test]
fn lists_plugins_with_features() {
    let Some((lib, ..)) = surge() else { return };
    let plugins = lib.plugins();
    let synth = plugins
        .iter()
        .find(|p| p.name == "Surge XT")
        .expect("Surge XT");
    assert!(synth.is_instrument(), "features {:?}", synth.features);
    assert_eq!(synth.id.len(), 32);
    // The effects plugin is in the same bundle and is not an instrument.
    if let Some(fx) = plugins.iter().find(|p| p.name.contains("Effects")) {
        assert!(!fx.is_instrument(), "features {:?}", fx.features);
    }
}

#[test]
fn plays_through_the_engine_and_follows_parameters() {
    let Some((lib, waker, rx)) = surge() else {
        return;
    };
    let mut plugin = lib.create(&instrument_id(&lib), waker).expect("create");
    pump(&mut plugin, &rx);
    let params = plugin.params();
    assert!(
        params.len() > 100,
        "Surge exposes many parameters, got {}",
        params.len()
    );
    let volume = params
        .iter()
        .find(|p| p.name == "Global Volume" && p.automatable)
        .unwrap_or_else(|| {
            panic!(
                "no Global Volume in {:?}",
                params.iter().map(|p| &p.name).take(20).collect::<Vec<_>>()
            )
        });

    let sr = 48000.0;
    let mut r = OfflineRenderer::new(sr, 128);
    let processor = plugin.activate(sr as f64, 128).expect("activate");
    r.handle()
        .send(Command::SetProcessor { slot: 0, processor })
        .ok();
    r.handle()
        .send(Command::Select {
            slot: 0,
            mode: SelectMode::Exclusive,
        })
        .ok();
    pump(&mut plugin, &rx);
    r.render(128 * 4, vec![]);

    let heard = r.render(48000, vec![(0, on(60)), (0, on(64)), (0, on(67))]);
    let loud = heard.peak(8000..48000);
    assert!(loud > 0.02, "Surge makes sound: {loud}");
    assert_eq!(r.handle().shared.held[60].load(Ordering::Relaxed), 1);

    // Turning the volume down from the main thread silences the held chord.
    plugin.set_param(volume.id, 0.0);
    pump(&mut plugin, &rx);
    let quiet = r.render(24000, vec![]);
    assert!(
        quiet.peak(12000..24000) < loud * 0.1,
        "volume applied: {} vs {loud}",
        quiet.peak(12000..24000)
    );

    // Clean shutdown: panic, retire the processor, deactivate, and again after reactivation.
    r.handle().send(Command::Panic).ok();
    r.render(4800, vec![]);
    r.handle().send(Command::ClearProcessor { slot: 0 }).ok();
    r.render(128, vec![]);
    drop(r.handle().take_retired());
    plugin.deactivate();
    let again = plugin.activate(sr as f64, 128).expect("reactivate");
    drop(again);
    plugin.deactivate();
}

#[test]
fn idle_instrument_sleeps_and_wakes_when_selected() {
    let Some((lib, waker, rx)) = surge() else {
        return;
    };
    let mut plugin = lib.create(&instrument_id(&lib), waker).expect("create");
    pump(&mut plugin, &rx);
    let sr = 48000.0;
    let mut r = OfflineRenderer::new(sr, 128);
    let processor = plugin.activate(sr as f64, 128).unwrap();
    r.handle()
        .send(Command::SetProcessor { slot: 0, processor })
        .ok();
    // Keyboard on another (empty) slot, so slot 0 is idle: after a second of silence it sleeps.
    r.handle()
        .send(Command::SetProcessor {
            slot: 1,
            processor: Box::new(merlin_hosting::builtin::SineSynth::new(sr)),
        })
        .ok();
    r.handle()
        .send(Command::Select {
            slot: 1,
            mode: SelectMode::Exclusive,
        })
        .ok();
    r.render(48000 * 2, vec![]);
    assert_eq!(
        r.handle().shared.sleeping.load(Ordering::Relaxed) & 1,
        1,
        "asleep when silent and idle"
    );
    r.handle()
        .send(Command::Select {
            slot: 0,
            mode: SelectMode::Exclusive,
        })
        .ok();
    let heard = r.render(24000, vec![(256, on(60))]);
    assert!(
        heard.peak(4000..24000) > 0.02,
        "selecting it and playing a note works"
    );
    assert_eq!(r.handle().shared.sleeping.load(Ordering::Relaxed) & 1, 0);

    r.handle().send(Command::ClearProcessor { slot: 0 }).ok();
    r.render(128, vec![]);
    drop(r.handle().take_retired());
    plugin.deactivate();
}

#[test]
fn state_round_trips() {
    let Some((lib, waker, rx)) = surge() else {
        return;
    };
    let mut plugin = lib.create(&instrument_id(&lib), waker).expect("create");
    pump(&mut plugin, &rx);
    let volume = plugin
        .params()
        .into_iter()
        .find(|p| p.name == "Global Volume")
        .unwrap();

    let sr = 48000.0;
    let mut r = OfflineRenderer::new(sr, 128);
    let processor = plugin.activate(sr as f64, 128).unwrap();
    r.handle()
        .send(Command::SetProcessor { slot: 0, processor })
        .ok();
    r.render(128 * 4, vec![]);
    pump(&mut plugin, &rx);

    let init = plugin.save_state().expect("save");
    assert!(
        init.len() > 1000,
        "Surge state is a patch: {} bytes",
        init.len()
    );
    // Loading the blob back and saving again gives the same bytes.
    plugin.load_state(&init).expect("load");
    pump(&mut plugin, &rx);
    r.render(128 * 8, vec![]);
    assert_eq!(plugin.save_state().unwrap(), init, "load/save is stable");

    // A parameter change shows up in the state; restoring brings the first one back.
    plugin.set_param(volume.id, 0.1);
    r.render(128 * 8, vec![]);
    pump(&mut plugin, &rx);
    let changed = plugin.save_state().unwrap();
    assert_ne!(changed, init, "a parameter change shows up in the state");
    plugin.load_state(&init).unwrap();
    pump(&mut plugin, &rx);
    r.render(128 * 8, vec![]);
    assert_eq!(
        plugin.save_state().unwrap(),
        init,
        "restore is byte-identical"
    );
    assert!(plugin.load_state(b"garbage").is_err());

    r.handle().send(Command::ClearProcessor { slot: 0 }).ok();
    r.render(128, vec![]);
    drop(r.handle().take_retired());
    plugin.deactivate();
}
