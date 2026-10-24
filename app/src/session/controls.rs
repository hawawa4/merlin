//! MIDI mappings and MIDI learn on the session (plan §4.10, Phase 3).
//!
//! The audio thread reports control messages (everything a controller sends, and what keyboards
//! send that something is mapped to); a small pump thread moves them to the main thread; here
//! they go through the [`Mapper`] and come out as the same operations the buttons on screen
//! perform.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use merlin_audio_io::{ControlEvent, MARKER_INPUT, friendly_name};
use merlin_control::{Context, Incoming, Message, Outcome};
use merlin_model::{
    ControlAction, FX_KNOBS, FxKind, Glue, INSTRUMENT, MAX_TEMPO, MIN_TEMPO, Mapping, SOUND_KNOBS,
    SamplerField, Source, SourceKind, Takeover,
};

use super::{MAX_SLOTS, Session, UI_SLOTS, UserEvent, insert_id};

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

/// How long to wait for a touch.
const TOUCH_WAIT: Duration = Duration::from_secs(30);
/// How long one poll may spend reading parameters.
const TOUCH_BUDGET: Duration = Duration::from_millis(6);

/// A touch-learn in progress.
pub(super) struct TouchLearn {
    slot: usize,
    insert: Option<usize>,
    ids: Vec<u32>,
    detector: crate::touch::Detector,
    cursor: usize,
    until: Instant,
}

impl Session {
    /// Takes in the control messages that arrived: learning one, or running what is mapped.
    pub fn poll_control(&mut self) {
        while let Ok(ev) = self.control_rx.try_recv() {
            if ev.input == MARKER_INPUT {
                self.marker_fired(u16::from(ev.data[0]) << 8 | u16::from(ev.data[1]));
                continue;
            }
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
            self.surface_input(&incoming);
            let ctx = self.control_context();
            for outcome in self.mapper.handle(&incoming, &ctx) {
                self.apply_outcome(outcome);
            }
        }
        self.update_surfaces();
    }

    fn control_context(&mut self) -> Context {
        // Where the mapped plugin parameters sit: what we last set, or their default.
        let mut params = std::collections::HashMap::new();
        let wanted: Vec<_> = self
            .rig
            .mappings
            .iter()
            .filter_map(|m| match m.action {
                ControlAction::SlotParam { slot, insert, id } => Some((slot, insert, id)),
                _ => None,
            })
            .collect();
        for key in wanted {
            if let Some(v) = self.param_position(key) {
                params.insert(key, v);
            }
        }
        let fx_bits = |f: &merlin_model::FxSettings| {
            u8::from(f.eq.enabled)
                | u8::from(f.comp.enabled) << 1
                | u8::from(f.delay.enabled) << 2
                | u8::from(f.reverb.enabled) << 3
        };
        let t = self.rig.transport;
        Context {
            pans: std::array::from_fn(|i| {
                self.rig.slots.get(i).map_or(0.5, |s| (s.pan + 1.0) / 2.0)
            }),
            click_level: t.click_volume,
            tempo: (self.tempo() - MIN_TEMPO) / (MAX_TEMPO - MIN_TEMPO),
            params,
            muted: (0..UI_SLOTS)
                .filter(|&i| self.rig.slots.get(i).is_some_and(|s| s.mute))
                .fold(0, |m, i| m | 1 << i),
            fx_on: std::array::from_fn(|i| self.rig.slots.get(i).map_or(0, |s| fx_bits(&s.fx))),
            bypassed: std::array::from_fn(|i| {
                self.rig.slots.get(i).map_or(0, |s| {
                    s.inserts
                        .iter()
                        .enumerate()
                        .filter(|(_, ins)| ins.as_ref().is_some_and(|x| x.bypass))
                        .fold(0, |m, (k, _)| m | 1 << k)
                })
            }),
            click_on: t.click,
            metronome_on: self.engine.shared.transport.metronome(),
            transport_running: self.transport_running(),
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
            knobs: std::array::from_fn(|i| {
                std::array::from_fn(|k| {
                    if i < UI_SLOTS && self.rig.slots[i].instrument.is_some() {
                        self.knob_value(i, k)
                    } else {
                        0.0
                    }
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
            Outcome::SetKnob(i, k, v) => self.set_knob(i, k, v),
            Outcome::Recall(n) => self.recall_snapshot(n),
            Outcome::NextPart => self.next_part(),
            Outcome::PrevPart => self.prev_part(),
            Outcome::NextSong => self.next_song(),
            Outcome::PrevSong => self.prev_song(),
            Outcome::TransportToggle => self.transport_toggle(),
            Outcome::ClickToggle => self.toggle_click(),
            Outcome::MetronomeToggle => self.toggle_metronome(),
            Outcome::TapTempo => self.tap_tempo(),
            Outcome::Panic => self.panic(),
            Outcome::CaptureMidi => self.capture_midi(),
            Outcome::SetPan(i, v) => self.set_pan(i, v * 2.0 - 1.0),
            Outcome::SetClickLevel(v) => {
                let on = self.rig.transport.click;
                self.set_click(on, v);
            }
            Outcome::SetTempo(v) => self.set_tempo(MIN_TEMPO + v * (MAX_TEMPO - MIN_TEMPO)),
            Outcome::SetParam(key, v) => self.set_param_position(key, v),
            Outcome::ToggleFx(i, fx) => self.toggle_fx(i, fx),
            Outcome::ToggleBypass(i, k) => self.toggle_insert_bypass(i, k),
        }
    }

    /// Script support: map a control (any device) to a named plugin parameter.
    pub fn map_param_named(
        &mut self,
        kind: SourceKind,
        slot: usize,
        insert: Option<usize>,
        query: &str,
        pickup: bool,
    ) -> anyhow::Result<()> {
        let q = query.to_lowercase();
        let info = self
            .plugin_params(slot, insert)
            .iter()
            .find(|p| p.name.to_lowercase().contains(&q))
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("no parameter matching {query:?}"))?;
        let mut m = Mapping::new(
            Source {
                device: None,
                channel: None,
                kind,
            },
            ControlAction::SlotParam {
                slot: slot as u8,
                insert: insert.map_or(INSTRUMENT, |i| i as u8),
                id: info.id,
            },
        );
        m.label = Some(info.name);
        if pickup {
            m.takeover = Takeover::Pickup;
        }
        self.add_mapping(m);
        Ok(())
    }

    /// For the script's report: where the mapped parameters sit.
    pub fn param_report(&self) -> Vec<String> {
        let mut v: Vec<_> = self
            .param_values
            .iter()
            .map(|((s, _, id), x)| format!("{}#{id}={x:.2}", *s + 1))
            .collect();
        v.sort();
        v
    }

    // ---- targets that only a mapping reaches ------------------------------------------------

    pub fn set_pan(&mut self, slot: usize, pan: f32) {
        if slot >= UI_SLOTS {
            return;
        }
        self.rig.slots[slot].pan = pan.clamp(-1.0, 1.0);
        self.dirty = true;
        self.push_slot_settings(slot);
    }

    pub fn toggle_fx(&mut self, slot: usize, fx: FxKind) {
        if slot >= UI_SLOTS {
            return;
        }
        let mut all = self.rig.slots[slot].fx;
        let on = match fx {
            FxKind::Eq => &mut all.eq.enabled,
            FxKind::Comp => &mut all.comp.enabled,
            FxKind::Delay => &mut all.delay.enabled,
            FxKind::Reverb => &mut all.reverb.enabled,
        };
        *on = !*on;
        self.set_fx(slot, all);
    }

    /// The plugin and call id a mapped parameter lives in.
    fn param_target(&self, key: merlin_control::ParamKey) -> Option<(usize, Option<usize>)> {
        let (slot, insert, _) = key;
        let slot = slot as usize;
        (slot < UI_SLOTS).then_some(())?;
        Some((slot, (insert != INSTRUMENT).then_some(insert as usize)))
    }

    /// Where a mapped parameter sits (0..=1 of its range): what was last set, else its default.
    fn param_position(&mut self, key: merlin_control::ParamKey) -> Option<f32> {
        // Knobs of the built-in effects and the SoundFont sound are read from where they are, so
        // a change made on screen and a change made by the controller are the same thing.
        let slot = key.0 as usize;
        match key.1 {
            FX_KNOBS => {
                let fx = self.rig.slots.get(slot)?.fx;
                let knob = merlin_model::fx_knob(key.2)?;
                return Some(knob.position(fx.knob_value(key.2)?));
            }
            SOUND_KNOBS => {
                let field = *SamplerField::ALL.get(key.2 as usize)?;
                let (lo, hi) = field.range();
                return Some(((self.sampler_field(slot, field)? - lo) / (hi - lo)).clamp(0.0, 1.0));
            }
            _ => {}
        }
        if let Some(&v) = self.param_values.get(&key) {
            return Some(v);
        }
        let (slot, insert) = self.param_target(key)?;
        let info = self
            .plugin_params(slot, insert)
            .iter()
            .find(|p| p.id == key.2)?
            .clone();
        let span = info.max - info.min;
        (span > 0.0).then(|| ((info.default - info.min) / span).clamp(0.0, 1.0) as f32)
    }

    /// Puts a mapped parameter at `v` (0..=1 of its range).
    pub fn set_param_position(&mut self, key: merlin_control::ParamKey, v: f32) {
        let slot = key.0 as usize;
        let v = if v.is_finite() {
            v.clamp(0.0, 1.0)
        } else {
            0.0
        };
        match key.1 {
            FX_KNOBS if slot < UI_SLOTS => {
                let mut fx = self.rig.slots[slot].fx;
                if fx.set_knob_position(key.2, v) {
                    self.set_fx(slot, fx);
                }
                return;
            }
            SOUND_KNOBS => {
                if let Some(&field) = SamplerField::ALL.get(key.2 as usize) {
                    let (lo, hi) = field.range();
                    self.set_sampler_field(slot, field, lo + (hi - lo) * v);
                }
                return;
            }
            _ => {}
        }
        let Some((slot, insert)) = self.param_target(key) else {
            return;
        };
        let Some(info) = self
            .plugin_params(slot, insert)
            .iter()
            .find(|p| p.id == key.2)
            .cloned()
        else {
            return;
        };
        let v = if v.is_finite() {
            v.clamp(0.0, 1.0)
        } else {
            0.0
        };
        let mut value = info.min + (info.max - info.min) * f64::from(v);
        if info.stepped {
            value = value.round();
        }
        let (call_id, loaded) = match insert {
            None => (slot, self.plugins[slot].as_mut()),
            Some(i) => (
                super::insert_id(slot, i),
                self.inserts[slot].get_mut(i).and_then(Option::as_mut),
            ),
        };
        if let Some(l) = loaded {
            let _call = self.guard.call(
                call_id,
                "setting a parameter",
                std::time::Duration::from_secs(10),
            );
            l.plugin.set_param(key.2, value);
        }
        self.param_values.insert(key, v);
        self.dirty = true;
    }

    // ---- the mapping list --------------------------------------------------------------

    pub fn mappings(&self) -> &[Mapping] {
        &self.rig.mappings
    }

    /// The mapping that drives `action`, and where it is in the list.
    pub fn mapping_for(&self, action: ControlAction) -> Option<(usize, &Mapping)> {
        self.rig
            .mappings
            .iter()
            .enumerate()
            .find(|(_, m)| m.action == action)
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

    pub fn set_mapping_switch(&mut self, index: usize, switch: merlin_model::Switch) {
        if let Some(m) = self.rig.mappings.get_mut(index) {
            m.switch = switch;
            self.mappings_changed();
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

    // ---- learn by touch -----------------------------------------------------------------

    /// Starts waiting for the user to turn a knob in the window of the slot's instrument (or of
    /// one of its inserts). The first parameter that moves becomes the thing to map: the next
    /// control the user moves on their device is mapped to it.
    pub fn start_touch_learn(&mut self, slot: usize, insert: Option<usize>) {
        self.cancel_learn();
        self.touch = None;
        if slot >= UI_SLOTS {
            return;
        }
        let params: Vec<(u32, f64)> = self
            .plugin_params(slot, insert)
            .iter()
            .map(|p| (p.id, (p.max - p.min).abs()))
            .collect();
        if params.is_empty() {
            self.say("this plugin lists no parameters to touch".to_owned());
            return;
        }
        let detector = crate::touch::Detector::new(params);
        self.touch = Some(TouchLearn {
            slot,
            insert,
            ids: detector.ids(),
            detector,
            cursor: 0,
            until: Instant::now() + TOUCH_WAIT,
        });
        self.say("turn a knob in the plugin's window".to_owned());
    }

    pub fn cancel_touch_learn(&mut self) {
        self.touch = None;
    }

    /// Which plugin is being watched for a touch, if any.
    pub fn touch_learning(&self) -> Option<(usize, Option<usize>)> {
        self.touch.as_ref().map(|t| (t.slot, t.insert))
    }

    /// Reads parameters for a few milliseconds (a plugin behind Wine answers slowly), and hands
    /// the one that moved over to the ordinary learn. Call it often while touch-learning.
    pub fn poll_touch_learn(&mut self, now: Instant) {
        let Some(mut t) = self.touch.take() else {
            return;
        };
        if now >= t.until {
            self.say("nothing was touched".to_owned());
            return;
        }
        let (slot, insert) = (t.slot, t.insert);
        let (call_id, loaded) = match insert {
            None => (slot, self.plugins[slot].as_mut()),
            Some(i) => (
                insert_id(slot, i),
                self.inserts[slot].get_mut(i).and_then(Option::as_mut),
            ),
        };
        let Some(loaded) = loaded else {
            return;
        };
        let _call = self
            .guard
            .call(call_id, "reading parameters", Duration::from_secs(10));
        let started = Instant::now();
        let mut found = None;
        while started.elapsed() < TOUCH_BUDGET {
            let Some(&id) = t.ids.get(t.cursor) else {
                t.cursor = 0;
                t.detector.end_pass();
                // Give the app its turn between passes.
                break;
            };
            t.cursor += 1;
            if let Some(v) = loaded.plugin.param_value(id)
                && let Some(hit) = t.detector.feed(id, v)
            {
                found = Some(hit);
                break;
            }
        }
        match found {
            Some(id) => {
                let name = self
                    .plugin_params(slot, insert)
                    .iter()
                    .find(|p| p.id == id)
                    .map(|p| p.name.clone())
                    .unwrap_or_default();
                self.say(format!("{name}: now move a control on your device"));
                self.start_learn_labelled(
                    ControlAction::SlotParam {
                        slot: slot as u8,
                        insert: insert.map_or(INSTRUMENT, |i| i as u8),
                        id,
                    },
                    Some(name),
                );
                self.learn_replaces = true;
            }
            None => self.touch = Some(t),
        }
    }

    // ---- learn --------------------------------------------------------------------------

    /// Waits for the next control the user moves or presses, and maps it to `action`.
    pub fn start_learn(&mut self, action: ControlAction) {
        self.start_learn_labelled(action, None);
    }

    /// As [`Session::start_learn`], keeping `label` with the mapping (a parameter's name).
    pub fn start_learn_labelled(&mut self, action: ControlAction, label: Option<String>) {
        self.learn = Some(action);
        self.learn_label = label;
        if let Some(b) = self.backend.as_ref() {
            b.interest.learning.store(true, Ordering::Relaxed);
        }
    }

    pub fn cancel_learn(&mut self) {
        self.learn = None;
        self.learn_label = None;
        self.learn_replaces = false;
        if let Some(b) = self.backend.as_ref() {
            b.interest.learning.store(false, Ordering::Relaxed);
        }
    }

    /// The name of what is being learned (a parameter's), if it has one.
    pub fn learn_label(&self) -> Option<&str> {
        self.learn_label.as_deref()
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
        let mut mapping = Mapping::new(
            Source {
                device: Some(inc.device.clone()),
                channel: Some(inc.channel),
                kind,
            },
            action,
        );
        if action.is_continuous() {
            mapping.takeover = Takeover::Pickup;
        }
        mapping.label = self.learn_label.take();
        // In map mode (and when learned by touch) a control has one source: teaching it again
        // replaces the old one.
        let replace = self.map_mode || self.learn_replaces;
        self.cancel_learn();
        if replace {
            self.rig.mappings.retain(|m| m.action != action);
        }
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
