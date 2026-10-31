//! What the desktop's audio system says about where sound goes. JACK has no notion of a default
//! output, so this asks WirePlumber (`wpctl`), the place the desktop's volume control reads it
//! from. Without `wpctl` there is no answer, and the callers fall back to the first outputs the
//! audio server lists.

use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

/// How long to wait for an answer before doing without one.
const ANSWER_WITHIN: Duration = Duration::from_secs(2);

/// The default output's description (`Ryzen HD Audio Controller Analog Stereo`), which is what
/// PipeWire names the device's JACK ports after. `None` when `wpctl` is missing or does not
/// answer: the caller does without rather than being held up.
pub fn default_output() -> Option<String> {
    let (tx, rx) = mpsc::channel();
    thread::Builder::new()
        .name("default-output".into())
        .spawn(move || {
            let answer = Command::new("wpctl")
                .args(["inspect", "@DEFAULT_AUDIO_SINK@"])
                .stdin(Stdio::null())
                .stderr(Stdio::null())
                .output()
                .ok()
                .filter(|out| out.status.success())
                .and_then(|out| parse_description(&String::from_utf8_lossy(&out.stdout)));
            let _ = tx.send(answer);
        })
        .ok()?;
    rx.recv_timeout(ANSWER_WITHIN).ok().flatten()
}

/// `node.description` out of `wpctl inspect`, whose lines look like
/// `  * node.description = "Ryzen HD Audio Controller Analog Stereo"` (the star marks a
/// property set by the user rather than the device).
fn parse_description(text: &str) -> Option<String> {
    text.lines().find_map(|line| {
        let rest = line
            .trim_start()
            .trim_start_matches('*')
            .trim_start()
            .strip_prefix("node.description")?;
        let value = rest.trim_start().strip_prefix('=')?.trim();
        value
            .strip_prefix('"')?
            .strip_suffix('"')
            .filter(|v| !v.is_empty())
            .map(str::to_owned)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_description_is_read_from_wpctl_inspect() {
        let real = "id 35, type PipeWire:Interface:Node\n\
                    \x20   alsa.card = \"2\"\n\
                    \x20   device.description = \"Wrong One\"\n\
                    \x20 * node.description = \"Ryzen HD Audio Controller Analog Stereo\"\n\
                    \x20   node.name = \"alsa_output.pci-0000_13_00.6.analog-stereo\"\n\
                    \x20   node.nick = \"ALC897 Analog\"\n";
        assert_eq!(
            parse_description(real).as_deref(),
            Some("Ryzen HD Audio Controller Analog Stereo")
        );
        // Without the star, and with a bracket and a slash in the name.
        assert_eq!(
            parse_description(
                "    node.description = \"Navi 48 HDMI/DP Audio (HDMI 2) [24G2W1G4]\""
            )
            .as_deref(),
            Some("Navi 48 HDMI/DP Audio (HDMI 2) [24G2W1G4]")
        );
        assert_eq!(parse_description("node.name = \"x\""), None);
        assert_eq!(parse_description("node.description = \"\""), None);
        assert_eq!(parse_description(""), None);
    }
}
