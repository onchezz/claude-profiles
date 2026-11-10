// `claude-profiles app <action> ...`: the interface the menu bar app uses.
//
// Never prompts. Progress text goes to stderr (ui::set_quiet), and a success
// prints one JSON object on stdout. A failure exits non-zero with the reason
// on stderr. Everything the app does goes through here, so the app holds no
// logic of its own beyond showing state and asking for confirmation.
//
//   state                         everything the panel shows
//   sizes                         folder and copy sizes (slow; asked separately)
//   add --name N --targets both|desktop|code [--color C] [--seed-code]
//       [--seed-connectors] [--gh] [--adopt]
//   remove NAME [--delete-desktop-data] [--delete-code-data]
//   rename OLD NEW
//   self-update NAME on|off
//   repair NAME
//   open-desktop NAME | open-default-desktop
//   quit-desktop NAME             quit the profile's running copy of Claude
//   open-cli NAME|default [--dir FOLDER] [--terminal Terminal|iTerm|Ghostty|Warp]
//   cli-command NAME|default      the shell command that starts that account
//   stop-session PID              end one Claude Code session (SIGTERM)
//   doctor-fix                    run doctor --fix, output on stderr
//   in-terminal doctor|add|extensions|status [--terminal T]
//                                 run an interactive command in a terminal

use std::collections::HashMap;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};

use super::add::{apply, is_unmanaged_existing_dir};
use super::{reserved, without_claude_prefix};
use crate::appclone::{app_version, clone_path_for, clone_state, has_custom_icon, CloneState, COLORS};
use crate::claudebin::{default_claude_config_dir, default_desktop_data_dir, find_claude_app, resolve_claude};
use crate::code::{keychain_service, default_alias_name_for, default_config_dir_for, default_user_config_file, has_gh_cli, CodeSetup};
use crate::desktop::{default_app_path_for, default_data_dir_for, desktop_account_uuid, shell_quote, DesktopSetup};
use crate::launcher::{helper_state, install_helper, on_profile, HelperState};
use crate::registry::{self, Profile};
use crate::updates::{update_block_state, wants_update_block, BlockState};
use crate::util::{file_exists, is_mac, path_string, read_json, run_output, run_quiet, sanitize_name};

// ---- Reading the machine ------------------------------------------------------------------

/// One line of `ps`: pid, resident memory in KB, CPU percent, and the
/// command line with the environment after it.
struct Proc {
    pid: i32,
    rss_kb: u64,
    cpu: f64,
    comm: String,
    command: String,
    elapsed: String,
}

fn processes() -> Vec<Proc> {
    // `comm` is the bare executable name; `command` with -E carries the
    // environment, which is how a session's CLAUDE_CONFIG_DIR is found. Both
    // are only visible for the user's own processes, which is all we want.
    let out = run_output("/bin/ps", ["-axwwE", "-o", "pid=,rss=,%cpu=,etime=,ucomm=,command="]).unwrap_or_default();
    out.lines()
        .filter_map(|l| {
            let mut f = l.split_whitespace();
            let pid = f.next()?.parse().ok()?;
            let rss_kb = f.next()?.parse().unwrap_or(0);
            let cpu = f.next()?.parse().unwrap_or(0.0);
            let elapsed = f.next()?.to_string();
            let comm = f.next()?.to_string();
            let command = f.collect::<Vec<_>>().join(" ");
            Some(Proc { pid, rss_kb, cpu, comm, command, elapsed })
        })
        .collect()
}

/// The value of `name` in a process's environment, as `ps -E` prints it.
fn env_value(command: &str, name: &str) -> Option<String> {
    let key = format!(" {name}=");
    let i = command.find(&key)? + key.len();
    let rest = &command[i..];
    // The value runs to the next " KEY=" (environment entries are
    // space-separated; a path with spaces would otherwise be cut short).
    let mut end = rest.len();
    let bytes = rest.as_bytes();
    let mut j = 0;
    while j < bytes.len() {
        if bytes[j] == b' ' {
            let word: String = rest[j + 1..].chars().take_while(|c| *c != ' ' && *c != '=').collect();
            let is_key = !word.is_empty()
                && word.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
                && rest[j + 1 + word.len()..].starts_with('=');
            if is_key {
                end = j;
                break;
            }
        }
        j += 1;
    }
    Some(rest[..end].to_string())
}

fn cwd_of(pid: i32) -> Option<String> {
    let out = run_output("/usr/sbin/lsof", ["-a", "-p", &pid.to_string(), "-d", "cwd", "-Fn"])?;
    out.lines().find_map(|l| l.strip_prefix('n')).map(String::from)
}

/// Claude Code sessions, grouped by the config folder they run with (the
/// default ~/.claude when none is set).
fn sessions(procs: &[Proc]) -> HashMap<PathBuf, Vec<Value>> {
    let default = default_claude_config_dir();
    let mut out: HashMap<PathBuf, Vec<Value>> = HashMap::new();
    for p in procs.iter().filter(|p| p.comm == "claude") {
        let dir = env_value(&p.command, "CLAUDE_CONFIG_DIR").map(PathBuf::from).unwrap_or_else(|| default.clone());
        // Sessions Claude Desktop starts for its Code tab live under its app
        // folders; say so, since stopping one affects the app.
        let from_desktop = p.command.contains("/Application Support/Claude") || p.command.contains("/claude-code/");
        out.entry(dir).or_default().push(json!({
            "pid": p.pid,
            "cwd": cwd_of(p.pid),
            "uptime": p.elapsed,
            "memoryMB": p.rss_kb / 1024,
            "cpu": p.cpu,
            "fromDesktop": from_desktop,
        }));
    }
    out
}

/// Memory and CPU of everything running from one app bundle, helpers included.
fn bundle_usage(procs: &[Proc], bundle: &Path) -> (u64, f64, Vec<(i32, String)>) {
    let needle = format!("{}/Contents/", bundle.display());
    let main = format!("{}/Contents/MacOS/", bundle.display());
    let (mut mem, mut cpu, mut mains) = (0, 0.0, Vec::new());
    for p in procs.iter().filter(|p| p.command.contains(&needle)) {
        mem += p.rss_kb;
        cpu += p.cpu;
        if p.command.contains(&main) {
            mains.push((p.pid, p.command.clone()));
        }
    }
    (mem / 1024, cpu, mains)
}

/// Who a Claude Code config folder is signed in as, from its .claude.json.
/// Only identity fields are read; tokens live in the Keychain, never here.
fn code_account(config_file: &Path) -> Value {
    let Some(v) = read_json(config_file) else { return Value::Null };
    let Some(o) = v.get("oauthAccount").filter(|o| o.is_object()) else { return Value::Null };
    json!({
        "email": o.get("emailAddress"),
        "name": o.get("displayName").or_else(|| o.get("fullName")),
        "organization": o.get("organizationName"),
        "accountUuid": o.get("accountUuid"),
        "plan": o.get("billingType").or_else(|| o.get("seatTier")),
    })
}

fn code_config_file(config_dir: &Path) -> PathBuf {
    if config_dir == default_claude_config_dir() {
        default_user_config_file()
    } else {
        config_dir.join(".claude.json")
    }
}

fn update_mode(p: &Profile) -> &'static str {
    let d = p.desktop.as_ref().expect("Desktop profiles only");
    if !wants_update_block(p) {
        return "self";
    }
    match update_block_state(&d.data_dir) {
        BlockState::Blocked => "launcher",
        BlockState::Foreign(_) => "own-config",
        BlockState::Absent => "pending",
    }
}

fn state() -> Result<Value> {
    let reg = registry::load();
    let procs = processes();
    let mut sess = sessions(&procs);
    let installed = find_claude_app();
    let installed_version = installed.as_deref().and_then(app_version);
    let cli = resolve_claude();

    // Desktop records only an account ID; a Code profile on the same account
    // records the email, so match them up where possible.
    let mut emails: HashMap<String, Value> = HashMap::new();
    let mut remember = |acct: &Value| {
        if let Some(id) = acct.get("accountUuid").and_then(|v| v.as_str()) {
            emails.insert(id.to_string(), acct.clone());
        }
    };
    let default_code_account = code_account(&default_user_config_file());
    remember(&default_code_account);
    let code_accounts: HashMap<String, Value> = reg
        .profiles
        .iter()
        .filter_map(|p| p.code.as_ref().map(|c| (p.name.clone(), code_account(&code_config_file(&c.config_dir)))))
        .collect();
    for a in code_accounts.values() {
        remember(a);
    }
    let names = crate::state::account_names();
    let desktop_account = |dir: &Path| -> Value {
        match desktop_account_uuid(dir) {
            None => Value::Null,
            Some(id) => json!({ "accountUuid": id, "matched": emails.get(&id), "nickname": names.get(&id) }),
        }
    };

    let mut profiles = Vec::new();
    for p in &reg.profiles {
        let desktop = p.desktop.as_ref().map(|d| {
            let clone = clone_path_for(&p.name);
            let exists = file_exists(&clone);
            let (mem, cpu, mains) = bundle_usage(&procs, &clone);
            let data = path_string(&d.data_dir);
            let wrong: Vec<i32> = mains.iter().filter(|(_, c)| !on_profile(c, &data)).map(|(pid, _)| *pid).collect();
            json!({
                "dataDir": d.data_dir,
                "appPath": d.app_path,
                "launcherExists": file_exists(&d.app_path),
                "color": d.color,
                "updates": update_mode(p),
                "account": desktop_account(&d.data_dir),
                "clone": {
                    "path": clone,
                    "exists": exists,
                    "version": app_version(&clone),
                    "state": match clone_state(&clone, &d.claude_app_path) {
                        CloneState::Missing => "missing",
                        CloneState::Current => "current",
                        CloneState::Behind => "behind",
                        CloneState::Ahead => "ahead",
                        CloneState::Unknown => "unknown",
                    },
                    "colorIntact": d.color.is_none() || has_custom_icon(&clone),
                    "running": !mains.is_empty(),
                    "wrongAccountPids": wrong,
                    "memoryMB": mem,
                    "cpu": (cpu * 10.0).round() / 10.0,
                },
            })
        });
        let code = p.code.as_ref().map(|c| {
            json!({
                "configDir": c.config_dir,
                "alias": c.alias_name,
                "ghConfigDir": c.gh_config_dir,
                "account": code_accounts.get(&p.name),
                "sessions": sess.remove(&c.config_dir).unwrap_or_default(),
            })
        });
        profiles.push(json!({
            "name": p.name,
            "kind": p.kind,
            "createdAt": p.created_at,
            "desktop": desktop,
            "code": code,
        }));
    }

    let default_desktop_data = default_desktop_data_dir();
    let default_usage = installed.as_deref().map(|a| bundle_usage(&procs, a));
    let default_sessions = sess.remove(&default_claude_config_dir()).unwrap_or_default();
    // Sessions on a config folder no profile owns (set by hand, or a profile
    // that has since been removed).
    let other: Vec<Value> = sess
        .into_iter()
        .map(|(dir, list)| json!({ "configDir": dir, "sessions": list }))
        .collect();

    Ok(json!({
        "version": crate::state::VERSION,
        "isMac": is_mac(),
        "helper": match helper_state() {
            HelperState::Current => "current",
            HelperState::Outdated => "outdated",
            HelperState::Missing => "missing",
        },
        "ghAvailable": has_gh_cli(),
        "colors": COLORS.iter().map(|(c, _)| *c).collect::<Vec<_>>(),
        "claude": {
            "desktopApp": installed.as_ref().map(|a| json!({ "path": a, "version": installed_version })),
            "cli": cli.map(|b| json!({ "path": b.winner, "version": b.version, "shadowed": b.shadowed })),
        },
        "default": {
            "desktop": file_exists(&default_desktop_data).then(|| json!({
                "dataDir": default_desktop_data,
                "account": desktop_account(&default_desktop_data),
                "running": default_usage.as_ref().is_some_and(|u| !u.2.is_empty()),
                "memoryMB": default_usage.as_ref().map(|u| u.0).unwrap_or(0),
                "cpu": default_usage.as_ref().map(|u| (u.1 * 10.0).round() / 10.0).unwrap_or(0.0),
            })),
            "code": {
                "configDir": default_claude_config_dir(),
                "account": default_code_account,
                "sessions": default_sessions,
            },
        },
        "otherSessions": other,
        "profiles": profiles,
    }))
}

/// Disk use, in MB, as `du` reports it. A copy of Claude.app is an APFS clone
/// that shares its blocks with the original, so its figure is what it would
/// take if it ever stopped sharing, not what it takes now.
fn du_mb(p: &Path) -> Option<u64> {
    if !file_exists(p) {
        return None;
    }
    let out = run_output("/usr/bin/du", [OsStr::new("-sk"), p.as_os_str()])?;
    out.split_whitespace().next()?.parse::<u64>().ok().map(|kb| kb / 1024)
}

fn sizes() -> Value {
    let reg = registry::load();
    let profiles: Vec<Value> = reg
        .profiles
        .iter()
        .map(|p| {
            json!({
                "name": p.name,
                "desktopDataMB": p.desktop.as_ref().and_then(|d| du_mb(&d.data_dir)),
                "cloneMB": p.desktop.as_ref().and_then(|_| du_mb(&clone_path_for(&p.name))),
                "codeConfigMB": p.code.as_ref().and_then(|c| du_mb(&c.config_dir)),
            })
        })
        .collect();
    let default = json!({
        "name": "default",
        "desktopDataMB": du_mb(&default_desktop_data_dir()),
        "cloneMB": null,
        "codeConfigMB": du_mb(&default_claude_config_dir()),
    });
    json!({ "default": default, "profiles": profiles })
}

// ---- Claude Code's URL handler -----------------------------------------------------------------

/// Give Claude Code's "Claude Code URL Handler.app" Claude's icon. Claude
/// Code makes that app (it opens claude-cli:// links) with no icon, so it
/// shows as a blank grid in Launchpad, and makes it again when it updates,
/// so this runs each time the menu bar app starts. The app is a bare
/// Info.plist plus a link to the claude binary: there is no bundle signature
/// for an added icon to break. Returns whether it changed anything.
fn url_handler_icon() -> bool {
    let handler = crate::util::home().join("Applications/Claude Code URL Handler.app");
    let plist = handler.join("Contents/Info.plist");
    if crate::appclone::plist_value(&handler, "CFBundleIdentifier").as_deref() != Some("com.anthropic.claude-code-url-handler") {
        return false;
    }
    let icon = handler.join("Contents/Resources/ClaudeIcon.icns");
    if file_exists(&icon) && crate::appclone::plist_value(&handler, "CFBundleIconFile").as_deref() == Some("ClaudeIcon") {
        return false;
    }
    let Some(claude) = find_claude_app() else { return false };
    let source_name = crate::appclone::plist_value(&claude, "CFBundleIconFile").unwrap_or_else(|| "electron".into());
    let source_name = if source_name.ends_with(".icns") { source_name } else { format!("{source_name}.icns") };
    let source = claude.join("Contents/Resources").join(source_name);
    if std::fs::create_dir_all(handler.join("Contents/Resources")).is_err() || std::fs::copy(&source, &icon).is_err() {
        return false;
    }
    use crate::desktop::plist_buddy;
    if plist_buddy("Set :CFBundleIconFile ClaudeIcon", &plist).is_none()
        && plist_buddy("Add :CFBundleIconFile string ClaudeIcon", &plist).is_none()
    {
        return false;
    }
    // Make Launchpad and Finder notice the new icon.
    let _ = filetime_touch(&handler);
    super::lsregister("-f", &handler);
    true
}

fn filetime_touch(p: &Path) -> bool {
    run_quiet("/usr/bin/touch", [p.as_os_str()])
}

// ---- Import -----------------------------------------------------------------------------------

/// A profile name guessed from a folder name: "Claude-Work" and ".claude-work"
/// both give "work".
fn guess_name(folder: &str) -> String {
    let lower = folder.trim_start_matches('.').to_lowercase();
    let rest = lower.strip_prefix("claude").unwrap_or(&lower).trim_start_matches(['-', ' ', '_']);
    let name = sanitize_name(rest);
    if name.is_empty() { "imported".into() } else { name }
}

fn is_registered(dir: &Path) -> bool {
    registry::load().profiles.iter().any(|p| {
        p.code.as_ref().is_some_and(|c| c.config_dir == dir) || p.desktop.as_ref().is_some_and(|d| d.data_dir == dir)
    })
}

/// A Claude Desktop data folder this tool does not manage: a Claude-*
/// sibling of the default one (the original tool's profiles, or one made
/// by hand) that Electron has written to.
fn desktop_candidates() -> Vec<PathBuf> {
    let base = default_desktop_data_dir();
    let Some(parent) = base.parent() else { return vec![] };
    let Ok(entries) = std::fs::read_dir(parent) else { return vec![] };
    let mut out: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            let n = p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
            (n.starts_with("Claude-") || n.starts_with("Claude "))
                && !n.ends_with("-3p")
                && !n.starts_with("Claude Extensions")
                && p.join("Local State").exists()
                && !is_registered(p)
        })
        .collect();
    out.sort();
    out
}

/// A Claude Code config folder this tool does not manage: a hidden folder
/// in your home with Claude Code's .claude.json in it, or the folder of a
/// session running right now.
fn code_candidates(running: impl IntoIterator<Item = PathBuf>) -> Vec<PathBuf> {
    let default = default_claude_config_dir();
    let other_tool = crate::util::home().join(".claude-profiles");
    let mut out: Vec<PathBuf> = std::fs::read_dir(crate::util::home())
        .map(|it| it.flatten().map(|e| e.path()).collect())
        .unwrap_or_default();
    out.retain(|p| {
        p.file_name().is_some_and(|n| n.to_string_lossy().starts_with('.')) && p.join(".claude.json").is_file()
    });
    out.extend(running.into_iter().filter(|p| p.is_dir()));
    out.retain(|p| *p != default && *p != other_tool && !is_registered(p));
    out.sort();
    out.dedup();
    out
}

/// Accounts already on this Mac that could become profiles, with Desktop and
/// Code folders of the same name paired up.
fn importable() -> Value {
    let running: Vec<PathBuf> = sessions(&processes()).into_keys().collect();
    let mut found: Vec<(String, Option<PathBuf>, Option<PathBuf>)> = Vec::new();
    for d in desktop_candidates() {
        let name = guess_name(&d.file_name().unwrap_or_default().to_string_lossy());
        found.push((name, Some(d), None));
    }
    for c in code_candidates(running.clone()) {
        let name = guess_name(&c.file_name().unwrap_or_default().to_string_lossy());
        match found.iter_mut().find(|f| f.0 == name && f.2.is_none()) {
            Some(f) => f.2 = Some(c),
            None => found.push((name, None, Some(c))),
        }
    }
    let list: Vec<Value> = found
        .into_iter()
        .map(|(name, desktop, code)| {
            // A name already taken gets a number, so Import works as offered.
            let mut suggested = name.clone();
            let mut n = 2;
            while registry::find_profile(&suggested).is_some() || reserved(&suggested).is_some() {
                suggested = format!("{name}-{n}");
                n += 1;
            }
            json!({
                "name": suggested,
                "desktopDir": desktop,
                "desktopAccount": desktop.as_deref().and_then(desktop_account_uuid),
                "codeDir": code,
                "codeAccount": code.as_deref().map(|c| code_account(&c.join(".claude.json"))),
                "running": code.as_ref().is_some_and(|c| running.contains(c)),
            })
        })
        .collect();
    json!({ "candidates": list })
}

/// Make a profile from folders already in use. Nothing in them is touched:
/// sign-ins, chats and settings carry on as they were, and Claude Code finds
/// its Keychain sign-in because the folder path is unchanged.
fn import(args: &[String]) -> Result<Value> {
    let name = new_profile_name(args)?;
    let desktop_dir = value(args, "--desktop-dir").map(PathBuf::from);
    let code_dir = value(args, "--code-dir").map(PathBuf::from);
    if desktop_dir.is_none() && code_dir.is_none() {
        bail!("Name a --desktop-dir or a --code-dir to import.");
    }
    // Only what importable() would offer, so a hand-made call cannot adopt
    // the default folders or another profile's.
    if let Some(d) = &desktop_dir {
        if !desktop_candidates().contains(d) {
            bail!("{} is not a Claude Desktop data folder that can be imported.", d.display());
        }
    }
    if let Some(c) = &code_dir {
        let running: Vec<PathBuf> = sessions(&processes()).into_keys().collect();
        if !code_candidates(running).contains(c) {
            bail!("{} is not a Claude Code config folder that can be imported.", c.display());
        }
    }
    let desktop = match desktop_dir {
        Some(data_dir) => Some(DesktopSetup {
            name: name.clone(),
            data_dir,
            app_path: default_app_path_for(&name),
            claude_app: find_claude_app().context("Claude.app was not found in /Applications or ~/Applications.")?,
            apply_icon: true,
            color: value(args, "--color").filter(|c| !c.is_empty() && *c != "none").map(String::from),
            seed_connectors: false,
        }),
        None => None,
    };
    let code = code_dir.map(|config_dir| CodeSetup {
        name: name.clone(),
        config_dir,
        alias_name: default_alias_name_for(&name),
        seed: false,
        isolate_gh: false,
    });
    apply(&name, desktop.as_ref(), code.as_ref())?;
    Ok(json!({ "name": name }))
}

// ---- Plan usage -------------------------------------------------------------------------------

/// The plan usage of the account a config folder is signed in to, the same
/// figures Claude Code's /usage shows. Read-only: an expired token is
/// reported, never refreshed, since refreshing would rotate the sign-in out
/// from under Claude Code.
fn code_usage(config_dir: &Path) -> Value {
    let service = keychain_service(config_dir);
    let Some(raw) = run_output("/usr/bin/security", ["find-generic-password", "-s", service.as_str(), "-w"]) else {
        return json!({ "status": "signed-out" });
    };
    let Some(oauth) = serde_json::from_str::<Value>(raw.trim()).ok().and_then(|v| v.get("claudeAiOauth").cloned()) else {
        return json!({ "status": "signed-out" });
    };
    let Some(token) = oauth.get("accessToken").and_then(Value::as_str) else {
        return json!({ "status": "signed-out" });
    };
    let expires = oauth.get("expiresAt").and_then(Value::as_i64).unwrap_or(i64::MAX);
    if expires <= chrono::Utc::now().timestamp_millis() {
        return json!({ "status": "expired" });
    }
    match fetch_usage(token) {
        Some(u) => {
            let limit = |key: &str| {
                u.get(key).filter(|v| v.is_object()).map(|v| {
                    json!({ "percent": v.get("utilization"), "resetsAt": v.get("resets_at") })
                })
            };
            json!({
                "status": "ok",
                "fiveHour": limit("five_hour"),
                "sevenDay": limit("seven_day"),
                "sevenDayOpus": limit("seven_day_opus"),
                "sevenDaySonnet": limit("seven_day_sonnet"),
            })
        }
        None => json!({ "status": "unavailable" }),
    }
}

/// GET the usage endpoint with curl. The token goes in through stdin as curl
/// config, so it never shows in the process list.
fn fetch_usage(token: &str) -> Option<Value> {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let mut child = Command::new("/usr/bin/curl")
        .args(["-sf", "-m", "10", "-K", "-", "https://api.anthropic.com/api/oauth/usage"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let config = format!(
        "header = \"Authorization: Bearer {token}\"\nheader = \"anthropic-beta: oauth-2025-04-20\"\nheader = \"Content-Type: application/json\"\n"
    );
    child.stdin.take()?.write_all(config.as_bytes()).ok()?;
    let out = child.wait_with_output().ok()?;
    out.status.success().then(|| serde_json::from_slice(&out.stdout).ok()).flatten()
}

/// Usage for the default Claude Code and every Code profile, fetched in
/// parallel. Kept out of `state` so a slow network never holds the panel up.
fn usage() -> Value {
    let reg = registry::load();
    let mut dirs: Vec<(Option<String>, PathBuf)> = vec![(None, default_claude_config_dir())];
    dirs.extend(reg.profiles.iter().filter_map(|p| p.code.as_ref().map(|c| (Some(p.name.clone()), c.config_dir.clone()))));
    let handles: Vec<_> = dirs
        .into_iter()
        .map(|(name, dir)| std::thread::spawn(move || (name, code_usage(&dir))))
        .collect();
    let mut default = Value::Null;
    let mut profiles = Vec::new();
    for h in handles {
        let Ok((name, u)) = h.join() else { continue };
        match name {
            None => default = u,
            Some(n) => profiles.push(json!({ "name": n, "usage": u })),
        }
    }
    json!({ "default": default, "profiles": profiles })
}

// ---- Actions ----------------------------------------------------------------------------------

fn flag(args: &[String], name: &str) -> bool {
    args.iter().any(|a| a == name)
}

fn value<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).map(String::as_str)
}

fn need<'a>(args: &'a [String], i: usize, what: &str) -> Result<&'a str> {
    args.get(i).map(String::as_str).with_context(|| format!("missing {what}"))
}

fn profile(name: &str) -> Result<Profile> {
    registry::find_profile(name).with_context(|| format!("No profile called \"{name}\"."))
}

/// The same checks the wizard makes, with its defaults for everything the
/// app does not ask.
/// A new profile's name, checked the way `add` checks it.
fn new_profile_name(args: &[String]) -> Result<String> {
    let raw = value(args, "--name").context("missing --name")?;
    let name = sanitize_name(raw);
    if name.is_empty() {
        bail!("The name needs at least one letter or number.");
    }
    if name != raw.trim().to_lowercase() {
        bail!("Use lowercase letters, numbers, and hyphens only. Suggestion: \"{name}\"");
    }
    if let Some(why) = reserved(&name) {
        bail!("\"{name}\" is reserved. It would collide with {why}.");
    }
    if let Some(s) = without_claude_prefix(&name) {
        bail!("Leave out \"claude-\": the tool adds it. Suggestion: \"{s}\"");
    }
    if registry::find_profile(&name).is_some() {
        bail!("Profile \"{name}\" already exists. To add a missing half to it, run `claude-profiles add` in a terminal.");
    }
    Ok(name)
}

fn add(args: &[String]) -> Result<Value> {
    let name = new_profile_name(args)?;
    let targets = value(args, "--targets").unwrap_or("both");
    let (wants_desktop, wants_code) = match targets {
        "both" => (true, true),
        "desktop" => (true, false),
        "code" => (false, true),
        other => bail!("--targets must be both, desktop or code, not \"{other}\""),
    };
    let adopt = flag(args, "--adopt");

    let desktop = if wants_desktop {
        if !is_mac() {
            bail!("Claude Desktop profiles need macOS.");
        }
        let claude_app = find_claude_app().context("Claude.app was not found in /Applications or ~/Applications.")?;
        let data_dir = default_data_dir_for(&name);
        if is_unmanaged_existing_dir(&data_dir) && !adopt {
            bail!("{} already exists and isn't managed by this tool.", data_dir.display());
        }
        let color = value(args, "--color").filter(|c| !c.is_empty() && *c != "none").map(String::from);
        if let Some(c) = &color {
            if crate::appclone::hue_for(c).is_none() {
                bail!("Unknown colour: {c}");
            }
        }
        Some(DesktopSetup {
            name: name.clone(),
            data_dir,
            app_path: default_app_path_for(&name),
            claude_app,
            apply_icon: true,
            color,
            seed_connectors: flag(args, "--seed-connectors"),
        })
    } else {
        None
    };
    let code = if wants_code {
        let config_dir = default_config_dir_for(&name);
        if is_unmanaged_existing_dir(&config_dir) && !adopt {
            bail!("{} already exists and isn't managed by this tool.", config_dir.display());
        }
        Some(CodeSetup {
            name: name.clone(),
            config_dir,
            alias_name: default_alias_name_for(&name),
            seed: flag(args, "--seed-code"),
            isolate_gh: flag(args, "--gh"),
        })
    } else {
        None
    };
    apply(&name, desktop.as_ref(), code.as_ref())?;
    Ok(json!({ "name": name }))
}

/// The command that starts Claude Code as a given account.
fn cli_command(target: &str) -> Result<String> {
    if target == "default" {
        return Ok("claude".into());
    }
    let p = profile(target)?;
    let c = p.code.with_context(|| format!("\"{target}\" has no Claude Code half."))?;
    let mut cmd = format!("CLAUDE_CONFIG_DIR={}", shell_quote(&path_string(&c.config_dir)));
    if let Some(gh) = &c.gh_config_dir {
        cmd += &format!(" GH_CONFIG_DIR={}", shell_quote(&path_string(gh)));
    }
    cmd += " claude";
    Ok(cmd)
}

/// Open a terminal window running `command`. The command arrives as an
/// argument to a fixed AppleScript, never as part of the script.
fn run_in_terminal(command: &str, app: &str) -> Result<()> {
    let script = match app {
        "iTerm" => "on run argv\n tell application \"iTerm\"\n activate\n create window with default profile command \"/bin/zsh -lic \" & quoted form of (item 1 of argv)\n end tell\nend run",
        "Ghostty" | "Warp" => {
            // Neither takes a command over AppleScript; hand them a .command file.
            let dir = tempfile::Builder::new().prefix("claude-profiles-").tempdir()?.keep();
            let file = dir.join("start.command");
            std::fs::write(&file, format!("#!/bin/zsh -l\n{command}\n"))?;
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o700))?;
            if !run_quiet("/usr/bin/open", [OsStr::new("-a"), OsStr::new(app), file.as_os_str()]) {
                bail!("Could not open {app}.");
            }
            return Ok(());
        }
        _ => "on run argv\n tell application \"Terminal\"\n activate\n do script (item 1 of argv)\n end tell\nend run",
    };
    if !run_quiet("/usr/bin/osascript", ["-e", script, command]) {
        bail!("Could not open {app}. macOS may need permission for Claude Profiles to control it (System Settings > Privacy & Security > Automation).");
    }
    Ok(())
}

fn open_cli(args: &[String]) -> Result<Value> {
    let target = need(args, 0, "profile name")?;
    let mut cmd = cli_command(target)?;
    if flag(args, "--login") {
        cmd += " auth login";
    }
    if let Some(dir) = value(args, "--dir") {
        if !Path::new(dir).is_dir() {
            bail!("{dir} is not a folder.");
        }
        cmd = format!("cd {} && {cmd}", shell_quote(dir));
    }
    run_in_terminal(&cmd, value(args, "--terminal").unwrap_or("Terminal"))?;
    Ok(json!({ "command": cmd }))
}

fn quit_desktop(name: &str) -> Result<Value> {
    let p = profile(name)?;
    p.desktop.as_ref().context("No Desktop half.")?;
    let clone = clone_path_for(name);
    let procs = processes();
    let (_, _, mains) = bundle_usage(&procs, &clone);
    if mains.is_empty() {
        return Ok(json!({ "quit": 0 }));
    }
    // The same polite Quit as Claude's own menu, so nothing unsaved is lost
    // without Claude asking.
    const TERMINATE: &str = "ObjC.import('AppKit');\nfunction run(argv) {\n  argv.forEach(function (p) {\n    var ra = $.NSRunningApplication.runningApplicationWithProcessIdentifier(parseInt(p, 10));\n    if (!ra.isNil()) ra.terminate;\n  });\n  return 'OK';\n}";
    let mut a: Vec<String> = vec!["-l".into(), "JavaScript".into(), "-e".into(), TERMINATE.into()];
    a.extend(mains.iter().map(|(pid, _)| pid.to_string()));
    run_quiet("/usr/bin/osascript", &a);
    Ok(json!({ "quit": mains.len() }))
}

fn stop_session(pid: &str) -> Result<Value> {
    let pid: i32 = pid.parse().context("not a process ID")?;
    // Only ever a Claude Code session of this user.
    let is_session = processes().iter().any(|p| p.pid == pid && p.comm == "claude");
    if !is_session {
        bail!("Process {pid} is not a running Claude Code session.");
    }
    if unsafe { libc::kill(pid, libc::SIGTERM) } != 0 {
        bail!("Could not stop process {pid}.");
    }
    Ok(json!({ "stopped": pid }))
}

fn open_path(p: &Path) -> Result<()> {
    if !run_quiet("/usr/bin/open", [p.as_os_str()]) {
        bail!("Could not open {}.", p.display());
    }
    Ok(())
}

pub fn run(args: &[String]) -> Result<()> {
    crate::ui::set_quiet();
    let action = args.first().map(String::as_str).unwrap_or("");
    let rest = if args.is_empty() { &[][..] } else { &args[1..] };
    let reply = match action {
        "state" => state()?,
        "sizes" => sizes(),
        "usage" => usage(),
        "update-helper" | "startup" => {
            // Reinstalling the app or the command leaves the helper the
            // launchers run one build behind; bring it level.
            let updated = !registry::load().profiles.iter().all(|p| p.desktop.is_none())
                && install_helper()? != crate::launcher::Installed::Current;
            json!({ "updated": updated, "urlHandlerIcon": url_handler_icon() })
        }
        "add" => add(rest)?,
        "importable" => importable(),
        "remove-part" => {
            let p = profile(need(rest, 0, "profile name")?)?;
            let desktop = match need(rest, 1, "desktop or code")? {
                "desktop" => true,
                "code" => false,
                other => bail!("Say desktop or code, not \"{other}\"."),
            };
            if desktop {
                if let Some(d) = &p.desktop {
                    if crate::appclone::clone_is_running(&clone_path_for(&p.name)) || crate::desktop::data_dir_in_use(&d.data_dir) {
                        bail!("\"{}\" is open in Claude Desktop. Quit it first.", p.name);
                    }
                }
            }
            super::remove::remove_part(&p, desktop, flag(rest, "--delete-data"))?;
            if let (false, Some(d)) = (desktop, &p.desktop) {
                // The launcher pointed Claude Desktop's own Code tab at the
                // removed folder; rebuild it without that, as rename does.
                let target = crate::desktop::launch_target_for(&p.name, d);
                crate::desktop::compile_app(&crate::desktop::CompileSpec {
                    name: &p.name,
                    data_dir: &d.data_dir,
                    app_path: &d.app_path,
                    target: &target,
                    code_config_dir: None,
                    gh_config_dir: None,
                })?;
                crate::desktop::copy_claude_icon(&d.app_path, &d.claude_app_path);
            }
            json!({ "removed": if desktop { "desktop" } else { "code" } })
        }
        "sign-out" => {
            // Claude Code's own logout, for this profile's folder only: it
            // clears that folder's Keychain sign-in and nothing else.
            let p = profile(need(rest, 0, "profile name")?)?;
            let c = p.code.context("That profile has no Claude Code part.")?;
            let claude = resolve_claude().context("The claude command was not found.")?;
            let ok = std::process::Command::new(&claude.winner)
                .args(["auth", "logout"])
                .env("CLAUDE_CONFIG_DIR", &c.config_dir)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .is_ok_and(|s| s.success());
            if !ok {
                bail!("Claude Code could not sign \"{}\" out.", p.name);
            }
            json!({ "signedOut": p.name })
        }
        "relink-login" => {
            // For a profile renamed before rename carried the sign-in over:
            // move it from the old folder's Keychain entry to the current
            // one. The old folder must be gone, so this cannot take the
            // sign-in of a folder still in use.
            let p = profile(need(rest, 0, "profile name")?)?;
            let c = p.code.context("That profile has no Claude Code half.")?;
            let old = PathBuf::from(need(rest, 1, "old config folder")?);
            if old.exists() || is_registered(&old) {
                bail!("{} still exists, so its sign-in stays where it is.", old.display());
            }
            if !crate::code::move_keychain_login(&old, &c.config_dir) {
                bail!("No sign-in was found for {}.", old.display());
            }
            json!({ "moved": true })
        }
        "name-account" => {
            let uuid = need(rest, 0, "account ID")?;
            if uuid.len() != 36 || !uuid.chars().all(|c| c.is_ascii_hexdigit() || c == '-') {
                bail!("\"{uuid}\" is not an account ID.");
            }
            let name = rest.get(1).map(String::as_str).unwrap_or("");
            if name.chars().count() > 60 {
                bail!("Keep the name under 60 characters.");
            }
            crate::state::set_account_name(uuid, name)?;
            json!({ "accountUuid": uuid, "name": name.trim() })
        }
        "import" => import(rest)?,
        "remove" => {
            let p = profile(need(rest, 0, "profile name")?)?;
            if let Some(d) = &p.desktop {
                if crate::appclone::clone_is_running(&clone_path_for(&p.name)) || crate::desktop::data_dir_in_use(&d.data_dir) {
                    bail!("\"{}\" is open in Claude Desktop. Quit it first.", p.name);
                }
            }
            super::remove::remove_profile(&p, flag(rest, "--delete-desktop-data"), flag(rest, "--delete-code-data"))?;
            json!({ "removed": p.name })
        }
        "rename" => {
            let (old, new) = (need(rest, 0, "old name")?, need(rest, 1, "new name")?);
            super::rename::run(&[old.to_string(), new.to_string(), "--yes".to_string()])?;
            json!({ "renamed": new })
        }
        "self-update" => {
            super::selfupdate::run(rest)?;
            json!({})
        }
        "repair" => {
            super::repair::run(&[need(rest, 0, "profile name")?.to_string()])?;
            json!({})
        }
        "doctor-fix" => {
            super::doctor::run(&["--fix".to_string()])?;
            json!({})
        }
        "open-desktop" => {
            let p = profile(need(rest, 0, "profile name")?)?;
            open_path(&p.desktop.context("No Desktop half.")?.app_path)?;
            json!({})
        }
        "open-default-desktop" => {
            open_path(&find_claude_app().context("Claude.app not found.")?)?;
            json!({})
        }
        "quit-desktop" => quit_desktop(need(rest, 0, "profile name")?)?,
        "open-cli" => open_cli(rest)?,
        "cli-command" => json!({ "command": cli_command(need(rest, 0, "profile name")?)? }),
        "stop-session" => stop_session(need(rest, 0, "process ID")?)?,
        "in-terminal" => {
            // Only these: the interactive commands the panel has no form for.
            let sub = need(rest, 0, "command")?;
            if !["doctor", "add", "extensions", "status"].contains(&sub) {
                bail!("\"{sub}\" cannot be opened in a terminal from here.");
            }
            let exe = std::env::current_exe()?;
            let cmd = format!("{} {sub}", shell_quote(&path_string(&exe)));
            run_in_terminal(&cmd, value(rest, "--terminal").unwrap_or("Terminal"))?;
            json!({})
        }
        other => bail!("unknown app action \"{other}\""),
    };
    println!("{}", serde_json::to_string(&reply)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn guess_name_strips_claude_prefixes() {
        assert_eq!(super::guess_name("Claude-Work"), "work");
        assert_eq!(super::guess_name(".claude-client-acme"), "client-acme");
        assert_eq!(super::guess_name("Claude Personal Stuff"), "personal-stuff");
        assert_eq!(super::guess_name(".claude"), "imported");
        assert_eq!(super::guess_name(".config-claude"), "config-claude");
    }

    use super::*;

    #[test]
    fn environment_values_are_read_whole_even_with_spaces() {
        let cmd = "claude --x HOME=/Users/me CLAUDE_CONFIG_DIR=/Users/me/My Profiles/.claude-w PATH=/usr/bin";
        assert_eq!(env_value(cmd, "CLAUDE_CONFIG_DIR").as_deref(), Some("/Users/me/My Profiles/.claude-w"));
        assert_eq!(env_value(cmd, "PATH").as_deref(), Some("/usr/bin"));
        assert_eq!(env_value(cmd, "GH_CONFIG_DIR"), None);
    }
}
