//! Plugins in processes of their own: the VST2 test plugin run through `merlin-plugin-host`,
//! including what happens when it crashes or hangs. A plugin dying must never take this test
//! process (standing in for the app) down.

use std::path::PathBuf;
use std::process::Command;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use merlin_hosting::{
    HostEvent, Io, MidiEvent, PluginHandle, PluginProcessor, ProcessStatus, Waker,
};
use merlin_sandbox::{PluginFailed, RemotePlugin};

const HOST: &str = env!("CARGO_BIN_EXE_merlin-plugin-host");
const ID: &str = "54535931";

struct Events(Mutex<mpsc::Sender<HostEvent>>);
impl Waker for Events {
    fn wake(&self, e: HostEvent) {
        let _ = self.0.lock().unwrap().send(e);
    }
}

fn waker() -> (Arc<Events>, mpsc::Receiver<HostEvent>) {
    let (tx, rx) = mpsc::channel();
    (Arc::new(Events(Mutex::new(tx))), rx)
}

/// Builds the fixture plugin; `None` (and the test is skipped) without a C compiler.
fn fixture() -> Option<PathBuf> {
    static BUILT: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();
    BUILT
        .get_or_init(|| {
            let dir =
                std::env::temp_dir().join(format!("merlin-sandbox-fixture-{}", std::process::id()));
            std::fs::create_dir_all(&dir).ok()?;
            let out = dir.join("test_synth.so");
            let src = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../crates/hosting/tests/fixtures/test_synth_vst2.c");
            Command::new("cc")
                .args(["-shared", "-fPIC", "-O1", "-o"])
                .arg(&out)
                .arg(&src)
                .arg("-lm")
                .status()
                .ok()?
                .success()
                .then_some(out)
        })
        .clone()
}

fn start() -> Option<(RemotePlugin, mpsc::Receiver<HostEvent>)> {
    let path = fixture()?;
    let (w, rx) = waker();
    let plugin = RemotePlugin::spawn(
        HOST.as_ref(),
        &path,
        ID,
        w,
        48_000.0,
        512,
        Duration::from_secs(20),
    )
    .expect("the plugin loads");
    Some((plugin, rx))
}

fn block(proc: &mut dyn PluginProcessor, events: &[MidiEvent]) -> (Vec<f32>, ProcessStatus) {
    let (mut l, mut r) = (vec![0.0; 512], vec![0.0; 512]);
    let status = proc.process(
        &mut Io {
            input: None,
            out_l: &mut l,
            out_r: &mut r,
        },
        events,
    );
    assert_eq!(l, r);
    (l, status)
}

fn peak(v: &[f32]) -> f32 {
    v.iter().fold(0.0, |m, x| m.max(x.abs()))
}

fn alive(pid: u32) -> bool {
    // A zombie counts as gone.
    std::fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|s| {
        !s.rsplit(')')
            .next()
            .unwrap_or("")
            .trim_start()
            .starts_with('Z')
    })
}

#[test]
fn a_sandboxed_plugin_is_played_saved_and_edited_like_any_other() {
    let Some((mut plugin, _events)) = start() else {
        eprintln!("no C compiler: skipping");
        return;
    };
    assert_eq!(plugin.info().name, "Test Synth");
    assert!(plugin.info().is_instrument());
    let mut proc = plugin.activate(48_000.0, 512).unwrap();

    let (silent, status) = block(proc.as_mut(), &[]);
    assert_eq!(peak(&silent), 0.0);
    assert_eq!(status, ProcessStatus::Continue);
    let (loud, _) = block(proc.as_mut(), &[MidiEvent::note_on(0, 0, 69, 127)]);
    assert!((peak(&loud) - 0.5).abs() < 0.02, "{}", peak(&loud));
    let crossings = loud
        .windows(2)
        .filter(|w| w[0] <= 0.0 && w[1] > 0.0)
        .count();
    assert!((4..=5).contains(&crossings), "{crossings}");

    plugin.set_param(0, 0.25);
    // Setting a parameter does not wait for the plugin (it is on its way within a moment): any
    // call that needs an answer comes after it.
    plugin.params();
    let (quieter, _) = block(proc.as_mut(), &[]);
    assert!((peak(&quieter) - 0.25).abs() < 0.02, "{}", peak(&quieter));
    let params = plugin.params();
    assert_eq!(params.len(), 2);
    assert_eq!(params[0].name, "Gain");
    assert!((plugin.param_value(0).unwrap() - 0.25).abs() < 0.01);

    // The tempo the app's transport has reaches the plugin in the other process.
    merlin_hosting::host_time::set_tempo(150.0);
    block(proc.as_mut(), &[]);
    std::thread::sleep(Duration::from_millis(40)); // past the readings' cache
    assert!((plugin.param_value(1).unwrap() - 0.5).abs() < 0.01);

    let state = plugin.save_state().unwrap();
    plugin.set_param(0, 1.0);
    plugin.params();
    block(proc.as_mut(), &[]);
    plugin.load_state(&state).unwrap();
    let (restored, _) = block(proc.as_mut(), &[]);
    assert!((peak(&restored) - 0.25).abs() < 0.02, "{}", peak(&restored));
    assert!(
        plugin.load_state(b"junk").is_err(),
        "an ordinary error, not a crash"
    );
    assert!(plugin.failure().is_none());

    let ed = plugin.editor().expect("the plugin has an editor");
    let size = ed.attach(1234, 1.0).unwrap();
    assert_eq!((size.width, size.height), (320, 200));
    assert!(ed.is_attached());
    ed.detach();
    assert!(!ed.is_attached());

    let pid = plugin.pid();
    drop(proc);
    plugin.deactivate();
    drop(plugin);
    assert!(!alive(pid), "the host process leaves with its plugin");
}

#[test]
fn a_plugin_that_crashes_while_playing_is_silent_and_the_app_lives() {
    let Some((mut plugin, events)) = start() else {
        return;
    };
    let (mut other, _other_events) = start().unwrap();
    let mut proc = plugin.activate(48_000.0, 512).unwrap();
    let mut other_proc = other.activate(48_000.0, 512).unwrap();
    block(proc.as_mut(), &[]);

    // Note 0 makes the test plugin dereference null on its audio thread.
    let (out, status) = block(proc.as_mut(), &[MidiEvent::note_on(0, 0, 0, 100)]);
    assert_eq!(peak(&out), 0.0);
    assert_eq!(status, ProcessStatus::Sleep);
    // From now on it costs nothing and answers nothing.
    let t = Instant::now();
    for _ in 0..100 {
        assert_eq!(block(proc.as_mut(), &[]).1, ProcessStatus::Sleep);
    }
    assert!(t.elapsed() < Duration::from_millis(100));

    let why = plugin.failure().expect("it failed");
    assert!(why.starts_with("crashed (SIGSEGV"), "{why}");
    assert_eq!(
        events.recv_timeout(Duration::from_secs(2)).unwrap(),
        HostEvent::Crashed
    );
    // Every call returns at once with an error that says why.
    let t = Instant::now();
    let err = plugin.save_state().unwrap_err();
    assert!(err.downcast_ref::<PluginFailed>().is_some(), "{err:#}");
    assert!(plugin.params().is_empty());
    plugin.set_param(0, 0.1);
    plugin.deactivate();
    assert!(t.elapsed() < Duration::from_millis(100));

    // The other plugin did not notice.
    let (loud, _) = block(other_proc.as_mut(), &[MidiEvent::note_on(0, 0, 69, 127)]);
    assert!(peak(&loud) > 0.4);
    assert!(other.failure().is_none());

    let pid = plugin.pid();
    drop(proc);
    drop(plugin);
    assert!(!alive(pid));
}

#[test]
fn a_plugin_that_hangs_while_playing_is_given_up_on() {
    let Some((mut plugin, events)) = start() else {
        return;
    };
    let mut proc = plugin.activate(48_000.0, 512).unwrap();
    block(proc.as_mut(), &[]);
    let pid = plugin.pid();
    let t = Instant::now();
    // Note 1 makes the test plugin loop forever in its audio thread.
    let (out, status) = block(proc.as_mut(), &[MidiEvent::note_on(0, 0, 1, 100)]);
    let took = t.elapsed();
    eprintln!("failure: {:?}", plugin.failure());
    assert_eq!((peak(&out), status), (0.0, ProcessStatus::Sleep));
    assert!(took > Duration::from_millis(900), "{took:?}");
    assert!(took < Duration::from_secs(3), "{took:?}");
    assert_eq!(
        plugin.failure().as_deref(),
        Some("stopped responding while playing")
    );
    assert_eq!(
        events.recv_timeout(Duration::from_secs(2)).unwrap(),
        HostEvent::Crashed
    );
    drop(proc);
    drop(plugin);
    assert!(!alive(pid), "the stuck process was killed");
}

#[test]
fn a_plugin_that_crashes_on_the_main_thread_fails_the_call() {
    let Some((mut plugin, events)) = start() else {
        return;
    };
    let _proc = plugin.activate(48_000.0, 512).unwrap();
    let mut state = plugin.save_state().unwrap();
    // The state's chunk starts after the magic and a tag: 666.0 crashes the test plugin.
    state[5..9].copy_from_slice(&666.0f32.to_le_bytes());
    let t = Instant::now();
    let err = plugin.load_state(&state).unwrap_err();
    let failed = err
        .downcast_ref::<PluginFailed>()
        .expect("a crash, not an error");
    assert!(failed.0.starts_with("crashed (SIGSEGV"), "{failed}");
    assert!(t.elapsed() < Duration::from_secs(3));
    assert_eq!(
        events.recv_timeout(Duration::from_secs(2)).unwrap(),
        HostEvent::Crashed
    );
}

#[test]
fn a_plugin_whose_main_thread_is_stuck_is_found_by_the_pings() {
    let Some((mut plugin, events)) = start() else {
        return;
    };
    let pid = plugin.pid() as i32;
    let t0 = Instant::now();
    // Healthy: pings go out and come back.
    for n in 1..=3u64 {
        plugin.service(t0 + Duration::from_secs(4 * n));
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(plugin.failure().is_none());
    // A frozen process answers nothing (what a plugin stuck in its main thread looks like).
    // SAFETY: stopping the host process this test started.
    unsafe { libc::kill(pid, libc::SIGSTOP) };
    plugin.service(t0 + Duration::from_secs(20));
    assert!(
        plugin.failure().is_none(),
        "one missing answer is not yet a hang"
    );
    plugin.service(t0 + Duration::from_secs(25));
    assert!(plugin.failure().is_none());
    plugin.service(t0 + Duration::from_secs(40));
    assert_eq!(
        plugin.failure().as_deref(),
        Some("stopped responding while idle")
    );
    assert_eq!(
        events.recv_timeout(Duration::from_secs(2)).unwrap(),
        HostEvent::Crashed
    );
    drop(plugin);
    assert!(
        !alive(pid as u32),
        "a stopped process is killed all the same"
    );
}

#[test]
fn a_plugin_that_cannot_load_is_an_error_and_a_crash_while_loading_is_one_too() {
    let Some(path) = fixture() else { return };
    let (w, rx) = waker();
    let err = RemotePlugin::spawn(
        HOST.as_ref(),
        &path,
        "no such plugin",
        w,
        48_000.0,
        512,
        Duration::from_secs(20),
    )
    .err()
    .expect("an error");
    assert!(err.downcast_ref::<PluginFailed>().is_none(), "{err:#}");
    // Nobody is told about a plugin that never started.
    assert!(rx.recv_timeout(Duration::from_millis(300)).is_err());

    // A "plugin host" that dies at once stands for a plugin that crashes in its constructor.
    let (w, _rx) = waker();
    let err = RemotePlugin::spawn(
        "/bin/false".as_ref(),
        &path,
        ID,
        w,
        48_000.0,
        512,
        Duration::from_secs(20),
    )
    .err()
    .expect("an error");
    assert!(err.downcast_ref::<PluginFailed>().is_some(), "{err:#}");
    let missing = RemotePlugin::spawn(
        "/no/such/host".as_ref(),
        &path,
        ID,
        waker().0,
        48_000.0,
        512,
        Duration::from_secs(1),
    );
    assert!(missing.is_err());
}

#[test]
fn the_host_leaves_when_the_app_does() {
    use std::os::fd::AsRawFd;
    use std::os::unix::net::UnixStream;
    use std::os::unix::process::CommandExt;

    let Some(path) = fixture() else { return };
    // Talk to the host by hand, then close the socket without a goodbye: what an app that
    // crashed leaves behind.
    let shm = merlin_sandbox::shm::Shm::create().unwrap();
    let (mut mine, theirs) = UnixStream::pair().unwrap();
    let (sock_fd, shm_fd) = (theirs.as_raw_fd(), shm.raw_fd());
    let mut cmd = Command::new(HOST);
    cmd.args(["--sandbox", &sock_fd.to_string(), &shm_fd.to_string()])
        .process_group(0);
    // SAFETY: only fcntl runs between fork and exec.
    unsafe {
        cmd.pre_exec(move || {
            for fd in [sock_fd, shm_fd] {
                libc::fcntl(fd, libc::F_SETFD, 0);
            }
            Ok(())
        });
    }
    let mut child = cmd.spawn().unwrap();
    drop(theirs);
    let load = merlin_sandbox::proto::ToHost {
        id: 1,
        request: merlin_sandbox::proto::Request::Load {
            path,
            id: ID.into(),
            sample_rate: 48_000.0,
            max_block: 512,
        },
    };
    merlin_sandbox::proto::write_message(&mut mine, &load, &[]).unwrap();
    let (reply, _): (merlin_sandbox::proto::ToApp, _) =
        merlin_sandbox::proto::read_message(&mut mine)
            .unwrap()
            .unwrap();
    assert!(matches!(
        reply,
        merlin_sandbox::proto::ToApp::Reply {
            response: merlin_sandbox::proto::Response::Loaded { .. },
            ..
        }
    ));
    assert!(child.try_wait().unwrap().is_none(), "it is running");
    drop(mine);
    let until = Instant::now() + Duration::from_secs(3);
    while child.try_wait().unwrap().is_none() {
        assert!(Instant::now() < until, "the host is still running");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn a_block_costs_microseconds_not_milliseconds() {
    let Some((mut plugin, _events)) = start() else {
        return;
    };
    let mut proc = plugin.activate(48_000.0, 512).unwrap();
    for _ in 0..50 {
        block(proc.as_mut(), &[]);
    }
    let n = 2000;
    let t = Instant::now();
    for _ in 0..n {
        block(proc.as_mut(), &[]);
    }
    let each = t.elapsed() / n;
    eprintln!("a block takes {each:?} through the process boundary");
    // A 512-frame block at 48 kHz lasts 10.7 ms. Even a slow, shared CI machine stays far below.
    assert!(each < Duration::from_millis(2), "{each:?}");
}
