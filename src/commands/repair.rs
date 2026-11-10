// `claude-profiles repair [name]`: when a launcher stops responding to
// double-clicks although `open` on it still works.
//
// The usual cause is a stale LaunchServices entry. Re-registering the bundle
// fixes it; so does giving it a unique bundle ID, clearing a quarantine flag,
// and restarting the Dock so a pinned tile picks the change up. Harmless on a
// healthy launcher.

use anyhow::Result;

use super::{fail, lsregister};
use crate::desktop::{is_ui_element, set_bundle_id, set_ui_element, strip_asset_catalog, strip_quarantine, unique_bundle_id};
use crate::registry;
use crate::ui::{self, blank, command, err, header, info, ok, path_str, step, warn};
use crate::util::{file_exists, is_mac, run_quiet, tildify, LSREGISTER};

pub fn run(args: &[String]) -> Result<()> {
    header("Repair a profile launcher");
    if !is_mac() {
        fail("repair is a macOS-only command (LaunchServices doesn't exist elsewhere).");
    }

    let name = match args.first() {
        Some(n) => n.clone(),
        None => {
            let reg = registry::load();
            let items: Vec<(String, String)> = reg
                .profiles
                .iter()
                .filter_map(|p| p.desktop.as_ref().map(|d| (format!("{} {}", p.name, path_str(tildify(&d.app_path))), p.name.clone())))
                .collect();
            if items.is_empty() {
                warn("No Desktop profiles configured. Nothing to repair.");
                info(format!("Create one with {}.", command("claude-profiles add")));
                return Ok(());
            }
            ui::select("Which profile's launcher do you want to repair?", &items, 0)?
        }
    };
    let Some(profile) = registry::find_profile(&name) else {
        err(format!("Profile \"{name}\" not found."));
        info(format!("Run {} to see configured profiles.", command("claude-profiles list")));
        std::process::exit(1);
    };
    let Some(d) = &profile.desktop else {
        info(format!("Profile \"{name}\" is a Code-only profile."));
        info("There's no Desktop launcher to repair.");
        info(format!("If `claude-{name}` isn't working in your terminal, check that your shell rc file is sourced."));
        return Ok(());
    };

    let app = &d.app_path;
    step(format!("Repairing launcher for \"{name}\""));
    info(format!("Launcher: {}", path_str(tildify(app))));
    if !file_exists(app) {
        err("Launcher .app not found on disk.");
        info(format!(
            "Re-run {} to recreate it (use the same profile name and paths to keep your data folder).",
            command("claude-profiles add")
        ));
        std::process::exit(1);
    }

    // Every osacompile'd launcher starts with the same bundle ID, and with
    // several of them LaunchServices routes Dock clicks unpredictably.
    let id = unique_bundle_id(&name);
    if set_bundle_id(app, &id) {
        ok(format!("Set unique bundle identifier: {id}"));
    } else {
        warn("Could not rewrite CFBundleIdentifier in Info.plist. The repair may be incomplete.");
    }
    if strip_quarantine(app) {
        ok("Cleared quarantine attribute (if any).");
    }
    if strip_asset_catalog(app) {
        ok("Removed the stock icon catalog so the Claude icon is used.");
    }
    if is_ui_element(app) == Some(true) && set_ui_element(app, false) {
        ok("Launcher restored to a normal app, so it can stay pinned in the Dock.");
    }

    if !file_exists(LSREGISTER) {
        err("lsregister not found at the expected path:");
        err(format!("  {LSREGISTER}"));
        fail("This is unexpected on macOS. The repair cannot proceed.");
    }
    if !lsregister("-f", app) {
        fail("lsregister failed.");
    }
    ok("Re-registered with LaunchServices.");

    if run_quiet("/usr/bin/touch", [app.as_os_str()]) {
        ok("Refreshed bundle modification time (icon cache hint).");
    } else {
        warn("Could not touch the .app, but lsregister succeeded. The fix should still work.");
    }

    // The Dock keeps its own cache of what a pinned tile points at; restarting
    // it (macOS brings it straight back) makes it re-read the registration.
    info("Refreshing the Dock (it'll briefly disappear and reappear)...");
    if run_quiet("/usr/bin/killall", ["Dock"]) {
        ok("Dock refreshed.");
    } else {
        warn("Could not refresh the Dock. If the icon is still unresponsive, drag it off the Dock and re-add the launcher.");
    }

    blank();
    ok(format!("Done. Try double-clicking {} now.", path_str(tildify(app))));
    info("Pin it from Finder rather than dragging the running window's tile: that tile belongs to Claude itself, so pinning it would launch the shared Claude rather than this profile.");
    info("If it still doesn't launch, log out and log back in to force a full LaunchServices reset.");
    Ok(())
}
