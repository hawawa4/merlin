//! The Standard MIDI File writer, and the tidying a window cut out of a performance needs.

use crate::{Capture, Event};

/// Ticks per quarter note in the files we write: fine enough that a tick is about half a
/// millisecond at 120 bpm, so what was played keeps its feel.
pub const TICKS_PER_BEAT: u16 = 960;
/// The biggest value a variable-length quantity holds.
const VLQ_MAX: u64 = 0x0FFF_FFFF;

/// What the file says about the music: it does not change when the notes sound, only where a
/// sequencer draws its bars and what the tracks are called.
#[derive(Clone, Debug)]
pub struct SmfOptions {
    /// The tempo the file declares (what the transport ran at); the notes keep their real times.
    pub tempo: f32,
    pub beats_per_bar: u8,
    /// The name of the first track.
    pub title: String,
    /// Names for the tracks, by engine input; an input without one is called "MIDI input N".
    pub input_names: Vec<String>,
}

impl Default for SmfOptions {
    fn default() -> Self {
        Self {
            tempo: 120.0,
            beats_per_bar: 4,
            title: "Merlin capture".into(),
            input_names: Vec::new(),
        }
    }
}

/// Notes held and pedals down on one MIDI input, per channel.
#[derive(Default)]
struct Held {
    notes: [u128; 16],
    pedal: u16,
}

impl Held {
    fn any(&self) -> bool {
        self.pedal != 0 || self.notes.iter().any(|&n| n != 0)
    }
}

/// What is left of a window of playing once its edges are tidied.
pub(crate) struct Tidied {
    pub events: Vec<Event>,
    /// Where it ends: the last message, or now when a note or the pedal is still down.
    pub end: u64,
}

/// A note-off whose note-on was cut off by the start of the window is dropped, and what is still
/// down at the end is let go there, so no file has a note that never ends or a stray release.
pub(crate) fn tidy(events: &[Event], now: u64) -> Tidied {
    let mut held: Vec<(u8, Held)> = Vec::new();
    let mut out = Vec::with_capacity(events.len());
    for &ev in events {
        let i = match held.iter().position(|(input, _)| *input == ev.input) {
            Some(i) => i,
            None => {
                held.push((ev.input, Held::default()));
                held.len() - 1
            }
        };
        let h = &mut held[i].1;
        let ch = (ev.data[0] & 15) as usize;
        let key = ev.data[1] & 127;
        match ev.data[0] & 0xF0 {
            0x90 if ev.data[2] != 0 => h.notes[ch] |= 1 << key,
            0x80 | 0x90 => {
                if h.notes[ch] & (1 << key) == 0 {
                    continue;
                }
                h.notes[ch] &= !(1 << key);
            }
            0xB0 => match key {
                64 if ev.data[2] >= 64 => h.pedal |= 1 << ch,
                64 => h.pedal &= !(1 << ch),
                // All sound off, all notes off.
                120 | 123 => h.notes[ch] = 0,
                _ => {}
            },
            _ => {}
        }
        out.push(ev);
    }
    let mut end = out.last().map_or(0, |e| e.frame);
    if held.iter().any(|(_, h)| h.any()) {
        end = end.max(now);
        for (input, h) in &held {
            for ch in 0..16u8 {
                for key in 0..128u8 {
                    if h.notes[ch as usize] & (1 << key) != 0 {
                        out.push(Event {
                            frame: end,
                            input: *input,
                            data: [0x80 | ch, key, 0],
                        });
                    }
                }
                if h.pedal & (1 << ch) != 0 {
                    out.push(Event {
                        frame: end,
                        input: *input,
                        data: [0xB0 | ch, 64, 0],
                    });
                }
            }
        }
    }
    Tidied { events: out, end }
}

fn vlq(out: &mut Vec<u8>, value: u64) {
    let v = value.min(VLQ_MAX) as u32;
    let mut groups = [0u8; 4];
    let mut n = 0;
    let mut rest = v;
    loop {
        groups[n] = (rest & 0x7F) as u8;
        rest >>= 7;
        n += 1;
        if rest == 0 {
            break;
        }
    }
    for i in (0..n).rev() {
        out.push(groups[i] | if i > 0 { 0x80 } else { 0 });
    }
}

fn meta(track: &mut Vec<u8>, delta: u64, kind: u8, data: &[u8]) {
    vlq(track, delta);
    track.extend([0xFF, kind]);
    vlq(track, data.len() as u64);
    track.extend(data);
}

fn chunk(file: &mut Vec<u8>, id: &[u8; 4], body: &[u8]) {
    file.extend(id);
    file.extend((body.len() as u32).to_be_bytes());
    file.extend(body);
}

pub(crate) fn write(capture: &Capture, options: &SmfOptions) -> Vec<u8> {
    let tidied = tidy(&capture.events, capture.now);
    let start = tidied.events.first().map_or(0, |e| e.frame);
    // The tempo as the file stores it (microseconds per beat), so ticks and tempo agree.
    let tempo = options.tempo.clamp(10.0, 1000.0);
    let micros = (60_000_000.0 / f64::from(tempo)).round().max(1.0);
    let ticks_per_frame = f64::from(TICKS_PER_BEAT) * (60_000_000.0 / micros)
        / (60.0 * f64::from(capture.sample_rate));
    let tick = |frame: u64| ((frame - start) as f64 * ticks_per_frame).round() as u64;

    let mut inputs: Vec<u8> = tidied.events.iter().map(|e| e.input).collect();
    inputs.sort_unstable();
    inputs.dedup();

    let mut file = Vec::new();
    let mut header = Vec::new();
    header.extend(1u16.to_be_bytes());
    header.extend((inputs.len() as u16 + 1).to_be_bytes());
    header.extend(TICKS_PER_BEAT.to_be_bytes());
    chunk(&mut file, b"MThd", &header);

    // The conductor track: name, tempo, time signature.
    let mut conductor = Vec::new();
    meta(&mut conductor, 0, 0x03, options.title.as_bytes());
    meta(&mut conductor, 0, 0x51, &(micros as u32).to_be_bytes()[1..]);
    meta(
        &mut conductor,
        0,
        0x58,
        &[options.beats_per_bar.clamp(1, 32), 2, 24, 8],
    );
    meta(&mut conductor, tick(tidied.end), 0x2F, &[]);
    chunk(&mut file, b"MTrk", &conductor);

    for &input in &inputs {
        let name = options
            .input_names
            .get(input as usize)
            .filter(|n| !n.is_empty())
            .cloned()
            .unwrap_or_else(|| format!("MIDI input {input}"));
        let mut track = Vec::new();
        meta(&mut track, 0, 0x03, name.as_bytes());
        let mut at = 0;
        for ev in tidied.events.iter().filter(|e| e.input == input) {
            let t = tick(ev.frame).max(at);
            vlq(&mut track, t - at);
            at = t;
            // The status byte, then the data bytes that kind of message has (7 bits each).
            track.push(ev.data[0]);
            track.push(ev.data[1] & 127);
            if !matches!(ev.data[0] & 0xF0, 0xC0 | 0xD0) {
                track.push(ev.data[2] & 127);
            }
        }
        meta(&mut track, tick(tidied.end).max(at) - at, 0x2F, &[]);
        chunk(&mut file, b"MTrk", &track);
    }
    file
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: u32 = 48_000;

    fn ev(frame: u64, input: u8, data: [u8; 3]) -> Event {
        Event { frame, input, data }
    }

    fn cap(events: Vec<Event>, now: u64) -> Capture {
        Capture {
            events,
            now,
            sample_rate: SR,
        }
    }

    /// What a track holds, read back: (absolute tick, bytes), and its name.
    #[derive(Debug, Default)]
    struct Track {
        name: String,
        events: Vec<(u64, Vec<u8>)>,
        end: u64,
        tempo: Option<u32>,
        signature: Option<[u8; 4]>,
    }

    fn read_vlq(b: &[u8], i: &mut usize) -> u64 {
        let mut v = 0u64;
        loop {
            let byte = b[*i];
            *i += 1;
            v = v << 7 | u64::from(byte & 0x7F);
            if byte & 0x80 == 0 {
                return v;
            }
        }
    }

    /// A reader for exactly the files we write: no running status, channel messages and meta.
    fn parse(file: &[u8]) -> (u16, u16, Vec<Track>) {
        assert_eq!(&file[..4], b"MThd");
        assert_eq!(u32::from_be_bytes(file[4..8].try_into().unwrap()), 6);
        let format = u16::from_be_bytes([file[8], file[9]]);
        let count = u16::from_be_bytes([file[10], file[11]]);
        let division = u16::from_be_bytes([file[12], file[13]]);
        let mut i = 14;
        let mut tracks = Vec::new();
        while i < file.len() {
            assert_eq!(&file[i..i + 4], b"MTrk");
            let len = u32::from_be_bytes(file[i + 4..i + 8].try_into().unwrap()) as usize;
            let body = &file[i + 8..i + 8 + len];
            i += 8 + len;
            let (mut j, mut at) = (0, 0u64);
            let mut t = Track::default();
            while j < body.len() {
                at += read_vlq(body, &mut j);
                let status = body[j];
                j += 1;
                if status == 0xFF {
                    let kind = body[j];
                    j += 1;
                    let n = read_vlq(body, &mut j) as usize;
                    let data = &body[j..j + n];
                    j += n;
                    match kind {
                        0x03 => t.name = String::from_utf8(data.to_vec()).unwrap(),
                        0x51 => t.tempo = Some(u32::from_be_bytes([0, data[0], data[1], data[2]])),
                        0x58 => t.signature = Some(data.try_into().unwrap()),
                        0x2F => t.end = at,
                        _ => panic!("unexpected meta {kind:#x}"),
                    }
                } else {
                    let n = if matches!(status & 0xF0, 0xC0 | 0xD0) {
                        1
                    } else {
                        2
                    };
                    let mut bytes = vec![status];
                    bytes.extend(&body[j..j + n]);
                    assert!(bytes[1..].iter().all(|b| *b < 128), "data bytes are 7 bit");
                    j += n;
                    t.events.push((at, bytes));
                }
            }
            tracks.push(t);
        }
        assert_eq!(tracks.len(), count as usize);
        (format, division, tracks)
    }

    #[test]
    fn variable_length_numbers_match_the_spec_examples() {
        for (value, bytes) in [
            (0u64, vec![0x00]),
            (0x40, vec![0x40]),
            (0x7F, vec![0x7F]),
            (0x80, vec![0x81, 0x00]),
            (0x2000, vec![0xC0, 0x00]),
            (0x3FFF, vec![0xFF, 0x7F]),
            (0x4000, vec![0x81, 0x80, 0x00]),
            (0x1F_FFFF, vec![0xFF, 0xFF, 0x7F]),
            (0x20_0000, vec![0x81, 0x80, 0x80, 0x00]),
            (0x0FFF_FFFF, vec![0xFF, 0xFF, 0xFF, 0x7F]),
        ] {
            let mut out = Vec::new();
            vlq(&mut out, value);
            assert_eq!(out, bytes, "{value:#x}");
        }
        // Past the largest one it saturates instead of wrapping.
        let mut out = Vec::new();
        vlq(&mut out, u64::MAX);
        assert_eq!(out, vec![0xFF, 0xFF, 0xFF, 0x7F]);
    }

    #[test]
    fn a_note_lands_where_it_was_played() {
        // 120 bpm: a second is two beats, so 1920 ticks.
        let c = cap(
            vec![
                ev(10_000, 1, [0x90, 60, 100]),
                ev(10_000 + SR as u64, 1, [0x80, 60, 0]),
            ],
            200_000,
        );
        let (format, division, tracks) = parse(&c.to_smf(&SmfOptions::default()));
        assert_eq!((format, division), (1, TICKS_PER_BEAT));
        assert_eq!(tracks.len(), 2);
        assert_eq!(tracks[0].name, "Merlin capture");
        assert_eq!(tracks[0].tempo, Some(500_000));
        assert_eq!(tracks[0].signature, Some([4, 2, 24, 8]));
        assert_eq!(tracks[1].name, "MIDI input 1");
        // The file starts at the first note: no silence before it.
        assert_eq!(
            tracks[1].events,
            vec![(0, vec![0x90, 60, 100]), (1920, vec![0x80, 60, 0])]
        );
        // And ends with the last message, not at "now".
        assert_eq!(tracks[0].end, 1920);
        assert_eq!(tracks[1].end, 1920);
        assert_eq!(c.duration().as_secs(), 1);
    }

    #[test]
    fn the_declared_tempo_scales_the_ticks_and_not_the_time() {
        let c = cap(
            vec![ev(0, 1, [0x90, 60, 100]), ev(SR as u64, 1, [0x80, 60, 0])],
            SR as u64,
        );
        let opt = SmfOptions {
            tempo: 60.0,
            beats_per_bar: 3,
            ..SmfOptions::default()
        };
        let (_, _, tracks) = parse(&c.to_smf(&opt));
        // At 60 bpm a second is one beat.
        assert_eq!(tracks[1].events[1].0, 960);
        assert_eq!(tracks[0].tempo, Some(1_000_000));
        assert_eq!(tracks[0].signature.unwrap()[0], 3);
    }

    #[test]
    fn a_track_for_each_input_with_its_name() {
        let c = cap(
            vec![
                ev(0, 2, [0x90, 40, 90]),
                ev(100, 0, [0x90, 60, 90]),
                ev(200, 2, [0x80, 40, 0]),
                ev(300, 0, [0x80, 60, 0]),
            ],
            1000,
        );
        let opt = SmfOptions {
            input_names: vec!["On-screen keyboard".into(), "".into(), "Bass keys".into()],
            ..SmfOptions::default()
        };
        let (_, _, tracks) = parse(&c.to_smf(&opt));
        let names: Vec<&str> = tracks.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["Merlin capture", "On-screen keyboard", "Bass keys"]);
        assert_eq!(tracks[1].events.len(), 2);
        assert_eq!(tracks[2].events.len(), 2);
    }

    #[test]
    fn a_note_off_cut_off_from_its_note_is_left_out() {
        let c = cap(
            vec![
                // The window began while this note sounded.
                ev(100, 1, [0x80, 60, 0]),
                ev(200, 1, [0x90, 62, 100]),
                // A note-on with velocity 0 is a note-off too.
                ev(300, 1, [0x90, 62, 0]),
                ev(400, 1, [0x90, 62, 0]),
            ],
            1000,
        );
        let t = tidy(&c.events, c.now);
        assert_eq!(t.events.len(), 2);
        assert_eq!(t.events[0].frame, 200);
        // The file starts at the first message that is kept.
        let (_, _, tracks) = parse(&c.to_smf(&SmfOptions::default()));
        assert_eq!(tracks[1].events[0].0, 0);
    }

    #[test]
    fn what_is_held_at_the_end_is_let_go_at_now() {
        let c = cap(
            vec![
                ev(0, 1, [0x90, 60, 100]),
                ev(0, 1, [0x91, 64, 100]),
                ev(10, 1, [0xB0, 64, 127]),
            ],
            SR as u64,
        );
        let (_, _, tracks) = parse(&c.to_smf(&SmfOptions::default()));
        let t = &tracks[1].events;
        assert_eq!(
            t[3..],
            [
                // Channel by channel: the note and the pedal of channel 1, then channel 2's note.
                (1920, vec![0x80, 60, 0]),
                (1920, vec![0xB0, 64, 0]),
                (1920, vec![0x81, 64, 0]),
            ]
        );
        assert_eq!(tracks[0].end, 1920);
        assert_eq!(c.duration().as_secs(), 1);
    }

    #[test]
    fn all_notes_off_lets_notes_go() {
        let c = cap(
            vec![ev(0, 1, [0x90, 60, 100]), ev(100, 1, [0xB0, 123, 0])],
            SR as u64,
        );
        // Nothing is held after it, so the file ends at the controller and has no extra note-off.
        let t = tidy(&c.events, c.now);
        assert_eq!(t.events.len(), 2);
        assert_eq!(t.end, 100);
    }

    #[test]
    fn every_kind_of_message_is_written_with_its_own_length() {
        let c = cap(
            vec![
                ev(0, 1, [0xC3, 5, 0]),
                ev(1, 1, [0xD3, 77, 0]),
                ev(2, 1, [0xE3, 0, 64]),
                ev(3, 1, [0xA3, 60, 20]),
                ev(4, 1, [0xB3, 1, 99]),
            ],
            100,
        );
        let (_, _, tracks) = parse(&c.to_smf(&SmfOptions::default()));
        let sizes: Vec<usize> = tracks[1].events.iter().map(|e| e.1.len()).collect();
        assert_eq!(sizes, [2, 2, 3, 3, 3]);
    }

    #[test]
    fn data_bytes_stay_below_128_whatever_came_in() {
        let c = cap(vec![ev(0, 1, [0xB0, 0xFF, 0x80])], 10);
        let (_, _, tracks) = parse(&c.to_smf(&SmfOptions::default()));
        assert_eq!(tracks[1].events[0].1, vec![0xB0, 0x7F, 0x00]);
    }

    #[test]
    fn an_empty_capture_is_a_valid_file_with_only_the_tempo_track() {
        let c = cap(Vec::new(), 100);
        assert!(c.is_empty());
        let (_, _, tracks) = parse(&c.to_smf(&SmfOptions::default()));
        assert_eq!(tracks.len(), 1);
        assert_eq!(c.duration().as_secs(), 0);
    }

    #[test]
    fn half_an_hour_of_busy_playing_is_a_small_file() {
        // 30 minutes of a wheel moving 200 times a second, with a note on top of it.
        let n = 30 * 60 * 200;
        let events: Vec<Event> = (0..n)
            .map(|i| ev(i as u64 * 240, 1, [0xE0, (i % 128) as u8, 64]))
            .collect();
        let now = n as u64 * 240;
        let file = cap(events, now).to_smf(&SmfOptions::default());
        let (_, _, tracks) = parse(&file);
        assert_eq!(tracks[1].events.len(), n);
        assert!(file.len() < 3_000_000, "{}", file.len());
        // The last tick agrees with the time of the last frame at 120 bpm (1920 ticks a second).
        assert_eq!(
            tracks[1].events.last().unwrap().0,
            (n as u64 - 1) * 240 * 1920 / u64::from(SR)
        );
    }
}
