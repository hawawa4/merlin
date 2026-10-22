//! MIDI mappings and MIDI learn on the session (plan §4.10, Phase 3).
//!
//! The audio thread reports control messages (everything a controller sends, and what keyboards
//! send that something is mapped to); a small pump thread moves them to the main thread; here
//! they go through the [`Mapper`] and come out as the same operations the buttons on screen
//! perform.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::Duration;

use merlin_audio_io::{ControlEvent, friendly_name};
use merlin_control::{Context, Incoming, Message, Outcome};
use merlin_model::{ControlAction, Glue, Mapping, Source, SourceKind, Takeover};

use super::{MAX_SLOTS, Session, UI_SLOTS, UserEvent};

/// How often the pump looks for new control messages: a foot switch should feel instant, and
/// the wait costs next to nothing.
const PUMP_INTERVAL: Duration = Duration::from_millis(8);

/// Moves control messages from the audio thread's queue to the main thread and wakes it.
pub(super) fn spawn_pump(
    mut queue: rtrb::Consumer<ControlEvent>,
    to_main: mpsc::Sender<ControlEvent>,
    wake: winit::event_loop::EventLoopProxy<UserEvent>,
    stop: std::sync::Arc<AtomicBool>,
) {
    let _ = std::thread::Builder::new()
        .name("control-pump".into())
        .spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                let mut any = false;
                while let Ok(ev) = queue.pop() {
                    any = true;
                    if to_main.send(ev).is_err() {
                        return;
                    }
                }
                if any {
                    let _ = wake.send_event(UserEvent::Control);
                }
                std::thread::sleep(PUMP_INTERVAL);
            }
        });
}

/// A message as mappings see it, from the raw bytes.
fn decode(data: [u8; 3]) -> Option<(u8, Message)> {
    let channel = data[0] & 15;
    let message = match data[0] & 0xF0 {
        0x80 => Message::Note {
            key: data[1],
            velocity: 0,
            on: false,
        },
        0x90 => Message::Note {
            key: data[1],
            velocity: data[2],
            on: data[2] > 0,
        },
        0xB0 => Message::Cc {
            number: data[1],
            value: data[2],
        },
        0xC0 => Message::Program { number: data[1] },
        _ => return None,
    };
    Some((channel, message))
}

impl Session {
    /// Takes in the control messages that arrived: learning one, or running what is mapped.
    pub fn poll_control(&mut self) {
        while let Ok(ev) = self.control_rx.try_recv() {
            let Some((channel, message)) = decode(ev.data) else {
                continue;
            };
            let Some(device) = self
                .midi_sources
                .iter()
                .find(|m| m.input == ev.input)
                .map(|m| friendly_name(&m.device))
            else {
                continue;
            };
            let incoming = Incoming {
                device,
                channel,
                message,
            };
            self.last_control = Some(describe(&incoming));
            if self.learn.is_some() {
                self.learn_from(&incoming);
                continue;
            }
            let ctx = self.control_context();
            for outcome in self.mapper.handle(&incoming, &ctx) {
                self.apply_outcome(outcome);
            }
        }
    }

    fn control_context(&self) -> Context {
        Context {
            loaded: (0..UI_SLOTS)
                .filter(|&i| self.plugins[i].is_some())
                .fold(0, |m, i| m | 1 << i),
            targets: self.targets.iter().fold(0, |m, &i| m | 1 << i),
            volumes: std::array::from_fn(|i| self.rig.slots.get(i).map_or(1.0, |s| s.volume)),
            master: self.rig.master_volume,
            primary: self.targets.first().copied(),
            macros: std::array::from_fn(|i| {
                std::array::from_fn(|k| {
                    self.rig
                        .slots
                        .get(i)
                        .map_or(0.0, |_| self.macro_value(i, k))
                })
            }),
            snapshots: self.rig.snapshots.len(),
            snapshot: self.last_snapshot,
        }
    }

    fn apply_outcome(&mut self, outcome: Outcome) {
        match outcome {
            Outcome::Select(i) => self.select(i, false),
            Outcome::ToggleLayer(i) => self.select(i, true),
            Outcome::ToggleMute(i) => self.toggle_mute(i),
            Outcome::SetVolume(i, v) => self.set_volume(i, v),
            Outcome::SetMaster(v) => self.set_master_volume(v),
            Outcome::SetMacro(i, k, v) => self.set_macro(i, k, v),
            Outcome::Recall(n) => self.recall_snapshot(n),
            Outcome::Panic => self.panic(),
        }
    }

    // ---- the mapping list --------------------------------------------------------------

    pub fn mappings(&self) -> &[Mapping] {
        &self.rig.mappings
    }

    pub fn add_mapping(&mut self, mapping: Mapping) {
        if !mapping.is_valid() {
            return;
        }
        self.rig.mappings.push(mapping);
        self.mappings_changed();
    }

    pub fn remove_mapping(&mut self, index: usize) {
        if index < self.rig.mappings.len() {
            self.rig.mappings.remove(index);
            self.mappings_changed();
        }
    }

    pub fn set_mapping_glue(&mut self, index: usize, glue: Glue) {
        if let Some(m) = self.rig.mappings.get_mut(index) {
            m.glue = glue;
            if m.is_valid() {
                self.mappings_changed();
            } else {
                m.glue = Glue::default();
            }
        }
    }

    pub fn set_mapping_takeover(&mut self, index: usize, takeover: Takeover) {
        if let Some(m) = self.rig.mappings.get_mut(index) {
            m.takeover = takeover;
            self.mappings_changed();
        }
    }

    /// The mappings changed (or the rig was replaced): tell the mapper and the audio thread.
    pub(super) fn mappings_changed(&mut self) {
        self.dirty = true;
        self.sync_mappings();
    }

    pub(super) fn sync_mappings(&mut self) {
        self.mapper.set_mappings(&self.rig.mappings);
        self.refresh_interest();
    }

    /// Tells the audio thread which keyboard messages are mapped (they must not play).
    pub(super) fn refresh_interest(&mut self) {
        let Some(b) = self.backend.as_ref() else {
            return;
        };
        for input in &b.interest.inputs {
            input.clear();
        }
        for src in &self.midi_sources {
            let Some(wanted) = b.interest.inputs.get(src.input as usize - 1) else {
                continue;
            };
            let name = friendly_name(&src.device);
            for m in self.mapper.mappings() {
                if m.source.device.as_deref().is_some_and(|d| d != name) {
                    continue;
                }
                match m.source.kind {
                    SourceKind::Cc { number } => wanted.add_cc(number),
                    SourceKind::Note { key } => wanted.add_note(key),
                    SourceKind::Program => wanted.add_program(),
                }
            }
        }
    }

    // ---- learn --------------------------------------------------------------------------

    /// Waits for the next control the user moves or presses, and maps it to `action`.
    pub fn start_learn(&mut self, action: ControlAction) {
        self.learn = Some(action);
        if let Some(b) = self.backend.as_ref() {
            b.interest.learning.store(true, Ordering::Relaxed);
        }
    }

    pub fn cancel_learn(&mut self) {
        self.learn = None;
        if let Some(b) = self.backend.as_ref() {
            b.interest.learning.store(false, Ordering::Relaxed);
        }
    }

    pub fn learning(&self) -> Option<ControlAction> {
        self.learn
    }

    /// The last control message seen, in words, for the Controls tab.
    pub fn last_control(&self) -> Option<&str> {
        self.last_control.as_deref()
    }

    fn learn_from(&mut self, inc: &Incoming) {
        let Some(action) = self.learn else { return };
        // A key coming up or a pedal's release teaches nothing; wait for the press.
        let kind = match inc.message {
            Message::Cc { number, .. } => SourceKind::Cc { number },
            Message::Note {
                key,
                on: true,
                velocity,
            } if velocity > 0 => SourceKind::Note { key },
            Message::Program { .. } => SourceKind::Program,
            _ => return,
        };
        let mapping = Mapping {
            source: Source {
                device: Some(inc.device.clone()),
                channel: Some(inc.channel),
                kind,
            },
            action,
            takeover: if action.is_continuous() {
                Takeover::Pickup
            } else {
                Takeover::Jump
            },
            glue: Glue::default(),
        };
        self.cancel_learn();
        self.say(format!(
            "mapped {} to {}",
            mapping.source.describe(),
            mapping.action.describe()
        ));
        self.add_mapping(mapping);
    }
}

/// A message in words: `Launchpad · ch 1 · CC 20 = 127`.
fn describe(inc: &Incoming) -> String {
    let (what, value) = match inc.message {
        Message::Cc { number, value } => (format!("CC {number}"), format!(" = {value}")),
        Message::Note { key, velocity, on } => (
            format!("note {key}"),
            if on {
                format!(" on ({velocity})")
            } else {
                " off".into()
            },
        ),
        Message::Program { number } => ("Program Change".to_owned(), format!(" = {number}")),
    };
    format!("{} · ch {} · {what}{value}", inc.device, inc.channel + 1)
}

const _: () = assert!(UI_SLOTS <= MAX_SLOTS);
