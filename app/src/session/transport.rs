//! The transport on the session (plan §5): tempo, the click with its count-in, MIDI clock.
//! The audio thread runs the clock; here are the buttons, the tempo that songs and parts set,
//! tap tempo, and where the click is heard.

use std::time::{Duration, Instant};

use merlin_model::{MAX_COUNT_IN_BARS, MAX_TEMPO, MIN_TEMPO};

use super::Session;

/// Taps further apart than this start a new tempo.
const TAP_TIMEOUT: Duration = Duration::from_secs(2);
/// How many of the latest taps are averaged.
const TAPS_KEPT: usize = 5;

impl Session {
    /// The tempo the transport runs at now.
    pub fn tempo(&self) -> f32 {
        self.engine.shared.transport.tempo()
    }

    pub fn transport_running(&self) -> bool {
        self.engine.shared.transport.running()
    }

    pub fn counting_in(&self) -> bool {
        self.engine.shared.transport.counting_in()
    }

    /// Pushes the rig's transport settings to the audio thread (after a rig was loaded).
    pub(super) fn sync_transport(&mut self) {
        let t = self.rig.transport;
        let shared = &self.engine.shared.transport;
        shared.stop();
        shared.set_tempo(t.tempo);
        shared.set_beats_per_bar(t.beats_per_bar);
        shared.set_click(t.click, t.click_volume);
    }

    /// Sets the tempo for now and as the rig's own (what a tap or a nudge does).
    pub fn set_tempo(&mut self, bpm: f32) {
        let bpm = bpm.clamp(MIN_TEMPO, MAX_TEMPO);
        self.rig.transport.tempo = bpm;
        self.engine.shared.transport.set_tempo(bpm);
        self.dirty = true;
    }

    /// A song or part sets the tempo and the time signature, without touching the rig's own.
    pub(super) fn set_tempo_for_part(&mut self, bpm: f32, beats_per_bar: u8) {
        let shared = &self.engine.shared.transport;
        shared.set_tempo(bpm);
        shared.set_beats_per_bar(beats_per_bar);
    }

    pub fn set_beats_per_bar(&mut self, beats: u8) {
        self.rig.transport.beats_per_bar = beats.clamp(1, 16);
        self.engine
            .shared
            .transport
            .set_beats_per_bar(self.rig.transport.beats_per_bar);
        self.dirty = true;
    }

    pub fn set_count_in_bars(&mut self, bars: u8) {
        self.rig.transport.count_in_bars = bars.min(MAX_COUNT_IN_BARS);
        self.dirty = true;
    }

    pub fn set_click(&mut self, on: bool, volume: f32) {
        self.rig.transport.click = on;
        self.rig.transport.click_volume = volume.clamp(0.0, 1.0);
        self.engine
            .shared
            .transport
            .set_click(on, self.rig.transport.click_volume);
        self.dirty = true;
    }

    pub fn toggle_click(&mut self) {
        let t = self.rig.transport;
        self.set_click(!t.click, t.click_volume);
    }

    /// Starts (after the count-in) or stops.
    pub fn transport_toggle(&mut self) {
        let shared = &self.engine.shared.transport;
        if shared.running() || shared.counting_in() {
            shared.stop();
            self.say("transport stopped");
        } else {
            let t = self.rig.transport;
            // The song's own time signature is already on the audio thread.
            let beats = u32::from(t.count_in_bars) * u32::from(t.beats_per_bar);
            shared.start(beats.min(255) as u8);
            self.say(format!("transport started at {:.0} bpm", shared.tempo()));
        }
    }

    pub fn tap_tempo(&mut self) {
        let now = Instant::now();
        if self
            .taps
            .last()
            .is_some_and(|&last| now.duration_since(last) > TAP_TIMEOUT)
        {
            self.taps.clear();
        }
        self.taps.push(now);
        if self.taps.len() > TAPS_KEPT {
            self.taps.remove(0);
        }
        if self.taps.len() >= 2 {
            let span = self.taps[self.taps.len() - 1].duration_since(self.taps[0]);
            let beat = span.as_secs_f32() / (self.taps.len() - 1) as f32;
            if beat > 0.0 {
                self.set_tempo(60.0 / beat);
            }
        }
    }

    /// Where the click is heard: in the main outputs (`None`) or on two of the interface's own
    /// outputs, so it can go to the player's headphones only.
    pub fn set_click_output(&mut self, ports: Option<[String; 2]>) {
        self.settings.click_output = ports;
        self.apply_click_output();
        self.save_settings();
    }

    pub(super) fn apply_click_output(&mut self) {
        let Some(backend) = self.backend.as_ref() else {
            return;
        };
        match self.settings.click_output.as_ref() {
            Some([l, r]) => {
                backend.connect_click(Some([l.as_str(), r.as_str()]));
                self.engine.shared.transport.set_click_to_main(false);
            }
            None => {
                backend.connect_click(None);
                self.engine.shared.transport.set_click_to_main(true);
            }
        }
    }

    /// The interface's output ports, for choosing the click's.
    pub fn playback_ports(&self) -> Vec<String> {
        self.backend
            .as_ref()
            .map(|b| b.playback_ports())
            .unwrap_or_default()
    }
}
