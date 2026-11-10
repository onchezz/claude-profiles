// Stopping a profile's copy of Claude from updating itself.
//
// Each Desktop profile runs its own copy of Claude.app, and Claude's updater
// runs inside that copy. An update replaces the whole app and relaunches it
// WITHOUT the launcher's --user-data-dir, so the new window is on the default
// account while looking like the profile, and the colour is lost.
//
// Claude's "Block auto-updates" policy (`disableAutoUpdates`) can be set per
// profile: a profile launched with --user-data-dir=<dir> reads it from
// <dir>-3p/configLibrary/. That folder is also where Claude keeps a user's own
// third-party inference setup, so the tool only writes it when it does not
// exist, marks what it wrote, and never edits a configuration it did not
// create.

use std::path::{Path, PathBuf};

use anyhow::Result;
use serde_json::json;

use crate::registry::Profile;
use crate::util::{file_exists, read_json, remove_all, to_json_pretty, with_suffix};

const MARKER: &str = ".claude-profiles";

/// Mirrors Claude's own rule: a data folder already ending in -3p is used as is.
pub fn local_config_root_for(data_dir: &Path) -> PathBuf {
    if data_dir.to_string_lossy().ends_with("-3p") {
        data_dir.to_path_buf()
    } else {
        with_suffix(data_dir, "-3p")
    }
}

fn library_dir(data_dir: &Path) -> PathBuf {
    local_config_root_for(data_dir).join("configLibrary")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockState {
    /// The tool's own block is in place.
    Blocked,
    /// No local configuration; the copy updates itself.
    Absent,
    /// A configuration the tool did not write, probably the user's
    /// third-party inference setup. Never touched. The bool says whether it
    /// happens to block updates anyway.
    Foreign(bool),
}

pub fn update_block_state(data_dir: &Path) -> BlockState {
    let lib = library_dir(data_dir);
    let Some(meta) = read_json(&lib.join("_meta.json")) else { return BlockState::Absent };
    let applied = meta.get("appliedId").and_then(|v| v.as_str()).unwrap_or("");
    let blocked = !applied.is_empty()
        && read_json(&lib.join(format!("{applied}.json")))
            .and_then(|e| e.get("disableAutoUpdates").cloned())
            .is_some_and(|v| v == true);
    let ours = read_json(&local_config_root_for(data_dir).join(MARKER))
        .and_then(|m| m.get("entryId").and_then(|v| v.as_str()).map(String::from))
        .is_some_and(|id| !id.is_empty() && id == applied);
    match (ours, blocked) {
        (true, true) => BlockState::Blocked,
        (true, false) => BlockState::Absent,
        (false, b) => BlockState::Foreign(b),
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum BlockResult {
    Created,
    Already,
    /// Left alone: not the tool's configuration.
    Foreign,
}

pub fn block_updates(data_dir: &Path) -> Result<BlockResult> {
    match update_block_state(data_dir) {
        BlockState::Blocked => return Ok(BlockResult::Already),
        BlockState::Foreign(_) => return Ok(BlockResult::Foreign),
        BlockState::Absent => {}
    }
    let root = local_config_root_for(data_dir);
    let lib = library_dir(data_dir);
    // "Absent" can still be the tool's own marker with a damaged entry; start
    // clean only in a folder that is ours or empty.
    let ours = file_exists(root.join(MARKER));
    if file_exists(&lib) && !ours {
        return Ok(BlockResult::Foreign);
    }
    if ours {
        remove_all(&lib)?;
    }
    std::fs::create_dir_all(&lib)?;
    let id = uuid::Uuid::new_v4().to_string();
    // The same shape Claude writes for itself: one applied entry, "Default".
    std::fs::write(lib.join(format!("{id}.json")), to_json_pretty(&json!({ "disableAutoUpdates": true })))?;
    std::fs::write(
        lib.join("_meta.json"),
        format!("{}\n", json!({ "appliedId": id, "entries": [{ "id": id, "name": "Default" }] })),
    )?;
    std::fs::write(
        root.join(MARKER),
        to_json_pretty(&json!({
            "entryId": id,
            "writtenBy": "claude-profiles",
            "why": "blocks this profile's copy of Claude from updating itself"
        })),
    )?;
    Ok(BlockResult::Created)
}

/// Removes only what the tool wrote. True if anything was removed.
pub fn unblock_updates(data_dir: &Path) -> Result<bool> {
    let root = local_config_root_for(data_dir);
    if !file_exists(root.join(MARKER)) {
        return Ok(false);
    }
    if matches!(update_block_state(data_dir), BlockState::Foreign(_)) {
        return Ok(false);
    }
    remove_all(&library_dir(data_dir))?;
    remove_all(&root.join(MARKER))?;
    // Leave the -3p folder if Claude has put anything else in it.
    if std::fs::read_dir(&root).map(|mut d| d.next().is_none()).unwrap_or(false) {
        let _ = std::fs::remove_dir(&root);
    }
    Ok(true)
}

/// For rename: Claude finds the block by the data folder's name, so it moves
/// with the folder. A configuration the tool did not write is the user's.
pub fn move_update_block(old_data_dir: &Path, new_data_dir: &Path) -> Result<bool> {
    let from = local_config_root_for(old_data_dir);
    let to = local_config_root_for(new_data_dir);
    if !file_exists(from.join(MARKER)) || file_exists(&to) {
        return Ok(false);
    }
    std::fs::rename(from, to)?;
    Ok(true)
}

/// Blocking is the default; `self-update on` records the choice not to.
pub fn wants_update_block(profile: &Profile) -> bool {
    profile.desktop.as_ref().is_some_and(|d| d.self_update != Some(true))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_the_verified_layout_once() {
        let root = tempfile::tempdir().unwrap();
        let data = root.path().join("Claude-W");
        assert_eq!(update_block_state(&data), BlockState::Absent);
        assert_eq!(block_updates(&data).unwrap(), BlockResult::Created);
        assert_eq!(update_block_state(&data), BlockState::Blocked);
        assert_eq!(block_updates(&data).unwrap(), BlockResult::Already);
        let lib = root.path().join("Claude-W-3p/configLibrary");
        let meta = read_json(&lib.join("_meta.json")).unwrap();
        let id = meta["appliedId"].as_str().unwrap();
        assert_eq!(read_json(&lib.join(format!("{id}.json"))).unwrap()["disableAutoUpdates"], true);
        assert_eq!(meta["entries"][0]["name"], "Default");
    }

    #[test]
    fn never_edits_a_configuration_it_did_not_write() {
        let root = tempfile::tempdir().unwrap();
        let data = root.path().join("Claude-W");
        let lib = root.path().join("Claude-W-3p/configLibrary");
        std::fs::create_dir_all(&lib).unwrap();
        std::fs::write(lib.join("_meta.json"), r#"{"appliedId":"u1","entries":[]}"#).unwrap();
        std::fs::write(lib.join("u1.json"), r#"{"inferenceProvider":"bedrock"}"#).unwrap();
        assert_eq!(update_block_state(&data), BlockState::Foreign(false));
        assert_eq!(block_updates(&data).unwrap(), BlockResult::Foreign);
        assert!(!unblock_updates(&data).unwrap());
        assert_eq!(std::fs::read_to_string(lib.join("u1.json")).unwrap(), r#"{"inferenceProvider":"bedrock"}"#);
    }

    #[test]
    fn unblock_and_move_act_on_the_tools_own_block_only() {
        let root = tempfile::tempdir().unwrap();
        let data = root.path().join("Claude-A");
        block_updates(&data).unwrap();
        let moved = root.path().join("Claude-B");
        assert!(move_update_block(&data, &moved).unwrap());
        assert_eq!(update_block_state(&moved), BlockState::Blocked);
        assert!(unblock_updates(&moved).unwrap());
        assert!(!root.path().join("Claude-B-3p").exists());
    }

    #[test]
    fn config_root_naming() {
        assert_eq!(local_config_root_for(Path::new("/x/Claude-W")), PathBuf::from("/x/Claude-W-3p"));
        assert_eq!(local_config_root_for(Path::new("/x/Claude-3p")), PathBuf::from("/x/Claude-3p"));
    }
}
