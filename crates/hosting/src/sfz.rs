//! SFZ instruments (a `.sfz` text file that maps samples on disk to keys), played by sfizz.
//!
//! Like a SoundFont, an SFZ file is not a plugin but is treated as one: it has a [`PluginInfo`],
//! a [`PluginHandle`] and a [`PluginProcessor`], no editor, and the slot shows a list of sounds
//! with a level knob instead. A folder with several `.sfz` files directly inside it (how SFZ
//! libraries usually ship their variations) is one instrument, each file one of its sounds.
//!
//! Loading a file reads the start of every sample it uses, which takes from a moment to many
//! seconds, and sfizz must not render while it loads. So a synth is always loaded on a thread of
//! its own and then handed to the audio thread whole: [`SfzPlugin::activate`] (on the loader's
//! worker) loads the first, and a worker thread per active instrument loads the next one when
//! another sound is chosen. The old synth plays out its notes' release beside the new one and
//! goes back to the worker to be freed, never on the audio thread.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::thread::{JoinHandle, Thread};
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use merlin_model::Format;
use merlin_sfizz::Synth;
use rtrb::{Consumer, Producer, RingBuffer};

use crate::{
    Io, MidiEvent, MidiKind, ParamInfo, PluginEditor, PluginHandle, PluginInfo, PluginProcessor,
    PresetInfo, ProcessStatus,
};

/// The id of a folder played as one instrument starts with this.
pub const FOLDER_PREFIX: &str = "sfz-folder:";
/// A folder holds at most this many files (a MIDI program change reaches each).
pub const MAX_COLLECTION: usize = 128;
/// State: which file plays (its number and its name, so files added to or removed from a
/// folder do not move a saved choice), and the level.
const STATE_V1: &[u8; 4] = b"SFZ1";
/// Voices a synth can play at once.
const VOICES: usize = 96;
/// The longest the previous sound's notes ring on after a switch.
const TAIL: Duration = Duration::from_secs(3);

/// True for a file this module plays.
pub fn is_sfz(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("sfz"))
}

/// Whether a saved or scanned instrument (`path`, `id`) is played by this module: an `.sfz`
/// file, or a folder of them (known by its id: a folder alone could be a SoundFont folder).
pub fn is_source(path: &Path, id: &str) -> bool {
    is_sfz(path) || id.starts_with(FOLDER_PREFIX)
}

/// A file's name as a sound is called: its stem, without the leading underscores some
/// libraries use to sort their main files first.
fn sound_name(path: &Path) -> String {
    let stem = path
        .file_stem()
        .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
    match stem.trim_start_matches(['_', ' ']) {
        "" => stem,
        s => s.to_owned(),
    }
}

/// What the browser shows for an SFZ file. Cheap: nothing is read.
pub fn info_for(path: &Path) -> PluginInfo {
    let file = path
        .file_name()
        .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
    // Libraries name files alike ("piano.sfz"): the folder makes the id tell them apart.
    let id = match path.parent().and_then(Path::file_name) {
        Some(dir) => format!("{}/{file}", dir.to_string_lossy()),
        None => file,
    };
    PluginInfo {
        format: Format::Sfz,
        id,
        name: sound_name(path),
        vendor: "SFZ".into(),
        version: String::new(),
        features: vec!["instrument".into(), "sfz".into()],
    }
}

/// What the browser shows for a folder of SFZ files: one instrument named like the folder.
pub fn collection_info(dir: &Path) -> PluginInfo {
    let name = dir
        .file_name()
        .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
    PluginInfo {
        format: Format::Sfz,
        id: format!("{FOLDER_PREFIX}{name}"),
        name,
        vendor: "SFZ folder".into(),
        version: String::new(),
        features: vec!["instrument".into(), "sfz".into()],
    }
}

/// The SFZ files an instrument is made of: the file itself, or, for a folder, the `.sfz` files
/// directly inside it, in name order.
pub fn files_of(path: &Path) -> Vec<PathBuf> {
    if !path.is_dir() {
        return vec![path.to_owned()];
    }
    let mut files: Vec<PathBuf> = std::fs::read_dir(path)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file() && is_sfz(p))
        .collect();
    files.sort();
    files.truncate(MAX_COLLECTION);
    files
}

/// Loads one SFZ file into a new synth ready for the audio thread. Slow: never on the audio or
/// UI thread.
fn build(path: &Path, sample_rate: f64, max_block: u32) -> Result<Synth> {
    if !path.is_file() {
        bail!("{} is missing", path.display());
    }
    let mut s = Synth::new();
    s.set_sample_rate(sample_rate as f32);
    s.set_samples_per_block(max_block as usize);
    s.set_num_voices(VOICES);
    if !s.load_file(path) {
        bail!("cannot read {} as an SFZ instrument", path.display());
    }
    if s.num_regions() == 0 {
        // sfizz drops regions whose samples it cannot find or decode.
        bail!(
            "{} has nothing to play (are its samples missing?)",
            path.display()
        );
    }
    Ok(s)
}

/// What the main thread, the worker and the audio thread share.
struct Shared {
    /// The file chosen (index into the files), by the UI or a MIDI program change.
    want: AtomicU32,
    /// `f32` bits of the output level.
    gain: AtomicU32,
}

/// A synth on its way to the audio thread, with the file it plays.
type Loaded = (u32, Synth);

pub struct SfzPlugin {
    info: PluginInfo,
    files: Arc<[PathBuf]>,
    presets: Vec<PresetInfo>,
    /// A folder of files (its state also names the chosen file).
    collection: bool,
    shared: Arc<Shared>,
    worker: Option<Worker>,
}

impl SfzPlugin {
    /// An SFZ file, or a folder of them played as one instrument. Cheap: the file is read when
    /// the instrument is activated.
    pub fn open(path: &Path) -> Result<Self> {
        let collection = path.is_dir();
        if !collection && !path.is_file() {
            bail!("{} is missing", path.display());
        }
        let files = files_of(path);
        if files.is_empty() {
            bail!("{} has no SFZ files", path.display());
        }
        let presets = files
            .iter()
            .enumerate()
            .map(|(n, f)| PresetInfo {
                bank: 0,
                program: n as u8,
                name: sound_name(f),
                file: None,
            })
            .collect();
        Ok(Self {
            info: if collection {
                collection_info(path)
            } else {
                info_for(path)
            },
            files: files.into(),
            presets,
            collection,
            shared: Arc::new(Shared {
                want: AtomicU32::new(0),
                gain: AtomicU32::new(1.0f32.to_bits()),
            }),
            worker: None,
        })
    }

    fn stop_worker(&mut self) {
        if let Some(w) = self.worker.take() {
            w.stop.store(true, Ordering::Relaxed);
            w.thread.thread().unpark();
            // Not joined: it may be in the middle of a load. It frees what it holds and ends.
        }
    }
}

impl Drop for SfzPlugin {
    fn drop(&mut self) {
        self.stop_worker();
    }
}

impl PluginHandle for SfzPlugin {
    fn info(&self) -> &PluginInfo {
        &self.info
    }

    fn activate(&mut self, sample_rate: f64, max_block: u32) -> Result<Box<dyn PluginProcessor>> {
        self.stop_worker();
        let max_block = max_block.clamp(1, merlin_sfizz::MAX_BLOCK as u32);
        let mut index = self.shared.want.load(Ordering::Relaxed);
        let synth = match build(&self.files[index as usize], sample_rate, max_block) {
            Ok(s) => s,
            // In a folder, a damaged file must not keep the others from playing.
            Err(e) if self.collection => {
                let other = (0..self.files.len() as u32)
                    .filter(|&n| n != index)
                    .find_map(|n| {
                        build(&self.files[n as usize], sample_rate, max_block)
                            .ok()
                            .map(|s| (n, s))
                    });
                let Some((n, s)) = other else { return Err(e) };
                eprintln!(
                    "sfz: {e:#}; playing {} instead",
                    self.presets[n as usize].name
                );
                index = n;
                self.shared.want.store(n, Ordering::Relaxed);
                s
            }
            Err(e) => return Err(e),
        };
        // One synth in flight each way is enough: the worker loads the next sound only once
        // the audio thread has taken the last one.
        let (to_audio, incoming) = RingBuffer::<Loaded>::new(1);
        let (retire, retired) = RingBuffer::<Synth>::new(4);
        let stop = Arc::new(AtomicBool::new(false));
        let job = WorkerJob {
            files: self.files.clone(),
            names: self.presets.iter().map(|p| p.name.clone()).collect(),
            shared: self.shared.clone(),
            to_audio,
            retired,
            sample_rate,
            max_block,
            loaded: index,
            stop: stop.clone(),
        };
        let thread = std::thread::Builder::new()
            .name("sfz-loader".into())
            .spawn(move || job.run())?;
        let waker = thread.thread().clone();
        self.worker = Some(Worker { stop, thread });
        Ok(Box::new(Processor {
            synth,
            playing: index,
            count: self.files.len() as u32,
            incoming,
            retire,
            waker,
            shared: self.shared.clone(),
            fading: None,
            scratch: [
                vec![0.0; max_block as usize].into_boxed_slice(),
                vec![0.0; max_block as usize].into_boxed_slice(),
            ],
            held: [false; 128],
            tail_frames: (TAIL.as_secs_f64() * sample_rate) as u64,
        }))
    }

    fn deactivate(&mut self) {
        self.stop_worker();
    }

    fn params(&mut self) -> Vec<ParamInfo> {
        Vec::new()
    }
    fn set_param(&mut self, _id: u32, _value: f64) {}

    fn save_state(&mut self) -> Result<Vec<u8>> {
        let index = self.shared.want.load(Ordering::Relaxed);
        let mut v = STATE_V1.to_vec();
        v.extend_from_slice(&(index as u16).to_le_bytes());
        v.extend_from_slice(&self.shared.gain.load(Ordering::Relaxed).to_le_bytes());
        let name = self
            .files
            .get(index as usize)
            .and_then(|f| f.file_name())
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
        v.extend_from_slice(&(name.len() as u16).to_le_bytes());
        v.extend_from_slice(name.as_bytes());
        Ok(v)
    }

    fn load_state(&mut self, data: &[u8]) -> Result<()> {
        if data.len() < 12 || &data[..4] != STATE_V1 {
            bail!("not an SFZ slot state");
        }
        let mut index = u16::from_le_bytes([data[4], data[5]]) as usize;
        let gain = f32::from_le_bytes([data[6], data[7], data[8], data[9]]);
        let len = u16::from_le_bytes([data[10], data[11]]) as usize;
        // The file by name first: the folder may have gained or lost files since.
        if let Some(name) = data
            .get(12..12 + len)
            .and_then(|n| std::str::from_utf8(n).ok())
            && let Some(n) = self
                .files
                .iter()
                .position(|f| f.file_name().is_some_and(|f| f.to_string_lossy() == name))
        {
            index = n;
        }
        if index < self.files.len() {
            self.set_preset(0, index as u8);
        }
        if gain.is_finite() {
            self.set_gain(gain);
        }
        Ok(())
    }

    fn presets(&self) -> Option<&[PresetInfo]> {
        Some(&self.presets)
    }

    fn preset(&self) -> Option<(u16, u8)> {
        Some((0, self.shared.want.load(Ordering::Relaxed) as u8))
    }

    fn set_preset(&mut self, bank: u16, program: u8) {
        if bank == 0 && usize::from(program) < self.files.len() {
            self.shared
                .want
                .store(u32::from(program), Ordering::Relaxed);
            if let Some(w) = &self.worker {
                w.thread.thread().unpark();
            }
        }
    }

    fn gain(&self) -> Option<f32> {
        Some(f32::from_bits(self.shared.gain.load(Ordering::Relaxed)))
    }

    fn set_gain(&mut self, gain: f32) {
        self.shared
            .gain
            .store(gain.clamp(0.0, 2.0).to_bits(), Ordering::Relaxed);
    }

    fn editor(&mut self) -> Option<&mut dyn PluginEditor> {
        None
    }
    fn on_main_thread(&mut self) {}
    fn service(&mut self, _now: Instant) {}
    fn next_deadline(&self) -> Option<Instant> {
        None
    }
    fn on_fd_ready(&mut self) {}
}

/// The loader thread of an active instrument.
struct Worker {
    stop: Arc<AtomicBool>,
    thread: JoinHandle<()>,
}

struct WorkerJob {
    files: Arc<[PathBuf]>,
    names: Vec<String>,
    shared: Arc<Shared>,
    to_audio: Producer<Loaded>,
    retired: Consumer<Synth>,
    sample_rate: f64,
    max_block: u32,
    /// The file of the newest synth sent to the audio thread.
    loaded: u32,
    stop: Arc<AtomicBool>,
}

impl WorkerJob {
    fn run(mut self) {
        loop {
            // Woken by a choice or a stop; the timeout frees retired synths and catches
            // program changes from the audio thread.
            std::thread::park_timeout(Duration::from_millis(50));
            while let Ok(old) = self.retired.pop() {
                drop(old);
            }
            if self.stop.load(Ordering::Relaxed) {
                return;
            }
            let want = self.shared.want.load(Ordering::Relaxed);
            if want == self.loaded || self.to_audio.is_full() {
                continue;
            }
            let Some(file) = self.files.get(want as usize) else {
                continue;
            };
            match build(file, self.sample_rate, self.max_block) {
                Ok(synth) => {
                    if self.to_audio.push((want, synth)).is_ok() {
                        self.loaded = want;
                    }
                }
                Err(e) => {
                    eprintln!(
                        "sfz: {e:#}; {} keeps playing",
                        self.names[self.loaded as usize]
                    );
                    // Back to what plays, unless something else was chosen meanwhile.
                    let _ = self.shared.want.compare_exchange(
                        want,
                        self.loaded,
                        Ordering::Relaxed,
                        Ordering::Relaxed,
                    );
                }
            }
        }
    }
}

/// The previous sound after a switch, ringing out.
struct Fading {
    synth: Synth,
    /// Frames it may still play.
    left: u64,
}

struct Processor {
    synth: Synth,
    /// The file `synth` plays.
    playing: u32,
    /// Files the instrument has.
    count: u32,
    incoming: Consumer<Loaded>,
    retire: Producer<Synth>,
    waker: Thread,
    shared: Arc<Shared>,
    fading: Option<Fading>,
    /// Where `fading` renders before it is mixed in.
    scratch: [Box<[f32]>; 2],
    /// Keys down now, released on the old synth at a switch.
    held: [bool; 128],
    tail_frames: u64,
}

impl Processor {
    /// Takes a newly loaded synth, if one came; the one playing rings out.
    fn take_new(&mut self) {
        if self.fading.is_some() {
            // One at a time: the last switch's tail is still ringing.
            return;
        }
        let Ok((index, synth)) = self.incoming.pop() else {
            return;
        };
        let mut old = std::mem::replace(&mut self.synth, synth);
        self.playing = index;
        // The new synth never saw these keys go down; the old one hears them go up now.
        for key in 0..128u8 {
            if std::mem::take(&mut self.held[key as usize]) {
                old.note_off(0, key, 0);
            }
        }
        old.cc(0, 64, 0);
        self.fading = Some(Fading {
            synth: old,
            left: self.tail_frames,
        });
    }

    fn event(&mut self, ev: &MidiEvent, delay: u32) {
        let s = &mut self.synth;
        match ev.kind() {
            MidiKind::NoteOn { key, vel, .. } => {
                self.held[key as usize & 127] = true;
                s.note_on(delay, key, vel);
            }
            MidiKind::NoteOff { key, vel, .. } => {
                self.held[key as usize & 127] = false;
                s.note_off(delay, key, vel);
            }
            MidiKind::PolyPressure { key, value, .. } => s.poly_aftertouch(delay, key, value),
            MidiKind::ChannelPressure { value, .. } => s.channel_aftertouch(delay, value),
            MidiKind::PitchBend { value, .. } => s.pitch_wheel(delay, value),
            MidiKind::Cc { cc, value, .. } => s.cc(delay, cc, value),
            MidiKind::ProgramChange { program, .. } => {
                // A sound of this instrument: the worker loads it, the rack shows it.
                if u32::from(program) < self.count {
                    self.shared
                        .want
                        .store(u32::from(program), Ordering::Relaxed);
                    self.waker.unpark();
                }
            }
            MidiKind::Other => {}
        }
    }
}

impl PluginProcessor for Processor {
    fn process(&mut self, io: &mut Io, events: &[MidiEvent]) -> ProcessStatus {
        self.take_new();
        let frames = io.frames();
        for ev in events {
            let delay = (ev.time as usize).min(frames.saturating_sub(1)) as u32;
            self.event(ev, delay);
        }
        self.synth.render(io.out_l, io.out_r);
        if let Some(f) = &mut self.fading {
            let [l, r] = &mut self.scratch;
            let n = frames.min(l.len());
            f.synth.render(&mut l[..n], &mut r[..n]);
            for (o, x) in io.out_l.iter_mut().zip(&l[..n]) {
                *o += x;
            }
            for (o, x) in io.out_r.iter_mut().zip(&r[..n]) {
                *o += x;
            }
            f.left = f.left.saturating_sub(frames as u64);
            if (f.left == 0 || f.synth.active_voices() == 0) && !self.retire.is_full() {
                let f = self.fading.take().expect("fading");
                let _ = self.retire.push(f.synth);
                self.waker.unpark();
            }
        }
        let gain = f32::from_bits(self.shared.gain.load(Ordering::Relaxed));
        if gain != 1.0 {
            for x in io.out_l.iter_mut().chain(io.out_r.iter_mut()) {
                *x *= gain;
            }
        }
        // Stay awake while a chosen sound is on its way, or it would wait for the next note.
        let switching = self.shared.want.load(Ordering::Relaxed) != self.playing;
        if events.is_empty()
            && self.fading.is_none()
            && !switching
            && self.synth.active_voices() == 0
        {
            ProcessStatus::Sleep
        } else {
            ProcessStatus::Continue
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tempdir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("merlin-sfz-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn names_ids_and_folders() {
        assert!(is_sfz(Path::new("/x/Piano.SFZ")));
        assert!(!is_sfz(Path::new("/x/piano.sf2")));
        let info = info_for(Path::new("/libs/Rhodes/_Rhodes-stereo.sfz"));
        assert_eq!(info.format, Format::Sfz);
        assert_eq!(info.id, "Rhodes/_Rhodes-stereo.sfz");
        assert_eq!(info.name, "Rhodes-stereo");
        assert!(info.is_instrument());
        let folder = collection_info(Path::new("/libs/Rhodes"));
        assert_eq!(folder.id, "sfz-folder:Rhodes");
        assert!(is_source(Path::new("/libs/Rhodes"), &folder.id));
        assert!(!is_source(Path::new("/libs/Fonts"), "folder:Fonts"));

        let d = tempdir("files");
        for f in ["b.sfz", "a.sfz", "notes.txt", "c.sfzh"] {
            std::fs::write(d.join(f), "").unwrap();
        }
        std::fs::create_dir(d.join("samples")).unwrap();
        let names: Vec<_> = files_of(&d)
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["a.sfz", "b.sfz"]);
        let p = SfzPlugin::open(&d).unwrap();
        assert_eq!(p.info().format, Format::Sfz);
        let sounds: Vec<_> = p.presets().unwrap().iter().map(|p| &p.name).collect();
        assert_eq!(sounds, ["a", "b"]);
        assert!(SfzPlugin::open(&d.join("missing.sfz")).is_err());
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn the_chosen_file_is_found_by_name_after_the_folder_changes() {
        let d = tempdir("state");
        for f in ["a.sfz", "b.sfz", "c.sfz"] {
            std::fs::write(d.join(f), "").unwrap();
        }
        let mut p = SfzPlugin::open(&d).unwrap();
        p.set_preset(0, 2);
        p.set_gain(0.5);
        assert_eq!(p.preset(), Some((0, 2)));
        let state = p.save_state().unwrap();

        // "a" goes away: "c" is now the second file.
        std::fs::remove_file(d.join("a.sfz")).unwrap();
        let mut q = SfzPlugin::open(&d).unwrap();
        q.load_state(&state).unwrap();
        assert_eq!(q.preset(), Some((0, 1)));
        assert_eq!(q.gain(), Some(0.5));
        assert!(q.load_state(b"ISF2garbage").is_err());
        // A choice out of range is ignored.
        q.set_preset(0, 9);
        assert_eq!(q.preset(), Some((0, 1)));
        std::fs::remove_dir_all(&d).ok();
    }
}
