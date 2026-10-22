//! The settings panel: plugins, audio, MIDI, Windows plugins, performance check.

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use egui::{Color32, RichText};

use crate::perf::{self, Status};
use crate::session::Session;
use crate::ui::{Action, UiState};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettingsTab {
    Plugins,
    Audio,
    Midi,
    Windows,
    Performance,
}

fn status_color(s: Status) -> Color32 {
    match s {
        Status::Ok => Color32::from_rgb(0x6c, 0xc9, 0x6a),
        Status::Warn => Color32::from_rgb(0xe0, 0xb4, 0x4a),
        Status::Fail => Color32::from_rgb(0xe0, 0x6c, 0x5a),
    }
}

pub fn show(ui: &mut egui::Ui, s: &Session, st: &mut UiState, actions: &mut Vec<Action>) {
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.heading("Settings");
        ui.separator();
        for (tab, name) in [
            (SettingsTab::Plugins, "Plugins"),
            (SettingsTab::Audio, "Audio"),
            (SettingsTab::Midi, "MIDI devices"),
            (SettingsTab::Windows, "Windows plugins"),
            (SettingsTab::Performance, "Performance check"),
        ] {
            ui.selectable_value(&mut st.settings_tab, tab, name);
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.button("Close").clicked() {
                actions.push(Action::ToggleSettings);
            }
        });
    });
    ui.separator();
    egui::ScrollArea::vertical()
        .auto_shrink(false)
        .show(ui, |ui| match st.settings_tab {
            SettingsTab::Plugins => plugins(ui, s, st, actions),
            SettingsTab::Audio => audio(ui, s, actions),
            SettingsTab::Midi => midi(ui, s, actions),
            SettingsTab::Windows => windows(ui, s, st, actions),
            SettingsTab::Performance => performance(ui, s, st),
        });
}

fn plugins(ui: &mut egui::Ui, s: &Session, st: &mut UiState, actions: &mut Vec<Action>) {
    ui.strong("Plugin folders");
    ui.label(
        RichText::new("The standard folders are always searched:")
            .small()
            .weak(),
    );
    for d in merlin_scanner::default_plugin_dirs() {
        ui.label(RichText::new(d.display().to_string()).small().weak());
    }
    ui.add_space(4.0);
    for (i, d) in s.settings.plugin_dirs.iter().enumerate() {
        ui.horizontal(|ui| {
            ui.label(d.display().to_string());
            if ui.small_button("remove").clicked() {
                actions.push(Action::RemoveDir(i));
            }
        });
    }
    ui.horizontal(|ui| {
        ui.add(
            egui::TextEdit::singleline(&mut st.dir_text)
                .hint_text("/path/to/more/plugins")
                .desired_width(360.0),
        );
        if ui.button("Add folder").clicked() && !st.dir_text.trim().is_empty() {
            actions.push(Action::AddDir(std::mem::take(&mut st.dir_text)));
        }
    });
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        ui.add_enabled_ui(!s.scanning(), |ui| {
            if ui.button("Rescan changed files").clicked() {
                actions.push(Action::Rescan(false));
            }
            if ui.button("Rescan everything").clicked() {
                actions.push(Action::Rescan(true));
            }
        });
        if s.scanning() {
            let file = s
                .scan_current
                .as_ref()
                .and_then(|p| p.file_name())
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            ui.label(format!("scanning… {file}"));
        } else {
            ui.label(format!(
                "{} plugins in {} files",
                s.found.len(),
                s.entries.len()
            ));
        }
    });

    let failed: Vec<_> = s.entries.iter().filter(|e| e.error.is_some()).collect();
    if !failed.is_empty() {
        ui.add_space(8.0);
        ui.strong("Files that could not be scanned");
        ui.label(
            RichText::new("They are tried again when the file changes, or with Retry.")
                .small()
                .weak(),
        );
        for e in failed {
            ui.horizontal_wrapped(|ui| {
                ui.colored_label(status_color(Status::Fail), e.path.display().to_string());
                ui.label(e.error.as_deref().unwrap_or_default());
                if ui.small_button("Retry").clicked() {
                    actions.push(Action::RetryFile(e.path.clone()));
                }
            });
        }
    }

    let quarantined = s.quarantine.entries();
    if !quarantined.is_empty() {
        ui.add_space(8.0);
        ui.strong("Quarantined plugins");
        ui.label(
            RichText::new("These crashed the app. They are not loaded automatically.")
                .small()
                .weak(),
        );
        for q in quarantined {
            ui.horizontal_wrapped(|ui| {
                ui.colored_label(status_color(Status::Warn), &q.name);
                ui.label(&q.reason);
                if ui.small_button("Remove from quarantine").clicked() {
                    actions.push(Action::Unquarantine(q.path.clone(), q.id.clone()));
                }
            });
        }
    }
}

fn audio(ui: &mut egui::Ui, s: &Session, actions: &mut Vec<Action>) {
    let Some(b) = s.backend() else {
        ui.label("Audio is not running.");
        return;
    };
    let (frames, rate) = (b.buffer_size(), s.sample_rate);
    ui.strong("Buffer size");
    ui.horizontal(|ui| {
        for n in [64u32, 128, 256, 512, 1024] {
            if ui.selectable_label(frames == n, format!("{n}")).clicked() && frames != n {
                actions.push(Action::SetBuffer(n));
            }
        }
        ui.label("frames");
    });
    let one_way = frames as f32 * 1000.0 / rate as f32;
    ui.label(format!(
        "{frames} frames at {rate} Hz: {one_way:.1} ms per buffer, about {:.1} ms round trip.",
        one_way * 2.0
    ));
    ui.label(
        RichText::new("Smaller is more responsive but needs a faster, well-set-up machine (see the performance check).")
            .small()
            .weak(),
    );
    ui.add_space(8.0);
    ui.strong("Now");
    ui.label(format!(
        "DSP load {:.1}%   dropouts {}   slowest callback {:.0} µs",
        b.cpu_load(),
        b.stats.xruns.load(Ordering::Relaxed),
        b.stats.max_ns.load(Ordering::Relaxed) as f64 / 1000.0,
    ));
    ui.label(format!("Outputs: {}", b.out_names.join(", ")));
}

fn midi(ui: &mut egui::Ui, s: &Session, actions: &mut Vec<Action>) {
    ui.strong("MIDI inputs");
    ui.label(
        RichText::new(
            "Each connected device gets its own input, so a slot can listen to one device only (zone and channel filters come with the slot settings).",
        )
        .small()
        .weak(),
    );
    ui.add_space(4.0);
    if s.midi_sources.is_empty() {
        ui.label("No MIDI devices connected.");
    }
    for m in &s.midi_sources {
        ui.horizontal(|ui| {
            ui.label(format!("input {}", m.input));
            ui.label(RichText::new(&m.device).strong());
        });
    }
    ui.add_space(6.0);
    if ui.button("Look for new devices").clicked() {
        actions.push(Action::RefreshMidi);
    }
    if let Some(b) = s.backend() {
        let others: Vec<String> = b
            .hardware_midi_sources()
            .into_iter()
            .filter(|d| !s.midi_sources.iter().any(|m| &m.device == d))
            .collect();
        if !others.is_empty() {
            ui.add_space(6.0);
            ui.label(
                RichText::new("Seen but not connected (all inputs in use):")
                    .small()
                    .weak(),
            );
            for d in others {
                ui.label(d);
            }
        }
    }
}

fn windows(ui: &mut egui::Ui, s: &Session, st: &mut UiState, actions: &mut Vec<Action>) {
    ui.strong("Wine and yabridge");
    ui.label(
        RichText::new("Windows plugins run through Wine and yabridge. A bundled, tested runtime will replace these paths; until then they can be set by hand. Changes apply after restarting the app.")
            .small()
            .weak(),
    );
    ui.add_space(6.0);
    for (label, text) in [
        ("Wine program", &mut st.wine_text),
        ("yabridge folder", &mut st.yab_text),
    ] {
        ui.horizontal(|ui| {
            ui.label(label);
            ui.add(egui::TextEdit::singleline(text).desired_width(460.0));
            let path = std::path::Path::new(text.as_str());
            if text.is_empty() {
                ui.label(RichText::new("not set").weak());
            } else if path.exists() {
                ui.colored_label(status_color(Status::Ok), "found");
            } else {
                ui.colored_label(status_color(Status::Fail), "does not exist");
            }
        });
    }
    if ui.button("Save").clicked() {
        actions.push(Action::SaveWine(st.wine_text.clone(), st.yab_text.clone()));
    }
    ui.add_space(12.0);
    windows_plugins(ui, s, st, actions);
    ui.add_space(8.0);
    ui.strong("In use right now");
    for var in ["WINELOADER", "PATH"] {
        let value = std::env::var(var).unwrap_or_default();
        let shown = if var == "PATH" {
            value
                .split(':')
                .filter(|p| p.contains("yabridge"))
                .collect::<Vec<_>>()
                .join(", ")
        } else {
            value
        };
        ui.label(format!(
            "{var}: {}",
            if shown.is_empty() {
                "(not set)"
            } else {
                &shown
            }
        ));
    }
}

/// Installing Windows plugins and what the Wine setup looks like (a small "Wine doctor").
fn windows_plugins(ui: &mut egui::Ui, s: &Session, st: &mut UiState, actions: &mut Vec<Action>) {
    ui.strong("Windows plugins");
    let Some(rt) = s.windows_runtime() else {
        ui.label(
            RichText::new(
                "Set the Wine program and the yabridge folder above to install Windows plugins.",
            )
            .small()
            .weak(),
        );
        return;
    };
    let check = |ui: &mut egui::Ui, ok: bool, text: String| {
        ui.horizontal(|ui| {
            ui.colored_label(
                status_color(if ok { Status::Ok } else { Status::Fail }),
                if ok { "OK  " } else { "FAIL" },
            );
            ui.label(text);
        });
    };
    check(
        ui,
        rt.version().is_some(),
        format!(
            "Wine: {}",
            rt.version().unwrap_or_else(|| "does not run".into())
        ),
    );
    check(
        ui,
        rt.chainloader().is_file() && rt.vst3_chainloader().is_file(),
        "yabridge: CLAP and VST3 chainloaders".to_owned(),
    );
    ui.horizontal(|ui| {
        let ready = rt.prefix_ready();
        ui.colored_label(
            status_color(if ready { Status::Ok } else { Status::Warn }),
            if ready { "OK  " } else { "WAIT" },
        );
        ui.label(format!(
            "Windows environment: {}{}",
            rt.prefix.display(),
            if ready {
                ""
            } else {
                " (made when you install the first plugin)"
            }
        ));
    });
    ui.add_space(6.0);
    ui.label("Installer (.exe or .msi)");
    ui.horizontal(|ui| {
        ui.add(
            egui::TextEdit::singleline(&mut st.installer_text)
                .hint_text("/path/to/Plugin-Setup.exe")
                .desired_width(460.0),
        );
        let busy = s.windows_busy();
        if ui
            .add_enabled(
                !busy && !st.installer_text.trim().is_empty(),
                egui::Button::new("Install"),
            )
            .clicked()
        {
            actions.push(Action::InstallWindows(st.installer_text.clone()));
        }
        if ui
            .add_enabled(!busy, egui::Button::new("Look for installed plugins"))
            .on_hover_text(
                "Use this after installing a plugin by hand into the Windows environment",
            )
            .clicked()
        {
            actions.push(Action::SyncWindows);
        }
    });
    if !s.windows_status.is_empty() {
        ui.label(&s.windows_status);
    }
    ui.add_space(4.0);
    ui.label(
        RichText::new(
            "Only 64-bit CLAP and VST3 plugins are picked up. Plugins with copy protection (iLok and the like) may not run. Prefer a native Linux version when there is one.",
        )
        .small()
        .weak(),
    );
}

fn performance(ui: &mut egui::Ui, s: &Session, st: &mut UiState) {
    let stale = st
        .perf
        .as_ref()
        .is_none_or(|(t, _)| t.elapsed() > Duration::from_secs(2));
    if stale && let Some(b) = s.backend() {
        let inputs = perf::gather(
            b.buffer_size(),
            s.sample_rate,
            b.stats.xruns.load(Ordering::Relaxed),
            b.cpu_load(),
        );
        st.perf = Some((Instant::now(), perf::evaluate(&inputs)));
    }
    let Some((_, checks)) = &st.perf else {
        ui.label("Audio is not running.");
        return;
    };
    let overall = perf::overall(checks);
    ui.horizontal(|ui| {
        ui.heading("Performance check");
        ui.colored_label(
            status_color(overall),
            match overall {
                Status::Ok => "ready for live playing",
                Status::Warn => "usable, with warnings",
                Status::Fail => "needs attention",
            },
        );
    });
    ui.add_space(6.0);
    for c in checks {
        ui.horizontal(|ui| {
            ui.colored_label(
                status_color(c.status),
                match c.status {
                    Status::Ok => "OK  ",
                    Status::Warn => "WARN",
                    Status::Fail => "FAIL",
                },
            );
            ui.strong(c.name);
            ui.label(&c.detail);
        });
        if let Some(fix) = &c.fix {
            ui.horizontal(|ui| {
                ui.add_space(52.0);
                ui.label(RichText::new(fix).color(Color32::from_gray(190)));
            });
        }
        ui.add_space(2.0);
    }
}
