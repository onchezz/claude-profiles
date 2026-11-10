// `claude-profiles status`: does everything the registry names still
// exist, on disk and in the shell config? And does the terminal you are in
// have the aliases that are on disk?

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use anyhow::Result;
use chrono::{DateTime, Local, Utc};

use crate::claudebin::{detect_defaults, resolve_claude};
use crate::code::{current_shell, profile_shell};
use crate::registry::{self, Health, Profile};
use crate::shell::{alias_session_state, find_session_shell, read_alias_stamp, read_managed_aliases, SessionState};
use crate::ui::{command, dim, header, info, ok, path_str, warn};
use crate::util::{file_exists, path_string, print_launch_hints, tildify};

/// Aliases present in each rc file, read once per file. Each profile is
/// checked against the rc file it recorded, not the one for $SHELL.
#[derive(Default)]
pub struct AliasCache(HashMap<PathBuf, HashSet<String>>);

impl AliasCache {
    pub fn has(&mut self, p: &Profile) -> (bool, PathBuf) {
        let (_, rc) = profile_shell(p);
        let names = self
            .0
            .entry(rc.clone())
            .or_insert_with(|| read_managed_aliases(&rc).into_iter().map(|a| a.name).collect());
        let alias = p.code.as_ref().map(|c| c.alias_name.as_str()).unwrap_or("");
        (names.contains(alias), rc)
    }
}

fn mark(okay: bool) -> &'static str {
    if okay { "✓" } else { "✗" }
}

pub fn run() -> Result<()> {
    header("Claude profiles status");
    let defaults = detect_defaults();
    let reg = registry::load();
    let (shell, rc) = current_shell();
    let mut aliases = AliasCache::default();

    if registry::health() == Health::Corrupt {
        warn(format!("The registry at {} is not valid JSON.", path_str(tildify(registry::registry_path()))));
        info("Profiles below may be incomplete. Fix the JSON by hand (a .bak may sit next to it).");
        println!();
    }

    println!("  {} {}", path_str("default"), dim("(existing Claude install, not managed by this tool)"));
    match &defaults.desktop {
        Some((app, data)) => {
            println!("    ✓ Desktop app:    {}", path_string(app));
            println!("    ✓ Desktop data:   {}", tildify(data));
        }
        None => println!("    {}", dim("Claude Desktop: not detected (either not installed or never launched)")),
    }
    match &defaults.code {
        Some(c) => println!("    ✓ Code config:    {}", tildify(c)),
        None => println!("    {}", dim("Claude Code: not detected (either not installed or never launched)")),
    }
    println!();

    println!("  {} {}", path_str("claude binary"), dim("(shared by all profiles)"));
    match resolve_claude() {
        Some(bin) => {
            println!("    ✓ Path:           {}", tildify(&bin.winner));
            println!("    ✓ Version:        {}", bin.version.unwrap_or_else(|| dim("unknown")));
            if !bin.shadowed.is_empty() {
                println!("    ⚠ {} other copy/copies on PATH (shadowed):", bin.shadowed.len());
                for p in &bin.shadowed {
                    println!("        {}", dim(tildify(p)));
                }
            }
        }
        None => println!("    ✗ Not found on PATH. Claude Code profiles won't launch."),
    }
    println!();

    if reg.profiles.is_empty() {
        info("No additional profiles configured.");
        info(format!("Run {} to create one.", command("claude-profiles add")));
        return Ok(());
    }

    println!("  {}\n", dim("Additional profiles managed by claude-profiles:"));
    for p in &reg.profiles {
        println!("  {} ({})", path_str(&p.name), p.kind);
        let mut issues = 0;
        if let Some(d) = &p.desktop {
            for (label, path, display) in [
                ("Desktop data folder", &d.data_dir, tildify(&d.data_dir)),
                ("Launcher app", &d.app_path, tildify(&d.app_path)),
                ("Claude.app source", &d.claude_app_path, path_string(&d.claude_app_path)),
            ] {
                let okay = file_exists(path);
                println!("    {} {label}: {display}", mark(okay));
                issues += usize::from(!okay);
            }
        }
        if let Some(c) = &p.code {
            let cfg_ok = file_exists(&c.config_dir);
            println!("    {} Code config folder: {}", mark(cfg_ok), tildify(&c.config_dir));
            issues += usize::from(!cfg_ok);
            let (alias_ok, alias_rc) = aliases.has(p);
            println!("    {} Shell alias \"{}\" in {}", mark(alias_ok), c.alias_name, tildify(&alias_rc));
            issues += usize::from(!alias_ok);
        }
        if issues == 0 {
            ok("    All checks passed.");
        } else {
            warn(format!("    {issues} issue{} detected.", if issues > 1 { "s" } else { "" }));
            info(format!("    Re-run {} for this profile, or fix manually.", command("claude-profiles add")));
        }
        print_launch_hints(p);
        println!();
    }

    info(format!("Registry: {}", path_str(tildify(registry::registry_path()))));
    info(format!("Shell: {shell} ({})", path_str(tildify(&rc))));
    report_stale_session(&rc, find_session_shell().map(|s| s.started_at), read_alias_stamp(&rc));
    Ok(())
}

/// Everything above checks the files. This checks whether this terminal
/// agrees with them: a shell reads its rc file once, so one opened before the
/// last add, rename or remove still has the old aliases, and after a rename
/// the old alias still runs against a folder that has moved.
pub fn report_stale_session(rc: &std::path::Path, started: Option<DateTime<Utc>>, written: Option<DateTime<Utc>>) {
    if alias_session_state(started, written) != SessionState::Stale {
        return;
    }
    let when = |t: DateTime<Utc>| t.with_timezone(&Local).format("%b %-d, %H:%M").to_string();
    println!();
    warn("This terminal is older than the aliases on disk.");
    info(format!(
        "  Session opened {}, aliases last changed {}.",
        when(started.expect("stale implies known")),
        when(written.expect("stale implies known"))
    ));
    info("  The aliases above are correct in the file but not yet loaded here.");
    info(format!("  Run {} or open a new terminal.", command(crate::shell::reload_command(rc))));
}
