// `claude-profiles list`: the default install, the shared `claude`
// binary, and every profile. Showing the default install too means an empty
// registry reads as "nothing added yet" rather than "the tool sees nothing".

use anyhow::Result;

use crate::claudebin::{detect_defaults, resolve_claude};
use crate::registry::{self, Health};
use crate::ui::{command, dim, header, info, path_str, warn};
use crate::util::{path_string, print_launch_hints, tildify};

pub fn run() -> Result<()> {
    header("Claude installs and profiles");
    let defaults = detect_defaults();
    let reg = registry::load();

    if registry::health() == Health::Corrupt {
        warn(format!("The registry at {} is not valid JSON.", path_str(tildify(registry::registry_path()))));
        info("Profiles below may be incomplete. Fix the JSON by hand (a .bak may sit next to it).");
        println!();
    }

    if defaults.desktop.is_some() || defaults.code.is_some() {
        println!("  {} {}", path_str("default"), dim("(your existing Claude install, not managed by this tool)"));
        if let Some((app, data)) = &defaults.desktop {
            println!("    Desktop data:    {}", tildify(data));
            println!("    Desktop app:     {}", path_string(app));
        }
        if let Some(code) = &defaults.code {
            println!("    Code config:     {}", tildify(code));
        }
    } else {
        println!("  {}", dim("No default Claude install detected on this machine."));
    }
    println!();

    // Every profile runs this one binary, so which copy wins on PATH affects
    // them all at once.
    match resolve_claude() {
        Some(bin) => {
            println!("  {} {}", path_str("claude binary"), dim("(shared by all profiles)"));
            println!("    Path:            {}", tildify(&bin.winner));
            println!("    Version:         {}", bin.version.unwrap_or_else(|| dim("unknown")));
            if !bin.shadowed.is_empty() {
                let list: Vec<String> = bin.shadowed.iter().map(tildify).collect();
                println!("    {}", dim(format!("Also on PATH (shadowed): {}", list.join(", "))));
            }
            println!();
        }
        None => {
            warn("No `claude` binary found on PATH. Claude Code profiles won't launch.");
            println!();
        }
    }

    if reg.profiles.is_empty() {
        info("No additional profiles configured yet.");
        info(format!("Run {} to create one.", command("claude-profiles add")));
        return Ok(());
    }

    println!("  {}\n", dim("Additional profiles managed by claude-profiles:"));
    for p in &reg.profiles {
        println!("  {} ({})", path_str(&p.name), p.kind);
        if let Some(d) = &p.desktop {
            println!("    Desktop data:    {}", tildify(&d.data_dir));
            println!("    Desktop launcher: {}", tildify(&d.app_path));
        }
        if let Some(c) = &p.code {
            println!("    Code config:     {}", tildify(&c.config_dir));
            println!("    Code alias:      {}", c.alias_name);
        }
        println!("    Created:         {}", p.created_at.split('T').next().unwrap_or(""));
        print_launch_hints(p);
        println!();
    }
    info(format!("Registry file: {}", path_str(tildify(registry::registry_path()))));
    Ok(())
}
