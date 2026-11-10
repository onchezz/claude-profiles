// `claude-profiles remove [name]`: tear down a profile.
//
// The launcher, the profile's copy of Claude, the alias and the registry entry
// go. The data folders stay unless you say otherwise, each behind its own
// question that defaults to no, so chats and settings can be recovered.

use std::path::Path;

use anyhow::Result;

use super::lsregister;
use crate::appclone::{clone_is_running, clone_path_for, remove_clone};
use crate::code::{delete_keychain_login, profile_shell, remove_alias};
use crate::desktop::data_dir_in_use;
use crate::permissions::{resync_deny_rules, strip_managed_deny_rules};
use crate::registry::{self, Profile};
use crate::ui::{self, blank, dim, explain, header, info, ok, path_str, step, warn};
use crate::updates::unblock_updates;
use crate::util::{file_exists, remove_all, tildify};

/// The registry is a plain file the README invites people to edit, so the
/// launcher path in it is checked before anything is deleted without a
/// question: it must be an .app bundle this tool built.
pub fn is_our_launcher(app: &Path, name: &str) -> bool {
    app.extension().is_some_and(|e| e == "app")
        && crate::desktop::get_bundle_id(app).as_deref() == Some(crate::desktop::unique_bundle_id(name).as_str())
}

pub fn run(args: &[String]) -> Result<()> {
    header("Remove a Claude profile");
    let reg = registry::load();
    if reg.profiles.is_empty() {
        warn("No profiles configured.");
        return Ok(());
    }

    let target = match args.first() {
        Some(n) => n.clone(),
        None => {
            let items: Vec<(String, String)> = reg.profiles.iter().map(|p| (format!("{} ({})", p.name, p.kind), p.name.clone())).collect();
            ui::select("Which profile do you want to remove?", &items, 0)?
        }
    };
    let Some(profile) = reg.find(&target).cloned() else {
        warn(format!("Profile \"{target}\" not found. Run `claude-profiles list` to see options."));
        return Ok(());
    };

    // Removal deletes the profile's copy of Claude, and deleting an app while
    // it runs can crash it.
    if let Some(d) = &profile.desktop {
        if clone_is_running(&clone_path_for(&profile.name)) || data_dir_in_use(&d.data_dir) {
            warn(format!("\"{}\" is open in Claude Desktop.", profile.name));
            info("Quit it first (Claude > Quit Claude in that window), then run remove again.");
            return Ok(());
        }
    }

    explain(
        "
        Removal does the following:

          - Deletes the launcher .app (Desktop profiles only)
          - Removes the shell alias from your rc file (Code profiles only)
          - Removes the entry from the registry

        By default, the data folder (chat history, settings, MCP config) is
        LEFT IN PLACE so you can recover it later. You'll be asked separately
        if you want to delete it too; deleting it also clears what goes with
        it (Desktop's -3p folder, Claude Code's Keychain sign-in).
        ",
    );
    if !ui::confirm(&format!("Remove profile \"{}\"?", profile.name), false)? {
        warn("Cancelled.");
        return Ok(());
    }
    // Both questions before anything is deleted, so an answer cannot be
    // regretted halfway through.
    let wipe_desktop = match &profile.desktop {
        Some(d) => ui::confirm(&format!("Also delete the Desktop data folder ({})? This is permanent.", tildify(&d.data_dir)), false)?,
        None => false,
    };
    let wipe_code = match &profile.code {
        Some(c) => ui::confirm(&format!("Also delete the Code config folder ({})? This is permanent.", tildify(&c.config_dir)), false)?,
        None => false,
    };
    remove_profile(&profile, wipe_desktop, wipe_code)?;

    // With the folder kept, the sign-in stays too, so re-adding the
    // profile with the same folder picks up where it left off.
    if profile.code.is_some() && !wipe_code {
        blank();
        info("Its Claude Code sign-in stays in your Keychain alongside the kept folder.");
        println!("  {}", dim("Delete the Code config folder too and it goes with it."));
    }
    Ok(())
}

/// Tear a profile down. The caller has checked it is not open and asked
/// about the data folders.
pub fn remove_profile(profile: &Profile, wipe_desktop: bool, wipe_code: bool) -> Result<()> {
    teardown(profile, wipe_desktop, wipe_code)?;
    Ok(())
}

/// Remove one half of a two-part profile and keep the other: the Desktop
/// launcher and copy, or the Code alias, plus that half's data if asked.
pub fn remove_part(profile: &Profile, desktop: bool, wipe: bool) -> Result<()> {
    if profile.desktop.is_none() || profile.code.is_none() {
        anyhow::bail!("\"{}\" has only one part. Remove the whole profile instead.", profile.name);
    }
    let (mut gone, mut kept) = (profile.clone(), profile.clone());
    if desktop {
        gone.code = None;
        kept.desktop = None;
    } else {
        gone.desktop = None;
        kept.code = None;
    }
    teardown(&gone, desktop && wipe, !desktop && wipe)?;
    kept.kind = Profile::kind_for(kept.desktop.is_some(), kept.code.is_some()).into();
    registry::replace(&profile.name, kept)?;
    ok(format!("Removed the {} part of \"{}\".", if desktop { "Claude Desktop" } else { "Claude Code" }, profile.name));
    resync_deny_rules(&registry::load(), true);
    Ok(())
}

fn teardown(profile: &Profile, wipe_desktop: bool, wipe_code: bool) -> Result<()> {
    if let Some(d) = &profile.desktop {
        step("Removing Desktop launcher");
        if !file_exists(&d.app_path) {
            info("Launcher already gone, skipping.");
        } else if !is_our_launcher(&d.app_path, &profile.name) {
            warn(format!(
                "{} is not a launcher this tool built (no .app, or a different bundle ID), so it was left alone.",
                tildify(&d.app_path)
            ));
            info("  Delete it yourself if it is the one you meant.");
        } else {
            // Unregister BEFORE deleting: afterwards lsregister cannot resolve
            // the bundle, and the stale entry lingers.
            if lsregister("-u", &d.app_path) {
                ok("Unregistered launcher from LaunchServices.");
            } else {
                warn("Could not unregister from LaunchServices (continuing).");
            }
            match remove_all(&d.app_path) {
                Ok(()) => ok(format!("Deleted {}.", path_str(tildify(&d.app_path)))),
                Err(e) => warn(format!("Could not delete launcher: {e}")),
            }
        }
        // The copy holds no user data and is entirely derived from Claude.app.
        if remove_clone(&profile.name).unwrap_or(false) {
            ok("Removed this profile's copy of Claude.");
        }
        if wipe_desktop {
            if file_exists(&d.data_dir) {
                match remove_all(&d.data_dir) {
                    Ok(()) => ok(format!("Deleted {}.", path_str(tildify(&d.data_dir)))),
                    Err(e) => warn(format!("Could not delete data folder: {e}")),
                }
            }
            // Claude Desktop keeps a small sibling folder beside each data
            // folder for its third-party settings.
            let mut sibling = d.data_dir.clone().into_os_string();
            sibling.push("-3p");
            let sibling = std::path::PathBuf::from(sibling);
            if sibling.is_dir() {
                match remove_all(&sibling) {
                    Ok(()) => ok(format!("Deleted {}.", path_str(tildify(&sibling)))),
                    Err(e) => warn(format!("Could not delete {}: {e}", tildify(&sibling))),
                }
            }
            // The update settings go with the data folder, and only what the
            // tool wrote.
            match unblock_updates(&d.data_dir) {
                Ok(true) => ok("Removed its update settings."),
                Ok(false) => {}
                Err(e) => warn(format!("Could not remove its update settings: {e}")),
            }
        }
    }

    if let Some(c) = &profile.code {
        step("Removing Code alias");
        let (_, rc) = profile_shell(profile);
        remove_alias(&c.alias_name, &rc)?;
        ok(format!("Alias removed from {}.", path_str(tildify(&rc))));
        if wipe_code {
            if file_exists(&c.config_dir) {
                match remove_all(&c.config_dir) {
                    Ok(()) => ok(format!("Deleted {}.", path_str(tildify(&c.config_dir)))),
                    Err(e) => warn(format!("Could not delete config folder: {e}")),
                }
            }
            // Its sign-in is useless without the folder.
            if delete_keychain_login(&c.config_dir) {
                ok("Removed its saved sign-in from the Keychain.");
            }
        } else if strip_managed_deny_rules(profile) {
            // The kept folder would otherwise block reads of profiles it no
            // longer has anything to do with.
            ok("Removed this tool's read-protection rules from the kept folder.");
        }
    }

    registry::remove(&profile.name)?;
    ok(format!("Profile \"{}\" removed.", profile.name));
    resync_deny_rules(&registry::load(), true);
    Ok(())
}
