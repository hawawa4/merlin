//! Reading a Standard MIDI File into timed channel messages, for playing it through the rig.
//!
//! All tracks are merged and the tempo map applied, so what comes out is a list of messages in
//! seconds. System exclusive and meta events other than tempo are left out: the rig is played
//! like a keyboard plays it.

use std::fmt;

/// One channel message of the file and when it sounds.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Timed {
    pub seconds: f64,
    /// Padded with zeros for a two-byte message (program change, channel pressure).
    pub data: [u8; 3],
}

/// A file, read.
#[derive(Clone, Debug, Default)]
pub struct MidiFile {
    /// In time order (messages at the same time keep the order of the file).
    pub events: Vec<Timed>,
    /// When the file ends: its last event, or its end-of-track marker if that is later.
    pub seconds: f64,
    /// The first track's name, when it has one.
    pub title: Option<String>,
}

impl MidiFile {
    /// How many notes it plays.
    pub fn notes(&self) -> usize {
        self.events
            .iter()
            .filter(|e| e.data[0] & 0xF0 == 0x90 && e.data[2] > 0)
            .count()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadError(String);

impl fmt::Display for ReadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ReadError {}

fn bad(text: &str) -> ReadError {
    ReadError(text.to_owned())
}

enum Kind {
    Channel([u8; 3]),
    /// Microseconds per quarter note.
    Tempo(u32),
    End,
}

struct Raw {
    tick: u64,
    /// Track, then position in the track: the order of events at the same tick.
    order: (u16, u32),
    kind: Kind,
}

/// Reads a Standard MIDI File (format 0, 1 or 2; a RIFF `.rmi` wrapper is fine too).
pub fn read_smf(bytes: &[u8]) -> Result<MidiFile, ReadError> {
    let bytes = unwrap_riff(bytes);
    let start = find(bytes, b"MThd").ok_or_else(|| bad("not a MIDI file"))?;
    let mut at = start + 4;
    let header_len = be32(bytes, at).ok_or_else(|| bad("the file is cut short"))? as usize;
    at += 4;
    if header_len < 6 || at + header_len > bytes.len() {
        return Err(bad("the file is cut short"));
    }
    let division = u16::from_be_bytes([bytes[at + 4], bytes[at + 5]]);
    if division == 0 {
        return Err(bad("the file has no time division"));
    }
    at += header_len;

    let mut raw: Vec<Raw> = Vec::new();
    let mut title = None;
    let mut track = 0u16;
    while at + 8 <= bytes.len() {
        let id = &bytes[at..at + 4];
        let len = be32(bytes, at + 4).unwrap_or(0) as usize;
        let body_start = at + 8;
        // A length past the end is a damaged file: read what is there.
        let body_end = body_start.saturating_add(len).min(bytes.len());
        if id == b"MTrk" {
            let name = read_track(&bytes[body_start..body_end], track, &mut raw);
            if track == 0 {
                title = name;
            }
            track = track.saturating_add(1);
        }
        at = body_end;
    }
    if track == 0 {
        return Err(bad("the MIDI file has no tracks"));
    }
    raw.sort_by_key(|r| (r.tick, r.order));

    // Ticks to seconds, through the tempo map.
    let smpte = division & 0x8000 != 0;
    let ticks_per_second = if smpte {
        let fps = match (division >> 8) as u8 as i8 {
            -29 => 29.97,
            f => f64::from(-f),
        };
        fps * f64::from(division & 0xFF).max(1.0)
    } else {
        0.0
    };
    let per_beat = f64::from(division);
    let (mut tempo, mut base_tick, mut base_seconds) = (500_000.0f64, 0u64, 0.0f64);
    let seconds_at = |tick: u64, tempo: f64, base_tick: u64, base_seconds: f64| {
        if smpte {
            tick as f64 / ticks_per_second
        } else {
            base_seconds + (tick - base_tick) as f64 * tempo / 1e6 / per_beat
        }
    };
    let mut events = Vec::new();
    let mut end = 0.0f64;
    for r in &raw {
        let seconds = seconds_at(r.tick, tempo, base_tick, base_seconds);
        end = end.max(seconds);
        match r.kind {
            Kind::Channel(data) => events.push(Timed { seconds, data }),
            Kind::Tempo(micros) => {
                base_seconds = seconds;
                base_tick = r.tick;
                tempo = f64::from(micros.max(1));
            }
            Kind::End => {}
        }
    }
    Ok(MidiFile {
        events,
        seconds: end,
        title,
    })
}

/// The events of one track; returns its name.
fn read_track(b: &[u8], track: u16, out: &mut Vec<Raw>) -> Option<String> {
    let mut i = 0;
    let mut tick = 0u64;
    let mut running: Option<u8> = None;
    let mut n = 0u32;
    let mut name = None;
    let mut push = |tick: u64, kind: Kind, n: &mut u32| {
        out.push(Raw {
            tick,
            order: (track, *n),
            kind,
        });
        *n += 1;
    };
    while i < b.len() {
        let Some(delta) = vlq(b, &mut i) else { break };
        tick += delta;
        let Some(&first) = b.get(i) else { break };
        let status = if first & 0x80 != 0 {
            i += 1;
            first
        } else if let Some(s) = running {
            s
        } else {
            // Data with no status before it: the track is damaged from here.
            break;
        };
        match status {
            0x80..=0xEF => {
                running = Some(status);
                let len = if matches!(status & 0xF0, 0xC0 | 0xD0) {
                    1
                } else {
                    2
                };
                if i + len > b.len() {
                    break;
                }
                let mut data = [status, 0, 0];
                data[1..=len].copy_from_slice(&b[i..i + len]);
                data[1] &= 0x7F;
                data[2] &= 0x7F;
                i += len;
                push(tick, Kind::Channel(data), &mut n);
            }
            0xF0 | 0xF7 => {
                running = None;
                let Some(len) = vlq(b, &mut i) else { break };
                i = i.saturating_add(len as usize);
            }
            0xFF => {
                running = None;
                let Some(&kind) = b.get(i) else { break };
                i += 1;
                let Some(len) = vlq(b, &mut i) else { break };
                let len = len as usize;
                let Some(body) = b.get(i..i.saturating_add(len)) else {
                    break;
                };
                i += len;
                match kind {
                    0x51 if len == 3 => {
                        let micros = u32::from_be_bytes([0, body[0], body[1], body[2]]);
                        push(tick, Kind::Tempo(micros), &mut n);
                    }
                    0x03 if name.is_none() => {
                        let text = String::from_utf8_lossy(body).trim().to_owned();
                        if !text.is_empty() {
                            name = Some(text);
                        }
                    }
                    0x2F => {
                        push(tick, Kind::End, &mut n);
                        break;
                    }
                    _ => {}
                }
            }
            // System common / real-time bytes have no place in a file: stop reading the track.
            _ => break,
        }
    }
    name
}

fn vlq(b: &[u8], i: &mut usize) -> Option<u64> {
    let mut v = 0u64;
    for _ in 0..4 {
        let byte = *b.get(*i)?;
        *i += 1;
        v = v << 7 | u64::from(byte & 0x7F);
        if byte & 0x80 == 0 {
            return Some(v);
        }
    }
    None
}

fn be32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

fn find(b: &[u8], what: &[u8; 4]) -> Option<usize> {
    // The header is at the start; a few files have junk before it.
    b.windows(4).take(4096).position(|w| w == what)
}

/// The MIDI data of an `.rmi` (RIFF `RMID`) file; anything else as it is.
fn unwrap_riff(b: &[u8]) -> &[u8] {
    if b.len() < 12 || &b[0..4] != b"RIFF" || &b[8..12] != b"RMID" {
        return b;
    }
    let mut at = 12;
    while at + 8 <= b.len() {
        let len = u32::from_le_bytes(b[at + 4..at + 8].try_into().unwrap()) as usize;
        let body = at + 8;
        if &b[at..at + 4] == b"data" {
            return &b[body..body.saturating_add(len).min(b.len())];
        }
        at = body.saturating_add(len + (len & 1));
    }
    b
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Capture, Event, SmfOptions};

    fn chunk(id: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut v = id.to_vec();
        v.extend((body.len() as u32).to_be_bytes());
        v.extend(body);
        v
    }

    fn file(format: u16, division: u16, tracks: &[Vec<u8>]) -> Vec<u8> {
        let mut header = Vec::new();
        header.extend(format.to_be_bytes());
        header.extend((tracks.len() as u16).to_be_bytes());
        header.extend(division.to_be_bytes());
        let mut f = chunk(b"MThd", &header);
        for t in tracks {
            f.extend(chunk(b"MTrk", t));
        }
        f
    }

    #[test]
    fn running_status_and_tempo_changes_are_followed() {
        // 480 ticks per beat. Track 0: tempo 120 bpm, then 60 bpm at beat 2.
        let conductor = vec![
            0x00, 0xFF, 0x03, 0x04, b'S', b'o', b'n', b'g', //
            0x00, 0xFF, 0x51, 0x03, 0x07, 0xA1, 0x20, // 500000
            0x87, 0x40, 0xFF, 0x51, 0x03, 0x0F, 0x42, 0x40, // +960 ticks: 1000000
            0x00, 0xFF, 0x2F, 0x00,
        ];
        // Track 1: a note at beat 0, its release (running status, velocity 0) at beat 1, a
        // program change at beat 2, a note at beat 3.
        let notes = vec![
            0x00, 0x90, 60, 100, //
            0x83, 0x60, 60, 0, // +480, running status
            0x83, 0x60, 0xC1, 5, // +480
            0x83, 0x60, 0x91, 64, 90, // +480
            0x00, 0xF0, 0x02, 0x7E, 0xF7, // a sysex, skipped
            0x83, 0x60, 0x81, 64, 0, //
            0x00, 0xFF, 0x2F, 0x00,
        ];
        let f = read_smf(&file(1, 480, &[conductor, notes])).unwrap();
        assert_eq!(f.title.as_deref(), Some("Song"));
        let got: Vec<(f64, [u8; 3])> = f.events.iter().map(|e| (e.seconds, e.data)).collect();
        assert_eq!(
            got,
            vec![
                (0.0, [0x90, 60, 100]),
                (0.5, [0x90, 60, 0]),
                (1.0, [0xC1, 5, 0]),
                // Beat 3 is a beat after the change to 60 bpm.
                (2.0, [0x91, 64, 90]),
                (3.0, [0x81, 64, 0]),
            ]
        );
        assert_eq!(f.seconds, 3.0);
        assert_eq!(f.notes(), 2);
    }

    #[test]
    fn smpte_time_is_read_in_frames_and_ticks() {
        // 25 fps, 40 ticks per frame: 1000 ticks a second.
        let division = u16::from_be_bytes([(-25i8) as u8, 40]);
        let t = vec![0x87, 0x68, 0x90, 60, 100, 0x00, 0xFF, 0x2F, 0x00]; // tick 1000
        let f = read_smf(&file(0, division, &[t])).unwrap();
        assert_eq!(f.events[0].seconds, 1.0);
    }

    #[test]
    fn what_the_capture_writes_reads_back_at_the_same_times() {
        let sr = 48_000u32;
        let events = vec![
            Event {
                frame: 48_000,
                input: 1,
                data: [0x90, 60, 100],
            },
            Event {
                frame: 72_000,
                input: 1,
                data: [0x80, 60, 0],
            },
            Event {
                frame: 96_000,
                input: 0,
                data: [0xB0, 1, 64],
            },
        ];
        let cap = Capture {
            events,
            now: 100_000,
            sample_rate: sr,
        };
        let bytes = cap.to_smf(&SmfOptions {
            tempo: 97.0,
            origin: Some(0),
            ..SmfOptions::default()
        });
        let f = read_smf(&bytes).unwrap();
        let times: Vec<f64> = f.events.iter().map(|e| e.seconds).collect();
        for (got, want) in times.iter().zip([1.0, 1.5, 2.0]) {
            assert!((got - want).abs() < 0.001, "{times:?}");
        }
        assert_eq!(f.events.len(), 3);
    }

    #[test]
    fn damaged_files_are_refused_or_read_as_far_as_they_go() {
        assert!(read_smf(b"").is_err());
        assert!(read_smf(b"RIFF....WAVEfmt ").is_err());
        assert!(read_smf(&file(1, 0, &[vec![]])).is_err());
        // A track cut off in the middle of a message keeps what came before.
        let t = vec![0x00, 0x90, 60, 100, 0x10, 0x80, 60];
        let f = read_smf(&file(0, 96, &[t])).unwrap();
        assert_eq!(f.events.len(), 1);
        // A chunk that says it is longer than the file.
        let mut bytes = file(0, 96, &[vec![0x00, 0x90, 60, 100]]);
        let n = bytes.len();
        bytes[n - 8..n - 4].copy_from_slice(&1000u32.to_be_bytes());
        assert_eq!(read_smf(&bytes).unwrap().events.len(), 1);
    }

    #[test]
    fn an_rmi_file_is_unwrapped() {
        let smf = file(0, 96, &[vec![0x00, 0x90, 60, 100, 0x00, 0xFF, 0x2F, 0x00]]);
        let mut rmi = b"RIFF".to_vec();
        rmi.extend(((smf.len() + 12) as u32).to_le_bytes());
        rmi.extend(b"RMIDdata");
        rmi.extend((smf.len() as u32).to_le_bytes());
        rmi.extend(&smf);
        assert_eq!(read_smf(&rmi).unwrap().events.len(), 1);
    }
}
