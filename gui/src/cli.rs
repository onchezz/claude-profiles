// Talking to `claude-profiles app …`, the same JSON interface the macOS menu
// bar app uses. The window holds no logic of its own: every change is a call
// to the command, which prints one JSON object or fails with a reason on
// stderr.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;

use serde_json::Value;

const EXE: &str = if cfg!(windows) { "claude-profiles.exe" } else { "claude-profiles" };

pub fn home() -> PathBuf {
    let var = |k: &str| std::env::var_os(k).filter(|h| !h.is_empty()).map(PathBuf::from);
    var("HOME")
        .or_else(|| if cfg!(windows) { var("USERPROFILE") } else { None })
        .unwrap_or_else(|| PathBuf::from("/"))
}

/// The command's config folder, where the window keeps its own settings too.
pub fn config_dir() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".config"))
        .join("claude-profiles")
}

fn path_dirs() -> Vec<PathBuf> {
    std::env::split_paths(environment_path().as_os_str()).collect()
}

/// The command to drive: $CLAUDE_PROFILES_BIN, then one beside this program
/// (how releases ship them), then the macOS app bundle's copy, then PATH.
pub fn binary() -> Option<PathBuf> {
    static BIN: OnceLock<Option<PathBuf>> = OnceLock::new();
    BIN.get_or_init(|| {
        if let Some(p) = std::env::var_os("CLAUDE_PROFILES_BIN").map(PathBuf::from).filter(|p| p.is_file()) {
            return Some(p);
        }
        let here = std::env::current_exe().ok().and_then(|e| e.parent().map(Path::to_path_buf));
        let mut candidates: Vec<PathBuf> = Vec::new();
        if let Some(dir) = &here {
            candidates.push(dir.join(EXE));
            candidates.push(dir.join("../Resources").join(EXE));
        }
        candidates.extend(path_dirs().into_iter().map(|d| d.join(EXE)));
        candidates.push(home().join(".cargo").join("bin").join(EXE));
        candidates.into_iter().find(|p| p.is_file())
    })
    .clone()
}

/// Programs started from a desktop launcher get a bare PATH, so `claude`,
/// `gh` and `npm` would be invisible to the command. Use the login shell's
/// PATH, plus the usual install folders.
fn environment_path() -> OsString {
    static PATH: OnceLock<OsString> = OnceLock::new();
    PATH.get_or_init(|| {
        let h = home();
        let mut dirs: Vec<PathBuf> = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()).collect();
        if cfg!(windows) {
            if let Some(appdata) = std::env::var_os("APPDATA") {
                dirs.push(PathBuf::from(appdata).join("npm"));
            }
            dirs.extend([h.join(".local").join("bin"), h.join(".cargo").join("bin")]);
        } else {
            let shell = std::env::var("SHELL").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "/bin/sh".into());
            if let Ok(out) = Command::new(&shell).args(["-l", "-c", "printf %s \"$PATH\""]).stdin(Stdio::null()).stderr(Stdio::null()).output() {
                let login = String::from_utf8_lossy(&out.stdout).to_string();
                let mut first: Vec<PathBuf> = std::env::split_paths(&login).collect();
                first.append(&mut dirs);
                dirs = first;
            }
            dirs.extend([
                h.join(".local/bin"),
                h.join(".cargo/bin"),
                h.join(".npm-global/bin"),
                h.join(".bun/bin"),
                "/opt/homebrew/bin".into(),
                "/usr/local/bin".into(),
                "/usr/bin".into(),
                "/bin".into(),
            ]);
        }
        let mut seen = std::collections::HashSet::new();
        dirs.retain(|d| !d.as_os_str().is_empty() && seen.insert(d.clone()));
        std::env::join_paths(dirs).unwrap_or_default()
    })
    .clone()
}

/// Run `claude-profiles app <args>` and return its JSON reply, or the reason
/// it gave for failing.
pub fn run(args: &[String]) -> Result<Value, String> {
    let bin = binary().ok_or_else(|| {
        "The claude-profiles command was not found. Install it with `cargo install --path .`, or put it beside this program."
            .to_string()
    })?;
    let mut cmd = Command::new(bin);
    cmd.arg("app").args(args).env("PATH", environment_path()).stdin(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let out = cmd.output().map_err(|e| format!("Could not run claude-profiles: {e}"))?;
    if !out.status.success() {
        let said = String::from_utf8_lossy(&out.stderr);
        // Progress lines come first; the reason is the last thing said.
        let reason = said.lines().map(str::trim).rfind(|l| !l.is_empty()).unwrap_or("claude-profiles failed.");
        return Err(strip_ansi(reason).trim_start_matches(['✗', '✖', ' ']).to_string());
    }
    serde_json::from_slice(&out.stdout).map_err(|e| format!("claude-profiles replied with something unexpected: {e}"))
}

fn strip_ansi(s: &str) -> String {
    let mut out = String::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            for n in chars.by_ref() {
                if n.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn ansi_colours_are_removed() {
        assert_eq!(super::strip_ansi("\u{1b}[31mNo profile\u{1b}[0m"), "No profile");
    }
}
