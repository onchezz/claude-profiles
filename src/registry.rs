// Profile registry.
//
// A small, human-readable JSON file in the XDG config folder (~/.config when
// XDG_CONFIG_HOME is not set, which is usual on macOS) recording every profile
// so list, status, remove and the rest work without scraping the filesystem.
//
// Fields this version does not know about are kept as they are, so a file
// written by a newer version, or edited by hand, survives a round trip.

use std::path::{Path, PathBuf};

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::util::{home, to_json_pretty};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DesktopTarget {
    pub data_dir: PathBuf,
    pub app_path: PathBuf,
    pub claude_app_path: PathBuf,
    #[serde(default)]
    pub color: Option<String>,
    /// true when the user chose to let Claude update this profile's copy
    /// itself (`self-update <name> on`). Absent means the default: blocked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub self_update: Option<bool>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CodeTarget {
    pub config_dir: PathBuf,
    pub alias_name: String,
    /// The shell and rc file the alias was written to. Absent on entries
    /// from before they were recorded; the current shell is used then.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shell: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rc_path: Option<PathBuf>,
    #[serde(default)]
    pub gh_config_dir: Option<PathBuf>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Profile {
    pub name: String,
    /// "desktop", "code" or "both".
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub desktop: Option<DesktopTarget>,
    #[serde(default)]
    pub code: Option<CodeTarget>,
    #[serde(default)]
    pub created_at: String,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Profile {
    pub fn kind_for(desktop: bool, code: bool) -> &'static str {
        match (desktop, code) {
            (true, true) => "both",
            (true, false) => "desktop",
            _ => "code",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Registry {
    #[serde(default = "one")]
    pub version: u32,
    pub profiles: Vec<Profile>,
    /// false turns cross-profile read protection off for every profile.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read_protection: Option<bool>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

fn one() -> u32 {
    1
}

impl Default for Registry {
    fn default() -> Self {
        Registry { version: 1, profiles: Vec::new(), read_protection: None, extra: Map::new() }
    }
}

impl Registry {
    pub fn find(&self, name: &str) -> Option<&Profile> {
        self.profiles.iter().find(|p| p.name == name)
    }
}

// ---- Where it lives -------------------------------------------------------------

/// The tool's config folder, for the registry and the files beside it.
pub fn config_dir() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".config"));
    base.join("claude-profiles")
}

pub fn registry_path() -> PathBuf {
    config_dir().join("profiles.json")
}

// ---- Read -----------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Health {
    Ok,
    /// No file yet: a fresh install.
    Missing,
    /// The file exists but cannot be read as a registry. Never treated as
    /// empty for writing, or the next add or remove would erase the list.
    Corrupt,
}

fn read_raw(path: &Path) -> (Health, Option<Registry>) {
    let Ok(raw) = std::fs::read_to_string(path) else { return (Health::Missing, None) };
    match serde_json::from_str::<Registry>(&raw) {
        Ok(r) => (Health::Ok, Some(r)),
        Err(_) => (Health::Corrupt, None),
    }
}

pub fn health() -> Health {
    read_raw(&registry_path()).0
}

/// Always a usable registry, so read-only commands keep working. A corrupt
/// file reads as empty here; saving refuses to overwrite it.
pub fn load() -> Registry {
    load_from(&registry_path())
}

pub fn load_from(path: &Path) -> Registry {
    read_raw(path).1.unwrap_or_default()
}

pub fn find_profile(name: &str) -> Option<Profile> {
    load().find(name).cloned()
}

// ---- Write ----------------------------------------------------------------------

pub fn save_to(path: &Path, reg: &Registry) -> Result<()> {
    match read_raw(path).0 {
        Health::Corrupt => bail!(
            "The registry at {p} exists but is not valid JSON. Refusing to overwrite it. \
             Fix the JSON by hand (or restore {p}.bak if present), then re-run this command.",
            p = path.display()
        ),
        Health::Ok => {
            // One backup generation of the last good file; best effort.
            let _ = std::fs::copy(path, crate::util::with_suffix(path, ".bak"));
        }
        Health::Missing => {}
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, to_json_pretty(reg))?;
    Ok(())
}

fn update(f: impl FnOnce(&mut Registry)) -> Result<()> {
    let path = registry_path();
    let mut reg = load_from(&path);
    f(&mut reg);
    save_to(&path, &reg)
}

pub fn add(profile: Profile) -> Result<()> {
    update(|reg| {
        reg.profiles.retain(|p| p.name != profile.name);
        reg.profiles.push(profile);
    })
}

pub fn remove(name: &str) -> Result<()> {
    update(|reg| reg.profiles.retain(|p| p.name != name))
}

/// Swap the entry for `old_name` with `profile`, which may carry a new name.
/// Order is kept so `list` output does not jump around.
pub fn replace(old_name: &str, profile: Profile) -> Result<()> {
    update(|reg| match reg.profiles.iter().position(|p| p.name == old_name) {
        Some(i) => reg.profiles[i] = profile,
        None => reg.profiles.push(profile),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(name: &str) -> Profile {
        Profile {
            name: name.into(),
            kind: "code".into(),
            desktop: None,
            code: Some(CodeTarget {
                config_dir: PathBuf::from(format!("/h/.claude-{name}")),
                alias_name: format!("claude-{name}"),
                shell: Some("zsh".into()),
                rc_path: Some("/h/.zshrc".into()),
                gh_config_dir: None,
                extra: Map::new(),
            }),
            created_at: "2026-01-01T00:00:00.000Z".into(),
            extra: Map::new(),
        }
    }

    #[test]
    fn corrupt_reads_as_empty_but_is_never_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("profiles.json");
        std::fs::write(&p, "{ not json").unwrap();
        assert_eq!(read_raw(&p).0, Health::Corrupt);
        assert!(load_from(&p).profiles.is_empty());
        assert!(save_to(&p, &Registry::default()).is_err());
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "{ not json");
    }

    #[test]
    fn writes_back_up_the_previous_file_and_keep_unknown_fields() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("profiles.json");
        std::fs::write(
            &p,
            r#"{"version":1,"readProtection":false,"future":1,"profiles":[{"name":"a","type":"code","desktop":null,"code":null,"createdAt":"x","note":"kept"}]}"#,
        )
        .unwrap();
        let mut reg = load_from(&p);
        reg.profiles.push(sample("b"));
        save_to(&p, &reg).unwrap();
        assert!(dir.path().join("profiles.json.bak").exists());
        let back: Value = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        assert_eq!(back["future"], 1);
        assert_eq!(back["readProtection"], false);
        assert_eq!(back["profiles"][0]["note"], "kept");
        assert_eq!(back["profiles"][1]["code"]["ghConfigDir"], Value::Null);
    }
}
