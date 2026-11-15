//! Audio takes: what leaves the main outputs, written to a WAV file from a press of Record to a
//! press of Stop. Unlike the MIDI capture nothing is kept in the background: while no take runs,
//! the audio thread only reads one atomic per cycle.
//!
//! The audio thread copies each cycle into a lock-free ring (no allocation, no lock); a writer
//! thread, started for the take, empties it into the file. The file's header is brought up to
//! date every second, so a take cut short by a crash still opens.

use std::fs::File;
use std::io::{self, BufWriter, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Seconds of audio the ring holds for a writer that is held up by the disk.
const RING_SECS: usize = 4;
/// How often the writer empties the ring.
const DRAIN_EVERY: Duration = Duration::from_millis(50);
/// How often the header is brought up to date while recording.
const HEADER_EVERY: Duration = Duration::from_secs(1);
/// A stop the audio thread does not take up by then (it is not running) finishes anyway.
const STOP_GRACE: Duration = Duration::from_millis(500);
/// Bytes per stereo frame: two 24-bit samples.
const FRAME_BYTES: u64 = 6;
/// The most audio a WAV file can say it holds (its sizes are 32-bit), in whole frames.
const MAX_DATA: u64 = (u32::MAX as u64 - 36) / FRAME_BYTES * FRAME_BYTES;

const IDLE: u8 = 0;
/// A take was asked for; the audio thread starts it at its next cycle.
const ARMED: u8 = 1;
const RECORDING: u8 = 2;
/// A stop was asked for; the audio thread takes it up at its next cycle.
const STOPPING: u8 = 3;
/// The audio thread has stopped writing; the writer empties the ring and closes the file.
const STOPPED: u8 = 4;

pub(crate) struct AudioShared {
    state: AtomicU8,
    /// The frame (recorder clock) of the take's first sample, and the one after its last.
    start: AtomicU64,
    end: AtomicU64,
    /// Frames handed to the writer, and frames lost because the ring was full.
    frames: AtomicU64,
    lost: AtomicU64,
    /// The loudest sample of the last cycle that was recorded, as `f32` bits.
    last_peak: AtomicU32,
    /// The file reached the size a WAV can have.
    full: AtomicBool,
    /// The writer's end of the ring: only the writer thread of the take that runs locks it (the
    /// audio thread never does).
    ring: Mutex<rtrb::Consumer<f32>>,
}

impl AudioShared {
    fn ring(&self) -> std::sync::MutexGuard<'_, rtrb::Consumer<f32>> {
        self.ring.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// The audio thread's end.
pub(crate) struct AudioTap {
    tx: rtrb::Producer<f32>,
    shared: Arc<AudioShared>,
}

impl AudioTap {
    /// Hands one cycle of the main outputs to the take, if one is running. `cycle_start` is the
    /// recorder's frame clock at the cycle's first frame.
    pub(crate) fn write(&mut self, cycle_start: u64, l: &[f32], r: &[f32]) {
        let s = &self.shared;
        match s.state.load(Ordering::Acquire) {
            RECORDING => {}
            ARMED => {
                s.start.store(cycle_start, Ordering::Relaxed);
                s.frames.store(0, Ordering::Relaxed);
                s.lost.store(0, Ordering::Relaxed);
                // The main thread may have asked for a stop since: then this take never began.
                if s.state
                    .compare_exchange(ARMED, RECORDING, Ordering::AcqRel, Ordering::Acquire)
                    .is_err()
                {
                    return;
                }
            }
            STOPPING => {
                s.end.store(cycle_start, Ordering::Relaxed);
                // Release: every sample pushed before is visible to whoever sees STOPPED.
                let _ = s.state.compare_exchange(
                    STOPPING,
                    STOPPED,
                    Ordering::AcqRel,
                    Ordering::Relaxed,
                );
                return;
            }
            _ => return,
        }
        let n = l.len().min(r.len());
        let peak = l[..n]
            .iter()
            .chain(&r[..n])
            .fold(0f32, |m, x| m.max(x.abs()));
        s.last_peak.store(peak.to_bits(), Ordering::Relaxed);
        match self.tx.write_chunk_uninit(n * 2) {
            Ok(chunk) => {
                chunk.fill_from_iter(l[..n].iter().zip(&r[..n]).flat_map(|(&a, &b)| [a, b]));
                s.frames.fetch_add(n as u64, Ordering::Relaxed);
            }
            // The writer is behind: this cycle is lost (counted, and said when the take ends).
            Err(_) => {
                s.lost.fetch_add(n as u64, Ordering::Relaxed);
            }
        }
    }
}

/// A finished take.
#[derive(Clone, Debug)]
pub struct Take {
    pub path: PathBuf,
    /// Frames in the file.
    pub frames: u64,
    /// Frames that could not be written because the disk was too slow (left out of the file).
    pub lost: u64,
    /// The recorder's frame clock at the take's first frame and just after its last: what
    /// [`Recorder::capture_between`](crate::Recorder::capture_between) takes.
    pub start: u64,
    pub end: u64,
    /// The take stopped by itself because the file reached the largest size a WAV can have.
    pub full: bool,
    pub sample_rate: u32,
}

impl Take {
    pub fn duration(&self) -> Duration {
        Duration::from_secs_f64(self.frames as f64 / f64::from(self.sample_rate))
    }
}

/// What the take is doing.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TakeState {
    Idle,
    /// Recording, this long so far.
    Recording(Duration),
    /// Stopped; the file is being finished.
    Finishing,
}

/// The main thread's end.
pub(crate) struct AudioTakes {
    shared: Arc<AudioShared>,
    sample_rate: u32,
    writer: Mutex<Option<JoinHandle<io::Result<Take>>>>,
}

pub(crate) fn new(sample_rate: u32) -> (AudioTap, AudioTakes) {
    let (tx, rx) = rtrb::RingBuffer::new(sample_rate.max(1) as usize * RING_SECS * 2);
    let shared = Arc::new(AudioShared {
        state: AtomicU8::new(IDLE),
        start: AtomicU64::new(0),
        end: AtomicU64::new(0),
        frames: AtomicU64::new(0),
        lost: AtomicU64::new(0),
        last_peak: AtomicU32::new(0),
        full: AtomicBool::new(false),
        ring: Mutex::new(rx),
    });
    (
        AudioTap {
            tx,
            shared: shared.clone(),
        },
        AudioTakes {
            shared,
            sample_rate,
            writer: Mutex::new(None),
        },
    )
}

impl AudioTakes {
    fn writer(&self) -> std::sync::MutexGuard<'_, Option<JoinHandle<io::Result<Take>>>> {
        self.writer.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub(crate) fn start(&self, file: File, path: PathBuf) -> io::Result<()> {
        let mut writer = self.writer();
        if writer.is_some() || self.shared.state.load(Ordering::Acquire) != IDLE {
            return Err(io::Error::other("a recording is already running"));
        }
        {
            // Whatever a take cut short by a dead audio thread left behind.
            let mut rx = self.shared.ring();
            let left = rx.slots();
            if let Ok(chunk) = rx.read_chunk(left) {
                chunk.commit_all();
            }
        }
        self.shared.full.store(false, Ordering::Relaxed);
        self.shared.frames.store(0, Ordering::Relaxed);
        self.shared.lost.store(0, Ordering::Relaxed);
        self.shared.last_peak.store(0, Ordering::Relaxed);
        let job = Writer {
            shared: self.shared.clone(),
            sample_rate: self.sample_rate,
            path,
        };
        let shared = self.shared.clone();
        // Armed before the writer runs: it must not see IDLE and take the take for over.
        self.shared.state.store(ARMED, Ordering::Release);
        let handle = std::thread::Builder::new()
            .name("merlin-take".into())
            .spawn(move || {
                let result = job.run(file, &mut shared.ring());
                shared.state.store(IDLE, Ordering::Release);
                result
            });
        match handle {
            Ok(h) => {
                *writer = Some(h);
                Ok(())
            }
            Err(e) => {
                self.shared.state.store(IDLE, Ordering::Release);
                Err(e)
            }
        }
    }

    pub(crate) fn stop(&self) {
        let s = &self.shared;
        for from in [RECORDING, ARMED] {
            if s.state
                .compare_exchange(from, STOPPING, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                break;
            }
        }
        if let Some(h) = self.writer().as_ref() {
            h.thread().unpark();
        }
    }

    pub(crate) fn state(&self) -> TakeState {
        let running = self.writer().is_some();
        match self.shared.state.load(Ordering::Acquire) {
            ARMED => TakeState::Recording(Duration::ZERO),
            RECORDING => TakeState::Recording(Duration::from_secs_f64(
                self.shared.frames.load(Ordering::Relaxed) as f64 / f64::from(self.sample_rate),
            )),
            _ if running => TakeState::Finishing,
            _ => TakeState::Idle,
        }
    }

    /// The loudest sample of the last recorded cycle.
    pub(crate) fn last_peak(&self) -> f32 {
        f32::from_bits(self.shared.last_peak.load(Ordering::Relaxed))
    }

    /// The take that has been finished since the last call, if one has.
    pub(crate) fn finished(&self) -> Option<io::Result<Take>> {
        let mut writer = self.writer();
        if !writer.as_ref()?.is_finished() {
            return None;
        }
        let handle = writer.take()?;
        Some(
            handle
                .join()
                .unwrap_or_else(|_| Err(io::Error::other("the recording thread failed"))),
        )
    }
}

impl Drop for AudioTakes {
    /// The backend is going away: the take ends here, and its file is closed properly.
    fn drop(&mut self) {
        self.stop();
        if let Some(h) = self.writer().take() {
            let _ = h.join();
        }
    }
}

struct Writer {
    shared: Arc<AudioShared>,
    sample_rate: u32,
    path: PathBuf,
}

impl Writer {
    fn run(self, file: File, rx: &mut rtrb::Consumer<f32>) -> io::Result<Take> {
        let mut out = BufWriter::with_capacity(1 << 18, file);
        write_header(&mut out, self.sample_rate, 0)?;
        let mut data: u64 = 0;
        let mut bytes: Vec<u8> = Vec::with_capacity(1 << 16);
        let mut header_at = Instant::now();
        let mut stop_asked: Option<Instant> = None;
        let mut failed: Option<io::Error> = None;
        loop {
            // The state first: what the audio thread pushed before it said STOPPED is then in
            // the ring for the drain below.
            let state = self.shared.state.load(Ordering::Acquire);
            let available = rx.slots() / 2 * 2;
            if let Ok(chunk) = rx.read_chunk(available) {
                let (a, b) = chunk.as_slices();
                bytes.clear();
                for &x in a.iter().chain(b) {
                    let v = (x.clamp(-1.0, 1.0) * 8_388_607.0).round() as i32;
                    bytes.extend_from_slice(&v.to_le_bytes()[..3]);
                }
                chunk.commit_all();
                let room = MAX_DATA - data;
                let keep = (bytes.len() as u64).min(room) as usize;
                if failed.is_none() && keep > 0 {
                    match out.write_all(&bytes[..keep]) {
                        Ok(()) => data += keep as u64,
                        // Keep emptying the ring (the audio thread must not back up), but stop.
                        Err(e) => {
                            failed = Some(e);
                            self.ask_stop();
                        }
                    }
                }
                if keep < bytes.len() {
                    self.shared.full.store(true, Ordering::Relaxed);
                    self.ask_stop();
                }
            }
            match state {
                STOPPED | IDLE => break,
                STOPPING => {
                    let since = *stop_asked.get_or_insert_with(Instant::now);
                    if since.elapsed() > STOP_GRACE {
                        // The audio thread is not running: no more is coming.
                        let now = self.shared.start.load(Ordering::Relaxed)
                            + self.shared.frames.load(Ordering::Relaxed)
                            + self.shared.lost.load(Ordering::Relaxed);
                        self.shared.end.store(now, Ordering::Relaxed);
                        break;
                    }
                }
                _ => {}
            }
            if failed.is_none() && header_at.elapsed() >= HEADER_EVERY {
                header_at = Instant::now();
                if let Err(e) = update_header(&mut out, self.sample_rate, data) {
                    failed = Some(e);
                    self.ask_stop();
                }
            }
            std::thread::park_timeout(DRAIN_EVERY);
        }
        if let Some(e) = failed {
            return Err(e);
        }
        update_header(&mut out, self.sample_rate, data)?;
        let file = out.into_inner().map_err(|e| e.into_error())?;
        file.sync_all()?;
        Ok(Take {
            path: self.path,
            frames: data / FRAME_BYTES,
            lost: self.shared.lost.load(Ordering::Relaxed),
            start: self.shared.start.load(Ordering::Relaxed),
            end: self.shared.end.load(Ordering::Relaxed),
            full: self.shared.full.load(Ordering::Relaxed),
            sample_rate: self.sample_rate,
        })
    }

    fn ask_stop(&self) {
        let _ = self.shared.state.compare_exchange(
            RECORDING,
            STOPPING,
            Ordering::AcqRel,
            Ordering::Relaxed,
        );
    }
}

/// A 44-byte header for 24-bit stereo PCM holding `data` bytes of audio.
fn header(sample_rate: u32, data: u64) -> [u8; 44] {
    let data = data.min(MAX_DATA) as u32;
    let mut h = [0u8; 44];
    h[0..4].copy_from_slice(b"RIFF");
    h[4..8].copy_from_slice(&(36 + data).to_le_bytes());
    h[8..16].copy_from_slice(b"WAVEfmt ");
    h[16..20].copy_from_slice(&16u32.to_le_bytes());
    h[20..22].copy_from_slice(&1u16.to_le_bytes()); // PCM
    h[22..24].copy_from_slice(&2u16.to_le_bytes()); // stereo
    h[24..28].copy_from_slice(&sample_rate.to_le_bytes());
    h[28..32].copy_from_slice(&(sample_rate * FRAME_BYTES as u32).to_le_bytes());
    h[32..34].copy_from_slice(&(FRAME_BYTES as u16).to_le_bytes());
    h[34..36].copy_from_slice(&24u16.to_le_bytes());
    h[36..40].copy_from_slice(b"data");
    h[40..44].copy_from_slice(&data.to_le_bytes());
    h
}

fn write_header(out: &mut impl Write, sample_rate: u32, data: u64) -> io::Result<()> {
    out.write_all(&header(sample_rate, data))
}

/// Writes what is buffered and puts the sizes in the header, so the file opens as it is now.
fn update_header(out: &mut BufWriter<File>, sample_rate: u32, data: u64) -> io::Result<()> {
    out.flush()?;
    let file = out.get_mut();
    file.seek(SeekFrom::Start(0))?;
    file.write_all(&header(sample_rate, data))?;
    file.seek(SeekFrom::End(0))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: u32 = 48_000;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("merlin-take-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        let _ = std::fs::remove_file(&path);
        path
    }

    /// Runs the audio thread's side until `until` says stop (or ten seconds pass).
    fn cycles(tap: &mut AudioTap, frame: &mut u64, value: f32, mut until: impl FnMut(u64) -> bool) {
        let block = vec![value; 256];
        let deadline = Instant::now() + Duration::from_secs(10);
        while !until(*frame) && Instant::now() < deadline {
            tap.write(*frame, &block, &block);
            *frame += 256;
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    fn finish(takes: &AudioTakes) -> Take {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(t) = takes.finished() {
                return t.unwrap();
            }
            assert!(Instant::now() < deadline, "the take never finished");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// The samples of a 24-bit stereo WAV, checking its header on the way.
    fn read_wav(path: &std::path::Path) -> Vec<i32> {
        let b = std::fs::read(path).unwrap();
        assert_eq!(&b[0..4], b"RIFF");
        assert_eq!(&b[8..12], b"WAVE");
        assert_eq!(
            u32::from_le_bytes(b[4..8].try_into().unwrap()) as usize,
            b.len() - 8
        );
        assert_eq!(u16::from_le_bytes([b[22], b[23]]), 2);
        assert_eq!(u32::from_le_bytes(b[24..28].try_into().unwrap()), SR);
        assert_eq!(u16::from_le_bytes([b[34], b[35]]), 24);
        let data = u32::from_le_bytes(b[40..44].try_into().unwrap()) as usize;
        assert_eq!(data, b.len() - 44);
        b[44..]
            .as_chunks::<3>()
            .0
            .iter()
            .map(|s| i32::from_le_bytes([0, s[0], s[1], s[2]]) >> 8)
            .collect()
    }

    #[test]
    fn nothing_is_written_while_no_take_runs() {
        let (mut tap, takes) = new(SR);
        let mut frame = 0;
        let mut n = 0;
        cycles(&mut tap, &mut frame, 0.5, |_| {
            n += 1;
            n > 50
        });
        assert_eq!(takes.state(), TakeState::Idle);
        assert_eq!(takes.shared.ring().slots(), 0);
    }

    #[test]
    fn a_take_holds_what_was_played_from_record_to_stop() {
        let (mut tap, takes) = new(SR);
        let mut frame = 0;
        // Before the take: not recorded.
        cycles(&mut tap, &mut frame, 0.9, |f| f >= 1024);
        let path = temp("take.wav");
        takes
            .start(File::create(&path).unwrap(), path.clone())
            .unwrap();
        assert!(
            takes
                .start(File::create(temp("other.wav")).unwrap(), path.clone())
                .is_err()
        );
        cycles(&mut tap, &mut frame, 0.5, |f| f >= 1024 + 256 * 40);
        assert!(matches!(takes.state(), TakeState::Recording(_)));
        takes.stop();
        // After the stop: not recorded either.
        let mut more = 0;
        cycles(&mut tap, &mut frame, -0.9, |_| {
            more += 1;
            takes.state() == TakeState::Idle && more > 3
        });
        let take = finish(&takes);
        assert_eq!(take.frames, 256 * 40);
        assert_eq!(take.start, 1024);
        assert_eq!(take.end, 1024 + 256 * 40);
        assert_eq!(take.lost, 0);
        assert!(!take.full);
        let samples = read_wav(&path);
        assert_eq!(samples.len(), 256 * 40 * 2);
        let half = (0.5 * 8_388_607.0f32).round() as i32;
        assert!(samples.iter().all(|&s| s == half), "{:?}", &samples[..4]);
        // And another take can follow.
        let path2 = temp("take2.wav");
        takes
            .start(File::create(&path2).unwrap(), path2.clone())
            .unwrap();
        cycles(
            &mut tap,
            &mut frame,
            0.25,
            |_| matches!(takes.state(), TakeState::Recording(d) if d > Duration::ZERO),
        );
        takes.stop();
        cycles(&mut tap, &mut frame, 0.0, |_| {
            takes.state() == TakeState::Idle
        });
        let take = finish(&takes);
        assert!(take.frames > 0);
        assert_eq!(read_wav(&path2).len() as u64, take.frames * 2);
    }

    #[test]
    fn a_stop_without_an_audio_thread_still_finishes_the_file() {
        let (_tap, takes) = new(SR);
        let path = temp("dead.wav");
        takes
            .start(File::create(&path).unwrap(), path.clone())
            .unwrap();
        takes.stop();
        let take = finish(&takes);
        assert_eq!(take.frames, 0);
        assert!(read_wav(&path).is_empty());
    }

    #[test]
    fn samples_past_full_scale_are_clipped_not_wrapped() {
        let (mut tap, takes) = new(SR);
        let path = temp("loud.wav");
        takes
            .start(File::create(&path).unwrap(), path.clone())
            .unwrap();
        let mut frame = 0;
        cycles(
            &mut tap,
            &mut frame,
            3.0,
            |_| matches!(takes.state(), TakeState::Recording(d) if d > Duration::ZERO),
        );
        takes.stop();
        cycles(&mut tap, &mut frame, 0.0, |_| {
            takes.state() == TakeState::Idle
        });
        finish(&takes);
        let samples = read_wav(&path);
        assert!(!samples.is_empty());
        assert!(samples.iter().all(|&s| s == 8_388_607));
    }
}
