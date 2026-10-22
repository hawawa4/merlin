//! Controllers with a built-in layout and lights (plan §4.10, Phase 4): when a Launchkey MK3's
//! DAW port is connected as a controller, it is put in DAW mode, its default layout joins the
//! mappings, and its pads, buttons and screen show what the rig is doing. Leaving the app gives
//! the device back.

use merlin_audio_io::{DEVICE_OUTPUTS, DeviceMessage, friendly_name, sink_friendly_name};
use merlin_control::surface::{self, PADS, RigState, SlotState};
use surface_api::Surface;

use super::{Session, UI_SLOTS};

/// A controller we are driving.
pub(super) struct ActiveSurface {
    /// The device's readable name (what its mappings are filed under).
    pub device: String,
    /// Which of our device outputs goes to it, and the port it is connected to.
    port: u8,
    sink: String,
    surface: Box<dyn Surface>,
    /// Set while the connection to the device is being made: the first messages wait until then
    /// (the link is made a moment after the call, and what is sent before it is lost).
    wait_until: Option<std::time::Instant>,
}

/// How long to wait after connecting before the first message.
const LINK_DELAY: std::time::Duration = std::time::Duration::from_millis(300);

impl Session {
    /// Starts driving the controllers that are connected, and lets go of the ones that left.
    /// Called whenever the MIDI devices were looked at again.
    pub(super) fn sync_surfaces(&mut self) {
        let Some(b) = self.backend.as_ref() else {
            return;
        };
        let wanted: Vec<(String, Box<dyn Surface>)> = self
            .midi_sources
            .iter()
            .filter(|m| m.role == merlin_audio_io::InputRole::Controller)
            .filter_map(|m| {
                let name = friendly_name(&m.device);
                crate::drivers::detect(&name).map(|surface| (name, surface))
            })
            .collect();
        let sinks = b.hardware_midi_sinks();
        // Let go of what is gone (or no longer a controller).
        let mut left = Vec::new();
        self.surfaces.retain_mut(|s| {
            let keep = wanted.iter().any(|(n, _)| *n == s.device) && sinks.contains(&s.sink);
            if !keep {
                // The device may be unplugged already; the message is then lost, which is fine.
                left.push((s.port, s.surface.disconnect()));
            }
            keep
        });
        for (port, bye) in left {
            Self::send_all(self.backend.as_mut(), port, &bye);
            if let Some(b) = self.backend.as_ref() {
                b.connect_device_out(port as usize, None);
            }
        }
        // Take up the new ones.
        for (name, surface) in wanted {
            if self.surfaces.iter().any(|s| s.device == name) {
                continue;
            }
            let Some(sink) = sinks
                .iter()
                .find(|s| sink_friendly_name(s) == name)
                .cloned()
            else {
                continue;
            };
            let Some(port) =
                (0..DEVICE_OUTPUTS as u8).find(|p| !self.surfaces.iter().any(|s| s.port == *p))
            else {
                continue;
            };
            if let Some(b) = self.backend.as_ref() {
                b.connect_device_out(port as usize, Some(&sink));
            }
            self.say(format!(
                "controller: {} found ({name}); its layout is on",
                surface.description().model
            ));
            self.surfaces.push(ActiveSurface {
                device: name,
                port,
                sink,
                surface,
                wait_until: Some(std::time::Instant::now() + LINK_DELAY),
            });
        }
        let profile = self
            .surfaces
            .iter()
            .flat_map(|s| surface::profile(&s.device, s.surface.description()))
            .collect();
        self.mapper.set_profile(profile);
        self.update_surfaces();
    }

    fn send_all(
        backend: Option<&mut merlin_audio_io::JackBackend>,
        port: u8,
        messages: &[Vec<u8>],
    ) -> bool {
        let Some(b) = backend else { return false };
        let mut all = true;
        for m in messages {
            if let Some(m) = DeviceMessage::new(port, m) {
                all &= b.send_to_device(m);
            }
        }
        all
    }

    /// What the controllers should show now, in the app's own terms.
    fn rig_state(&self) -> RigState {
        let colour = |i: usize| {
            let c = crate::keyboard::COLORS[i];
            [c.r(), c.g(), c.b()]
        };
        let mut v = RigState::default();
        for i in 0..PADS.min(UI_SLOTS) {
            v.slots[i] = SlotState {
                loaded: self.plugins[i].is_some(),
                playing: self.targets.contains(&i),
                muted: self.rig.slots[i].mute,
                rgb: colour(i),
            };
        }
        v.snapshots = self.rig.snapshots.len();
        v.snapshot = self.last_snapshot;
        let primary = self.targets.first().copied();
        v.lines[0] = match primary.and_then(|i| Some((i, self.rig.slots[i].instrument.as_ref()?))) {
            Some((i, r)) => format!("{} {}", i + 1, r.name),
            None => "Merlin".to_owned(),
        };
        let tempo = self.current_tempo();
        v.lines[1] = match (self.position, self.last_snapshot) {
            (Some((s, p)), _) => format!("S{}.{} {tempo:.0}bpm", s + 1, p + 1),
            (None, Some(n)) => format!("Snap {} {tempo:.0}bpm", n + 1),
            (None, None) => format!("{tempo:.0} bpm"),
        };
        if let Some(i) = primary {
            for k in 0..PADS {
                v.knobs[k] = self.knob_label(i, k);
            }
            v.drum = (i < UI_SLOTS).then(|| colour(i));
        }
        for i in 0..PADS {
            v.faders[i] = format!("{:.2}", self.rig.slots[i].volume);
        }
        v.faders[PADS] = format!("{:.2}", self.rig.master_volume);
        v
    }

    /// Brings the controllers' lights and screens in line with the rig (cheap when nothing
    /// changed: only differences are sent).
    pub fn update_surfaces(&mut self) {
        if self.surfaces.is_empty() {
            return;
        }
        let state = self.rig_state();
        let now = std::time::Instant::now();
        for s in &mut self.surfaces {
            if let Some(t) = s.wait_until {
                if now < t {
                    continue;
                }
                s.wait_until = None;
                let hello = s.surface.connect();
                Self::send_all(self.backend.as_mut(), s.port, &hello);
            }
            let view = surface::to_view(&state, s.surface.description());
            let out = s.surface.render(&view);
            if !Self::send_all(self.backend.as_mut(), s.port, &out) {
                // The queue was full: forget what was sent so the next round sends it all.
                s.surface.connect();
            }
        }
    }

    /// A message from a controller we drive, before the mappings see it: the driver follows
    /// what the device reports about itself (which pad layout is showing).
    pub(super) fn surface_input(&mut self, incoming: &merlin_control::Incoming) {
        let merlin_control::Message::Cc { number, value } = incoming.message else {
            return;
        };
        let bytes = [0xB0 | incoming.channel, number, value];
        let mut changed = false;
        for s in self
            .surfaces
            .iter_mut()
            .filter(|s| s.device == incoming.device && s.wait_until.is_none())
        {
            changed |= s.surface.input(&bytes);
        }
        if changed {
            self.update_surfaces();
        }
    }

    /// The app is leaving: give the controllers back (screen cleared, DAW mode off).
    pub(super) fn release_surfaces(&mut self) {
        for mut s in std::mem::take(&mut self.surfaces) {
            let bye = s.surface.disconnect();
            Self::send_all(self.backend.as_mut(), s.port, &bye);
        }
        // The messages leave on the next audio cycle.
        std::thread::sleep(std::time::Duration::from_millis(60));
    }

    /// The built-in layouts in force, each with whether it is in use (the user has not mapped
    /// the same control).
    pub fn profile_mappings(&self) -> Vec<(merlin_model::Mapping, bool)> {
        self.mapper.profile_mappings()
    }

    /// The controllers being driven, for the settings screen: (device, description).
    pub fn surface_devices(&self) -> Vec<(String, String)> {
        self.surfaces
            .iter()
            .map(|s| (s.device.clone(), s.surface.description().model.clone()))
            .collect()
    }
}
