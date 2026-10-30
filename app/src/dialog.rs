//! Choosing a folder or a file with the desktop's own dialog (KDE's kdialog, else GNOME's
//! zenity), so Settings never asks anyone to type a path. The dialog is another process and is
//! waited for on a thread: the app keeps playing and drawing while it is open.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::{self, Receiver, TryRecvError};

/// What a dialog was opened for, so the answer goes to the right place.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Want {
    PluginDir,
    SoundDir,
    CaptureDir,
    WineProgram,
    YabridgeDir,
    Installer,
    /// The folder of backing tracks the Setlist view browses.
    BackingDir,
    /// Audio files (several at once) to add as backing tracks to this song.
    BackingTrack(usize),
}

impl Want {
    fn folder(self) -> bool {
        !matches!(
            self,
            Self::WineProgram | Self::Installer | Self::BackingTrack(_)
        )
    }

    /// Several files can be picked at once.
    fn multiple(self) -> bool {
        matches!(self, Self::BackingTrack(_))
    }

    /// Only these files are offered (a label and glob patterns).
    fn filter(self) -> Option<(&'static str, &'static str)> {
        match self {
            Self::BackingTrack(_) => Some((
                "Audio files",
                "*.wav *.WAV *.flac *.FLAC *.mp3 *.MP3 *.ogg *.OGG *.m4a *.M4A",
            )),
            _ => None,
        }
    }

    fn title(self) -> &'static str {
        match self {
            Self::PluginDir => "Choose a folder with plugins (any kind)",
            Self::SoundDir => "Choose a folder with SoundFonts",
            Self::CaptureDir => "Choose where MIDI captures are saved",
            Self::WineProgram => "Choose the wine program",
            Self::YabridgeDir => "Choose the yabridge folder",
            Self::Installer => "Choose a Windows plugin installer",
            Self::BackingDir => "Choose the folder with your backing tracks",
            Self::BackingTrack(_) => "Choose backing tracks",
        }
    }
}

/// A dialog that is open: what for, and where its answer will arrive.
pub struct Open {
    pub want: Want,
    answer: Receiver<Result<Vec<PathBuf>, String>>,
}

/// What a poll of an open dialog found.
pub enum Poll {
    Waiting,
    Chosen(Want, Vec<PathBuf>),
    Cancelled,
    Failed(String),
}

impl Open {
    /// Opens the dialog on a thread, starting in `start` (or the home folder).
    pub fn start(want: Want, start: Option<&Path>) -> Self {
        let (tx, rx) = mpsc::channel();
        let start = start
            .filter(|p| p.is_dir())
            .map(Path::to_owned)
            .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
            .unwrap_or_else(|| PathBuf::from("/"));
        std::thread::spawn(move || {
            let _ = tx.send(run(want, &start));
        });
        Self { want, answer: rx }
    }

    pub fn poll(&self) -> Poll {
        match self.answer.try_recv() {
            Ok(Ok(p)) if !p.is_empty() => Poll::Chosen(self.want, p),
            Ok(Ok(_)) => Poll::Cancelled,
            Ok(Err(e)) => Poll::Failed(e),
            Err(TryRecvError::Empty) => Poll::Waiting,
            Err(TryRecvError::Disconnected) => Poll::Failed("the dialog thread ended".into()),
        }
    }
}

/// The dialog programs to try, KDE's first on a KDE desktop.
fn tools() -> [&'static str; 2] {
    let kde = std::env::var("XDG_CURRENT_DESKTOP").is_ok_and(|d| d.to_lowercase().contains("kde"));
    if kde {
        ["kdialog", "zenity"]
    } else {
        ["zenity", "kdialog"]
    }
}

/// The program and arguments that ask for `want`, starting in `start`.
fn command(tool: &str, want: Want, start: &Path) -> (String, Vec<String>) {
    let title = want.title().to_owned();
    let start = format!("{}/", start.display());
    let mut args: Vec<String> = Vec::new();
    if tool == "kdialog" {
        args.extend(["--title".into(), title]);
        if want.multiple() {
            args.extend(["--multiple".into(), "--separate-output".into()]);
        }
        args.push(
            if want.folder() {
                "--getexistingdirectory"
            } else {
                "--getopenfilename"
            }
            .into(),
        );
        args.push(start);
        if let Some((label, globs)) = want.filter() {
            args.push(format!("{label} ({globs})"));
        }
    } else {
        args.push("--file-selection".into());
        if want.folder() {
            args.push("--directory".into());
        }
        if want.multiple() {
            args.extend(["--multiple".into(), "--separator=\n".into()]);
        }
        args.push(format!("--title={title}"));
        args.push(format!("--filename={start}"));
        if let Some((label, globs)) = want.filter() {
            args.push(format!("--file-filter={label} | {globs}"));
        }
    }
    (tool.to_owned(), args)
}

/// Opens a folder in the desktop's default file manager. The manager runs on its own; we only
/// wait for it in a thread so it does not linger as a zombie.
pub fn open_folder(dir: &Path) -> Result<(), String> {
    let mut child = Command::new("xdg-open")
        .arg(dir)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => "xdg-open is not installed".to_owned(),
            _ => format!("cannot run xdg-open: {e}"),
        })?;
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

fn run(want: Want, start: &Path) -> Result<Vec<PathBuf>, String> {
    for tool in tools() {
        let (program, args) = command(tool, want, start);
        let output = match Command::new(&program).args(&args).output() {
            Ok(o) => o,
            // Not installed: try the other one.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(format!("cannot run {program}: {e}")),
        };
        // Cancelling gives an empty answer and a non-zero exit.
        if !output.status.success() {
            return Ok(Vec::new());
        }
        return Ok(String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(PathBuf::from)
            .collect());
    }
    Err("no file dialog found: install kdialog or zenity".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_folder_dialog_asks_for_a_directory_in_each_tool() {
        let (_, k) = command("kdialog", Want::SoundDir, Path::new("/home/me"));
        assert!(k.contains(&"--getexistingdirectory".to_owned()));
        assert_eq!(k.last().unwrap(), "/home/me/");
        let (_, z) = command("zenity", Want::SoundDir, Path::new("/home/me"));
        assert!(z.contains(&"--directory".to_owned()));
    }

    #[test]
    fn a_file_dialog_does_not_ask_for_a_directory() {
        let (_, k) = command("kdialog", Want::Installer, Path::new("/"));
        assert!(k.contains(&"--getopenfilename".to_owned()));
        let (_, z) = command("zenity", Want::WineProgram, Path::new("/"));
        assert!(!z.contains(&"--directory".to_owned()));
    }

    #[test]
    fn backing_tracks_ask_for_several_audio_files() {
        let (_, k) = command("kdialog", Want::BackingTrack(0), Path::new("/m"));
        assert!(
            k.contains(&"--multiple".to_owned()) && k.contains(&"--getopenfilename".to_owned())
        );
        assert!(k.last().unwrap().starts_with("Audio files (*.wav"));
        let (_, z) = command("zenity", Want::BackingTrack(2), Path::new("/m"));
        assert!(z.contains(&"--multiple".to_owned()) && !z.contains(&"--directory".to_owned()));
        assert!(
            z.iter()
                .any(|a| a.starts_with("--file-filter=Audio files | *.wav"))
        );
        let (_, d) = command("zenity", Want::BackingDir, Path::new("/m"));
        assert!(d.contains(&"--directory".to_owned()) && !d.contains(&"--multiple".to_owned()));
    }
}
