// Claude Code profiles.
//
// Claude Code keeps everything under ~/.claude unless CLAUDE_CONFIG_DIR points
// elsewhere. A Code profile is a folder of its own plus a shell alias that
// sets the variable. Its sign-in is separate automatically: Claude Code keys
// its Keychain entry by the config folder.

use std::path::{Path, PathBuf};

use anyhow::{bail, Result};
use serde_json::{Map, Value};

use crate::claudebin::{default_claude_config_dir, resolve_binaries};
use crate::registry::Profile;
use crate::shell::{build_alias_line, detect_shell, rc_path_for_shell, read_managed_aliases, write_aliases};
use crate::ui::{info, ok, path_str, step, warn};
use crate::util::{
    clone_copy, create_private_dir, file_exists, file_stays_within_dir, home, read_json, tildify, to_json_pretty,
    write_private_file,
};

pub fn default_config_dir_for(name: &str) -> PathBuf {
    home().join(format!(".claude-{name}"))
}

/// The profile's own GitHub CLI config. Inside the profile on purpose, so
/// rename moves it and remove deletes it without knowing gh exists.
/// The Keychain entry Claude Code keeps a config folder's sign-in under: the
/// plain name for ~/.claude, otherwise suffixed with the first 8 hex digits
/// of the SHA-256 of the folder's path.
pub fn keychain_service(config_dir: &Path) -> String {
    use sha2::{Digest, Sha256};
    if config_dir == default_claude_config_dir() {
        return "Claude Code-credentials".into();
    }
    let hash = Sha256::digest(config_dir.as_os_str().as_encoded_bytes());
    let hex: String = hash.iter().map(|b| format!("{b:02x}")).collect();
    format!("Claude Code-credentials-{}", &hex[..8])
}

/// Move a Claude Code sign-in from one config folder's Keychain entry to
/// another's, for a rename. The secret goes to `security` as hex on stdin, so
/// it never shows in the process list, and the old entry is deleted only once
/// the new one reads back the same. Returns whether it moved.
pub fn move_keychain_login(from_dir: &Path, to_dir: &Path) -> bool {
    use std::io::Write;
    use std::process::{Command, Stdio};
    if from_dir == default_claude_config_dir() || to_dir == default_claude_config_dir() {
        return false;
    }
    let (from, to) = (keychain_service(from_dir), keychain_service(to_dir));
    let read = |service: &str| {
        crate::util::run_output("/usr/bin/security", ["find-generic-password", "-s", service, "-w"])
            .map(|s| s.trim_end_matches('\n').to_string())
    };
    let Some(secret) = read(&from) else { return false };
    // Claude Code files its entries under your login name.
    let account = std::env::var("USER").unwrap_or_else(|_| "claude".into());
    if account.contains(['"', '\\', '\n']) {
        return false;
    }
    let hex: String = secret.as_bytes().iter().map(|b| format!("{b:02x}")).collect();
    let Ok(mut child) = Command::new("/usr/bin/security")
        .arg("-i")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        return false;
    };
    let script = format!("add-generic-password -U -a \"{account}\" -s \"{to}\" -X {hex}\n");
    let wrote = child.stdin.take().is_some_and(|mut i| i.write_all(script.as_bytes()).is_ok());
    if !wrote || !child.wait().is_ok_and(|s| s.success()) || read(&to).as_deref() != Some(secret.as_str()) {
        return false;
    }
    crate::util::run_quiet("/usr/bin/security", ["delete-generic-password", "-s", from.as_str()]);
    true
}

/// Delete a profile's saved Claude Code sign-in. Never the default
/// account's: that one belongs to ~/.claude. Returns whether one was found.
pub fn delete_keychain_login(config_dir: &Path) -> bool {
    if config_dir == default_claude_config_dir() {
        return false;
    }
    let service = keychain_service(config_dir);
    let mut found = false;
    // One call removes one item; a re-login can have left a duplicate.
    for _ in 0..5 {
        if !crate::util::run_quiet("/usr/bin/security", ["delete-generic-password", "-s", service.as_str()]) {
            break;
        }
        found = true;
    }
    found
}

pub fn default_gh_config_dir_for(config_dir: &Path) -> PathBuf {
    config_dir.join("gh")
}

pub fn default_alias_name_for(name: &str) -> String {
    // Never the bare `claude`: shadowing it would surprise people and break
    // tooling that expects the original.
    format!("claude-{name}")
}

pub fn has_gh_cli() -> bool {
    !resolve_binaries("gh").is_empty()
}

/// gh prefers these over any config folder, so one exported globally defeats
/// per-profile gh logins. Returns the variable's name.
pub fn gh_token_override() -> Option<&'static str> {
    gh_token_override_in(|k| std::env::var(k).ok())
}

pub fn gh_token_override_in(get: impl Fn(&str) -> Option<String>) -> Option<&'static str> {
    ["GH_TOKEN", "GITHUB_TOKEN", "GH_ENTERPRISE_TOKEN"]
        .into_iter()
        .find(|v| get(v).is_some_and(|s| !s.is_empty()))
}

// ---- Seeding a new profile ------------------------------------------------------------
//
// An allowlist: what a new profile takes is named here, and everything else
// stays behind, including anything Anthropic adds later. The rule is "setup
// you would otherwise redo by hand", never "state from using an account", so
// conversations, history, sessions and caches never come along.

/// Top-level entries of ~/.claude that are setup.
pub const SEED_ITEMS: &[&str] =
    &["settings.json", "CLAUDE.md", "keybindings.json", "skills", "commands", "agents", "hooks", "output-styles"];

/// Inside plugins/: what is installed, not what plugins have stored. `data/`
/// is plugin state and `synced/` is tied to the signed-in account.
pub const SEED_PLUGIN_ITEMS: &[&str] = &[
    "installed_plugins.json",
    "known_marketplaces.json",
    "config.json",
    "blocklist.json",
    "cache",
    "marketplaces",
    "repos",
];

/// User-scope MCP servers live in ~/.claude.json, beside the account's
/// identity and usage; only its mcpServers entry is taken.
pub fn default_user_config_file() -> PathBuf {
    home().join(".claude.json")
}

/// Which keys of settings.json hold paths meaning "this profile's own copy".
/// Only these are rewritten: other keys, such as claudeMdExcludes, may point
/// at the default profile on purpose.
pub const SETTINGS_REBASE_KEYS: &[&str] = &["hooks", "statusLine"];

const MANIFESTS: &[&str] = &["installed_plugins.json", "known_marketplaces.json"];

fn copy_entry(from: &Path, to: &Path, independent: bool) -> Result<()> {
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if independent {
        let parent = to.parent().unwrap_or(Path::new("/"));
        if !file_stays_within_dir(to, parent) {
            bail!("Refusing to copy setup through an external symlink: {}", to.display());
        }
        // The contents, not a source link: rewriting paths must never edit
        // the source.
        std::fs::copy(from, to)?;
        return Ok(());
    }
    clone_copy(from, to)
}

/// Point paths that lead into `from_dir` at `to_dir` instead, in all the
/// forms people write them (absolute, ~/, $HOME/, ${HOME}/). A path ends at
/// whitespace or a quote, so "bash ~/.claude/hooks/x.sh --flag" keeps its
/// arguments. A path is only rewritten when `exists` says the new profile
/// has its own copy; otherwise it keeps working from where it was.
pub fn rebase_paths(text: &str, from_dir: &Path, to_dir: &Path, home: &Path, exists: &mut dyn FnMut(&str) -> bool) -> String {
    let rel = |d: &Path| -> Option<String> {
        d.strip_prefix(home).ok().map(|r| r.to_string_lossy().to_string()).filter(|r| !r.is_empty())
    };
    let (from_rel, to_rel) = (rel(from_dir), rel(to_dir));
    let to_abs = format!("{}/", to_dir.display());
    let mut forms = vec![(format!("{}/", from_dir.display()), to_abs.clone())];
    if let Some(fr) = &from_rel {
        for prefix in ["~/", "$HOME/", "${HOME}/"] {
            let to = match &to_rel {
                Some(tr) => format!("{prefix}{tr}/"),
                None => to_abs.clone(),
            };
            forms.push((format!("{prefix}{fr}/"), to));
        }
    }

    let is_end = |c: char| c.is_whitespace() || c == '"' || c == '\'' || c == '`';
    let mut out = text.to_string();
    for (from, to) in forms {
        let mut result = String::with_capacity(out.len());
        let mut rest = out.as_str();
        while let Some(i) = rest.find(&from) {
            result.push_str(&rest[..i]);
            let after = &rest[i + from.len()..];
            let end = after.find(is_end).unwrap_or(after.len());
            let tail = &after[..end];
            if exists(tail) {
                result.push_str(&to);
            } else {
                result.push_str(&from);
            }
            result.push_str(tail);
            rest = &after[end..];
        }
        result.push_str(rest);
        out = result;
    }
    out
}

fn map_strings(v: &Value, f: &mut dyn FnMut(&str) -> String) -> Value {
    match v {
        Value::String(s) => Value::String(f(s)),
        Value::Array(a) => Value::Array(a.iter().map(|x| map_strings(x, f)).collect()),
        Value::Object(m) => Value::Object(m.iter().map(|(k, x)| (k.clone(), map_strings(x, f))).collect()),
        other => other.clone(),
    }
}

fn rebase_value(v: &Value, from: &Path, to: &Path, keys: Option<&[&str]>) -> Value {
    let h = home();
    let mut f = |s: &str| rebase_paths(s, from, to, &h, &mut |rel: &str| to.join(rel).exists());
    match (keys, v) {
        (None, _) => map_strings(v, &mut f),
        (Some(keys), Value::Object(m)) => Value::Object(
            m.iter()
                .map(|(k, x)| (k.clone(), if keys.contains(&k.as_str()) { map_strings(x, &mut f) } else { x.clone() }))
                .collect(),
        ),
        _ => v.clone(),
    }
}

/// Rewrite a JSON file's strings with rebase_paths, limited to the values
/// under `keys` when given. A file that does not parse, or that lives
/// through a link out of `to_dir`, is left alone. True when it changed.
pub fn rebase_json_file(file: &Path, from_dir: &Path, to_dir: &Path, keys: Option<&[&str]>) -> bool {
    if !file_stays_within_dir(file, to_dir) {
        return false;
    }
    let Some(before) = read_json(file) else { return false };
    let after = rebase_value(&before, from_dir, to_dir, keys);
    if after == before {
        return false;
    }
    std::fs::write(file, to_json_pretty(&after)).is_ok()
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct PathCounts {
    /// Paths into the other profile that this profile has its own copy of.
    pub rebasable: usize,
    /// Paths it has no copy of, which must keep pointing where they do.
    pub stuck: usize,
}

/// For doctor: how many paths in a JSON file still lead into `from_dir`.
pub fn paths_into_other_profile(file: &Path, from_dir: &Path, to_dir: &Path, keys: Option<&[&str]>) -> PathCounts {
    let mut counts = PathCounts::default();
    let Some(parsed) = read_json(file) else { return counts };
    let scope = match (keys, &parsed) {
        (Some(keys), Value::Object(m)) => {
            Value::Object(m.iter().filter(|(k, _)| keys.contains(&k.as_str())).map(|(k, v)| (k.clone(), v.clone())).collect())
        }
        (Some(_), _) => Value::Object(Map::new()),
        (None, _) => parsed,
    };
    let text = serde_json::to_string(&scope).unwrap_or_default();
    rebase_paths(&text, from_dir, to_dir, &home(), &mut |rel: &str| {
        if to_dir.join(rel).exists() {
            counts.rebasable += 1;
        } else {
            counts.stuck += 1;
        }
        false
    });
    counts
}

/// claudeMdExcludes entries that an old version of the original tool turned
/// from the default profile's CLAUDE.md into the profile's own, which reversed
/// what they were for. Returns (from, to) for each entry that would change;
/// with `apply`, writes the file.
pub fn flipped_claude_md_excludes(settings_file: &Path, profile_dir: &Path, default_dir: &Path, apply: bool) -> Vec<(String, String)> {
    if !file_stays_within_dir(settings_file, profile_dir) {
        return Vec::new();
    }
    let Some(Value::Object(mut settings)) = read_json(settings_file) else { return Vec::new() };
    let Some(list) = settings.get("claudeMdExcludes").and_then(|v| v.as_array()).cloned() else { return Vec::new() };
    let h = home();
    let mut changes = Vec::new();
    let restored: Vec<Value> = list
        .into_iter()
        .map(|entry| match entry {
            Value::String(s) => {
                let back = rebase_paths(&s, profile_dir, default_dir, &h, &mut |rel: &str| default_dir.join(rel).exists());
                if back != s {
                    changes.push((s, back.clone()));
                }
                Value::String(back)
            }
            other => other,
        })
        .collect();
    if apply && !changes.is_empty() {
        settings.insert("claudeMdExcludes".into(), Value::Array(restored));
        let _ = std::fs::write(settings_file, to_json_pretty(&settings));
    }
    changes
}

/// Conversations an older, whole-folder seed copied in: transcripts present
/// in both profiles under the same project and session ID, which can only
/// have come from a copy.
pub fn copied_transcripts(profile_dir: &Path, source_dir: &Path) -> usize {
    fn walk(dir: &Path, rel: &str, out: &mut std::collections::HashSet<String>) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            let r = if rel.is_empty() { name.clone() } else { format!("{rel}/{name}") };
            match e.file_type() {
                Ok(t) if t.is_dir() => walk(&e.path(), &r, out),
                _ if name.ends_with(".jsonl") => {
                    out.insert(r);
                }
                _ => {}
            }
        }
    }
    let list = |root: &Path| {
        let mut s = std::collections::HashSet::new();
        walk(&root.join("projects"), "", &mut s);
        s
    };
    let theirs = list(source_dir);
    list(profile_dir).iter().filter(|r| theirs.contains(*r)).count()
}

#[derive(Debug, Default)]
pub struct SeedSummary {
    pub items: Vec<String>,
    pub plugins: bool,
    pub mcp_servers: usize,
}

pub fn seed_config_dir(from_dir: &Path, to_dir: &Path, user_config_file: &Path) -> Result<SeedSummary> {
    create_private_dir(to_dir)?;
    let mut summary = SeedSummary::default();

    for item in SEED_ITEMS {
        let src = from_dir.join(item);
        if !src.exists() {
            continue;
        }
        copy_entry(&src, &to_dir.join(item), *item == "settings.json")?;
        summary.items.push(item.to_string());
    }

    let plugins_from = from_dir.join("plugins");
    if plugins_from.exists() {
        for item in SEED_PLUGIN_ITEMS {
            let src = plugins_from.join(item);
            if src.exists() {
                copy_entry(&src, &to_dir.join("plugins").join(item), MANIFESTS.contains(item))?;
            }
        }
        for m in MANIFESTS {
            let f = to_dir.join("plugins").join(m);
            if f.exists() {
                rebase_json_file(&f, from_dir, to_dir, None);
            }
        }
        summary.plugins = true;
    }

    let settings = to_dir.join("settings.json");
    if settings.exists() {
        rebase_json_file(&settings, from_dir, to_dir, Some(SETTINGS_REBASE_KEYS));
    }

    // MCP servers: the one entry of ~/.claude.json that is setup. Written
    // owner-only, since it can hold tokens.
    if let Some(servers) = read_json(user_config_file).and_then(|v| v.get("mcpServers").cloned()) {
        let count = servers.as_object().map(|m| m.len()).unwrap_or(0);
        if count > 0 {
            let file = to_dir.join(".claude.json");
            let mut obj = Map::new();
            obj.insert("mcpServers".into(), servers);
            write_private_file(&file, &to_json_pretty(&obj))?;
            rebase_json_file(&file, from_dir, to_dir, None);
            summary.mcp_servers = count;
        }
    }
    Ok(summary)
}

/// Create the config folder. An existing folder is left exactly as it is:
/// it may be a partly completed earlier run the user wants to keep.
/// Returns None when it already existed.
pub fn ensure_config_dir(config_dir: &Path, seed: bool) -> Result<Option<SeedSummary>> {
    if config_dir.exists() {
        return Ok(None);
    }
    let default = default_claude_config_dir();
    if seed && default.exists() {
        return seed_config_dir(&default, config_dir, &default_user_config_file()).map(Some);
    }
    create_private_dir(config_dir)?;
    Ok(Some(SeedSummary::default()))
}

// ---- Aliases ---------------------------------------------------------------------------------

/// The shell and rc file an existing profile's alias lives in: what it
/// recorded, falling back to the current shell only for entries that predate
/// the record. Using $SHELL instead would make remove leave the alias behind
/// whenever it runs from a different shell.
pub fn profile_shell(profile: &Profile) -> (String, PathBuf) {
    let code = profile.code.as_ref();
    let shell = code.and_then(|c| c.shell.clone()).unwrap_or_else(detect_shell);
    let rc = code.and_then(|c| c.rc_path.clone()).unwrap_or_else(|| rc_path_for_shell(&shell));
    (shell, rc)
}

pub fn current_shell() -> (String, PathBuf) {
    let shell = detect_shell();
    let rc = rc_path_for_shell(&shell);
    (shell, rc)
}

/// Add or replace one alias. The whole block is rebuilt, sorted, every time.
pub fn add_alias(alias: &str, config_dir: &Path, gh: Option<&Path>, shell: &str, rc: &Path) -> Result<()> {
    let mut lines: Vec<String> =
        read_managed_aliases(rc).into_iter().filter(|a| a.name != alias).map(|a| a.line).collect();
    lines.push(build_alias_line(shell, alias, config_dir, gh));
    lines.sort();
    write_aliases(&lines, rc)?;
    Ok(())
}

pub fn remove_alias(alias: &str, rc: &Path) -> Result<()> {
    let mut lines: Vec<String> =
        read_managed_aliases(rc).into_iter().filter(|a| a.name != alias).map(|a| a.line).collect();
    lines.sort();
    write_aliases(&lines, rc)?;
    Ok(())
}

// ---- Setup -------------------------------------------------------------------------------------

pub struct CodeSetup {
    pub name: String,
    pub config_dir: PathBuf,
    pub alias_name: String,
    pub seed: bool,
    pub isolate_gh: bool,
}

pub struct CodeResult {
    pub config_dir: PathBuf,
    pub alias_name: String,
    pub shell: String,
    pub rc_path: PathBuf,
    pub gh_config_dir: Option<PathBuf>,
}

pub fn setup_code(c: &CodeSetup) -> Result<CodeResult> {
    step(format!("Creating Claude Code profile \"{}\"", c.name));
    let gh = c.isolate_gh.then(|| default_gh_config_dir_for(&c.config_dir));

    info(format!("Config folder: {}", path_str(tildify(&c.config_dir))));
    info(format!("Shell alias: {}", path_str(&c.alias_name)));
    if let Some(g) = &gh {
        info(format!("GitHub CLI config: {}", path_str(tildify(g))));
    }

    match ensure_config_dir(&c.config_dir, c.seed)? {
        Some(summary) if c.seed && file_exists(default_claude_config_dir()) => {
            let mut parts = summary.items.clone();
            if summary.plugins {
                parts.push("plugins".into());
            }
            if summary.mcp_servers > 0 {
                parts.push(format!("{} MCP server{}", summary.mcp_servers, if summary.mcp_servers == 1 { "" } else { "s" }));
            }
            ok(format!("Config folder created and seeded from {}.", path_str(tildify(default_claude_config_dir()))));
            if parts.is_empty() {
                ok("There was no setup to carry over.");
            } else {
                ok(format!("Carried over: {}.", parts.join(", ")));
            }
            info("Not carried over: conversations, prompt history, sessions, caches, or Keychain sign-in.");
        }
        Some(_) => ok("Config folder created (empty)."),
        None => warn(format!("Config folder already existed; left untouched. ({})", path_str(tildify(&c.config_dir)))),
    }

    if let Some(g) = &gh {
        // gh writes its own files at first sign-in; the folder gives the
        // alias somewhere real to point and `gh auth login` a home.
        std::fs::create_dir_all(g)?;
        ok("GitHub CLI config folder ready (sign in separately with `gh auth login`).");
    }

    let (shell, rc) = current_shell();
    add_alias(&c.alias_name, &c.config_dir, gh.as_deref(), &shell, &rc)?;
    ok(format!("Alias \"{}\" added to {} (shell: {shell}).", c.alias_name, path_str(tildify(&rc))));

    Ok(CodeResult {
        config_dir: c.config_dir.clone(),
        alias_name: c.alias_name.clone(),
        shell,
        rc_path: rc,
        gh_config_dir: gh,
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn keychain_service_matches_claude_code() {
        assert_eq!(
            super::keychain_service(std::path::Path::new("/Users/onches/.claude-onchezrynn-cloude")),
            "Claude Code-credentials-7556b214"
        );
        assert_eq!(super::keychain_service(&super::default_claude_config_dir()), "Claude Code-credentials");
    }

    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn fake_claude_home(root: &Path) -> (PathBuf, PathBuf) {
        let src = root.join(".claude");
        for d in ["skills/s1", "commands", "projects/p1", "plugins/cache/m/p", "plugins/data/x", "hooks"] {
            std::fs::create_dir_all(src.join(d)).unwrap();
        }
        std::fs::write(src.join("settings.json"), r#"{"model":"opus"}"#).unwrap();
        std::fs::write(src.join("CLAUDE.md"), "be kind").unwrap();
        std::fs::write(src.join("skills/s1/SKILL.md"), "x").unwrap();
        std::fs::write(src.join("projects/p1/abc.jsonl"), "{}").unwrap();
        std::fs::write(src.join("history.jsonl"), "{}").unwrap();
        std::fs::write(src.join(".credentials.json"), "{}").unwrap();
        std::fs::write(src.join("plugins/data/x/state"), "x").unwrap();
        std::fs::write(src.join("hooks/x.sh"), "#!/bin/sh").unwrap();
        (src, root.join(".claude.json"))
    }

    #[test]
    fn seed_copies_setup_never_conversations_history_or_credentials() {
        let root = tempfile::tempdir().unwrap();
        let (src, user_cfg) = fake_claude_home(root.path());
        let dst = root.path().join("profile");
        let s = seed_config_dir(&src, &dst, &user_cfg).unwrap();
        assert!(dst.join("settings.json").exists());
        assert!(dst.join("CLAUDE.md").exists());
        assert!(dst.join("skills/s1/SKILL.md").exists());
        assert!(dst.join("plugins/cache/m/p").exists());
        assert!(!dst.join("projects").exists());
        assert!(!dst.join("history.jsonl").exists());
        assert!(!dst.join(".credentials.json").exists());
        assert!(!dst.join("plugins/data").exists());
        assert!(s.plugins);
        assert_eq!(s.mcp_servers, 0);
    }

    #[test]
    fn seeded_profile_and_copied_credentials_are_owner_only() {
        let root = tempfile::tempdir().unwrap();
        let (src, user_cfg) = fake_claude_home(root.path());
        std::fs::write(&user_cfg, r#"{"mcpServers":{"github":{"command":"demo","env":{"GITHUB_TOKEN":"dummy"}}},"userID":"secret"}"#).unwrap();
        let dst = root.path().join("profile");
        let s = seed_config_dir(&src, &dst, &user_cfg).unwrap();
        assert_eq!(s.mcp_servers, 1);
        assert_eq!(std::fs::metadata(&dst).unwrap().permissions().mode() & 0o777, 0o700);
        let f = dst.join(".claude.json");
        assert_eq!(std::fs::metadata(&f).unwrap().permissions().mode() & 0o777, 0o600);
        let v = read_json(&f).unwrap();
        assert_eq!(v["mcpServers"]["github"]["env"]["GITHUB_TOKEN"], "dummy");
        assert!(v.get("userID").is_none(), "nothing but mcpServers comes along");
    }

    #[test]
    fn new_empty_profiles_are_private() {
        let root = tempfile::tempdir().unwrap();
        let dst = root.path().join("p");
        create_private_dir(&dst).unwrap();
        assert_eq!(std::fs::metadata(&dst).unwrap().permissions().mode() & 0o777, 0o700);
    }

    #[test]
    fn rebase_handles_every_form_and_never_a_longer_sibling() {
        let home = Path::new("/Users/me");
        let from = home.join(".claude");
        let to = home.join(".claude-work");
        let mut yes = |_: &str| true;
        assert_eq!(
            rebase_paths("bash ~/.claude/hooks/x.sh --flag", &from, &to, home, &mut yes),
            "bash ~/.claude-work/hooks/x.sh --flag"
        );
        assert_eq!(rebase_paths("$HOME/.claude/a ${HOME}/.claude/b", &from, &to, home, &mut yes), "$HOME/.claude-work/a ${HOME}/.claude-work/b");
        assert_eq!(rebase_paths("/Users/me/.claude/plugins/x", &from, &to, home, &mut yes), "/Users/me/.claude-work/plugins/x");
        assert_eq!(rebase_paths("~/.claude-mem/x", &from, &to, home, &mut yes), "~/.claude-mem/x");
        let mut no = |_: &str| false;
        assert_eq!(rebase_paths("~/.claude/hooks/x.sh", &from, &to, home, &mut no), "~/.claude/hooks/x.sh");
    }

    #[test]
    fn seeded_paths_follow_the_copy_only_where_it_exists() {
        let root = tempfile::tempdir().unwrap();
        let (src, user_cfg) = fake_claude_home(root.path());
        let settings = format!(
            r#"{{"hooks":{{"Stop":[{{"command":"{s}/hooks/x.sh"}},{{"command":"{s}/hooks/missing.sh"}}]}},"claudeMdExcludes":["{s}/CLAUDE.md"]}}"#,
            s = src.display()
        );
        std::fs::write(src.join("settings.json"), settings).unwrap();
        let dst = root.path().join("profile");
        seed_config_dir(&src, &dst, &user_cfg).unwrap();
        let v = read_json(&dst.join("settings.json")).unwrap();
        assert_eq!(v["hooks"]["Stop"][0]["command"], format!("{}/hooks/x.sh", dst.display()));
        assert_eq!(v["hooks"]["Stop"][1]["command"], format!("{}/hooks/missing.sh", src.display()));
        assert_eq!(v["claudeMdExcludes"][0], format!("{}/CLAUDE.md", src.display()), "not a rebase key");
        // The source was never touched.
        assert!(std::fs::read_to_string(src.join("settings.json")).unwrap().contains(&format!("{}/hooks/x.sh", src.display())));
    }

    #[test]
    fn linked_settings_become_independent_copies() {
        let root = tempfile::tempdir().unwrap();
        let (src, user_cfg) = fake_claude_home(root.path());
        let shared = root.path().join("shared-settings.json");
        std::fs::write(&shared, format!(r#"{{"hooks":{{"a":"{}/hooks/x.sh"}}}}"#, src.display())).unwrap();
        std::fs::remove_file(src.join("settings.json")).unwrap();
        std::os::unix::fs::symlink(&shared, src.join("settings.json")).unwrap();
        let dst = root.path().join("profile");
        seed_config_dir(&src, &dst, &user_cfg).unwrap();
        assert!(!std::fs::symlink_metadata(dst.join("settings.json")).unwrap().file_type().is_symlink());
        assert!(std::fs::read_to_string(&shared).unwrap().contains(&src.display().to_string()), "the shared file is untouched");
    }

    #[test]
    fn rebase_json_leaves_files_linked_out_alone() {
        let root = tempfile::tempdir().unwrap();
        let to = root.path().join("to");
        std::fs::create_dir_all(&to).unwrap();
        let outside = root.path().join("outside.json");
        std::fs::write(&outside, r#"{"a":"/from/x"}"#).unwrap();
        std::os::unix::fs::symlink(&outside, to.join("settings.json")).unwrap();
        assert!(!rebase_json_file(&to.join("settings.json"), Path::new("/from"), &to, None));
    }

    #[test]
    fn transcripts_and_stuck_paths_are_counted_for_doctor() {
        let root = tempfile::tempdir().unwrap();
        let (src, _) = fake_claude_home(root.path());
        let dst = root.path().join("profile");
        std::fs::create_dir_all(dst.join("projects/p1")).unwrap();
        std::fs::write(dst.join("projects/p1/abc.jsonl"), "{}").unwrap();
        std::fs::write(dst.join("projects/p1/own.jsonl"), "{}").unwrap();
        assert_eq!(copied_transcripts(&dst, &src), 1);

        std::fs::create_dir_all(dst.join("hooks")).unwrap();
        std::fs::write(dst.join("hooks/x.sh"), "").unwrap();
        let f = dst.join("settings.json");
        std::fs::write(&f, format!(r#"{{"hooks":{{"a":"{s}/hooks/x.sh","b":"{s}/hooks/gone.sh"}},"other":"{s}/hooks/x.sh"}}"#, s = src.display())).unwrap();
        assert_eq!(paths_into_other_profile(&f, &src, &dst, Some(SETTINGS_REBASE_KEYS)), PathCounts { rebasable: 1, stuck: 1 });
        assert_eq!(paths_into_other_profile(&f, &src, &dst, None), PathCounts { rebasable: 2, stuck: 1 });
    }

    #[test]
    fn flipped_excludes_are_pointed_back() {
        let root = tempfile::tempdir().unwrap();
        let (src, _) = fake_claude_home(root.path());
        let dst = root.path().join("profile");
        std::fs::create_dir_all(&dst).unwrap();
        let f = dst.join("settings.json");
        std::fs::write(&f, format!(r#"{{"claudeMdExcludes":["{d}/CLAUDE.md","/elsewhere/CLAUDE.md"]}}"#, d = dst.display())).unwrap();
        let changes = flipped_claude_md_excludes(&f, &dst, &src, true);
        assert_eq!(changes.len(), 1);
        let v = read_json(&f).unwrap();
        assert_eq!(v["claudeMdExcludes"][0], format!("{}/CLAUDE.md", src.display()));
        assert_eq!(v["claudeMdExcludes"][1], "/elsewhere/CLAUDE.md");
    }

    #[test]
    fn gh_token_override_names_the_variable() {
        assert_eq!(gh_token_override_in(|k| (k == "GITHUB_TOKEN").then(|| "x".into())), Some("GITHUB_TOKEN"));
        assert_eq!(gh_token_override_in(|k| (k == "GH_TOKEN").then(String::new)), None);
        assert_eq!(gh_token_override_in(|_| None), None);
    }
}
