// claude-profiles: run several Claude accounts side by side.
//
// A small argv dispatcher; there are a handful of commands and almost no
// flags, so a match is clearer than a framework.

mod appclone;
mod claudebin;
mod code;
mod commands;
mod desktop;
mod launcher;
mod permissions;
mod platform;
mod registry;
mod shell;
mod snapshot;
mod state;
mod ui;
mod updates;
mod util;

use crate::state::VERSION;
use crate::ui::{command, err, info};

const HELP: &str = "claude-profiles - run multiple Claude accounts side by side

USAGE
  claude-profiles <command> [options]

COMMANDS
  add                    Create a new profile (interactive wizard)
  list                   List configured profiles
  status                 Health-check all configured profiles
  doctor [--fix|--undo]  Diagnose the whole setup: which claude wins on PATH,
                         broken installs, directory collisions, and
                         cross-profile read protection (--fix repairs drift;
                         --undo puts back what the last --fix changed)
  extensions             Copy Claude Desktop extensions and MCP connectors
                         between profiles
                         (interactive: pick source, then target)
  rename [old] [new]     Rename a profile and move its folders, alias, and
                         launcher to match (interactive if no names given)
  repair [name]          Re-register a profile launcher with macOS LaunchServices
                         (interactive if no name given; fixes Dock icons that
                         stop responding to double-click)
  remove [name]          Remove a profile (interactive if no name given)
  self-update [name] [on|off]
                         Whether a profile's copy of Claude updates itself
                         (off by default: its launcher keeps it current)
  upgrade [--no-fix] [--git URL | --path DIR]
                         Reinstall this tool with cargo, then apply the new
                         version's changes to your profiles (--no-fix: install only)
  help                   Show this help
  version                Show the installed version

WHAT IS A PROFILE?
  A profile is an isolated Claude install that runs alongside your
  existing one. Each profile has its own login, chats, settings, and
  MCP connectors. Common uses:

    - Personal vs. work
    - Multiple clients
    - Separate testing or evaluation accounts

  This tool sets up profiles for both Claude Desktop (the GUI app) and
  Claude Code (the terminal CLI), independently or together.
";

/// `cargo install` replaces the binary but applies nothing to existing
/// profiles. Say so once per command, until doctor --fix has run.
fn remind_if_fixes_pending(cmd: &str) {
    if ["doctor", "upgrade", "help", "version", "app"].contains(&cmd) {
        return;
    }
    let count = registry::load().profiles.len();
    if !state::fix_pass_due(VERSION, count, state::fixes_applied_for().as_deref()) {
        return;
    }
    info(format!("claude-profiles is now {VERSION}, and its changes have not been applied to your profiles yet."));
    info(format!("Run {} to apply them.", command("claude-profiles doctor --fix")));
    println!();
}

fn pick_command() -> anyhow::Result<String> {
    println!("claude-profiles v{VERSION}\n");
    let items: Vec<(String, String)> = [
        ("add          Create a new profile (interactive wizard)", "add"),
        ("list         List configured profiles", "list"),
        ("status       Health-check all profiles", "status"),
        ("doctor       Diagnose PATH, installs, collisions, isolation", "doctor"),
        ("extensions   Copy Desktop extensions and connectors between profiles", "extensions"),
        ("rename       Rename a profile and move its folders/alias", "rename"),
        ("repair       Re-register a profile launcher with macOS", "repair"),
        ("remove       Remove a profile", "remove"),
        ("self-update  Show or change how each profile updates", "self-update"),
        ("upgrade      Upgrade claude-profiles to the latest version", "upgrade"),
        ("help         Show full help text", "help"),
        ("exit", "exit"),
    ]
    .iter()
    .map(|(l, v)| (l.to_string(), v.to_string()))
    .collect();
    ui::select("What would you like to do?", &items, 0)
}

fn dispatch(cmd: &str, rest: &[String]) -> anyhow::Result<()> {
    use commands::*;
    match cmd {
        "add" => add::run(),
        // The menu bar app's interface: JSON out, no prompts.
        "app" => app::run(rest),
        "list" => list::run(),
        "status" => status::run(),
        "doctor" => doctor::run(rest),
        "extensions" => extensions::run(rest),
        "rename" => rename::run(rest),
        "repair" => repair::run(rest),
        "remove" => remove::run(rest),
        "self-update" => selfupdate::run(rest),
        "upgrade" => upgrade::run(rest),
        _ => {
            err(format!("Unknown command: {cmd}"));
            println!("\n{HELP}");
            std::process::exit(1);
        }
    }
}

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let first = argv.first().map(String::as_str);

    match first {
        Some("help" | "--help" | "-h") => {
            println!("{HELP}");
            return;
        }
        Some("version" | "--version" | "-v") => {
            println!("{VERSION}");
            return;
        }
        // What a profile's launcher runs on every click. Never prompts on
        // the terminal and never fails loudly: see launcher.rs.
        Some("__launch") => {
            println!("{}", launcher::run(&argv[1..]));
            return;
        }
        _ => {}
    }

    let (cmd, rest): (String, Vec<String>) = match first {
        Some(c) => {
            remind_if_fixes_pending(c);
            (c.to_string(), argv[1..].to_vec())
        }
        None => {
            remind_if_fixes_pending("");
            match pick_command() {
                Ok(c) if c == "exit" => return,
                Ok(c) if c == "help" => {
                    println!("{HELP}");
                    return;
                }
                Ok(c) => (c, Vec::new()),
                Err(e) if e.is::<ui::Cancelled>() => {
                    println!();
                    return;
                }
                Err(e) => {
                    err(e.to_string());
                    std::process::exit(1);
                }
            }
        }
    };

    if let Err(e) = dispatch(&cmd, &rest) {
        if e.is::<ui::Cancelled>() {
            println!("\nCancelled.");
            std::process::exit(130);
        }
        err(format!("{e:#}"));
        std::process::exit(1);
    }
}
