//! What this app does with a control surface (plan §4.10, Phase 4): the policy on top of a
//! device driver. A driver (see `devices/`) says what a device has; here are the decisions about
//! what the controls are *for* and what the lights show. Nothing here knows which device it is:
//! it works from the driver's [`Description`], so any device with pads, knobs, faders or
//! buttons gets a sensible layout.
//!
//! * [`profile`] turns a description into mappings (they sit behind the user's own).
//! * [`to_view`] turns the app's state into what the device should show.

use merlin_model::{ControlAction, Mapping, Source, SourceKind, Takeover};
use surface_api::{Description, Kind, Light, Rgb, Role, SourceKind as DeviceSource, View, page};

/// Slots and snapshots on one row of pads.
pub const PADS: usize = 8;

/// The controls' default jobs. `device` is the name the mappings are filed under (the control
/// port's readable name).
pub fn profile(device: &str, desc: &Description) -> Vec<Mapping> {
    let mut out = Vec::new();
    for c in desc.all_controls() {
        let n = c.col;
        let page = desc.page_of(&c.id);
        let action = match (c.kind, page) {
            (Kind::Pad, Some(page::MAIN)) => match c.row {
                0 => ControlAction::SelectSlot { slot: n },
                1 => ControlAction::RecallSnapshot { snapshot: n },
                _ => continue,
            },
            // While a "select" button is held: slots to select, and to layer.
            (Kind::Pad, Some(page::SELECT)) => match c.row {
                0 => ControlAction::SelectSlot { slot: n },
                1 => ControlAction::ToggleLayer { slot: n },
                _ => continue,
            },
            // Drum pads play instruments; the app only colours them.
            (Kind::Pad, _) => continue,
            (Kind::Knob, _) => ControlAction::SelectedKnob { index: n },
            (Kind::Fader, _) => match n as usize {
                i if i < PADS => ControlAction::SlotVolume { slot: n },
                PADS => ControlAction::MasterVolume,
                _ => continue,
            },
            (Kind::FaderButton, _) => match n as usize {
                i if i < PADS => ControlAction::ToggleMute { slot: n },
                PADS => ControlAction::Panic,
                _ => continue,
            },
            (Kind::Button, _) => match c.role {
                Some(Role::TrackPrev) => ControlAction::PrevSlot,
                Some(Role::TrackNext) => ControlAction::NextSlot,
                Some(Role::ScenePrev) => ControlAction::PrevPart,
                Some(Role::SceneNext) => ControlAction::NextPart,
                Some(Role::Play) => ControlAction::TransportToggle,
                Some(Role::Click) => ControlAction::ClickToggle,
                Some(Role::Tap) => ControlAction::TapTempo,
                _ => continue,
            },
        };
        let kind = match c.source.kind {
            DeviceSource::Cc(number) => SourceKind::Cc { number },
            DeviceSource::Note(key) => SourceKind::Note { key },
        };
        let mut m = Mapping::new(
            Source {
                device: Some(device.to_owned()),
                channel: c.source.channel,
                kind,
            },
            action,
        );
        m.label = Some(match page {
            Some(p) if p != page::MAIN => format!("{} ({p})", c.label),
            _ => c.label.clone(),
        });
        if matches!(c.kind, Kind::Knob | Kind::Fader) {
            // The device's controls do not know where the app's values are.
            m.takeover = Takeover::Pickup;
        }
        out.push(m);
    }
    out
}

/// How a slot is shown.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SlotState {
    pub loaded: bool,
    /// A target of the keyboard now.
    pub playing: bool,
    pub muted: bool,
    /// The slot's colour on screen.
    pub rgb: Rgb,
}

/// What the app wants a surface to show.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RigState {
    pub slots: [SlotState; PADS],
    pub snapshots: usize,
    pub snapshot: Option<usize>,
    /// The lines of a small screen.
    pub lines: [String; 2],
    /// Name and value of each knob (the slot that plays).
    pub knobs: [(String, String); PADS],
    /// The value text of each fader when it is touched: eight slot volumes and the master.
    pub faders: [String; PADS + 1],
    /// The colour the drum pads take: that of the slot that plays.
    pub drum: Option<Rgb>,
}

const GREY: Rgb = [110, 110, 110];
const WHITE: Rgb = [255, 255, 255];
const GREEN: Rgb = [70, 255, 70];
const BLUE: Rgb = [80, 90, 255];

/// The lights, text and labels for a device with this description.
pub fn to_view(state: &RigState, desc: &Description) -> View {
    let mut v = View {
        text: state.lines.to_vec(),
        ..View::default()
    };
    for c in desc.all_controls() {
        let i = c.col as usize;
        let slot = state.slots.get(i);
        let slot_light = |s: &SlotState| match (s.loaded, s.playing, s.muted) {
            (false, ..) => Light::OFF,
            (true, true, false) => Light::pulse(s.rgb),
            (true, _, true) => Light::steady(GREY),
            (true, false, false) => Light::steady(s.rgb),
        };
        let light = match (c.kind, desc.page_of(&c.id)) {
            (Kind::Pad, Some(page::MAIN)) => match (c.row, slot) {
                (0, Some(s)) => Some(slot_light(s)),
                (1, _) if i >= state.snapshots => Some(Light::OFF),
                (1, _) if state.snapshot == Some(i) => Some(Light::steady(GREEN)),
                (1, _) => Some(Light::steady(BLUE)),
                _ => None,
            },
            (Kind::Pad, Some(page::SELECT)) => match (c.row, slot) {
                (0, Some(s)) => Some(slot_light(s)),
                // Layering: bright for the slots that play, dim for the others.
                (1, Some(s)) if !s.loaded => Some(Light::OFF),
                (1, Some(s)) if s.playing => Some(Light::steady(s.rgb)),
                (1, Some(_)) => Some(Light::steady(GREY)),
                _ => None,
            },
            (Kind::Pad, Some(page::DRUM)) => Some(state.drum.map_or(Light::OFF, Light::steady)),
            (Kind::FaderButton, _) => slot.map(|s| match (s.loaded, s.muted) {
                (false, _) => Light::OFF,
                (true, false) => Light::steady(WHITE),
                (true, true) => Light::steady(GREY),
            }),
            _ => None,
        };
        if let Some(light) = light {
            v.lights.insert(c.id.clone(), light);
        }
        match c.kind {
            Kind::Knob if i < PADS => {
                v.labels.insert(c.id.clone(), state.knobs[i].clone());
            }
            Kind::Fader if i <= PADS => {
                let name = if i < PADS {
                    format!("Volume {}", i + 1)
                } else {
                    "Master".to_owned()
                };
                v.labels
                    .insert(c.id.clone(), (name, state.faders[i].clone()));
            }
            _ => {}
        }
    }
    v
}

#[cfg(test)]
mod tests {
    use surface_api::{
        Control, Description, Page, Screen, Source as DSource, SourceKind as DKind, id,
    };

    use super::*;
    use crate::{Context, Incoming, Mapper, Message, Outcome};

    /// A small pretend device: pads on two pages, two knobs, a fader and two buttons. (The real
    /// ones live in `devices/`; this keeps the policy tested without any of them.)
    fn device() -> Description {
        let pad = |page: &str, row: u8, col: u8, key: u8| Control {
            id: id::pad(page, row, col),
            kind: Kind::Pad,
            role: None,
            row,
            col,
            source: DSource {
                channel: None,
                kind: DKind::Note(key),
            },
            label: format!("pad {col}"),
            has_light: true,
        };
        let mut main = Vec::new();
        let mut select = Vec::new();
        let mut drum = Vec::new();
        for col in 0..8u8 {
            main.push(pad(page::MAIN, 0, col, 10 + col));
            main.push(pad(page::MAIN, 1, col, 20 + col));
            select.push(pad(page::SELECT, 0, col, 30 + col));
            select.push(pad(page::SELECT, 1, col, 40 + col));
            drum.push(pad(page::DRUM, 0, col, 50 + col));
        }
        let ctl = |id: String, kind, role, col: u8, number: u8| Control {
            id,
            kind,
            role,
            row: 0,
            col,
            source: DSource {
                channel: None,
                kind: DKind::Cc(number),
            },
            label: "x".into(),
            has_light: false,
        };
        let mut controls = vec![
            ctl(id::knob(2), Kind::Knob, None, 2, 22),
            ctl(id::fader(0), Kind::Fader, None, 0, 60),
            ctl(id::fader(8), Kind::Fader, None, 8, 68),
            ctl(id::fader_button(0), Kind::FaderButton, None, 0, 70),
            ctl(id::fader_button(8), Kind::FaderButton, None, 8, 78),
        ];
        for (name, role, n) in [
            ("next", Role::TrackNext, 90),
            ("prev", Role::TrackPrev, 91),
            ("play", Role::Play, 92),
            ("shift", Role::Shift, 93),
        ] {
            controls.push(ctl(id::button(name), Kind::Button, Some(role), 0, n));
        }
        Description {
            model: "Test".into(),
            pages: [("main", main), ("select", select), ("drum", drum)]
                .into_iter()
                .map(|(i, controls)| Page {
                    id: i.into(),
                    name: i.into(),
                    controls,
                })
                .collect(),
            controls,
            screen: Some(Screen {
                rows: 2,
                columns: 16,
            }),
        }
    }

    fn cc(device: &str, number: u8, value: u8) -> Incoming {
        Incoming {
            device: device.into(),
            channel: 15,
            message: Message::Cc { number, value },
        }
    }

    fn pad(device: &str, key: u8) -> Incoming {
        Incoming {
            device: device.into(),
            channel: 0,
            message: Message::Note {
                key,
                velocity: 100,
                on: true,
            },
        }
    }

    #[test]
    fn the_controls_get_their_jobs_from_the_description() {
        let dev = "Test: DAW";
        let mut m = Mapper::new(&[]);
        m.set_profile(profile(dev, &device()));
        let mut ctx = Context {
            loaded: 0b111,
            targets: 0b1,
            snapshots: 3,
            volumes: [1.0; merlin_model::MAX_SLOTS],
            primary: Some(0),
            ..Context::default()
        };
        assert_eq!(m.handle(&pad(dev, 11), &ctx), vec![Outcome::Select(1)]);
        assert_eq!(m.handle(&pad(dev, 20), &ctx), vec![Outcome::Recall(0)]);
        // The held "select" page: select and layer.
        assert_eq!(m.handle(&pad(dev, 32), &ctx), vec![Outcome::Select(2)]);
        assert_eq!(m.handle(&pad(dev, 41), &ctx), vec![Outcome::ToggleLayer(1)]);
        // Drum pads are left to the instruments.
        assert!(m.handle(&pad(dev, 50), &ctx).is_empty());
        assert_eq!(m.handle(&cc(dev, 90, 127), &ctx), vec![Outcome::Select(1)]);
        assert_eq!(m.handle(&cc(dev, 91, 127), &ctx), vec![Outcome::Select(2)]);
        assert_eq!(
            m.handle(&cc(dev, 92, 127), &ctx),
            vec![Outcome::TransportToggle]
        );
        assert!(
            m.handle(&cc(dev, 93, 127), &ctx).is_empty(),
            "no job for Shift"
        );
        assert_eq!(
            m.handle(&cc(dev, 70, 127), &ctx),
            vec![Outcome::ToggleMute(0)]
        );
        assert_eq!(m.handle(&cc(dev, 78, 127), &ctx), vec![Outcome::Panic]);
        // Knobs and faders pick up: nothing until they reach the value.
        ctx.knobs[0][2] = 0.5;
        assert!(m.handle(&cc(dev, 22, 5), &ctx).is_empty());
        assert!(!m.handle(&cc(dev, 22, 64), &ctx).is_empty());
        assert!(m.handle(&cc("Other: DAW", 22, 64), &ctx).is_empty());
        // The ninth fader is the master.
        ctx.master = 1.0;
        let out = m.handle(&cc(dev, 68, 127), &ctx);
        assert!(matches!(out.as_slice(), [Outcome::SetMaster(_)]), "{out:?}");
    }

    #[test]
    fn the_user_wins_over_the_layout() {
        let dev = "Test: DAW";
        let mine = Mapping::new(
            Source {
                device: Some(dev.into()),
                channel: None,
                kind: SourceKind::Note { key: 10 },
            },
            ControlAction::Panic,
        );
        let mut m = Mapper::new(&[mine]);
        m.set_profile(profile(dev, &device()));
        let ctx = Context {
            loaded: 1,
            ..Context::default()
        };
        assert_eq!(m.handle(&pad(dev, 10), &ctx), vec![Outcome::Panic]);
    }

    #[test]
    fn the_view_lights_each_page_and_labels_the_knobs() {
        let d = device();
        let mut s = RigState::default();
        s.slots[0] = SlotState {
            loaded: true,
            playing: true,
            muted: false,
            rgb: [255, 0, 0],
        };
        s.slots[1] = SlotState {
            loaded: true,
            playing: false,
            muted: true,
            rgb: [0, 255, 0],
        };
        s.snapshots = 2;
        s.snapshot = Some(1);
        s.drum = Some([255, 0, 0]);
        s.lines = ["1 Piano".into(), "Snap 2".into()];
        s.knobs[2] = ("Bass".into(), "+3dB".into());
        s.faders[8] = "1.00".into();
        let v = to_view(&s, &d);
        assert_eq!(v.lights["main.pad.0.0"], Light::pulse([255, 0, 0]));
        assert_eq!(v.lights["main.pad.0.1"], Light::steady(GREY), "muted");
        assert_eq!(v.lights["main.pad.0.2"], Light::OFF, "not loaded");
        assert_eq!(v.lights["main.pad.1.0"], Light::steady(BLUE));
        assert_eq!(
            v.lights["main.pad.1.1"],
            Light::steady(GREEN),
            "the current snapshot"
        );
        assert_eq!(v.lights["main.pad.1.2"], Light::OFF, "no such snapshot");
        assert_eq!(
            v.lights["select.pad.1.0"],
            Light::steady([255, 0, 0]),
            "layered"
        );
        assert_eq!(
            v.lights["select.pad.1.1"],
            Light::steady(GREY),
            "not layered"
        );
        assert_eq!(v.lights["drum.pad.0.5"], Light::steady([255, 0, 0]));
        assert_eq!(v.lights["fader_button.0"], Light::steady(WHITE));
        assert_eq!(v.text, vec!["1 Piano".to_owned(), "Snap 2".to_owned()]);
        assert_eq!(v.labels["knob.2"], ("Bass".to_owned(), "+3dB".to_owned()));
        assert_eq!(
            v.labels["fader.8"],
            ("Master".to_owned(), "1.00".to_owned())
        );
        assert_eq!(v.labels["fader.0"].0, "Volume 1");
        // Nothing playing: dark drum pads.
        s.drum = None;
        assert_eq!(to_view(&s, &d).lights["drum.pad.0.0"], Light::OFF);
    }
}
