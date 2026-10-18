//! S6: SoundFont engines compared on SF2 and SF3.
//!
//! Engines: FluidSynth (libfluidsynth.so.3 loaded at runtime, no -dev package needed),
//! rustysynth (pure Rust, SF2 only) and rustysynth-ext (fork with SF3 via lewton).
//!
//! For each engine and file: load time, resident memory, preset count, a rendered test
//! pattern written to `spikes/s6-soundfont/out/<engine>-<file>.wav`, render speed, and
//! (when built with RTSAN_ENABLE) RealtimeSanitizer checks around every call the audio
//! thread would make: note on/off, program change and render.
//!
//! Usage: `cargo run -p s6-soundfont --release -- <file.sf2|sf3>...`

mod fluid;
mod oxi;

use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Result, anyhow};

pub const SR: u32 = 48_000;
const BLOCK: usize = 128;

pub struct PresetInfo {
    pub bank: i32,
    pub program: i32,
    pub name: String,
}

pub trait Engine {
    fn name(&self) -> &'static str;
    fn presets(&mut self) -> Vec<PresetInfo>;
    fn program(&mut self, ch: i32, bank: i32, program: i32);
    fn note_on(&mut self, ch: i32, key: i32, vel: i32);
    fn note_off(&mut self, ch: i32, key: i32);
    fn render(&mut self, l: &mut [f32], r: &mut [f32]);
}

macro_rules! rusty_engine {
    ($ty:ident, $krate:ident, $label:literal) => {
        struct $ty {
            sf: Arc<$krate::SoundFont>,
            synth: $krate::Synthesizer,
        }

        impl $ty {
            fn load(path: &Path) -> Result<Self> {
                let mut f = std::fs::File::open(path)?;
                let sf = Arc::new($krate::SoundFont::new(&mut f).map_err(|e| anyhow!("{e:?}"))?);
                let mut settings = $krate::SynthesizerSettings::new(SR as i32);
                settings.block_size = BLOCK;
                let synth =
                    $krate::Synthesizer::new(&sf, &settings).map_err(|e| anyhow!("{e:?}"))?;
                Ok(Self { sf, synth })
            }
        }

        impl Engine for $ty {
            fn name(&self) -> &'static str {
                $label
            }
            fn presets(&mut self) -> Vec<PresetInfo> {
                self.sf
                    .get_presets()
                    .iter()
                    .map(|p| PresetInfo {
                        bank: p.get_bank_number(),
                        program: p.get_patch_number(),
                        name: p.get_name().to_string(),
                    })
                    .collect()
            }
            fn program(&mut self, ch: i32, bank: i32, program: i32) {
                self.synth.process_midi_message(ch, 0xB0, 0x00, bank);
                self.synth.process_midi_message(ch, 0xC0, program, 0);
            }
            fn note_on(&mut self, ch: i32, key: i32, vel: i32) {
                self.synth.note_on(ch, key, vel);
            }
            fn note_off(&mut self, ch: i32, key: i32) {
                self.synth.note_off(ch, key);
            }
            fn render(&mut self, l: &mut [f32], r: &mut [f32]) {
                self.synth.render(l, r);
            }
        }
    };
}

rusty_engine!(Rusty, rustysynth, "rustysynth");
rusty_engine!(RustyExt, rustysynth_ext, "rustysynth-ext");

/// Run `f` as if it were on the audio thread: RTSan aborts or reports (depending on
/// RTSAN_OPTIONS) on malloc, locks, sleeps and syscalls inside. No-op without RTSAN_ENABLE.
fn rt<R>(f: impl FnOnce() -> R) -> R {
    rtsan_standalone::realtime_enter();
    let r = f();
    rtsan_standalone::realtime_exit();
    r
}

fn rss_mib() -> f64 {
    let statm = std::fs::read_to_string("/proc/self/statm").unwrap_or_default();
    let pages: f64 = statm
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0.0);
    pages * 4096.0 / (1024.0 * 1024.0)
}

/// Piano chord, strings chord, guitar arpeggio, drum groove: one second each plus release.
fn run_pattern(engine: &mut dyn Engine, out: &Path) -> Result<(f64, f32, f32)> {
    struct Part {
        ch: i32,
        bank: i32,
        program: i32,
        keys: &'static [i32],
        stagger: usize,
    }
    let parts = [
        Part {
            ch: 0,
            bank: 0,
            program: 0,
            keys: &[48, 60, 64, 67, 72],
            stagger: 0,
        },
        Part {
            ch: 1,
            bank: 0,
            program: 48,
            keys: &[53, 57, 60, 65],
            stagger: 0,
        },
        Part {
            ch: 2,
            bank: 0,
            program: 24,
            keys: &[55, 59, 62, 67, 71],
            stagger: 4_800,
        },
        Part {
            ch: 9,
            bank: 128,
            program: 0,
            keys: &[36, 42, 38, 42, 36, 36, 38, 46],
            stagger: 6_000,
        },
    ];
    let part_len = SR as usize; // notes held for 1 s
    let tail = SR as usize / 2; // then 0.5 s of release
    let total = parts.len() * (part_len + tail);

    let mut left = vec![0f32; total];
    let mut right = vec![0f32; total];
    let mut render_secs = 0.0;
    let mut pos = 0usize;

    for part in &parts {
        rt(|| engine.program(part.ch, part.bank, part.program));
        // Schedule note-ons at block boundaries; note-offs at the end of the part.
        let mut pending: Vec<(usize, i32)> = part
            .keys
            .iter()
            .enumerate()
            .map(|(i, &k)| (pos + i * part.stagger, k))
            .collect();
        let off_at = pos + part_len;
        let end = off_at + tail;
        while pos < end {
            let n = BLOCK.min(end - pos);
            let t = Instant::now();
            rt(|| {
                while let Some(&(at, key)) = pending.first() {
                    if at >= pos + n {
                        break;
                    }
                    engine.note_on(part.ch, key, 100);
                    pending.remove(0);
                }
                if (pos..pos + n).contains(&off_at) {
                    for &k in part.keys {
                        engine.note_off(part.ch, k);
                    }
                }
                engine.render(&mut left[pos..pos + n], &mut right[pos..pos + n]);
            });
            render_secs += t.elapsed().as_secs_f64();
            pos += n;
        }
    }

    let peak = left.iter().chain(&right).fold(0f32, |m, s| m.max(s.abs()));
    let rms = (left.iter().map(|s| s * s).sum::<f32>() / total as f32).sqrt();

    // Engines differ in output gain (FluidSynth defaults to 0.2), and louder usually sounds
    // "better". Write the WAVs RMS-matched (-20 dBFS) so the listening test is about timbre.
    let target_rms = 0.1f32;
    let gain = if rms > 0.0 {
        (target_rms / rms).min(0.98 / peak)
    } else {
        1.0
    };

    let spec = hound::WavSpec {
        channels: 2,
        sample_rate: SR,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    let mut w = hound::WavWriter::create(out, spec)?;
    for (l, r) in left.iter().zip(&right) {
        w.write_sample(*l * gain)?;
        w.write_sample(*r * gain)?;
    }
    w.finalize()?;
    let audio_secs = total as f64 / SR as f64;
    Ok((audio_secs / render_secs, peak, rms))
}

fn main() -> Result<()> {
    rtsan_standalone::ensure_initialized();
    let files: Vec<String> = std::env::args().skip(1).collect();
    if files.is_empty() {
        return Err(anyhow!("usage: s6-soundfont <file.sf2|sf3>..."));
    }
    let out_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("out");
    std::fs::create_dir_all(&out_dir)?;

    type Loader = fn(&Path) -> Result<Box<dyn Engine>>;
    let loaders: [(&str, Loader); 4] = [
        ("fluidsynth", |p| Ok(Box::new(fluid::Fluid::load(p)?))),
        ("rustysynth", |p| Ok(Box::new(Rusty::load(p)?))),
        ("rustysynth-ext", |p| Ok(Box::new(RustyExt::load(p)?))),
        ("oxisynth", |p| Ok(Box::new(oxi::Oxi::load(p)?))),
    ];

    for file in &files {
        let path = Path::new(file);
        let stem = path.file_name().unwrap().to_string_lossy();
        let size = std::fs::metadata(path)?.len() as f64 / (1024.0 * 1024.0);
        println!("\n=== {stem} ({size:.1} MiB)");
        for (label, load) in &loaders {
            let rss0 = rss_mib();
            let t = Instant::now();
            let mut engine = match load(path) {
                Ok(e) => e,
                Err(e) => {
                    println!("{label:<15} load FAILED: {e}");
                    continue;
                }
            };
            let load_ms = t.elapsed().as_secs_f64() * 1000.0;
            let rss = rss_mib() - rss0;
            let presets = engine.presets();
            let sample: Vec<String> = presets
                .iter()
                .take(3)
                .map(|p| format!("{}:{} {}", p.bank, p.program, p.name.trim()))
                .collect();
            let wav = out_dir.join(format!("{}-{stem}.wav", engine.name()));
            let (speed, peak, rms) = run_pattern(engine.as_mut(), &wav)?;
            println!(
                "{label:<15} load {load_ms:>7.0} ms  +{rss:>6.0} MiB  {:>4} presets  \
                 {speed:>6.0}x realtime  peak {peak:.2}  rms {rms:.3}  [{}]",
                presets.len(),
                sample.join(", ")
            );
        }
    }
    Ok(())
}
