// Cross-profile read protection for Claude Code profiles.
//
// Profiles are isolated by configuration, not by reachability: a running Code
// profile's tools can still wander into a sibling's folder during an ordinary
// search and pull its CLAUDE.md, skills or MCP config into context. Claude
// Code's `permissions.deny` rules with a `Read(...)` matcher block that outright
// (Grep and Glob included), so every Code profile gets a rule for each other
// profile's folders.
//
// Only the rules this tool wrote are ever removed. They are recorded under a
// marker key in the same settings.json; the user's own rules are left alone.
// Desktop profiles have no equivalent hook, so this covers Code only.

use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use crate::registry::{Profile, Registry};
use crate::ui::{dim, info, ok, warn};
use crate::util::{file_exists, tildify, to_json_pretty};

const MARKER_KEY: &str = "claudeProfilesManagedDeny";

pub fn settings_path_for(config_dir: &Path) -> PathBuf {
    config_dir.join("settings.json")
}

#[derive(Debug, Clone, PartialEq)]
pub enum WriteSafety {
    Safe,
    /// settings.json is a link out of the profile (often to shared settings).
    /// Writing would push this profile's rules into a file the others read,
    /// and each profile would strip the others' rules on every change.
    Symlink(Option<PathBuf>),
    /// A broken link: writing would create its target somewhere else.
    BrokenSymlink,
}

pub fn settings_write_safety(config_dir: &Path) -> WriteSafety {
    let p = settings_path_for(config_dir);
    let Ok(meta) = std::fs::symlink_metadata(&p) else { return WriteSafety::Safe };
    if !meta.file_type().is_symlink() {
        return WriteSafety::Safe;
    }
    let Ok(target) = std::fs::canonicalize(&p) else { return WriteSafety::BrokenSymlink };
    let Ok(root) = std::fs::canonicalize(config_dir) else { return WriteSafety::Symlink(Some(target)) };
    if target.starts_with(&root) {
        WriteSafety::Safe
    } else {
        WriteSafety::Symlink(Some(target))
    }
}

/// Claude Code wants a DOUBLE leading slash for an absolute path; one slash
/// would be read relative to the settings file. `/**` covers the folder and
/// everything in it.
fn deny_rule_for_path(p: &Path) -> Option<String> {
    let s = p.to_string_lossy();
    if !s.starts_with('/') {
        return None;
    }
    let trimmed = s.trim_end_matches('/').trim_start_matches('/');
    Some(format!("Read(//{trimmed}/**)"))
}

/// What a profile should be blocked from reading: every OTHER profile's Code
/// config and Desktop data folders. The default ~/.claude is not denied; the
/// point is that profiles do not discover each other.
pub fn managed_rules_for(profile: &Profile, all: &[Profile]) -> Vec<String> {
    let mut rules: Vec<String> = Vec::new();
    for other in all.iter().filter(|o| o.name != profile.name) {
        let mut dirs = Vec::new();
        if let Some(c) = &other.code {
            dirs.push(&c.config_dir);
        }
        if let Some(d) = &other.desktop {
            dirs.push(&d.data_dir);
        }
        for d in dirs {
            if let Some(r) = deny_rule_for_path(d) {
                if !rules.contains(&r) {
                    rules.push(r);
                }
            }
        }
    }
    rules.sort();
    rules
}

enum Settings {
    Ok(Map<String, Value>),
    /// Present but not a JSON object. Never written over: a trailing comma
    /// is all it takes, and writing would replace the user's file.
    Malformed,
}

fn read_settings(p: &Path) -> Settings {
    let Ok(raw) = std::fs::read_to_string(p) else { return Settings::Ok(Map::new()) };
    if raw.trim().is_empty() {
        return Settings::Ok(Map::new());
    }
    match serde_json::from_str::<Value>(&raw) {
        Ok(Value::Object(m)) => Settings::Ok(m),
        _ => Settings::Malformed,
    }
}

fn string_list(v: Option<&Value>) -> Vec<String> {
    v.and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
        .unwrap_or_default()
}

pub enum Resync {
    Updated { name: String, path: PathBuf, rules: usize },
    Malformed { name: String, path: PathBuf },
    Linked { name: String, path: PathBuf, target: Option<PathBuf> },
}

fn resync_one(profile: &Profile, all: &[Profile]) -> Option<Resync> {
    let code = profile.code.as_ref()?;
    if !file_exists(&code.config_dir) {
        return None;
    }
    let path = settings_path_for(&code.config_dir);
    match settings_write_safety(&code.config_dir) {
        WriteSafety::Safe => {}
        WriteSafety::Symlink(target) => return Some(Resync::Linked { name: profile.name.clone(), path, target }),
        WriteSafety::BrokenSymlink => return Some(Resync::Linked { name: profile.name.clone(), path, target: None }),
    }
    let Settings::Ok(mut settings) = read_settings(&path) else {
        return Some(Resync::Malformed { name: profile.name.clone(), path });
    };
    let previous = string_list(settings.get(MARKER_KEY));
    let managed = managed_rules_for(profile, all);
    // A single profile with nothing written before: leave its file alone.
    if managed.is_empty() && previous.is_empty() {
        return None;
    }

    let permissions = settings.entry("permissions").or_insert_with(|| Value::Object(Map::new()));
    if !permissions.is_object() {
        *permissions = Value::Object(Map::new());
    }
    let perms = permissions.as_object_mut().expect("just made an object");
    let mut deny: Vec<Value> = perms.get("deny").and_then(|d| d.as_array()).cloned().unwrap_or_default();
    deny.retain(|r| !r.as_str().is_some_and(|s| previous.iter().any(|p| p == s)));
    for r in &managed {
        if !deny.iter().any(|d| d.as_str() == Some(r)) {
            deny.push(Value::String(r.clone()));
        }
    }
    perms.insert("deny".into(), Value::Array(deny));
    if managed.is_empty() {
        settings.remove(MARKER_KEY);
    } else {
        settings.insert(MARKER_KEY.into(), Value::from(managed.clone()));
    }
    std::fs::write(&path, to_json_pretty(&settings)).ok()?;
    Some(Resync::Updated { name: profile.name.clone(), path, rules: managed.len() })
}

/// Opting out is registry-wide: profiles meant to share context need every
/// one of them readable.
pub fn read_protection_enabled(reg: &Registry) -> bool {
    reg.read_protection != Some(false)
}

/// Rewrite the deny rules of every Code profile. Call after any change to the
/// set of profiles.
pub fn resync_deny_rules(reg: &Registry, verbose: bool) {
    if !read_protection_enabled(reg) {
        let stripped: Vec<&str> =
            reg.profiles.iter().filter(|p| strip_managed_deny_rules(p)).map(|p| p.name.as_str()).collect();
        if verbose && !stripped.is_empty() {
            ok(format!("Removed cross-profile read-protection from {}.", stripped.join(", ")));
        }
        return;
    }
    let results: Vec<Resync> = reg.profiles.iter().filter_map(|p| resync_one(p, &reg.profiles)).collect();
    let updated: Vec<&Resync> = results.iter().filter(|r| matches!(r, Resync::Updated { .. })).collect();
    if verbose && !updated.is_empty() {
        ok("Updated cross-profile read-protection:");
        for r in updated {
            if let Resync::Updated { name, path, rules } = r {
                info(format!(
                    "  {name}: {rules} rule{} {}",
                    if *rules == 1 { "" } else { "s" },
                    dim(format!("({})", tildify(path)))
                ));
            }
        }
    }
    // A skipped file is a safety event, so it is reported even when quiet.
    for r in &results {
        match r {
            Resync::Malformed { name, path } => {
                warn(format!("Skipped {name}: {} is not valid JSON, so it was left untouched.", tildify(path)));
                info("  Fix the JSON by hand, then run `claude-profiles doctor --fix`.");
            }
            Resync::Linked { name, path, target } => {
                warn(format!("Skipped {name}: {} is a symlink out of the profile.", tildify(path)));
                if let Some(t) = target {
                    info(format!("  It points at {}", tildify(t)));
                }
                info("  Writing there would push this profile's rules into shared config,");
                info("  where other profiles would read and overwrite them. Replace the link");
                info("  with a real file to get cross-profile read protection.");
            }
            Resync::Updated { .. } => {}
        }
    }
}

/// Take this tool's rules (and its marker) out of one profile's settings,
/// leaving the user's own. For a folder kept after `remove`.
pub fn strip_managed_deny_rules(profile: &Profile) -> bool {
    let Some(code) = &profile.code else { return false };
    if !file_exists(&code.config_dir) {
        return false;
    }
    let path = settings_path_for(&code.config_dir);
    if settings_write_safety(&code.config_dir) != WriteSafety::Safe {
        warn(format!("{} is a symlink out of the profile; left untouched.", tildify(&path)));
        return false;
    }
    let Settings::Ok(mut settings) = read_settings(&path) else {
        warn(format!("{} is not valid JSON; left untouched.", tildify(&path)));
        return false;
    };
    let previous = string_list(settings.get(MARKER_KEY));
    if previous.is_empty() {
        return false;
    }
    if let Some(deny) = settings.get_mut("permissions").and_then(|p| p.get_mut("deny")).and_then(|d| d.as_array_mut()) {
        deny.retain(|r| !r.as_str().is_some_and(|s| previous.iter().any(|p| p == s)));
    }
    settings.remove(MARKER_KEY);
    std::fs::write(&path, to_json_pretty(&settings)).is_ok()
}

pub struct Audit {
    pub name: String,
    pub settings_path: PathBuf,
    pub expected: usize,
    pub missing: Vec<String>,
    pub symlink: Option<Option<PathBuf>>,
    pub malformed: bool,
}

/// Read-only, for doctor: does each Code profile deny every sibling it should?
pub fn audit_deny_rules(reg: &Registry) -> Vec<Audit> {
    if !read_protection_enabled(reg) {
        return Vec::new();
    }
    let mut out = Vec::new();
    for p in &reg.profiles {
        let Some(code) = &p.code else { continue };
        let expected = managed_rules_for(p, &reg.profiles);
        if expected.is_empty() {
            continue;
        }
        let settings_path = settings_path_for(&code.config_dir);
        let base = |missing: Vec<String>| Audit {
            name: p.name.clone(),
            settings_path: settings_path.clone(),
            expected: expected.len(),
            missing,
            symlink: None,
            malformed: false,
        };
        match settings_write_safety(&code.config_dir) {
            WriteSafety::Safe => {}
            WriteSafety::Symlink(t) => {
                out.push(Audit { symlink: Some(t), ..base(expected.clone()) });
                continue;
            }
            WriteSafety::BrokenSymlink => {
                out.push(Audit { symlink: Some(None), ..base(expected.clone()) });
                continue;
            }
        }
        match read_settings(&settings_path) {
            Settings::Malformed => out.push(Audit { malformed: true, ..base(expected.clone()) }),
            Settings::Ok(s) => {
                let deny = string_list(s.get("permissions").and_then(|p| p.get("deny")));
                let missing = expected.iter().filter(|r| !deny.contains(r)).cloned().collect();
                out.push(base(missing));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::{CodeTarget, DesktopTarget};

    fn code_profile(name: &str, root: &Path) -> Profile {
        let dir = root.join(format!(".claude-{name}"));
        std::fs::create_dir_all(&dir).unwrap();
        Profile {
            name: name.into(),
            kind: "code".into(),
            desktop: None,
            code: Some(CodeTarget {
                config_dir: dir,
                alias_name: format!("claude-{name}"),
                shell: None,
                rc_path: None,
                gh_config_dir: None,
                extra: Map::new(),
            }),
            created_at: String::new(),
            extra: Map::new(),
        }
    }

    fn reg(profiles: Vec<Profile>) -> Registry {
        Registry { profiles, ..Registry::default() }
    }

    fn deny_of(p: &Profile) -> Vec<String> {
        let s: Value = serde_json::from_str(&std::fs::read_to_string(settings_path_for(&p.code.as_ref().unwrap().config_dir)).unwrap()).unwrap();
        string_list(s["permissions"].get("deny"))
    }

    #[test]
    fn each_profile_denies_every_sibling_with_double_slash_syntax() {
        let root = tempfile::tempdir().unwrap();
        let a = code_profile("a", root.path());
        let mut b = code_profile("b", root.path());
        b.desktop = Some(DesktopTarget {
            data_dir: root.path().join("Claude-B"),
            app_path: root.path().join("Claude B.app"),
            claude_app_path: "/Applications/Claude.app".into(),
            color: None,
            self_update: None,
            extra: Map::new(),
        });
        let r = reg(vec![a.clone(), b.clone()]);
        resync_deny_rules(&r, false);
        let rb = root.path().to_string_lossy().trim_start_matches('/').to_string();
        assert_eq!(deny_of(&a), vec![format!("Read(//{rb}/.claude-b/**)"), format!("Read(//{rb}/Claude-B/**)")]);
        assert_eq!(deny_of(&b), vec![format!("Read(//{rb}/.claude-a/**)")]);
    }

    #[test]
    fn user_rules_survive_and_stale_rules_go() {
        let root = tempfile::tempdir().unwrap();
        let a = code_profile("a", root.path());
        let b = code_profile("b", root.path());
        let path = settings_path_for(&a.code.as_ref().unwrap().config_dir);
        std::fs::write(&path, r#"{"model":"x","permissions":{"deny":["Read(./secret)"]}}"#).unwrap();
        resync_deny_rules(&reg(vec![a.clone(), b.clone()]), false);
        assert_eq!(deny_of(&a).len(), 2);
        // b goes away: its rule goes, the user's stays, and twice is the same as once.
        resync_deny_rules(&reg(vec![a.clone()]), false);
        resync_deny_rules(&reg(vec![a.clone()]), false);
        assert_eq!(deny_of(&a), vec!["Read(./secret)"]);
        let s: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert!(s.get(MARKER_KEY).is_none());
        assert_eq!(s["model"], "x");
        // Key order is kept as the user wrote it.
        assert!(std::fs::read_to_string(&path).unwrap().find("\"model\"") < std::fs::read_to_string(&path).unwrap().find("\"permissions\""));
    }

    #[test]
    fn read_protection_off_strips_ours_and_keeps_theirs() {
        let root = tempfile::tempdir().unwrap();
        let a = code_profile("a", root.path());
        let b = code_profile("b", root.path());
        let path = settings_path_for(&a.code.as_ref().unwrap().config_dir);
        std::fs::write(&path, r#"{"permissions":{"deny":["Read(./mine)"]}}"#).unwrap();
        resync_deny_rules(&reg(vec![a.clone(), b.clone()]), false);
        let mut off = reg(vec![a.clone(), b.clone()]);
        off.read_protection = Some(false);
        resync_deny_rules(&off, false);
        assert_eq!(deny_of(&a), vec!["Read(./mine)"]);
        assert!(audit_deny_rules(&off).is_empty());
    }

    #[test]
    fn malformed_settings_are_never_overwritten() {
        let root = tempfile::tempdir().unwrap();
        let a = code_profile("a", root.path());
        let b = code_profile("b", root.path());
        let path = settings_path_for(&a.code.as_ref().unwrap().config_dir);
        std::fs::write(&path, "{ \"x\": 1, }").unwrap();
        resync_deny_rules(&reg(vec![a.clone(), b.clone()]), false);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{ \"x\": 1, }");
        let audit = audit_deny_rules(&reg(vec![a, b]));
        assert!(audit.iter().any(|f| f.name == "a" && f.malformed));
    }

    #[test]
    fn never_writes_through_a_link_out_but_allows_one_inside() {
        let root = tempfile::tempdir().unwrap();
        let a = code_profile("a", root.path());
        let b = code_profile("b", root.path());
        let shared = root.path().join("shared.json");
        std::fs::write(&shared, "{}").unwrap();
        let a_dir = a.code.as_ref().unwrap().config_dir.clone();
        std::os::unix::fs::symlink(&shared, settings_path_for(&a_dir)).unwrap();
        resync_deny_rules(&reg(vec![a.clone(), b.clone()]), false);
        assert_eq!(std::fs::read_to_string(&shared).unwrap(), "{}");
        assert!(matches!(settings_write_safety(&a_dir), WriteSafety::Symlink(_)));

        let b_dir = b.code.as_ref().unwrap().config_dir.clone();
        std::fs::write(b_dir.join("real.json"), "{}").unwrap();
        std::fs::remove_file(settings_path_for(&b_dir)).unwrap();
        std::os::unix::fs::symlink(b_dir.join("real.json"), settings_path_for(&b_dir)).unwrap();
        assert_eq!(settings_write_safety(&b_dir), WriteSafety::Safe);
    }

    #[test]
    fn strip_removes_ours_and_the_marker() {
        let root = tempfile::tempdir().unwrap();
        let a = code_profile("a", root.path());
        let b = code_profile("b", root.path());
        let path = settings_path_for(&a.code.as_ref().unwrap().config_dir);
        std::fs::write(&path, r#"{"permissions":{"deny":["Read(./mine)"]}}"#).unwrap();
        resync_deny_rules(&reg(vec![a.clone(), b]), false);
        assert!(strip_managed_deny_rules(&a));
        assert_eq!(deny_of(&a), vec!["Read(./mine)"]);
        assert!(!strip_managed_deny_rules(&a), "nothing left to strip");
    }

    #[test]
    fn audit_reports_drift() {
        let root = tempfile::tempdir().unwrap();
        let a = code_profile("a", root.path());
        let b = code_profile("b", root.path());
        let r = reg(vec![a.clone(), b.clone()]);
        let before = audit_deny_rules(&r);
        assert!(before.iter().all(|f| f.missing.len() == 1));
        resync_deny_rules(&r, false);
        assert!(audit_deny_rules(&r).iter().all(|f| f.missing.is_empty()));
    }
}
