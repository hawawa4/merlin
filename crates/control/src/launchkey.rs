//! The Novation Launchkey [MK3] (25, 37, 49, 61, 88) in DAW mode, from Novation's *Launchkey
//! [MK3] Programmer's Reference Guide*: the layout it gets by default (a controller profile),
//! and the lights and screen that show the app's state back (plan §4.10, Phase 4).
//!
//! Pure bytes in and out, no I/O: the app sends what [`Surface`] returns and feeds the device's
//! messages to the [`crate::Mapper`] like any other controller's.
//!
//! The device has two MIDI ports: "MIDI" (keys, wheels, pedal) and "DAW" (everything else in DAW
//! mode, and where its lights and screen are driven from). Only the DAW port is a controller.

use std::collections::HashMap;

use merlin_model::{ControlAction, Mapping, Source, SourceKind, Takeover};

/// Slots and snapshots on one row of pads.
pub const PADS: usize = 8;
/// The device's screen: two rows of this many characters.
pub const SCREEN_COLUMNS: usize = 16;

/// Which MK3 it is: the 25 and 37 have no faders, the 88 uses another SysEx header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Model {
    pub keys: u8,
}

impl Model {
    pub fn has_faders(self) -> bool {
        self.keys >= 49
    }

    fn header(self) -> [u8; 6] {
        [
            0xF0,
            0x00,
            0x20,
            0x29,
            0x02,
            if self.keys == 88 { 0x12 } else { 0x0F },
        ]
    }
}

/// Is this MIDI port the DAW port of a Launchkey MK3? Takes the readable device name
/// (`Launchkey MK3 49: LKMK3 DAW In`).
pub fn detect(device: &str) -> Option<Model> {
    let lower = device.to_ascii_lowercase();
    if !lower.contains("launchkey mk3") || !lower.contains("daw") {
        return None;
    }
    let rest = &lower[lower.find("launchkey mk3")? + "launchkey mk3".len()..];
    let keys: u8 = rest
        .trim_start()
        .split(|c: char| !c.is_ascii_digit())
        .next()?
        .parse()
        .ok()?;
    matches!(keys, 25 | 37 | 49 | 61 | 88).then_some(Model { keys })
}

// ---- what the device sends (decimal, as in the guide) ---------------------------------------

const POT_FIRST: u8 = 21;
const FADER_FIRST: u8 = 53;
const FADER_BUTTON_FIRST: u8 = 37;
const TOP_PADS: u8 = 96;
const BOTTOM_PADS: u8 = 112;
const TRACK_LEFT: u8 = 102;
const TRACK_RIGHT: u8 = 103;
const SCENE_UP: u8 = 104;
const SCENE_DOWN: u8 = 105;
const QUANTISE: u8 = 75;
const CLICK: u8 = 76;
const PLAY: u8 = 115;

/// The layout the device has without any setup, as mappings for its DAW port (`device` is that
/// port's name). They sit behind the ones in the rig: anything the user maps to the same control
/// wins.
pub fn profile(device: &str, model: Model) -> Vec<Mapping> {
    let mut out = Vec::new();
    let mut add = |kind: SourceKind, action: ControlAction, label: &str, knob: bool| {
        let mut m = Mapping::new(
            Source {
                device: Some(device.to_owned()),
                channel: None,
                kind,
            },
            action,
        );
        m.label = Some(label.to_owned());
        if knob {
            // The device's controls do not know where the app's values are.
            m.takeover = Takeover::Pickup;
        }
        out.push(m);
    };
    for i in 0..PADS as u8 {
        add(
            SourceKind::Note { key: TOP_PADS + i },
            ControlAction::SelectSlot { slot: i },
            &format!("pad {}, top row", i + 1),
            false,
        );
        add(
            SourceKind::Note {
                key: BOTTOM_PADS + i,
            },
            ControlAction::RecallSnapshot { snapshot: i },
            &format!("pad {}, bottom row", i + 1),
            false,
        );
        add(
            SourceKind::Cc {
                number: POT_FIRST + i,
            },
            ControlAction::SelectedMacro { index: i },
            &format!("knob {}", i + 1),
            true,
        );
        add(
            SourceKind::Cc {
                number: FADER_BUTTON_FIRST + i,
            },
            ControlAction::ToggleMute { slot: i },
            &format!("fader button {}", i + 1),
            false,
        );
        if model.has_faders() {
            add(
                SourceKind::Cc {
                    number: FADER_FIRST + i,
                },
                ControlAction::SlotVolume { slot: i },
                &format!("fader {}", i + 1),
                true,
            );
        }
    }
    if model.has_faders() {
        add(
            SourceKind::Cc {
                number: FADER_FIRST + PADS as u8,
            },
            ControlAction::MasterVolume,
            "fader 9",
            true,
        );
        add(
            SourceKind::Cc {
                number: FADER_BUTTON_FIRST + PADS as u8,
            },
            ControlAction::Panic,
            "fader button 9",
            false,
        );
    }
    let buttons = [
        (TRACK_LEFT, ControlAction::PrevSlot, "Track left"),
        (TRACK_RIGHT, ControlAction::NextSlot, "Track right"),
        (SCENE_UP, ControlAction::PrevPart, "Scene up"),
        (SCENE_DOWN, ControlAction::NextPart, "Scene down"),
        (PLAY, ControlAction::TransportToggle, "Play"),
        (CLICK, ControlAction::ClickToggle, "Click"),
        (QUANTISE, ControlAction::TapTempo, "Quantise (tap tempo)"),
    ];
    for (cc, action, label) in buttons {
        add(SourceKind::Cc { number: cc }, action, label, false);
    }
    out
}

// ---- what the device shows -------------------------------------------------------------------

/// Palette entries (the guide's colour table) the app uses.
pub mod colour {
    pub const OFF: u8 = 0;
    pub const GREY: u8 = 1;
    pub const WHITE: u8 = 3;
    pub const RED: u8 = 5;
    pub const YELLOW: u8 = 13;
    pub const GREEN: u8 = 21;
    pub const BLUE: u8 = 45;
}

/// A palette entry that looks like the slot's colour on screen, for each of the eight slots.
pub const SLOT_COLOURS: [u8; PADS] = [5, 45, 21, 13, 49, 33, 53, 17];

/// How a slot's pad is lit.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SlotView {
    pub loaded: bool,
    /// A target of the keyboard now.
    pub playing: bool,
    pub muted: bool,
}

/// What the app wants the device to show.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct View {
    pub slots: [SlotView; PADS],
    pub snapshots: usize,
    pub snapshot: Option<usize>,
    /// The two lines of the default display.
    pub lines: [String; 2],
    /// Name and value of each knob (the macros of the slot that plays).
    pub knobs: [(String, String); PADS],
    /// The value text of each fader when it is touched: eight slot volumes and the master.
    pub faders: [String; PADS + 1],
}

/// How an LED is lit: which of the guide's three channels carry the colour.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Light {
    Off,
    Static(u8),
    Pulse(u8),
}

impl Light {
    fn bytes(self, status_base: u8, index: u8) -> [u8; 3] {
        match self {
            Self::Off => [status_base, index, 0],
            Self::Static(c) => [status_base, index, c],
            Self::Pulse(c) => [status_base | 2, index, c],
        }
    }
}

/// Keeps what was last sent, so only changes go out.
#[derive(Debug)]
pub struct Surface {
    model: Model,
    leds: HashMap<(u8, u8), Light>,
    sent_text: HashMap<[u8; 2], Vec<u8>>,
}

fn text_bytes(text: &str) -> Vec<u8> {
    text.chars()
        .map(|c| {
            if (' '..='~').contains(&c) {
                c as u8
            } else {
                b'?'
            }
        })
        .take(SCREEN_COLUMNS)
        .collect()
}

impl Surface {
    pub fn new(model: Model) -> Self {
        Self {
            model,
            leds: HashMap::new(),
            sent_text: HashMap::new(),
        }
    }

    /// What to send when the device is found: DAW mode on. Everything is sent afresh by the next
    /// [`Surface::update`].
    pub fn connect(&mut self) -> Vec<Vec<u8>> {
        self.leds.clear();
        self.sent_text.clear();
        vec![vec![0x9F, 0x0C, 0x7F]]
    }

    /// What to send when the app leaves: give the screen back and leave DAW mode.
    pub fn disconnect(&mut self) -> Vec<Vec<u8>> {
        self.leds.clear();
        self.sent_text.clear();
        let mut clear = self.model.header().to_vec();
        clear.extend([0x06, 0xF7]);
        vec![clear, vec![0x9F, 0x0C, 0x00]]
    }

    fn led(&mut self, out: &mut Vec<Vec<u8>>, status_base: u8, index: u8, light: Light) {
        if self.leds.insert((status_base, index), light) != Some(light) {
            out.push(light.bytes(status_base, index).to_vec());
        }
    }

    fn sysex(&mut self, out: &mut Vec<Vec<u8>>, command: u8, id: &[u8], text: &str) {
        let mut m = self.model.header().to_vec();
        m.push(command);
        m.extend_from_slice(id);
        m.extend(text_bytes(text));
        m.push(0xF7);
        let key = [command, id.first().copied().unwrap_or(0)];
        if self.sent_text.insert(key, m.clone()).as_ref() != Some(&m) {
            out.push(m);
        }
    }

    /// The messages that bring the device in line with `view`.
    pub fn update(&mut self, view: &View) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        for (i, (&s, &colour)) in view.slots.iter().zip(&SLOT_COLOURS).enumerate() {
            let top = match (s.loaded, s.playing, s.muted) {
                (false, ..) => Light::Off,
                (true, true, false) => Light::Pulse(colour),
                (true, _, true) => Light::Static(colour::GREY),
                (true, false, false) => Light::Static(colour),
            };
            self.led(&mut out, 0x90, TOP_PADS + i as u8, top);
            let bottom = if i >= view.snapshots {
                Light::Off
            } else if view.snapshot == Some(i) {
                Light::Static(colour::GREEN)
            } else {
                Light::Static(colour::BLUE)
            };
            self.led(&mut out, 0x90, BOTTOM_PADS + i as u8, bottom);
            // The white LEDs under the faders: on while the slot sounds.
            let button = match (s.loaded, s.muted) {
                (false, _) => Light::Off,
                (true, false) => Light::Static(colour::WHITE),
                (true, true) => Light::Static(colour::GREY),
            };
            self.led(&mut out, 0xB0, FADER_BUTTON_FIRST + i as u8, button);
        }
        for (row, line) in view.lines.iter().enumerate() {
            self.sysex(&mut out, 0x04, &[row as u8], line);
        }
        for (k, (name, value)) in view.knobs.iter().enumerate() {
            let id = 0x38 + k as u8;
            self.sysex(&mut out, 0x07, &[id], name);
            self.sysex(&mut out, 0x08, &[id], value);
        }
        if self.model.has_faders() {
            for (k, value) in view.faders.iter().enumerate() {
                let id = 0x50 + k as u8;
                let name = if k < PADS {
                    format!("Volume {}", k + 1)
                } else {
                    "Master".to_owned()
                };
                self.sysex(&mut out, 0x07, &[id], &name);
                self.sysex(&mut out, 0x08, &[id], value);
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Context, Incoming, Mapper, Message, Outcome};

    #[test]
    fn only_the_daw_port_of_a_launchkey_mk3_is_found() {
        assert_eq!(
            detect("Launchkey MK3 49: LKMK3 DAW In"),
            Some(Model { keys: 49 })
        );
        assert_eq!(
            detect("Launchkey MK3 88: LKMK3 DAW Out"),
            Some(Model { keys: 88 })
        );
        assert_eq!(detect("Launchkey MK3 49: LKMK3 MIDI In"), None);
        assert_eq!(detect("Launchkey Mini MK4 DAW"), None);
        assert_eq!(detect("Launchkey MK3 50: LKMK3 DAW In"), None);
        assert!(!Model { keys: 37 }.has_faders());
        assert!(Model { keys: 61 }.has_faders());
    }

    fn cc(device: &str, number: u8, value: u8) -> Incoming {
        Incoming {
            device: device.into(),
            channel: 15,
            message: Message::Cc { number, value },
        }
    }

    #[test]
    fn the_default_layout_drives_the_app() {
        let dev = "Launchkey MK3 49: LKMK3 DAW In";
        let mut m = Mapper::new(&[]);
        m.set_profile(profile(dev, Model { keys: 49 }));
        let mut ctx = Context {
            loaded: 0b111,
            targets: 0b1,
            snapshots: 3,
            volumes: [1.0; merlin_model::MAX_SLOTS],
            primary: Some(0),
            ..Context::default()
        };
        let pad = |key: u8| Incoming {
            device: dev.into(),
            channel: 0,
            message: Message::Note {
                key,
                velocity: 100,
                on: true,
            },
        };
        assert_eq!(m.handle(&pad(97), &ctx), vec![Outcome::Select(1)]);
        assert_eq!(m.handle(&pad(112), &ctx), vec![Outcome::Recall(0)]);
        assert_eq!(m.handle(&cc(dev, 103, 127), &ctx), vec![Outcome::Select(1)]);
        assert_eq!(
            m.handle(&cc(dev, 37, 127), &ctx),
            vec![Outcome::ToggleMute(0)]
        );
        assert_eq!(
            m.handle(&cc(dev, 115, 127), &ctx),
            vec![Outcome::TransportToggle]
        );
        assert_eq!(m.handle(&cc(dev, 105, 127), &ctx), vec![Outcome::NextPart]);
        // Knobs and faders pick up: nothing until they reach the value.
        ctx.macros[0][2] = 0.5;
        assert!(m.handle(&cc(dev, 23, 5), &ctx).is_empty());
        assert!(!m.handle(&cc(dev, 23, 64), &ctx).is_empty());
        assert!(
            m.handle(&cc("Launchkey MK3 49: LKMK3 MIDI In", 23, 64), &ctx)
                .is_empty()
        );
        // The ninth fader is the master.
        ctx.master = 1.0;
        let out = m.handle(&cc(dev, 61, 127), &ctx);
        assert!(matches!(out.as_slice(), [Outcome::SetMaster(_)]), "{out:?}");
        // A 25-key model has no faders.
        let small = profile(dev, Model { keys: 25 });
        assert!(
            !small.iter().any(|p| p.action == ControlAction::MasterVolume
                || matches!(p.action, ControlAction::SlotVolume { .. }))
        );
    }

    #[test]
    fn the_user_wins_over_the_layout() {
        let dev = "Launchkey MK3 49: LKMK3 DAW In";
        let mine = Mapping::new(
            Source {
                device: Some(dev.into()),
                channel: None,
                kind: SourceKind::Note { key: 96 },
            },
            ControlAction::Panic,
        );
        let mut m = Mapper::new(&[mine]);
        m.set_profile(profile(dev, Model { keys: 49 }));
        let ctx = Context {
            loaded: 1,
            ..Context::default()
        };
        let pad = Incoming {
            device: dev.into(),
            channel: 0,
            message: Message::Note {
                key: 96,
                velocity: 90,
                on: true,
            },
        };
        assert_eq!(
            m.handle(&pad, &ctx),
            vec![Outcome::Panic],
            "only the user's mapping runs"
        );
    }

    #[test]
    fn lights_and_text_go_out_once_and_again_only_when_they_change() {
        let mut s = Surface::new(Model { keys: 49 });
        assert_eq!(s.connect(), vec![vec![0x9F, 0x0C, 0x7F]]);
        let mut v = View::default();
        v.slots[0] = SlotView {
            loaded: true,
            playing: true,
            muted: false,
        };
        v.slots[1].loaded = true;
        v.snapshots = 2;
        v.snapshot = Some(1);
        v.lines = ["1 Piano".into(), "Snap 2".into()];
        let first = s.update(&v);
        // Slot 1 plays: pulsing red on the first top pad (channel 3 of the guide).
        assert!(first.contains(&vec![0x92, 96, SLOT_COLOURS[0]]));
        // Slot 2 is loaded: steady colour.
        assert!(first.contains(&vec![0x90, 97, SLOT_COLOURS[1]]));
        // Snapshot 2 is the current one (green), snapshot 1 blue, 3 not there.
        assert!(first.contains(&vec![0x90, 113, colour::GREEN]));
        assert!(first.contains(&vec![0x90, 112, colour::BLUE]));
        assert!(first.contains(&vec![0x90, 119, 0]));
        // The screen's first line: header, 04h, row 0, the text.
        let mut line = vec![0xF0, 0, 0x20, 0x29, 2, 0x0F, 4, 0];
        line.extend(b"1 Piano");
        line.push(0xF7);
        assert!(first.contains(&line));
        // Nothing changed: nothing is sent.
        assert!(s.update(&v).is_empty());
        // Slot 1 stops: its pad goes steady, and only that.
        v.slots[0].playing = false;
        assert_eq!(s.update(&v), vec![vec![0x90, 96, SLOT_COLOURS[0]]]);
        // Leaving gives the screen back and leaves DAW mode.
        let bye = s.disconnect();
        assert_eq!(bye.last(), Some(&vec![0x9F, 0x0C, 0x00]));
        assert_eq!(bye[0][6..], [0x06, 0xF7]);
    }

    #[test]
    fn text_is_ascii_sixteen_wide_and_the_88_has_its_own_header() {
        assert_eq!(
            text_bytes("Grand Piano ♪ and much more text"),
            b"Grand Piano ? an"
        );
        let mut s = Surface::new(Model { keys: 88 });
        let mut v = View::default();
        v.lines[0] = "x".into();
        let out = s.update(&v);
        assert!(
            out.iter()
                .any(|m| m.starts_with(&[0xF0, 0, 0x20, 0x29, 2, 0x12]))
        );
        // A model without faders gets no fader texts.
        let mut small = Surface::new(Model { keys: 25 });
        assert!(
            !small
                .update(&View::default())
                .iter()
                .any(|m| m.len() > 6 && m[6] == 0x07 && m[7] >= 0x50)
        );
    }
}
