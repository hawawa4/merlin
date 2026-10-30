//! `curl`, as the Windows plugin runtime download uses it, so no HTTP stack has to be linked.

use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use sha2::{Digest, Sha256};

fn curl(url: &str) -> Command {
    let mut c = Command::new("curl");
    c.args([
        "--fail",
        "--location",
        "--silent",
        "--show-error",
        "--retry",
        "3",
        "--user-agent",
        "merlin-updater",
    ])
    .arg(url)
    .stdin(Stdio::null());
    c
}

fn curl_missing(e: &std::io::Error) -> anyhow::Error {
    if e.kind() == std::io::ErrorKind::NotFound {
        anyhow!("`curl` is needed to check for updates and was not found")
    } else {
        anyhow!("cannot run curl: {e}")
    }
}

/// Fetches a small text file (the release list, the checksums).
pub fn fetch_text(url: &str) -> Result<String> {
    let out = curl(url)
        .args([
            "--max-time",
            "30",
            "--header",
            "Accept: application/vnd.github+json",
        ])
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| curl_missing(&e))?;
    if !out.status.success() {
        bail!(
            "cannot fetch {url}: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    String::from_utf8(out.stdout).context("the answer is not text")
}

/// Downloads `url` to `dest`, calling `progress(bytes so far)` while it runs. `dest` is removed
/// again if the download fails.
pub fn download(url: &str, dest: &Path, progress: &dyn Fn(u64)) -> Result<()> {
    let mut child = curl(url)
        .arg("--output")
        .arg(dest)
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| curl_missing(&e))?;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        progress(std::fs::metadata(dest).map_or(0, |m| m.len()));
        std::thread::sleep(Duration::from_millis(200));
    };
    if !status.success() {
        let mut err = String::new();
        if let Some(mut e) = child.stderr.take() {
            let _ = e.read_to_string(&mut err);
        }
        let _ = std::fs::remove_file(dest);
        bail!("download failed: {}", err.trim());
    }
    progress(std::fs::metadata(dest).map_or(0, |m| m.len()));
    Ok(())
}

pub fn sha256_file(path: &Path) -> Result<String> {
    let mut f = std::fs::File::open(path)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}
