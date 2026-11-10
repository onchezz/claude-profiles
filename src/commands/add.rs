// `claude-profiles add`: the interactive wizard.
//
//   1. What kind of profile? (Desktop, Code, or both)
//   2. The name, and the questions for each half.
//   3. Review, apply, and the next steps.
//
// A name that already exists is not always taken: a profile missing the half
// you selected is offered the missing half (linking), and a complete one can
// still gain its own GitHub CLI login.

use std::path::{Path, PathBuf};

use anyhow::Result;
use chrono::Utc;
use serde_json::Map;

use super::{lsregister, reserved, without_claude_prefix};
use crate::appclone::COLORS;
use crate::claudebin::{default_claude_config_dir, find_claude_app};
use crate::code::{
    add_alias, default_alias_name_for, default_config_dir_for, default_gh_config_dir_for, gh_token_override, has_gh_cli,
    profile_shell, setup_code, CodeSetup,
};
use crate::desktop::{
    compile_app, copy_claude_icon, default_app_path_for, default_data_dir_for, default_desktop_config_file,
    launch_target_for, mcp_server_names, setup_desktop, CompileSpec, DesktopSetup,
};
use crate::permissions::resync_deny_rules;
use crate::registry::{self, CodeTarget, DesktopTarget, Profile};
use crate::ui::{self, blank, command, err, explain, header, info, ok, path_str, step, warn};
use crate::util::{expand_home, file_exists, home, is_mac, path_string, sanitize_name, tildify, title_case};

/// Which of the selected halves a profile does not have yet.
pub fn missing_targets(p: &Profile, wants_desktop: bool, wants_code: bool) -> Vec<&'static str> {
    let mut m = Vec::new();
    if wants_desktop && p.desktop.is_none() {
        m.push("desktop");
    }
    if wants_code && p.code.is_none() {
        m.push("code");
    }
    m
}

/// A complete profile can still gain its own GitHub CLI login.
pub fn can_enable_gh(p: &Profile, gh_available: bool) -> bool {
    gh_available && p.code.as_ref().is_some_and(|c| c.gh_config_dir.is_none())
}

/// A folder about to be claimed is only safe if it does not exist yet, or is
/// already this tool's. Anything else is somebody else's data, and `remove`
/// would later offer to delete it.
pub fn is_unmanaged_existing_dir(dir: &Path) -> bool {
    if !file_exists(dir) {
        return false;
    }
    !registry::load().profiles.iter().any(|p| {
        p.code.as_ref().is_some_and(|c| c.config_dir == dir) || p.desktop.as_ref().is_some_and(|d| d.data_dir == dir)
    })
}

pub fn run() -> Result<()> {
    header("Add a Claude profile");
    explain(
        "
        A \"profile\" is an isolated Claude install that runs alongside your existing
        one. Each profile has its own login, chats, settings, and MCP connectors.
        You typically want one for personal use and one for work, but you can
        create as many as you need (client A, client B, etc.).

        This tool does NOT touch your existing default Claude. Your current login
        and chats stay exactly as they are. We only set up the new profile next
        to it.
        ",
    );

    let targets = ui::select(
        "What do you want to set up for this profile?",
        &[
            ("Both Claude Desktop and Claude Code (recommended)".to_string(), "both"),
            ("Claude Desktop only (the GUI chat app)".to_string(), "desktop"),
            ("Claude Code only (the terminal CLI)".to_string(), "code"),
        ],
        0,
    )?;
    let wants_desktop = targets != "code";
    let wants_code = targets != "desktop";
    if wants_desktop && !is_mac() {
        warn(format!("Claude Desktop multi-profile setup only works on macOS. Detected: {}.", std::env::consts::OS));
        warn("Continuing with Claude Code setup only.");
    }
    let desktop_applicable = wants_desktop && is_mac();

    let gh_available = has_gh_cli();
    let raw = ui::input("Profile name (e.g. work, personal, client-acme):", None, |v| {
        let cleaned = sanitize_name(v);
        if cleaned.is_empty() {
            return Err("Name cannot be empty.".into());
        }
        if cleaned != v.trim().to_lowercase() {
            return Err(format!("Use lowercase letters, numbers, and hyphens only. Suggestion: \"{cleaned}\""));
        }
        if let Some(why) = reserved(&cleaned) {
            return Err(format!("\"{cleaned}\" is reserved. It would collide with {why}. Pick another name."));
        }
        let existing = registry::find_profile(&cleaned);
        if let (Some(shorter), None) = (without_claude_prefix(&cleaned), &existing) {
            return Err(format!(
                "Leave out \"claude-\": the tool adds it, so \"{cleaned}\" would become Claude {}.app and the command claude-{cleaned}. Suggestion: \"{shorter}\"",
                title_case(&cleaned)
            ));
        }
        if let Some(p) = &existing {
            if missing_targets(p, wants_desktop, wants_code).is_empty() && !can_enable_gh(p, gh_available) {
                return Err(format!("Profile \"{cleaned}\" already exists with everything you selected."));
            }
        }
        Ok(())
    })?;
    let name = sanitize_name(&raw);

    if let Some(existing) = registry::find_profile(&name) {
        let missing = missing_targets(&existing, wants_desktop, wants_code);
        if missing.is_empty() {
            return enable_gh_for_profile(existing);
        }
        return link_existing(existing, &missing);
    }

    let mut desktop = None;
    if desktop_applicable {
        desktop = ask_desktop_questions(&name)?;
        if desktop.is_none() && !wants_code {
            warn("Setup cancelled.");
            return Ok(());
        }
    }
    let mut code = None;
    if wants_code {
        code = ask_code_questions(&name)?;
        if code.is_none() && desktop.is_none() {
            return Ok(());
        }
    }

    step("Review");
    print_plan(&name, desktop.as_ref(), code.as_ref());
    if !ui::confirm("Apply this configuration?", true)? {
        warn("Cancelled. Nothing was changed.");
        return Ok(());
    }

    let (desktop_result, code_result) = apply(&name, desktop.as_ref(), code.as_ref())?;
    print_next_steps(&name, desktop_result.as_ref().map(|d| d.app_path.as_path()), code_result.as_ref());
    Ok(())
}

/// Create the profile from answered questions: both halves, the registry
/// entry, and every profile's read protection. Shared by the wizard and the
/// menu bar app.
pub fn apply(
    name: &str,
    desktop: Option<&DesktopSetup>,
    code: Option<&CodeSetup>,
) -> Result<(Option<crate::desktop::DesktopResult>, Option<crate::code::CodeResult>)> {
    let name = name.to_string();
    // The Code folder is handed to the Desktop launcher so Claude Code started
    // from inside Desktop uses it, rather than the shared ~/.claude.
    let gh_dir = code.filter(|c| c.isolate_gh).map(|c| default_gh_config_dir_for(&c.config_dir));
    let desktop_result = match desktop {
        Some(d) => Some(setup_desktop(d, code.map(|c| c.config_dir.as_path()), gh_dir.as_deref())?),
        None => None,
    };
    let code_result = match code {
        Some(c) => Some(setup_code(c)?),
        None => None,
    };

    registry::add(Profile {
        name: name.clone(),
        kind: Profile::kind_for(desktop_result.is_some(), code_result.is_some()).into(),
        desktop: desktop_result.as_ref().map(|d| DesktopTarget {
            data_dir: d.data_dir.clone(),
            app_path: d.app_path.clone(),
            claude_app_path: d.claude_app.clone(),
            color: d.color.clone(),
            self_update: None,
            extra: Map::new(),
        }),
        code: code_result.as_ref().map(|c| CodeTarget {
            config_dir: c.config_dir.clone(),
            alias_name: c.alias_name.clone(),
            shell: Some(c.shell.clone()),
            rc_path: Some(c.rc_path.clone()),
            gh_config_dir: c.gh_config_dir.clone(),
            extra: Map::new(),
        }),
        created_at: Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string(),
        extra: Map::new(),
    })?;

    // A first profile is built by this version, so there is nothing older
    // to bring up to date; without this, the next command would remind a
    // brand-new user to run doctor --fix.
    if registry::load().profiles.len() == 1 && crate::state::fixes_applied_for().is_none() {
        crate::state::record_fix_pass(crate::state::VERSION);
    }

    resync_deny_rules(&registry::load(), true);
    Ok((desktop_result, code_result))
}

/// Rebuild an existing profile's launcher so it exports the given folders.
fn rebuild_launcher(p: &Profile, code_dir: &Path, gh: Option<&Path>) -> Result<()> {
    let d = p.desktop.as_ref().expect("caller checked");
    let target = launch_target_for(&p.name, d);
    compile_app(&CompileSpec {
        name: &p.name,
        data_dir: &d.data_dir,
        app_path: &d.app_path,
        target: &target,
        code_config_dir: Some(code_dir),
        gh_config_dir: gh,
    })?;
    copy_claude_icon(&d.app_path, &d.claude_app_path);
    lsregister("-f", &d.app_path);
    Ok(())
}

// ---- Giving a complete profile its own GitHub CLI login ---------------------------------
//
// Both places a profile runs Claude Code have to export GH_CONFIG_DIR, or the
// isolation is half applied: the alias for the terminal, and the launcher for
// Claude Code opened from inside Desktop.

fn enable_gh_for_profile(profile: Profile) -> Result<()> {
    step(format!("Profile \"{}\" is already set up", profile.name));
    info("It has both Claude Desktop and Claude Code.");
    blank();
    explain(
        "
        One thing it does not have yet is its own GitHub CLI login. Right now any
        \"gh\" command Claude runs in this profile uses your machine's default
        GitHub account.

        Turning this on points GH_CONFIG_DIR at a folder inside the profile, so
        the profile can be signed in as a different GitHub account. You sign in
        once afterwards with \"gh auth login\".
        ",
    );
    if !ui::confirm(&format!("Give \"{}\" its own GitHub CLI login?", profile.name), true)? {
        warn("Nothing was changed.");
        return Ok(());
    }
    if let Some(v) = gh_token_override() {
        blank();
        warn(format!("{v} is set in your environment."));
        info("gh prefers that token over any config directory, so this profile");
        info("would still act as that token's account. Unset it in your shell");
        info("config for per-profile gh logins to take effect.");
        blank();
    }

    let code = profile.code.clone().expect("can_enable_gh checked");
    let gh = default_gh_config_dir_for(&code.config_dir);
    std::fs::create_dir_all(&gh)?;
    ok(format!("Created {}", path_str(tildify(&gh))));

    let (shell, rc) = profile_shell(&profile);
    add_alias(&code.alias_name, &code.config_dir, Some(&gh), &shell, &rc)?;
    ok(format!("Alias \"{}\" updated in {}.", code.alias_name, path_str(tildify(&rc))));

    if let Some(d) = &profile.desktop {
        if !file_exists(&d.claude_app_path) {
            warn(format!("Claude.app not found at {}; launcher left as is.", d.claude_app_path.display()));
            info("Claude Code opened from the Desktop app will keep using your default gh account.");
        } else {
            match rebuild_launcher(&profile, &code.config_dir, Some(&gh)) {
                Ok(()) => ok("Desktop launcher rebuilt so it exports the same gh config."),
                Err(e) => {
                    err(format!("Could not rebuild the launcher: {e:#}"));
                    info(format!("Run {} to retry.", command("claude-profiles doctor --fix")));
                }
            }
        }
    }

    let mut next = profile.clone();
    next.code = Some(CodeTarget { gh_config_dir: Some(gh.clone()), rc_path: Some(rc.clone()), ..code });
    registry::replace(&profile.name, next)?;

    blank();
    ok(format!("\"{}\" now has its own GitHub CLI config.", profile.name));
    blank();
    info("Sign in for this profile (run this once, from anywhere):");
    // Absolute: a quoted "~" would not expand.
    println!("  {}", command(format!("GH_CONFIG_DIR=\"{}\" gh auth login", path_string(&gh))));
    blank();
    info("Then reload your shell so the updated alias takes effect:");
    println!("  {}", command(format!("source {}", tildify(&rc))));
    explain(
        "
        After that, every \"gh\" command Claude runs inside this profile uses that
        account. If the Desktop app is open, quit and reopen it to pick up the
        rebuilt launcher.
        ",
    );
    Ok(())
}

// ---- Linking a missing half onto an existing profile ------------------------------------------
//
// Linking Code onto a Desktop profile is not just a config folder: the
// launcher bakes in CLAUDE_CONFIG_DIR at build time, so it is rebuilt too.
// Without that the link would look done while Desktop kept starting Claude
// Code on the shared ~/.claude.

fn link_existing(profile: Profile, missing: &[&str]) -> Result<()> {
    step(format!("Completing the existing profile \"{}\"", profile.name));
    let has: Vec<&str> = [profile.desktop.as_ref().map(|_| "Claude Desktop"), profile.code.as_ref().map(|_| "Claude Code")]
        .into_iter()
        .flatten()
        .collect();
    let adding: Vec<&str> = missing.iter().map(|m| if *m == "desktop" { "Claude Desktop" } else { "Claude Code" }).collect();
    let adding = adding.join(" and ");
    info(format!("It currently has: {}", has.join(" and ")));
    info(format!("Missing: {adding}"));
    blank();
    if missing.contains(&"code") && profile.desktop.is_some() {
        explain(
            "
            Right now, Claude Code started from inside this Desktop profile falls
            back to your shared ~/.claude, so its CLAUDE.md, settings, and project
            memory are the same ones your other profiles use.

            Linking gives it its own config folder and rebuilds the launcher so the
            Desktop app points Claude Code at it. Your Desktop chats, login, and
            settings are untouched.
            ",
        );
    }
    if !ui::confirm(&format!("Add {adding} to \"{}\"?", profile.name), true)? {
        warn("Cancelled. Nothing was changed.");
        return Ok(());
    }

    let mut desktop = None;
    let mut code = None;
    if missing.contains(&"desktop") {
        if !is_mac() {
            warn(format!("Claude Desktop setup requires macOS. Detected: {}.", std::env::consts::OS));
            return Ok(());
        }
        desktop = ask_desktop_questions(&profile.name)?;
        if desktop.is_none() {
            warn("Cancelled. Nothing was changed.");
            return Ok(());
        }
    }
    if missing.contains(&"code") {
        code = ask_code_questions(&profile.name)?;
        if code.is_none() {
            return Ok(());
        }
    }

    let mut next = profile.clone();
    if let Some(c) = &code {
        let r = setup_code(c)?;
        next.code = Some(CodeTarget {
            config_dir: r.config_dir,
            alias_name: r.alias_name,
            shell: Some(r.shell),
            rc_path: Some(r.rc_path),
            gh_config_dir: r.gh_config_dir,
            extra: Map::new(),
        });
    }

    if let Some(d) = &desktop {
        let code_dir = next.code.as_ref().map(|c| c.config_dir.clone());
        let gh = next.code.as_ref().and_then(|c| c.gh_config_dir.clone());
        let r = setup_desktop(d, code_dir.as_deref(), gh.as_deref())?;
        next.desktop = Some(DesktopTarget {
            data_dir: r.data_dir,
            app_path: r.app_path,
            claude_app_path: r.claude_app,
            color: r.color,
            self_update: None,
            extra: Map::new(),
        });
    } else if let (Some(c), Some(d)) = (&next.code, &profile.desktop) {
        step("Rebuilding the Desktop launcher");
        if !file_exists(&d.claude_app_path) {
            err(format!("Claude.app not found at {}; cannot rebuild the launcher.", d.claude_app_path.display()));
            info(format!("Fix that, then run {}.", command("claude-profiles doctor --fix")));
        } else {
            // Keep an existing gh isolation through the rebuild.
            match rebuild_launcher(&profile, &c.config_dir, c.gh_config_dir.as_deref()) {
                Ok(()) => ok("Launcher rebuilt. Claude Code opened from this Desktop profile now uses its own config."),
                Err(e) => {
                    err(format!("Could not rebuild the launcher: {e:#}"));
                    info(format!("Run {} to retry.", command("claude-profiles doctor --fix")));
                }
            }
        }
    }

    next.kind = Profile::kind_for(next.desktop.is_some(), next.code.is_some()).into();
    registry::replace(&profile.name, next.clone())?;
    resync_deny_rules(&registry::load(), true);

    blank();
    ok(format!("Profile \"{}\" now has Claude Desktop and Claude Code linked.", profile.name));
    if let (Some(_), Some(c)) = (&code, &next.code) {
        blank();
        info("Activate the shell alias in this terminal:");
        println!("  {}", command(format!("source {}", tildify(c.rc_path.as_deref().unwrap_or(Path::new("~/.zshrc"))))));
        blank();
        info("Then launch the profile's Claude Code with:");
        println!("  {}", command(&c.alias_name));
        explain(
            "
            On first run, use /login inside the REPL to sign in for this profile.

            If the Desktop app is open, quit and reopen it so it picks up the
            rebuilt launcher.
            ",
        );
    }
    Ok(())
}

// ---- Desktop questions -----------------------------------------------------------------------

fn ask_desktop_questions(name: &str) -> Result<Option<DesktopSetup>> {
    step("Claude Desktop configuration");
    explain(
        "
        Claude Desktop stores everything (login, chats, settings, MCP servers)
        in a single folder. Giving the new profile its own folder gives it its
        own login, chats and settings, separate from your existing Claude.

        We will also create a real macOS .app launcher for this profile so you
        can put it on your Dock and launch it like any other app.
        ",
    );

    let claude_app = match find_claude_app() {
        Some(p) => {
            info(format!("Found Claude Desktop at {}.", path_str(path_string(&p))));
            p
        }
        None => {
            warn("Claude.app was not found at any of the standard locations.");
            info("If you have Claude Desktop installed somewhere unusual, you can point us at it now.");
            let custom = ui::input("Path to Claude.app (or leave blank to skip Desktop setup):", None, |v| {
                if v.is_empty() {
                    return Ok(());
                }
                let p = expand_home(v);
                if !file_exists(&p) {
                    return Err(format!("Not found: {}", p.display()));
                }
                if !v.ends_with(".app") {
                    return Err("Path must end in .app".into());
                }
                Ok(())
            })?;
            if custom.is_empty() {
                return Ok(None);
            }
            expand_home(&custom)
        }
    };

    explain(
        "
        Where should the new profile's data live? The default puts it next to
        your current Claude data, both inside ~/Library/Application Support/.
        The folder will be created if it doesn't exist; nothing inside your
        existing ~/Library/Application Support/Claude folder will be touched.
        ",
    );
    let default_data = path_string(default_data_dir_for(name));
    let default_folder = home().join("Library").join("Application Support").join("Claude");
    let data_raw = ui::input("Data folder for this profile:", Some(&default_data), |v| {
        let p = expand_home(v.trim());
        if v.trim().is_empty() {
            return Err("Path cannot be empty.".into());
        }
        if p == default_folder {
            return Err("That's the default Claude folder. Pick a different path so the new profile stays isolated.".into());
        }
        Ok(())
    })?;
    let data_dir = expand_home(data_raw.trim());

    if is_unmanaged_existing_dir(&data_dir) {
        blank();
        warn(format!("{} already exists and isn't managed by this tool.", tildify(&data_dir)));
        explain(
            "
            This tool will treat that folder's contents as this profile's Desktop
            data, including when \"claude-profiles remove\" later offers to
            delete it. If it belongs to something else, choose a different path.
            ",
        );
        if !ui::confirm(&format!("Use {} anyway?", tildify(&data_dir)), false)? {
            warn("Cancelled. Re-run and choose a different data folder.");
            return Ok(None);
        }
    }

    explain(
        "
        We'll generate a small .app bundle that, when double-clicked, launches
        Claude with the right --user-data-dir flag for this profile. You can
        drag the .app to your Dock for one-click access.

        The default location is ~/Applications because it doesn't require
        administrator permission to write to. You can also use /Applications,
        but that might prompt for your password.
        ",
    );
    let default_app = path_string(default_app_path_for(name));
    let app_raw = ui::input("Where to save the launcher .app:", Some(&default_app), |v| {
        if !v.trim().ends_with(".app") {
            return Err("Path must end in .app".into());
        }
        Ok(())
    })?;
    let app_path = expand_home(app_raw.trim());

    let apply_icon = ui::confirm("Copy the Claude icon onto the launcher? (recommended)", true)?;

    explain(
        "
        Claude Desktop's Dock tile always shows the standard Claude icon, even
        when you customise the launcher, because the running window belongs to
        Claude itself rather than to the launcher.

        Giving this profile a colour works around that: it launches a private
        copy of Claude.app tinted that colour, so the running window's Dock tile
        is finally distinguishable. The copy is an APFS clone, which costs a few
        megabytes rather than a few hundred, and Anthropic's signature stays
        intact so your login keeps working.
        ",
    );
    let mut colors: Vec<(String, Option<String>)> = vec![("none (standard Claude icon)".into(), None)];
    colors.extend(COLORS.iter().map(|(c, _)| (c.to_string(), Some(c.to_string()))));
    let color = ui::select("Dock colour for this profile:", &colors, 0)?;

    // Offered only when there are some, and a choice: a connector's settings
    // often hold the default account's credentials.
    let mut seed_connectors = false;
    let servers = mcp_server_names(&default_desktop_config_file()).unwrap_or_default();
    if !servers.is_empty() {
        explain(&format!(
            "
            Your default Claude Desktop has {n} MCP connector{s} set up
            ({list}). We can copy them into this profile so you
            don't set them up again.

            Their settings come along as they are, including any API keys or tokens
            in them, which belong to your default account. Say no if this profile
            should connect with its own credentials; you can copy individual ones
            later with `claude-profiles extensions`.
            ",
            n = servers.len(),
            s = if servers.len() == 1 { "" } else { "s" },
            list = servers.join(", ")
        ));
        seed_connectors = ui::confirm("Copy your default profile's MCP connectors into this profile?", true)?;
    }

    Ok(Some(DesktopSetup { name: name.to_string(), data_dir, app_path, claude_app, apply_icon, color, seed_connectors }))
}

// ---- Code questions ------------------------------------------------------------------------------

fn ask_code_questions(name: &str) -> Result<Option<CodeSetup>> {
    step("Claude Code configuration");
    explain(
        "
        Claude Code (the terminal CLI) keeps everything under ~/.claude by
        default. We'll give this profile its own config directory and add a
        shell alias so you can launch it with a single command.
        ",
    );
    let default_dir = path_string(default_config_dir_for(name));
    let default_code = default_claude_config_dir();
    let raw = ui::input("Config folder for this profile:", Some(&default_dir), |v| {
        if v.trim().is_empty() {
            return Err("Path cannot be empty.".into());
        }
        if expand_home(v.trim()) == default_code {
            return Err("That's the default Claude Code folder. Pick a different path.".into());
        }
        Ok(())
    })?;
    let config_dir: PathBuf = expand_home(raw.trim());

    if is_unmanaged_existing_dir(&config_dir) {
        blank();
        warn(format!("{} already exists and isn't managed by this tool.", tildify(&config_dir)));
        explain(
            "
            Pointing a profile at an existing folder means this tool will treat its
            contents as that profile's data, including when you later run
            \"claude-profiles remove\", which offers to delete the folder.

            If this folder belongs to another tool (claude-mem, claude-profiles) or
            to a setup you made by hand, pick a different path instead.
            ",
        );
        if !ui::confirm(&format!("Use {} anyway?", tildify(&config_dir)), false)? {
            warn("Cancelled. Re-run and choose a different config folder.");
            return Ok(None);
        }
    }

    let alias_name = ui::input("Shell alias to launch this profile:", Some(&default_alias_name_for(name)), |v| {
        if v.trim().is_empty() {
            return Err("Alias cannot be empty.".into());
        }
        if v == "claude" {
            return Err("Don't shadow the bare `claude` command; pick a different alias.".into());
        }
        let mut chars = v.chars();
        let first_ok = chars.next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_');
        if !first_ok || !chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
            return Err("Alias must start with a letter and contain only letters, digits, hyphens, or underscores.".into());
        }
        Ok(())
    })?;

    let mut seed = false;
    if file_exists(&default_code) {
        explain(
            "
            You already have a Claude Code setup in ~/.claude. We can copy your
            setup into the new profile so you don't redo it: settings, CLAUDE.md,
            skills, slash commands, agents, hooks, plugins, and MCP servers.

            Settings and MCP servers are copied as they are, including any API
            keys or tokens in them. The new profile can use those credentials to
            access the same services as your default profile.

            Nothing from USING that account comes along: no conversations, no
            prompt history, no sessions, no caches. Claude's Keychain sign-in does
            not carry over; Claude Code keeps it under a key tied to
            the config folder, so you'll sign in fresh on first launch.
            ",
        );
        seed = ui::confirm("Copy your Claude Code setup, including any API keys or tokens, into the new profile?", true)?;
    }

    let mut isolate_gh = false;
    if has_gh_cli() {
        explain(
            "
            The GitHub CLI (gh) reads its logged-in account from a config folder,
            and it honors GH_CONFIG_DIR the same way Claude Code honors
            CLAUDE_CONFIG_DIR. We can give this profile its own, so any \"gh\"
            command Claude runs here acts as a separate GitHub account.

            Useful when a profile maps to a client or employer with its own GitHub
            org. Leave it off to keep using your current single gh login
            everywhere, which is what most setups want.
            ",
        );
        isolate_gh = ui::confirm("Give this profile its own GitHub CLI login?", false)?;
        if isolate_gh {
            if let Some(v) = gh_token_override() {
                warn(format!("{v} is set in your environment."));
                info("That overrides per-profile gh config, so every profile would use that token.");
                info("Unset it in your shell config for profile-specific gh logins to take effect.");
            }
        }
    }

    Ok(Some(CodeSetup { name: name.to_string(), config_dir, alias_name, seed, isolate_gh }))
}

// ---- Plan and next steps ---------------------------------------------------------------------------

fn yes_no(b: bool) -> &'static str {
    if b { "yes" } else { "no" }
}

fn print_plan(name: &str, desktop: Option<&DesktopSetup>, code: Option<&CodeSetup>) {
    println!("  Profile name: {}\n", path_str(name));
    if let Some(d) = desktop {
        println!("  Claude Desktop:");
        println!("    Data folder: {}", path_str(tildify(&d.data_dir)));
        println!("    Launcher app: {}", path_str(tildify(&d.app_path)));
        println!("    Apply Claude icon: {}", yes_no(d.apply_icon));
        println!("    Dock colour: {}", d.color.as_deref().unwrap_or("none"));
        println!("    Copy MCP connectors: {}\n", yes_no(d.seed_connectors));
    }
    if let Some(c) = code {
        println!("  Claude Code:");
        println!("    Config folder: {}", path_str(tildify(&c.config_dir)));
        println!("    Shell alias: {}", path_str(&c.alias_name));
        println!("    Copy Claude Code setup: {}", yes_no(c.seed));
        println!("    Own GitHub CLI login: {}\n", yes_no(c.isolate_gh));
    }
}

fn print_next_steps(name: &str, desktop_app: Option<&Path>, code: Option<&crate::code::CodeResult>) {
    blank();
    ok(format!("Profile \"{name}\" is ready."));
    blank();
    if let Some(app) = desktop_app {
        step("Next: sign in to Claude Desktop");
        explain(
            "
            The first time you launch the new Desktop profile, you'll need to sign
            in with the account that should belong to it.

            Easiest, with your other Claude still open: choose \"Continue with
            email\" and type the code from the email into the profile's window.
            Don't click the link in that email.

            Signing in with Google, or any way that finishes in your browser,
            answers through a claude:// link, and macOS can hand it to your other
            Claude instead, which ignores it. For those:

              1. Quit every other Claude window first (Cmd+Q from the menu bar).
              2. Double-click the new launcher (or run the open command below).
              3. Sign in with the account for this profile.

            Once it's signed in, both profiles can run at the same time. Open your
            default Claude from the Dock for the original account, and your new
            launcher for this one.
            ",
        );
        info("First-launch command (only needed if you didn't drag the .app yet):");
        println!("  {}", command(format!("open \"{}\"", path_string(app))));
        blank();
        explain(
            "
            To keep it in your Dock, drag the launcher itself from ~/Applications.

            Do NOT drag the Claude window's tile down while it is running. That
            tile is this profile's copy of Claude, not its launcher, so clicking it
            later starts the copy without its profile, on your default account.
            Clicking the launcher then offers to fix it, but pinning the launcher
            avoids the problem altogether.
            ",
        );
        blank();
    }
    if let Some(c) = code {
        step("Next: activate the shell alias");
        explain(
            "
            The alias was added to your shell config but won't be available in
            already-open terminal windows. Either open a new terminal tab, or
            reload your config in this one.
            ",
        );
        info("Reload your shell config:");
        println!("  {}", command(format!("source {}", tildify(&c.rc_path))));
        blank();
        info("Then launch your new profile with:");
        println!("  {}", command(&c.alias_name));
        blank();
        explain(
            "
            On the first run, you'll see Claude Code's normal login flow. Run
            /login inside the REPL and sign in with the account for this profile.
            The session is saved to the new config folder, so future launches
            keep you signed in.
            ",
        );
    }
    step("Done.");
    info(format!("Run {} to see all configured profiles.", command("claude-profiles list")));
    info(format!("Run {} for a health check.", command("claude-profiles status")));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile(desktop: bool, code: bool, gh: bool) -> Profile {
        Profile {
            name: "w".into(),
            kind: Profile::kind_for(desktop, code).into(),
            desktop: desktop.then(|| DesktopTarget {
                data_dir: "/d".into(),
                app_path: "/a.app".into(),
                claude_app_path: "/c.app".into(),
                color: None,
                self_update: None,
                extra: Map::new(),
            }),
            code: code.then(|| CodeTarget {
                config_dir: "/h/.claude-w".into(),
                alias_name: "claude-w".into(),
                shell: None,
                rc_path: None,
                gh_config_dir: gh.then(|| "/h/.claude-w/gh".into()),
                extra: Map::new(),
            }),
            created_at: String::new(),
            extra: Map::new(),
        }
    }

    #[test]
    fn an_existing_name_is_only_taken_when_nothing_is_left_to_add() {
        assert_eq!(missing_targets(&profile(true, false, false), true, true), vec!["code"]);
        assert_eq!(missing_targets(&profile(false, true, false), true, true), vec!["desktop"]);
        assert!(missing_targets(&profile(true, true, false), true, true).is_empty());
        assert!(missing_targets(&profile(true, false, false), true, false).is_empty());
    }

    #[test]
    fn a_complete_profile_can_still_gain_gh_isolation() {
        assert!(can_enable_gh(&profile(true, true, false), true));
        assert!(!can_enable_gh(&profile(true, true, false), false), "no gh installed");
        assert!(!can_enable_gh(&profile(true, true, true), true), "already isolated");
        assert!(!can_enable_gh(&profile(true, false, false), true), "no Code half");
    }
}
