// Shell aliases.
//
// A Code profile is started with an alias such as
//
//     alias claude-work='CLAUDE_CONFIG_DIR="/Users/me/.claude-work" claude'
//
// written into a delimited block in the rc file:
//
//     # >>> claude-profiles >>>
//     ...managed lines...
//     # <<< claude-profiles <<<
//
// Every write replaces the block's contents, so re-running never piles up
// duplicates. Lines outside the markers are never touched.
//
// A shell reads its rc file once, at startup, so a terminal opened before the
// last change still has the old aliases. The block carries its own timestamp,
// written only when the aliases actually change, so `status` can say when the
// terminal is older than the aliases on disk.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Local, NaiveDateTime, TimeZone, Utc};

use crate::util::home;

const BLOCK_START: &str = "# >>> claude-profiles >>>";
const BLOCK_END: &str = "# <<< claude-profiles <<<";
const STAMP_PREFIX: &str = "# updated ";

// ---- Detection --------------------------------------------------------------------

/// The user's shell, from $SHELL. zsh, the macOS default, when unsure. On
/// Windows, PowerShell: pwsh (7+) when installed, else Windows PowerShell.
pub fn detect_shell() -> String {
    if cfg!(windows) {
        return if crate::platform::which_all("pwsh").is_empty() { "powershell".into() } else { "pwsh".into() };
    }
    let shell = std::env::var("SHELL").unwrap_or_default();
    for s in ["zsh", "bash", "fish"] {
        if shell.ends_with(&format!("/{s}")) {
            return s.into();
        }
    }
    "zsh".into()
}

pub fn rc_path_for_shell(shell: &str) -> PathBuf {
    let h = home();
    match shell {
        "zsh" => h.join(".zshrc"),
        // Terminal.app starts bash as a login shell, which reads .bash_profile.
        "bash" => h.join(".bash_profile"),
        "fish" => h.join(".config").join("fish").join("config.fish"),
        // The all-hosts profile, so the command works in Windows Terminal,
        // VS Code and the plain console alike.
        "pwsh" => documents_dir().join("PowerShell").join("profile.ps1"),
        "powershell" => documents_dir().join("WindowsPowerShell").join("profile.ps1"),
        _ => h.join(".profile"),
    }
}

/// Where PowerShell looks for profiles: the Documents folder, which OneDrive
/// often moves, so Windows is asked rather than assumed.
fn documents_dir() -> PathBuf {
    if cfg!(windows) {
        let asked = crate::util::run_output(
            "powershell",
            ["-NoProfile", "-NonInteractive", "-Command", "[Environment]::GetFolderPath('MyDocuments')"],
        );
        if let Some(d) = asked.map(|s| s.trim().to_string()).filter(|s| !s.is_empty()) {
            return PathBuf::from(d);
        }
    }
    home().join("Documents")
}

pub fn is_powershell(shell: &str) -> bool {
    shell == "pwsh" || shell == "powershell"
}

/// The command that reloads an rc file into the current shell.
pub fn reload_command(rc: &Path) -> String {
    let shown = crate::util::tildify(rc);
    if cfg!(windows) {
        format!(". '{}'", rc.to_string_lossy().replace('\'', "''"))
    } else {
        format!("source {shown}")
    }
}

// ---- Reading and writing the block -----------------------------------------------------

struct Block<'a> {
    before: &'a str,
    inside: &'a str,
    after: &'a str,
    found: bool,
}

fn extract_block(content: &str) -> Block<'_> {
    let none = Block { before: content, inside: "", after: "", found: false };
    let Some(start) = content.find(BLOCK_START) else { return none };
    let after_start = start + BLOCK_START.len();
    // A start marker with no end marker is treated as no block, rather than
    // eating lines the user may want.
    let Some(end_rel) = content[after_start..].find(BLOCK_END) else { return none };
    let end = after_start + end_rel;
    Block {
        before: &content[..start],
        inside: &content[after_start..end],
        after: &content[end + BLOCK_END.len()..],
        found: true,
    }
}

fn read_rc(rc_path: &Path) -> String {
    std::fs::read_to_string(rc_path).unwrap_or_default()
}

/// The name a managed line defines, or None. Fish entries are functions,
/// since fish's alias cannot carry an environment prefix, so both forms are
/// read; reading only `alias` once made a second fish profile drop the first.
pub fn parse_managed_line(line: &str) -> Option<String> {
    let t = line.trim();
    let name = if let Some(rest) = t.strip_prefix("alias") {
        if !rest.starts_with(char::is_whitespace) {
            return None;
        }
        let rest = rest.trim_start();
        let eq = rest.find('=')?;
        let name = &rest[..eq];
        (!name.is_empty() && !name.contains(char::is_whitespace)).then_some(name)?
    } else {
        let rest = t.strip_prefix("function")?;
        if !rest.starts_with(char::is_whitespace) {
            return None;
        }
        let rest = rest.trim_start();
        let end = rest.find(|c: char| c.is_whitespace() || c == ';')?;
        let name = &rest[..end];
        // The name must be followed (after optional spaces) by a semicolon,
        // or by a brace for PowerShell.
        let next = rest[end..].trim_start();
        if !(next.starts_with(';') || next.starts_with('{')) || name.is_empty() {
            return None;
        }
        name
    };
    Some(name.to_string())
}

pub struct ManagedAlias {
    pub name: String,
    pub line: String,
}

pub fn read_managed_aliases(rc_path: &Path) -> Vec<ManagedAlias> {
    let content = read_rc(rc_path);
    let b = extract_block(&content);
    if !b.found {
        return Vec::new();
    }
    b.inside
        .lines()
        .filter_map(|l| parse_managed_line(l).map(|name| ManagedAlias { name, line: l.trim().to_string() }))
        .collect()
}

/// When the managed block last changed, or None if it predates stamping.
pub fn read_alias_stamp(rc_path: &Path) -> Option<DateTime<Utc>> {
    let content = read_rc(rc_path);
    let b = extract_block(&content);
    if !b.found {
        return None;
    }
    b.inside.lines().find_map(|l| {
        let s = l.trim().strip_prefix(STAMP_PREFIX)?;
        DateTime::parse_from_rfc3339(s.trim()).ok().map(|d| d.with_timezone(&Utc))
    })
}

fn iso(t: DateTime<Utc>) -> String {
    t.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string()
}

/// Replace (or insert) the managed block with `lines`. An empty list takes
/// the block out altogether.
pub fn write_aliases(lines: &[String], rc_path: &Path) -> std::io::Result<()> {
    let content = read_rc(rc_path);
    let b = extract_block(&content);

    if lines.is_empty() {
        if b.found {
            let next = format!("{}\n{}", b.before.trim_end(), b.after.trim_start());
            std::fs::write(rc_path, next.trim_start_matches('\n'))?;
        }
        return Ok(());
    }

    // Re-stamp only when the aliases change, or a no-op write would claim a
    // change that did not happen and churn dotfiles kept in git. An unchanged
    // block without a stamp stays unstamped.
    let previous: Vec<String> = read_managed_aliases(rc_path).into_iter().map(|a| a.line).collect();
    let unchanged = b.found && previous == lines;
    let stamp = if unchanged { read_alias_stamp(rc_path) } else { Some(Utc::now()) };

    let mut block = vec![
        BLOCK_START.to_string(),
        "# Managed by claude-profiles. Edits inside this block may be overwritten.".into(),
        "# Run `claude-profiles list` to see what's configured.".into(),
    ];
    if let Some(t) = stamp {
        block.push(format!("{STAMP_PREFIX}{}", iso(t)));
    }
    block.push(String::new());
    block.extend(lines.iter().cloned());
    block.push(String::new());
    block.push(BLOCK_END.into());
    let block = block.join("\n");

    let next = if b.found {
        format!("{}\n\n{}\n\n{}", b.before.trim_end(), block, b.after.trim_start())
    } else {
        let trimmed = content.trim_end();
        if trimmed.is_empty() { format!("{block}\n") } else { format!("{trimmed}\n\n{block}\n") }
    };
    if let Some(parent) = rc_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(rc_path, next)
}

/// The line that defines one profile's command. Paths are data, never shell
/// code: each is escaped for a double-quoted string in the target shell, and
/// for bash and zsh the whole body is then escaped for the alias's single
/// quotes.
pub fn build_alias_line(shell: &str, alias: &str, config_dir: &Path, gh_config_dir: Option<&Path>) -> String {
    let quote = |p: &Path| -> String {
        let special: &[char] = if shell == "fish" { &['\\', '"', '$'] } else { &['\\', '"', '$', '`'] };
        let mut out = String::from("\"");
        for c in p.to_string_lossy().chars() {
            if special.contains(&c) {
                out.push('\\');
            }
            out.push(c);
        }
        out.push('"');
        out
    };
    if is_powershell(shell) {
        // Set for the one call and put back after, so the console keeps
        // the account it had.
        let q = |p: &Path| format!("'{}'", p.to_string_lossy().replace('\'', "''"));
        let mut vars = vec![("CLAUDE_CONFIG_DIR", q(config_dir))];
        if let Some(gh) = gh_config_dir {
            vars.push(("GH_CONFIG_DIR", q(gh)));
        }
        let save: Vec<String> = vars.iter().enumerate().map(|(i, (k, v))| format!("$o{i} = $env:{k}; $env:{k} = {v}")).collect();
        let restore: Vec<String> = vars.iter().enumerate().map(|(i, (k, _))| format!("$env:{k} = $o{i}")).collect();
        return format!("function {alias} {{ {}; try {{ claude @args }} finally {{ {} }} }}", save.join("; "), restore.join("; "));
    }
    let mut env = vec![format!("CLAUDE_CONFIG_DIR={}", quote(config_dir))];
    if let Some(gh) = gh_config_dir {
        env.push(format!("GH_CONFIG_DIR={}", quote(gh)));
    }
    let prefix = env.join(" ");
    if shell == "fish" {
        return format!("function {alias}; {prefix} claude $argv; end");
    }
    let body = format!("{prefix} claude").replace('\'', "'\\''");
    format!("alias {alias}='{body}'")
}

// ---- Is this terminal running the aliases on disk? ----------------------------------------
//
// A child cannot read its parent shell's aliases, but it can compare when the
// shell started with when the aliases last changed. Every step degrades to
// "unknown" rather than guessing: a wrong warning would send people to
// re-source a file that was already fine.

const SESSION_SHELLS: &[&str] = &["zsh", "bash", "fish", "sh", "dash", "ksh", "tcsh", "csh"];

/// A login shell shows in ps as "-zsh", an interactive one as "/bin/zsh". A
/// `zsh -c '...'` runs one command and holds no aliases worth warning about.
pub fn is_session_shell(command: &str) -> bool {
    let parts: Vec<&str> = command.split_whitespace().collect();
    let Some(first) = parts.first() else { return false };
    let base = Path::new(first.trim_start_matches('-'))
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    SESSION_SHELLS.contains(&base.as_str()) && !parts[1..].contains(&"-c")
}

pub struct PsRecord {
    pub ppid: i32,
    pub started_at: DateTime<Utc>,
    pub command: String,
}

/// Parse `ps -o ppid=,lstart=,command=`: "PPID Www Mmm D HH:MM:SS YYYY cmd".
/// lstart contains spaces, so the date is taken as the next five fields.
pub fn parse_ps_record(line: &str) -> Option<PsRecord> {
    let mut fields = line.split_whitespace();
    let ppid: i32 = fields.next()?.parse().ok()?;
    let date: Vec<&str> = fields.by_ref().take(5).collect();
    if date.len() != 5 {
        return None;
    }
    // The weekday is skipped: it adds nothing, and a mismatch would only
    // turn a usable time into none.
    let naive = NaiveDateTime::parse_from_str(&date[1..].join(" "), "%b %e %H:%M:%S %Y").ok()?;
    let started_at = Local.from_local_datetime(&naive).earliest()?.with_timezone(&Utc);
    // The command is everything after the six leading fields (ppid and the
    // five of the date), with its own spacing kept.
    let mut rest = line.trim_start();
    for _ in 0..6 {
        let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
        rest = rest[end..].trim_start();
    }
    let command = rest.trim_end().to_string();
    if command.is_empty() {
        return None;
    }
    Some(PsRecord { ppid, started_at, command })
}

fn ps_record(pid: i32) -> Option<PsRecord> {
    let out = crate::util::run_output("/bin/ps", ["-o", "ppid=,lstart=,command=", "-p", &pid.to_string()])?;
    parse_ps_record(out.trim())
}

pub struct SessionShell {
    pub started_at: DateTime<Utc>,
}

/// The interactive shell this command was run from: usually the parent, but
/// wrappers add a level or two, so walk up a bounded distance.
pub fn find_session_shell() -> Option<SessionShell> {
    let mut pid = crate::platform::parent_pid()?;
    for _ in 0..12 {
        if pid <= 1 {
            return None;
        }
        let rec = ps_record(pid)?;
        if is_session_shell(&rec.command) {
            return Some(SessionShell { started_at: rec.started_at });
        }
        pid = rec.ppid;
    }
    None
}

#[derive(Debug, PartialEq, Eq)]
pub enum SessionState {
    Unknown,
    Current,
    Stale,
}

pub fn alias_session_state(shell_started: Option<DateTime<Utc>>, aliases_written: Option<DateTime<Utc>>) -> SessionState {
    match (shell_started, aliases_written) {
        (Some(s), Some(w)) if w > s => SessionState::Stale,
        (Some(_), Some(_)) => SessionState::Current,
        _ => SessionState::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alias_line_for_zsh_and_bash() {
        assert_eq!(
            build_alias_line("zsh", "claude-work", Path::new("/Users/me/.claude-work"), None),
            r#"alias claude-work='CLAUDE_CONFIG_DIR="/Users/me/.claude-work" claude'"#
        );
        assert_eq!(
            build_alias_line("bash", "claude-w", Path::new("/u/.claude-w"), Some(Path::new("/u/.claude-w/gh"))),
            r#"alias claude-w='CLAUDE_CONFIG_DIR="/u/.claude-w" GH_CONFIG_DIR="/u/.claude-w/gh" claude'"#
        );
    }

    #[test]
    fn alias_line_keeps_awkward_paths_literal() {
        let line = build_alias_line("zsh", "claude-x", Path::new("/Users/o'brien/$HOME/`x`/\"q\""), None);
        assert_eq!(
            line,
            r#"alias claude-x='CLAUDE_CONFIG_DIR="/Users/o'\''brien/\$HOME/\`x\`/\"q\"" claude'"#
        );
    }

    #[test]
    fn alias_line_for_powershell_restores_the_environment() {
        assert_eq!(
            build_alias_line("pwsh", "claude-w", Path::new(r"C:\Users\it's\.claude-w"), None),
            r"function claude-w { $o0 = $env:CLAUDE_CONFIG_DIR; $env:CLAUDE_CONFIG_DIR = 'C:\Users\it''s\.claude-w'; try { claude @args } finally { $env:CLAUDE_CONFIG_DIR = $o0 } }"
        );
        let line = build_alias_line("powershell", "claude-w", Path::new("/w"), Some(Path::new("/w/gh")));
        assert!(line.contains("$o1 = $env:GH_CONFIG_DIR; $env:GH_CONFIG_DIR = '/w/gh'"));
        assert!(line.ends_with("finally { $env:CLAUDE_CONFIG_DIR = $o0; $env:GH_CONFIG_DIR = $o1 } }"));
    }

    #[test]
    fn alias_line_for_fish_is_a_function() {
        assert_eq!(
            build_alias_line("fish", "claude-work", Path::new("/u/.claude-work"), None),
            r#"function claude-work; CLAUDE_CONFIG_DIR="/u/.claude-work" claude $argv; end"#
        );
    }

    #[test]
    fn managed_lines_are_read_for_both_shells() {
        assert_eq!(parse_managed_line("alias claude-a='x'").as_deref(), Some("claude-a"));
        assert_eq!(parse_managed_line("  function claude-b; X=1 claude $argv; end").as_deref(), Some("claude-b"));
        assert_eq!(
            parse_managed_line("function claude-c { $o0 = $env:X; try { claude @args } finally { } }").as_deref(),
            Some("claude-c")
        );
        assert_eq!(parse_managed_line("# alias nope='x'"), None);
        assert_eq!(parse_managed_line("aliasx='y'"), None);
        assert_eq!(parse_managed_line(""), None);
    }

    #[test]
    fn block_is_created_replaced_and_removed() {
        let dir = tempfile::tempdir().unwrap();
        let rc = dir.path().join(".zshrc");
        std::fs::write(&rc, "export A=1\n").unwrap();
        write_aliases(&["alias claude-a='x'".into()], &rc).unwrap();
        let first = std::fs::read_to_string(&rc).unwrap();
        assert!(first.starts_with("export A=1\n\n# >>> claude-profiles >>>"));
        assert!(first.contains("# updated "));
        assert_eq!(read_managed_aliases(&rc).len(), 1);

        std::fs::write(&rc, format!("{first}export B=2\n")).unwrap();
        write_aliases(&["alias claude-a='x'".into(), "alias claude-b='y'".into()], &rc).unwrap();
        let second = std::fs::read_to_string(&rc).unwrap();
        assert_eq!(second.matches(BLOCK_START).count(), 1);
        assert!(second.contains("export A=1") && second.contains("export B=2"));
        assert_eq!(read_managed_aliases(&rc).len(), 2);

        write_aliases(&[], &rc).unwrap();
        let third = std::fs::read_to_string(&rc).unwrap();
        assert!(!third.contains(BLOCK_START));
        assert!(third.contains("export A=1") && third.contains("export B=2"));
    }

    #[test]
    fn an_unchanged_write_keeps_its_stamp() {
        let dir = tempfile::tempdir().unwrap();
        let rc = dir.path().join(".zshrc");
        write_aliases(&["alias claude-a='x'".into()], &rc).unwrap();
        let before = read_alias_stamp(&rc).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(5));
        write_aliases(&["alias claude-a='x'".into()], &rc).unwrap();
        assert_eq!(read_alias_stamp(&rc).unwrap(), before);
    }

    #[test]
    fn session_shells() {
        assert!(is_session_shell("-zsh"));
        assert!(is_session_shell("/bin/zsh -l"));
        assert!(is_session_shell("/opt/homebrew/bin/fish"));
        assert!(!is_session_shell("/bin/zsh -c echo hi"));
        assert!(!is_session_shell("node /x/claude-profiles"));
        assert!(!is_session_shell(""));
    }

    #[test]
    fn ps_record_takes_the_start_time_from_the_middle() {
        let r = parse_ps_record("  4242 Tue Nov  4 09:15:02 2026     -zsh --login").unwrap();
        assert_eq!(r.ppid, 4242);
        assert_eq!(r.command, "-zsh --login");
        assert_eq!(r.started_at.with_timezone(&Local).format("%H:%M:%S").to_string(), "09:15:02");
        assert!(parse_ps_record("garbage").is_none());
    }

    #[test]
    fn stale_only_when_both_times_are_known() {
        let t1 = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
        let t2 = Utc.with_ymd_and_hms(2026, 1, 2, 0, 0, 0).unwrap();
        assert_eq!(alias_session_state(Some(t1), Some(t2)), SessionState::Stale);
        assert_eq!(alias_session_state(Some(t2), Some(t1)), SessionState::Current);
        assert_eq!(alias_session_state(None, Some(t1)), SessionState::Unknown);
        assert_eq!(alias_session_state(Some(t1), None), SessionState::Unknown);
    }
}
