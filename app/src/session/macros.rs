//! Slot macros on the session (plan §4.10, Phase 3): each slot has up to eight knobs, and each
//! knob moves any number of plugin parameters or SoundFont sound settings over a range. A knob
//! is moved from the Macros tab, a MIDI control, or by recalling a snapshot.

use std::time::Duration;

use merlin_hosting::ParamInfo;
use merlin_model::{MACROS_PER_SLOT, MAX_MACRO_TARGETS, Macro, MacroTarget, SamplerField};

use super::{Session, UI_SLOTS, insert_id};

/// A parameter call into a plugin is a plain queue push for CLAP and a short call for VST3.
const PARAM_CALL_LIMIT: Duration = Duration::from_secs(10);

impl Session {
    pub fn macros(&self, slot: usize) -> &[Macro] {
        &self.rig.slots[slot].macros
    }

    /// Where macro `k` of `slot` sits.
    pub fn macro_value(&self, slot: usize, k: usize) -> f32 {
        self.rig.slots[slot].macros.get(k).map_or(0.0, |m| m.value)
    }

    fn macro_mut(&mut self, slot: usize, k: usize) -> Option<&mut Macro> {
        if slot >= UI_SLOTS || k >= MACROS_PER_SLOT {
            return None;
        }
        let list = &mut self.rig.slots[slot].macros;
        if list.len() <= k {
            list.resize_with(k + 1, Macro::default);
        }
        Some(&mut list[k])
    }

    /// Moves a macro: stores its position and sets everything it drives.
    pub fn set_macro(&mut self, slot: usize, k: usize, value: f32) {
        let value = if value.is_finite() {
            value.clamp(0.0, 1.0)
        } else {
            0.0
        };
        let Some(m) = self.macro_mut(slot, k) else {
            return;
        };
        m.value = value;
        let targets = m.targets.clone();
        for t in &targets {
            self.apply_macro_target(slot, t, value);
        }
        self.dirty = true;
    }

    fn apply_macro_target(&mut self, slot: usize, target: &MacroTarget, value: f32) {
        match target {
            MacroTarget::Param { insert, id, .. } => {
                let Some(v) = target.param_value(value) else {
                    return;
                };
                let (call_id, loaded) = match *insert {
                    None => (slot, self.plugins[slot].as_mut()),
                    Some(i) => (
                        insert_id(slot, i as usize),
                        self.inserts[slot]
                            .get_mut(i as usize)
                            .and_then(Option::as_mut),
                    ),
                };
                if let Some(l) = loaded {
                    let _call = self
                        .guard
                        .call(call_id, "setting a parameter", PARAM_CALL_LIMIT);
                    l.plugin.set_param(*id, v);
                }
            }
            MacroTarget::Sampler { .. } => {
                let Some((field, v)) = target.sampler_value(value) else {
                    return;
                };
                if field == SamplerField::Level {
                    self.set_sampler_gain(slot, v);
                } else if let Some(mut tone) = self.sampler_tone(slot) {
                    match field {
                        SamplerField::Attack => tone.attack = v,
                        SamplerField::Release => tone.release = v,
                        SamplerField::Brightness => tone.cutoff = v,
                        SamplerField::Resonance => tone.resonance = v,
                        SamplerField::Tuning => tone.fine_tune = v,
                        SamplerField::Level => {}
                    }
                    self.set_sampler_tone(slot, tone);
                }
            }
        }
    }

    pub fn rename_macro(&mut self, slot: usize, k: usize, name: String) {
        if let Some(m) = self.macro_mut(slot, k) {
            m.name = name;
            self.dirty = true;
        }
    }

    /// Makes macro `k` drive a plugin parameter over its whole range.
    pub fn add_macro_param(
        &mut self,
        slot: usize,
        k: usize,
        insert: Option<usize>,
        info: &ParamInfo,
    ) {
        let target = MacroTarget::Param {
            insert: insert.map(|i| i as u8),
            id: info.id,
            name: info.name.clone(),
            min: info.min,
            max: info.max,
            stepped: info.stepped,
            from: 0.0,
            to: 1.0,
        };
        // Assume the parameter sits at its default until the macro is moved.
        let span = info.max - info.min;
        let at = if span > 0.0 {
            ((info.default - info.min) / span) as f32
        } else {
            0.0
        };
        self.add_macro_target(slot, k, target, at);
    }

    pub fn add_macro_sampler(&mut self, slot: usize, k: usize, field: SamplerField) {
        let tone = self.sampler_tone(slot).unwrap_or_default();
        let now = match field {
            SamplerField::Level => self.sampler_gain(slot).unwrap_or(1.0),
            SamplerField::Attack => tone.attack,
            SamplerField::Release => tone.release,
            SamplerField::Brightness => tone.cutoff,
            SamplerField::Resonance => tone.resonance,
            SamplerField::Tuning => tone.fine_tune,
        };
        let (lo, hi) = field.range();
        let at = (now - lo) / (hi - lo);
        self.add_macro_target(
            slot,
            k,
            MacroTarget::Sampler {
                field,
                from: 0.0,
                to: 1.0,
            },
            at,
        );
    }

    /// The first thing a macro drives sets where the knob sits (`at`, the target's current
    /// position), so adding it changes nothing; later ones follow the knob where it is.
    fn add_macro_target(&mut self, slot: usize, k: usize, target: MacroTarget, at: f32) {
        let Some(m) = self.macro_mut(slot, k) else {
            return;
        };
        if m.targets.len() >= MAX_MACRO_TARGETS || m.targets.contains(&target) {
            return;
        }
        let first = m.targets.is_empty();
        let value = m.value;
        m.targets.push(target.clone());
        if first {
            m.value = if at.is_finite() {
                at.clamp(0.0, 1.0)
            } else {
                0.0
            };
        } else {
            self.apply_macro_target(slot, &target, value);
        }
        self.dirty = true;
    }

    pub fn remove_macro_target(&mut self, slot: usize, k: usize, index: usize) {
        if let Some(m) = self.macro_mut(slot, k)
            && index < m.targets.len()
        {
            m.targets.remove(index);
            self.dirty = true;
        }
    }

    /// Sets which part of a target's range the macro's ends map to (fractions 0..=1).
    pub fn set_macro_range(&mut self, slot: usize, k: usize, index: usize, from: f32, to: f32) {
        let value = self.macro_value(slot, k);
        let Some(m) = self.macro_mut(slot, k) else {
            return;
        };
        let Some(t) = m.targets.get_mut(index) else {
            return;
        };
        t.set_range_fractions(from, to);
        let t = t.clone();
        self.apply_macro_target(slot, &t, value);
        self.dirty = true;
    }

    /// The parameters of a slot's instrument (`insert` `None`) or one of its inserts that a
    /// macro can drive. Asked of the plugin once, then remembered until it is unloaded.
    pub fn plugin_params(&mut self, slot: usize, insert: Option<usize>) -> &[ParamInfo] {
        let key = (slot, insert);
        if !self.param_cache.contains_key(&key) {
            let (call_id, loaded) = match insert {
                None => (slot, self.plugins[slot].as_mut()),
                Some(i) => (
                    insert_id(slot, i),
                    self.inserts[slot].get_mut(i).and_then(Option::as_mut),
                ),
            };
            let list = match loaded {
                Some(l) => {
                    let _call =
                        self.guard
                            .call(call_id, "listing its parameters", PARAM_CALL_LIMIT);
                    l.plugin
                        .params()
                        .into_iter()
                        .filter(|p| !p.hidden && !p.read_only)
                        .collect()
                }
                None => Vec::new(),
            };
            self.param_cache.insert(key, list);
        }
        &self.param_cache[&key]
    }

    /// A plugin went away: forget what it told us.
    pub(super) fn forget_params(&mut self, slot: usize, insert: Option<usize>) {
        match insert {
            None => self.param_cache.retain(|&(s, _), _| s != slot),
            Some(i) => {
                self.param_cache.remove(&(slot, Some(i)));
            }
        }
    }
}
