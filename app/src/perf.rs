//! The performance check (plan P2): is this machine set up for glitch-free live playing, and if
//! not, what exactly should the user change? The judgement is a pure function of the measured
//! facts, so it is unit tested; `gather` reads the facts from the system.

use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Ok,
    Warn,
    Fail,
}

#[derive(Clone, Debug)]
pub struct Check {
    pub name: &'static str,
    pub status: Status,
    pub detail: String,
    /// What to do about it (shown only when the status is not `Ok`).
    pub fix: Option<String>,
}

/// Everything the checks look at.
#[derive(Clone, Debug, Default)]
pub struct PerfInputs {
    /// `RLIMIT_RTPRIO` soft limit; `None` if unknown.
    pub rtprio_limit: Option<u64>,
    /// `RLIMIT_MEMLOCK` soft limit in bytes (`u64::MAX` = unlimited).
    pub memlock_limit: Option<u64>,
    pub groups: Vec<String>,
    pub rtkit_available: bool,
    /// Is the audio callback thread scheduled realtime (SCHED_FIFO/RR)? `None` if not found.
    pub audio_thread_realtime: Option<bool>,
    pub cpu_governor: Option<String>,
    pub buffer_frames: u32,
    pub sample_rate: u32,
    pub xruns: u32,
    pub dsp_load_percent: f32,
}

const MIB: u64 = 1024 * 1024;

fn join_group_advice(groups: &[String]) -> String {
    let group = ["pipewire", "audio", "realtime"]
        .into_iter()
        .find(|g| groups.iter().any(|x| x == g));
    match group {
        // In a group that normally grants realtime but the limit is still low: the login
        // session predates the group change.
        Some(g) => format!(
            "You are in the '{g}' group; log out and in again so its realtime limits apply."
        ),
        None => {
            "Run: sudo usermod -aG pipewire $USER (or install rtkit), then log out and in again."
                .into()
        }
    }
}

pub fn evaluate(p: &PerfInputs) -> Vec<Check> {
    let mut checks = Vec::new();

    // Realtime scheduling: the single most important setting for dropout-free audio.
    let rt_limit = p.rtprio_limit.unwrap_or(0);
    let (status, detail) = match (p.audio_thread_realtime, rt_limit, p.rtkit_available) {
        (Some(true), ..) => (
            Status::Ok,
            "The audio thread runs with realtime priority.".to_string(),
        ),
        (_, limit, _) if limit >= 70 => (
            Status::Ok,
            format!("Realtime priority is allowed (limit {limit})."),
        ),
        (_, _, true) => (
            Status::Ok,
            "rtkit can grant realtime priority on request.".to_string(),
        ),
        _ => (
            Status::Fail,
            "The audio thread is not realtime: any busy moment on the machine can cause a dropout."
                .to_string(),
        ),
    };
    let fix = (status != Status::Ok).then(|| join_group_advice(&p.groups));
    checks.push(Check {
        name: "Realtime scheduling",
        status,
        detail,
        fix,
    });

    // Locked memory.
    let (status, detail, fix) = match p.memlock_limit {
        Some(u64::MAX) => (Status::Ok, "Locked memory is unlimited.".to_string(), None),
        Some(limit) if limit >= 512 * MIB => (
            Status::Ok,
            format!("Locked memory limit is {} MiB.", limit / MIB),
            None,
        ),
        Some(limit) => (
            Status::Warn,
            format!(
                "Locked memory is limited to {} MiB, so the app does not lock its memory and the audio path can page-fault under memory pressure.",
                limit / MIB
            ),
            Some("Set 'memlock unlimited' for your audio group in /etc/security/limits.d/.".into()),
        ),
        None => (
            Status::Warn,
            "Could not read the locked memory limit.".into(),
            None,
        ),
    };
    checks.push(Check {
        name: "Locked memory",
        status,
        detail,
        fix,
    });

    // Buffer size and the latency it implies.
    let ms = if p.sample_rate > 0 {
        p.buffer_frames as f32 * 1000.0 / p.sample_rate as f32
    } else {
        0.0
    };
    let (status, fix) = match ms {
        m if m <= 6.0 => (Status::Ok, None),
        m if m <= 12.0 => (
            Status::Warn,
            Some("Noticeable when playing; 128 or 256 frames is the usual choice on a fast machine.".to_string()),
        ),
        _ => (
            Status::Fail,
            Some("Choose a smaller buffer in Settings > Audio (128 frames is about 2.7 ms at 48 kHz).".to_string()),
        ),
    };
    checks.push(Check {
        name: "Buffer size",
        status,
        detail: format!(
            "{} frames at {} Hz = {ms:.1} ms one way.",
            p.buffer_frames, p.sample_rate
        ),
        fix,
    });

    // Dropouts so far.
    checks.push(Check {
        name: "Dropouts (xruns)",
        status: if p.xruns == 0 { Status::Ok } else { Status::Warn },
        detail: format!("{} since the app started.", p.xruns),
        fix: (p.xruns > 0).then(|| {
            "Dropouts mean the audio thread missed its deadline: use a bigger buffer, fix realtime scheduling, or unload heavy plugins.".to_string()
        }),
    });

    // Load.
    let (status, fix) = match p.dsp_load_percent {
        l if l < 50.0 => (Status::Ok, None),
        l if l < 80.0 => (
            Status::Warn,
            Some("Close to the limit: heavy passages may drop out.".to_string()),
        ),
        _ => (
            Status::Fail,
            Some("Too busy: unload plugins or use a bigger buffer.".to_string()),
        ),
    };
    checks.push(Check {
        name: "Audio load",
        status,
        detail: format!("{:.0}% of the buffer time is used.", p.dsp_load_percent),
        fix,
    });

    // CPU frequency scaling.
    match p.cpu_governor.as_deref() {
        Some("powersave") => checks.push(Check {
            name: "CPU frequency",
            status: Status::Warn,
            detail: "The CPU governor is 'powersave': clock speed ramps up late and can cause dropouts.".into(),
            fix: Some("Switch to 'performance' (for example with cpupower or your desktop's power profile).".into()),
        }),
        Some(g) => checks.push(Check {
            name: "CPU frequency",
            status: Status::Ok,
            detail: format!("CPU governor: {g}."),
            fix: None,
        }),
        None => {}
    }
    checks
}

/// The worst status among the checks.
pub fn overall(checks: &[Check]) -> Status {
    checks
        .iter()
        .map(|c| c.status)
        .fold(Status::Ok, |worst, s| match (worst, s) {
            (Status::Fail, _) | (_, Status::Fail) => Status::Fail,
            (Status::Warn, _) | (_, Status::Warn) => Status::Warn,
            _ => Status::Ok,
        })
}

// ---- reading the system -------------------------------------------------------------------

fn rlimit(resource: libc::__rlimit_resource_t) -> Option<u64> {
    let mut l = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: plain syscall writing into our struct.
    if unsafe { libc::getrlimit(resource, &mut l) } != 0 {
        return None;
    }
    Some(if l.rlim_cur == libc::RLIM_INFINITY {
        u64::MAX
    } else {
        l.rlim_cur
    })
}

/// Scheduling policies of this process' threads, by thread name.
fn thread_policies() -> Vec<(String, u32)> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir("/proc/self/task") else {
        return out;
    };
    for e in rd.flatten() {
        let Ok(stat) = std::fs::read_to_string(e.path().join("stat")) else {
            continue;
        };
        let (Some(open), Some(close)) = (stat.find('('), stat.rfind(')')) else {
            continue;
        };
        let fields: Vec<&str> = stat[close + 2..].split(' ').collect();
        // After the comm: field 41 overall is the policy, index 38 here.
        if let Some(policy) = fields.get(38).and_then(|p| p.parse().ok()) {
            out.push((stat[open + 1..close].to_string(), policy));
        }
    }
    out
}

/// Is the JACK/PipeWire callback thread of this process realtime? (PipeWire's client thread is
/// called `data-loop.0`.)
fn audio_thread_realtime() -> Option<bool> {
    let threads = thread_policies();
    let audio: Vec<&(String, u32)> = threads
        .iter()
        .filter(|(name, _)| {
            name.starts_with("data-loop") || name.contains("jack") || name == "audio"
        })
        .collect();
    (!audio.is_empty()).then(|| audio.iter().any(|(_, policy)| *policy == 1 || *policy == 2))
}

fn groups() -> Vec<String> {
    std::process::Command::new("id")
        .arg("-nG")
        .output()
        .ok()
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .split_whitespace()
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn rtkit_available() -> bool {
    [
        "/usr/lib/rtkit/rtkit-daemon",
        "/usr/libexec/rtkit-daemon",
        "/usr/lib/rtkit-daemon",
    ]
    .iter()
    .any(|p| Path::new(p).exists())
        && std::process::Command::new("busctl")
            .args(["--system", "status", "org.freedesktop.RealtimeKit1"])
            .output()
            .is_ok_and(|o| o.status.success())
}

fn cpu_governor() -> Option<String> {
    std::fs::read_to_string("/sys/devices/system/cpu/cpu0/cpufreq/scaling_governor")
        .ok()
        .map(|s| s.trim().to_owned())
}

/// Reads what the checks need from the system; the audio numbers come from the caller.
pub fn gather(
    buffer_frames: u32,
    sample_rate: u32,
    xruns: u32,
    dsp_load_percent: f32,
) -> PerfInputs {
    PerfInputs {
        rtprio_limit: rlimit(libc::RLIMIT_RTPRIO),
        memlock_limit: rlimit(libc::RLIMIT_MEMLOCK),
        groups: groups(),
        rtkit_available: rtkit_available(),
        audio_thread_realtime: audio_thread_realtime(),
        cpu_governor: cpu_governor(),
        buffer_frames,
        sample_rate,
        xruns,
        dsp_load_percent,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn good() -> PerfInputs {
        PerfInputs {
            rtprio_limit: Some(95),
            memlock_limit: Some(u64::MAX),
            groups: vec!["audio".into()],
            rtkit_available: false,
            audio_thread_realtime: Some(true),
            cpu_governor: Some("performance".into()),
            buffer_frames: 128,
            sample_rate: 48000,
            xruns: 0,
            dsp_load_percent: 8.0,
        }
    }

    fn find<'a>(checks: &'a [Check], name: &str) -> &'a Check {
        checks.iter().find(|c| c.name == name).unwrap()
    }

    #[test]
    fn a_well_set_up_machine_passes_everything() {
        let checks = evaluate(&good());
        assert!(checks.iter().all(|c| c.status == Status::Ok), "{checks:?}");
        assert!(checks.iter().all(|c| c.fix.is_none()));
        assert_eq!(overall(&checks), Status::Ok);
    }

    #[test]
    fn this_machines_situation_is_diagnosed_with_the_exact_fix() {
        // The dev machine in spike S1: no realtime limit, not in the pipewire group, no rtkit.
        let p = PerfInputs {
            rtprio_limit: Some(0),
            memlock_limit: Some(8 * MIB),
            groups: vec!["hawa".into(), "wheel".into()],
            audio_thread_realtime: Some(false),
            ..good()
        };
        let checks = evaluate(&p);
        let rt = find(&checks, "Realtime scheduling");
        assert_eq!(rt.status, Status::Fail);
        assert!(rt.fix.as_deref().unwrap().contains("usermod -aG pipewire"));
        let mem = find(&checks, "Locked memory");
        assert_eq!(mem.status, Status::Warn);
        assert!(mem.detail.contains("8 MiB"));
        assert_eq!(overall(&checks), Status::Fail);
    }

    #[test]
    fn already_in_the_group_means_log_in_again() {
        let p = PerfInputs {
            rtprio_limit: Some(0),
            audio_thread_realtime: Some(false),
            groups: vec!["pipewire".into()],
            ..good()
        };
        let rt = evaluate(&p)
            .into_iter()
            .find(|c| c.name == "Realtime scheduling")
            .unwrap();
        assert!(rt.fix.unwrap().contains("log out and in again"));
    }

    #[test]
    fn rtkit_counts_as_a_way_to_get_realtime() {
        let p = PerfInputs {
            rtprio_limit: Some(0),
            audio_thread_realtime: None,
            rtkit_available: true,
            ..good()
        };
        assert_eq!(
            find(&evaluate(&p), "Realtime scheduling").status,
            Status::Ok
        );
    }

    #[test]
    fn buffer_load_and_dropouts_are_graded() {
        let grade = |f: &dyn Fn(&mut PerfInputs), name: &str| {
            let mut p = good();
            f(&mut p);
            find(&evaluate(&p), name).status
        };
        assert_eq!(grade(&|p| p.buffer_frames = 256, "Buffer size"), Status::Ok);
        assert_eq!(
            grade(&|p| p.buffer_frames = 512, "Buffer size"),
            Status::Warn
        );
        assert_eq!(
            grade(&|p| p.buffer_frames = 2048, "Buffer size"),
            Status::Fail
        );
        assert_eq!(
            grade(&|p| p.dsp_load_percent = 65.0, "Audio load"),
            Status::Warn
        );
        assert_eq!(
            grade(&|p| p.dsp_load_percent = 95.0, "Audio load"),
            Status::Fail
        );
        assert_eq!(grade(&|p| p.xruns = 3, "Dropouts (xruns)"), Status::Warn);
        assert_eq!(
            grade(
                &|p| p.cpu_governor = Some("powersave".into()),
                "CPU frequency"
            ),
            Status::Warn
        );
    }

    #[test]
    fn the_real_system_can_be_read() {
        let p = gather(128, 48000, 0, 1.0);
        assert!(p.rtprio_limit.is_some() && p.memlock_limit.is_some());
        // Whatever the machine looks like, evaluation produces a verdict for every check.
        assert!(evaluate(&p).len() >= 5);
    }
}
