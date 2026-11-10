// `claude-profiles doctor [--fix] [--undo [--force]]`: diagnose the machine.
//
// `status` asks whether each profile's paperwork is in order. `doctor` asks
// whether the profiles will actually behave, which includes failures outside
// the registry: two `claude` binaries on PATH, a half-finished install, a
// profile on another tool's folder, a launcher that does not export the
// profile's Code folder, two profiles signed in as the same account, a copy of
// Claude started without its profile, drifted read protection.
//
// Read-only by default. `--fix` repairs what is safe to repair automatically,
// after taking a snapshot so `--undo` can put it back.

use std::collections::{HashMap, HashSet};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Result;
use serde_json::Value;

use super::lsregister;
use crate::appclone::{
    app_version, apply_color, clone_is_running, clone_path_for, clone_state, ensure_colored_clone, has_custom_icon,
    running_copies, CloneState,
};
use crate::claudebin::{default_claude_config_dir, detect_defaults, find_claude_app, output_with_timeout, resolve_binaries, resolve_claude};
use crate::code::{copied_transcripts, flipped_claude_md_excludes, gh_token_override, paths_into_other_profile, rebase_json_file, SETTINGS_REBASE_KEYS};
use crate::desktop::{
    compile_app, copy_claude_icon, default_desktop_config_file, desktop_account_uuid, desktop_config_file, get_bundle_id,
    get_bundle_name, has_asset_catalog, is_ui_element, launch_target_for, launcher_code_config_dir, launcher_gh_config_dir,
    launcher_uses_helper, launcher_waits_for_helper, mcp_server_names, refresh_launcher, set_bundle_id, set_bundle_name,
    set_ui_element, strip_asset_catalog, unique_bundle_id, CompileSpec, DEFAULT_APPLET_BUNDLE_ID, STAGED_LAUNCHER_NAME,
};
use crate::launcher::{helper_path, helper_state, install_helper, launch_log, recent_launch_log, refresh_leftovers, unfinished_refresh, HelperState};
use crate::permissions::{audit_deny_rules, read_protection_enabled, resync_deny_rules};
use crate::registry::{self, Health, Profile, Registry};
use crate::snapshot::{discard, finish_snapshot, fix_targets, take_snapshot, undo_latest};
use crate::state::{record_fix_pass, VERSION};
use crate::ui::{blank, command, dim, err, header, info, ok, path_str, step, warn};
use crate::updates::{block_updates, update_block_state, wants_update_block, BlockState};
use crate::util::{file_exists, file_stem, home, is_mac, path_string, plural, remove_all, run_output, tildify, with_suffix};

use super::status::AliasCache;

/// Folders under $HOME that belong to other tools. A profile on one of these
/// would offer that tool's data up for deletion on `remove`.
const FOREIGN_DIRS: &[(&str, &str)] =
    &[(".claude-mem", "claude-mem"), (".claude-profiles", "another tool also named claude-profiles"), (".claude-multiprofile", "claude-multiprofile")];

#[derive(Default)]
struct Tally {
    problems: usize,
    warnings: usize,
}

fn repair_hint(t: &mut Tally) {
    info(format!("  Repair with {}", command("claude-profiles doctor --fix")));
    t.warnings += 1;
}

/// Rebuild a profile's launcher with its current folders, icon included.
fn rebuild(p: &Profile) -> Result<()> {
    let d = p.desktop.as_ref().expect("Desktop profiles only");
    let target = launch_target_for(&p.name, d);
    compile_app(&CompileSpec {
        name: &p.name,
        data_dir: &d.data_dir,
        app_path: &d.app_path,
        target: &target,
        code_config_dir: p.code.as_ref().map(|c| c.config_dir.as_path()),
        gh_config_dir: p.code.as_ref().and_then(|c| c.gh_config_dir.as_deref()),
    })?;
    copy_claude_icon(&d.app_path, &d.claude_app_path);
    refresh_launcher(&d.app_path);
    Ok(())
}

// ---- The shared claude binary ---------------------------------------------------------------

fn check_claude_binary(t: &mut Tally) {
    step("Claude Code binary");
    let Some(bin) = resolve_claude() else {
        err("No `claude` on PATH.");
        info("Claude Code profiles cannot launch. Install Claude Code, then re-run.");
        t.problems += 1;
        return;
    };
    ok(format!("Resolved: {}", tildify(&bin.winner)));
    info(format!("Version:  {}", bin.version.as_deref().unwrap_or("unknown")));
    if !bin.shadowed.is_empty() {
        // The most common cause of "I upgraded and nothing changed".
        warn(format!("{} additional `claude` on PATH, shadowed by the one above:", bin.shadowed.len()));
        for p in &bin.shadowed {
            println!("      {}", dim(tildify(p)));
        }
        info("If the winning version looks stale, the shadowed copy may be the one you upgraded.");
        t.warnings += 1;
    }
}

// ---- A broken npm install of Claude Code ------------------------------------------------------
//
// Claude Code is usually installed with npm. A package folder left without its
// package.json de-registers the command; a postinstall that stopped partway
// leaves a perfect package.json in front of a command that cannot start. So
// the declared executable is checked, and run.

/// The first dotted-numeric token in `--version` output ("2.1.126 (Claude
/// Code)", "v1.2.3"), or None.
pub fn parse_reported_version(output: &str) -> Option<String> {
    let chars: Vec<char> = output.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i].is_ascii_digit() {
            let start = i;
            let mut dots = 0;
            let mut j = i;
            while j < chars.len() && (chars[j].is_ascii_digit() || (chars[j] == '.' && j + 1 < chars.len() && chars[j + 1].is_ascii_digit() && dots < 2)) {
                if chars[j] == '.' {
                    dots += 1;
                }
                j += 1;
            }
            let next_is_more = j < chars.len() && (chars[j].is_ascii_digit() || chars[j] == '.');
            if dots >= 1 && !next_is_more {
                return Some(chars[start..j].iter().collect());
            }
            i = j.max(i + 1);
        } else {
            i += 1;
        }
    }
    None
}

/// A package's `bin` field as (command, absolute path). npm allows a bare
/// string (one command, named after the package) or a map.
pub fn bin_targets_for(dir: &Path, pkg: &Value) -> Vec<(String, PathBuf)> {
    let fallback = pkg.get("name").and_then(|n| n.as_str()).map(String::from).unwrap_or_else(|| file_stem(dir));
    match pkg.get("bin") {
        Some(Value::String(rel)) if !rel.is_empty() => vec![(fallback, dir.join(rel))],
        Some(Value::Object(m)) => m
            .iter()
            .filter_map(|(k, v)| v.as_str().filter(|s| !s.is_empty()).map(|rel| (k.clone(), dir.join(rel))))
            .collect(),
        _ => Vec::new(),
    }
}

fn suggest_reinstall(dir: &Path, pkg: &Value) {
    let name = pkg.get("name").and_then(|n| n.as_str()).unwrap_or("@anthropic-ai/claude-code");
    if let Some(post) = pkg.get("scripts").and_then(|s| s.get("postinstall")).and_then(|p| p.as_str()) {
        info(format!("    Re-run its install step: {}", command(format!("cd \"{}\" && {post}", path_string(dir)))));
    }
    info(format!("    Or reinstall: {}", command(format!("npm install -g {name}@latest --force"))));
}

fn check_bin_targets(dir: &Path, pkg: &Value, t: &mut Tally) -> (usize, usize) {
    use std::os::unix::fs::PermissionsExt;
    let (mut problems, mut warnings) = (0, 0);
    let pkg_version = pkg.get("version").and_then(|v| v.as_str());
    for (cmd, target) in bin_targets_for(dir, pkg) {
        let rel = target.strip_prefix(dir).map(|r| r.display().to_string()).unwrap_or_else(|_| target.display().to_string());
        let Ok(meta) = std::fs::metadata(&target) else {
            err(format!("  \"{cmd}\" points at {rel}, which does not exist."));
            problems += 1;
            t.problems += 1;
            continue;
        };
        if meta.permissions().mode() & 0o111 == 0 {
            err(format!("  \"{cmd}\" ({rel}) is not executable."));
            info("    A shell that finds it will fail with `permission denied`.");
            problems += 1;
            t.problems += 1;
            suggest_reinstall(dir, pkg);
            continue;
        }
        // Executable is not the same as working: running it is the only
        // honest test.
        match output_with_timeout(&target.to_string_lossy(), &["--version"], Duration::from_secs(20)) {
            None => {
                err(format!("  \"{cmd}\" could not be executed, or did not finish."));
                if is_mac() {
                    info("    On Apple Silicon an unsigned or badly signed binary is killed on start.");
                }
                problems += 1;
                t.problems += 1;
                suggest_reinstall(dir, pkg);
            }
            Some((false, out)) => {
                err(format!("  \"{cmd}\" exited with an error when run."));
                if let Some(first) = out.trim().lines().next() {
                    info(format!("    It said: {first}"));
                }
                problems += 1;
                t.problems += 1;
                suggest_reinstall(dir, pkg);
            }
            Some((true, out)) => {
                // It runs, but can still be a different build than its
                // package describes.
                if let (Some(reported), Some(declared)) = (parse_reported_version(&out), pkg_version) {
                    if reported != declared {
                        warn(format!("  \"{cmd}\" reports {reported}, but the package says {declared}."));
                        info("    The installed command is not the build this package describes.");
                        suggest_reinstall(dir, pkg);
                        warnings += 1;
                        t.warnings += 1;
                    }
                }
            }
        }
    }
    (problems, warnings)
}

fn check_broken_installs(t: &mut Tally) {
    step("npm install integrity (Claude Code)");
    let Some(root) = run_output("npm", ["root", "-g"]).map(|s| PathBuf::from(s.trim())).filter(|p| file_exists(p)) else {
        info("Could not determine the global npm root; skipping this check.");
        return;
    };
    let dir = root.join("@anthropic-ai").join("claude-code");
    if !file_exists(&dir) {
        info("Claude Code is not installed in the global npm root.");
        info(dim("  (Normal if you installed it with the native installer, Homebrew, or a version manager.)"));
        return;
    }
    let manifest = dir.join("package.json");
    if !file_exists(&manifest) {
        err(format!("Broken install: {}", tildify(&dir)));
        info("  The directory exists but has no package.json.");
        info("  npm won't treat it as installed and its command may vanish from PATH.");
        info(format!("  Fix: {}", command("npm install -g @anthropic-ai/claude-code@latest --force")));
        t.problems += 1;
        return;
    }
    let Some(pkg) = crate::util::read_json(&manifest) else {
        warn(format!("{}: package.json is unreadable or malformed.", tildify(&dir)));
        t.warnings += 1;
        return;
    };
    let label = format!(
        "{} {}",
        pkg.get("name").and_then(|n| n.as_str()).unwrap_or("@anthropic-ai/claude-code"),
        pkg.get("version").and_then(|n| n.as_str()).unwrap_or("")
    );
    let label = label.trim();
    match check_bin_targets(&dir, &pkg, t) {
        (p, _) if p > 0 => warn(format!("{label}: package metadata is fine but its command is not usable.")),
        (_, w) if w > 0 => warn(format!("{label} is installed, but see the note above.")),
        _ => ok(format!("{label} install looks intact.")),
    }
}

// ---- This tool ----------------------------------------------------------------------------------

fn check_own_version(t: &mut Tally) {
    step("claude-profiles version");
    info(format!("Running: {VERSION}"));
    // The copy people run may not be this one, which is the "I upgraded and
    // nothing changed" trap for this tool too.
    let copies = resolve_binaries("claude-profiles");
    let me = std::env::current_exe().ok().and_then(|e| std::fs::canonicalize(e).ok());
    match copies.first() {
        None => info("claude-profiles is not on PATH (running it by path works, but `upgrade` cannot verify itself)."),
        Some(w) if std::fs::canonicalize(w).ok() == me => ok("This is the copy on PATH."),
        Some(w) => {
            let v = crate::claudebin::binary_version(w).unwrap_or_else(|| "unknown".into());
            if me.as_deref().is_some_and(|m| m == helper_path()) {
                info(format!("Running as the launch helper; the copy on PATH is {} ({v}).", tildify(w)));
            } else {
                warn(format!("The claude-profiles on PATH is {} ({v}), not this one.", tildify(w)));
                t.warnings += 1;
            }
        }
    }
}

// ---- Profiles, folders and collisions ---------------------------------------------------------------

fn check_profiles(t: &mut Tally, reg: &Registry) {
    step("Profiles");
    if reg.profiles.is_empty() {
        info("No profiles configured.");
        return;
    }
    let mut aliases = AliasCache::default();
    let mut owners: HashMap<PathBuf, String> = HashMap::new();
    let h = home();
    for p in &reg.profiles {
        println!();
        println!("  {} {}", path_str(&p.name), dim(format!("({})", p.kind)));
        let mut dirs: Vec<(&str, &PathBuf)> = Vec::new();
        if let Some(c) = &p.code {
            dirs.push(("Code config", &c.config_dir));
        }
        if let Some(d) = &p.desktop {
            dirs.push(("Desktop data", &d.data_dir));
            dirs.push(("Launcher", &d.app_path));
        }
        for (label, dir) in dirs {
            if file_exists(dir) {
                println!("    ✓ {label}: {}", tildify(dir));
            } else {
                println!("    ✗ {label}: {} {}", tildify(dir), dim("(missing)"));
                t.problems += 1;
            }
            let base = dir.file_name().map(|b| b.to_string_lossy().to_string()).unwrap_or_default();
            if let Some((_, owner)) = FOREIGN_DIRS.iter().find(|(b, _)| *b == base) {
                if *dir == h.join(&base) {
                    err(format!("      Collision: this is {owner}'s directory."));
                    info("      `remove` on this profile would offer to delete it. Consider renaming the profile.");
                    t.problems += 1;
                }
            }
            if label != "Launcher" {
                if let Some(prev) = owners.get(dir) {
                    if prev != &p.name {
                        err(format!("      Conflict: also used by profile \"{prev}\"."));
                        t.problems += 1;
                    }
                }
                owners.insert(dir.clone(), p.name.clone());
            }
        }
        if let Some(c) = &p.code {
            let (present, rc) = aliases.has(p);
            if present {
                println!("    ✓ Alias \"{}\" present in {}", c.alias_name, tildify(&rc));
            } else {
                println!("    ✗ Alias \"{}\" missing from {}", c.alias_name, tildify(&rc));
                t.problems += 1;
            }
        }
    }
}

// ---- Launchers ------------------------------------------------------------------------------------------

fn desktop_launchers(reg: &Registry) -> Vec<&Profile> {
    reg.profiles.iter().filter(|p| p.desktop.as_ref().is_some_and(|d| file_exists(&d.app_path))).collect()
}

fn check_launcher_bundle_ids(t: &mut Tally, reg: &Registry, fix: bool) {
    if !is_mac() {
        return;
    }
    let launchers = desktop_launchers(reg);
    if launchers.is_empty() {
        return;
    }
    step("Launcher bundle IDs");
    for p in launchers {
        let app = &p.desktop.as_ref().expect("filtered").app_path;
        let Some(id) = get_bundle_id(app) else {
            warn(format!("{}: could not read the launcher's Info.plist.", p.name));
            t.warnings += 1;
            continue;
        };
        if id != DEFAULT_APPLET_BUNDLE_ID {
            ok(format!("{}: {}", p.name, dim(&id)));
            // The stock asset catalog overrides the Claude icon in the Dock.
            if has_asset_catalog(app) {
                if fix {
                    if strip_asset_catalog(app) {
                        refresh_launcher(app);
                        ok("  Repaired: launcher now uses the Claude icon in the Dock.");
                    }
                } else {
                    info(format!("{}: its Dock icon is being overridden by the stock AppleScript icon.", p.name));
                    repair_hint(t);
                }
            }
            // A background-agent launcher will not stay pinned.
            if is_ui_element(app) == Some(true) {
                if fix {
                    if set_ui_element(app, false) {
                        refresh_launcher(app);
                        ok("  Repaired: launcher can stay pinned in the Dock again.");
                    }
                } else {
                    info(format!("{}: its launcher is marked as a background agent, so macOS will not keep it pinned.", p.name));
                    repair_hint(t);
                }
            }
            continue;
        }
        warn(format!("{}: launcher still has the default AppleScript bundle ID.", p.name));
        info("  With two or more launchers like this, Dock double-clicks can stop working.");
        if fix {
            let new_id = unique_bundle_id(&p.name);
            if set_bundle_id(app, &new_id) {
                lsregister("-f", app);
                ok(format!("  Repaired: restamped as {new_id} and re-registered."));
            } else {
                err("  Could not rewrite the bundle ID.");
                t.problems += 1;
            }
        } else {
            info(format!(
                "  Repair with {} or {}",
                command("claude-profiles doctor --fix"),
                command(format!("claude-profiles repair {}", p.name))
            ));
            t.warnings += 1;
        }
    }
}

/// Launchers compiled from a temporary folder were named after it. Only that
/// exact name is changed; a name the user gave a launcher is theirs.
fn check_launcher_names(t: &mut Tally, reg: &Registry, fix: bool) {
    if !is_mac() {
        return;
    }
    let stuck: Vec<&Profile> = desktop_launchers(reg)
        .into_iter()
        .filter(|p| get_bundle_name(&p.desktop.as_ref().expect("filtered").app_path).as_deref() == Some(STAGED_LAUNCHER_NAME))
        .collect();
    if stuck.is_empty() {
        return;
    }
    step("Launcher names");
    for p in stuck {
        let app = &p.desktop.as_ref().expect("filtered").app_path;
        let name = file_stem(app);
        warn(format!("{}: its launcher shows as \"{STAGED_LAUNCHER_NAME}\" in Raycast, Alfred and the app switcher.", p.name));
        if fix {
            if set_bundle_name(app, &name) {
                refresh_launcher(app);
                ok(format!("  Repaired: renamed to \"{name}\"."));
            } else {
                err("  Could not rename it.");
                t.problems += 1;
            }
        } else {
            repair_hint(t);
        }
    }
}

/// A launcher must export the profile's Code folder, or Claude Code started
/// from inside Desktop falls back to the shared ~/.claude. The symptom is
/// invisible: Desktop is isolated, only the CLI half quietly merges.
fn check_launcher_env(t: &mut Tally, reg: &Registry, fix: bool) {
    if !is_mac() {
        return;
    }
    let candidates: Vec<&Profile> = desktop_launchers(reg).into_iter().filter(|p| p.code.is_some()).collect();
    let desktop_only: Vec<&Profile> = reg.profiles.iter().filter(|p| p.desktop.is_some() && p.code.is_none()).collect();
    if candidates.is_empty() && desktop_only.is_empty() {
        return;
    }
    step("Desktop-spawned Claude Code");
    for p in desktop_only {
        info(format!("{}: Desktop only, so Claude Code opened inside it uses {}.", p.name, tildify(default_claude_config_dir())));
        println!(
            "      {}",
            dim(format!("Give it its own with {}, choosing Claude Code and the name \"{}\".", command("claude-profiles add"), p.name))
        );
    }
    for p in candidates {
        let d = p.desktop.as_ref().expect("filtered");
        let expected = path_string(&p.code.as_ref().expect("filtered").config_dir);
        let Some(current) = launcher_code_config_dir(&d.app_path) else {
            warn(format!("{}: could not read the launcher's script.", p.name));
            t.warnings += 1;
            continue;
        };
        if current.as_deref() == Some(expected.as_str()) {
            ok(format!("{}: launcher exports {}", p.name, dim(tildify(&expected))));
            continue;
        }
        match &current {
            None => {
                warn(format!("{}: launcher does not set CLAUDE_CONFIG_DIR.", p.name));
                info("  Claude Code started from inside this Desktop profile will use the");
                info(format!("  shared {} instead of the profile's own config.", tildify(default_claude_config_dir())));
            }
            Some(c) => {
                warn(format!("{}: launcher exports a stale config dir.", p.name));
                info(format!("  Launcher says: {}", tildify(c)));
                info(format!("  Profile wants: {}", tildify(&expected)));
            }
        }
        if !fix {
            repair_hint(t);
            continue;
        }
        if !file_exists(&d.claude_app_path) {
            err(format!("  Cannot rebuild: Claude.app not found at {}", d.claude_app_path.display()));
            t.problems += 1;
            continue;
        }
        match rebuild(p) {
            Ok(()) => ok(format!("  Repaired: launcher rebuilt and now exports {}", tildify(&expected))),
            Err(e) => {
                err(format!("  Could not rebuild the launcher: {e:#}"));
                info(format!("  Recreate it with {} using the same name and paths.", command("claude-profiles add")));
                t.problems += 1;
            }
        }
    }
}

// ---- Two profiles signed in as the same account ----------------------------------------------------------
//
// No path check can see this, because no path is wrong: a claude:// sign-in
// link reached the wrong running instance and the token landed in the wrong
// folder. Two folders recording the same account is the only visible signal.
// Not fixable automatically: it means signing out and in with nothing else open.

fn check_account_collisions(t: &mut Tally, reg: &Registry) {
    let defaults = detect_defaults();
    let mut seats: Vec<(String, PathBuf)> = Vec::new();
    if let Some((_, data)) = defaults.desktop {
        seats.push(("default".into(), data));
    }
    for p in &reg.profiles {
        if let Some(d) = &p.desktop {
            seats.push((p.name.clone(), d.data_dir.clone()));
        }
    }
    if seats.len() < 2 {
        return;
    }
    step("Signed-in accounts");
    let mut by_account: Vec<(String, Vec<String>)> = Vec::new();
    let mut any = false;
    for (label, dir) in &seats {
        let Some(id) = desktop_account_uuid(dir) else {
            info(format!("{label}: not signed in yet."));
            continue;
        };
        any = true;
        // A prefix is enough to compare by eye without printing the whole ID.
        info(format!("{label}: account {}", dim(id.chars().take(8).collect::<String>())));
        match by_account.iter_mut().find(|(a, _)| *a == id) {
            Some((_, names)) => names.push(label.clone()),
            None => by_account.push((id, vec![label.clone()])),
        }
    }
    if !any {
        return;
    }
    let collisions: Vec<&Vec<String>> = by_account.iter().map(|(_, n)| n).filter(|n| n.len() > 1).collect();
    if collisions.is_empty() {
        ok("Every profile is a different account.");
        return;
    }
    for names in collisions {
        blank();
        err(format!("{} are signed in as the SAME account.", names.join(" and ")));
        info("  These are supposed to be different accounts, so one of them holds a");
        info("  token that belongs to the other.");
        info("  This happens when a sign-in runs while two Claude windows are open:");
        info("  the claude:// callback goes to whichever instance answers first, and");
        info("  the token lands in the wrong data folder. A Claude update that forces");
        info("  re-authentication is the usual trigger.");
        info("  To fix, in the profile that shows the wrong account:");
        info("    1. Quit every Claude window (Cmd+Q), leaving none running.");
        info("    2. Open only that profile and sign out.");
        info("    3. Sign back in with the account it should be, still with nothing else open.");
        t.problems += 1;
    }
}

// ---- Each profile's copy of Claude ---------------------------------------------------------------------------

fn check_app_clones(t: &mut Tally, reg: &Registry, fix: bool) {
    if !is_mac() {
        return;
    }
    let desktop: Vec<&Profile> = reg.profiles.iter().filter(|p| p.desktop.is_some()).collect();
    if desktop.is_empty() {
        return;
    }
    step("Per-profile Claude copies");

    for p in desktop.iter().filter(|p| !file_exists(clone_path_for(&p.name))) {
        let d = p.desktop.as_ref().expect("filtered");
        warn(format!("{}: has no dedicated copy of Claude.app.", p.name));
        info("  Clicking its Dock icon opens another window every time instead of");
        info("  focusing the one already open.");
        if !fix {
            repair_hint(t);
            continue;
        }
        let r = ensure_colored_clone(&p.name, &d.claude_app_path, d.color.as_deref(), false).and_then(|_| rebuild(p));
        match r {
            Ok(()) => ok(format!("  Repaired: {} now has its own copy, and its icon focuses instead of duplicating.", p.name)),
            Err(e) => {
                err(format!("  Could not build the copy: {e:#}"));
                t.problems += 1;
            }
        }
    }

    for p in desktop.iter().filter(|p| file_exists(clone_path_for(&p.name))) {
        let d = p.desktop.as_ref().expect("filtered");
        let clone = clone_path_for(&p.name);
        let (cv, sv) = (app_version(&clone).unwrap_or_default(), app_version(&d.claude_app_path).unwrap_or_default());
        let state = clone_state(&clone, &d.claude_app_path);

        if state == CloneState::Behind {
            // With a launcher that goes through the helper, being behind is the
            // normal state between a Claude update and the next click, which
            // rebuilds it. That is information, not a warning.
            let managed = launcher_uses_helper(&d.app_path) == Some(true) && helper_state() != HelperState::Missing;
            let open = clone_is_running(&clone);
            if fix && !open {
                info(format!("{}: copy is Claude {cv}, but {sv} is installed.", p.name));
                match ensure_colored_clone(&p.name, &d.claude_app_path, d.color.as_deref(), true) {
                    Ok(_) => ok("  Repaired: copy rebuilt from the current Claude.app."),
                    Err(e) => {
                        err(format!("  Could not rebuild the copy: {e:#}"));
                        t.problems += 1;
                    }
                }
                continue;
            }
            if managed {
                info(format!("{}: will update from Claude {cv} to {sv} the next time you open it.", p.name));
                if open {
                    info("  It is open now, so that happens after you quit it and open it again.");
                }
                continue;
            }
            warn(format!("{}: copy is Claude {cv}, but {sv} is installed.", p.name));
            info("  Nothing updates it on its own: its launcher opens the copy directly.");
            if fix {
                info("  It is open right now, so it was left alone; rebuilding an open app can crash it.");
                t.warnings += 1;
            } else {
                repair_hint(t);
            }
            continue;
        }

        if state == CloneState::Ahead {
            info(format!("{}: copy is Claude {cv}, newer than the installed {sv}.", p.name));
            info("  Claude updated this copy itself. Keeping it; rebuilding from the");
            info("  installed app would be a downgrade.");
        }

        let Some(color) = &d.color else {
            ok(format!("{}: Claude {}", p.name, dim(&cv)));
            continue;
        };
        // A matching version says nothing about the colour: an update swaps
        // the whole bundle folder, and the tint went with the old one.
        if has_custom_icon(&clone) {
            ok(format!("{}: {color}, Claude {}", p.name, dim(&cv)));
            continue;
        }
        warn(format!("{}: its {color} colour is gone.", p.name));
        info("  A Claude update replaces the whole app, and the colour went with it.");
        if fix {
            match apply_color(&clone, &clone, color) {
                Ok(()) => {
                    refresh_launcher(&clone);
                    ok(format!("  Repaired: {color} re-applied. The Dock shows it from the next launch."));
                }
                Err(e) => {
                    err(format!("  Could not re-apply the colour: {e:#}"));
                    t.problems += 1;
                }
            }
        } else {
            repair_hint(t);
        }
    }
}

// ---- How each profile updates ---------------------------------------------------------------------------------

fn check_managed_updates(t: &mut Tally, reg: &Registry, fix: bool) {
    if !is_mac() {
        return;
    }
    let with_copies: Vec<&Profile> = reg.profiles.iter().filter(|p| p.desktop.is_some() && file_exists(clone_path_for(&p.name))).collect();
    if with_copies.is_empty() {
        return;
    }
    step("Profile updates");

    let hs = helper_state();
    if hs != HelperState::Current {
        if hs == HelperState::Missing {
            warn("The launch helper is not installed.");
            info("  Launchers still open Claude without it, but copies are not kept current");
            info("  and a profile open on the wrong account is not caught.");
        } else {
            warn("The launch helper is a different build from this one.");
            info("  It still works; the current one is what this version's fixes rely on.");
        }
        if fix {
            match install_helper() {
                Ok(_) => ok(format!("  Repaired: helper written to {}", tildify(helper_path()))),
                Err(e) => {
                    err(format!("  Could not write the helper: {e:#}"));
                    t.problems += 1;
                }
            }
        } else {
            repair_hint(t);
        }
    }

    let log = recent_launch_log(Duration::from_secs(7 * 24 * 3600), None);
    for p in with_copies {
        let d = p.desktop.as_ref().expect("filtered");
        // A launcher that opens the copy directly, or starts the helper in the
        // background and quits (which can get the helper killed mid-update),
        // is rebuilt in place.
        let uses = if file_exists(&d.app_path) { launcher_uses_helper(&d.app_path) } else { None };
        let waits = if uses == Some(true) { launcher_waits_for_helper(&d.app_path) } else { None };
        if uses == Some(false) || waits == Some(false) {
            if uses == Some(false) {
                warn(format!("{}: its launcher opens the copy directly, without the helper.", p.name));
            } else {
                warn(format!("{}: its launcher starts the helper and quits without waiting for it.", p.name));
                info("  That can cut an update off part-way: nothing opens, and a");
                info("  half-made copy is left behind.");
            }
            if fix {
                match rebuild(p) {
                    Ok(()) => ok("  Repaired: launcher rebuilt in place (a Dock pin keeps working)."),
                    Err(e) => {
                        err(format!("  Could not rebuild the launcher: {e:#}"));
                        t.problems += 1;
                    }
                }
            } else {
                repair_hint(t);
            }
        }

        // An update a launcher started and never finished.
        let clone = clone_path_for(&p.name);
        let leftovers = refresh_leftovers(&clone);
        let unfinished = unfinished_refresh(&file_stem(&d.app_path), &log);
        if !leftovers.is_empty() || unfinished.is_some() {
            let detail = unfinished
                .as_ref()
                .map(|u| format!(" ({}, at {})", u.message.trim_start_matches("updating the copy "), u.at.format("%b %-d %H:%M")))
                .unwrap_or_default();
            warn(format!("{}: an update of its copy was cut off part-way{detail}.", p.name));
            if !leftovers.is_empty() {
                let names: Vec<String> = leftovers.iter().map(|x| x.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()).collect();
                info(format!("  It left {} behind.", names.join(" and ")));
            }
            if fix && !clone_is_running(&clone) {
                let r = (|| -> std::io::Result<()> {
                    let previous = with_suffix(&clone, ".previous");
                    if !file_exists(&clone) && file_exists(&previous) {
                        std::fs::rename(&previous, &clone)?;
                    }
                    for x in refresh_leftovers(&clone) {
                        remove_all(&x)?;
                    }
                    Ok(())
                })();
                match r {
                    Ok(()) => ok("  Repaired: cleaned up. The copy updates the next time you open it, or now with doctor --fix."),
                    Err(e) => {
                        err(format!("  Could not clean up: {e}"));
                        t.problems += 1;
                    }
                }
            } else if fix {
                info("  It is open now; its launcher cleans this up the next time you open it.");
            } else {
                info(format!("  Clean up with {}; its launcher also does on the next click.", command("claude-profiles doctor --fix")));
                t.warnings += 1;
            }
        }

        if !wants_update_block(p) {
            info(format!("{}: updates itself, by choice ({} to change).", p.name, command(format!("claude-profiles self-update {} off", p.name))));
            continue;
        }
        match update_block_state(&d.data_dir) {
            BlockState::Blocked => {
                ok(format!("{}: updates managed by its launcher.", p.name));
                continue;
            }
            BlockState::Foreign(blocked) => {
                info(format!("{}: has its own Claude configuration (likely third-party inference), left alone.", p.name));
                info(if blocked {
                    "  It already blocks updates, so the copy stays put and its launcher keeps it current."
                } else {
                    "  The copy may update itself, which relaunches it on the default account."
                });
                continue;
            }
            BlockState::Absent => {}
        }
        warn(format!("{}: its copy of Claude updates itself.", p.name));
        info("  Every update relaunches it on your default account and removes its colour.");
        if fix {
            match block_updates(&d.data_dir) {
                Ok(_) => {
                    ok("  Repaired: updates now managed by its launcher.");
                    info("  Takes effect the next time the profile starts.");
                }
                Err(e) => {
                    err(format!("  Could not set up managed updates: {e:#}"));
                    t.problems += 1;
                }
            }
        } else {
            repair_hint(t);
        }
    }

    // The helper never blocks a launch, so its failures only show up here.
    let errors: Vec<_> = log.iter().filter(|e| e.level == "ERROR").collect();
    if !errors.is_empty() {
        warn(format!("The launch helper hit {} error(s) in the last 7 days:", errors.len()));
        for e in errors.iter().rev().take(3).rev() {
            println!("      {}", dim(&e.line));
        }
        info(format!("  Full log: {}", tildify(launch_log())));
        t.warnings += 1;
    }
}

// ---- How Desktop is actually being started ---------------------------------------------------------------------
//
// Isolation lives in the launch COMMAND, not the app. A copy started without
// --user-data-dir (a tile pinned from the running window, Spotlight, a Login
// Item, a relaunch after an update) runs on the default profile and simply
// shows the wrong account.

/// File paths of the Dock's pinned tiles, from `defaults read`'s text plist
/// (its binary icon blobs make a JSON conversion refuse the whole thing).
pub fn parse_dock_paths(plist_text: &str) -> Vec<String> {
    let key = "\"_CFURLString\"";
    let mut out = Vec::new();
    let mut rest = plist_text;
    while let Some(i) = rest.find(key) {
        rest = &rest[i + key.len()..];
        let after_eq = rest.trim_start();
        let Some(after_eq) = after_eq.strip_prefix('=') else { continue };
        let Some(value) = after_eq.trim_start().strip_prefix('"') else { continue };
        let Some(end) = value.find('"') else { continue };
        let url = &value[..end];
        if let Some(path) = url.strip_prefix("file://") {
            if let Ok(decoded) = percent_encoding::percent_decode_str(path).decode_utf8() {
                out.push(decoded.trim_end_matches('/').to_string());
            }
        }
    }
    out
}

fn dock_pinned_paths() -> Option<Vec<String>> {
    run_output("/usr/bin/defaults", ["read", "com.apple.dock", "persistent-apps"]).map(|s| parse_dock_paths(&s))
}

fn check_desktop_launch_path(t: &mut Tally, reg: &Registry) {
    if !is_mac() {
        return;
    }
    let with_copies: Vec<&Profile> = reg.profiles.iter().filter(|p| p.desktop.is_some() && file_exists(clone_path_for(&p.name))).collect();
    if with_copies.is_empty() {
        return;
    }
    step("Desktop launch path");
    let pinned = dock_pinned_paths();
    let mut flagged = false;
    for p in with_copies {
        let d = p.desktop.as_ref().expect("filtered");
        let copy = clone_path_for(&p.name);
        for proc in running_copies(&copy) {
            if proc.command.contains("--user-data-dir=") {
                continue;
            }
            flagged = true;
            err(format!("{}: running without --user-data-dir (pid {}).", p.name, proc.pid));
            info(format!("  It is on the shared default profile, not {},", path_str(tildify(&d.data_dir))));
            info("  so it shows whichever account that one is signed in to.");
            info(format!("  Quit it and reopen from {}.", path_str(tildify(&d.app_path))));
            t.problems += 1;
        }
        let copy_s = path_string(&copy);
        if pinned.as_ref().is_some_and(|ps| ps.iter().any(|x| *x == copy_s.trim_end_matches('/'))) {
            flagged = true;
            warn(format!("{}: its Claude copy is pinned to the Dock instead of its launcher.", p.name));
            info("  Clicking that tile starts the copy directly, skipping the launcher,");
            info("  which is what drops --user-data-dir and opens the default profile.");
            info(format!("  Remove that tile and pin {} instead.", path_str(tildify(&d.app_path))));
            t.warnings += 1;
        }
    }
    match &pinned {
        None => info("Could not read the Dock configuration; skipped the pinned-tile check."),
        Some(ps) => {
            if let Some(stock) = find_claude_app() {
                if ps.contains(&path_string(&stock)) {
                    info(format!("{} is pinned to the Dock.", tildify(&stock)));
                    info("  That is the unprofiled Claude: it always opens the shared default");
                    info("  profile, never one of the profiles above.");
                }
            }
        }
    }
    if !flagged {
        ok("Every running Claude copy was started through its launcher.");
    }
}

// ---- GitHub CLI isolation ------------------------------------------------------------------------------------------

fn check_gh_isolation(t: &mut Tally, reg: &Registry, fix: bool) {
    let isolated: Vec<&Profile> = reg.profiles.iter().filter(|p| p.code.as_ref().is_some_and(|c| c.gh_config_dir.is_some())).collect();
    if isolated.is_empty() {
        return;
    }
    step("GitHub CLI isolation");
    if let Some(v) = gh_token_override() {
        warn(format!("{v} is set in your environment."));
        info("  gh prefers that token over any config directory, so every profile");
        info("  acts as its account regardless of per-profile settings.");
        info("  Unset it in your shell config for per-profile gh logins to work.");
        t.warnings += 1;
    }
    for p in isolated {
        let gh = p.code.as_ref().and_then(|c| c.gh_config_dir.clone()).expect("filtered");
        if !file_exists(&gh) {
            warn(format!("{}: gh config folder is missing ({}).", p.name, tildify(&gh)));
            info("  gh will fall back to your default login for this profile.");
            info(format!(
                "  Recreate it with {} then sign in with {}.",
                command(format!("mkdir -p \"{}\"", path_string(&gh))),
                command("gh auth login")
            ));
            t.warnings += 1;
            continue;
        }
        ok(format!("{}: {}", p.name, dim(tildify(&gh))));
        // The alias covers the terminal; the launcher covers Claude Code opened
        // from inside Desktop.
        let Some(d) = p.desktop.as_ref().filter(|d| is_mac() && file_exists(&d.app_path)) else { continue };
        if launcher_gh_config_dir(&d.app_path) != Some(None) {
            continue;
        }
        warn("  Its Desktop launcher does not export GH_CONFIG_DIR.");
        info("  Claude Code opened from the Desktop app would use your default gh account.");
        if fix && file_exists(&d.claude_app_path) {
            match rebuild(p) {
                Ok(()) => ok("  Repaired: launcher rebuilt with both config dirs."),
                Err(e) => {
                    err(format!("  Could not rebuild the launcher: {e:#}"));
                    t.problems += 1;
                }
            }
        } else {
            repair_hint(t);
        }
    }
}

// ---- What a whole-folder seed left behind --------------------------------------------------------------------------
//
// Profiles seeded by early versions of the original tool got a copy of the
// whole ~/.claude: the default account's conversations, and plugin and hook
// paths that still load from ~/.claude. Conversations are reported, never
// deleted. Paths are pointed at the profile's own copies, only where those
// exist.

fn check_seeded_profiles(t: &mut Tally, reg: &Registry, fix: bool) {
    let default = default_claude_config_dir();
    let code: Vec<&Profile> = reg
        .profiles
        .iter()
        .filter(|p| p.code.as_ref().is_some_and(|c| c.config_dir != default && file_exists(&c.config_dir)))
        .collect();
    if code.is_empty() || !file_exists(&default) {
        return;
    }
    step("Copied from your default Claude Code setup");
    for p in code {
        let dir = &p.code.as_ref().expect("filtered").config_dir;
        let mut clean = true;

        let copied = copied_transcripts(dir, &default);
        if copied > 0 {
            clean = false;
            info(format!(
                "{}: {copied} conversation{} copied here from your default profile when it was created.",
                p.name,
                plural(copied, " was", "s were")
            ));
            info("  These are left as they are;");
            info(format!("  to drop them, delete the matching transcripts under {}.", tildify(dir.join("projects"))));
        }

        let settings = dir.join("settings.json");
        let files: Vec<(PathBuf, Option<&[&str]>)> = [
            (dir.join("plugins").join("installed_plugins.json"), None),
            (dir.join("plugins").join("known_marketplaces.json"), None),
            (settings.clone(), Some(SETTINGS_REBASE_KEYS)),
        ]
        .into_iter()
        .filter(|(f, _)| file_exists(f))
        .collect();
        let (mut rebasable, mut stuck) = (0, 0);
        for (f, keys) in &files {
            let c = paths_into_other_profile(f, &default, dir, *keys);
            rebasable += c.rebasable;
            stuck += c.stuck;
        }

        let flipped = if file_exists(&settings) { flipped_claude_md_excludes(&settings, dir, &default, false) } else { Vec::new() };
        if !flipped.is_empty() {
            clean = false;
            warn(format!("{}: claudeMdExcludes excludes this profile's own instructions instead of your default profile's.", p.name));
            for (from, to) in &flipped {
                info(format!("    {}  should be  {}", tildify(from), tildify(to)));
            }
            if fix {
                flipped_claude_md_excludes(&settings, dir, &default, true);
                ok("  Repaired: pointed back at your default profile. Takes effect in new sessions.");
            } else {
                repair_hint(t);
            }
        }

        if rebasable > 0 {
            clean = false;
            warn(format!(
                "{}: {rebasable} plugin or hook path{} into your default profile's folder.",
                p.name,
                plural(rebasable, " leads", "s lead")
            ));
            info("  So it loads those from the default profile, and changes there reach it too.");
            if fix {
                for (f, keys) in &files {
                    rebase_json_file(f, &default, dir, *keys);
                }
                ok("  Repaired: pointed at this profile's own copies. Takes effect in new sessions.");
            } else {
                repair_hint(t);
            }
        }
        if stuck > 0 {
            clean = false;
            info(format!(
                "{}: {stuck} path{} still load{} from your default profile, because this profile has no copy of {}.",
                p.name,
                plural(stuck, "", "s"),
                plural(stuck, "s", ""),
                plural(stuck, "it", "them")
            ));
            info("  Left alone, since pointing elsewhere would break them. Reinstalling that plugin here gives it its own copy.");
        }
        if clean {
            ok(format!("{}: nothing copied from the default profile.", p.name));
        }
    }
}

// ---- Desktop MCP connectors ------------------------------------------------------------------------------------------
//
// Reports which of the default profile's connectors a profile lacks. Never
// copies, even with --fix: their settings can carry the default account's
// credentials, and `upgrade` runs --fix on its own. `extensions` copies the
// ones the user picks.

fn check_desktop_connectors(t: &mut Tally, reg: &Registry) {
    let desktop: Vec<&Profile> = reg.profiles.iter().filter(|p| p.desktop.as_ref().is_some_and(|d| file_exists(&d.data_dir))).collect();
    let default_file = default_desktop_config_file();
    if desktop.is_empty() || !file_exists(&default_file) {
        return;
    }
    let Some(wanted) = mcp_server_names(&default_file).filter(|w| !w.is_empty()) else { return };
    step("Desktop MCP connectors");
    for p in desktop {
        let file = desktop_config_file(&p.desktop.as_ref().expect("filtered").data_dir);
        let have = if file_exists(&file) {
            match mcp_server_names(&file) {
                Some(h) => h,
                None => {
                    warn(format!("{}: claude_desktop_config.json is not valid JSON, so it was left alone.", p.name));
                    info(format!("  Claude may not load this profile's connectors. Fix the JSON by hand: {}", tildify(&file)));
                    t.warnings += 1;
                    continue;
                }
            }
        } else {
            Vec::new()
        };
        let missing: Vec<&String> = wanted.iter().filter(|k| !have.contains(k)).collect();
        if missing.is_empty() {
            ok(format!("{}: has every connector your default profile has ({}).", p.name, have.len()));
            continue;
        }
        let names: Vec<&str> = missing.iter().map(|s| s.as_str()).collect();
        info(format!(
            "{}: does not have {} connector{} your default profile has ({}).",
            p.name,
            missing.len(),
            plural(missing.len(), "", "s"),
            names.join(", ")
        ));
        info(format!("  If it should, copy the ones you want with {}.", command("claude-profiles extensions")));
    }
}

// ---- Cross-profile read protection ---------------------------------------------------------------------------------------

fn check_deny_rules(t: &mut Tally, reg: &Registry, fix: bool) {
    step("Cross-profile read protection");
    if !read_protection_enabled(reg) {
        info(format!("Off: {} in {}.", dim("\"readProtection\": false"), tildify(registry::registry_path())));
        if fix {
            resync_deny_rules(reg, true);
        }
        return;
    }
    let findings = audit_deny_rules(reg);
    if findings.is_empty() {
        info("Not applicable (needs two or more profiles, at least one of them Code).");
        return;
    }
    let drifted: Vec<_> = findings.iter().filter(|f| !f.missing.is_empty()).collect();
    if drifted.is_empty() {
        ok("Every Code profile blocks reads of the others' directories.");
        return;
    }
    for f in &drifted {
        warn(format!("{}: {} of {} deny rule(s) missing.", f.name, f.missing.len(), f.expected));
        println!("      {}", dim(tildify(&f.settings_path)));
        if let Some(target) = &f.symlink {
            info("      Its settings.json is a symlink out of the profile, so rules are not written.");
            if let Some(t) = target {
                info(format!("      It points at {}", tildify(t)));
            }
            info("      Replace the link with a real file to get read protection for this profile.");
        } else if f.malformed {
            info("      Its settings.json is not valid JSON, so it was left untouched.");
        }
    }
    let repairable = drifted.iter().filter(|f| f.symlink.is_none() && !f.malformed).count();
    if fix && repairable == 0 {
        warn("Nothing here is safe to repair automatically. See the notes above.");
        t.warnings += 1;
    } else if fix {
        resync_deny_rules(reg, false);
        ok("Repaired: deny rules rewritten from the registry.");
    } else {
        info(format!("Repair with {}", command("claude-profiles doctor --fix")));
        t.warnings += 1;
    }
}

// ---- Top level -------------------------------------------------------------------------------------------------------------

fn undo_last_fix(force: bool) -> Result<()> {
    let Some(r) = undo_latest(force)? else {
        info("Nothing to undo: no doctor --fix run has a snapshot.");
        return Ok(());
    };
    let when = chrono::DateTime::from_timestamp_millis(r.at)
        .map(|t| t.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M:%S").to_string())
        .unwrap_or_default();
    step(format!("Undoing the doctor --fix run from {when}"));
    for f in &r.restored {
        ok(format!("Restored {}", tildify(f)));
    }
    for f in &r.removed {
        ok(format!("Removed {} (that run created it)", tildify(f)));
    }
    for (p, why) in &r.skipped {
        warn(format!("Left {} alone: {why}.", tildify(p)));
    }
    if !r.skipped.is_empty() {
        info(format!("  Use {} to put those back anyway.", command("claude-profiles doctor --undo --force")));
    }
    // A launcher whose script or plist came back needs macOS to re-read it.
    let mut launchers: HashSet<PathBuf> = HashSet::new();
    for f in &r.restored {
        let s = path_string(f);
        if let Some(i) = s.find(".app/Contents/") {
            launchers.insert(PathBuf::from(&s[..i + 4]));
        }
    }
    for a in launchers {
        refresh_launcher(&a);
    }
    blank();
    info("Not part of an undo: rebuilt copies of Claude.app, re-applied colours, and a launcher's");
    info("removed icon catalog. Those are rebuilt rather than edited, and doctor --fix redoes them.");
    Ok(())
}

pub fn run(args: &[String]) -> Result<()> {
    header("claude-profiles doctor");
    let has = |f: &str| args.iter().any(|a| a == f);
    if has("--undo") {
        return undo_last_fix(has("--force"));
    }
    let fix = has("--fix");
    let mut t = Tally::default();
    let reg = registry::load();

    // Before any check can change anything: record what --fix may touch.
    let snap = if fix {
        match take_snapshot(&fix_targets(&reg)) {
            Ok(s) => Some(s),
            Err(e) => {
                warn(format!("Could not take a snapshot before fixing ({e}); this run cannot be undone."));
                None
            }
        }
    } else {
        None
    };

    // First: every later check reads the registry, and a corrupt one
    // otherwise looks like "no profiles configured".
    if registry::health() == Health::Corrupt {
        step("Registry");
        err(format!("{} exists but is not valid JSON.", tildify(registry::registry_path())));
        info("Profiles are configured but can't be read. Mutating commands will refuse to run.");
        info(format!("Fix the JSON by hand; a last-known-good copy may exist at {}.bak", tildify(registry::registry_path())));
        t.problems += 1;
    }

    check_claude_binary(&mut t);
    check_broken_installs(&mut t);
    check_own_version(&mut t);
    check_profiles(&mut t, &reg);
    check_launcher_bundle_ids(&mut t, &reg, fix);
    check_launcher_names(&mut t, &reg, fix);
    check_launcher_env(&mut t, &reg, fix);
    check_account_collisions(&mut t, &reg);
    check_app_clones(&mut t, &reg, fix);
    check_managed_updates(&mut t, &reg, fix);
    check_desktop_launch_path(&mut t, &reg);
    check_gh_isolation(&mut t, &reg, fix);
    check_seeded_profiles(&mut t, &reg, fix);
    check_desktop_connectors(&mut t, &reg);
    check_deny_rules(&mut t, &reg, fix);

    println!();
    let defaults = detect_defaults();
    info(format!("Registry: {}", path_str(tildify(registry::registry_path()))));
    info(format!(
        "Default Claude: Desktop {}, Code {}",
        if defaults.desktop.is_some() { "detected" } else { "not detected" },
        if defaults.code.is_some() { "detected" } else { "not detected" }
    ));

    if fix {
        record_fix_pass(VERSION);
    }
    if let Some(s) = snap {
        let changed = finish_snapshot(&s).unwrap_or_default();
        // state.json alone is bookkeeping, not a change worth undoing.
        let real: Vec<&PathBuf> = changed.iter().filter(|f| f.file_name() != Some(OsStr::new("state.json"))).collect();
        if real.is_empty() && !changed.is_empty() {
            discard(&s);
        }
        if !real.is_empty() {
            println!();
            info(format!(
                "--fix changed {} file{}. To put {} back: {}",
                real.len(),
                plural(real.len(), "", "s"),
                plural(real.len(), "it", "them"),
                command("claude-profiles doctor --undo")
            ));
        }
    }

    println!();
    if t.problems == 0 && t.warnings == 0 {
        ok("No problems found.");
    } else {
        if t.problems > 0 {
            err(format!("{} problem{} found.", t.problems, plural(t.problems, "", "s")));
        }
        if t.warnings > 0 {
            warn(format!("{} warning{}.", t.warnings, plural(t.warnings, "", "s")));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn reported_versions_are_pulled_out_of_decorated_output() {
        assert_eq!(parse_reported_version("2.1.126 (Claude Code)").as_deref(), Some("2.1.126"));
        assert_eq!(parse_reported_version("v1.2.3").as_deref(), Some("1.2.3"));
        assert_eq!(parse_reported_version("0.1.19\n").as_deref(), Some("0.1.19"));
        assert_eq!(parse_reported_version("tool 3.4").as_deref(), Some("3.4"));
        assert_eq!(parse_reported_version("no version here"), None);
        assert_eq!(parse_reported_version("build 7"), None);
    }

    #[test]
    fn bin_targets_handle_both_npm_shapes_and_ignore_junk() {
        let dir = Path::new("/n/pkg");
        assert_eq!(bin_targets_for(dir, &json!({"name": "x", "bin": "cli.js"})), vec![("x".to_string(), dir.join("cli.js"))]);
        let mut m = bin_targets_for(dir, &json!({"bin": {"a": "a.js", "b": "", "c": 3}}));
        m.sort();
        assert_eq!(m, vec![("a".to_string(), dir.join("a.js"))]);
        assert!(bin_targets_for(dir, &json!({"bin": ["x"]})).is_empty());
        assert!(bin_targets_for(dir, &json!({})).is_empty());
    }

    #[test]
    fn dock_paths_are_decoded_and_non_file_tiles_skipped() {
        let text = r#"(
    { "tile-data" = { "file-data" = { "_CFURLString" = "file:///Users/me/Applications/Claude%20WORK.app/"; "_CFURLStringType" = 15; }; }; },
    { "tile-data" = { "file-data" = { "_CFURLString" = "https://example.com/"; }; }; },
    { "tile-data" = { "file-data" = { "_CFURLString" = "file:///Applications/Claude.app/"; }; }; }
)"#;
        assert_eq!(parse_dock_paths(text), vec!["/Users/me/Applications/Claude WORK.app", "/Applications/Claude.app"]);
    }
}
