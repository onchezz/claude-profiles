// `claude-profiles self-update [name] [on|off]`: who updates a profile's
// copy of Claude.
//
//   off (default)  The copy's own updater is blocked; its launcher rebuilds it
//                  from the installed Claude on the first click after that
//                  updates.
//   on             Claude updates the copy itself. Every such update
//                  relaunches it on the default account and drops its colour,
//                  which is why it is not the default.

use anyhow::Result;

use super::fail;
use crate::registry::{self, Profile};
use crate::ui::{command, header, info, ok, path_str, warn};
use crate::updates::{block_updates, unblock_updates, update_block_state, wants_update_block, BlockResult, BlockState};

fn describe(p: &Profile) -> &'static str {
    let d = p.desktop.as_ref().expect("Desktop profiles only");
    if !wants_update_block(p) {
        return "on: Claude updates this copy itself";
    }
    match update_block_state(&d.data_dir) {
        BlockState::Blocked => "off: its launcher keeps it current",
        BlockState::Foreign(_) => "its own Claude configuration is in charge (left alone)",
        BlockState::Absent => "off, but not in place yet: run `claude-profiles doctor --fix`",
    }
}

pub fn run(args: &[String]) -> Result<()> {
    header("Profile self-updates");
    let reg = registry::load();
    let desktops: Vec<&Profile> = reg.profiles.iter().filter(|p| p.desktop.is_some()).collect();

    let Some(name) = args.first() else {
        if desktops.is_empty() {
            info("No Desktop profiles configured.");
            return Ok(());
        }
        for p in desktops {
            println!("  {}  {}", path_str(&p.name), describe(p));
        }
        println!();
        info(format!("Change one with {}.", command("claude-profiles self-update <name> on|off")));
        return Ok(());
    };

    let Some(profile) = reg.find(name).filter(|p| p.desktop.is_some()).cloned() else {
        fail(format!("No Desktop profile called \"{name}\"."));
    };
    let Some(choice) = args.get(1) else {
        println!("  {}  {}", path_str(&profile.name), describe(&profile));
        return Ok(());
    };
    let data_dir = profile.desktop.as_ref().map(|d| d.data_dir.clone()).expect("filtered above");

    match choice.as_str() {
        "on" => {
            unblock_updates(&data_dir)?;
            let mut next = profile.clone();
            if let Some(d) = next.desktop.as_mut() {
                d.self_update = Some(true);
            }
            registry::replace(&profile.name, next)?;
            ok(format!("{}: Claude now updates this copy itself.", profile.name));
            warn("Each of those updates relaunches it on your default account and removes its colour.");
            info(format!(
                "Quit it and reopen from its launcher when that happens. {} reports it.",
                command("claude-profiles doctor")
            ));
        }
        "off" => {
            let r = block_updates(&data_dir)?;
            let mut next = profile.clone();
            if let Some(d) = next.desktop.as_mut() {
                d.self_update = None;
            }
            registry::replace(&profile.name, next)?;
            if r == BlockResult::Foreign {
                warn(format!("{} has its own Claude configuration in that folder, so it was left alone.", profile.name));
                info("Its launcher still keeps the copy current; whether the copy also updates itself is up to that configuration.");
                return Ok(());
            }
            ok(format!("{}: its launcher now keeps this copy current.", profile.name));
        }
        other => fail(format!("Expected \"on\" or \"off\", got \"{other}\".")),
    }
    info("Takes effect the next time the profile starts.");
    Ok(())
}
