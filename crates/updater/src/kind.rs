//! How this copy of Merlin was installed, which decides how it updates.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Running from an AppImage: the one file replaces itself.
    AppImage,
    /// Installed by the `.deb`: the package manager owns the files.
    Deb,
    /// The tarball's binaries, placed by hand: they replace themselves.
    Binary,
    /// A platform no release is built for.
    Unsupported,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::AppImage => "appimage",
            Kind::Deb => "deb",
            Kind::Binary => "binary",
            Kind::Unsupported => "unsupported",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Install {
    pub kind: Kind,
    /// The file to replace: the AppImage, or the running executable (`merlin`) for the others.
    pub path: PathBuf,
}

impl Install {
    /// The release file to download for this install.
    pub fn asset_name(&self, version: &str) -> Option<String> {
        match self.kind {
            Kind::AppImage => Some(format!("merlin-{version}-linux-amd64.AppImage")),
            Kind::Deb => Some(format!("merlin_{version}_amd64.deb")),
            Kind::Binary => Some(format!("merlin-{version}-linux-amd64.tar.gz")),
            Kind::Unsupported => None,
        }
    }
}

/// Inspects the running process.
pub fn detect() -> Install {
    if std::env::consts::OS != "linux" || std::env::consts::ARCH != "x86_64" {
        return Install {
            kind: Kind::Unsupported,
            path: PathBuf::new(),
        };
    }
    let exe = std::env::current_exe().unwrap_or_default();
    detect_from(std::env::var_os("APPIMAGE").as_deref(), &exe)
}

/// Classifies an install from the `APPIMAGE` variable (set by an AppImage's runtime) and the
/// resolved path of the executable.
pub fn detect_from(appimage: Option<&OsStr>, exe: &Path) -> Install {
    if let Some(a) = appimage.filter(|a| !a.is_empty()) {
        let a = PathBuf::from(a);
        if a.metadata().is_ok_and(|m| m.is_file()) {
            return Install {
                kind: Kind::AppImage,
                path: a,
            };
        }
    }
    // After an update replaced the file under a running process, Linux names it "… (deleted)".
    let exe = exe.to_string_lossy();
    let exe = Path::new(exe.strip_suffix(" (deleted)").unwrap_or(&exe));
    let exe = std::fs::canonicalize(exe).unwrap_or_else(|_| exe.to_owned());
    // Package-managed files live under /usr; /usr/local is a hand-placed binary.
    let kind = if exe.starts_with("/usr/") && !exe.starts_with("/usr/local/") {
        Kind::Deb
    } else {
        Kind::Binary
    };
    Install { kind, path: exe }
}

/// Why a `.deb` update can't be run from here, or `None`: a GUI has no terminal for sudo, so
/// the install goes through polkit's own password dialog.
pub fn deb_prerequisites() -> Option<String> {
    ["pkexec", "apt-get"].into_iter().find_map(|tool| {
        Command::new(tool)
            .arg("--version")
            .output()
            .is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound)
            .then(|| format!("{tool} isn't installed"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usr_is_the_package_and_usr_local_is_by_hand() {
        assert_eq!(
            detect_from(None, Path::new("/usr/bin/merlin")).kind,
            Kind::Deb
        );
        assert_eq!(
            detect_from(None, Path::new("/usr/local/bin/merlin")).kind,
            Kind::Binary
        );
        assert_eq!(
            detect_from(None, Path::new("/home/me/merlin/merlin")).kind,
            Kind::Binary
        );
    }

    #[test]
    fn an_appimage_wins_over_where_it_is_mounted() {
        let me = std::env::current_exe().unwrap();
        let i = detect_from(
            Some(me.as_os_str()),
            Path::new("/tmp/.mount_merlinX/usr/bin/merlin"),
        );
        assert_eq!((i.kind, i.path.as_path()), (Kind::AppImage, me.as_path()));
        // A stale variable (the file is gone) is ignored.
        let i = detect_from(
            Some(OsStr::new("/nonexistent/Merlin.AppImage")),
            Path::new("/usr/bin/merlin"),
        );
        assert_eq!(i.kind, Kind::Deb);
    }

    #[test]
    fn a_replaced_executable_is_still_found() {
        let i = detect_from(None, Path::new("/home/me/merlin/merlin (deleted)"));
        assert_eq!(i.path, Path::new("/home/me/merlin/merlin"));
    }

    #[test]
    fn asset_names_follow_the_release_workflow() {
        let deb = Install {
            kind: Kind::Deb,
            path: "/usr/bin/merlin".into(),
        };
        let bin = Install {
            kind: Kind::Binary,
            path: "/x/merlin".into(),
        };
        assert_eq!(deb.asset_name("0.6.0").unwrap(), "merlin_0.6.0_amd64.deb");
        assert_eq!(
            bin.asset_name("0.6.0").unwrap(),
            "merlin-0.6.0-linux-amd64.tar.gz"
        );
    }
}
