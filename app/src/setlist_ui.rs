//! The Setlist screen: songs in playing order on the left; the selected song's tempo and parts
//! on the right. A part is a snapshot plus a tempo and a note.

use std::path::Path;

use egui::{Color32, RichText};

use crate::dialog::Want;

use crate::session::{Session, TrackEdit, TrackState};
use crate::ui::{Action, PartEdit, SongEdit, UiState};

pub fn show(ui: &mut egui::Ui, s: &Session, st: &mut UiState, actions: &mut Vec<Action>) {
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.heading("Setlist");
    });
    ui.label(
        RichText::new(
            "Songs in playing order. A song has parts; each part brings back one of your scenes (made in the Rack view) and can change the tempo when you enter it. Move through them with Next part / Previous part, mapped to a foot switch in Settings > Controls, or in the Perform view. Everything is saved in the rig.",
        )
        .small()
        .weak(),
    );
    ui.add_space(6.0);
    st.edit_song = st.edit_song.min(s.songs().len().saturating_sub(1));
    egui::Panel::left("setlist-songs")
        .default_size(300.0)
        .size_range(240.0..=520.0)
        .show(ui, |ui| songs(ui, s, st, actions));
    egui::Panel::right("setlist-library")
        .default_size(300.0)
        .size_range(220.0..=560.0)
        .show(ui, |ui| library(ui, s, st, actions));
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
    /// Each small button, and the gap between them.
    const BUTTON: f32 = 24.0;
    const GAP: f32 = 4.0;
    let height = ui.spacing().interact_size.y;
    for (i, song) in s.songs().iter().enumerate() {
        // One rectangle for the whole row, cut into the name and three buttons that sit at fixed
        // distances from its right edge: they line up in every row and cannot cover the name.
        let (row, _) = ui.allocate_exact_size(
            egui::vec2(ui.available_width(), height),
            egui::Sense::hover(),
        );
        let buttons_left = row.right() - 3.0 * BUTTON - 2.0 * GAP;
        let name_rect =
            egui::Rect::from_min_max(row.min, egui::pos2(buttons_left - GAP, row.bottom()));
        let response = ui.interact(
            name_rect,
            ui.make_persistent_id(("song-row", i)),
            egui::Sense::click(),
        );
        let selected = st.edit_song == i;
        if selected || response.hovered() {
            let fill = if selected {
                ui.visuals().selection.bg_fill
            } else {
                ui.visuals().widgets.hovered.weak_bg_fill
            };
            ui.painter().rect_filled(name_rect, 4.0, fill);
        }
        let mut text = RichText::new(format!("{}  {}", i + 1, song.label(i)));
        if here == Some(i) {
            text = text.color(Color32::from_rgb(0x6c, 0xc9, 0xe0));
        }
        let mut label = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(name_rect.shrink2(egui::vec2(6.0, 0.0)))
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
        );
        label.add(egui::Label::new(text).truncate().selectable(false));
        if response.clicked() {
            st.edit_song = i;
        }
        let tips = [
            "Earlier in the setlist",
            "Later in the setlist",
            "Remove the song",
        ];
        for (k, (label, tip)) in ["^", "v", "x"].into_iter().zip(tips).enumerate() {
            let at = egui::Rect::from_min_size(
                egui::pos2(buttons_left + k as f32 * (BUTTON + GAP), row.top()),
                egui::vec2(BUTTON, height),
            );
            if ui
                .put(at, egui::Button::new(label).small())
                .on_hover_text(tip)
                .clicked()
            {
                actions.push(match k {
                    0 => Action::MoveSong(i, false),
                    1 => Action::MoveSong(i, true),
                    _ => Action::RemoveSong(i),
                });
            }
        }
    }
    ui.add_space(6.0);
    if ui.button("+ Add song").clicked() {
        actions.push(Action::AddSong);
    }
}

fn song_editor(ui: &mut egui::Ui, s: &Session, st: &mut UiState, actions: &mut Vec<Action>) {
    let n = st.edit_song;
    let Some(song) = s.songs().get(n) else { return };
    ui.horizontal_wrapped(|ui| {
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
                    .desired_width((ui.available_width() - 4.0).max(120.0)),
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
                    ui.horizontal_wrapped(|ui| {
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
                                        .range(merlin_model::MIN_TEMPO..=merlin_model::MAX_TEMPO)
                                        .suffix(" bpm"),
                                )
                                .changed()
                            {
                                actions.push(Action::EditPart(n, k, PartEdit::Tempo(Some(t))));
                            }
                        }
                        if ui
                            .button("Go")
                            .on_hover_text("Enter this part now")
                            .clicked()
                        {
                            actions.push(Action::GoTo(n, k));
                        }
                        if ui.small_button("^").clicked() {
                            actions.push(Action::MovePart(n, k, false));
                        }
                        if ui.small_button("v").clicked() {
                            actions.push(Action::MovePart(n, k, true));
                        }
                        if ui
                            .small_button("x")
                            .on_hover_text("Remove the part")
                            .clicked()
                        {
                            actions.push(Action::RemovePart(n, k));
                        }
                    });
                    ui.horizontal(|ui| {
                        ui.add_space(20.0);
                        let mut note = part.note.clone();
                        if ui
                            .add(
                                egui::TextEdit::singleline(&mut note)
                                    .hint_text("cue for the player")
                                    .desired_width((ui.available_width() - 4.0).max(120.0)),
                            )
                            .changed()
                        {
                            actions.push(Action::EditPart(n, k, PartEdit::Note(note)));
                        }
                    });
                });
            ui.add_space(3.0);
        }
        if ui
            .button("+ Add part")
            .on_hover_text("Then choose which scene it brings back")
            .clicked()
        {
            actions.push(Action::AddPart(n, false));
        }
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
        ui.horizontal_wrapped(|ui| {
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
                        egui::DragValue::new(&mut shown).range(1..=merlin_model::MAX_PARTS as u32),
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
        ui.horizontal_wrapped(|ui| {
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
        if crate::ui::pick_button(
            ui,
            &mut st.picking,
            "+ Add tracks…",
            Want::BackingTrack(n),
            s.backing_dir(),
        ) {
            // The answer comes back through the dialog's poll.
        }
        ui.label(
            RichText::new("or double-click a file in the library on the right")
                .small()
                .weak(),
        );
    });
}

/// The backing-track library: the folder chosen in Settings > Audio, browsed like any file
/// manager. A double-click (or +) adds a file to the song being edited.
fn library(ui: &mut egui::Ui, s: &Session, st: &mut UiState, actions: &mut Vec<Action>) {
    use crate::icons::{self, Glyph};
    use crate::library;
    ui.strong("Backing tracks library");
    let Some(root) = s.backing_dir().map(Path::to_owned) else {
        ui.add_space(6.0);
        ui.label(
            RichText::new("Choose the folder where your backing tracks live and they are listed here, ready to add to a song.")
                .small()
                .weak(),
        );
        ui.add_space(4.0);
        crate::ui::pick_button(
            ui,
            &mut st.picking,
            "Choose folder…",
            Want::BackingDir,
            None,
        );
        return;
    };
    let dir = st.library.current(&root);
    st.library.load(&dir, false);
    // Where we are, relative to the root, and the ways to move.
    let here = dir
        .strip_prefix(&root)
        .ok()
        .filter(|r| !r.as_os_str().is_empty())
        .map(|r| format!("/{}", r.display()))
        .unwrap_or_default();
    let root_name = root.file_name().map_or_else(
        || root.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    );
    ui.horizontal(|ui| {
        let up = dir != root;
        if ui
            .add_enabled(up, egui::Button::new("Up").small())
            .on_hover_text("Up one folder")
            .clicked()
            && let Some(parent) = dir.parent()
        {
            st.library.dir = Some(parent.to_owned());
        }
        if ui
            .add(egui::Button::new("Refresh").small())
            .on_hover_text("Read the folder again")
            .clicked()
        {
            st.library.load(&dir, true);
        }
        crate::ui::pick_button(
            ui,
            &mut st.picking,
            "Change…",
            Want::BackingDir,
            Some(&root),
        );
    });
    ui.label(RichText::new(format!("{root_name}{here}")).small().weak())
        .on_hover_text(dir.display().to_string());
    ui.add(
        egui::TextEdit::singleline(&mut st.library.filter)
            .hint_text("filter…")
            .desired_width(f32::INFINITY),
    );
    ui.separator();
    let filter = st.library.filter.trim().to_lowercase();
    let song = st.edit_song;
    let has_song = !s.songs().is_empty();
    let mut enter = None;
    egui::ScrollArea::vertical()
        .auto_shrink(false)
        .show(ui, |ui| {
            if st.library.entries.is_empty() {
                ui.label(RichText::new("no audio files or folders here").weak());
            }
            for e in &st.library.entries {
                if !filter.is_empty() && !e.dir && !e.name.to_lowercase().contains(&filter) {
                    continue;
                }
                ui.horizontal(|ui| {
                    icons::show(
                        ui,
                        if e.dir {
                            Glyph::Folder
                        } else {
                            Glyph::AudioFile
                        },
                        crate::hw::TEXT_DIM,
                    );
                    if e.dir {
                        let r = ui
                            .add(
                                egui::Label::new(RichText::new(&e.name).strong())
                                    .sense(egui::Sense::click()),
                            )
                            .on_hover_cursor(egui::CursorIcon::PointingHand);
                        if r.clicked() {
                            enter = Some(e.path.clone());
                        }
                        return;
                    }
                    let r = ui
                        .add(
                            egui::Label::new(&e.name)
                                .truncate()
                                .sense(egui::Sense::click()),
                        )
                        .on_hover_text(format!(
                            "{}\n{}\nDouble-click to add it to the song",
                            e.path.display(),
                            library::size_text(e.size)
                        ));
                    if has_song && r.double_clicked() {
                        actions.push(Action::AddTrack(song, e.path.clone()));
                    }
                });
            }
        });
    if let Some(d) = enter {
        st.library.dir = Some(d);
    }
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
        None => "no scene".to_owned(),
    };
    egui::ComboBox::from_id_salt(("part-snapshot", song, part))
        .selected_text(text)
        .width(170.0)
        .show_ui(ui, |ui| {
            if ui
                .selectable_label(current.is_none(), "no scene (leave the sound alone)")
                .clicked()
            {
                actions.push(Action::EditPart(song, part, PartEdit::Snapshot(None)));
            }
            if s.snapshots().is_empty() {
                ui.label(
                    RichText::new("No scenes yet: store one in the Rack view")
                        .small()
                        .weak(),
                );
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
