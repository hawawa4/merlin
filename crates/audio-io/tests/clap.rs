//! Hosts a real CLAP plugin (Surge XT) through the hosting traits and renders it with the
//! engine. Skipped when the test plugin is not installed (`just setup` downloads it).

use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, mpsc};

use merlin_audio_io::{Item, OfflineRenderer};
use merlin_engine::{Command, SelectMode};
use merlin_hosting::clap::ClapLibrary;
use merlin_hosting::{HostEvent, MidiEvent, PluginHandle, Waker};

const SURGE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../test-assets/dl/surge-linux/Surge XT.clap"
);
const SURGE_ID: &str = "org.surge-synth-team.surge-xt";

struct ChannelWaker(Mutex<mpsc::Sender<HostEvent>>);

impl Waker for ChannelWaker {
    fn wake(&self, event: HostEvent) {
        let _ = self.0.lock().unwrap().send(event);
    }
}

fn surge() -> Option<(ClapLibrary, Arc<ChannelWaker>, mpsc::Receiver<HostEvent>)> {
    let path = PathBuf::from(SURGE);
    if !path.exists() {
        eprintln!("skipped: {SURGE} not installed");
        return None;
    }
    let (tx, rx) = mpsc::channel();
    Some((
        ClapLibrary::load(&path).expect("load Surge"),
        Arc::new(ChannelWaker(Mutex::new(tx))),
        rx,
    ))
}

/// Runs what the plugin asked of the main thread.
fn pump(plugin: &mut dyn PluginHandle, rx: &mpsc::Receiver<HostEvent>) {
    while let Ok(ev) = rx.try_recv() {
        if ev == HostEvent::MainThreadCallback {
            plugin.on_main_thread();
        }
    }
}

fn on(key: u8) -> Item {
    Item::Midi(MidiEvent::note_on(0, 0, key, 110))
}

#[test]
fn lists_and_describes_plugins() {
    let Some((lib, ..)) = surge() else { return };
    let plugins = lib.plugins().unwrap();
    let p = plugins.iter().find(|p| p.id == SURGE_ID).expect("Surge XT");
    assert_eq!(p.name, "Surge XT");
    assert!(p.is_instrument());
    assert!(!p.vendor.is_empty());
}

#[test]
fn plays_through_the_engine_and_follows_parameters() {
    let Some((lib, waker, rx)) = surge() else {
        return;
    };
    let mut plugin = lib.create(SURGE_ID, waker).expect("create");
    let params = plugin.params();
    assert!(
        params.len() > 100,
        "Surge exposes many parameters, got {}",
        params.len()
    );
    let volume = params
        .iter()
        .find(|p| p.name == "Global Volume" && p.automatable && !p.hidden)
        .unwrap_or_else(|| {
            panic!(
                "no Global Volume parameter in {:?}",
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

    // A held chord is heard.
    let heard = r.render(48000, vec![(0, on(60)), (0, on(64)), (0, on(67))]);
    assert!(
        heard.peak(8000..48000) > 0.02,
        "Surge makes sound: {}",
        heard.peak(8000..48000)
    );
    assert_eq!(r.handle().shared.held[60].load(Ordering::Relaxed), 1);

    // Turning the volume down from the main thread silences the held chord.
    plugin.set_param(volume.id, volume.min);
    pump(&mut plugin, &rx);
    let quiet = r.render(24000, vec![]);
    assert!(
        quiet.peak(12000..24000) < heard.peak(8000..48000) * 0.1,
        "volume parameter applied: {} vs {}",
        quiet.peak(12000..24000),
        heard.peak(8000..48000)
    );

    // Release everything; clean shutdown: retire the processor, then deactivate.
    r.handle().send(Command::Panic).ok();
    r.render(4800, vec![]);
    r.handle().send(Command::ClearProcessor { slot: 0 }).ok();
    r.render(128, vec![]);
    let retired = r.handle().take_retired();
    assert_eq!(retired.len(), 1);
    drop(retired);
    plugin.deactivate();
    // The plugin can be activated again afterwards.
    let again = plugin.activate(sr as f64, 128).expect("reactivate");
    drop(again);
    plugin.deactivate();
}

#[test]
fn state_round_trips() {
    let Some((lib, waker, rx)) = surge() else {
        return;
    };
    let mut plugin = lib.create(SURGE_ID, waker).expect("create");
    pump(&mut plugin, &rx);
    let params = plugin.params();
    let volume = params
        .iter()
        .find(|p| p.name == "Global Volume" && p.automatable && !p.hidden)
        .unwrap();

    let init = plugin.save_state().expect("save");
    assert!(
        init.len() > 1000,
        "Surge state is a patch: {} bytes",
        init.len()
    );
    // The state blob is stable: load it back, save again, get the same bytes.
    plugin.load_state(&init).expect("load");
    pump(&mut plugin, &rx);
    assert_eq!(plugin.save_state().unwrap(), init);

    // Loading a different patch changes the blob, and restoring brings the first one back.
    let sr = 48000.0;
    let mut r = OfflineRenderer::new(sr, 128);
    let processor = plugin.activate(sr as f64, 128).unwrap();
    r.handle()
        .send(Command::SetProcessor { slot: 0, processor })
        .ok();
    r.handle()
        .send(Command::Select {
            slot: 0,
            mode: SelectMode::Exclusive,
        })
        .ok();
    plugin.set_param(volume.id, volume.min);
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

    r.handle().send(Command::ClearProcessor { slot: 0 }).ok();
    r.render(128, vec![]);
    drop(r.handle().take_retired());
    plugin.deactivate();
}

/// A Windows plugin behind yabridge, through the same traits. Needs `just test-wine` (it sets
/// PATH and WINELOADER); skipped otherwise.
#[test]
fn windows_plugin_plays_through_yabridge() {
    let path = PathBuf::from(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../test-assets/bridges-vblank/clap/Six Sines.clap"
    ));
    if std::env::var_os("WINELOADER").is_none() || !path.exists() {
        eprintln!("skipped: run `just test-wine`");
        return;
    }
    let (tx, rx) = mpsc::channel();
    let waker = Arc::new(ChannelWaker(Mutex::new(tx)));
    let lib = ClapLibrary::load(&path).expect("load bridge");
    let info = lib.plugins().unwrap().remove(0);
    assert_eq!(info.name, "Six Sines");
    let mut plugin = lib.create(&info.id, waker).expect("create");
    pump(&mut plugin, &rx);
    assert!(!plugin.params().is_empty());

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
    r.render(128 * 4, vec![]);
    // The bridge round-trips through Wine on every block, so give it real time to answer.
    let heard = r.render(24000, vec![(0, on(60))]);
    assert!(
        heard.peak(4000..24000) > 0.01,
        "bridged plugin makes sound: {}",
        heard.peak(4000..24000)
    );
    let state = plugin.save_state().expect("state through the bridge");
    assert!(!state.is_empty());

    r.handle().send(Command::ClearProcessor { slot: 0 }).ok();
    r.render(128, vec![]);
    drop(r.handle().take_retired());
    plugin.deactivate();
}
