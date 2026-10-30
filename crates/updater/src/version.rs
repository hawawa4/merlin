//! Release version numbers: `1.2.3`, with an optional leading `v`.

/// Reads `1.2.3` or `v1.2.3`. Anything else (pre-release suffixes, `dev`) is not a release.
pub fn parse(s: &str) -> Option<[u64; 3]> {
    let mut parts = s.trim().trim_start_matches('v').split('.');
    let mut v = [0u64; 3];
    for slot in &mut v {
        let p = parts.next()?;
        let n: u64 = p.parse().ok()?;
        // Rejects "01" and "+1", which parse but are not how a release is written.
        if n.to_string() != p {
            return None;
        }
        *slot = n;
    }
    parts.next().is_none().then_some(v)
}

pub fn is_release(v: &str) -> bool {
    parse(v).is_some()
}

/// Whether `latest` is a higher release than `current`. False when either is not a release.
pub fn newer(latest: &str, current: &str) -> bool {
    match (parse(latest), parse(current)) {
        (Some(l), Some(c)) => l > c,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_plain_versions_only() {
        assert_eq!(parse("1.2.3"), Some([1, 2, 3]));
        assert_eq!(parse("v0.6.10"), Some([0, 6, 10]));
        for bad in [
            "dev",
            "1.2",
            "1.2.3.4",
            "1.2.3-rc1",
            "01.2.3",
            "1.-2.3",
            "",
            "runtime",
        ] {
            assert_eq!(parse(bad), None, "{bad}");
        }
    }

    #[test]
    fn compares_numerically() {
        assert!(newer("0.10.0", "0.9.9"));
        assert!(newer("1.0.0", "0.99.99"));
        assert!(!newer("0.6.0", "0.6.0"));
        assert!(!newer("0.5.9", "0.6.0"));
    }

    #[test]
    fn development_builds_are_never_newer_or_older() {
        assert!(!newer("1.0.0", "dev"));
        assert!(!newer("dev", "1.0.0"));
    }
}
