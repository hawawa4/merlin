//! Programs installed in the prefix that a person would want to open: the "manager" or
//! "downloader" app many plugin vendors ship (Native Access, Arturia Software Center, Plugin
//! Alliance Installation Manager, ...) instead of installing plugins directly.
//!
//! They are found the way Windows' own Start menu finds them: through the shortcuts (`.lnk`
//! files) an installer leaves in the Start Menu and on the Desktop. Wine's `IShellLink` writes
//! the target's full path into every shortcut it saves, so there is no need to guess which of
//! the `.exe` files below `Program Files` is the one to start.

use std::path::{Path, PathBuf};

use crate::Runtime;

/// Folders (relative to a prefix's `drive_c`) that hold shortcuts for every user.
const SHARED_SHORTCUTS: &[&str] = &[
    "ProgramData/Microsoft/Windows/Start Menu/Programs",
    "users/Public/Desktop",
    "users/Public/Public Desktop",
];
/// The same, below `drive_c/users/<name>/`.
const USER_SHORTCUTS: &[&str] = &[
    "AppData/Roaming/Microsoft/Windows/Start Menu/Programs",
    "Desktop",
];
/// How deep below a shortcut folder `.lnk` files are looked for (vendors use subfolders).
const MAX_DEPTH: usize = 4;

/// A Windows program in the prefix that can be started.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WindowsApp {
    /// What the shortcut is called, e.g. `Native Access`.
    pub name: String,
    /// The program, as a path on this machine.
    pub exe: PathBuf,
    /// The folder to start it in (the shortcut's, or the program's own).
    pub dir: PathBuf,
}

impl Runtime {
    /// The programs installed in this prefix that have a shortcut, by name.
    pub fn installed_apps(&self) -> Vec<WindowsApp> {
        installed_apps(&self.prefix)
    }

    /// The command that starts `app` in this prefix.
    pub fn app_command(&self, app: &WindowsApp) -> std::process::Command {
        let mut cmd = self.command(&app.exe);
        cmd.current_dir(&app.dir);
        cmd
    }
}

/// The programs with a Start Menu or Desktop shortcut in `prefix`, sorted by name. Uninstallers,
/// documentation and Wine's own programs are left out.
pub fn installed_apps(prefix: &Path) -> Vec<WindowsApp> {
    let drive_c = prefix.join("drive_c");
    let mut folders: Vec<PathBuf> = SHARED_SHORTCUTS.iter().map(|d| drive_c.join(d)).collect();
    if let Ok(users) = std::fs::read_dir(drive_c.join("users")) {
        for user in users.flatten().filter(|u| u.path().is_dir()) {
            folders.extend(USER_SHORTCUTS.iter().map(|d| user.path().join(d)));
        }
    }
    let mut links = Vec::new();
    for folder in &folders {
        collect_links(folder, MAX_DEPTH, &mut links);
    }
    links.sort();

    let mut apps: Vec<WindowsApp> = Vec::new();
    for link in links {
        let Some(app) = app_of_link(prefix, &link) else {
            continue;
        };
        // The same program is usually on the Desktop and in the Start Menu.
        if !apps.iter().any(|a| same_file(&a.exe, &app.exe)) {
            apps.push(app);
        }
    }
    apps.sort_by_key(|a| a.name.to_lowercase());
    apps
}

fn same_file(a: &Path, b: &Path) -> bool {
    a.to_string_lossy()
        .eq_ignore_ascii_case(&b.to_string_lossy())
}

fn collect_links(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        // A linked folder is not followed: a prefix's `Desktop` and `My Documents` are links
        // into the user's own home, and what is in there is not ours to walk.
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_dir() {
            if depth > 0 {
                collect_links(&path, depth - 1, out);
            }
        } else if has_extension(&path, "lnk") {
            out.push(path);
        }
    }
}

fn has_extension(path: &Path, ext: &str) -> bool {
    path.extension()
        .is_some_and(|e| e.to_string_lossy().eq_ignore_ascii_case(ext))
}

/// The program a shortcut starts, unless it is not one to offer.
fn app_of_link(prefix: &Path, link: &Path) -> Option<WindowsApp> {
    let shortcut = parse_lnk(&std::fs::read(link).ok()?)?;
    let target = shortcut.target?;
    if !target.to_ascii_lowercase().ends_with(".exe") {
        return None; // a document, a web page, a folder
    }
    // Wine's own programs (notepad, regedit, ...) are not plugin managers.
    if target.to_ascii_lowercase().starts_with("c:\\windows\\") {
        return None;
    }
    let name = link.file_stem()?.to_string_lossy().trim().to_owned();
    let file = target.rsplit(['\\', '/']).next().unwrap_or(&target);
    if name.is_empty() || is_uninstaller(&name, file) {
        return None;
    }
    let exe = unix_path(prefix, &target).filter(|p| p.is_file())?;
    let dir = shortcut
        .working_dir
        .and_then(|d| unix_path(prefix, &d))
        .filter(|d| d.is_dir())
        .or_else(|| exe.parent().map(Path::to_path_buf))?;
    Some(WindowsApp { name, exe, dir })
}

fn is_uninstaller(shortcut: &str, file: &str) -> bool {
    let (shortcut, file) = (shortcut.to_lowercase(), file.to_lowercase());
    shortcut.contains("uninstall")
        || shortcut.contains("uninst")
        || file.starts_with("unins") // Inno Setup's unins000.exe
        || file.contains("uninstall")
}

/// A Windows path (`C:\Program Files\X\x.exe`) as a path on this machine, in `prefix`. Windows
/// ignores case and the disk does not, so each part is matched either way. `None` when there is no
/// such file, or the drive is not one of the prefix's.
fn unix_path(prefix: &Path, windows: &str) -> Option<PathBuf> {
    let (drive, rest) = windows.split_once(':')?;
    if drive.len() != 1 || !drive.is_ascii() {
        return None;
    }
    let letter = drive.to_ascii_lowercase();
    // `dosdevices/c:` is a link to `drive_c`; use where it leads, so the path is the real one.
    let mut root = std::fs::canonicalize(prefix.join("dosdevices").join(format!("{letter}:")))
        .or_else(|_| match letter.as_str() {
            "c" => Ok(prefix.join("drive_c")),
            _ => Err(()),
        })
        .ok()?;
    for part in rest.split(['\\', '/']).filter(|p| !p.is_empty()) {
        let exact = root.join(part);
        root = if exact.exists() {
            exact
        } else {
            std::fs::read_dir(&root)
                .ok()?
                .flatten()
                .find(|e| e.file_name().to_string_lossy().eq_ignore_ascii_case(part))?
                .path()
        };
    }
    Some(root)
}

/// What is read from a shortcut.
#[derive(Debug, Default, PartialEq, Eq)]
struct Shortcut {
    /// The program, as a Windows path.
    target: Option<String>,
    working_dir: Option<String>,
}

/// Reads a Shell Link (`.lnk`) file, as far as it names the program: the target comes from the
/// `LinkInfo` block (a local path), the working directory from the string data after it.
/// Format: [MS-SHLLINK].
fn parse_lnk(b: &[u8]) -> Option<Shortcut> {
    const HEADER_SIZE: usize = 0x4c;
    const HAS_ID_LIST: u32 = 1;
    const HAS_LINK_INFO: u32 = 1 << 1;
    const HAS_NAME: u32 = 1 << 2;
    const HAS_RELATIVE_PATH: u32 = 1 << 3;
    const HAS_WORKING_DIR: u32 = 1 << 4;
    const IS_UNICODE: u32 = 1 << 7;

    let u16_at = |at: usize| -> Option<usize> {
        Some(u16::from_le_bytes(b.get(at..at + 2)?.try_into().ok()?) as usize)
    };
    let u32_at = |at: usize| -> Option<usize> {
        Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?) as usize)
    };
    if u32_at(0)? != HEADER_SIZE {
        return None;
    }
    let flags = u32_at(0x14)? as u32;
    let mut pos = HEADER_SIZE;
    if flags & HAS_ID_LIST != 0 {
        pos += 2 + u16_at(pos)?;
    }
    let mut shortcut = Shortcut::default();
    if flags & HAS_LINK_INFO != 0 {
        let start = pos;
        let (size, header, link_flags) = (u32_at(start)?, u32_at(start + 4)?, u32_at(start + 8)?);
        if link_flags & 1 != 0 {
            // A local path: the base path, then what is left of it to the file.
            let mut path = ansi_at(b, start + u32_at(start + 16)?)?;
            path.push_str(&ansi_at(b, start + u32_at(start + 24)?)?);
            if header >= 0x24 {
                // Newer writers add the same two strings in UTF-16, which can hold more.
                let (base, suffix) = (u32_at(start + 28)?, u32_at(start + 32)?);
                if base != 0 {
                    let mut wide = utf16_at(b, start + base)?;
                    if suffix != 0 {
                        wide.push_str(&utf16_at(b, start + suffix)?);
                    }
                    path = wide;
                }
            }
            shortcut.target = Some(path);
        }
        pos = start + size;
    }
    // Then strings with a length in characters: name, relative path, working directory, ...
    let unicode = flags & IS_UNICODE != 0;
    let mut string = |present: bool| -> Option<Option<String>> {
        if !present {
            return Some(None);
        }
        let chars = u16_at(pos)?;
        pos += 2;
        let (text, bytes) = if unicode {
            let units = utf16_units(b.get(pos..pos + chars * 2)?);
            (String::from_utf16_lossy(&units), chars * 2)
        } else {
            (decode_ansi(b.get(pos..pos + chars)?), chars)
        };
        pos += bytes;
        Some(Some(text))
    };
    string(flags & HAS_NAME != 0)?;
    string(flags & HAS_RELATIVE_PATH != 0)?;
    shortcut.working_dir = string(flags & HAS_WORKING_DIR != 0)?.filter(|d| !d.is_empty());
    Some(shortcut)
}

/// The zero-terminated 8-bit string at `at`.
fn ansi_at(b: &[u8], at: usize) -> Option<String> {
    let rest = b.get(at..)?;
    let end = rest.iter().position(|&c| c == 0)?;
    Some(decode_ansi(&rest[..end]))
}

/// UTF-8 when it is, else one character per byte (what Windows' Latin-1 code page does).
fn decode_ansi(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(s) => s.to_owned(),
        Err(_) => bytes.iter().map(|&c| c as char).collect(),
    }
}

/// The zero-terminated UTF-16 string at `at`.
fn utf16_at(b: &[u8], at: usize) -> Option<String> {
    let units: Vec<u16> = utf16_units(b.get(at..)?)
        .into_iter()
        .take_while(|&u| u != 0)
        .collect();
    Some(String::from_utf16_lossy(&units))
}

/// Little-endian UTF-16 code units (a trailing odd byte is dropped).
fn utf16_units(raw: &[u8]) -> Vec<u16> {
    raw.as_chunks::<2>()
        .0
        .iter()
        .map(|c| u16::from_le_bytes(*c))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A shortcut the way Wine's `IShellLink` writes one: header, LinkInfo with the local path,
    /// then UTF-16 strings (name, working directory).
    fn lnk(target: &str, name: Option<&str>, working_dir: Option<&str>) -> Vec<u8> {
        let mut out = vec![0u8; 0x4c];
        out[..4].copy_from_slice(&0x4cu32.to_le_bytes());
        let mut flags = 1u32 << 1 | 1 << 7; // link info, unicode
        if name.is_some() {
            flags |= 1 << 2;
        }
        if working_dir.is_some() {
            flags |= 1 << 4;
        }
        out[0x14..0x18].copy_from_slice(&flags.to_le_bytes());
        // LinkInfo: 28-byte header, a (here empty) volume table, the path, an empty suffix.
        let volume = [0u8; 17];
        let path_at = 28 + volume.len();
        let total = path_at + target.len() + 1 + 1;
        let mut info = Vec::new();
        for field in [total, 28, 1, 28, path_at, 0, path_at + target.len() + 1] {
            info.extend_from_slice(&(field as u32).to_le_bytes());
        }
        info.extend_from_slice(&volume);
        info.extend_from_slice(target.as_bytes());
        info.extend_from_slice(&[0, 0]);
        out.extend_from_slice(&info);
        for text in [name, working_dir].into_iter().flatten() {
            let units: Vec<u16> = text.encode_utf16().collect();
            out.extend_from_slice(&(units.len() as u16).to_le_bytes());
            for u in units {
                out.extend_from_slice(&u.to_le_bytes());
            }
        }
        out
    }

    fn tempdir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("merlin-apps-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn touch(path: &Path) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, b"x").unwrap();
    }

    /// A prefix with `C:` and `Z:` set up like Wine does.
    fn prefix(name: &str) -> PathBuf {
        let prefix = tempdir(name).canonicalize().unwrap();
        std::fs::create_dir_all(prefix.join("drive_c/users/me")).unwrap();
        std::fs::create_dir_all(prefix.join("dosdevices")).unwrap();
        std::os::unix::fs::symlink("../drive_c", prefix.join("dosdevices/c:")).unwrap();
        std::os::unix::fs::symlink("/", prefix.join("dosdevices/z:")).unwrap();
        prefix
    }

    const MENU: &str = "drive_c/ProgramData/Microsoft/Windows/Start Menu/Programs";

    #[test]
    fn reads_the_target_and_working_dir_of_a_shortcut() {
        let bytes = lnk(
            "C:\\Program Files\\Acme\\Acme Manager.exe",
            Some("Acme"),
            Some("C:\\Program Files\\Acme"),
        );
        assert_eq!(
            parse_lnk(&bytes),
            Some(Shortcut {
                target: Some("C:\\Program Files\\Acme\\Acme Manager.exe".into()),
                working_dir: Some("C:\\Program Files\\Acme".into()),
            })
        );
        let bare = lnk("C:\\a.exe", None, None);
        assert_eq!(parse_lnk(&bare).unwrap().working_dir, None);
    }

    #[test]
    fn something_that_is_not_a_shortcut_is_not_one() {
        assert_eq!(parse_lnk(b""), None);
        assert_eq!(parse_lnk(b"MZ this is an exe, not a shortcut"), None);
        let cut = lnk("C:\\a.exe", Some("A"), None);
        // A shortcut cut short anywhere must be refused, not panic.
        for n in 0..cut.len() {
            let _ = parse_lnk(&cut[..n]);
        }
    }

    #[test]
    fn finds_managers_through_their_shortcuts_and_leaves_the_rest() {
        let prefix = prefix("find");
        let c = prefix.join("drive_c");
        for exe in [
            "Program Files/Acme/Acme Manager.exe",
            "Program Files/Acme/unins000.exe",
            "Program Files/Beta/Beta.exe",
            "windows/notepad.exe",
        ] {
            touch(&c.join(exe));
        }
        let menu = prefix.join(MENU);
        let write = |dir: &Path, name: &str, target: &str| {
            std::fs::create_dir_all(dir).unwrap();
            std::fs::write(dir.join(name), lnk(target, None, None)).unwrap();
        };
        // The manager: in a vendor folder of the Start Menu, and again on the user's Desktop.
        write(
            &menu.join("Acme"),
            "Acme Manager.lnk",
            // Wine writes the case the file has; Windows programs often do not.
            "c:\\program files\\acme\\ACME MANAGER.EXE",
        );
        write(
            &c.join("users/me/Desktop"),
            "Acme Manager.lnk",
            "C:\\Program Files\\Acme\\Acme Manager.exe",
        );
        write(
            &menu.join("Acme"),
            "Uninstall Acme.lnk",
            "C:\\Program Files\\Acme\\unins000.exe",
        );
        write(&menu, "beta.lnk", "C:\\Program Files\\Beta\\Beta.exe");
        write(&menu, "Notepad.lnk", "C:\\windows\\notepad.exe");
        write(&menu, "Manual.lnk", "C:\\Program Files\\Acme\\Manual.pdf");
        write(&menu, "Gone.lnk", "C:\\Program Files\\Gone\\Gone.exe");
        // Z: reaches the whole machine.
        let outside = tempdir("outside").join("Tool.exe");
        touch(&outside);
        write(
            &menu,
            "Tool.lnk",
            &format!("Z:{}", outside.display()).replace('/', "\\"),
        );

        let apps = installed_apps(&prefix);
        let names: Vec<&str> = apps.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, ["Acme Manager", "beta", "Tool"]);
        assert_eq!(apps[0].exe, c.join("Program Files/Acme/Acme Manager.exe"));
        assert_eq!(apps[0].dir, c.join("Program Files/Acme"));
        assert_eq!(apps[2].exe, outside);
    }

    #[test]
    fn folders_that_are_links_are_not_walked() {
        let prefix = prefix("links");
        touch(&prefix.join("drive_c/Program Files/Acme/Acme.exe"));
        let elsewhere = tempdir("elsewhere");
        std::fs::write(
            elsewhere.join("Acme.lnk"),
            lnk("C:\\Program Files\\Acme\\Acme.exe", None, None),
        )
        .unwrap();
        let menu = prefix.join(MENU);
        std::fs::create_dir_all(&menu).unwrap();
        std::os::unix::fs::symlink(&elsewhere, menu.join("Linked")).unwrap();
        assert!(installed_apps(&prefix).is_empty());
        // The same shortcut inside the folder itself is found.
        std::fs::copy(elsewhere.join("Acme.lnk"), menu.join("Acme.lnk")).unwrap();
        assert_eq!(installed_apps(&prefix).len(), 1);
    }

    #[test]
    fn a_prefix_with_nothing_installed_has_no_apps() {
        assert!(installed_apps(&prefix("empty")).is_empty());
        assert!(installed_apps(Path::new("/nonexistent/prefix")).is_empty());
    }

    #[test]
    fn apps_start_in_their_own_folder_under_the_prefix() {
        let rt = Runtime {
            wine: "/opt/wine/bin/wine".into(),
            yabridge_dir: "/opt/yab".into(),
            prefix: "/p".into(),
        };
        let app = WindowsApp {
            name: "Acme".into(),
            exe: "/p/drive_c/Program Files/Acme/Acme.exe".into(),
            dir: "/p/drive_c/Program Files/Acme".into(),
        };
        let cmd = rt.app_command(&app);
        assert_eq!(cmd.get_program(), "/opt/wine/bin/wine");
        let args: Vec<_> = cmd.get_args().collect();
        assert_eq!(args, ["/p/drive_c/Program Files/Acme/Acme.exe"]);
        assert_eq!(
            cmd.get_current_dir(),
            Some(Path::new("/p/drive_c/Program Files/Acme"))
        );
    }
}
