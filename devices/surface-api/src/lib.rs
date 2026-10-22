//! A device-neutral way to describe a MIDI control surface and to drive its lights.
//!
//! This crate has no dependency on any application. A **driver** (one crate per device family,
//! for example `launchkey-mk3`) implements [`Driver`] and [`Surface`]; a **host** (an app, a DAW
//! plugin, a command-line tool) uses them. The split is:
//!
//! * The driver knows the device: which MIDI messages its controls send, which messages light
//!   its pads and write on its screen, how to switch it into the mode where it can be driven.
//!   It tells the host what the device has with a [`Description`], and turns a [`View`] (what
//!   the host wants shown) into MIDI bytes.
//! * The host knows what the controls are *for*. It reads the [`Description`] and decides, say,
//!   that the first pad row selects instruments and the knobs are sound controls. The same
//!   decisions then work for every device that has pads and knobs.
//!
//! Nothing here touches MIDI ports or threads: bytes come in and go out, so a host can run a
//! driver on any thread, and the types (with the `serde` feature) can cross a process
//! boundary if a driver is ever run out of process.
//!
//! # Vocabulary
//!
//! Controls have stable string ids: `knob.0`, `fader.8`, `fader_button.3`, `button.play`,
//! `main.pad.1.4` (page `main`, row 1, column 4). Counting starts at 0, rows from the top.
//! Pads belong to a **page**: a layout the device can be in (its normal grid, a "drum" layout,
//! a held-down selection layout). The driver follows which page is showing (the device reports
//! it) and only lights that page's pads.

use std::collections::HashMap;

/// A colour, 0-255 per channel. Drivers pick the nearest colour the device can show.
pub type Rgb = [u8; 3];

/// How a light behaves.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Effect {
    Steady,
    /// Breathes in time with the MIDI clock, on devices that can.
    Pulse,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Light {
    pub rgb: Rgb,
    pub effect: Effect,
}

impl Light {
    pub const OFF: Light = Light {
        rgb: [0, 0, 0],
        effect: Effect::Steady,
    };

    pub fn steady(rgb: Rgb) -> Self {
        Self {
            rgb,
            effect: Effect::Steady,
        }
    }

    pub fn pulse(rgb: Rgb) -> Self {
        Self {
            rgb,
            effect: Effect::Pulse,
        }
    }

    pub fn is_off(&self) -> bool {
        self.rgb == [0, 0, 0]
    }
}

/// What kind of control it is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Kind {
    Pad,
    /// An absolute knob (0-127).
    Knob,
    Fader,
    /// A button that belongs to a fader (select, arm, mute...).
    FaderButton,
    Button,
}

/// What a button is for on the device, when it has a well-known job. Hosts map these to their
/// own actions without knowing which device it is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Role {
    /// Select the previous / next track (instrument): the arrows beside the transport.
    TrackPrev,
    TrackNext,
    /// Scene launch up / down: move through the song.
    ScenePrev,
    SceneNext,
    Play,
    Stop,
    Record,
    Loop,
    Click,
    /// Quantise or tap tempo.
    Tap,
    Undo,
    Shift,
}

/// The MIDI message a control sends (and, for lit ones, is lit by).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum SourceKind {
    Cc(u8),
    Note(u8),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Source {
    /// MIDI channel 0-15 the control reports on; `None` if any will do.
    pub channel: Option<u8>,
    pub kind: SourceKind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Control {
    /// `knob.0`, `main.pad.1.4`, `button.play`: see the crate docs.
    pub id: String,
    pub kind: Kind,
    pub role: Option<Role>,
    /// For pads the row (0 = top) and column; for knobs, faders and fader buttons the number in
    /// `col` (counting from 0, left to right) and 0 in `row`; 0, 0 for buttons.
    pub row: u8,
    pub col: u8,
    pub source: Source,
    /// What the device calls it, for lists: "Track right", "Knob 3".
    pub label: String,
    /// Can the host light it?
    pub has_light: bool,
}

/// A layout the pads can be in.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Page {
    pub id: String,
    pub name: String,
    pub controls: Vec<Control>,
}

/// Well-known page ids. A driver offers the ones its device has.
pub mod page {
    /// The device's normal grid, the one a host lights to show its state.
    pub const MAIN: &str = "main";
    /// A grid shown while a "select" button is held.
    pub const SELECT: &str = "select";
    /// Drum pads: the notes go to the instruments, the host only colours them.
    pub const DRUM: &str = "drum";
}

/// The ids of controls.
pub mod id {
    pub fn pad(page: &str, row: u8, col: u8) -> String {
        format!("{page}.pad.{row}.{col}")
    }
    pub fn knob(n: u8) -> String {
        format!("knob.{n}")
    }
    pub fn fader(n: u8) -> String {
        format!("fader.{n}")
    }
    pub fn fader_button(n: u8) -> String {
        format!("fader_button.{n}")
    }
    pub fn button(name: &str) -> String {
        format!("button.{name}")
    }
}

/// A text display: `rows` lines of `columns` characters.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Screen {
    pub rows: u8,
    pub columns: u8,
}

/// What a device has.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Description {
    /// "Launchkey MK3 49".
    pub model: String,
    pub pages: Vec<Page>,
    /// Controls that are always there whatever the page: knobs, faders, buttons.
    pub controls: Vec<Control>,
    pub screen: Option<Screen>,
}

impl Description {
    /// Every control on every page, then the fixed ones.
    pub fn all_controls(&self) -> impl Iterator<Item = &Control> {
        self.pages
            .iter()
            .flat_map(|p| p.controls.iter())
            .chain(self.controls.iter())
    }

    pub fn page(&self, id: &str) -> Option<&Page> {
        self.pages.iter().find(|p| p.id == id)
    }

    /// The page a control is on (`None` for the fixed ones).
    pub fn page_of(&self, control_id: &str) -> Option<&str> {
        self.pages
            .iter()
            .find(|p| p.controls.iter().any(|c| c.id == control_id))
            .map(|p| p.id.as_str())
    }

    pub fn control(&self, id: &str) -> Option<&Control> {
        self.all_controls().find(|c| c.id == id)
    }
}

/// What the host wants the device to show. Controls the host does not mention are left dark
/// (or as the device has them); ids the device does not have are ignored.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct View {
    /// Lights by control id: pads of any page (only the page showing is sent), buttons, fader
    /// buttons.
    pub lights: HashMap<String, Light>,
    /// The lines of the screen.
    pub text: Vec<String>,
    /// What to show while a knob or fader is touched, by control id: its name and its value.
    pub labels: HashMap<String, (String, String)>,
}

/// One MIDI message to send.
pub type Message = Vec<u8>;

/// A device family. Cheap to create and stateless: one driver serves every device it matches.
pub trait Driver: Send + Sync {
    /// A stable id for settings and file names: `novation.launchkey-mk3`.
    fn id(&self) -> &str;

    /// Is the MIDI port with this readable name (`Launchkey MK3 49: LKMK3 DAW In`) the control
    /// port of a device this driver runs? Returns a surface for it. Only the port that carries
    /// the controls and the lights matches; a device's keyboard port does not.
    fn detect(&self, port_name: &str) -> Option<Box<dyn Surface>>;
}

/// One connected device.
pub trait Surface: Send {
    fn description(&self) -> &Description;

    /// The messages that put the device in the mode where it can be driven. The host sends them
    /// when it finds the device (and again when the device comes back).
    fn connect(&mut self) -> Vec<Message>;

    /// The messages that give the device back (screen cleared, normal mode).
    fn disconnect(&mut self) -> Vec<Message>;

    /// A MIDI message the device sent on its control port. Returns true if the surface now
    /// shows something else (a new page), so the host should render again.
    fn input(&mut self, bytes: &[u8]) -> bool;

    /// The page the pads are showing.
    fn page(&self) -> &str;

    /// The messages that bring the device in line with `view`; only what changed since the last
    /// call.
    fn render(&mut self, view: &View) -> Vec<Message>;
}
