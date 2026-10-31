//! Where the main mix is heard: which interface's outputs it is linked to, and what the audio
//! settings say about it. By default it goes to the system's default output, the one every other
//! program on the desktop uses, as it was when Merlin started; a choice is kept by port name and
//! followed when that interface comes back after being unplugged.

use merlin_audio_io::{OutputPair, device_label, devices_of, output_pairs};

use super::Session;
use crate::system_audio;

/// A saved pair of ports, the way the backend wants them.
pub(super) fn as_strs(ports: &Option<[String; 2]>) -> Option<[&str; 2]> {
    ports.as_ref().map(|[l, r]| [l.as_str(), r.as_str()])
}

/// Where the main mix should go: the interface the user chose if it is there, else the system's
/// default output, else `None` (the backend then takes the first outputs the server lists, which
/// is also what happens when the default is a device that has no ports to link to).
pub(super) fn target(
    pairs: &[OutputPair],
    chosen: &Option<[String; 2]>,
    default: Option<&str>,
) -> Option<[String; 2]> {
    chosen
        .as_ref()
        .filter(|c| pairs.iter().any(|p| &p.ports == *c))
        .cloned()
        .or_else(|| {
            let default = default?;
            pairs
                .iter()
                .find(|p| device_label(&p.ports[0]) == default)
                .map(|p| p.ports.clone())
        })
}

impl Session {
    /// Chooses the interface the main mix goes to (`None`: the system's default output).
    pub fn set_main_output(&mut self, ports: Option<[String; 2]>) {
        // Choosing Automatic looks at the system default again, so it is not a stale one.
        if ports.is_none()
            && let Some(default) = system_audio::default_output()
        {
            self.default_output = Some(default);
        }
        self.settings.main_output = ports;
        self.apply_main_output();
        self.save_settings();
        let devices = self.main_output_devices();
        self.say(format!(
            "main output: {}",
            if devices.is_empty() {
                "none".to_owned()
            } else {
                devices.join(", ")
            }
        ));
    }

    /// Links the main outputs as the settings and the system default say. Returns whether that
    /// moved them.
    pub(super) fn apply_main_output(&mut self) -> bool {
        let Some(backend) = self.backend.as_ref() else {
            return false;
        };
        let before = backend.output_links();
        let wanted = target(
            &output_pairs(&backend.playback_ports()),
            &self.settings.main_output,
            self.default_output.as_deref(),
        );
        backend.connect_outputs(as_strs(&wanted));
        backend.output_links() != before
    }

    /// The system's default output as the desktop names it, if it could be found out.
    pub fn system_default_output(&self) -> Option<&str> {
        self.default_output.as_deref()
    }

    /// The devices the main outputs are heard on now.
    pub fn main_output_devices(&self) -> Vec<String> {
        self.backend
            .as_ref()
            .map(|b| devices_of(&b.output_links()))
            .unwrap_or_default()
    }

    /// The interface's outputs, in pairs, for choosing where the main mix and the click go.
    pub fn output_pairs(&self) -> Vec<OutputPair> {
        self.backend
            .as_ref()
            .map(|b| output_pairs(&b.playback_ports()))
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pair(device: &str) -> OutputPair {
        OutputPair {
            ports: [
                format!("{device}:playback_FL"),
                format!("{device}:playback_FR"),
            ],
            label: device.to_owned(),
        }
    }

    #[test]
    fn the_mix_goes_where_the_user_chose_else_where_the_desktop_does() {
        let pairs = [pair("Onboard"), pair("Monitor")];
        let monitor = Some(pairs[1].ports.clone());
        // The system default, when nothing was chosen: not the first one listed.
        assert_eq!(target(&pairs, &None, Some("Monitor")), monitor);
        // A choice wins over the default.
        assert_eq!(
            target(&pairs, &Some(pairs[0].ports.clone()), Some("Monitor")),
            Some(pairs[0].ports.clone())
        );
        // A choice that is unplugged: the default.
        let gone = Some(pair("Gone USB").ports);
        assert_eq!(target(&pairs, &gone, Some("Monitor")), monitor);
        // Nothing known, or a default with no ports here: leave it to the backend's fallback.
        assert_eq!(target(&pairs, &None, None), None);
        assert_eq!(target(&pairs, &None, Some("Virtual Sink")), None);
        assert_eq!(target(&[], &gone, Some("Monitor")), None);
    }
}
