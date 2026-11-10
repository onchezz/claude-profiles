// Shared helpers: paths, names, versions, and running macOS's own tools.

use std::ffi::OsStr;
use std::path::{Path, PathBuf, MAIN_SEPARATOR};
use std::process::{Command, Stdio};

use crate::ui::{command, dim};

pub const LSREGISTER: &str = "/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister";

// ---- Paths ------------------------------------------------------------------

/// The home folder, read from $HOME each time so tests can point it elsewhere.
/// Windows sets USERPROFILE instead; $HOME still wins there when set, as it
/// does for Git Bash and MSYS.
pub fn home() -> PathBuf {
    let var = |k: &str| std::env::var_os(k).filter(|h| !h.is_empty()).map(PathBuf::from);
    var("HOME")
        .or_else(|| if cfg!(windows) { var("USERPROFILE") } else { None })
        .unwrap_or_else(|| PathBuf::from("/"))
}

/// Accept "~/foo" everywhere and resolve it once.
pub fn expand_home(p: &str) -> PathBuf {
    if p == "~" {
        return home();
    }
    if let Some(rest) = p.strip_prefix("~/") {
        return home().join(rest);
    }
    PathBuf::from(p)
}

/// Shorten "/Users/x/foo" to "~/foo" for display. Never for anything a user
/// will paste: a quoted "~" does not expand in zsh or bash.
pub fn tildify(p: impl AsRef<Path>) -> String {
    let s = p.as_ref().to_string_lossy().to_string();
    let h = home().to_string_lossy().to_string();
    if s == h {
        return "~".into();
    }
    let prefix = format!("{h}{MAIN_SEPARATOR}");
    match s.strip_prefix(&prefix) {
        Some(rest) => format!("~/{rest}"),
        None => s,
    }
}

pub fn file_exists(p: impl AsRef<Path>) -> bool {
    p.as_ref().exists()
}

pub fn path_string(p: impl AsRef<Path>) -> String {
    p.as_ref().to_string_lossy().to_string()
}

/// `path` with `suffix` appended to its last component, as in
/// "Claude w.app" + ".previous".
pub fn with_suffix(p: &Path, suffix: &str) -> PathBuf {
    let mut s = p.as_os_str().to_os_string();
    s.push(suffix);
    PathBuf::from(s)
}

/// A file's name without its extension, as in "Claude WORK" for
/// "/x/Claude WORK.app".
pub fn file_stem(p: &Path) -> String {
    p.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default()
}

/// Whether writing `file` stays inside `dir` once every link is resolved.
/// Broken links, and links that escape the folder, are never safe targets.
pub fn file_stays_within_dir(file: &Path, dir: &Path) -> bool {
    let Ok(root) = std::fs::canonicalize(dir) else { return false };
    let target = match std::fs::symlink_metadata(file) {
        Ok(_) => match std::fs::canonicalize(file) {
            Ok(t) => t,
            Err(_) => return false,
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let (Some(parent), Some(name)) = (file.parent(), file.file_name()) else {
                return false;
            };
            match std::fs::canonicalize(parent) {
                Ok(p) => p.join(name),
                Err(_) => return false,
            }
        }
        Err(_) => return false,
    };
    target.starts_with(&root) && target != root
}

/// Create a folder (and its parents) that only its owner can read. On
/// Windows a folder in the user's profile is already private to them.
#[cfg(not(unix))]
pub fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)
}

#[cfg(unix)]
pub fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
    // The mode above is filtered through the umask; set it outright.
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
}

/// Write a file that only its owner can read.
#[cfg(not(unix))]
pub fn write_private_file(file: &Path, contents: &str) -> std::io::Result<()> {
    std::fs::write(file, contents)
}

#[cfg(unix)]
pub fn write_private_file(file: &Path, contents: &str) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(file)?;
    f.write_all(contents.as_bytes())?;
    std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o600))
}

/// Remove a file or a folder and everything in it. Missing is not an error.
pub fn remove_all(p: &Path) -> std::io::Result<()> {
    match std::fs::symlink_metadata(p) {
        Ok(m) if m.is_dir() => std::fs::remove_dir_all(p),
        Ok(_) => std::fs::remove_file(p),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

// ---- JSON files ---------------------------------------------------------------

/// Pretty JSON with a trailing newline, the shape every file here is written in.
pub fn to_json_pretty(v: &impl serde::Serialize) -> String {
    let mut s = serde_json::to_string_pretty(v).unwrap_or_else(|_| "{}".into());
    s.push('\n');
    s
}

pub fn read_json(p: &Path) -> Option<serde_json::Value> {
    let raw = std::fs::read_to_string(p).ok()?;
    serde_json::from_str(&raw).ok()
}

// ---- Names --------------------------------------------------------------------

/// Strip everything that is not safe in folder names, .app names and shell
/// command names, all of which a profile name flows into.
pub fn sanitize_name(name: &str) -> String {
    let lower = name.trim().to_lowercase();
    let mut out = String::new();
    let mut in_bad = false;
    for c in lower.chars() {
        if c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' {
            out.push(c);
            in_bad = false;
        } else if !in_bad {
            out.push('-');
            in_bad = true;
        }
    }
    let trimmed = out.trim_matches('-');
    let mut collapsed = String::new();
    for c in trimmed.chars() {
        if c == '-' && collapsed.ends_with('-') {
            continue;
        }
        collapsed.push(c);
    }
    collapsed
}

/// The display name of the .app: "work" -> "WORK" for short names, and
/// "client-acme" -> "Client Acme" for longer ones.
pub fn title_case(name: &str) -> String {
    if name.chars().count() <= 4 {
        return name.to_uppercase();
    }
    name.split('-')
        .map(|part| {
            let mut c = part.chars();
            match c.next() {
                Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Numeric dotted-version compare. Enough for x.y.z; no prerelease handling.
pub fn compare_versions(a: &str, b: &str) -> std::cmp::Ordering {
    let parse = |s: &str| -> Vec<u64> {
        s.split('.')
            .map(|n| {
                let digits: String = n.chars().take_while(|c| c.is_ascii_digit()).collect();
                digits.parse().unwrap_or(0)
            })
            .collect()
    };
    let (pa, pb) = (parse(a), parse(b));
    for i in 0..pa.len().max(pb.len()) {
        let (x, y) = (pa.get(i).copied().unwrap_or(0), pb.get(i).copied().unwrap_or(0));
        if x != y {
            return x.cmp(&y);
        }
    }
    std::cmp::Ordering::Equal
}

// ---- Running tools ------------------------------------------------------------
//
// Every external program gets its arguments as a list, never a shell string,
// so a path can never be re-read as shell code.

/// Stdout of a command that succeeded, or None.
pub fn run_output<I, S>(program: &str, args: I) -> Option<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let out = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).to_string())
}

/// Whether a command ran and succeeded, with its output discarded.
pub fn run_quiet<I, S>(program: &str, args: I) -> bool
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Like run_quiet, but an error carries what the command printed.
pub fn run_checked<I, S>(program: &str, args: I) -> anyhow::Result<()>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let out = Command::new(program).args(args).stdin(Stdio::null()).output()?;
    if out.status.success() {
        return Ok(());
    }
    let said = String::from_utf8_lossy(&out.stderr).trim().to_string();
    anyhow::bail!(
        "{program} failed{}",
        if said.is_empty() { String::new() } else { format!(": {said}") }
    )
}

/// APFS clone of a folder, falling back to a real copy on other filesystems.
pub fn clone_copy(from: &Path, to: &Path) -> anyhow::Result<()> {
    if !is_mac() {
        return copy_tree(from, to);
    }
    if run_quiet("/bin/cp", [OsStr::new("-Rc"), from.as_os_str(), to.as_os_str()]) {
        return Ok(());
    }
    let _ = remove_all(to);
    run_checked("/bin/cp", [OsStr::new("-R"), from.as_os_str(), to.as_os_str()])
}

/// A plain recursive copy, for systems without `cp -c`. Links are copied as
/// links where the system allows it, so a copy never reaches outside `from`.
fn copy_tree(from: &Path, to: &Path) -> anyhow::Result<()> {
    let meta = std::fs::symlink_metadata(from)?;
    if meta.file_type().is_symlink() {
        let target = std::fs::read_link(from)?;
        #[cfg(unix)]
        std::os::unix::fs::symlink(&target, to)?;
        #[cfg(windows)]
        {
            let made = if from.is_dir() {
                std::os::windows::fs::symlink_dir(&target, to)
            } else {
                std::os::windows::fs::symlink_file(&target, to)
            };
            // Links need Developer Mode on Windows; copy what it points at instead.
            if made.is_err() && from.exists() {
                return copy_tree(&std::fs::canonicalize(from)?, to);
            }
        }
        return Ok(());
    }
    if meta.is_dir() {
        std::fs::create_dir_all(to)?;
        for e in std::fs::read_dir(from)? {
            let e = e?;
            copy_tree(&e.path(), &to.join(e.file_name()))?;
        }
        return Ok(());
    }
    std::fs::copy(from, to)?;
    Ok(())
}

pub fn is_mac() -> bool {
    cfg!(target_os = "macos")
}

// ---- Launch hints -------------------------------------------------------------

/// Print how to actually start a profile. The command and the display paths
/// disagree in case (a profile "ipsy" lives in Claude-IPSY but runs as
/// claude-ipsy), so the exact runnable command is printed rather than inferred.
pub fn launch_hints(profile: &crate::registry::Profile) -> Vec<(String, &'static str)> {
    let mut lines = Vec::new();
    if let Some(code) = &profile.code {
        if !code.alias_name.is_empty() {
            lines.push((code.alias_name.clone(), "Claude Code, in a terminal"));
        }
    }
    if let Some(d) = &profile.desktop {
        // Absolute, not tildified: a quoted "~" does not expand.
        lines.push((format!("open \"{}\"", path_string(&d.app_path)), "Claude Desktop"));
    }
    lines
}

pub fn print_launch_hints(profile: &crate::registry::Profile) {
    let lines = launch_hints(profile);
    if lines.is_empty() {
        return;
    }
    println!("    {}", dim("To launch:"));
    for (cmd, what) in lines {
        println!("      {}  {}", command(&cmd), dim(format!("({what})")));
    }
}

pub fn plural(n: usize, one: &str, many: &str) -> String {
    if n == 1 { one.to_string() } else { many.to_string() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cmp::Ordering;

    #[test]
    fn compare_versions_orders_numerically() {
        assert_eq!(compare_versions("0.1.9", "0.1.10"), Ordering::Less);
        assert_eq!(compare_versions("2.10.0", "2.9.0"), Ordering::Greater);
        assert_eq!(compare_versions("1.0", "1.0.0"), Ordering::Equal);
    }

    #[test]
    fn sanitize_name_trims_lowercases_and_replaces() {
        assert_eq!(sanitize_name("  Work  "), "work");
        assert_eq!(sanitize_name("Client ACME!"), "client-acme");
        assert_eq!(sanitize_name("--a__b--"), "a-b");
        assert_eq!(sanitize_name("a  --  b"), "a-b");
        assert_eq!(sanitize_name("!!!"), "");
    }

    #[test]
    fn title_case_short_and_long() {
        assert_eq!(title_case("work"), "WORK");
        assert_eq!(title_case("ipsy"), "IPSY");
        assert_eq!(title_case("client-acme"), "Client Acme");
        assert_eq!(title_case("personal"), "Personal");
    }

    #[cfg(unix)]
    #[test]
    fn files_inside_and_outside_a_folder() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("profile");
        let outside = root.path().join("outside");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        assert!(file_stays_within_dir(&dir.join("new.json"), &dir));
        std::os::unix::fs::symlink(outside.join("x.json"), dir.join("link.json")).unwrap();
        assert!(!file_stays_within_dir(&dir.join("link.json"), &dir), "broken link out");
        std::fs::write(outside.join("x.json"), "{}").unwrap();
        assert!(!file_stays_within_dir(&dir.join("link.json"), &dir), "link out");
        std::fs::write(dir.join("real.json"), "{}").unwrap();
        std::os::unix::fs::symlink(dir.join("real.json"), dir.join("inner.json")).unwrap();
        assert!(file_stays_within_dir(&dir.join("inner.json"), &dir), "link inside");
    }

    #[test]
    fn with_suffix_appends_to_the_last_component() {
        assert_eq!(with_suffix(Path::new("/a/Claude w.app"), ".previous"), PathBuf::from("/a/Claude w.app.previous"));
    }
}
