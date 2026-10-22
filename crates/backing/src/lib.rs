//! Backing tracks (plan §5): audio files decoded on a background thread and streamed to the
//! audio thread through a lock-free ring. The audio thread only ever pops samples; opening,
//! decoding and resampling all happen off it. Every stem comes out as stereo at the engine's
//! sample rate (a mono file is copied to both sides; extra channels are dropped).

use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use rubato::{
    Resampler, SincFixedIn, SincInterpolationParameters, SincInterpolationType, WindowFunction,
};
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{CODEC_TYPE_NULL, DecoderOptions};
use symphonia::core::errors::Error as SymError;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

/// Seconds of audio the ring holds ahead of the audio thread.
const RING_SECS: usize = 3;
/// A stem is ready to start once this many frames (or its whole file) are in the ring.
const READY_SECS: f32 = 1.0;
/// Frames fed to the resampler at a time.
const RESAMPLE_CHUNK: usize = 1024;

/// What opening a file found out about it.
#[derive(Clone, Debug, PartialEq)]
pub struct StemInfo {
    pub path: PathBuf,
    pub channels: usize,
    /// The file's own sample rate.
    pub sample_rate: u32,
    /// Length in seconds, when the file says.
    pub seconds: Option<f64>,
}

/// The audio thread's end: interleaved stereo frames, popped without locking.
pub struct StemReader {
    rx: rtrb::Consumer<f32>,
    eof: Arc<AtomicBool>,
}

impl StemReader {
    /// Fills `l` and `r` with as many frames as are ready; returns how many. A short count with
    /// [`finished`](Self::finished) false means the decoder fell behind.
    pub fn read(&mut self, l: &mut [f32], r: &mut [f32]) -> usize {
        let want = l.len().min(r.len());
        let have = self.rx.slots() / 2;
        let n = want.min(have);
        for i in 0..n {
            l[i] = self.rx.pop().unwrap_or(0.0);
            r[i] = self.rx.pop().unwrap_or(0.0);
        }
        n
    }

    /// The decoder has delivered everything and the ring is empty: the stem has ended.
    pub fn finished(&self) -> bool {
        self.eof.load(Ordering::Acquire) && self.rx.slots() == 0
    }

    /// Frames waiting in the ring.
    pub fn available(&self) -> usize {
        self.rx.slots() / 2
    }
}

/// The main thread's end: information, readiness, and stopping the decoder (on drop).
pub struct StemControl {
    pub info: StemInfo,
    stop: Arc<AtomicBool>,
    eof: Arc<AtomicBool>,
    ready_frames: usize,
    filled: Arc<AtomicUsize>,
    thread: Option<JoinHandle<()>>,
}

impl StemControl {
    /// Enough is decoded that starting now will not run dry.
    pub fn ready(&self) -> bool {
        self.eof.load(Ordering::Acquire) || self.filled.load(Ordering::Relaxed) >= self.ready_frames
    }

    /// The decoder stopped before the end because the file could not be read on.
    pub fn failed(&self) -> bool {
        self.thread.as_ref().is_some_and(|t| t.is_finished()) && !self.eof.load(Ordering::Acquire)
    }
}

impl Drop for StemControl {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Opens a file and starts decoding it from the beginning. Returns once the format is known;
/// decoding continues on its own thread.
pub fn open(path: &Path, engine_rate: u32) -> Result<(StemReader, StemControl)> {
    let file = File::open(path).with_context(|| format!("cannot open {}", path.display()))?;
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let stream = MediaSourceStream::new(Box::new(file), Default::default());
    let probed = symphonia::default::get_probe()
        .format(
            &hint,
            stream,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .map_err(|e| anyhow!("not an audio file this app can read ({e})"))?;
    let format = probed.format;
    let track = format
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
        .context("no audio in the file")?;
    let params = track.codec_params.clone();
    let track_id = track.id;
    let rate = params
        .sample_rate
        .context("the file does not say its sample rate")?;
    let channels = params.channels.map_or(2, |c| c.count()).max(1);
    let seconds = params.n_frames.map(|n| n as f64 / f64::from(rate));
    let decoder = symphonia::default::get_codecs()
        .make(&params, &DecoderOptions::default())
        .map_err(|e| anyhow!("no decoder for this file ({e})"))?;

    let ring = engine_rate as usize * RING_SECS * 2;
    let (tx, rx) = rtrb::RingBuffer::new(ring);
    let stop = Arc::new(AtomicBool::new(false));
    let eof = Arc::new(AtomicBool::new(false));
    let filled = Arc::new(AtomicUsize::new(0));
    let job = Job {
        format,
        decoder,
        track_id,
        tx,
        stop: stop.clone(),
        eof: eof.clone(),
        filled: filled.clone(),
        from_rate: rate,
        to_rate: engine_rate,
        channels,
    };
    let thread = std::thread::Builder::new()
        .name("backing-decoder".into())
        .spawn(move || {
            let _ = job.run();
        })?;
    let info = StemInfo {
        path: path.to_owned(),
        channels,
        sample_rate: rate,
        seconds,
    };
    Ok((
        StemReader {
            rx,
            eof: eof.clone(),
        },
        StemControl {
            info,
            stop,
            eof,
            ready_frames: (engine_rate as f32 * READY_SECS) as usize,
            filled,
            thread: Some(thread),
        },
    ))
}

struct Job {
    format: Box<dyn symphonia::core::formats::FormatReader>,
    decoder: Box<dyn symphonia::core::codecs::Decoder>,
    track_id: u32,
    tx: rtrb::Producer<f32>,
    stop: Arc<AtomicBool>,
    eof: Arc<AtomicBool>,
    filled: Arc<AtomicUsize>,
    from_rate: u32,
    to_rate: u32,
    channels: usize,
}

impl Job {
    /// Waits for room for one frame, or gives up when asked to stop.
    fn push(&mut self, l: f32, r: f32) -> bool {
        while self.tx.slots() < 2 {
            if self.stop.load(Ordering::Relaxed) {
                return false;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        let _ = self.tx.push(l);
        let _ = self.tx.push(r);
        self.filled.fetch_add(1, Ordering::Relaxed);
        true
    }

    fn run(mut self) -> Result<()> {
        let mut resampler = if self.from_rate != self.to_rate {
            let params = SincInterpolationParameters {
                sinc_len: 128,
                f_cutoff: 0.95,
                interpolation: SincInterpolationType::Linear,
                oversampling_factor: 128,
                window: WindowFunction::BlackmanHarris2,
            };
            Some(
                SincFixedIn::<f32>::new(
                    f64::from(self.to_rate) / f64::from(self.from_rate),
                    1.0,
                    params,
                    RESAMPLE_CHUNK,
                    2,
                )
                .map_err(|e| anyhow!("cannot resample: {e}"))?,
            )
        } else {
            None
        };
        // The resampler's output starts this many frames late: leave them out so the stem lines
        // up with the click.
        let mut skip = resampler.as_ref().map_or(0, |r| r.output_delay());
        let mut pending: [Vec<f32>; 2] = [Vec::new(), Vec::new()];
        let mut samples: Option<SampleBuffer<f32>> = None;
        loop {
            if self.stop.load(Ordering::Relaxed) {
                return Ok(());
            }
            // Do not decode far ahead of what the ring can take.
            while self.tx.slots() < RESAMPLE_CHUNK * 4 && !self.stop.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(5));
            }
            let packet = match self.format.next_packet() {
                Ok(p) => p,
                Err(SymError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
                Err(SymError::ResetRequired) => break,
                Err(e) => return Err(anyhow!("reading the file failed: {e}")),
            };
            if packet.track_id() != self.track_id {
                continue;
            }
            let decoded = match self.decoder.decode(&packet) {
                Ok(d) => d,
                // A damaged packet: skip it and carry on.
                Err(SymError::DecodeError(_)) => continue,
                Err(e) => return Err(anyhow!("decoding failed: {e}")),
            };
            let spec = *decoded.spec();
            let buf =
                samples.get_or_insert_with(|| SampleBuffer::new(decoded.capacity() as u64, spec));
            if buf.capacity() < decoded.capacity() * spec.channels.count() {
                *buf = SampleBuffer::new(decoded.capacity() as u64, spec);
            }
            buf.copy_interleaved_ref(decoded);
            let ch = self.channels.max(1);
            for frame in buf.samples().chunks_exact(ch) {
                let (l, r) = (frame[0], if ch > 1 { frame[1] } else { frame[0] });
                match resampler.as_mut() {
                    None => {
                        if !self.push(l, r) {
                            return Ok(());
                        }
                    }
                    Some(rs) => {
                        pending[0].push(l);
                        pending[1].push(r);
                        if pending[0].len() >= rs.input_frames_next() {
                            let out = rs
                                .process(&pending, None)
                                .map_err(|e| anyhow!("resampling failed: {e}"))?;
                            pending[0].clear();
                            pending[1].clear();
                            let first = skip.min(out[0].len());
                            skip -= first;
                            for (&l, &r) in out[0][first..].iter().zip(&out[1][first..]) {
                                if !self.push(l, r) {
                                    return Ok(());
                                }
                            }
                        }
                    }
                }
            }
        }
        // The tail: pad the last chunk with silence.
        if let Some(rs) = resampler.as_mut()
            && !pending[0].is_empty()
        {
            let need = rs.input_frames_next();
            let real = pending[0].len();
            pending[0].resize(need, 0.0);
            pending[1].resize(need, 0.0);
            if let Ok(out) = rs.process(&pending, None) {
                let keep =
                    (real as f64 * f64::from(self.to_rate) / f64::from(self.from_rate)) as usize;
                let from = skip.min(out[0].len());
                let to = out[0].len().min(keep + skip).max(from);
                for (&l, &r) in out[0][from..to].iter().zip(&out[1][from..to]) {
                    if !self.push(l, r) {
                        return Ok(());
                    }
                }
            }
        }
        self.eof.store(true, Ordering::Release);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 16-bit PCM WAV of a sine, `channels` channels.
    fn write_wav(path: &Path, rate: u32, channels: u16, secs: f32, hz: f32) {
        let frames = (rate as f32 * secs) as usize;
        let mut data = Vec::new();
        for i in 0..frames {
            let s = ((i as f32 / rate as f32 * hz * std::f32::consts::TAU).sin() * 0.5 * 32767.0)
                as i16;
            for _ in 0..channels {
                data.extend_from_slice(&s.to_le_bytes());
            }
        }
        let mut out = Vec::new();
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
        out.extend_from_slice(b"WAVEfmt ");
        out.extend_from_slice(&16u32.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&channels.to_le_bytes());
        out.extend_from_slice(&rate.to_le_bytes());
        out.extend_from_slice(&(rate * u32::from(channels) * 2).to_le_bytes());
        out.extend_from_slice(&(channels * 2).to_le_bytes());
        out.extend_from_slice(&16u16.to_le_bytes());
        out.extend_from_slice(b"data");
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&data);
        std::fs::write(path, out).unwrap();
    }

    /// Reads a whole stem the way the audio thread would, in blocks.
    fn drain(reader: &mut StemReader) -> (Vec<f32>, Vec<f32>) {
        let (mut l, mut r) = (Vec::new(), Vec::new());
        let (mut bl, mut br) = (vec![0.0; 256], vec![0.0; 256]);
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        while !reader.finished() && std::time::Instant::now() < deadline {
            let n = reader.read(&mut bl, &mut br);
            l.extend_from_slice(&bl[..n]);
            r.extend_from_slice(&br[..n]);
            if n == 0 {
                std::thread::sleep(Duration::from_millis(2));
            }
        }
        (l, r)
    }

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("irig-backing-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join(name)
    }

    #[test]
    fn a_stereo_file_at_the_engine_rate_comes_through_unchanged_in_length() {
        let path = temp("stereo.wav");
        write_wav(&path, 48_000, 2, 0.5, 440.0);
        let (mut reader, control) = open(&path, 48_000).unwrap();
        assert_eq!(control.info.channels, 2);
        assert!((control.info.seconds.unwrap() - 0.5).abs() < 0.01);
        let (l, r) = drain(&mut reader);
        assert_eq!(l.len(), 24_000);
        assert_eq!(l, r);
        let peak = l.iter().fold(0f32, |m, x| m.max(x.abs()));
        assert!((peak - 0.5).abs() < 0.01, "{peak}");
        assert!(control.ready());
    }

    #[test]
    fn a_mono_file_is_resampled_and_copied_to_both_sides() {
        let path = temp("mono44.wav");
        write_wav(&path, 44_100, 1, 1.0, 440.0);
        let (mut reader, _control) = open(&path, 48_000).unwrap();
        let (l, r) = drain(&mut reader);
        // One second at 44.1 kHz becomes about one second at 48 kHz.
        assert!((l.len() as i64 - 48_000).abs() < 1_500, "{}", l.len());
        assert_eq!(l, r);
        let peak = l.iter().fold(0f32, |m, x| m.max(x.abs()));
        assert!((0.45..0.55).contains(&peak), "{peak}");
    }

    #[test]
    fn dropping_the_control_stops_the_decoder() {
        let path = temp("long.wav");
        write_wav(&path, 48_000, 2, 30.0, 220.0);
        let (reader, control) = open(&path, 48_000).unwrap();
        // The ring fills (3 s) and the decoder waits; dropping must not hang.
        std::thread::sleep(Duration::from_millis(300));
        assert!(control.ready());
        drop(control);
        drop(reader);
    }

    #[test]
    fn things_that_are_not_audio_are_refused_with_a_reason() {
        let path = temp("notaudio.wav");
        std::fs::write(&path, b"this is not a wav file at all").unwrap();
        assert!(open(&path, 48_000).is_err());
        assert!(open(&temp("missing.wav"), 48_000).is_err());
    }
}
