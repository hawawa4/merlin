//! The Setlist screen: songs in playing order on the left; the selected song's tempo and parts
//! on the right. A part is a snapshot plus a tempo and a note.

use egui::{Color32, RichText};

use crate::session::{Session, TrackEdit, TrackState};
use crate::ui::{Action, PartEdit, SongEdit, UiState};

pub fn show(ui: &mut egui::Ui, s: &Session, st: &mut UiState, actions: &mut Vec<Action>) {
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.heading("Setlist");
        ui.separator();
        if ui.button("Close").clicked() {
            actions.push(Action::ToggleSetlist);
        }
    });
    ui.label(
        RichText::new(
            "Songs in playing order. A song has parts; a part brings back a snapshot (and can change the tempo) when you enter it. Move through them with Next part / Previous part, mapped to a foot switch in Settings > Controls, or in the Perform view. Everything is saved in the rig.",
        )
        .small()
        .weak(),
    );
    ui.add_space(6.0);
    st.edit_song = st.edit_song.min(s.songs().len().saturating_sub(1));
    egui::Panel::left("setlist-songs")
        .default_size(300.0)
        .show(ui, |ui| songs(ui, s, st, actions));
    egui::CentralPanel::default().show(ui, |ui| {
        if s.songs().is_empty() {
            ui.label("Add a song to begin.");
        } else {
            song_editor(ui, s, st, actions);
        }
    });
}

fn songs(ui: &mut egui::Ui, s: &Session, st: &mut UiState, actions: &mut Vec<Action>) {
    ui.strong("Songs");
    let here = s.position().map(|(song, _)| song);
    for (i, song) in s.songs().iter().enumerate() {
        ui.horizontal(|ui| {
            let mut text = RichText::new(format!("{}  {}", i + 1, song.label(i)));
            if here == Some(i) {
                text = text.color(Color32::from_rgb(0x6c, 0xc9, 0xe0));
            }
            if ui.selectable_label(st.edit_song == i, text).clicked() {
                st.edit_song = i;
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .small_button("x")
                    .on_hover_text("Remove the song")
                    .clicked()
                {
                    actions.push(Action::RemoveSong(i));
                }
                if ui
                    .small_button("v")
                    .on_hover_text("Later in the setlist")
                    .clicked()
                {
                    actions.push(Action::MoveSong(i, true));
                }
                if ui
                    .small_button("^")
                    .on_hover_text("Earlier in the setlist")
                    .clicked()
                {
                    actions.push(Action::MoveSong(i, false));
                }
            });
        });
    }
    ui.add_space(6.0);
    if ui.button("+ Add song").clicked() {
        actions.push(Action::AddSong);
    }
}

fn song_editor(ui: &mut egui::Ui, s: &Session, st: &mut UiState, actions: &mut Vec<Action>) {
    let n = st.edit_song;
    let Some(song) = s.songs().get(n) else { return };
    ui.horizontal(|ui| {
        ui.label("Name");
        let mut name = song.name.clone();
        if ui
            .add(
                egui::TextEdit::singleline(&mut name)
                    .hint_text(format!("Song {}", n + 1))
                    .desired_width(240.0),
            )
            .changed()
        {
            actions.push(Action::EditSong(n, SongEdit::Name(name)));
        }
        ui.label("tempo");
        let mut tempo = song.tempo;
        if ui
            .add(
                egui::DragValue::new(&mut tempo)
                    .range(merlin_model::MIN_TEMPO..=merlin_model::MAX_TEMPO)
                    .suffix(" bpm"),
            )
            .changed()
        {
            actions.push(Action::EditSong(n, SongEdit::Tempo(tempo)));
        }
        ui.label("beats per bar");
        let mut beats = song.beats_per_bar;
        if ui
            .add(egui::DragValue::new(&mut beats).range(1..=16))
            .changed()
        {
            actions.push(Action::EditSong(n, SongEdit::Beats(beats)));
        }
    });
    ui.horizontal(|ui| {
        ui.label("Note");
        let mut note = song.note.clone();
        if ui
            .add(
                egui::TextEdit::singleline(&mut note)
                    .hint_text("for the player")
                    .desired_width(500.0),
            )
            .changed()
        {
            actions.push(Action::EditSong(n, SongEdit::Note(note)));
        }
    });
    ui.add_space(8.0);
    ui.strong("Parts");
    let here = s.position();
    egui::ScrollArea::vertical().show(ui, |ui| {
        for (k, part) in song.parts.iter().enumerate() {
            egui::Frame::new()
                .fill(Color32::from_gray(30))
                .inner_margin(egui::Margin::symmetric(8, 4))
                .corner_radius(3)
                .stroke(if here == Some((n, k)) {
                    egui::Stroke::new(1.5, Color32::from_rgb(0x6c, 0xc9, 0xe0))
                } else {
                    egui::Stroke::NONE
                })
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        ui.label(format!("{}", k + 1));
                        let mut name = part.name.clone();
                        if ui
                            .add(
                                egui::TextEdit::singleline(&mut name)
                                    .hint_text(format!("Part {}", k + 1))
                                    .desired_width(150.0),
                            )
                            .changed()
                        {
                            actions.push(Action::EditPart(n, k, PartEdit::Name(name)));
                        }
                        snapshot_picker(ui, s, n, k, part.snapshot, actions);
                        let mut own = part.tempo.is_some();
                        if ui.checkbox(&mut own, "tempo").changed() {
                            actions.push(Action::EditPart(
                                n,
                                k,
                                PartEdit::Tempo(own.then_some(song.tempo)),
                            ));
                        }
                        if let Some(t) = part.tempo {
                            let mut t = t;
                            if ui
                                .add(
                                    egui::DragValue::new(&mut t)
                                        .range(
                                            merlin_model::MIN_TEMPO
                                                ..=merlin_model::MAX_TEMPO,
                                        )
                                        .suffix(" bpm"),
                                )
                                .changed()
                            {
                                actions.push(Action::EditPart(n, k, PartEdit::Tempo(Some(t))));
                            }
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui
                                .small_button("x")
                                .on_hover_text("Remove the part")
                                .clicked()
                            {
                                actions.push(Action::RemovePart(n, k));
                            }
                            if ui.small_button("v").clicked() {
                                actions.push(Action::MovePart(n, k, true));
                            }
                            if ui.small_button("^").clicked() {
                                actions.push(Action::MovePart(n, k, false));
                            }
                            if ui
                                .button("Go")
                                .on_hover_text("Enter this part now")
                                .clicked()
                            {
                                actions.push(Action::GoTo(n, k));
                            }
                        });
                    });
                    ui.horizontal(|ui| {
                        ui.add_space(20.0);
                        let mut note = part.note.clone();
                        if ui
                            .add(
                                egui::TextEdit::singleline(&mut note)
                                    .hint_text("cue for the player")
                                    .desired_width(500.0),
                            )
                            .changed()
                        {
                            actions.push(Action::EditPart(n, k, PartEdit::Note(note)));
                        }
                    });
                });
            ui.add_space(3.0);
        }
        ui.horizontal(|ui| {
            if ui
                .button("+ Part from the current sound")
                .on_hover_text(
                    "Saves the rig as it is now as a snapshot and adds a part that recalls it",
                )
                .clicked()
            {
                actions.push(Action::AddPart(n, true));
            }
            if ui.button("+ Empty part").clicked() {
                actions.push(Action::AddPart(n, false));
            }
        });
        ui.add_space(12.0);
        tracks(ui, s, st, n, actions);
        ui.add_space(12.0);
        timeline(ui, s, n, actions);
    });
}

/// Markers on the song's timeline.
fn timeline(ui: &mut egui::Ui, s: &Session, n: usize, actions: &mut Vec<Action>) {
    use merlin_model::{Marker, MarkerAction as M};
    let Some(song) = s.songs().get(n) else { return };
    ui.strong("Timeline");
    ui.label(
        RichText::new("Things that happen at a bar and beat of the song once the transport runs (bar 1 beat 1 is the first beat after the count-in): enter a part, show a cue to the player, stop, or send a Program Change or controller to other gear on the midi_out port. They follow the song's tempo and beats per bar.")
            .small()
            .weak(),
    );
    for (k, marker) in song.markers.iter().enumerate() {
        ui.horizontal(|ui| {
            let (mut bar, mut beat) = (marker.bar, u32::from(marker.beat));
            ui.label("bar");
            let a = ui.add(egui::DragValue::new(&mut bar).range(1..=9999));
            ui.label("beat");
            let b =
                ui.add(egui::DragValue::new(&mut beat).range(1..=u32::from(song.beats_per_bar)));
            let mut action = marker.action.clone();
            let kind = |a: &M| match a {
                M::GoToPart { .. } => 0,
                M::NextPart => 1,
                M::Cue { .. } => 2,
                M::Stop => 3,
                M::ProgramChange { .. } => 4,
                M::ControlChange { .. } => 5,
            };
            let names = [
                "Go to part",
                "Next part",
                "Cue",
                "Stop",
                "Program Change",
                "Controller",
            ];
            let mut chosen = kind(&action);
            egui::ComboBox::from_id_salt(("marker-kind", n, k))
                .selected_text(names[chosen])
                .width(130.0)
                .show_ui(ui, |ui| {
                    for (i, name) in names.iter().enumerate() {
                        ui.selectable_value(&mut chosen, i, *name);
                    }
                });
            if chosen != kind(&action) {
                action = match chosen {
                    0 => M::GoToPart { part: 0 },
                    1 => M::NextPart,
                    2 => M::Cue {
                        text: String::new(),
                    },
                    3 => M::Stop,
                    4 => M::ProgramChange {
                        channel: 0,
                        program: 0,
                    },
                    _ => M::ControlChange {
                        channel: 0,
                        number: 0,
                        value: 0,
                    },
                };
            }
            match &mut action {
                M::GoToPart { part } => {
                    let mut shown = u32::from(*part) + 1;
                    ui.label("part");
                    ui.add(
                        egui::DragValue::new(&mut shown)
                            .range(1..=merlin_model::MAX_PARTS as u32),
                    );
                    *part = (shown - 1) as u8;
                }
                M::Cue { text } => {
                    ui.add(
                        egui::TextEdit::singleline(text)
                            .hint_text("message")
                            .desired_width(260.0),
                    );
                }
                M::ProgramChange { channel, program } => {
                    let mut ch = u32::from(*channel) + 1;
                    ui.label("channel");
                    ui.add(egui::DragValue::new(&mut ch).range(1..=16));
                    *channel = (ch - 1) as u8;
                    ui.label("program");
                    ui.add(egui::DragValue::new(program).range(0..=127));
                }
                M::ControlChange {
                    channel,
                    number,
                    value,
                } => {
                    let mut ch = u32::from(*channel) + 1;
                    ui.label("channel");
                    ui.add(egui::DragValue::new(&mut ch).range(1..=16));
                    *channel = (ch - 1) as u8;
                    ui.label("controller");
                    ui.add(egui::DragValue::new(number).range(0..=127));
                    ui.label("value");
                    ui.add(egui::DragValue::new(value).range(0..=127));
                }
                M::NextPart | M::Stop => {}
            }
            let edited = Marker {
                bar,
                beat: beat as u8,
                action,
            };
            if a.changed() || b.changed() || edited != *marker {
                actions.push(Action::EditMarker(n, k, edited));
            }
            if ui
                .small_button("x")
                .on_hover_text("Remove the marker")
                .clicked()
            {
                actions.push(Action::RemoveMarker(n, k));
            }
        });
    }
    if ui.button("+ Marker").clicked() {
        let last = song.markers.last();
        actions.push(Action::AddMarker(
            n,
            Marker {
                bar: last.map_or(1, |m| m.bar + 1),
                beat: 1,
                action: M::Cue {
                    text: String::new(),
                },
            },
        ));
    }
}

/// The song's backing tracks.
fn tracks(ui: &mut egui::Ui, s: &Session, st: &mut UiState, n: usize, actions: &mut Vec<Action>) {
    let Some(song) = s.songs().get(n) else { return };
    ui.strong("Backing tracks");
    ui.label(
        RichText::new("Audio files (WAV, FLAC, MP3, OGG, M4A) that play from the start of the song when the transport starts, after the count-in. Tick *cue* to send a track to the click output instead of the main mix (a guide for your headphones). Drop a file on the window to add it.")
            .small()
            .weak(),
    );
    for (k, track) in song.tracks.iter().enumerate() {
        ui.horizontal(|ui| {
            let mut name = track.name.clone();
            if ui
                .add(
                    egui::TextEdit::singleline(&mut name)
                        .hint_text(track.label())
                        .desired_width(150.0),
                )
                .changed()
            {
                actions.push(Action::EditTrack(n, k, TrackEdit::Name(name)));
            }
            let file = track
                .path
                .file_name()
                .map_or_else(String::new, |f| f.to_string_lossy().into_owned());
            ui.label(RichText::new(file).small().weak())
                .on_hover_text(track.path.display().to_string());
            match s.track_state(n, k) {
                TrackState::Ready => {
                    let len = s
                        .track_seconds(n, k)
                        .map_or_else(String::new, |t| format!("  {}", clock(t)));
                    ui.colored_label(Color32::from_rgb(0x6c, 0xc9, 0x6a), format!("ready{len}"));
                }
                TrackState::Loading => {
                    ui.colored_label(Color32::from_rgb(0xe0, 0xb4, 0x4a), "loading...");
                }
                TrackState::Failed(why) => {
                    ui.colored_label(Color32::from_rgb(0xe0, 0x6c, 0x5a), "cannot play")
                        .on_hover_text(why);
                }
                TrackState::NotArmed => {
                    ui.label(
                        RichText::new("(opens when the song is entered)")
                            .small()
                            .weak(),
                    );
                }
            }
            let mut gain = track.gain;
            ui.spacing_mut().slider_width = 100.0;
            if ui
                .add(
                    egui::Slider::new(&mut gain, 0.0..=2.0)
                        .fixed_decimals(2)
                        .text("gain"),
                )
                .changed()
            {
                actions.push(Action::EditTrack(n, k, TrackEdit::Gain(gain)));
            }
            let mut mute = track.mute;
            if ui.checkbox(&mut mute, "mute").changed() {
                actions.push(Action::EditTrack(n, k, TrackEdit::Mute(mute)));
            }
            let mut cue = track.cue;
            if ui.checkbox(&mut cue, "cue").changed() {
                actions.push(Action::EditTrack(n, k, TrackEdit::Cue(cue)));
            }
            if ui
                .small_button("x")
                .on_hover_text("Remove the track")
                .clicked()
            {
                actions.push(Action::RemoveTrack(n, k));
            }
        });
    }
    ui.horizontal(|ui| {
        ui.add(
            egui::TextEdit::singleline(&mut st.track_text)
                .hint_text("/path/to/stem.wav")
                .desired_width(420.0),
        );
        if ui.button("+ Add track").clicked() && !st.track_text.trim().is_empty() {
            actions.push(Action::AddTrack(
                n,
                std::mem::take(&mut st.track_text).trim().into(),
            ));
        }
    });
}

/// `3:07` from seconds.
pub fn clock(secs: f64) -> String {
    let t = secs.max(0.0) as u64;
    format!("{}:{:02}", t / 60, t % 60)
}

fn snapshot_picker(
    ui: &mut egui::Ui,
    s: &Session,
    song: usize,
    part: usize,
    current: Option<usize>,
    actions: &mut Vec<Action>,
) {
    let text = match current.and_then(|n| s.snapshots().get(n).map(|x| (n, x))) {
        Some((n, snap)) => format!("{}  {}", n + 1, snap.name),
        None => "no snapshot".to_owned(),
    };
    egui::ComboBox::from_id_salt(("part-snapshot", song, part))
        .selected_text(text)
        .width(170.0)
        .show_ui(ui, |ui| {
            if ui
                .selectable_label(current.is_none(), "no snapshot (leave the sound alone)")
                .clicked()
            {
                actions.push(Action::EditPart(song, part, PartEdit::Snapshot(None)));
            }
            for (n, snap) in s.snapshots().iter().enumerate() {
                if ui
                    .selectable_label(current == Some(n), format!("{}  {}", n + 1, snap.name))
                    .clicked()
                {
                    actions.push(Action::EditPart(song, part, PartEdit::Snapshot(Some(n))));
                }
            }
        });
}
