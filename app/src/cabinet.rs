//! Cabinet (https://github.com/mark12870/cabinet), a Flatpak that installs Windows plugins into
//! Wine prefixes of its own, one per vendor, with the fixes that vendors' store apps and
//! installers need under Wine (Native Access and Kontakt, IK Product Manager, ...). This app has
//! none of those fixes, so installers from such vendors are sent there instead.
//!
//! Nothing else is needed to use what Cabinet installs: its bridges land in `~/.vst3/cabinet`,
//! `~/.clap/cabinet` and `~/.vst/cabinet`, which the plugin scan covers, and they run with
//! Cabinet's own Wine because the app keeps its Wine settings to its own bridges
//! ([`crate::session::Session::bridge_env`]).

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};

pub const APP_ID: &str = "io.github.mark12870.cabinet";
pub const HOMEPAGE: &str = "https://github.com/mark12870/cabinet";
const REMOTE: &str = "cabinet";
const REMOTE_URL: &str =
    "https://mark12870.github.io/cabinet/io.github.mark12870.cabinet.flatpakrepo";
const FLATHUB_URL: &str = "https://dl.flathub.org/repo/flathub.flatpakrepo";

/// A vendor whose plugins come through a store app that needs Cabinet's fixes.
struct Vendor {
    name: &'static str,
    /// Matched as whole words against the installer's publisher and product (its version
    /// resource) and its file name.
    words: &'static [&'static str],
    why: &'static str,
}

/// Every vendor whose store app Cabinet has a tested setup for.
const VENDORS: &[Vendor] = &[
    Vendor {
        name: "Native Instruments",
        // "X Setup PC.exe" is how NI names its product installers (Kontakt, Komplete Kontrol).
        words: &[
            "native instruments",
            "native access",
            "setup pc",
            "kontakt",
            "komplete",
        ],
        why: "Native Access needs several fixes to install and to sign in, and Kontakt's installer stalls under plain Wine.",
    },
    Vendor {
        name: "IK Multimedia",
        words: &["ik multimedia", "ik product manager"],
        why: "IK Product Manager has to be patched to run and to install products under Wine.",
    },
    Vendor {
        name: "iZotope",
        words: &["izotope"],
        why: "iZotope Product Portal keeps crashing and restarting under plain Wine, which can stall loaded plugins.",
    },
    Vendor {
        name: "Spitfire Audio",
        words: &["spitfire audio", "spitfireaudio"],
        why: "The Spitfire Audio app needs a Wine with more graphics support to show its window and sign in.",
    },
    Vendor {
        name: "Orchestral Tools",
        words: &["orchestral tools", "sine player"],
        why: "SINE Player needs Microsoft's WebView2 and fixes for it.",
    },
    Vendor {
        name: "Roland",
        words: &["roland"],
        why: "Roland Cloud Manager's installer cannot open its window under plain Wine.",
    },
    Vendor {
        name: "Arturia",
        words: &["arturia"],
        why: "Arturia's plugins come through Arturia Software Center, a store app that signs in and installs them.",
    },
    Vendor {
        name: "Waves",
        words: &["waves audio", "waves central"],
        why: "Waves' plugins come through Waves Central, a store app that signs in and installs them.",
    },
    Vendor {
        name: "UVI",
        words: &["uvi"],
        why: "UVI's instruments come through UVI Portal, a store app that signs in and installs them.",
    },
    Vendor {
        name: "Splice",
        words: &["splice"],
        why: "Splice's apps sign in through a web view that needs fixes under Wine.",
    },
    Vendor {
        name: "Minimal Audio",
        words: &["minimal audio", "minimal hub"],
        why: "Minimal Audio's plugins come through Minimal Hub, a store app that signs in and installs them.",
    },
    Vendor {
        name: "Klevgrand",
        words: &["klevgrand"],
        why: "Klevgrand's plugins come through Klevgrand Helper, a store app that signs in and installs them.",
    },
    Vendor {
        name: "Cymatics",
        words: &["cymatics"],
        why: "Cymatics' plugins come through Cymatics Hub, a store app that signs in and installs them.",
    },
    Vendor {
        name: "inMusic",
        words: &["inmusic"],
        why: "AIR and inMusic plugins come through inMusic Software Center, a store app that signs in and installs them.",
    },
];

/// Why an installer is better run in Cabinet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Advice {
    pub vendor: &'static str,
    pub why: &'static str,
}

/// Lowercase words separated by single spaces, with a space at each end, so that whole words
/// can be found with `contains(" word ")`.
fn words(text: &str) -> String {
    let mut out = String::from(" ");
    for part in text
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|p| !p.is_empty())
    {
        out.push_str(part);
        out.push(' ');
    }
    out
}

/// The vendor named in an installer's file name or version strings, if Cabinet is the better
/// place to install it.
fn advice_for(file_name: &str, strings: &[(String, String)]) -> Option<Advice> {
    let mut texts = vec![words(file_name)];
    texts.extend(
        strings
            .iter()
            .filter(|(k, _)| matches!(k.as_str(), "CompanyName" | "ProductName"))
            .map(|(_, v)| words(v)),
    );
    VENDORS
        .iter()
        .find(|v| {
            v.words.iter().any(|w| {
                let w = words(w);
                texts.iter().any(|t| t.contains(&w))
            })
        })
        .map(|v| Advice {
            vendor: v.name,
            why: v.why,
        })
}

/// [`advice_for`] the installer at `path`: its file name, and the publisher and product its
/// version resource names (which survive a renamed download).
pub fn advice(path: &Path) -> Option<Advice> {
    let name = path.file_name()?.to_string_lossy();
    advice_for(&name, &merlin_wine::version_info::version_strings(path))
}

/// Is Cabinet installed, for this user or for everyone?
pub fn installed() -> bool {
    let user = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| Path::new(&h).join(".local/share")));
    user.map(|d| d.join("flatpak"))
        .into_iter()
        .chain([PathBuf::from("/var/lib/flatpak")])
        .any(|root| root.join("app").join(APP_ID).join("current").exists())
}

/// Can this machine install Flatpaks at all?
pub fn flatpak_available() -> bool {
    std::env::var_os("PATH")
        .is_some_and(|path| std::env::split_paths(&path).any(|d| d.join("flatpak").is_file()))
}

/// Opens Cabinet's window. It runs on after this app quits.
pub fn open() -> Result<()> {
    use std::os::unix::process::CommandExt;
    let mut child = Command::new("flatpak")
        .args(["run", APP_ID])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .context("cannot start Cabinet (flatpak run)")?;
    std::thread::spawn(move || child.wait());
    Ok(())
}

/// The commands [`install`] runs, as the user would type them.
pub fn install_commands() -> [String; 2] {
    [
        format!("flatpak remote-add --user --if-not-exists {REMOTE} {REMOTE_URL}"),
        format!("flatpak install --user {REMOTE} {APP_ID}"),
    ]
}

/// Installs Cabinet for this user from its own Flatpak repository. Its runtime comes from
/// Flathub: when no Flathub this user can install from has it, a Flathub remote is added for
/// this user and the install tried once more. Takes minutes the first time (the runtime is
/// large); `say` hears what is happening.
pub fn install(say: &dyn Fn(String)) -> Result<()> {
    say("adding Cabinet's Flatpak repository...".into());
    flatpak(&[
        "remote-add",
        "--user",
        "--if-not-exists",
        REMOTE,
        REMOTE_URL,
    ])?;
    let install = || {
        flatpak(&[
            "install",
            "--user",
            "--noninteractive",
            "-y",
            REMOTE,
            APP_ID,
        ])
    };
    say("installing Cabinet (the first time this downloads its runtime too, which can take several minutes)...".into());
    if let Err(first) = install() {
        say(
            "Cabinet's runtime was not found; adding Flathub for this user and trying again..."
                .into(),
        );
        flatpak(&[
            "remote-add",
            "--user",
            "--if-not-exists",
            "flathub",
            FLATHUB_URL,
        ])
        .context(format!("{first:#}"))?;
        install()?;
    }
    if !installed() {
        bail!("flatpak finished, but Cabinet is not installed");
    }
    Ok(())
}

/// Runs `flatpak` with `args` to the end; its last lines of output are the error.
fn flatpak(args: &[&str]) -> Result<()> {
    let out = Command::new("flatpak")
        .args(args)
        .stdin(Stdio::null())
        .output()
        .context("cannot run flatpak")?;
    if out.status.success() {
        return Ok(());
    }
    let text = String::from_utf8_lossy(&out.stderr);
    let tail: Vec<&str> = text.lines().rev().take(3).collect();
    let tail: Vec<&str> = tail.into_iter().rev().collect();
    bail!("flatpak {} failed: {}", args[0], tail.join(" "))
}

/// Is `path` one of Cabinet's bridges (`~/.vst3/cabinet/...` and the same for CLAP and VST2)?
pub fn is_bridge(path: &Path) -> bool {
    let Some(home) = std::env::var_os("HOME") else {
        return false;
    };
    [".vst3", ".clap", ".vst"]
        .iter()
        .any(|d| path.starts_with(Path::new(&home).join(d).join("cabinet")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    #[test]
    fn store_apps_and_their_products_are_sent_to_cabinet() {
        // What Native Access's own installer says about itself (version 3.26, checked).
        let na = strings(&[
            ("CompanyName", "Native Instruments"),
            ("ProductName", "Native Access"),
        ]);
        assert_eq!(
            advice_for("Native-Access_2.exe", &na).map(|a| a.vendor),
            Some("Native Instruments")
        );
        // Renamed: the version resource still says who made it.
        assert!(advice_for("download (1).exe", &na).is_some());
        for name in [
            "Kontakt 8 8.13.1 Setup PC.exe",
            "Arturia_Software_Center__2_13_0_3440.exe",
            "iZotope_Product_Portal_v1_4_9.exe",
            "Install_Waves_Central.exe",
            "SINE_Player_1.4.4.1671.exe",
            "Roland-Cloud-Manager-Installer-3-1-25.exe",
            "UVI_Portal.exe",
            "Minimal Hub.exe",
            "ik_product_manager_1.1.16.exe",
        ] {
            assert!(advice_for(name, &[]).is_some(), "{name}");
        }
    }

    #[test]
    fn plain_installers_stay_here() {
        // CollaB3 (Inno Setup) and Surge XT, as their files describe themselves.
        let colla = strings(&[
            ("Comments", "This installation was built with Inno Setup."),
            ("ProductName", "CollaB3-V2-FREE"),
        ]);
        assert_eq!(
            advice_for("CollaB3-V2.5.2-FREE-Win-Installer.exe", &colla),
            None
        );
        let surge = strings(&[
            ("CompanyName", "Surge Synth Team"),
            ("ProductName", "Surge XT"),
        ]);
        assert_eq!(advice_for("surge-xt-win64-1.3.4-setup.exe", &surge), None);
        // Whole words only: "uvi" inside another word is not UVI, "roland" is not "orlando".
        for name in ["fluvio-setup.exe", "Orlando Synth.exe", "Wavestation.exe"] {
            assert_eq!(advice_for(name, &[]), None, "{name}");
        }
        // Only the publisher and product count, not a description that mentions a vendor.
        let mentions = strings(&[("FileDescription", "Works with Native Instruments Kontakt")]);
        assert_eq!(advice_for("library.exe", &mentions), None);
    }

    #[test]
    fn cabinet_bridges_are_told_apart() {
        let home = std::env::var("HOME").unwrap();
        assert!(is_bridge(
            &Path::new(&home).join(".vst3/cabinet/windows/Kontakt 8.vst3")
        ));
        assert!(!is_bridge(
            &Path::new(&home).join(".vst3/yabridge/Kontakt 8.vst3")
        ));
    }
}
