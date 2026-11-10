// What differs between macOS, Linux and Windows, in one place.
//
// Claude Code runs on all three; Claude Desktop profiles stay macOS-only
// (their launchers are AppleScript apps). Everything here keeps the rule the
// rest of the tool follows: programs get their arguments as a list, so a path
// is never re-read as shell code.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{bail, Result};

use crate::util::run_quiet;

// ---- Processes ----------------------------------------------------------------------

/// Whether a process with this ID exists.
pub fn pid_alive(pid: i32) -> bool {
    #[cfg(unix)]
    {
        unsafe { libc::kill(pid, 0) == 0 }
    }
    #[cfg(not(unix))]
    {
        let filter = format!("PID eq {pid}");
        crate::util::run_output("tasklist", ["/FI", filter.as_str(), "/NH", "/FO", "CSV"])
            .is_some_and(|o| o.contains(&format!("\"{pid}\"")))
    }
}

/// Ask a process to end (SIGTERM; on Windows, taskkill without /F, then with
/// it, since a console program ignores the polite close).
pub fn terminate(pid: i32) -> bool {
    #[cfg(unix)]
    {
        unsafe { libc::kill(pid, libc::SIGTERM) == 0 }
    }
    #[cfg(not(unix))]
    {
        let p = pid.to_string();
        run_quiet("taskkill", ["/PID", p.as_str(), "/T"]) || run_quiet("taskkill", ["/PID", p.as_str(), "/T", "/F"])
    }
}

/// This process's parent, where the system says.
pub fn parent_pid() -> Option<i32> {
    #[cfg(unix)]
    {
        Some(unsafe { libc::getppid() })
    }
    #[cfg(not(unix))]
    {
        None
    }
}

// ---- Files ------------------------------------------------------------------------------

/// Whether a file can be run: an execute bit on Unix; on Windows every file
/// that exists, since there the extension decides.
pub fn is_executable(p: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        p.is_file()
    }
}

/// Every copy of `cmd` on PATH, highest priority first, the way the shell
/// would find them. On Windows each PATHEXT extension is tried, so `claude`
/// finds claude.exe and npm's claude.cmd.
pub fn which_all(cmd: &str) -> Vec<String> {
    which_all_in(cmd, std::env::var_os("PATH").unwrap_or_default().as_os_str())
}

pub fn which_all_in(cmd: &str, path: &OsStr) -> Vec<String> {
    let exts: Vec<String> = if cfg!(windows) {
        let pathext = std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".into());
        pathext.split(';').filter(|e| !e.is_empty()).map(|e| e.to_lowercase()).collect()
    } else {
        vec![String::new()]
    };
    let mut out: Vec<String> = Vec::new();
    for dir in std::env::split_paths(path) {
        if dir.as_os_str().is_empty() {
            continue;
        }
        for ext in &exts {
            let candidate: PathBuf = dir.join(format!("{cmd}{ext}"));
            let s = candidate.to_string_lossy().to_string();
            if is_executable(&candidate) && !out.contains(&s) {
                out.push(s);
            }
        }
    }
    out
}

/// Show a file or folder in the system's file manager.
pub fn open_path(p: &Path) -> Result<()> {
    let opened = if cfg!(target_os = "macos") {
        run_quiet("/usr/bin/open", [p.as_os_str()])
    } else if cfg!(windows) {
        // explorer exits 1 even when it opened the folder.
        Command::new("explorer").arg(p).spawn().is_ok()
    } else {
        spawn_detached("xdg-open", [p.as_os_str()])
    };
    if !opened {
        bail!("Could not open {}.", p.display());
    }
    Ok(())
}

fn spawn_detached<I, S>(program: &str, args: I) -> bool
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .is_ok()
}

// ---- Shell commands ------------------------------------------------------------------------

/// One value for the shell a terminal window runs: POSIX single quotes, or
/// PowerShell's on Windows, where a quote is doubled instead.
pub fn quote(s: &str) -> String {
    if cfg!(windows) {
        format!("'{}'", s.replace('\'', "''"))
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

/// `NAME=value ... claude`, or PowerShell's `$env:NAME = 'value'; claude`.
pub fn with_env(vars: &[(&str, &str)], command: &str) -> String {
    if cfg!(windows) {
        let mut parts: Vec<String> = vars.iter().map(|(k, v)| format!("$env:{k} = {}", quote(v))).collect();
        parts.push(command.to_string());
        parts.join("; ")
    } else {
        let mut parts: Vec<String> = vars.iter().map(|(k, v)| format!("{k}={}", quote(v))).collect();
        parts.push(command.to_string());
        parts.join(" ")
    }
}

/// `command`, started in `dir`.
pub fn in_dir(dir: &str, command: &str) -> String {
    if cfg!(windows) {
        format!("Set-Location -LiteralPath {}; {command}", quote(dir))
    } else {
        format!("cd {} && {command}", quote(dir))
    }
}

// ---- Terminals ---------------------------------------------------------------------------

/// The terminals each system can open a command in. The first is the default.
pub fn terminals() -> &'static [&'static str] {
    if cfg!(target_os = "macos") {
        &["Terminal", "iTerm", "Ghostty", "Warp"]
    } else if cfg!(windows) {
        &["PowerShell", "Windows Terminal"]
    } else {
        &["Auto", "GNOME Terminal", "Konsole", "Kitty", "Alacritty", "WezTerm", "xterm"]
    }
}

/// Open a terminal window running `command`.
pub fn run_in_terminal(command: &str, app: &str) -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        mac_terminal(command, app)
    }
    #[cfg(windows)]
    {
        windows_terminal(command, app)
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        linux_terminal(command, app)
    }
}

/// The command reaches the terminal as an argument to a fixed AppleScript,
/// never as part of the script.
#[cfg(target_os = "macos")]
fn mac_terminal(command: &str, app: &str) -> Result<()> {
    let script = match app {
        "iTerm" => "on run argv\n tell application \"iTerm\"\n activate\n create window with default profile command \"/bin/zsh -lic \" & quoted form of (item 1 of argv)\n end tell\nend run",
        "Ghostty" | "Warp" => {
            // Neither takes a command over AppleScript; hand them a .command file.
            use std::os::unix::fs::PermissionsExt;
            let dir = tempfile::Builder::new().prefix("claude-profiles-").tempdir()?.keep();
            let file = dir.join("start.command");
            std::fs::write(&file, format!("#!/bin/zsh -l\n{command}\n"))?;
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

/// A new PowerShell console, kept open after the command ends.
#[cfg(windows)]
fn windows_terminal(command: &str, app: &str) -> Result<()> {
    use std::os::windows::process::CommandExt;
    const CREATE_NEW_CONSOLE: u32 = 0x0000_0010;
    let shell = if crate::platform::which_all("pwsh").is_empty() { "powershell" } else { "pwsh" };
    if app == "Windows Terminal" && !which_all("wt").is_empty() {
        // wt reads a bare ";" as its own separator between tabs.
        let escaped = command.replace(';', "\\;");
        if spawn_detached("wt", ["new-tab", "--", shell, "-NoExit", "-Command", escaped.as_str()]) {
            return Ok(());
        }
    }
    Command::new(shell)
        .args(["-NoExit", "-Command", command])
        .creation_flags(CREATE_NEW_CONSOLE)
        .spawn()
        .map(|_| ())
        .map_err(|e| anyhow::anyhow!("Could not open {shell}: {e}"))
}

/// The first terminal found, run through a login shell that stays open
/// after the command ends.
#[cfg(all(unix, not(target_os = "macos")))]
fn linux_terminal(command: &str, app: &str) -> Result<()> {
    let shell = std::env::var("SHELL").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "/bin/bash".into());
    let script = format!("{command}; exec {}", quote(&shell));
    let run = [shell.as_str(), "-lc", script.as_str()];
    // (program, flag that comes before the command)
    let known: &[(&str, &str, &str)] = &[
        ("GNOME Terminal", "gnome-terminal", "--"),
        ("Konsole", "konsole", "-e"),
        ("Kitty", "kitty", "--"),
        ("Alacritty", "alacritty", "-e"),
        ("WezTerm", "wezterm", "start --"),
        ("xterm", "xterm", "-e"),
    ];
    let mut order: Vec<(String, Vec<String>)> = Vec::new();
    let flags = |f: &str| f.split(' ').map(String::from).collect::<Vec<_>>();
    if app == "Auto" || app.is_empty() {
        if let Ok(t) = std::env::var("TERMINAL") {
            if !t.is_empty() {
                order.push((t, vec!["-e".into()]));
            }
        }
        order.push(("x-terminal-emulator".into(), vec!["-e".into()]));
        order.extend(known.iter().map(|(_, bin, f)| (bin.to_string(), flags(f))));
    } else if let Some((_, bin, f)) = known.iter().find(|(label, _, _)| *label == app) {
        order.push((bin.to_string(), flags(f)));
    } else {
        bail!("Unknown terminal: {app}");
    }
    for (bin, pre) in order {
        if which_all(&bin).is_empty() {
            continue;
        }
        let args: Vec<&str> = pre.iter().map(String::as_str).chain(run).collect();
        if spawn_detached(&bin, &args) {
            return Ok(());
        }
    }
    bail!("No terminal was found. Install one, or pick another in Settings.")
}

#[cfg(test)]
#[cfg(unix)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn which_finds_executables_in_path_order_without_duplicates() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let (a, b, c) = (root.path().join("a"), root.path().join("b"), root.path().join("c"));
        for d in [&a, &b, &c] {
            std::fs::create_dir_all(d).unwrap();
        }
        for d in [&a, &b] {
            std::fs::write(d.join("tool"), "#!/bin/sh").unwrap();
            std::fs::set_permissions(d.join("tool"), std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        std::fs::write(c.join("tool"), "not executable").unwrap();
        let path = std::env::join_paths([&b, &c, &a, &b]).unwrap();
        let found = which_all_in("tool", &path);
        assert_eq!(found, vec![b.join("tool").display().to_string(), a.join("tool").display().to_string()]);
    }

    #[cfg(unix)]
    #[test]
    fn commands_quote_values_for_the_shell() {
        assert_eq!(with_env(&[("A", "/x/it's")], "claude"), r"A='/x/it'\''s' claude");
        assert_eq!(in_dir("/a b", "claude"), "cd '/a b' && claude");
    }
}
