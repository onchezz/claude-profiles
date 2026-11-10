// Snapshots before `doctor --fix`, and `doctor --undo` to go back.
//
// `upgrade` runs `doctor --fix` on its own, so a repair rule that turns out to
// be wrong reaches every profile unasked. This makes every fix run
// reversible.
//
// A snapshot covers the small files --fix edits: each Code profile's
// settings.json and plugin manifests, each launcher's script, Info.plist and
// icon, the launch helper, the update-block folders it creates, and
// state.json. Not covered: rebuilt copies of Claude.app, re-applied colours
// and a launcher's removed asset catalog, which are rebuilt rather than
// edited and which running --fix again regenerates.
//
// Only what a run actually changed is kept; a run that changed nothing
// leaves no snapshot at all.

use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::launcher::helper_path;
use crate::registry::{config_dir, Registry};
use crate::updates::local_config_root_for;
use crate::util::{file_exists, remove_all, to_json_pretty};

const KEEP: usize = 10;
const MARKER: &str = ".claude-profiles";

fn snap_root() -> PathBuf {
    config_dir().join("snapshots")
}

fn sha(p: &Path) -> Option<String> {
    let bytes = std::fs::read(p).ok()?;
    Some(Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect())
}

pub struct Targets {
    pub files: Vec<PathBuf>,
    pub dirs: Vec<PathBuf>,
}

/// Every path a fix run may edit, for this registry.
pub fn fix_targets(reg: &Registry) -> Targets {
    let mut files = vec![helper_path(), config_dir().join("state.json")];
    let mut dirs = Vec::new();
    for p in &reg.profiles {
        if let Some(c) = &p.code {
            files.push(c.config_dir.join("settings.json"));
            files.push(c.config_dir.join("plugins").join("installed_plugins.json"));
            files.push(c.config_dir.join("plugins").join("known_marketplaces.json"));
        }
        if let Some(d) = &p.desktop {
            let contents = d.app_path.join("Contents");
            files.push(contents.join("Info.plist"));
            files.push(contents.join("Resources").join("Scripts").join("main.scpt"));
            files.push(contents.join("Resources").join("applet.icns"));
            dirs.push(local_config_root_for(&d.data_dir));
        }
    }
    Targets { files, dirs }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    pub kind: String,
    pub path: PathBuf,
    pub existed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stored: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<String>,
    #[serde(default)]
    pub created: bool,
}

#[derive(Debug, Serialize, Deserialize)]
struct Manifest {
    id: String,
    /// Milliseconds since the epoch.
    at: i64,
    entries: Vec<Entry>,
}

pub struct Snapshot {
    pub id: String,
    pub dir: PathBuf,
    pub entries: Vec<Entry>,
}

/// Record the state of every target before a fix run.
pub fn take_snapshot(t: &Targets) -> Result<Snapshot> {
    take_snapshot_in(&snap_root(), t)
}

fn take_snapshot_in(root: &Path, t: &Targets) -> Result<Snapshot> {
    let id = chrono::Utc::now().format("%Y-%m-%dT%H-%M-%S-%3fZ").to_string();
    let dir = root.join(&id);
    std::fs::create_dir_all(dir.join("files"))?;
    let mut entries = Vec::new();
    for (i, f) in t.files.iter().enumerate() {
        if f.is_file() {
            let stored = format!("files/{i}");
            std::fs::copy(f, dir.join(&stored))?;
            entries.push(Entry { kind: "file".into(), path: f.clone(), existed: true, stored: Some(stored), before: sha(f), after: None, created: false });
        } else {
            entries.push(Entry { kind: "file".into(), path: f.clone(), existed: false, stored: None, before: None, after: None, created: false });
        }
    }
    for d in &t.dirs {
        entries.push(Entry { kind: "dir".into(), path: d.clone(), existed: file_exists(d), stored: None, before: None, after: None, created: false });
    }
    Ok(Snapshot { id, dir, entries })
}

/// After the run: keep only what changed, with its new state, so undo can
/// tell whether anything was edited since. Returns the changed paths; with
/// nothing changed, the snapshot is deleted.
pub fn finish_snapshot(snap: &Snapshot) -> Result<Vec<PathBuf>> {
    finish_snapshot_in(&snap_root(), snap)
}

fn finish_snapshot_in(root: &Path, snap: &Snapshot) -> Result<Vec<PathBuf>> {
    let mut changed = Vec::new();
    for e in &snap.entries {
        if e.kind == "dir" {
            let now = file_exists(&e.path);
            if now != e.existed {
                changed.push(Entry { created: now && !e.existed, ..e.clone() });
            }
            continue;
        }
        let after = sha(&e.path);
        let differs = if e.existed { after != e.before } else { after.is_some() };
        if differs {
            changed.push(Entry { after, ..e.clone() });
        }
    }
    if changed.is_empty() {
        remove_all(&snap.dir)?;
        return Ok(Vec::new());
    }
    let manifest = Manifest { id: snap.id.clone(), at: chrono::Utc::now().timestamp_millis(), entries: changed.clone() };
    std::fs::write(snap.dir.join("manifest.json"), to_json_pretty(&manifest))?;
    let keep: Vec<&str> = changed.iter().filter_map(|e| e.stored.as_deref()).collect();
    for f in std::fs::read_dir(snap.dir.join("files"))?.flatten() {
        let rel = format!("files/{}", f.file_name().to_string_lossy());
        if !keep.contains(&rel.as_str()) {
            let _ = std::fs::remove_file(f.path());
        }
    }
    prune(root);
    Ok(changed.into_iter().map(|e| e.path).collect())
}

pub fn discard(snap: &Snapshot) {
    let _ = remove_all(&snap.dir);
}

fn list_snapshots(root: &Path) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(root) else { return Vec::new() };
    let mut names: Vec<String> = rd.flatten().map(|e| e.file_name().to_string_lossy().to_string()).collect();
    names.sort();
    names.into_iter().map(|n| root.join(n)).filter(|d| d.join("manifest.json").exists()).collect()
}

fn prune(root: &Path) {
    let all = list_snapshots(root);
    for d in all.iter().take(all.len().saturating_sub(KEEP)) {
        let _ = remove_all(d);
    }
}

pub struct Undo {
    pub at: i64,
    pub restored: Vec<PathBuf>,
    pub removed: Vec<PathBuf>,
    pub skipped: Vec<(PathBuf, &'static str)>,
}

/// Put back the most recent fix run's changes. A file edited since that run
/// is skipped unless `force`. A fully applied undo deletes its snapshot, so
/// the next one goes a run further back. None when there is nothing to undo.
pub fn undo_latest(force: bool) -> Result<Option<Undo>> {
    undo_latest_in(&snap_root(), force)
}

fn undo_latest_in(root: &Path, force: bool) -> Result<Option<Undo>> {
    let Some(dir) = list_snapshots(root).pop() else { return Ok(None) };
    let mut manifest: Manifest = serde_json::from_str(&std::fs::read_to_string(dir.join("manifest.json"))?)?;
    let mut r = Undo { at: manifest.at, restored: vec![], removed: vec![], skipped: vec![] };

    for e in &manifest.entries {
        if e.kind == "dir" {
            // Only a folder the run created, and still marked as the tool's.
            if !e.created || !file_exists(&e.path) {
                continue;
            }
            if !file_exists(e.path.join(MARKER)) {
                r.skipped.push((e.path.clone(), "no longer the tool's own folder"));
                continue;
            }
            remove_all(&e.path)?;
            r.removed.push(e.path.clone());
            continue;
        }
        let now = sha(&e.path);
        if !force && now != e.after {
            r.skipped.push((e.path.clone(), "changed since that run"));
            continue;
        }
        if e.existed {
            if let Some(parent) = e.path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::copy(dir.join(e.stored.as_deref().unwrap_or("")), &e.path)?;
            r.restored.push(e.path.clone());
        } else if now.is_some() {
            std::fs::remove_file(&e.path)?;
            r.removed.push(e.path.clone());
        }
    }

    if r.skipped.is_empty() {
        remove_all(&dir)?;
    } else {
        // Keep only what is left, so a retry with --force does not trip over
        // what was already put back.
        let left: Vec<&PathBuf> = r.skipped.iter().map(|(p, _)| p).collect();
        manifest.entries.retain(|e| left.contains(&&e.path));
        std::fs::write(dir.join("manifest.json"), to_json_pretty(&manifest))?;
    }
    Ok(Some(r))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn undo_puts_back_what_fix_changed_and_never_clobbers_later_edits() {
        let root = tempfile::tempdir().unwrap();
        let snaps = root.path().join("snapshots");
        let a = root.path().join("a.json");
        let b = root.path().join("b.json");
        let created = root.path().join("new.json");
        std::fs::write(&a, "a1").unwrap();
        std::fs::write(&b, "b1").unwrap();
        let t = Targets { files: vec![a.clone(), b.clone(), created.clone()], dirs: vec![] };

        let s = take_snapshot_in(&snaps, &t).unwrap();
        std::fs::write(&a, "a2").unwrap();
        std::fs::write(&created, "n").unwrap();
        let changed = finish_snapshot_in(&snaps, &s).unwrap();
        assert_eq!(changed, vec![a.clone(), created.clone()]);

        let u = undo_latest_in(&snaps, false).unwrap().unwrap();
        assert_eq!(u.restored, vec![a.clone()]);
        assert_eq!(u.removed, vec![created.clone()]);
        assert_eq!(std::fs::read_to_string(&a).unwrap(), "a1");
        assert!(!created.exists());
        assert!(undo_latest_in(&snaps, false).unwrap().is_none());

        // An edit after the run is left alone unless forced.
        let s = take_snapshot_in(&snaps, &t).unwrap();
        std::fs::write(&b, "b2").unwrap();
        finish_snapshot_in(&snaps, &s).unwrap();
        std::fs::write(&b, "b3").unwrap();
        let u = undo_latest_in(&snaps, false).unwrap().unwrap();
        assert_eq!(u.skipped.len(), 1);
        assert_eq!(std::fs::read_to_string(&b).unwrap(), "b3");
        let u = undo_latest_in(&snaps, true).unwrap().unwrap();
        assert_eq!(u.restored, vec![b.clone()]);
        assert_eq!(std::fs::read_to_string(&b).unwrap(), "b1");
    }

    #[test]
    fn a_run_that_changes_nothing_leaves_no_snapshot() {
        let root = tempfile::tempdir().unwrap();
        let snaps = root.path().join("snapshots");
        let a = root.path().join("a");
        std::fs::write(&a, "x").unwrap();
        let s = take_snapshot_in(&snaps, &Targets { files: vec![a], dirs: vec![] }).unwrap();
        assert!(finish_snapshot_in(&snaps, &s).unwrap().is_empty());
        assert!(list_snapshots(&snaps).is_empty());
    }
}
