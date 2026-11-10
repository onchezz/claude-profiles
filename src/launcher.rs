// The launch helper: what a profile's launcher runs when you click it.
//
// Two jobs, done at the moment you reach for the profile:
//
//   1. Keep the profile's copy of Claude current. Copies do not update
//      themselves (see updates.rs), so when the installed Claude has moved
//      ahead, the helper rebuilds the copy from it, puts the colour back, and
//      opens it. About a second, once per Claude update.
//   2. Catch the profile running on the wrong account. A copy started any way
//      other than its launcher (a pinned window tile, Spotlight, a Login Item,
//      an update relaunch) has no --user-data-dir and shows the default
//      account. Clicking the launcher then asks whether to quit it and reopen
//      it properly.
//
// The helper is a copy of this binary at a fixed path, run as
// `claude-profiles __launch ...`. A copy, not the installed binary, so the
// launcher keeps working wherever cargo puts the tool and whatever PATH a Dock
// click gets. Every decision is compiled code; there is no script built from
// strings, and every program it runs gets its arguments as a list.
//
// The helper must never be the reason Claude does not open: every failure is
// logged and falls through to opening the copy as it is, and the launcher
// itself runs a plain `open` if the helper is missing.
//
// Environment, for tests: CMP_LAUNCH_DRY_RUN=1 reports the decision instead of
// acting; CMP_LAUNCH_ASSUME=reopen|leave answers the wrong-account question
// without a dialog; CMP_LAUNCH_LOG points the log elsewhere.

use std::ffi::OsStr;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use anyhow::{bail, Result};
use chrono::{Local, NaiveDateTime, TimeZone};
use sha2::{Digest, Sha256};

use crate::appclone::{app_version, hue_for, parse_running_copies, plist_value, ps_snapshot, tint, tool_support_dir};
use crate::util::{clone_copy, compare_versions, file_exists, file_stem, remove_all, with_suffix, LSREGISTER};

pub fn helper_path() -> PathBuf {
    tool_support_dir().join("bin").join("claude-profiles")
}

pub fn launch_log() -> PathBuf {
    tool_support_dir().join("launch.log")
}

/// The fixed part of the helper path every launcher that uses it contains.
/// Matched on this suffix rather than the full path, which sits inside two
/// escaping layers in the compiled launcher.
pub const HELPER_MARK: &str = "claude-profiles/bin/claude-profiles";

// ---- Installing the helper ------------------------------------------------------------

fn sha(p: &Path) -> Option<Vec<u8>> {
    let bytes = std::fs::read(p).ok()?;
    Some(Sha256::digest(bytes).to_vec())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HelperState {
    Missing,
    Current,
    /// A different build than the one running now.
    Outdated,
}

pub fn helper_state() -> HelperState {
    let installed = helper_path();
    if !file_exists(&installed) {
        return HelperState::Missing;
    }
    match (sha(&installed), std::env::current_exe().ok().and_then(|e| sha(&e))) {
        (Some(a), Some(b)) if a == b => HelperState::Current,
        (Some(_), _) => HelperState::Outdated,
        _ => HelperState::Missing,
    }
}

#[derive(Debug, PartialEq, Eq)]
#[allow(clippy::enum_variant_names)]
pub enum Installed {
    Installed,
    Updated,
    Current,
}

/// Copy the running binary into place as the helper. Written beside the
/// target and renamed over it, so a launcher clicked mid-install runs either
/// the old helper or the new one, never half of one.
pub fn install_helper() -> Result<Installed> {
    let before = helper_state();
    if before == HelperState::Current {
        return Ok(Installed::Current);
    }
    let dest = helper_path();
    let dir = dest.parent().expect("helper path has a parent");
    std::fs::create_dir_all(dir)?;
    let exe = std::env::current_exe()?;
    let tmp = with_suffix(&dest, ".installing");
    std::fs::copy(&exe, &tmp)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))?;
    }
    std::fs::rename(&tmp, &dest)?;
    Ok(if before == HelperState::Missing { Installed::Installed } else { Installed::Updated })
}

pub struct HelperSpec {
    pub path: PathBuf,
    pub source: PathBuf,
    pub hue: String,
    pub label: String,
}

/// What compile_app needs to route a launcher through the helper.
pub fn helper_spec_for(source: &Path, color: Option<&str>, app_path: &Path) -> HelperSpec {
    HelperSpec {
        path: helper_path(),
        source: source.to_path_buf(),
        hue: color.and_then(hue_for).map(|h| h.to_string()).unwrap_or_default(),
        label: file_stem(app_path),
    }
}

// ---- The log --------------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct LogEntry {
    pub at: chrono::DateTime<Local>,
    pub label: String,
    pub level: String,
    pub message: String,
    pub line: String,
}

/// Parse one "YYYY-MM-DD HH:MM:SS <label>: <LEVEL> <message>" line.
pub fn parse_log_line(line: &str) -> Option<LogEntry> {
    if line.len() < 20 {
        return None;
    }
    let (stamp, rest) = line.split_at(19);
    let naive = NaiveDateTime::parse_from_str(stamp, "%Y-%m-%d %H:%M:%S").ok()?;
    let at = Local.from_local_datetime(&naive).earliest()?;
    let rest = rest.strip_prefix(' ')?;
    for level in ["INFO", "WARN", "ERROR"] {
        let sep = format!(": {level} ");
        if let Some(i) = rest.find(&sep) {
            return Some(LogEntry {
                at,
                label: rest[..i].to_string(),
                level: level.into(),
                message: rest[i + sep.len()..].to_string(),
                line: line.to_string(),
            });
        }
    }
    None
}

/// Recent helper log lines, oldest first.
pub fn recent_launch_log(since: Duration, levels: Option<&[&str]>) -> Vec<LogEntry> {
    let Ok(text) = std::fs::read_to_string(launch_log()) else { return Vec::new() };
    let cutoff = Local::now() - chrono::Duration::from_std(since).unwrap_or_default();
    text.lines()
        .filter_map(parse_log_line)
        .filter(|e| e.at >= cutoff && levels.map_or(true, |l| l.contains(&e.level.as_str())))
        .collect()
}

/// The newest update a launcher started and never finished: an "updating"
/// line with no outcome after it. Only a helper killed part-way leaves one.
pub fn unfinished_refresh(label: &str, entries: &[LogEntry]) -> Option<LogEntry> {
    let mut pending = None;
    for e in entries.iter().filter(|e| e.label == label) {
        if e.message.starts_with("updating the copy from ") {
            pending = Some(e.clone());
        } else if e.message.starts_with("updated the copy") || e.message.starts_with("could not update the copy") {
            pending = None;
        }
    }
    pending
}

/// Folders an update leaves beside the copy only when it is cut off.
pub fn refresh_leftovers(clone: &Path) -> Vec<PathBuf> {
    [".refreshing", ".previous"].iter().map(|s| with_suffix(clone, s)).filter(|p| file_exists(p)).collect()
}

// ---- The helper itself ------------------------------------------------------------------------

fn env(name: &str) -> String {
    std::env::var(name).unwrap_or_default()
}

fn dry() -> bool {
    env("CMP_LAUNCH_DRY_RUN") == "1"
}

struct Helper {
    label: String,
    log_path: PathBuf,
}

impl Helper {
    fn log(&self, level: &str, msg: &str) {
        // A dry run reports; it must not leave lines for doctor to find.
        if dry() {
            return;
        }
        let line = format!(
            "{} {}: {} {}\n",
            Local::now().format("%Y-%m-%d %H:%M:%S"),
            self.label,
            level,
            msg.replace(['\r', '\n'], " ")
        );
        let _ = (|| -> std::io::Result<()> {
            if let Some(dir) = self.log_path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            // Keep the log small: past 256 KB, drop the older half.
            if std::fs::metadata(&self.log_path).map(|m| m.len() > 262_144).unwrap_or(false) {
                let old = std::fs::read_to_string(&self.log_path).unwrap_or_default();
                let mut cut = old.len() / 2;
                while !old.is_char_boundary(cut) {
                    cut += 1;
                }
                let keep = old[cut..].find('\n').map(|i| &old[cut + i + 1..]).unwrap_or("");
                std::fs::write(&self.log_path, keep)?;
            }
            let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&self.log_path)?;
            f.write_all(line.as_bytes())
        })();
    }

    /// Ask a question in a dialog. Returns the button chosen; anything that
    /// goes wrong counts as the first (cautious) button.
    fn ask(&self, message: &str, buttons: &[&str], default: &str) -> String {
        let assumed = env("CMP_LAUNCH_ASSUME");
        if !assumed.is_empty() {
            return if assumed == "reopen" { default.to_string() } else { buttons[0].to_string() };
        }
        // Fixed script; the message, title and buttons arrive as arguments.
        let script = if buttons.len() == 1 {
            "on run argv\n activate\n set r to display dialog (item 1 of argv) with title (item 2 of argv) buttons {(item 3 of argv)} default button 1 with icon caution\n return button returned of r\nend run"
        } else {
            "on run argv\n activate\n set r to display dialog (item 1 of argv) with title (item 2 of argv) buttons {(item 3 of argv), (item 4 of argv)} default button (item 4 of argv) cancel button (item 3 of argv) with icon caution\n return button returned of r\nend run"
        };
        let mut args: Vec<&str> = vec!["-e", script, message, &self.label];
        args.extend(buttons.iter().copied());
        crate::util::run_output("/usr/bin/osascript", &args)
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| buttons[0].to_string())
    }

    /// Put things back after an update that was cut off part-way: the copy
    /// returned to its path if the cut fell between the two moves, and the
    /// leftovers cleared. True when there was anything to recover from.
    fn recover(&self, clone: &Path) -> Result<bool> {
        let tmp = with_suffix(clone, ".refreshing");
        let old = with_suffix(clone, ".previous");
        let mut interrupted = false;
        if !file_exists(clone) && file_exists(&old) {
            std::fs::rename(&old, clone)?;
            self.log("WARN", "an earlier update stopped between swapping the copies; put the previous copy back");
            interrupted = true;
        }
        if file_exists(&tmp) {
            remove_all(&tmp)?;
            self.log("WARN", "an earlier update stopped part-way; removed what it left behind");
            interrupted = true;
        }
        if file_exists(&old) && file_exists(clone) {
            remove_all(&old)?;
        }
        Ok(interrupted)
    }

    /// Rebuild the copy when its source is newer. The new copy is built beside
    /// the old one and swapped in only once complete, so a failure at any step
    /// leaves the old copy as it was. After an interrupted attempt, this click
    /// opens the copy as it is and the next one updates it, so whatever cut the
    /// last attempt short cannot stop the profile opening twice in a row.
    fn refresh(&self, clone: &Path, source: &Path, hue: &str, interrupted: bool) -> String {
        let (have, want) = (app_version(clone), app_version(source));
        let (Some(have), Some(want)) = (have.clone(), want.clone()) else {
            self.log(
                "WARN",
                &format!(
                    "could not read versions (copy {}, source {}); opening as is",
                    have.as_deref().unwrap_or("null"),
                    want.as_deref().unwrap_or("null")
                ),
            );
            return "unknown".into();
        };
        if compare_versions(&have, &want) != std::cmp::Ordering::Less {
            return "current".into();
        }
        if dry() {
            return format!("would-refresh {have} -> {want}");
        }
        if interrupted {
            self.log("WARN", &format!("opening {have} this time; the update to {want} happens on the next click"));
            return "deferred".into();
        }
        // Logged before any work, so a helper killed part-way still leaves a
        // line for doctor to find.
        self.log("INFO", &format!("updating the copy from {have} to {want}"));
        let tmp = with_suffix(clone, ".refreshing");
        let old = with_suffix(clone, ".previous");
        let attempt = || -> Result<()> {
            remove_all(&tmp)?;
            clone_copy(source, &tmp)?;
            let made = app_version(&tmp);
            if made.as_deref() != Some(want.as_str()) {
                bail!("the copy came out as {}, not {want}", made.as_deref().unwrap_or("null"));
            }
            if !hue.is_empty() {
                let degrees: u32 = hue.parse().unwrap_or(0);
                tint(&tmp, degrees, &tmp)?;
            }
            remove_all(&old)?;
            std::fs::rename(clone, &old)?;
            if let Err(e) = std::fs::rename(&tmp, clone) {
                std::fs::rename(&old, clone)?;
                return Err(e.into());
            }
            remove_all(&old)?;
            crate::util::run_quiet(LSREGISTER, [OsStr::new("-f"), clone.as_os_str()]);
            Ok(())
        };
        match attempt() {
            Ok(()) => {
                self.log("INFO", &format!("updated the copy from {have} to {want}"));
                "refreshed".into()
            }
            Err(e) => {
                let _ = remove_all(&tmp);
                self.log("ERROR", &format!("could not update the copy from {have} to {want}: {e}; opening {have}"));
                "failed".into()
            }
        }
    }
}

/// Only ever open Claude. Anything else (a launcher, most dangerously this
/// profile's own) would run the helper again and loop. A missing copy is
/// different: `open` fails on it harmlessly.
fn is_claude(app: &Path) -> bool {
    plist_value(app, "CFBundleIdentifier").map_or(true, |id| id.starts_with("com.anthropic."))
}

/// Followed by a space or the end, so Claude-WORK never matches Claude-WORK2.
pub fn on_profile(command: &str, data_dir: &str) -> bool {
    let flag = format!(" --user-data-dir={data_dir}");
    let mut from = 0;
    while let Some(i) = command[from..].find(&flag) {
        let end = from + i + flag.len();
        match command[end..].chars().next() {
            None | Some(' ') => return true,
            _ => from = from + i + 1,
        }
    }
    false
}

fn alive(pid: i32) -> bool {
    crate::platform::pid_alive(pid)
}

/// Ask specific processes to quit the way Claude's own Quit menu would, then
/// wait up to fifteen seconds for them to go.
fn quit(pids: &[i32]) -> bool {
    const TERMINATE: &str = "ObjC.import('AppKit');\nfunction run(argv) {\n  argv.forEach(function (p) {\n    var ra = $.NSRunningApplication.runningApplicationWithProcessIdentifier(parseInt(p, 10));\n    if (!ra.isNil()) ra.terminate;\n  });\n  return 'OK';\n}";
    let mut args: Vec<String> = vec!["-l".into(), "JavaScript".into(), "-e".into(), TERMINATE.into()];
    args.extend(pids.iter().map(|p| p.to_string()));
    crate::util::run_quiet("/usr/bin/osascript", &args);
    for _ in 0..60 {
        if pids.iter().all(|p| !alive(*p)) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    false
}

fn open(args: &[String]) {
    let _ = Command::new("/usr/bin/open").args(args).status();
}

/// `claude-profiles __launch <clone> <source> <hue|""> <dataDir> <label> <open args...>`
/// Returns the decision, which is printed (the launcher discards it; tests
/// read it).
pub fn run(argv: &[String]) -> String {
    if argv.len() < 5 {
        return "usage: __launch <clone> <source> <hue> <dataDir> <label> <open args...>".into();
    }
    let clone = PathBuf::from(&argv[0]);
    let source = PathBuf::from(&argv[1]);
    let (hue, data_dir) = (&argv[2], &argv[3]);
    let label = if argv[4].is_empty() { "launcher".to_string() } else { argv[4].clone() };
    let open_args = &argv[5..];
    let log_path = Some(env("CMP_LAUNCH_LOG")).filter(|s| !s.is_empty()).map(PathBuf::from).unwrap_or_else(launch_log);
    let h = Helper { label: label.clone(), log_path };
    let dry = dry();

    // First, because a copy cut off between the two moves is not where the
    // checks below expect it.
    let mut interrupted = false;
    if !dry {
        match h.recover(&clone) {
            Ok(i) => interrupted = i,
            Err(e) => h.log("ERROR", &format!("could not clean up after an earlier update: {e}")),
        }
    }

    if !is_claude(&clone) {
        h.log(
            "ERROR",
            &format!(
                "{} is not a copy of Claude (bundle {}); not opening it. Run claude-profiles doctor --fix",
                clone.display(),
                plist_value(&clone, "CFBundleIdentifier").unwrap_or_else(|| "null".into())
            ),
        );
        return if dry { "refused: not Claude".into() } else { "refused".into() };
    }

    let procs = parse_running_copies(&ps_snapshot(), &clone);
    if !procs.is_empty() {
        let wrong: Vec<i32> = procs.iter().filter(|p| !on_profile(&p.command, data_dir)).map(|p| p.pid).collect();
        if wrong.is_empty() {
            if dry {
                return "running on its profile: focus".into();
            }
            open(open_args);
            return "focused".into();
        }
        let list = wrong.iter().map(|p| p.to_string()).collect::<Vec<_>>().join(", ");
        h.log("WARN", &format!("open on the wrong account (pid {list}, not started with this profile's data folder)"));
        if dry {
            return format!("wrong account: pid {list}");
        }
        let answer = h.ask(
            &format!(
                "{label} is open on the wrong account.\n\nIt was started without its profile (by a Claude update, a pinned window tile, Spotlight or a Login Item), so it is showing your default Claude account instead.\n\nQuit it and reopen {label} correctly?"
            ),
            &["Leave It Open", "Quit and Reopen"],
            "Quit and Reopen",
        );
        if answer != "Quit and Reopen" {
            h.log("INFO", "left the wrong-account window open, as asked");
            return "left".into();
        }
        if !quit(&wrong) {
            h.log("WARN", "it did not quit within 15 seconds");
            h.ask(&format!("{label} did not quit. Quit it from its Claude menu, then click {label} again."), &["OK"], "OK");
            return "quit-timeout".into();
        }
        h.log("INFO", "quit the wrong-account window; reopening on the profile");
    }

    let r = h.refresh(&clone, &source, hue, interrupted);
    if dry {
        return format!("not running: {r}");
    }
    open(open_args);
    "opened".into()
}

#[cfg(test)]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
mod tests {
    use super::*;

    // The helper reads its switches from the environment, which is shared by
    // every test thread; tests that set them take turns.
    static ENV: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn fake_app(dir: &Path, version: &str, bundle: Option<&str>) {
        std::fs::create_dir_all(dir.join("Contents")).unwrap();
        let id = bundle.map(|b| format!("<key>CFBundleIdentifier</key><string>{b}</string>")).unwrap_or_default();
        std::fs::write(
            dir.join("Contents/Info.plist"),
            format!(r#"<?xml version="1.0" encoding="UTF-8"?><plist version="1.0"><dict>{id}<key>CFBundleShortVersionString</key><string>{version}</string></dict></plist>"#),
        )
        .unwrap();
    }

    fn argv(clone: &Path, source: &Path, data: &Path) -> Vec<String> {
        vec![
            clone.display().to_string(),
            source.display().to_string(),
            String::new(),
            data.display().to_string(),
            "Claude T".into(),
            "-a".into(),
            clone.display().to_string(),
        ]
    }

    #[test]
    fn on_profile_matches_the_exact_folder_only() {
        assert!(on_profile("/x/Claude --user-data-dir=/U/Claude-W", "/U/Claude-W"));
        assert!(on_profile("/x/Claude --user-data-dir=/U/Claude-W --other", "/U/Claude-W"));
        assert!(!on_profile("/x/Claude --user-data-dir=/U/Claude-W2", "/U/Claude-W"));
        assert!(!on_profile("/x/Claude", "/U/Claude-W"));
    }

    #[test]
    fn log_lines_round_trip_and_unfinished_updates_are_found() {
        let lines = [
            "2026-11-01 10:00:00 Claude W: INFO updating the copy from 1.0 to 2.0",
            "2026-11-01 10:00:01 Claude W: INFO updated the copy from 1.0 to 2.0",
            "2026-11-02 10:00:00 Claude W: INFO updating the copy from 2.0 to 3.0",
            "2026-11-02 10:00:00 Claude X: ERROR something",
            "not a log line",
        ];
        let entries: Vec<LogEntry> = lines.iter().filter_map(|l| parse_log_line(l)).collect();
        assert_eq!(entries.len(), 4);
        assert_eq!(entries[3].label, "Claude X");
        assert_eq!(entries[3].level, "ERROR");
        let pending = unfinished_refresh("Claude W", &entries).unwrap();
        assert!(pending.message.contains("2.0 to 3.0"));
        assert!(unfinished_refresh("Claude X", &entries).is_none());

        let dir = tempfile::tempdir().unwrap();
        let clone = dir.path().join("Claude w.app");
        std::fs::create_dir_all(with_suffix(&clone, ".previous")).unwrap();
        assert_eq!(refresh_leftovers(&clone), vec![with_suffix(&clone, ".previous")]);
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn decides_by_version_and_never_downgrades() {
        let _env = ENV.lock().unwrap_or_else(|e| e.into_inner());
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("Source.app");
        fake_app(&source, "2.10.0", None);
        let decide = |name: &str, version: Option<&str>| {
            let clone = root.path().join(name);
            if let Some(v) = version {
                fake_app(&clone, v, None);
            }
            std::env::set_var("CMP_LAUNCH_DRY_RUN", "1");
            let r = run(&argv(&clone, &source, &root.path().join("data")));
            std::env::remove_var("CMP_LAUNCH_DRY_RUN");
            r
        };
        assert_eq!(decide("Behind.app", Some("2.9.0")), "not running: would-refresh 2.9.0 -> 2.10.0");
        assert_eq!(decide("Current.app", Some("2.10.0")), "not running: current");
        assert_eq!(decide("Ahead.app", Some("2.11.0")), "not running: current");
        assert_eq!(decide("Missing.app", None), "not running: unknown");

        let launcher = root.path().join("Launcher.app");
        fake_app(&launcher, "1.0", Some("com.claude-profiles.t"));
        assert_eq!(decide("Launcher.app", None), "refused: not Claude");
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn the_click_after_a_cut_off_update_recovers_and_the_next_one_updates() {
        let _env = ENV.lock().unwrap_or_else(|e| e.into_inner());
        let root = tempfile::tempdir().unwrap();
        let log = root.path().join("launch.log");
        let source = root.path().join("Source.app");
        fake_app(&source, "2.0.0", Some("com.anthropic.claudefordesktop"));
        let clone = root.path().join("Claude w.app");
        // The worst cut: between the two moves.
        fake_app(&with_suffix(&clone, ".previous"), "1.0.0", Some("com.anthropic.claudefordesktop"));
        std::fs::create_dir_all(with_suffix(&clone, ".refreshing")).unwrap();

        // open(1) on a fake app fails harmlessly; point it at nothing useful.
        let mut args = argv(&clone, &source, &root.path().join("data"));
        args.truncate(5);
        args.push("--help-not-an-app".into());
        std::env::set_var("CMP_LAUNCH_LOG", &log);
        let first = run(&args);
        assert_eq!(first, "opened");
        assert!(clone.join("Contents/Info.plist").exists());
        assert!(!with_suffix(&clone, ".refreshing").exists());
        assert!(!with_suffix(&clone, ".previous").exists());
        let text = std::fs::read_to_string(&log).unwrap();
        assert!(text.contains("put the previous copy back"));
        assert!(text.contains("opening 1.0.0 this time"));

        run(&args);
        std::env::remove_var("CMP_LAUNCH_LOG");
        let text = std::fs::read_to_string(&log).unwrap();
        let a = text.find("updating the copy from 1.0.0 to 2.0.0").unwrap();
        let b = text.find("updated the copy from 1.0.0 to 2.0.0").unwrap();
        assert!(a < b);
        assert!(std::fs::read_to_string(clone.join("Contents/Info.plist")).unwrap().contains("2.0.0"));
    }
}
