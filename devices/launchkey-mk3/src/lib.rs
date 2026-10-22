//! Driver for the Novation Launchkey [MK3] (25, 37, 49, 61, 88) in DAW mode, written from
//! Novation's *Launchkey [MK3] Programmer's Reference Guide* (a public PDF) and checked on a
//! Launchkey MK3 49.
//!
//! The device has two MIDI ports: "MIDI" (keys, wheels, pedal, and the drum / chord pad layouts)
//! and "DAW" (everything else once DAW mode is on, and where its lights and screen are driven
//! from). [`LaunchkeyMk3`] matches the DAW port. A host sends it the bytes of
//! [`Surface::connect`](surface_api::Surface::connect), feeds it what the DAW port sends, and
//! sends what [`render`](surface_api::Surface::render) returns.
//!
//! Where the real device differs from the guide it is noted next to the number.

use std::collections::HashMap;

use surface_api::{
    Control, Description, Driver, Effect, Kind, Light, Message, Page, Role, Screen, Source,
    SourceKind, Surface, View, id, page,
};

/// Pads in a row.
const COLUMNS: u8 = 8;
const SCREEN_COLUMNS: usize = 16;

// What the device sends and is lit by (decimal, as in the guide).
const KNOB_FIRST: u8 = 21;
const FADER_FIRST: u8 = 53;
const FADER_BUTTON_FIRST: u8 = 37;
const SESSION_TOP: u8 = 96;
const SESSION_BOTTOM: u8 = 112;
const SELECT_TOP: u8 = 64;
const SELECT_BOTTOM: u8 = 80;
/// The drum layout's pads, rows of eight (the guide's figure): top row, then bottom row.
const DRUM_PADS: [[u8; 8]; 2] = [
    [0x28, 0x29, 0x2A, 0x2B, 0x30, 0x31, 0x32, 0x33],
    [0x24, 0x25, 0x26, 0x27, 0x2C, 0x2D, 0x2E, 0x2F],
];
/// Pad modes are reported (and set) by this control change on channel 16.
const PAD_MODE_CC: u8 = 3;

// Buttons. The two Track buttons report the other way round from the guide's diagram on a
// Launchkey MK3 49 (firmware 0.2.2.8): the "<" button sends 103 and ">" sends 102.
const TRACK_PREV: u8 = 103;
const TRACK_NEXT: u8 = 102;
const SCENE_UP: u8 = 104;
const SCENE_DOWN: u8 = 105;

/// The driver. Create one and hand it to the host.
#[derive(Clone, Copy, Debug, Default)]
pub struct LaunchkeyMk3;

impl Driver for LaunchkeyMk3 {
    fn id(&self) -> &str {
        "novation.launchkey-mk3"
    }

    fn detect(&self, port_name: &str) -> Option<Box<dyn Surface>> {
        Some(Box::new(Mk3Surface::new(detect(port_name)?)))
    }
}

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

/// Is this MIDI port the DAW port of a Launchkey MK3? Takes the readable name
/// (`Launchkey MK3 49: LKMK3 DAW In`).
pub fn detect(port_name: &str) -> Option<Model> {
    let lower = port_name.to_ascii_lowercase();
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

fn cc(number: u8) -> Source {
    Source {
        channel: None,
        kind: SourceKind::Cc(number),
    }
}

fn note(key: u8, channel: Option<u8>) -> Source {
    Source {
        channel,
        kind: SourceKind::Note(key),
    }
}

fn pads(page_id: &str, name: &str, rows: [[u8; 8]; 2], channel: Option<u8>) -> Page {
    let mut controls = Vec::new();
    for (row, notes) in rows.iter().enumerate() {
        for (col, &key) in notes.iter().enumerate() {
            controls.push(Control {
                id: id::pad(page_id, row as u8, col as u8),
                kind: Kind::Pad,
                role: None,
                row: row as u8,
                col: col as u8,
                source: note(key, channel),
                label: format!("Pad {}, row {}", col + 1, row + 1),
                has_light: true,
            });
        }
    }
    Page {
        id: page_id.to_owned(),
        name: name.to_owned(),
        controls,
    }
}

/// What this model has.
pub fn describe(model: Model) -> Description {
    let row = |first: u8| -> [u8; 8] { std::array::from_fn(|i| first + i as u8) };
    let pages = vec![
        pads(
            page::MAIN,
            "Session",
            [row(SESSION_TOP), row(SESSION_BOTTOM)],
            None,
        ),
        pads(
            page::SELECT,
            "Device Select (held)",
            [row(SELECT_TOP), row(SELECT_BOTTOM)],
            None,
        ),
        pads(page::DRUM, "Drum", DRUM_PADS, Some(9)),
    ];
    let mut controls = Vec::new();
    let mut add = |id: String, kind, role, col: u8, source, label: String, has_light| {
        controls.push(Control {
            id,
            kind,
            role,
            row: 0,
            col,
            source,
            label,
            has_light,
        });
    };
    for i in 0..COLUMNS {
        add(
            id::knob(i),
            Kind::Knob,
            None,
            i,
            cc(KNOB_FIRST + i),
            format!("Knob {}", i + 1),
            false,
        );
        add(
            id::fader_button(i),
            Kind::FaderButton,
            None,
            i,
            cc(FADER_BUTTON_FIRST + i),
            format!("Fader button {}", i + 1),
            true,
        );
        if model.has_faders() {
            add(
                id::fader(i),
                Kind::Fader,
                None,
                i,
                cc(FADER_FIRST + i),
                format!("Fader {}", i + 1),
                false,
            );
        }
    }
    if model.has_faders() {
        add(
            id::fader(COLUMNS),
            Kind::Fader,
            None,
            COLUMNS,
            cc(FADER_FIRST + COLUMNS),
            "Fader 9".into(),
            false,
        );
        add(
            id::fader_button(COLUMNS),
            Kind::FaderButton,
            None,
            COLUMNS,
            cc(FADER_BUTTON_FIRST + COLUMNS),
            "Fader button 9".into(),
            true,
        );
    }
    let buttons = [
        (
            "track_prev",
            Some(Role::TrackPrev),
            TRACK_PREV,
            "Track <",
            false,
        ),
        (
            "track_next",
            Some(Role::TrackNext),
            TRACK_NEXT,
            "Track >",
            false,
        ),
        (
            "scene_up",
            Some(Role::ScenePrev),
            SCENE_UP,
            "Scene up",
            true,
        ),
        (
            "scene_down",
            Some(Role::SceneNext),
            SCENE_DOWN,
            "Scene down",
            true,
        ),
        ("capture", None, 74, "Capture MIDI", false),
        ("quantise", Some(Role::Tap), 75, "Quantise", false),
        ("click", Some(Role::Click), 76, "Click", false),
        ("undo", Some(Role::Undo), 77, "Undo", false),
        ("play", Some(Role::Play), 115, "Play", false),
        ("stop", Some(Role::Stop), 116, "Stop", false),
        ("record", Some(Role::Record), 117, "Record", false),
        ("loop", Some(Role::Loop), 118, "Loop", false),
    ];
    for (name, role, number, label, lit) in buttons {
        add(
            id::button(name),
            Kind::Button,
            role,
            0,
            cc(number),
            label.to_owned(),
            lit,
        );
    }
    Description {
        model: format!("Launchkey MK3 {}", model.keys),
        pages,
        controls,
        screen: Some(Screen {
            rows: 2,
            columns: SCREEN_COLUMNS as u8,
        }),
    }
}

// ---- colours ---------------------------------------------------------------------------------

/// The palette entries of the guide's colour table that are used, with roughly what they look
/// like. (The table has 128 entries; a handful of distinct hues is what a host needs.)
const PALETTE: [(u8, [u8; 3]); 15] = [
    (0, [0, 0, 0]),
    (1, [110, 110, 110]),
    (3, [255, 255, 255]),
    (5, [255, 70, 70]),
    (9, [255, 150, 60]),
    (13, [255, 240, 70]),
    (17, [170, 255, 70]),
    (21, [70, 255, 70]),
    (29, [90, 255, 160]),
    (33, [70, 255, 230]),
    (41, [70, 190, 255]),
    (45, [80, 90, 255]),
    (49, [170, 90, 255]),
    (53, [255, 80, 255]),
    (57, [255, 100, 180]),
];

/// The palette entry nearest to a colour (black is off).
pub fn palette_index(rgb: [u8; 3]) -> u8 {
    if rgb == [0, 0, 0] {
        return 0;
    }
    let dist = |c: [u8; 3]| -> i32 {
        (0..3)
            .map(|i| (i32::from(c[i]) - i32::from(rgb[i])).pow(2))
            .sum()
    };
    PALETTE
        .iter()
        .skip(1)
        .min_by_key(|(_, c)| dist(*c))
        .map_or(0, |(i, _)| *i)
}

// ---- the surface -----------------------------------------------------------------------------

/// What the pads are showing, as the device reports it (the guide's list).
fn page_for_mode(v: u8) -> String {
    match v {
        1 => page::DRUM.to_owned(),
        2 => page::MAIN.to_owned(),
        9 => page::SELECT.to_owned(),
        // Scale chords, user chords, custom modes, navigation: the device lights these itself.
        other => format!("other.{other}"),
    }
}

/// One Launchkey MK3.
#[derive(Debug)]
pub struct Mk3Surface {
    model: Model,
    description: Description,
    page: String,
    /// What each LED was last told (status byte of the steady colour, number).
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

impl Mk3Surface {
    pub fn new(model: Model) -> Self {
        Self {
            model,
            description: describe(model),
            page: page::MAIN.to_owned(),
            leds: HashMap::new(),
            sent_text: HashMap::new(),
        }
    }

    fn led(&mut self, out: &mut Vec<Message>, base: u8, number: u8, light: Light) {
        if self.leds.insert((base, number), light) != Some(light) {
            let status = match light.effect {
                Effect::Steady => base,
                // The guide: the same message on the next-but-one channel pulses.
                Effect::Pulse => base | 2,
            };
            out.push(vec![status, number, palette_index(light.rgb)]);
        }
    }

    fn sysex(&mut self, out: &mut Vec<Message>, command: u8, id: &[u8], text: &str) {
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
}

impl Surface for Mk3Surface {
    fn description(&self) -> &Description {
        &self.description
    }

    fn connect(&mut self) -> Vec<Message> {
        self.leds.clear();
        self.sent_text.clear();
        self.page = page::MAIN.to_owned();
        // DAW mode on, then the pads in Session mode, to be sure.
        vec![vec![0x9F, 0x0C, 0x7F], vec![0xBF, PAD_MODE_CC, 2]]
    }

    fn disconnect(&mut self) -> Vec<Message> {
        self.leds.clear();
        self.sent_text.clear();
        // Give the screen back, leave DAW mode (a Note On of velocity 0 is a Note Off).
        let mut clear = self.model.header().to_vec();
        clear.extend([0x06, 0xF7]);
        vec![clear, vec![0x9F, 0x0C, 0x00]]
    }

    fn input(&mut self, bytes: &[u8]) -> bool {
        let &[0xBF, PAD_MODE_CC, value] = bytes else {
            return false;
        };
        let page = page_for_mode(value);
        if page == self.page {
            return false;
        }
        self.page = page;
        // Each layout's pads start dark: light them again from scratch.
        self.leds.retain(|(base, _), _| *base == 0xB0);
        true
    }

    fn page(&self) -> &str {
        &self.page
    }

    fn render(&mut self, view: &View) -> Vec<Message> {
        let mut out = Vec::new();
        // The pads of the page that is showing, every one (dark if the host says nothing).
        let base = if self.page == page::DRUM { 0x99 } else { 0x90 };
        let pad_lights: Vec<(u8, Light)> = self
            .description
            .page(&self.page)
            .map(|p| {
                p.controls
                    .iter()
                    .filter_map(|c| match c.source.kind {
                        SourceKind::Note(key) => {
                            Some((key, view.lights.get(&c.id).copied().unwrap_or(Light::OFF)))
                        }
                        SourceKind::Cc(_) => None,
                    })
                    .collect()
            })
            .unwrap_or_default();
        for (key, light) in pad_lights {
            self.led(&mut out, base, key, light);
        }
        // Buttons with an LED that the host mentions (fader buttons report on channel 16 but
        // are lit on channel 1).
        let lit: Vec<(u8, Light)> = self
            .description
            .controls
            .iter()
            .filter(|c| c.has_light)
            .filter_map(|c| match c.source.kind {
                SourceKind::Cc(n) => view.lights.get(&c.id).map(|l| (n, *l)),
                SourceKind::Note(_) => None,
            })
            .collect();
        for (number, light) in lit {
            self.led(&mut out, 0xB0, number, light);
        }
        for (row, line) in view.text.iter().take(2).enumerate() {
            self.sysex(&mut out, 0x04, &[row as u8], line);
        }
        // Names and values of the knobs and faders, shown while they are touched.
        let labelled: Vec<(u8, String, String)> = self
            .description
            .controls
            .iter()
            .filter_map(|c| {
                let (name, value) = view.labels.get(&c.id)?;
                let base = match c.kind {
                    Kind::Knob => 0x38,
                    Kind::Fader => 0x50,
                    _ => return None,
                };
                Some((base + c.col, name.clone(), value.clone()))
            })
            .collect();
        for (index, name, value) in labelled {
            self.sysex(&mut out, 0x07, &[index], &name);
            self.sysex(&mut out, 0x08, &[index], &value);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert!(
            LaunchkeyMk3
                .detect("Launchkey MK3 61 LKMK3 DAW In")
                .is_some()
        );
        assert!(LaunchkeyMk3.detect("Something else").is_none());
        assert!(!Model { keys: 37 }.has_faders());
        assert!(Model { keys: 61 }.has_faders());
    }

    #[test]
    fn the_description_has_what_the_device_has() {
        let d = describe(Model { keys: 49 });
        assert_eq!(d.model, "Launchkey MK3 49");
        let main = d.page(page::MAIN).unwrap();
        assert_eq!(main.controls.len(), 16);
        let pad = d.control("main.pad.1.2").unwrap();
        assert_eq!(pad.source.kind, SourceKind::Note(114));
        assert_eq!(d.page_of("select.pad.0.0"), Some(page::SELECT));
        assert_eq!(d.page_of("knob.0"), None);
        assert_eq!(d.control("knob.7").unwrap().source.kind, SourceKind::Cc(28));
        assert_eq!(
            d.control("fader.8").unwrap().source.kind,
            SourceKind::Cc(61)
        );
        // The Track buttons as the real device sends them.
        let prev = d.control("button.track_prev").unwrap();
        assert_eq!(
            (prev.role, prev.source.kind),
            (Some(Role::TrackPrev), SourceKind::Cc(103))
        );
        // The same ids are unique.
        let mut ids: Vec<_> = d.all_controls().map(|c| c.id.clone()).collect();
        let n = ids.len();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), n);
        // A model without faders has none.
        let small = describe(Model { keys: 25 });
        assert!(small.all_controls().all(|c| c.kind != Kind::Fader));
        assert_eq!(
            small.screen,
            Some(Screen {
                rows: 2,
                columns: 16
            })
        );
    }

    #[test]
    fn colours_go_to_the_nearest_palette_entry() {
        assert_eq!(palette_index([0, 0, 0]), 0);
        assert_eq!(palette_index([255, 255, 255]), 3);
        assert_eq!(palette_index([240, 60, 60]), 5);
        assert_eq!(palette_index([70, 255, 70]), 21);
        assert_eq!(palette_index([90, 170, 225]), 41);
        assert_eq!(palette_index([110, 110, 110]), 1);
    }

    fn view(lights: &[(&str, Light)]) -> View {
        View {
            lights: lights.iter().map(|(k, l)| ((*k).to_owned(), *l)).collect(),
            ..View::default()
        }
    }

    #[test]
    fn lights_and_text_go_out_once_and_again_only_when_they_change() {
        let mut s = Mk3Surface::new(Model { keys: 49 });
        assert_eq!(
            s.connect(),
            vec![vec![0x9F, 0x0C, 0x7F], vec![0xBF, 3, 2]],
            "DAW mode on, pads in Session mode"
        );
        let red = [255, 70, 70];
        let mut v = view(&[
            ("main.pad.0.0", Light::pulse(red)),
            ("main.pad.0.1", Light::steady([70, 80, 255])),
            ("fader_button.0", Light::steady([255, 255, 255])),
        ]);
        v.text = vec!["1 Piano".into(), "Snap 2".into()];
        v.labels
            .insert("knob.0".into(), ("Reverb".into(), "35%".into()));
        let first = s.render(&v);
        // A pulsing pad: the colour on the channel two above the steady one.
        assert!(first.contains(&vec![0x92, 96, 5]));
        assert!(first.contains(&vec![0x90, 97, 45]));
        // Pads the host did not mention are told to be dark.
        assert!(first.contains(&vec![0x90, 119, 0]));
        // A fader button's LED is lit on channel 1 though it reports on 16.
        assert!(first.contains(&vec![0xB0, 37, 3]));
        let mut line = vec![0xF0, 0, 0x20, 0x29, 2, 0x0F, 4, 0];
        line.extend(b"1 Piano");
        line.push(0xF7);
        assert!(first.contains(&line));
        let mut name = vec![0xF0, 0, 0x20, 0x29, 2, 0x0F, 7, 0x38];
        name.extend(b"Reverb");
        name.push(0xF7);
        assert!(first.contains(&name));
        // Nothing changed: nothing is sent.
        assert!(s.render(&v).is_empty());
        // The pad goes steady, and only that is sent.
        v.lights.insert("main.pad.0.0".into(), Light::steady(red));
        assert_eq!(s.render(&v), vec![vec![0x90, 96, 5]]);
        // Leaving gives the screen back and leaves DAW mode.
        let bye = s.disconnect();
        assert_eq!(bye.last(), Some(&vec![0x9F, 0x0C, 0x00]));
        assert_eq!(bye[0][6..], [0x06, 0xF7]);
    }

    #[test]
    fn the_pads_follow_the_page_the_device_reports() {
        let mut s = Mk3Surface::new(Model { keys: 49 });
        s.connect();
        let v = view(&[
            ("main.pad.0.0", Light::pulse([255, 70, 70])),
            ("select.pad.1.1", Light::steady([70, 255, 70])),
            ("drum.pad.0.0", Light::steady([70, 255, 70])),
            ("drum.pad.1.7", Light::steady([70, 255, 70])),
        ]);
        assert!(s.render(&v).contains(&vec![0x92, 96, 5]));
        assert!(s.render(&v).is_empty());
        // Other channels and controls change nothing.
        assert!(!s.input(&[0xB0, 3, 1]));
        assert!(!s.input(&[0xBF, 9, 1]));
        assert!(!s.input(&[0x90, 3, 1]));
        // Device Select held: the select notes, no Session pads.
        assert!(s.input(&[0xBF, 3, 9]));
        assert_eq!(s.page(), page::SELECT);
        let select = s.render(&v);
        assert!(select.contains(&vec![0x90, 81, 21]));
        assert!(!select.iter().any(|m| m[1] == 96));
        // Released: Session again, lit again from scratch.
        assert!(s.input(&[0xBF, 3, 2]));
        assert!(s.render(&v).contains(&vec![0x92, 96, 5]));
        // Drum: the sixteen drum pads on channel 10.
        assert!(s.input(&[0xBF, 3, 1]));
        let drum = s.render(&v);
        let pads: Vec<_> = drum.iter().filter(|m| m[0] == 0x99).collect();
        assert_eq!(pads.len(), 16);
        assert!(drum.contains(&vec![0x99, 0x28, 21]));
        assert!(drum.contains(&vec![0x99, 0x2F, 21]));
        // Chords and custom modes are the device's own: no pad lights from us.
        assert!(s.input(&[0xBF, 3, 4]));
        assert!(
            s.render(&v)
                .iter()
                .all(|m| m[0] != 0x99 && m[0] != 0x90 && m[0] != 0x92)
        );
    }

    #[test]
    fn text_is_ascii_sixteen_wide_and_the_88_has_its_own_header() {
        assert_eq!(
            text_bytes("Grand Piano ♪ and much more text"),
            b"Grand Piano ? an"
        );
        let mut s = Mk3Surface::new(Model { keys: 88 });
        let v = View {
            text: vec!["x".into()],
            ..View::default()
        };
        let out = s.render(&v);
        assert!(
            out.iter()
                .any(|m| m.starts_with(&[0xF0, 0, 0x20, 0x29, 2, 0x12]))
        );
        // A model without faders shows no fader names.
        let mut small = Mk3Surface::new(Model { keys: 25 });
        let mut v = View::default();
        v.labels.insert("fader.0".into(), ("a".into(), "b".into()));
        assert!(small.render(&v).iter().all(|m| m.len() < 8 || m[6] != 0x07));
    }
}
