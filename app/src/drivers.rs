//! The device drivers this build includes (see `devices/`). Each is a crate of its own that
//! knows one family of controllers and nothing about this app; the app asks them which of them
//! runs a MIDI port. A driver is left out of a build by turning its cargo feature off.

use surface_api::{Driver, Surface};

/// The drivers compiled in.
fn all() -> Vec<Box<dyn Driver>> {
    vec![
        #[cfg(feature = "launchkey-mk3")]
        Box::new(launchkey_mk3::LaunchkeyMk3),
    ]
}

/// A surface for the device whose control port has this readable name, if a driver knows it.
pub fn detect(port_name: &str) -> Option<Box<dyn Surface>> {
    all().iter().find_map(|d| d.detect(port_name))
}
