//! Known plugin problems and what we do about them (plan §4.6 point 7, "quirks database").
//! A table in code for now; it moves to a data file when there are enough entries.

/// What is known about a plugin when it runs here.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Quirks {
    /// Do not load this plugin: it is known to crash here. The text says why.
    pub unsupported: Option<&'static str>,
    /// A warning shown beside the editor; the editor is still created.
    pub warning: Option<&'static str>,
}

pub fn lookup(id: &str, windows: bool) -> Quirks {
    match (id, windows) {
        ("studio.kx.distrho.cardinal#synth", true) => Quirks {
            unsupported: Some(
                "Cardinal crashes Wine (a stack overflow while it starts), which takes the whole \
                 app down with it, so it is not loaded.",
            ),
            warning: None,
        },
        ("org.surge-synth-team.OB-Xf", true) => Quirks {
            unsupported: None,
            warning: Some(
                "OB-Xf's editor is blank under Wine for now (it needs Direct2D composition \
                 swap chains). The instrument itself plays.",
            ),
        },
        _ => Quirks::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_windows_build_of_cardinal_is_special_cased() {
        assert!(
            lookup("studio.kx.distrho.cardinal#synth", true)
                .unsupported
                .is_some()
        );
        assert_eq!(
            lookup("studio.kx.distrho.cardinal#synth", false),
            Quirks::default()
        );
        assert_eq!(
            lookup("org.surge-synth-team.surge-xt", true),
            Quirks::default()
        );
    }
}
