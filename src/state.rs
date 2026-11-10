// What this machine has had APPLIED, as opposed to what is installed.
//
// Installing a new build replaces the binary and nothing else. Installing the
// launch helper, rebuilding launchers and moving profiles to a new way of
// working are `doctor --fix`. This records the version whose `doctor --fix`
// last ran, so a later version can tell its changes have not reached the
// profiles yet and say so.

use std::cmp::Ordering;

use serde_json::{Map, Value};

use crate::registry::config_dir;
use crate::util::{compare_versions, read_json, to_json_pretty};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

fn state_path() -> std::path::PathBuf {
    config_dir().join("state.json")
}

pub fn read_state() -> Map<String, Value> {
    match read_json(&state_path()) {
        Some(Value::Object(m)) => m,
        _ => Map::new(),
    }
}

pub fn fixes_applied_for() -> Option<String> {
    read_state().get("fixesAppliedFor").and_then(|v| v.as_str()).map(String::from)
}

/// Losing this only means one extra reminder, so failures are ignored.
pub fn record_fix_pass(version: &str) {
    let mut s = read_state();
    s.insert("fixesAppliedFor".into(), Value::String(version.into()));
    let p = state_path();
    if let Some(parent) = p.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(p, to_json_pretty(&s));
}

/// Names you gave accounts that only an ID identifies, such as a Claude
/// Desktop account that no Claude Code folder is signed in to.
pub fn account_names() -> Map<String, Value> {
    match read_state().get("accountNames") {
        Some(Value::Object(m)) => m.clone(),
        _ => Map::new(),
    }
}

/// Name an account, or clear its name with an empty one.
pub fn set_account_name(uuid: &str, name: &str) -> std::io::Result<()> {
    let mut s = read_state();
    let mut names = account_names();
    if name.trim().is_empty() {
        names.remove(uuid);
    } else {
        names.insert(uuid.into(), Value::String(name.trim().into()));
    }
    s.insert("accountNames".into(), Value::Object(names));
    let p = state_path();
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(p, to_json_pretty(&s))
}

/// Whether this version's fixes still need to reach the profiles. With no
/// profiles there is nothing to apply; with profiles and no record, they were
/// made by an older version.
pub fn fix_pass_due(current: &str, profile_count: usize, applied_for: Option<&str>) -> bool {
    if profile_count == 0 {
        return false;
    }
    match applied_for {
        None => true,
        Some(a) => compare_versions(a, current) == Ordering::Less,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reminds_until_the_installed_versions_fixes_have_run() {
        assert!(!fix_pass_due("0.2.0", 0, None));
        assert!(fix_pass_due("0.2.0", 1, None));
        assert!(fix_pass_due("0.2.0", 1, Some("0.1.39")));
        assert!(!fix_pass_due("0.2.0", 1, Some("0.2.0")));
        assert!(!fix_pass_due("0.2.0", 1, Some("0.3.0")));
    }
}
