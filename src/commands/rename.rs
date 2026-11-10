// `claude-profiles rename [old] [new]`: rename a profile end to end.
//
// A name lives in the Code config folder, the alias, the Desktop data folder,
// the launcher and its bundle ID, the profile's copy of Claude, the registry,
// and every other profile's deny rules. This moves them together.
//
// Claude Code keys its Keychain sign-in by the config folder's path, so the
// sign-in is moved to the new path's entry alongside the folder. If that
// fails the user is told and signs in again. Desktop sign-ins live in the
// folder that moves.

use std::path::Path;

use anyhow::Result;

use super::{fail, lsregister, reserved, without_claude_prefix};
use crate::appclone::{clone_is_running, clone_path_for};
use crate::code::{move_keychain_login, add_alias, default_alias_name_for, default_config_dir_for, default_gh_config_dir_for, profile_shell, remove_alias};
use crate::desktop::{
    compile_app, copy_claude_icon, data_dir_in_use, default_app_path_for, default_data_dir_for, launch_target_for, CompileSpec,
};
use crate::permissions::resync_deny_rules;
use crate::registry::{self, CodeTarget, DesktopTarget, Profile};
use crate::ui::{self, blank, command, err, explain, header, info, ok, path_str, step, warn};
use crate::updates::move_update_block;
use crate::util::{file_exists, remove_all, sanitize_name, tildify};

/// Move a folder, creating its parent. False when there was nothing to move.
fn move_dir(from: &Path, to: &Path) -> Result<bool> {
    if !file_exists(from) {
        return Ok(false);
    }
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::rename(from, to)?;
    Ok(true)
}

pub struct RenamePlan {
    pub code: Option<CodeTarget>,
    pub desktop: Option<DesktopTarget>,
    /// (label, from, to) for each thing that moves.
    pub moves: Vec<(&'static str, String, String)>,
}

/// Only paths still at the DEFAULT location for the old name move. A path
/// the user chose stays where it is: they picked it, and it probably does not
/// encode the name.
pub fn plan_rename_paths(p: &Profile, old: &str, new: &str) -> RenamePlan {
    let mut moves = Vec::new();
    let mut code = p.code.clone();
    let mut desktop = p.desktop.clone();
    if let Some(c) = code.as_mut() {
        if c.config_dir == default_config_dir_for(old) {
            let to = default_config_dir_for(new);
            moves.push(("Code config", c.config_dir.display().to_string(), to.display().to_string()));
            c.config_dir = to;
        }
        if c.alias_name == default_alias_name_for(old) {
            let to = default_alias_name_for(new);
            moves.push(("Shell alias", c.alias_name.clone(), to.clone()));
            c.alias_name = to;
        }
    }
    if let Some(d) = desktop.as_mut() {
        if d.data_dir == default_data_dir_for(old) {
            let to = default_data_dir_for(new);
            moves.push(("Desktop data", d.data_dir.display().to_string(), to.display().to_string()));
            d.data_dir = to;
        }
        if d.app_path == default_app_path_for(old) {
            let to = default_app_path_for(new);
            moves.push(("Launcher app", d.app_path.display().to_string(), to.display().to_string()));
            d.app_path = to;
        }
    }
    RenamePlan { code, desktop, moves }
}

pub fn run(all_args: &[String]) -> Result<()> {
    header("Rename a profile");
    // --yes skips the confirmation (the menu bar app asks its own).
    let yes = all_args.iter().any(|a| a == "--yes");
    let args: Vec<String> = all_args.iter().filter(|a| *a != "--yes").cloned().collect();
    let reg = registry::load();
    if reg.profiles.is_empty() {
        warn("No profiles configured.");
        info(format!("Create one with {}.", command("claude-profiles add")));
        return Ok(());
    }

    let old = match args.first() {
        Some(n) => n.clone(),
        None => {
            let items: Vec<(String, String)> = reg.profiles.iter().map(|p| (format!("{} ({})", p.name, p.kind), p.name.clone())).collect();
            ui::select("Which profile do you want to rename?", &items, 0)?
        }
    };
    let Some(profile) = registry::find_profile(&old) else {
        err(format!("Profile \"{old}\" not found."));
        info(format!("Run {} to see configured profiles.", command("claude-profiles list")));
        std::process::exit(1);
    };

    let new_raw = match args.get(1) {
        Some(n) => n.clone(),
        None => ui::input("New name:", None, |v| {
            let c = sanitize_name(v);
            if c.is_empty() {
                return Err("Name cannot be empty.".into());
            }
            if c == old {
                return Err("That's the current name.".into());
            }
            if let Some(why) = reserved(&c) {
                return Err(format!("\"{c}\" is reserved. It would collide with {why}."));
            }
            if registry::find_profile(&c).is_some() {
                return Err(format!("Profile \"{c}\" already exists."));
            }
            if let Some(s) = without_claude_prefix(&c) {
                return Err(format!("Leave out \"claude-\": the tool adds it. Suggestion: \"{s}\""));
            }
            Ok(())
        })?,
    };
    let new = sanitize_name(&new_raw);

    if new == old {
        info("Name unchanged. Nothing to do.");
        return Ok(());
    }
    if new.is_empty() {
        fail("The new name has no usable characters.");
    }
    // Checked here as well as in the prompt: a name given on the command line
    // never goes through the prompt.
    if registry::find_profile(&new).is_some() {
        fail(format!("Profile \"{new}\" already exists."));
    }
    if let Some(why) = reserved(&new) {
        fail(format!("\"{new}\" is reserved. It would collide with {why}."));
    }
    if let Some(s) = without_claude_prefix(&new) {
        fail(format!("Leave out \"claude-\": the tool adds it, so \"{new}\" would become claude-{new}. Try \"{s}\"."));
    }
    // A running Claude keeps writing to the folder it started with, which
    // would split the profile across two folders.
    if let Some(d) = &profile.desktop {
        if clone_is_running(&clone_path_for(&old)) || data_dir_in_use(&d.data_dir) {
            err(format!("\"{old}\" is open in Claude Desktop."));
            info("Quit it first (Claude > Quit Claude in that window), then run the rename again.");
            std::process::exit(1);
        }
    }

    let plan = plan_rename_paths(&profile, &old, &new);
    step("Review");
    println!("  Profile: {} → {}\n", path_str(&old), path_str(&new));
    if plan.moves.is_empty() {
        info("All paths are custom; only the profile name and registry entry change.");
    } else {
        for (label, from, to) in &plan.moves {
            println!("  {label}:");
            println!("    {}", tildify(from));
            println!("    → {}\n", tildify(to));
        }
    }
    if profile.code.is_some() {
        explain(
            "
            Claude Code keeps its login in the macOS Keychain under a key derived
            from the config folder path. The login is moved to the new folder's
            key along with the folder. If that fails, you run /login once more.
            Your chats, skills, and MCP config all move normally.
            ",
        );
    }
    if !yes && !ui::confirm(&format!("Rename \"{old}\" to \"{new}\"?"), false)? {
        warn("Cancelled. Nothing was changed.");
        return Ok(());
    }

    step("Applying");
    let mut new_code = plan.code.clone();
    // Cleared once the Claude Code sign-in has been carried over.
    let mut signed_out = profile.code.as_ref().zip(plan.code.as_ref()).is_some_and(|(o, n)| o.config_dir != n.config_dir);
    let mut new_desktop = plan.desktop.clone();

    if let (Some(oc), Some(nc)) = (&profile.code, new_code.as_mut()) {
        if nc.config_dir != oc.config_dir {
            match move_dir(&oc.config_dir, &nc.config_dir) {
                Ok(true) => {
                    ok(format!("Moved config folder to {}.", path_str(tildify(&nc.config_dir))));
                    if move_keychain_login(&oc.config_dir, &nc.config_dir) {
                        ok("Moved its Claude Code sign-in along with it.");
                        signed_out = false;
                    }
                }
                Ok(false) => warn(format!("Config folder {} not found; skipped.", tildify(&oc.config_dir))),
                Err(e) => {
                    err(format!("Could not move config folder: {e}"));
                    fail("Aborting before any further changes. Nothing else was modified.");
                }
            }
        }
        let (shell, rc) = profile_shell(&profile);
        if nc.alias_name != oc.alias_name {
            remove_alias(&oc.alias_name, &rc)?;
        }
        // The gh folder lives inside the config folder, so it moved with it.
        if oc.gh_config_dir.is_some() {
            nc.gh_config_dir = Some(default_gh_config_dir_for(&nc.config_dir));
        }
        add_alias(&nc.alias_name, &nc.config_dir, nc.gh_config_dir.as_deref(), &shell, &rc)?;
        nc.rc_path = Some(rc.clone());
        nc.shell = Some(shell);
        ok(format!("Alias \"{}\" written to {}.", nc.alias_name, path_str(tildify(&rc))));
    }

    if let (Some(od), Some(nd)) = (&profile.desktop, new_desktop.as_mut()) {
        if nd.data_dir != od.data_dir {
            match move_dir(&od.data_dir, &nd.data_dir) {
                Ok(true) => ok(format!("Moved data folder to {}.", path_str(tildify(&nd.data_dir)))),
                Ok(false) => warn(format!("Data folder {} not found; skipped.", tildify(&od.data_dir))),
                Err(e) => fail(format!("Could not move data folder: {e}")),
            }
            // Claude finds the update block by the data folder's name.
            match move_update_block(&od.data_dir, &nd.data_dir) {
                Ok(true) => ok("Moved its update settings with it."),
                Ok(false) => {}
                Err(e) => {
                    warn(format!("Could not move the update settings: {e}"));
                    info(format!("Run {} to set them up again.", command("claude-profiles doctor --fix")));
                }
            }
        }

        // The copy is named after the profile; left behind it would be orphaned.
        let (old_clone, new_clone) = (clone_path_for(&old), clone_path_for(&new));
        if file_exists(&old_clone) && !file_exists(&new_clone) {
            match std::fs::rename(&old_clone, &new_clone) {
                Ok(()) => {
                    lsregister("-u", &old_clone);
                    lsregister("-f", &new_clone);
                    ok(format!("Moved its copy of Claude to {}.", path_str(tildify(&new_clone))));
                }
                Err(e) => {
                    warn(format!("Could not move its copy of Claude: {e}"));
                    info(format!("Run {} to build a new one.", command("claude-profiles doctor --fix")));
                }
            }
        }

        // Unregister and delete the old launcher before building the new one,
        // so LaunchServices does not keep an entry for a bundle that is gone.
        if file_exists(&od.app_path) && od.app_path != nd.app_path {
            if super::remove::is_our_launcher(&od.app_path, &old) {
                lsregister("-u", &od.app_path);
                if remove_all(&od.app_path).is_err() {
                    warn(format!("Could not delete the old launcher at {}.", tildify(&od.app_path)));
                }
            } else {
                warn(format!("{} is not a launcher this tool built, so it was left where it is.", tildify(&od.app_path)));
            }
        }

        // The rebuilt launcher keeps exporting the (possibly moved) Code and
        // gh folders.
        let target = launch_target_for(&new, od);
        let result = compile_app(&CompileSpec {
            name: &new,
            data_dir: &nd.data_dir,
            app_path: &nd.app_path,
            target: &target,
            code_config_dir: new_code.as_ref().map(|c| c.config_dir.as_path()),
            gh_config_dir: new_code.as_ref().and_then(|c| c.gh_config_dir.as_deref()),
        });
        match result {
            Ok(()) => {
                copy_claude_icon(&nd.app_path, &od.claude_app_path);
                ok(format!("Rebuilt launcher at {}.", path_str(tildify(&nd.app_path))));
            }
            Err(e) => {
                err(format!("Could not rebuild the launcher: {e:#}"));
                info(format!("You can recreate it with {} using the new name.", command("claude-profiles add")));
            }
        }
    }

    let mut renamed = profile.clone();
    renamed.name = new.clone();
    renamed.code = new_code.clone();
    renamed.desktop = new_desktop.take();
    registry::replace(&old, renamed)?;
    ok("Registry updated.");
    resync_deny_rules(&registry::load(), true);

    blank();
    ok(format!("Profile \"{old}\" is now \"{new}\"."));
    if let Some(c) = new_code.as_ref().filter(|_| signed_out) {
        blank();
        info("Reload your shell, then sign in again:");
        println!("  {}", command(crate::shell::reload_command(c.rc_path.as_deref().unwrap_or(Path::new("")))));
        println!("  {}", command(&c.alias_name));
        info("Run /login inside the REPL to re-authenticate this profile.");
    }
    if profile.desktop.is_some() {
        blank();
        info("If the old launcher was pinned to your Dock, drag it off and re-pin the new one.");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Map;

    #[test]
    fn default_located_paths_move_and_custom_ones_stay() {
        let p = Profile {
            name: "work".into(),
            kind: "both".into(),
            desktop: Some(DesktopTarget {
                data_dir: default_data_dir_for("work"),
                app_path: "/Custom/My Launcher.app".into(),
                claude_app_path: "/Applications/Claude.app".into(),
                color: None,
                self_update: None,
                extra: Map::new(),
            }),
            code: Some(CodeTarget {
                config_dir: "/elsewhere/cfg".into(),
                alias_name: "claude-work".into(),
                shell: None,
                rc_path: None,
                gh_config_dir: None,
                extra: Map::new(),
            }),
            created_at: String::new(),
            extra: Map::new(),
        };
        let plan = plan_rename_paths(&p, "work", "job");
        let labels: Vec<&str> = plan.moves.iter().map(|m| m.0).collect();
        assert_eq!(labels, vec!["Shell alias", "Desktop data"]);
        assert_eq!(plan.code.as_ref().unwrap().config_dir, Path::new("/elsewhere/cfg"));
        assert_eq!(plan.code.as_ref().unwrap().alias_name, "claude-job");
        assert_eq!(plan.desktop.as_ref().unwrap().data_dir, default_data_dir_for("job"));
        assert_eq!(plan.desktop.as_ref().unwrap().app_path, Path::new("/Custom/My Launcher.app"));
    }
}
