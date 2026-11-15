//! Audio recording and the MIDI file, on the session. Not a DAW: one Record button that takes
//! what the main outputs play into a WAV file (and what was played on the keyboards into a MIDI
//! file beside it), and one MIDI file at a time that plays through the rig like the on-screen
//! keyboard. "Render" is the two together: record, play the file, stop when its sound has died.
//!
//! Nothing is recorded in the background: the audio thread does nothing for a take until Record
//! is pressed (the MIDI capture's rolling window is separate, see `capture.rs`).

use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use merlin_engine::{Command, SeqEvent, Sequence};
use merlin_recorder::{SmfOptions, Take, TakeState};

use super::Session;
use super::capture::{clock, create_unique, save_unique, stamp};

/// A render stops once the output has been this quiet this long after the file ended...
const TAIL_QUIET: f32 = 1e-4;
const TAIL_QUIET_FOR: Duration = Duration::from_millis(600);
/// ...or this long after it ended, whatever is still sounding (a drone, a noisy plugin).
const TAIL_MAX: Duration = Duration::from_secs(12);
/// A file the engine has not started by then is not going to start (the audio is not running).
const START_TIMEOUT: Duration = Duration::from_secs(3);
/// How often the session looks at a take or a render while one runs.
const POLL: Duration = Duration::from_millis(100);

/// The MIDI file that is loaded.
#[derive(Clone, Debug)]
pub struct MidiFileInfo {
    pub path: PathBuf,
    /// The file's name without `.mid`.
    pub name: String,
    pub seconds: f64,
    pub notes: usize,
}

/// A render that is running: the take is recording and the file playing (or its tail dying).
#[derive(Clone, Copy, Debug)]
pub(super) struct Render {
    /// The engine's count of file starts before this one was asked for.
    runs_before: u32,
    asked_at: Instant,
    ended_at: Option<Instant>,
    quiet_since: Option<Instant>,
}

impl Session {
    // ---- the audio take ---------------------------------------------------------------

    /// What the Record button shows.
    pub fn recording(&self) -> TakeState {
        self.backend()
            .map_or(TakeState::Idle, |b| b.recorder.take_state())
    }

    pub fn toggle_recording(&mut self) -> Result<()> {
        match self.recording() {
            TakeState::Idle => self.start_recording(None),
            TakeState::Recording(_) => {
                self.stop_recording();
                Ok(())
            }
            TakeState::Finishing => Ok(()),
        }
    }

    /// Starts a take into a new file in the recordings folder, named after `name` (or "Merlin
    /// recording") and the time.
    fn start_recording(&mut self, name: Option<&str>) -> Result<()> {
        let dir = self.capture_dir();
        let backend = self.backend().context("audio is not running")?;
        if backend.recorder.take_state() != TakeState::Idle {
            return Err(anyhow!("a recording is already running"));
        }
        let stem = format!("{} {}", name.unwrap_or("Merlin recording"), stamp());
        let (file, path) = create_unique(&dir, &stem, "wav")?;
        if let Err(e) = backend.recorder.start_take(file, path.clone()) {
            let _ = std::fs::remove_file(&path);
            return Err(e).context("cannot start recording");
        }
        self.say(format!("recording to {}", path.display()));
        Ok(())
    }

    fn stop_recording(&mut self) {
        // Stopping a render's take stops its file too.
        if self.render.take().is_some() {
            let _ = self.engine.send(Command::StopSequence);
        }
        if let Some(b) = self.backend() {
            b.recorder.stop_take();
        }
    }

    /// A take that has been finished: its MIDI goes beside it, and the user is told where.
    fn take_finished(&mut self, take: std::io::Result<Take>) {
        let text = match take {
            Ok(take) => match self.save_take_midi(&take) {
                Ok(midi) => take_report(&take, midi.as_deref()),
                Err(e) => format!(
                    "{} (its MIDI could not be saved: {e:#})",
                    take_report(&take, None)
                ),
            },
            Err(e) => format!("recording failed: {e}"),
        };
        if let Some(b) = self.backend() {
            b.recorder.release_hold();
        }
        self.say(text.clone());
        self.capture_notice = Some((Instant::now(), text));
    }

    /// What the keyboards played during the take, as a `.mid` with the WAV's name; nothing if
    /// nothing was played.
    fn save_take_midi(&self, take: &Take) -> Result<Option<PathBuf>> {
        let Some(backend) = self.backend() else {
            return Ok(None);
        };
        let capture = backend.recorder.capture_between(take.start, take.end);
        if capture.is_empty() {
            return Ok(None);
        }
        let stem = take
            .path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "Merlin recording".into());
        let options = SmfOptions {
            tempo: self.tempo(),
            beats_per_bar: self.rig.transport.beats_per_bar,
            title: stem.clone(),
            input_names: self.capture_input_names(),
            // Time zero is the WAV's first sample: the two line up in any DAW.
            origin: Some(take.start),
        };
        let dir = take.path.parent().unwrap_or(Path::new("."));
        save_unique(dir, &stem, "mid", &capture.to_smf(&options)).map(Some)
    }

    // ---- the MIDI file ----------------------------------------------------------------

    pub fn midi_file(&self) -> Option<&MidiFileInfo> {
        self.midi_file.as_ref()
    }

    /// Reads a MIDI file and hands it to the engine, in place of the one that was loaded.
    pub fn load_midi_file(&mut self, path: &Path) -> Result<()> {
        if self.render.is_some() {
            return Err(anyhow!(
                "a render is running: wait for it, or stop it first"
            ));
        }
        let bytes =
            std::fs::read(path).with_context(|| format!("cannot read {}", path.display()))?;
        let file =
            merlin_recorder::read_smf(&bytes).map_err(|e| anyhow!("{}: {e}", path.display()))?;
        if file.events.is_empty() {
            return Err(anyhow!("{} plays nothing", path.display()));
        }
        let sr = f64::from(self.sample_rate);
        let frame = |seconds: f64| (seconds * sr).round().max(0.0) as u64;
        let events = file
            .events
            .iter()
            .map(|e| SeqEvent {
                frame: frame(e.seconds),
                data: e.data,
            })
            .collect();
        let sequence = Sequence::new(events, frame(file.seconds));
        if self
            .engine
            .send(Command::SetSequence(Some(Box::new(sequence))))
            .is_err()
        {
            return Err(anyhow!("the audio engine is busy, try again"));
        }
        let name = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        self.say(format!(
            "MIDI file loaded: {name} ({}, {} notes)",
            clock(Duration::from_secs_f64(file.seconds)),
            file.notes()
        ));
        self.midi_file = Some(MidiFileInfo {
            path: path.to_owned(),
            name,
            seconds: file.seconds,
            notes: file.notes(),
        });
        Ok(())
    }

    /// Takes the MIDI file out.
    pub fn unload_midi_file(&mut self) {
        if self.render.is_some() {
            self.stop_recording();
        }
        let _ = self.engine.send(Command::SetSequence(None));
        self.midi_file = None;
    }

    pub fn midi_file_playing(&self) -> bool {
        self.engine.shared.sequence_playing.load(Ordering::Relaxed)
    }

    /// How far the file has played, in seconds.
    pub fn midi_file_position(&self) -> f64 {
        self.engine.shared.sequence_position.load(Ordering::Relaxed) as f64
            / f64::from(self.sample_rate.max(1))
    }

    /// Play from the start, or stop.
    pub fn toggle_midi_file(&mut self) -> Result<()> {
        if self.midi_file.is_none() {
            return Err(anyhow!("no MIDI file: drop one on the window first"));
        }
        if self.midi_file_playing() || self.render.is_some() {
            if self.render.is_some() {
                // Stopping a render stops its take too; what was recorded is kept.
                self.stop_recording();
            } else {
                let _ = self.engine.send(Command::StopSequence);
            }
            return Ok(());
        }
        self.engine
            .send(Command::PlaySequence)
            .map_err(|_| anyhow!("the audio engine is busy, try again"))
    }

    pub fn rendering(&self) -> bool {
        self.render.is_some()
    }

    /// Records the file played through the rig: starts a take, plays the file from its start,
    /// and stops the take once the sound has died away after the file's end.
    pub fn render_midi_file(&mut self) -> Result<()> {
        let Some(info) = self.midi_file.clone() else {
            return Err(anyhow!("no MIDI file: drop one on the window first"));
        };
        if self.render.is_some() {
            return Ok(());
        }
        if self.recording() != TakeState::Idle {
            return Err(anyhow!("stop the recording that is running first"));
        }
        let _ = self.engine.send(Command::StopSequence);
        self.start_recording(Some(&info.name))?;
        let runs_before = self.engine.shared.sequence_runs.load(Ordering::Relaxed);
        if self.engine.send(Command::PlaySequence).is_err() {
            if let Some(b) = self.backend() {
                b.recorder.stop_take();
            }
            return Err(anyhow!("the audio engine is busy, try again"));
        }
        self.render = Some(Render {
            runs_before,
            asked_at: Instant::now(),
            ended_at: None,
            quiet_since: None,
        });
        Ok(())
    }

    /// Called often: ends a render when its tail has died away, and saves a take that has been
    /// finished. Says when it wants to be called again.
    pub(super) fn service_recording(&mut self) -> Option<Instant> {
        drop(self.engine.take_retired_sequences());
        let now = Instant::now();
        if let Some(mut render) = self.render {
            let shared = &self.engine.shared;
            let started = shared.sequence_runs.load(Ordering::Relaxed) != render.runs_before;
            let playing = shared.sequence_playing.load(Ordering::Relaxed);
            let peak = self.backend().map_or(0.0, |b| b.recorder.take_peak());
            let mut done = false;
            if !started {
                if render.asked_at.elapsed() > START_TIMEOUT {
                    self.say("render: the MIDI file never started (is the audio running?)");
                    done = true;
                }
            } else if !playing {
                let ended = *render.ended_at.get_or_insert(now);
                if peak < TAIL_QUIET {
                    let quiet = *render.quiet_since.get_or_insert(now);
                    done = quiet.elapsed() >= TAIL_QUIET_FOR;
                } else {
                    render.quiet_since = None;
                }
                done |= ended.elapsed() >= TAIL_MAX;
            }
            self.render = Some(render);
            if done || self.recording() == TakeState::Idle {
                self.render = None;
                if let Some(b) = self.backend() {
                    b.recorder.stop_take();
                }
            }
        }
        if let Some(finished) = self.backend().and_then(|b| b.recorder.finished_take()) {
            self.take_finished(finished);
        }
        let busy = self.render.is_some()
            || self.recording() != TakeState::Idle
            || self.midi_file_playing();
        busy.then(|| now + POLL)
    }
}

/// "saved 1:23 of audio to …" and what else there is to know about the take.
fn take_report(take: &Take, midi: Option<&Path>) -> String {
    let mut text = format!(
        "saved {} of audio to {}",
        clock(take.duration()),
        take.path.display()
    );
    if let Some(m) = midi {
        let name = m.file_name().map(|n| n.to_string_lossy().into_owned());
        text.push_str(&format!(
            " and what was played to {}",
            name.unwrap_or_default()
        ));
    }
    if take.lost > 0 {
        let ms = take.lost as f64 * 1000.0 / f64::from(take.sample_rate.max(1));
        text.push_str(&format!(
            " ({ms:.0} ms are missing: the disk could not keep up)"
        ));
    }
    if take.full {
        text.push_str(" (stopped: a WAV file cannot be longer)");
    }
    text
}
