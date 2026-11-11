// Past Claude Code sessions, every account at once: `claude-profiles app
// sessions`, `app resume-cli` and `app resume-app`.
//
// Two places hold them, and neither sees the other's:
//
//   - The CLI keeps a transcript per session in <config>/projects/<folder>/
//     <id>.jsonl. `claude --resume <id>`, run in that folder with that
//     config folder, picks it up.
//   - Claude Desktop keeps its Code tab's sessions as
//     <data>/claude-code-sessions/<account>/<org>/local_<id>.json, each
//     pointing at the CLI transcript(s) it runs on. Its sidebar lists only
//     these, so a session started in a terminal never shows there.
//
// The list merges the two. Resuming goes back through each one's own door:
// the terminal with the account's command, or Claude Desktop's own deep links
// (claude://code/continue for its sessions, claude://resume to import a
// terminal one), sent to the account's own copy of Claude.

use std::collections::{HashMap, HashSet};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};

use crate::appclone::{clone_path_for, parse_running_copies, ps_snapshot};
use crate::claudebin::{default_claude_config_dir, default_desktop_data_dir, find_claude_app};
use crate::launcher::on_profile;
use crate::platform;
use crate::registry;
use crate::util::{file_exists, is_mac, path_string, read_json};

/// Read from the start of a transcript for its folder and first prompt, and
/// from its end for its title: transcripts run to tens of megabytes.
const HEAD_BYTES: u64 = 64 * 1024;
const TAIL_BYTES: u64 = 256 * 1024;

// ---- Accounts ---------------------------------------------------------------------------------

/// One account, as far as sessions go.
struct Account {
    name: String,
    /// Where its terminal sessions live; none for a Desktop-only profile.
    code_dir: Option<PathBuf>,
    /// Its Claude Desktop data folder and the app that opens on it.
    desktop: Option<(PathBuf, PathBuf)>,
}

impl Account {
    /// Claude Desktop runs its Code sessions with the profile's config
    /// folder when it has one (the launcher passes CLAUDE_CONFIG_DIR), and
    /// with ~/.claude otherwise.
    fn desktop_code_dir(&self) -> PathBuf {
        self.code_dir.clone().unwrap_or_else(default_claude_config_dir)
    }
}

fn accounts() -> Vec<Account> {
    let mut out = vec![Account {
        name: "default".into(),
        code_dir: Some(default_claude_config_dir()),
        desktop: if is_mac() { find_claude_app().map(|app| (default_desktop_data_dir(), app)) } else { None },
    }];
    for p in registry::load().profiles {
        out.push(Account {
            code_dir: p.code.as_ref().map(|c| c.config_dir.clone()),
            desktop: p.desktop.as_ref().filter(|_| is_mac()).map(|d| (d.data_dir.clone(), clone_path_for(&p.name))),
            name: p.name,
        });
    }
    out
}

fn account(name: &str) -> Result<Account> {
    accounts().into_iter().find(|a| a.name == name).with_context(|| format!("No profile called \"{name}\"."))
}

/// The account whose terminal command resumes transcripts in `dir`.
fn cli_owner<'a>(all: &'a [Account], dir: &Path) -> Option<&'a Account> {
    all.iter().find(|a| a.code_dir.as_deref() == Some(dir))
}

/// The account whose Claude Desktop can open transcripts in `dir`.
fn app_owner<'a>(all: &'a [Account], dir: &Path) -> Option<&'a Account> {
    all.iter().find(|a| a.desktop.is_some() && a.desktop_code_dir() == dir)
}

// ---- Reading transcripts ----------------------------------------------------------------------

pub fn is_session_id(s: &str) -> bool {
    let parts: Vec<&str> = s.split('-').collect();
    parts.iter().map(|p| p.len()).eq([8, 4, 4, 4, 12]) && parts.iter().all(|p| p.chars().all(|c| c.is_ascii_hexdigit()))
}

fn is_app_session_id(s: &str) -> bool {
    s.strip_prefix("local_").is_some_and(|r| !r.is_empty() && r.len() <= 64 && r.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'))
}

/// What a transcript says about itself.
#[derive(Default, Debug, PartialEq)]
struct Transcript {
    cwd: Option<String>,
    branch: Option<String>,
    title: Option<String>,
    first_prompt: Option<String>,
    last_prompt: Option<String>,
}

/// The text of a user message, when it is something a person typed: not a
/// tool result, a slash command's wrapper or a note Claude Code added.
fn typed_text(entry: &Value) -> Option<String> {
    if entry["type"] != "user" || entry["isMeta"] == true || entry["isSidechain"] == true {
        return None;
    }
    let content = &entry["message"]["content"];
    let text = match content {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts.iter().find(|p| p["type"] == "text").and_then(|p| p["text"].as_str()).map(String::from)?,
        _ => return None,
    };
    let text = text.trim();
    (!text.is_empty() && !text.starts_with('<') && !text.starts_with("This session is being continued")).then(|| one_line(text))
}

fn one_line(s: &str) -> String {
    let flat: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() > 120 {
        format!("{}…", flat.chars().take(119).collect::<String>())
    } else {
        flat
    }
}

/// Whole lines only: a read that starts mid-file drops its first, partial line.
fn lines(text: &str, partial_start: bool) -> impl Iterator<Item = Value> + '_ {
    text.lines().skip(usize::from(partial_start)).filter_map(|l| serde_json::from_str(l).ok())
}

fn read_transcript(path: &Path) -> Option<Transcript> {
    let mut f = std::fs::File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    let mut head = Vec::new();
    (&mut f).take(HEAD_BYTES).read_to_end(&mut head).ok()?;
    let mut t = Transcript::default();
    for e in lines(&String::from_utf8_lossy(&head), false) {
        if t.cwd.is_none() {
            t.cwd = e["cwd"].as_str().map(String::from);
        }
        if t.branch.is_none() {
            t.branch = e["gitBranch"].as_str().filter(|b| !b.is_empty() && *b != "HEAD").map(String::from);
        }
        if t.first_prompt.is_none() {
            t.first_prompt = typed_text(&e);
        }
    }
    let start = len.saturating_sub(TAIL_BYTES);
    let mut tail = Vec::new();
    f.seek(SeekFrom::Start(start)).ok()?;
    f.take(TAIL_BYTES).read_to_end(&mut tail).ok()?;
    let (mut custom, mut ai, mut summary) = (None, None, None);
    for e in lines(&String::from_utf8_lossy(&tail), start > 0) {
        match e["type"].as_str() {
            Some("custom-title") => custom = e["customTitle"].as_str().or(e["title"].as_str()).map(String::from),
            Some("ai-title") => ai = e["aiTitle"].as_str().map(String::from),
            Some("summary") => summary = e["summary"].as_str().map(String::from),
            Some("last-prompt") => t.last_prompt = e["lastPrompt"].as_str().map(one_line),
            _ => {
                if let Some(text) = typed_text(&e) {
                    t.last_prompt = Some(text);
                }
            }
        }
    }
    // A name you gave it beats the one Claude Code made up.
    t.title = custom.or(ai).or(summary).filter(|s| !s.trim().is_empty()).map(|s| one_line(&s));
    Some(t)
}

fn modified_secs(p: &Path) -> u64 {
    std::fs::metadata(p).and_then(|m| m.modified()).ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map_or(0, |d| d.as_secs())
}

/// Every top-level transcript in a config folder, by session ID. Subagents'
/// transcripts sit a level deeper and are part of their session.
fn transcripts(code_dir: &Path) -> HashMap<String, PathBuf> {
    let mut out = HashMap::new();
    let Ok(projects) = std::fs::read_dir(code_dir.join("projects")) else { return out };
    for folder in projects.flatten() {
        let Ok(files) = std::fs::read_dir(folder.path()) else { continue };
        for f in files.flatten() {
            let p = f.path();
            let id = p.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
            if p.extension().is_some_and(|e| e == "jsonl") && is_session_id(&id) {
                out.insert(id, p);
            }
        }
    }
    out
}

/// Claude Desktop's own records of its Code sessions.
fn app_records(data_dir: &Path) -> Vec<Value> {
    let mut out = Vec::new();
    let Ok(accounts) = std::fs::read_dir(data_dir.join("claude-code-sessions")) else { return out };
    for acct in accounts.flatten() {
        let Ok(orgs) = std::fs::read_dir(acct.path()) else { continue };
        for org in orgs.flatten() {
            let Ok(files) = std::fs::read_dir(org.path()) else { continue };
            for f in files.flatten() {
                let p = f.path();
                let name = p.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
                if name.starts_with("local_") && name.ends_with(".json") {
                    if let Some(v) = read_json(&p) {
                        out.push(v);
                    }
                }
            }
        }
    }
    out
}

fn millis_to_secs(v: &Value) -> u64 {
    v.as_u64().or_else(|| v.as_f64().map(|f| f as u64)).unwrap_or(0) / 1000
}

// ---- The list ---------------------------------------------------------------------------------

/// `app sessions`: every account's sessions, newest first.
pub fn list() -> Result<Value> {
    let all = accounts();
    let mut by_dir: HashMap<PathBuf, HashMap<String, PathBuf>> = HashMap::new();
    for a in &all {
        for dir in a.code_dir.iter().cloned().chain(a.desktop.as_ref().map(|_| a.desktop_code_dir())) {
            by_dir.entry(dir.clone()).or_insert_with(|| transcripts(&dir));
        }
    }
    // Transcripts a Desktop session runs on are shown as that session, not
    // again on their own.
    let mut claimed: HashSet<(PathBuf, String)> = HashSet::new();
    let mut out = Vec::new();

    for a in &all {
        let mut rows: Vec<Value> = Vec::new();
        if let Some((data_dir, _)) = &a.desktop {
            let dir = a.desktop_code_dir();
            let files = &by_dir[&dir];
            for r in app_records(data_dir) {
                let Some(app_id) = r["sessionId"].as_str().filter(|s| is_app_session_id(s)) else { continue };
                let cli_id = r["cliSessionId"].as_str().filter(|s| is_session_id(s));
                for prior in r["priorCliSessionIds"].as_array().into_iter().flatten().filter_map(Value::as_str).chain(cli_id) {
                    claimed.insert((dir.clone(), prior.to_string()));
                }
                let file = cli_id.and_then(|id| files.get(id));
                let t = file.and_then(|f| read_transcript(f)).unwrap_or_default();
                let cwd = r["cwd"].as_str().filter(|c| Path::new(c).is_dir()).or(r["originCwd"].as_str()).map(String::from).or(t.cwd.clone());
                let last = millis_to_secs(&r["lastActivityAt"]).max(file.map_or(0, |f| modified_secs(f)));
                let cli = file.and_then(|_| cli_owner(&all, &dir));
                rows.push(json!({
                    "id": cli_id,
                    "appId": app_id,
                    "title": r["title"].as_str().map(one_line).or(t.title).or(t.first_prompt.clone()),
                    "firstPrompt": t.first_prompt,
                    "lastPrompt": t.last_prompt,
                    "cwd": cwd,
                    "branch": t.branch,
                    "created": millis_to_secs(&r["createdAt"]),
                    "lastActive": last,
                    "archived": r["isArchived"] == true,
                    "inApp": true,
                    "cliTarget": cli.map(|c| c.name.clone()),
                    "appTarget": a.name,
                }));
            }
        }
        if let Some(dir) = &a.code_dir {
            for (id, file) in &by_dir[dir] {
                if claimed.contains(&(dir.clone(), id.clone())) {
                    continue;
                }
                let Some(t) = read_transcript(file) else { continue };
                // Nothing typed yet: an empty start, not worth listing.
                if t.first_prompt.is_none() && t.title.is_none() {
                    continue;
                }
                rows.push(json!({
                    "id": id,
                    "appId": Value::Null,
                    "title": t.title.clone().or(t.first_prompt.clone()),
                    "firstPrompt": t.first_prompt,
                    "lastPrompt": t.last_prompt,
                    "cwd": t.cwd,
                    "branch": t.branch,
                    "created": Value::Null,
                    "lastActive": modified_secs(file),
                    "archived": false,
                    "inApp": false,
                    "cliTarget": a.name,
                    "appTarget": app_owner(&all, dir).map(|o| o.name.clone()),
                }));
            }
        }
        rows.sort_by_key(|r| std::cmp::Reverse(r["lastActive"].as_u64().unwrap_or(0)));
        out.push(json!({
            "name": a.name,
            "canCli": a.code_dir.is_some(),
            "canApp": a.desktop.is_some(),
            "sessions": rows,
        }));
    }
    Ok(json!({ "accounts": out }))
}

// ---- Resuming -----------------------------------------------------------------------------------

/// `app resume-cli ACCOUNT ID [--terminal T]`: a terminal in the session's
/// folder, running the account's `claude --resume ID`.
pub fn resume_cli(target: &str, id: &str, command: &str, terminal: &str) -> Result<Value> {
    if !is_session_id(id) {
        bail!("\"{id}\" is not a Claude Code session ID.");
    }
    let a = account(target)?;
    let dir = a.code_dir.with_context(|| format!("\"{target}\" has no Claude Code part, so it has no terminal sessions."))?;
    let file = transcripts(&dir).remove(id).with_context(|| format!("No session {id} in {}.", path_string(&dir)))?;
    let cwd = read_transcript(&file).and_then(|t| t.cwd).context("That session does not say which folder it ran in.")?;
    if !Path::new(&cwd).is_dir() {
        bail!("{cwd} no longer exists, and Claude Code resumes a session only in its own folder.");
    }
    let cmd = platform::in_dir(&cwd, &format!("{command} --resume {id}"));
    platform::run_in_terminal(&cmd, terminal)?;
    Ok(json!({ "command": cmd }))
}

/// Whether the account's Claude Desktop is open on its own data folder; an
/// error when it is open on the wrong one, where a link would land in the
/// wrong account.
fn app_open_on_profile(copy: &Path, data_dir: &Path) -> Result<bool> {
    let running = parse_running_copies(&ps_snapshot(), copy);
    if running.is_empty() {
        return Ok(false);
    }
    if running.iter().any(|r| !on_profile(&r.command, &path_string(data_dir))) {
        bail!("This profile's Claude is open on the wrong account. Quit it and open it from its launcher, then try again.");
    }
    Ok(true)
}

/// `app resume-app ACCOUNT ID|local_ID`: open the session in the account's
/// Claude Desktop. A Desktop session is continued; a terminal one is
/// imported into the Code tab first (Claude Desktop does that itself).
pub fn resume_app(target: &str, id: &str) -> Result<Value> {
    if !is_mac() {
        bail!("Opening sessions in Claude Desktop needs macOS.");
    }
    let url = if is_app_session_id(id) {
        format!("claude://code/continue?session={id}")
    } else if is_session_id(id) {
        format!("claude://resume?session={id}")
    } else {
        bail!("\"{id}\" is not a session ID.");
    };
    let a = account(target)?;
    let (data_dir, app) = a.desktop.clone().with_context(|| format!("\"{target}\" has no Claude Desktop."))?;
    if !file_exists(&app) {
        bail!("{} is missing. Run claude-profiles doctor --fix.", path_string(&app));
    }
    if a.name != "default" && !app_open_on_profile(&app, &data_dir)? {
        // Only its launcher opens it on the right account; then wait for it.
        let launcher = registry::load().profiles.into_iter().find(|p| p.name == a.name).and_then(|p| p.desktop).map(|d| d.app_path).context("No launcher.")?;
        platform::open_path(&launcher)?;
        let started = SystemTime::now();
        while !app_open_on_profile(&app, &data_dir)? {
            if started.elapsed().unwrap_or_default() > Duration::from_secs(45) {
                bail!("Claude did not open in time. Try again once its window is up.");
            }
            std::thread::sleep(Duration::from_millis(500));
        }
        // Give the window a moment to be ready for the link.
        std::thread::sleep(Duration::from_secs(4));
    }
    // -a sends the link to this copy, not whichever Claude macOS prefers.
    let ok = std::process::Command::new("/usr/bin/open").arg("-a").arg(&app).arg(&url).status().is_ok_and(|s| s.success());
    if !ok {
        bail!("Could not hand the session to Claude.");
    }
    Ok(json!({ "url": url }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_ids_are_checked_strictly() {
        assert!(is_session_id("beb353ac-a59a-4674-bbe3-ca13829ee2da"));
        assert!(!is_session_id("beb353ac-a59a-4674-bbe3-ca13829ee2d"));
        assert!(!is_session_id("beb353ac-a59a-4674-bbe3-ca13829ee2da; rm"));
        assert!(is_app_session_id("local_2ad07b7c-3fe4-43ff-aa3a-e4531de99017"));
        assert!(!is_app_session_id("local_"));
        assert!(!is_app_session_id("local_a&b"));
    }

    #[test]
    fn transcripts_give_their_folder_title_and_prompts() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("s.jsonl");
        let lines = [
            r#"{"type":"user","isMeta":true,"cwd":"/p","gitBranch":"main","message":{"content":"<local-command-caveat>x"}}"#,
            r#"{"type":"user","message":{"content":[{"type":"text","text":"fix   the\nlogin"}]}}"#,
            r#"{"type":"assistant","message":{"content":[]}}"#,
            r#"{"type":"ai-title","aiTitle":"Login fix"}"#,
            r#"{"type":"user","message":{"content":[{"type":"tool_result","content":"ok"}]}}"#,
            r#"{"type":"last-prompt","lastPrompt":"and the tests"}"#,
        ];
        std::fs::write(&f, lines.join("\n")).unwrap();
        let t = read_transcript(&f).unwrap();
        assert_eq!(t.cwd.as_deref(), Some("/p"));
        assert_eq!(t.branch.as_deref(), Some("main"));
        assert_eq!(t.first_prompt.as_deref(), Some("fix the login"));
        assert_eq!(t.title.as_deref(), Some("Login fix"));
        assert_eq!(t.last_prompt.as_deref(), Some("and the tests"));

        std::fs::write(&f, format!("{}\n{}", lines[1], r#"{"type":"custom-title","customTitle":"Mine"}"#)).unwrap();
        assert_eq!(read_transcript(&f).unwrap().title.as_deref(), Some("Mine"));
    }
}
