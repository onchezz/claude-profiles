// `claude-profiles extensions`: copy Claude Desktop extensions and MCP
// connectors from one install (the default, or any profile) into a profile.
//
// An extension is two pieces that must travel together: a folder under
// `Claude Extensions/<id>/` and its settings in
// `Claude Extensions Settings/<id>.json`.
//
// Profiles start without extensions by design; this is the way to bring the
// ones you want across without reinstalling each by hand. Source and target
// are both picked from menus, so there is no name to mistype.

use std::path::{Path, PathBuf};

use anyhow::{bail, Result};

use crate::appclone::{clone_is_running, clone_path_for};
use crate::claudebin::{default_desktop_data_dir, detect_defaults};
use crate::desktop::{data_dir_in_use, desktop_config_file, mcp_server_names, seed_desktop_mcp_servers, SeedStatus};
use crate::registry::{self, Profile};
use crate::ui::{self, blank, command, dim, err, header, info, ok, path_str, step, warn};
use crate::util::{clone_copy, file_exists, file_stays_within_dir, remove_all, tildify};

const EXT_DIR: &str = "Claude Extensions";
const EXT_SETTINGS_DIR: &str = "Claude Extensions Settings";

#[derive(Clone)]
pub struct Extension {
    pub id: String,
    pub folder: PathBuf,
    pub settings: PathBuf,
    pub has_settings: bool,
}

fn list_extensions(data_dir: &Path) -> Vec<Extension> {
    let ext_dir = data_dir.join(EXT_DIR);
    let settings_dir = data_dir.join(EXT_SETTINGS_DIR);
    let Ok(rd) = std::fs::read_dir(&ext_dir) else { return Vec::new() };
    let mut ids: Vec<String> = rd
        .flatten()
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    ids.sort();
    ids.into_iter()
        .map(|id| {
            let settings = settings_dir.join(format!("{id}.json"));
            Extension { folder: ext_dir.join(&id), has_settings: file_exists(&settings), settings, id }
        })
        .collect()
}

/// Copy an extension's folder and settings into a profile. Never writes
/// through a link out of the target profile: a linked extensions folder
/// would otherwise receive the copy, keys and all, somewhere else.
pub fn copy_extension(ext: &Extension, target_data_dir: &Path) -> Result<()> {
    let ext_dir = target_data_dir.join(EXT_DIR);
    let settings_dir = target_data_dir.join(EXT_SETTINGS_DIR);
    std::fs::create_dir_all(&ext_dir)?;
    std::fs::create_dir_all(&settings_dir)?;
    let folder = ext_dir.join(&ext.id);
    let settings = settings_dir.join(format!("{}.json", ext.id));
    for p in [&folder, &settings] {
        if !file_stays_within_dir(p, target_data_dir) {
            bail!("{} resolves outside the profile; left it alone", p.display());
        }
    }
    // Replace rather than merge into a stale copy.
    remove_all(&folder)?;
    clone_copy(&ext.folder, &folder)?;
    if ext.has_settings {
        std::fs::copy(&ext.settings, &settings)?;
    }
    Ok(())
}

#[derive(Clone)]
struct Source {
    label: String,
    data_dir: PathBuf,
}

pub fn run(args: &[String]) -> Result<()> {
    header("Copy Claude Desktop extensions and connectors");
    let force = args.iter().any(|a| a == "--force");
    let defaults = detect_defaults();
    let reg = registry::load();
    let desktops: Vec<&Profile> = reg.profiles.iter().filter(|p| p.desktop.is_some()).collect();

    // Sources: the default install and every Desktop profile. Targets: the
    // profiles only; the default install is never written to.
    let mut sources: Vec<(String, Source)> = Vec::new();
    if defaults.desktop.is_some() {
        let d = default_desktop_data_dir();
        sources.push((
            format!("default install {}", dim(format!("({})", tildify(&d)))),
            Source { label: "default install".into(), data_dir: d },
        ));
    }
    for p in &desktops {
        let d = &p.desktop.as_ref().expect("filtered").data_dir;
        sources.push((format!("{} {}", p.name, dim(format!("({})", tildify(d)))), Source { label: p.name.clone(), data_dir: d.clone() }));
    }
    if sources.is_empty() {
        err("No Claude Desktop installs found.");
        info(format!(
            "Add a Desktop profile with {}, or launch the default Claude Desktop at least once.",
            command("claude-profiles add")
        ));
        std::process::exit(1);
    }
    if desktops.is_empty() {
        err("No Desktop profiles configured to copy into.");
        info(format!("Create one first with {}.", command("claude-profiles add")));
        std::process::exit(1);
    }

    let source = ui::select("Copy FROM:", &sources, 0)?;
    let targets: Vec<(String, (Source, String))> = desktops
        .iter()
        .filter(|p| p.desktop.as_ref().is_some_and(|d| d.data_dir != source.data_dir))
        .map(|p| {
            let d = &p.desktop.as_ref().expect("filtered").data_dir;
            (
                format!("{} {}", p.name, dim(format!("({})", tildify(d)))),
                (Source { label: p.name.clone(), data_dir: d.clone() }, p.name.clone()),
            )
        })
        .collect();
    if targets.is_empty() {
        err("No eligible target profiles. (Need at least one Desktop profile other than the source.)");
        info(format!("Create another Desktop profile with {}.", command("claude-profiles add")));
        std::process::exit(1);
    }
    let (target, target_name) = ui::select("Copy TO:", &targets, 0)?;

    step(format!("Copying: {} → {}", source.label, target.label));
    info(format!("Source: {}", path_str(tildify(&source.data_dir))));
    info(format!("Target: {}", path_str(tildify(&target.data_dir))));

    let exts = list_extensions(&source.data_dir);
    let connectors = mcp_server_names(&desktop_config_file(&source.data_dir)).unwrap_or_default();
    if exts.is_empty() && connectors.is_empty() {
        warn(format!("No extensions or MCP connectors found in {}.", source.label));
        info("Set them up there first, then re-run this command.");
        return Ok(());
    }
    if !exts.is_empty() {
        copy_extensions_step(&exts, &target, force)?;
    }
    if !connectors.is_empty() {
        copy_connectors_step(&connectors, &source, &target, &target_name)?;
    }
    info("Restart that profile's Claude Desktop to pick up what was copied.");
    Ok(())
}

fn copy_extensions_step(exts: &[Extension], target: &Source, force: bool) -> Result<()> {
    let have: Vec<String> = list_extensions(&target.data_dir).into_iter().map(|e| e.id).collect();
    blank();
    let choices: Vec<(String, String, bool)> = exts
        .iter()
        .map(|e| {
            let conflict = have.contains(&e.id);
            let mut tags = Vec::new();
            if conflict {
                tags.push("already in target");
            }
            if !e.has_settings {
                tags.push("no settings file");
            }
            let suffix = if tags.is_empty() { String::new() } else { dim(format!(" ({})", tags.join(", "))) };
            (format!("{}{suffix}", e.id), e.id.clone(), !conflict)
        })
        .collect();
    let selected = ui::checkbox("Which extensions to copy? (space to toggle, enter to confirm)", &choices)?;
    if selected.is_empty() {
        warn("No extensions selected. Nothing to do.");
        return Ok(());
    }

    let conflicts: Vec<&String> = selected.iter().filter(|id| have.contains(id)).collect();
    let mut overwrite = force;
    if !conflicts.is_empty() && !force {
        blank();
        warn(format!("{} of the selected extensions already exist in the target profile:", conflicts.len()));
        for id in &conflicts {
            println!("    {id}");
        }
        blank();
        overwrite = ui::confirm("Overwrite the existing copies in the target? (Their current settings will be lost.)", false)?;
    }

    step("Copying");
    let (mut copied, mut skipped) = (0, 0);
    for id in &selected {
        let Some(ext) = exts.iter().find(|e| &e.id == id) else { continue };
        if have.contains(id) && !overwrite {
            info(format!("Skipped {id} (already exists, no overwrite)."));
            skipped += 1;
            continue;
        }
        match copy_extension(ext, &target.data_dir) {
            Ok(()) => {
                ok(format!("Copied {id}{}", if ext.has_settings { "" } else { " (no settings file)" }));
                copied += 1;
            }
            Err(e) => err(format!("Failed to copy {id}: {e}")),
        }
    }
    blank();
    ok(format!("Extensions: copied {copied}{}.", if skipped > 0 { format!(", skipped {skipped}") } else { String::new() }));
    Ok(())
}

/// Picked one by one, none pre-selected: a connector's settings often hold
/// the source account's credentials. One the target already has is never
/// replaced.
fn copy_connectors_step(names: &[String], source: &Source, target: &Source, target_name: &str) -> Result<()> {
    step("MCP connectors");
    let already = mcp_server_names(&desktop_config_file(&target.data_dir)).unwrap_or_default();
    let offer: Vec<&String> = names.iter().filter(|n| !already.contains(n)).collect();
    if offer.is_empty() {
        ok(format!("{} already has every connector {} has.", target.label, source.label));
        return Ok(());
    }
    // Claude rewrites this file while it runs, so a copy into an open profile
    // would be lost at its next save.
    if clone_is_running(&clone_path_for(target_name)) || data_dir_in_use(&target.data_dir) {
        warn(format!("{} is open in Claude Desktop, so connectors were not copied.", target.label));
        info("Claude would overwrite them. Quit it, then run this command again.");
        return Ok(());
    }
    for n in names.iter().filter(|n| already.contains(n)) {
        info(format!("{n}: already in target, left as is."));
    }
    info("Each connector's settings are copied as they are, including any API keys or tokens in them.");
    let choices: Vec<(String, String, bool)> = offer.iter().map(|n| (n.to_string(), n.to_string(), false)).collect();
    let selected = ui::checkbox("Which connectors to copy? (space to toggle, enter to confirm)", &choices)?;
    if selected.is_empty() {
        info("No connectors selected.");
        return Ok(());
    }
    let r = seed_desktop_mcp_servers(&target.data_dir, &desktop_config_file(&source.data_dir), Some(&selected));
    match r.status {
        SeedStatus::Seeded => ok(format!("Connectors: copied {}.", r.added.join(", "))),
        SeedStatus::UnsafePath => warn(format!(
            "{}'s claude_desktop_config.json resolves outside the profile or has a broken link, so it was left alone.",
            target.label
        )),
        SeedStatus::Foreign => warn(format!("{}'s claude_desktop_config.json is not valid JSON, so it was left alone.", target.label)),
        _ => info("Nothing new to copy."),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refuses_an_extensions_folder_linked_out_of_the_profile() {
        let root = tempfile::tempdir().unwrap();
        let src = root.path().join("src");
        std::fs::create_dir_all(src.join(EXT_DIR).join("ext.a")).unwrap();
        let target = root.path().join("Claude-T");
        let outside = root.path().join("outside");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, target.join(EXT_DIR)).unwrap();
        let ext = list_extensions(&src).remove(0);
        assert!(copy_extension(&ext, &target).is_err());
        assert!(std::fs::read_dir(&outside).unwrap().next().is_none());
    }

    #[test]
    fn copies_folder_and_settings_together() {
        let root = tempfile::tempdir().unwrap();
        let src = root.path().join("src");
        std::fs::create_dir_all(src.join(EXT_DIR).join("ext.a")).unwrap();
        std::fs::write(src.join(EXT_DIR).join("ext.a/manifest.json"), "{}").unwrap();
        std::fs::create_dir_all(src.join(EXT_SETTINGS_DIR)).unwrap();
        std::fs::write(src.join(EXT_SETTINGS_DIR).join("ext.a.json"), r#"{"k":1}"#).unwrap();
        let target = root.path().join("Claude-T");
        std::fs::create_dir_all(&target).unwrap();
        let ext = list_extensions(&src).remove(0);
        assert!(ext.has_settings);
        copy_extension(&ext, &target).unwrap();
        assert!(target.join(EXT_DIR).join("ext.a/manifest.json").exists());
        assert!(target.join(EXT_SETTINGS_DIR).join("ext.a.json").exists());
    }
}
