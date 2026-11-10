// `claude-profiles upgrade [--no-fix] [--git URL | --path DIR]`
//
// Reinstall the tool with cargo, then apply the new version's changes to your
// profiles, in one step.
//
// Installing replaces the binary and nothing else: the launch helper,
// rebuilt launchers and moving profiles to a new way of working all happen in
// `doctor --fix`. So the new binary's doctor --fix is run straight after, as
// its own process, since this process is still the old code.
//
// Where it installs from: --git or --path when given, otherwise the
// repository in Cargo.toml. There is no registry to ask what the latest
// version is, so whatever that source holds is what gets installed; read it
// first if it is not your own.

use std::path::PathBuf;
use std::process::Command;

use anyhow::Result;

use super::fail;
use crate::claudebin::{binary_version, resolve_binaries};
use crate::registry;
use crate::state::{fix_pass_due, fixes_applied_for, VERSION};
use crate::ui::{command, dim, err, header, info, ok, warn};
use crate::util::{compare_versions, home, tildify};

const PKG_NAME: &str = "claude-profiles";

enum Source {
    Git(String),
    Path(PathBuf),
}

fn source_from(args: &[String]) -> Option<Source> {
    let value = |flag: &str| args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1)).cloned();
    if let Some(g) = value("--git") {
        return Some(Source::Git(g));
    }
    if let Some(p) = value("--path") {
        return Some(Source::Path(crate::util::expand_home(&p)));
    }
    let repo = env!("CARGO_PKG_REPOSITORY");
    (!repo.is_empty()).then(|| Source::Git(repo.to_string()))
}

/// The arguments for `cargo install`. --locked builds with the dependency
/// versions the source pinned, not whatever is newest today.
pub fn install_args(source: &str, is_git: bool) -> Vec<String> {
    let mut a = vec!["install".to_string(), "--locked".into(), "--force".into()];
    a.push(if is_git { "--git".into() } else { "--path".into() });
    a.push(source.to_string());
    a
}

/// Where cargo puts installed binaries.
fn cargo_bin_dir() -> PathBuf {
    std::env::var_os("CARGO_HOME").map(PathBuf::from).unwrap_or_else(|| home().join(".cargo")).join("bin")
}

fn apply_fixes(binary: &str, version: &str) {
    println!();
    header(&format!("Applying {version}'s changes to your profiles"));
    match Command::new(binary).args(["doctor", "--fix"]).status() {
        Ok(s) if s.success() => {}
        Ok(s) => warn(format!(
            "doctor --fix exited with status {}. Run {} to see what is left.",
            s.code().unwrap_or(-1),
            command("claude-profiles doctor")
        )),
        Err(e) => warn(format!("Could not run doctor --fix: {e}")),
    }
}

pub fn run(args: &[String]) -> Result<()> {
    header("Upgrade claude-profiles");
    let skip_fixes = args.iter().any(|a| a == "--no-fix");
    info(format!("Installed version: {VERSION}"));

    let Some(source) = source_from(args) else {
        err("No source to upgrade from.");
        info(format!(
            "Pass {} or {}, or set `repository` in Cargo.toml before building.",
            command("--git <url>"),
            command("--path <folder>")
        ));
        std::process::exit(1);
    };
    let (src, is_git) = match &source {
        Source::Git(u) => (u.clone(), true),
        Source::Path(p) => (p.display().to_string(), false),
    };
    info(format!("Installing from: {src}"));
    println!();

    let a = install_args(&src, is_git);
    println!("Running {}\n", command(format!("cargo {}", a.join(" "))));
    let status = Command::new("cargo").args(&a).status();
    match status {
        Ok(s) if s.success() => {}
        Ok(s) => {
            println!();
            err("Upgrade failed. See cargo's output above.");
            std::process::exit(s.code().unwrap_or(1));
        }
        Err(e) => fail(format!("Could not run cargo: {e}. Is the Rust toolchain installed?")),
    }
    println!();

    // With more than one copy on PATH, the install can succeed while the
    // command people actually run stays old.
    let copies = resolve_binaries(PKG_NAME);
    let Some(winner) = copies.first() else {
        ok(format!("Installed into {}.", tildify(cargo_bin_dir())));
        warn(format!("{} is not on PATH, so nothing could be verified.", tildify(cargo_bin_dir())));
        info(format!("Add it to PATH, then run {} to apply its changes.", command("claude-profiles doctor --fix")));
        return Ok(());
    };
    let installed = cargo_bin_dir().join(PKG_NAME);
    let reported = binary_version(winner).map(|v| v.trim().to_string());
    if std::fs::canonicalize(winner).ok() != std::fs::canonicalize(&installed).ok() {
        warn(format!("Installed {}, but your shell runs {} first.", tildify(&installed), tildify(winner)));
        if copies.len() > 1 {
            info("Other copies on PATH:");
            for p in &copies[1..] {
                println!("      {}", dim(tildify(p)));
            }
        }
        info(format!("Its changes were not applied: fix PATH first, then run {}.", command("claude-profiles doctor --fix")));
        return Ok(());
    }
    let new_version = reported.unwrap_or_else(|| "unknown".into());
    match compare_versions(&new_version, VERSION) {
        std::cmp::Ordering::Equal => ok(format!("Reinstalled {PKG_NAME} {new_version}.")),
        _ => ok(format!("Upgraded {PKG_NAME} {VERSION} → {new_version}, verified at {}.", tildify(winner))),
    }

    let profiles = registry::load().profiles.len();
    if skip_fixes {
        if fix_pass_due(&new_version, profiles, fixes_applied_for().as_deref()) {
            info(format!("Run {} when you're ready to apply its changes.", command("claude-profiles doctor --fix")));
        }
        return Ok(());
    }
    if profiles == 0 {
        return Ok(());
    }
    apply_fixes(winner, &new_version);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_args_are_locked_and_name_the_source() {
        assert_eq!(install_args("https://example.com/x.git", true), vec!["install", "--locked", "--force", "--git", "https://example.com/x.git"]);
        assert_eq!(install_args("/src/x", false), vec!["install", "--locked", "--force", "--path", "/src/x"]);
    }
}
