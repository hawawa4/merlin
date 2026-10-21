//! Scanning a plugin file in a child process, with a timeout (plan §4.7).
//!
//! Loading an arbitrary plugin binary can crash, hang or take seconds (Windows plugins start
//! Wine). None of that may touch the app, so the app runs a separate scanner executable per
//! file: it loads the file, prints the descriptors as one tagged JSON line and exits. A crash,
//! a hang past the timeout, or garbage on stdout becomes an error for that file.

use std::io::Read;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow, bail};
use merlin_hosting::PluginInfo;

/// Marks the result line. Plugins and Wine print plenty of their own output on stdout.
pub const RESULT_PREFIX: &str = "MERLIN-SCAN-RESULT ";

/// Finds the result in a child's stdout: the last [`RESULT_PREFIX`] and the rest of its line. The
/// marker may follow other text on its line (a plugin that printed without a final newline).
pub fn parse_child_output(stdout: &str) -> Result<Vec<PluginInfo>> {
    let at = stdout
        .rfind(RESULT_PREFIX)
        .ok_or_else(|| anyhow!("the scanner printed no result"))?;
    let rest = &stdout[at + RESULT_PREFIX.len()..];
    let line = rest.lines().next().unwrap_or_default();
    Ok(serde_json::from_str(line)?)
}

/// Runs `command` (a scanner invocation) and waits at most `timeout` for it. The child gets its
/// own process group, which is killed as a whole on timeout so that Wine processes it started
/// do not linger.
pub fn run_isolated(mut command: Command, timeout: Duration) -> Result<Vec<PluginInfo>> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .process_group(0);
    let mut child = command.spawn()?;
    let mut stdout = child.stdout.take().expect("piped stdout");
    // Drain stdout on a thread so a chatty plugin cannot fill the pipe and block.
    let reader = std::thread::spawn(move || {
        let mut text = String::new();
        let mut bytes = Vec::new();
        let _ = stdout.read_to_end(&mut bytes);
        text.push_str(&String::from_utf8_lossy(&bytes));
        text
    });

    let start = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if start.elapsed() >= timeout {
            kill_group(child.id());
            let _ = child.wait();
            let _ = reader.join();
            bail!("timed out after {}s", timeout.as_secs());
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let output = reader.join().unwrap_or_default();
    // Whatever else the child started (a Wine host) goes with it.
    kill_group(child.id());
    if let Some(signal) = status.signal() {
        bail!("crashed (signal {signal})");
    }
    if !status.success() {
        bail!("failed (exit status {})", status.code().unwrap_or(-1));
    }
    parse_child_output(&output)
}

fn kill_group(pid: u32) {
    // SAFETY: signalling the process group we created for the child.
    unsafe { libc::kill(-(pid as i32), libc::SIGKILL) };
}

#[cfg(test)]
mod tests {
    use super::*;
    use merlin_model::Format;

    fn sh(script: &str) -> Command {
        let mut c = Command::new("sh");
        c.arg("-c").arg(script);
        c
    }

    fn result_line() -> String {
        let info = PluginInfo {
            format: Format::Clap,
            id: "x".into(),
            name: "X".into(),
            vendor: "v".into(),
            version: "1".into(),
            features: vec!["instrument".into()],
        };
        format!(
            "{RESULT_PREFIX}{}",
            serde_json::to_string(&vec![info]).unwrap()
        )
    }

    #[test]
    fn a_good_scan_returns_the_plugins_even_with_noise_around_it() {
        let script = format!(
            "echo noise from a plugin; echo '{}'; echo more noise",
            result_line()
        );
        let plugins = run_isolated(sh(&script), Duration::from_secs(5)).unwrap();
        assert_eq!(plugins.len(), 1);
        assert_eq!(plugins[0].id, "x");
    }

    #[test]
    fn a_crash_is_an_error_for_that_file_only() {
        let err = run_isolated(sh("kill -SEGV $$"), Duration::from_secs(5)).unwrap_err();
        assert!(err.to_string().contains("crashed (signal 11)"), "{err}");
        let err = run_isolated(sh("exit 3"), Duration::from_secs(5)).unwrap_err();
        assert!(err.to_string().contains("exit status 3"), "{err}");
    }

    #[test]
    fn a_hang_is_cut_off_at_the_timeout_and_its_children_with_it() {
        let marker = std::env::temp_dir().join(format!("instr-hang-{}", std::process::id()));
        let _ = std::fs::remove_file(&marker);
        // A grandchild that would write the marker after 3 s.
        let script = format!("(sleep 3; touch '{}') & sleep 30", marker.display());
        let t = Instant::now();
        let err = run_isolated(sh(&script), Duration::from_millis(400)).unwrap_err();
        assert!(err.to_string().contains("timed out"), "{err}");
        assert!(
            t.elapsed() < Duration::from_secs(2),
            "returned promptly: {:?}",
            t.elapsed()
        );
        std::thread::sleep(Duration::from_millis(3500));
        assert!(!marker.exists(), "the whole process group was killed");
    }

    #[test]
    fn garbage_and_missing_results_are_errors() {
        assert!(run_isolated(sh("echo hello"), Duration::from_secs(5)).is_err());
        let err = run_isolated(
            sh(&format!("echo '{RESULT_PREFIX}not json'")),
            Duration::from_secs(5),
        );
        assert!(err.is_err());
        assert!(
            run_isolated(Command::new("/nonexistent/scanner"), Duration::from_secs(1)).is_err()
        );
    }

    #[test]
    fn a_chatty_child_cannot_block_on_a_full_pipe() {
        let script = format!(
            "yes plugin-noise | head -c 5000000; echo '{}'",
            result_line()
        );
        assert_eq!(
            run_isolated(sh(&script), Duration::from_secs(10))
                .unwrap()
                .len(),
            1
        );
    }
}
