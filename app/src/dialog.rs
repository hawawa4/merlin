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
    Vst2Dir,
    CaptureDir,
    WineProgram,
    YabridgeDir,
    Installer,
}

impl Want {
    fn folder(self) -> bool {
        !matches!(self, Self::WineProgram | Self::Installer)
    }

    fn title(self) -> &'static str {
        match self {
            Self::PluginDir => "Choose a folder with plugins",
            Self::SoundDir => "Choose a folder with SoundFonts",
            Self::Vst2Dir => "Choose a folder with Windows VST2 plugins",
            Self::CaptureDir => "Choose where MIDI captures are saved",
            Self::WineProgram => "Choose the wine program",
            Self::YabridgeDir => "Choose the yabridge folder",
            Self::Installer => "Choose a Windows plugin installer",
        }
    }
}

/// A dialog that is open: what for, and where its answer will arrive.
pub struct Open {
    pub want: Want,
    answer: Receiver<Result<Option<PathBuf>, String>>,
}

/// What a poll of an open dialog found.
pub enum Poll {
    Waiting,
    Chosen(Want, PathBuf),
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
            Ok(Ok(Some(p))) => Poll::Chosen(self.want, p),
            Ok(Ok(None)) => Poll::Cancelled,
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
    let args = match (tool, want.folder()) {
        ("kdialog", true) => vec![
            "--title".into(),
            title,
            "--getexistingdirectory".into(),
            start,
        ],
        ("kdialog", false) => vec!["--title".into(), title, "--getopenfilename".into(), start],
        (_, true) => vec![
            "--file-selection".into(),
            "--directory".into(),
            format!("--title={title}"),
            format!("--filename={start}"),
        ],
        (_, false) => vec![
            "--file-selection".into(),
            format!("--title={title}"),
            format!("--filename={start}"),
        ],
    };
    (tool.to_owned(), args)
}

fn run(want: Want, start: &Path) -> Result<Option<PathBuf>, String> {
    for tool in tools() {
        let (program, args) = command(tool, want, start);
        let output = match Command::new(&program).args(&args).output() {
            Ok(o) => o,
            // Not installed: try the other one.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(format!("cannot run {program}: {e}")),
        };
        let text = String::from_utf8_lossy(&output.stdout);
        let line = text.lines().next().unwrap_or("").trim();
        // Cancelling gives an empty answer and a non-zero exit.
        return Ok((output.status.success() && !line.is_empty()).then(|| PathBuf::from(line)));
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
}
