//! Updates (Settings → Updates): looking for a newer release on GitHub and installing it. It
//! only runs when the user asks; nothing checks at startup or in the background, and nothing
//! pops up. The result is shown in the Settings tab.

use std::sync::mpsc;

use anyhow::{Result, anyhow, bail};
use merlin_updater::{self as updater, Kind, Progress};

use super::{Session, UserEvent};

/// The running version. Release builds get it from the workflow (`MERLIN_RELEASE_VERSION`);
/// anything else (`cargo run`, local builds) is `dev` and is never updated.
pub const CURRENT: &str = match option_env!("MERLIN_RELEASE_VERSION") {
    Some(v) => v,
    None => "dev",
};

/// What a check found.
#[derive(Clone, Debug)]
pub struct UpdateInfo {
    pub latest: String,
    pub available: bool,
    pub notes: String,
    pub page_url: String,
    pub kind: Kind,
    /// Whether the app can do the update itself; `reason` says why not when it can't (the
    /// release page is then the way to update).
    pub can_install: bool,
    pub reason: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Busy {
    #[default]
    No,
    Checking,
    Installing,
}

/// Messages from the worker thread.
pub(super) enum UpdateMsg {
    Status(String),
    Checked(Result<UpdateInfo, String>),
    Installed(Result<(), String>),
}

/// What the Updates tab shows.
#[derive(Default)]
pub struct UpdateState {
    pub busy: Busy,
    pub info: Option<UpdateInfo>,
    /// How the last check or install failed.
    pub error: String,
    /// What the install is doing now.
    pub status: String,
    /// The new version is on disk; a restart switches to it.
    pub installed: bool,
    rx: Option<mpsc::Receiver<UpdateMsg>>,
}

impl Session {
    /// Asks GitHub for the newest release, on a worker thread.
    pub fn check_for_update(&mut self) -> Result<()> {
        self.start_update_job(Busy::Checking, |_| {
            UpdateMsg::Checked(check().map_err(|e| format!("{e:#}")))
        })
    }

    /// Downloads and installs the newest release, on a worker thread. The running app keeps
    /// going; [`Session::restart_for_update`] switches to the new version. For a `.deb` this
    /// raises the system's password prompt.
    pub fn install_update(&mut self) -> Result<()> {
        if !updater::is_release(CURRENT) {
            bail!("development builds can't be updated");
        }
        self.start_update_job(Busy::Installing, |tx| {
            let report = |p: Progress| {
                let mb = |b: u64| b / (1 << 20);
                let _ = tx.send(UpdateMsg::Status(match p {
                    Progress::Downloading { done, total } if total > 0 => {
                        format!("downloading: {} of {} MB", mb(done), mb(total))
                    }
                    Progress::Downloading { done, .. } => format!("downloading: {} MB", mb(done)),
                    Progress::Installing => "installing...".into(),
                }));
            };
            let result = (|| {
                // Fetched again rather than trusted from the check: a newer release may have
                // been published since.
                let rel = updater::latest()?;
                if !updater::newer(&rel.version, CURRENT) {
                    bail!("already running the latest version");
                }
                updater::install(&rel, &updater::detect(), &report)
            })();
            UpdateMsg::Installed(result.map_err(|e| format!("{e:#}")))
        })
    }

    /// Restarts into the installed update (main does it on its next tick, from the autosave).
    pub fn restart_for_update(&mut self) -> Result<()> {
        if !self.update.installed {
            bail!("no update has been installed");
        }
        self.restart_requested = true;
        Ok(())
    }

    fn start_update_job(
        &mut self,
        busy: Busy,
        job: impl FnOnce(&mpsc::Sender<UpdateMsg>) -> UpdateMsg + Send + 'static,
    ) -> Result<()> {
        if self.update.busy != Busy::No {
            return Err(anyhow!("an update check or install is already running"));
        }
        let (tx, rx) = mpsc::channel();
        let proxy = self.proxy.clone();
        std::thread::Builder::new()
            .name("update".into())
            .spawn(move || {
                let _ = tx.send(job(&tx));
                let _ = proxy.send_event(UserEvent::Update);
            })?;
        self.update.busy = busy;
        self.update.error.clear();
        self.update.status = match busy {
            Busy::Installing => "starting...".into(),
            _ => String::new(),
        };
        self.update.rx = Some(rx);
        Ok(())
    }

    /// Takes in what the update worker reported.
    pub fn poll_update(&mut self) {
        let Some(rx) = self.update.rx.as_ref() else {
            return;
        };
        let mut finished = None;
        while let Ok(msg) = rx.try_recv() {
            match msg {
                UpdateMsg::Status(t) => self.update.status = t,
                done => finished = Some(done),
            }
        }
        let Some(done) = finished else { return };
        self.update.rx = None;
        self.update.busy = Busy::No;
        match done {
            UpdateMsg::Checked(Ok(info)) => self.update.info = Some(info),
            UpdateMsg::Checked(Err(e)) => {
                self.update.info = None;
                self.update.error = e;
            }
            UpdateMsg::Installed(Ok(())) => {
                self.update.installed = true;
                self.update.status.clear();
            }
            UpdateMsg::Installed(Err(e)) => self.update.error = e,
            UpdateMsg::Status(_) => unreachable!("handled above"),
        }
    }
}

/// Asks GitHub and works out whether and how this copy can update.
fn check() -> Result<UpdateInfo> {
    let rel = updater::latest()?;
    let inst = updater::detect();
    let (can_install, reason) = if !updater::is_release(CURRENT) {
        (false, "This is a development build.".to_owned())
    } else if inst.kind == Kind::Unsupported {
        (
            false,
            "In-app updates aren't available on this platform.".to_owned(),
        )
    } else if inst.kind == Kind::Deb
        && let Some(why) = updater::deb_prerequisites()
    {
        (false, format!("Installing the package needs {why}."))
    } else {
        (true, String::new())
    };
    Ok(UpdateInfo {
        available: updater::newer(&rel.version, CURRENT),
        latest: rel.version,
        notes: rel.notes,
        page_url: rel.page_url,
        kind: inst.kind,
        can_install,
        reason,
    })
}
