//! Measures what hosting a plugin costs: the same blocks and notes, run either in this process
//! (what the app does with `MERLIN_NO_SANDBOX=1`) or through a `merlin-plugin-host` process
//! (the normal case), paced in real time like an audio callback.
//!
//! ```text
//! cargo build --release -p merlin --bin merlin-plugin-host
//! cargo run --release -p merlin-sandbox --example handoff_bench -- \
//!     <plugin file> <plugin id> <inproc|sandbox> [frames=128] [seconds=15] [notes=10] [rt=0]
//! ```
//!
//! `MERLIN_PLUGIN_HOST` names the host executable (default: `target/release/merlin-plugin-host`).
//! It prints the time each `process()` call took, as a share of the block's duration (the
//! "DSP load" a JACK/PipeWire meter would show for it), and the CPU time both processes used.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use merlin_hosting::{HostEvent, Io, MidiEvent, PluginHandle, Waker, open_plugin};
use merlin_sandbox::RemotePlugin;

struct Quiet;

impl Waker for Quiet {
    fn wake(&self, _: HostEvent) {}
}

const RATE: f64 = 48_000.0;

/// Sum of the CPU time (ns) every thread of `pid` has used, from the scheduler's own books.
fn cpu_ns(pid: u32) -> u64 {
    let Ok(dir) = std::fs::read_dir(format!("/proc/{pid}/task")) else {
        return 0;
    };
    dir.flatten()
        .filter_map(|t| std::fs::read_to_string(t.path().join("schedstat")).ok())
        .filter_map(|s| s.split_whitespace().next()?.parse::<u64>().ok())
        .sum()
}

fn sleep_until(deadline: Instant, origin: Instant, origin_ts: libc::timespec) {
    let at = deadline - origin;
    let mut ts = origin_ts;
    let total = ts.tv_nsec as u64 + at.subsec_nanos() as u64;
    ts.tv_sec += at.as_secs() as libc::time_t + (total / 1_000_000_000) as libc::time_t;
    ts.tv_nsec = (total % 1_000_000_000) as libc::c_long;
    // SAFETY: sleeping the calling thread until an absolute time.
    unsafe {
        libc::clock_nanosleep(
            libc::CLOCK_MONOTONIC,
            libc::TIMER_ABSTIME,
            &ts,
            std::ptr::null_mut(),
        );
    }
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    if a.len() < 4 {
        eprintln!(
            "usage: handoff_bench <plugin file> <plugin id> <inproc|sandbox> [frames] [seconds] [notes] [rt]"
        );
        std::process::exit(64);
    }
    let (file, id, mode) = (Path::new(&a[1]), a[2].as_str(), a[3].as_str());
    let frames: usize = a.get(4).and_then(|s| s.parse().ok()).unwrap_or(128);
    let seconds: u64 = a.get(5).and_then(|s| s.parse().ok()).unwrap_or(15);
    let notes: u8 = a.get(6).and_then(|s| s.parse().ok()).unwrap_or(10);
    let realtime: bool = a.get(7).is_some_and(|s| s == "1");

    let waker: Arc<dyn Waker> = Arc::new(Quiet);
    let (mut plugin, host_pid): (Box<dyn PluginHandle>, Option<u32>) = match mode {
        "sandbox" => {
            let exe = std::env::var_os("MERLIN_PLUGIN_HOST")
                .map(PathBuf::from)
                .unwrap_or_else(|| "target/release/merlin-plugin-host".into());
            let p = RemotePlugin::spawn(
                &exe,
                file,
                id,
                waker,
                RATE,
                frames as u32,
                Duration::from_secs(60),
                &[],
            )
            .expect("spawn the plugin host");
            let pid = p.pid();
            (Box::new(p), Some(pid))
        }
        _ => (
            open_plugin(file, id, waker, RATE, frames as u32).expect("open the plugin"),
            None,
        ),
    };
    let mut processor = plugin.activate(RATE, frames as u32).expect("activate");

    if realtime {
        // SAFETY: scheduling this thread like an audio callback (needs RLIMIT_RTPRIO).
        let ok = unsafe {
            let p = libc::sched_param { sched_priority: 20 };
            libc::sched_setscheduler(0, libc::SCHED_FIFO, &p) == 0
        };
        eprintln!("realtime: {}", if ok { "yes" } else { "refused" });
    }
    // The app's callback flushes denormals; do the same here so only the host differs.
    #[cfg(target_arch = "x86_64")]
    // SAFETY: sets this thread's MXCSR.
    unsafe {
        let mut csr: u32 = 0;
        std::arch::asm!("stmxcsr [{}]", in(reg) &mut csr, options(nostack));
        csr |= 0x8040;
        std::arch::asm!("ldmxcsr [{}]", in(reg) &csr, options(nostack));
    }

    let period = Duration::from_secs_f64(frames as f64 / RATE);
    let (mut l, mut r) = (vec![0.0f32; frames], vec![0.0f32; frames]);
    let mut events: Vec<MidiEvent> = Vec::with_capacity(64);
    let mut times: Vec<u32> = Vec::with_capacity((seconds as f64 / period.as_secs_f64()) as usize);

    // Warm up (page in the buffers, let the plugin settle), then measure.
    let warm = (RATE as usize / frames) * 2;
    let total = warm + (seconds as f64 / period.as_secs_f64()) as usize;
    let mut origin_ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: reading the clock into our struct.
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut origin_ts) };
    let origin = Instant::now();
    let (mut self_before, mut host_before) = (0, 0);
    let mut held: Vec<u8> = Vec::new();
    // About 1/8 s between chords: a player mashing keys.
    let chord_every = ((RATE / 8.0) as usize / frames).max(1);

    for n in 0..total {
        if n == warm {
            self_before = cpu_ns(std::process::id());
            host_before = host_pid.map_or(0, cpu_ns);
        }
        events.clear();
        if n % chord_every == 0 {
            for k in held.drain(..) {
                events.push(MidiEvent::new(0, 1, [0x80, k, 0]));
            }
            let base = 36 + (n / chord_every % 40) as u8;
            for i in 0..notes {
                let k = base + i * 2 % 48;
                events.push(MidiEvent::new(i as u32 % frames as u32, 1, [0x90, k, 90]));
                held.push(k);
            }
        }
        l.fill(0.0);
        r.fill(0.0);
        let t = Instant::now();
        processor.process(
            &mut Io {
                input: None,
                out_l: &mut l,
                out_r: &mut r,
            },
            &events,
        );
        let took = t.elapsed();
        if n >= warm {
            times.push(took.as_micros().min(u32::MAX as u128) as u32);
        }
        sleep_until(origin + period * (n as u32 + 1), origin, origin_ts);
    }
    let self_cpu = cpu_ns(std::process::id()) - self_before;
    let host_cpu = host_pid.map_or(0, cpu_ns).saturating_sub(host_before);

    times.sort_unstable();
    let pct = |p: f64| times[((times.len() - 1) as f64 * p) as usize] as f64;
    let period_us = period.as_micros() as f64;
    let mean = times.iter().map(|&t| t as f64).sum::<f64>() / times.len() as f64;
    let late = times.iter().filter(|&&t| t as f64 > period_us).count();
    let wall = seconds as f64 * 1e9;
    println!(
        "{mode:8} {frames:4} frames ({:.2} ms)  process(): mean {:5.0} us ({:4.1}%)  p50 {:5.0}  p99 {:5.0}  max {:6.0}  over-budget {late}/{}   cpu: bench {:4.1}% host {:4.1}%",
        period_us / 1000.0,
        mean,
        mean / period_us * 100.0,
        pct(0.5),
        pct(0.99),
        pct(1.0),
        times.len(),
        self_cpu as f64 / wall * 100.0,
        host_cpu as f64 / wall * 100.0,
    );
    println!(
        "plugin audio thread priority: {:?}",
        plugin.audio_priority()
    );
    drop(processor);
    drop(plugin);
}
