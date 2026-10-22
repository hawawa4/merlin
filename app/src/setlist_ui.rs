//! The Setlist screen: songs in playing order on the left; the selected song's tempo and parts
//! on the right. A part is a snapshot plus a tempo and a note.

use egui::{Color32, RichText};

use crate::session::Session;
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
            song_editor(ui, s, st.edit_song, actions);
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

fn song_editor(ui: &mut egui::Ui, s: &Session, n: usize, actions: &mut Vec<Action>) {
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
    });
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
