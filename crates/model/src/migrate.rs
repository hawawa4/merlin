//! Rig file migrations: older files are upgraded step by step before they are parsed.

use serde_json::Value;

use crate::{RIG_VERSION, Rig, RigError};

/// Rewrites a rig's JSON from one format version to the next.
pub type Migration = fn(&mut Value);

/// `MIGRATIONS[n]` upgrades format `n + 1` to `n + 2`. Format 1 is the first, so there are none
/// yet: the first format change adds its step here and bumps [`RIG_VERSION`].
const MIGRATIONS: &[Migration] = &[];

/// Upgrades `value` to `current` with `steps`. Returns the version the file had. A file without
/// a version is format 1.
pub fn migrate(value: &mut Value, steps: &[Migration], current: u32) -> Result<u32, RigError> {
    let found = value
        .get("version")
        .and_then(Value::as_u64)
        .map_or(1, |v| v.clamp(1, u32::MAX as u64) as u32);
    if found > current {
        return Err(RigError::NewerVersion(found));
    }
    for v in found..current {
        let step = steps.get(v as usize - 1).ok_or(RigError::Unmigratable(v))?;
        step(value);
        value["version"] = Value::from(v + 1);
    }
    Ok(found)
}

impl Rig {
    /// Parses a rig file of any supported version. The flag says whether it was upgraded.
    pub fn from_json(bytes: &[u8]) -> Result<(Rig, bool), RigError> {
        let mut value: Value =
            serde_json::from_slice(bytes).map_err(|e| RigError::Parse(e.to_string()))?;
        let found = migrate(&mut value, MIGRATIONS, RIG_VERSION)?;
        let rig: Rig = serde_json::from_value(value).map_err(|e| RigError::Parse(e.to_string()))?;
        rig.validate()?;
        Ok((rig, found != RIG_VERSION))
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn rename_name_to_title(v: &mut Value) {
        if let Some(n) = v.as_object_mut().and_then(|o| o.remove("name")) {
            v["title"] = n;
        }
    }

    fn add_flag(v: &mut Value) {
        v["flag"] = json!(true);
    }

    #[test]
    fn steps_run_in_order_from_the_files_version() {
        let steps: &[Migration] = &[rename_name_to_title, add_flag];
        let mut v = json!({"version": 1, "name": "x"});
        assert_eq!(migrate(&mut v, steps, 3), Ok(1));
        assert_eq!(v, json!({"version": 3, "title": "x", "flag": true}));

        let mut v = json!({"version": 2, "title": "y"});
        assert_eq!(migrate(&mut v, steps, 3), Ok(2));
        assert_eq!(v, json!({"version": 3, "title": "y", "flag": true}));

        let mut v = json!({"version": 3});
        assert_eq!(migrate(&mut v, steps, 3), Ok(3));
        assert_eq!(v, json!({"version": 3}), "current files are untouched");
    }

    #[test]
    fn a_missing_version_is_format_one() {
        let mut v = json!({"name": "x"});
        assert_eq!(migrate(&mut v, &[add_flag], 2), Ok(1));
        assert_eq!(v["flag"], json!(true));
        let mut v = json!({"version": 0});
        assert_eq!(
            migrate(&mut v, &[add_flag], 2),
            Ok(1),
            "0 is not a real version"
        );
    }

    #[test]
    fn newer_and_unmigratable_files_are_refused() {
        let mut v = json!({"version": 9});
        assert_eq!(migrate(&mut v, &[], 3), Err(RigError::NewerVersion(9)));
        // Version 1 but no step to get to 2: refuse rather than guess.
        let mut v = json!({"version": 1});
        assert_eq!(migrate(&mut v, &[], 2), Err(RigError::Unmigratable(1)));
    }

    #[test]
    fn current_files_load_and_garbage_is_an_error() {
        let (rig, migrated) = Rig::from_json(br#"{"name":"x","slots":[{"name":"a"}]}"#).unwrap();
        assert!(!migrated);
        assert_eq!(rig.slots[0].name, "a");
        assert!(matches!(
            Rig::from_json(b"not json"),
            Err(RigError::Parse(_))
        ));
        assert_eq!(
            Rig::from_json(br#"{"version": 99}"#),
            Err(RigError::NewerVersion(99))
        );
    }
}
