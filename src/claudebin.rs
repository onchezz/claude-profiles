// Resolve the `claude` binary the way the user's shell would, and find the
// default installs that exist before any profile does.
//
// Every profile shares ONE system-wide `claude` and one Claude Desktop;
// isolation is by config and data folder. If more than one `claude` is on
// PATH, the one that wins can change silently and every profile rides on it,
// so list, status and doctor all show the winner and its version.

use std::path::PathBuf;
use std::time::Duration;

use crate::util::{file_exists, home};

/// Every copy of `cmd` on PATH, highest priority first.
pub fn resolve_binaries(cmd: &str) -> Vec<String> {
    crate::platform::which_all(cmd)
}

/// First line of `<bin> --version`, or None. Gives up after ten seconds so a
/// hung binary cannot hang a health check.
pub fn binary_version(bin: &str) -> Option<String> {
    let out = output_with_timeout(bin, &["--version"], Duration::from_secs(10))?;
    if !out.0 {
        return None;
    }
    out.1.trim().lines().next().map(String::from).filter(|s| !s.is_empty())
}

/// (succeeded, stdout + stderr), or None if it could not start or timed out.
pub fn output_with_timeout(bin: &str, args: &[&str], limit: Duration) -> Option<(bool, String)> {
    use std::io::Read;
    use std::process::{Command, Stdio};
    let mut child = Command::new(bin)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .ok()?;
    let start = std::time::Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) if start.elapsed() < limit => std::thread::sleep(Duration::from_millis(25)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    };
    let mut text = String::new();
    if let Some(mut o) = child.stdout.take() {
        let _ = o.read_to_string(&mut text);
    }
    if let Some(mut e) = child.stderr.take() {
        let _ = e.read_to_string(&mut text);
    }
    Some((status.success(), text))
}

pub struct ClaudeBinary {
    pub winner: String,
    pub version: Option<String>,
    pub shadowed: Vec<String>,
}

pub fn resolve_claude() -> Option<ClaudeBinary> {
    let paths = resolve_binaries("claude");
    let winner = paths.first()?.clone();
    Some(ClaudeBinary {
        version: binary_version(&winner),
        shadowed: paths[1..].to_vec(),
        winner,
    })
}

// ---- The default installs ---------------------------------------------------------

/// /Applications first, then ~/Applications for installs without admin rights.
pub fn find_claude_app() -> Option<PathBuf> {
    [PathBuf::from("/Applications/Claude.app"), home().join("Applications").join("Claude.app")]
        .into_iter()
        .find(|p| file_exists(p))
}

pub fn default_desktop_data_dir() -> PathBuf {
    home().join("Library").join("Application Support").join("Claude")
}

pub fn default_claude_config_dir() -> PathBuf {
    home().join(".claude")
}

pub struct Defaults {
    /// (Claude.app, its data folder), when Desktop is installed and has run.
    pub desktop: Option<(PathBuf, PathBuf)>,
    pub code: Option<PathBuf>,
}

/// The installs the user had before this tool. Not profiles, but worth
/// showing so an empty registry does not look like the tool ignored them.
pub fn detect_defaults() -> Defaults {
    let data = default_desktop_data_dir();
    let code = default_claude_config_dir();
    Defaults {
        desktop: find_claude_app().filter(|_| file_exists(&data)).map(|app| (app, data)),
        code: file_exists(&code).then_some(code),
    }
}
