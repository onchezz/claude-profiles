// Claude Desktop profiles.
//
// Claude Desktop is Electron, and Electron honours --user-data-dir: the whole
// app state (sign-in, chats, settings, connectors) moves to a folder of your
// choosing. A Desktop profile is that folder, its own copy of Claude.app (see
// appclone.rs), and a launcher .app that opens the copy with the flag.
//
// The launcher is a one-line AppleScript compiled by `osacompile`, which gives
// a real app for the Dock, Spotlight and the app switcher.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde_json::{Map, Value};

use crate::appclone::{apply_color, clone_path_for, ensure_colored_clone, find_app_icns, ps_snapshot};
use crate::launcher::{helper_spec_for, install_helper, HelperSpec, HELPER_MARK};
use crate::ui::{command, info, ok, path_str, step, warn};
use crate::updates::{block_updates, BlockResult};
use crate::util::{
    file_exists, file_stays_within_dir, home, read_json, remove_all, run_checked, run_output, run_quiet, tildify,
    title_case, to_json_pretty,
};

const PLIST_BUDDY: &str = "/usr/libexec/PlistBuddy";

/// Every osacompile'd bundle ships with this identifier until we stamp our
/// own. Two launchers sharing it confuse LaunchServices.
pub const DEFAULT_APPLET_BUNDLE_ID: &str = "com.apple.ScriptEditor.id.applet";

/// What a launcher compiled from a temporary folder called itself (#14 in
/// the original). Only this exact name is ever changed.
pub const STAGED_LAUNCHER_NAME: &str = "staged";

fn app_support() -> PathBuf {
    home().join("Library").join("Application Support")
}

pub fn default_data_dir_for(name: &str) -> PathBuf {
    app_support().join(format!("Claude-{}", title_case(name)))
}

/// ~/Applications, because it never needs an administrator password.
pub fn default_app_path_for(name: &str) -> PathBuf {
    home().join("Applications").join(format!("Claude {}.app", title_case(name)))
}

// ---- MCP connectors ---------------------------------------------------------------------
//
// Desktop reads its connectors from <data dir>/claude_desktop_config.json. The
// file also holds `preferences` and anything added by hand, so the mcpServers
// map is merged, never the file replaced, and a file that does not parse is
// left alone. Claude rewrites the file while it runs, so callers make sure the
// profile is not open.

pub fn default_desktop_config_file() -> PathBuf {
    app_support().join("Claude").join("claude_desktop_config.json")
}

pub fn desktop_config_file(data_dir: &Path) -> PathBuf {
    data_dir.join("claude_desktop_config.json")
}

#[derive(Debug, PartialEq, Eq)]
pub enum SeedStatus {
    Seeded,
    /// The profile already had every server.
    Current,
    /// Nothing to copy.
    None,
    /// The profile's config could not be parsed; left alone.
    Foreign,
    /// The profile's config is a link out of the profile, or broken.
    UnsafePath,
}

#[derive(Debug)]
pub struct SeedResult {
    pub status: SeedStatus,
    pub added: Vec<String>,
    pub total: usize,
}

/// Add the source's MCP servers (only those named in `only`, when given) to a
/// profile. A server the profile already has is never replaced: a work
/// profile's `github` server, with its work token, must not be overwritten by
/// the default profile's. Copying only ever adds.
pub fn seed_desktop_mcp_servers(data_dir: &Path, source_file: &Path, only: Option<&[String]>) -> SeedResult {
    let nothing = SeedResult { status: SeedStatus::None, added: vec![], total: 0 };
    let Some(Value::Object(mut servers)) = read_json(source_file).and_then(|v| v.get("mcpServers").cloned()) else {
        return nothing;
    };
    if let Some(only) = only {
        servers.retain(|k, _| only.contains(k));
    }
    if servers.is_empty() {
        return nothing;
    }
    let file = desktop_config_file(data_dir);
    if !file_stays_within_dir(&file, data_dir) {
        return SeedResult { status: SeedStatus::UnsafePath, added: vec![], total: 0 };
    }
    let mut config = Map::new();
    if file_exists(&file) {
        match read_json(&file) {
            Some(Value::Object(m)) => config = m,
            _ => return SeedResult { status: SeedStatus::Foreign, added: vec![], total: 0 },
        }
    }
    let existing = match config.get("mcpServers") {
        Some(Value::Object(m)) => m.clone(),
        _ => Map::new(),
    };
    let added: Vec<String> = servers.keys().filter(|k| !existing.contains_key(*k)).cloned().collect();
    // The source's first, then the profile's own on top, so the profile wins.
    let mut merged = servers;
    for (k, v) in existing {
        merged.insert(k, v);
    }
    let total = merged.len();
    if added.is_empty() {
        return SeedResult { status: SeedStatus::Current, added, total };
    }
    config.insert("mcpServers".into(), Value::Object(merged));
    if std::fs::write(&file, to_json_pretty(&config)).is_err() {
        return SeedResult { status: SeedStatus::Foreign, added: vec![], total: 0 };
    }
    SeedResult { status: SeedStatus::Seeded, added, total }
}

pub fn mcp_server_names(file: &Path) -> Option<Vec<String>> {
    let v = read_json(file)?;
    Some(match v.get("mcpServers") {
        Some(Value::Object(m)) => m.keys().cloned().collect(),
        _ => Vec::new(),
    })
}

// ---- The launcher's script ----------------------------------------------------------------

/// POSIX single quotes, with embedded apostrophes closed, escaped and reopened.
pub fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// An AppleScript string literal. AppleScript escapes only backslash and
/// double quote; everything else, apostrophes included, is literal.
pub fn applescript_string(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

pub struct LaunchSpec<'a> {
    pub data_dir: &'a Path,
    pub claude_app: &'a Path,
    pub code_config_dir: Option<&'a Path>,
    pub gh_config_dir: Option<&'a Path>,
    /// The profile has its own copy of Claude.app.
    pub dedicated: bool,
    pub helper: Option<&'a HelperSpec>,
}

/// The AppleScript a launcher runs.
///
/// Paths cross two quoting layers: each sits in shell single quotes inside
/// the AppleScript string that `do shell script` takes. Both are applied, in
/// that order, so a home folder such as /Users/o'brien works.
///
/// `-n` (a new instance) is needed only when the target is the shared
/// Claude.app; otherwise macOS hands the request to whatever Claude is
/// running and drops --user-data-dir. With a copy of its own, `open -a` on
/// the copy opens the profile when it is closed and focuses it when it is
/// open, instead of stacking up windows.
///
/// `--env CLAUDE_CONFIG_DIR` reaches the Claude Code sessions Desktop starts,
/// which would otherwise all share ~/.claude; GH_CONFIG_DIR does the same for
/// the GitHub CLI.
///
/// With a copy and the helper, the launcher runs the helper and WAITS for it.
/// On recent macOS, quitting an app soon force-quits the processes it
/// started, so a helper left running in the background could be killed in
/// the middle of rebuilding the copy. When the helper is missing, the
/// launcher runs the plain `open` line, so the helper can never be why
/// Claude does not open.
pub fn build_launch_applescript(s: &LaunchSpec) -> String {
    let p = |x: &Path| x.to_string_lossy().to_string();
    let code = s.code_config_dir.filter(|d| !d.as_os_str().is_empty());
    let gh = s.gh_config_dir.filter(|d| !d.as_os_str().is_empty());
    let mut open_line = format!("open {}-a {} ", if s.dedicated { "" } else { "-n " }, shell_quote(&p(s.claude_app)));
    if let Some(c) = code {
        open_line += &format!("--env {} ", shell_quote(&format!("CLAUDE_CONFIG_DIR={}", p(c))));
    }
    if let Some(g) = gh {
        open_line += &format!("--env {} ", shell_quote(&format!("GH_CONFIG_DIR={}", p(g))));
    }
    open_line += &format!("--args --user-data-dir={} > /dev/null 2>&1 &", shell_quote(&p(s.data_dir)));

    let Some(helper) = s.helper.filter(|_| s.dedicated) else {
        return format!("do shell script {}", applescript_string(&open_line));
    };

    let mut args = vec![
        p(s.claude_app),
        p(&helper.source),
        helper.hue.clone(),
        p(s.data_dir),
        helper.label.clone(),
        "-a".into(),
        p(s.claude_app),
    ];
    if let Some(c) = code {
        args.push("--env".into());
        args.push(format!("CLAUDE_CONFIG_DIR={}", p(c)));
    }
    if let Some(g) = gh {
        args.push("--env".into());
        args.push(format!("GH_CONFIG_DIR={}", p(g)));
    }
    args.push("--args".into());
    args.push(format!("--user-data-dir={}", p(s.data_dir)));
    let quoted: Vec<String> = args.iter().map(|a| shell_quote(a)).collect();
    let hp = shell_quote(&p(&helper.path));
    let cmd = format!(
        "if [ -x {hp} ]; then {hp} __launch {} > /dev/null 2>&1; else {open_line} fi",
        quoted.join(" ")
    );
    format!("do shell script {}", applescript_string(&cmd))
}

// ---- Reading a launcher back ----------------------------------------------------------------
//
// The compiled script is the only record of what a launcher actually does, so
// doctor decompiles it rather than inferring from the profile's age.

pub fn read_launcher_script(app: &Path) -> Option<String> {
    let scpt = app.join("Contents").join("Resources").join("Scripts").join("main.scpt");
    if !file_exists(&scpt) {
        return None;
    }
    run_output("/usr/bin/osadecompile", [scpt.as_os_str()])
}

pub fn launcher_uses_helper(app: &Path) -> Option<bool> {
    read_launcher_script(app).map(|s| s.contains(HELPER_MARK))
}

/// Whether a helper-using launcher waits for the helper rather than starting
/// it in the background. None when the script cannot be read or uses none.
pub fn launcher_waits_for_helper(app: &Path) -> Option<bool> {
    let src = read_launcher_script(app)?;
    src.contains(HELPER_MARK).then(|| !src.contains("2>&1 & else "))
}

/// The value a launcher script exports for `name`. None when it exports none.
pub fn env_var_in_script(src: &str, name: &str) -> Option<String> {
    let start_pat = format!("--env '{name}=");
    let start = src.find(&start_pat)? + start_pat.len();
    let end = start + src[start..].find("' --")?;
    // Undo the two escaping layers, outermost first: the decompiled text is
    // AppleScript source, and inside it the value is shell single-quoted.
    let un_as = src[start..end].replace("\\\"", "\"").replace("\\\\", "\\");
    Some(un_as.replace("'\\''", "'"))
}

/// Outer None: the script could not be read. Inner None: no such variable.
pub fn launcher_env_var(app: &Path, name: &str) -> Option<Option<String>> {
    read_launcher_script(app).map(|s| env_var_in_script(&s, name))
}

pub fn launcher_code_config_dir(app: &Path) -> Option<Option<String>> {
    launcher_env_var(app, "CLAUDE_CONFIG_DIR")
}

pub fn launcher_gh_config_dir(app: &Path) -> Option<Option<String>> {
    launcher_env_var(app, "GH_CONFIG_DIR")
}

// ---- Signed-in account --------------------------------------------------------------------------

/// Which account a Desktop data folder is signed in as. Two profiles must
/// never be the same account; that happens when a claude:// sign-in link
/// reaches the wrong running instance. None when never signed in.
pub fn desktop_account_uuid(data_dir: &Path) -> Option<String> {
    read_json(&data_dir.join("config.json"))?
        .get("lastKnownAccountUuid")?
        .as_str()
        .filter(|s| !s.is_empty())
        .map(String::from)
}

// ---- Bundle details -------------------------------------------------------------------------------

/// A unique reverse-DNS identifier, so LaunchServices tells launchers apart.
pub fn unique_bundle_id(name: &str) -> String {
    let lower = name.to_lowercase();
    let mut safe = String::new();
    let mut in_bad = false;
    for c in lower.chars() {
        if c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' {
            safe.push(c);
            in_bad = false;
        } else if !in_bad {
            safe.push('-');
            in_bad = true;
        }
    }
    let safe = safe.trim_matches('-');
    format!("com.claude-profiles.{}", if safe.is_empty() { "profile" } else { safe })
}

fn info_plist(app: &Path) -> PathBuf {
    app.join("Contents").join("Info.plist")
}

pub fn plist_buddy(cmd: &str, plist: &Path) -> Option<String> {
    run_output(PLIST_BUDDY, [OsStr::new("-c"), OsStr::new(cmd), plist.as_os_str()]).map(|s| s.trim().to_string())
}

pub fn get_bundle_id(app: &Path) -> Option<String> {
    let plist = info_plist(app);
    if !file_exists(&plist) {
        return None;
    }
    plist_buddy("Print :CFBundleIdentifier", &plist)
}

pub fn set_bundle_id(app: &Path, id: &str) -> bool {
    let plist = info_plist(app);
    if !file_exists(&plist) {
        return false;
    }
    plist_buddy(&format!("Set :CFBundleIdentifier {id}"), &plist).is_some()
        || plist_buddy(&format!("Add :CFBundleIdentifier string {id}"), &plist).is_some()
}

/// The launcher's display name, as Raycast, Alfred and the app switcher show it.
pub fn get_bundle_name(app: &Path) -> Option<String> {
    let plist = info_plist(app);
    if !file_exists(&plist) {
        return None;
    }
    run_output("/usr/bin/plutil", [OsStr::new("-extract"), OsStr::new("CFBundleName"), OsStr::new("raw"), OsStr::new("-o"), OsStr::new("-"), plist.as_os_str()])
        .map(|s| s.trim().to_string())
}

pub fn set_bundle_name(app: &Path, name: &str) -> bool {
    let plist = info_plist(app);
    file_exists(&plist)
        && run_quiet("/usr/bin/plutil", [OsStr::new("-replace"), OsStr::new("CFBundleName"), OsStr::new("-string"), OsStr::new(name), plist.as_os_str()])
}

/// osacompile ships a compiled asset catalog holding the stock AppleScript
/// icon, and macOS prefers it over applet.icns, so the Dock showed a generic
/// tile while Finder showed Claude's. Removing it leaves applet.icns as the
/// only icon.
pub fn strip_asset_catalog(app: &Path) -> bool {
    let mut changed = false;
    let plist = info_plist(app);
    if file_exists(&plist) && plist_buddy("Delete :CFBundleIconName", &plist).is_some() {
        changed = true;
    }
    let car = app.join("Contents").join("Resources").join("Assets.car");
    if file_exists(&car) && std::fs::remove_file(&car).is_ok() {
        changed = true;
    }
    changed
}

pub fn has_asset_catalog(app: &Path) -> bool {
    file_exists(app.join("Contents").join("Resources").join("Assets.car"))
}

/// LSUIElement marks an app as a background agent, which macOS will not keep
/// pinned in the Dock. A launcher should never have it; this clears it.
pub fn set_ui_element(app: &Path, hidden: bool) -> bool {
    let plist = info_plist(app);
    if !file_exists(&plist) {
        return false;
    }
    let v = if hidden { "true" } else { "false" };
    plist_buddy(&format!("Set :LSUIElement {v}"), &plist).is_some()
        || plist_buddy(&format!("Add :LSUIElement bool {v}"), &plist).is_some()
}

pub fn is_ui_element(app: &Path) -> Option<bool> {
    let plist = info_plist(app);
    if !file_exists(&plist) {
        return None;
    }
    Some(plist_buddy("Print :LSUIElement", &plist).is_some_and(|s| s == "true" || s == "1"))
}

/// Clear a quarantine flag picked up through cloud sync, AirDrop or a backup,
/// which can make double-clicks fail while `open` still works.
pub fn strip_quarantine(app: &Path) -> bool {
    run_quiet("/usr/bin/xattr", [OsStr::new("-dr"), OsStr::new("com.apple.quarantine"), app.as_os_str()])
}

/// Whether a running Claude is using this data folder, through the profile's
/// copy or the shared app.
pub fn data_dir_in_use_in(ps: &str, data_dir: &Path) -> bool {
    let d = data_dir.to_string_lossy();
    ps.lines().any(|l| crate::launcher::on_profile(l, &d))
}

pub fn data_dir_in_use(data_dir: &Path) -> bool {
    data_dir_in_use_in(&ps_snapshot(), data_dir)
}

pub fn refresh_launcher(app: &Path) {
    run_quiet("/usr/bin/touch", [app.as_os_str()]);
    run_quiet(crate::util::LSREGISTER, [OsStr::new("-f"), app.as_os_str()]);
}

// ---- Building a launcher ------------------------------------------------------------------------------

/// Where a launcher should point. Decided from the copy actually on disk, so
/// every path that rebuilds a launcher agrees.
pub struct LaunchTarget {
    pub claude_app: PathBuf,
    pub dedicated: bool,
    pub source: Option<PathBuf>,
    pub color: Option<String>,
}

pub fn launch_target_for(name: &str, desktop: &crate::registry::DesktopTarget) -> LaunchTarget {
    let clone = clone_path_for(name);
    if file_exists(&clone) {
        LaunchTarget { claude_app: clone, dedicated: true, source: Some(desktop.claude_app_path.clone()), color: desktop.color.clone() }
    } else {
        LaunchTarget { claude_app: desktop.claude_app_path.clone(), dedicated: false, source: None, color: None }
    }
}

pub struct CompileSpec<'a> {
    pub name: &'a str,
    pub data_dir: &'a Path,
    pub app_path: &'a Path,
    pub target: &'a LaunchTarget,
    pub code_config_dir: Option<&'a Path>,
    pub gh_config_dir: Option<&'a Path>,
}

/// Compile (or recompile) a launcher.
///
/// An existing launcher keeps its bundle: only its compiled script is
/// swapped. The Dock remembers a pin by location, so replacing the bundle
/// would break the pin after every rename or repair.
pub fn compile_app(c: &CompileSpec) -> Result<()> {
    // A launcher written over the app it opens would delete that app and then
    // open itself. macOS disks ignore case, so compare that way.
    if c.app_path.to_string_lossy().to_lowercase() == c.target.claude_app.to_string_lossy().to_lowercase() {
        bail!("the launcher path {} is the same as the app it opens", c.app_path.display());
    }
    let helper = match (&c.target.source, c.target.dedicated) {
        (Some(source), true) => {
            install_helper().context("could not install the launch helper")?;
            Some(helper_spec_for(source, c.target.color.as_deref(), c.app_path))
        }
        _ => None,
    };
    let script = build_launch_applescript(&LaunchSpec {
        data_dir: c.data_dir,
        claude_app: &c.target.claude_app,
        code_config_dir: c.code_config_dir,
        gh_config_dir: c.gh_config_dir,
        dedicated: c.target.dedicated,
        helper: helper.as_ref(),
    });

    let tmp = tempfile::Builder::new().prefix("cmp-").tempdir()?;
    let script_path = tmp.path().join("launcher.applescript");
    std::fs::write(&script_path, script)?;
    if let Some(parent) = c.app_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // Compiled under the launcher's own name: osacompile names the bundle
    // after its output path, and that name is what the app switcher shows.
    let file_name = c.app_path.file_name().context("launcher path has no file name")?;
    let staged = tmp.path().join(file_name);
    run_checked("/usr/bin/osacompile", [OsStr::new("-o"), staged.as_os_str(), script_path.as_os_str()])?;

    let rel = Path::new("Contents").join("Resources").join("Scripts").join("main.scpt");
    if file_exists(c.app_path) && file_exists(c.app_path.join(&rel)) {
        std::fs::copy(staged.join(&rel), c.app_path.join(&rel))?;
    } else {
        remove_all(c.app_path)?;
        if std::fs::rename(&staged, c.app_path).is_err() {
            // A different volume: copy instead.
            crate::util::clone_copy(&staged, c.app_path)?;
        }
    }
    set_bundle_id(c.app_path, &unique_bundle_id(c.name));
    strip_asset_catalog(c.app_path);
    Ok(())
}

/// Put Claude's own icon on a launcher.
pub fn copy_claude_icon(app: &Path, claude_app: &Path) -> bool {
    let Some(icns) = find_app_icns(claude_app) else { return false };
    let target = app.join("Contents").join("Resources").join("applet.icns");
    if !file_exists(&target) || std::fs::copy(&icns, &target).is_err() {
        return false;
    }
    // Without a touch, the generic icon can linger in Finder's cache.
    run_quiet("/usr/bin/touch", [app.as_os_str()]);
    true
}

// ---- Setup ----------------------------------------------------------------------------------------------

pub struct DesktopSetup {
    pub name: String,
    pub data_dir: PathBuf,
    pub app_path: PathBuf,
    pub claude_app: PathBuf,
    pub apply_icon: bool,
    pub color: Option<String>,
    pub seed_connectors: bool,
}

pub struct DesktopResult {
    pub data_dir: PathBuf,
    pub app_path: PathBuf,
    pub claude_app: PathBuf,
    pub color: Option<String>,
}

pub fn setup_desktop(d: &DesktopSetup, code_config_dir: Option<&Path>, gh_config_dir: Option<&Path>) -> Result<DesktopResult> {
    step(format!("Creating Claude Desktop profile \"{}\"", d.name));
    info(format!("Data folder: {}", path_str(tildify(&d.data_dir))));
    info(format!("Launcher app: {}", path_str(tildify(&d.app_path))));
    info(format!("Claude.app source: {}", path_str(d.claude_app.display().to_string())));
    if let Some(c) = code_config_dir {
        info(format!("Code config for Desktop-spawned Claude Code: {}", path_str(tildify(c))));
    }

    // Never seeded from the default profile: that would bring its sign-in
    // along, and the profile would open on the wrong account.
    std::fs::create_dir_all(&d.data_dir)?;
    ok("Data folder ready.");

    // A choice, made in the wizard: a connector's settings often hold the
    // default account's credentials.
    if d.seed_connectors {
        let r = seed_desktop_mcp_servers(&d.data_dir, &default_desktop_config_file(), None);
        match r.status {
            SeedStatus::Seeded => ok(format!(
                "MCP connectors copied from your default profile: {} added, {} in total.",
                r.added.len(),
                r.total
            )),
            SeedStatus::Current => ok(format!("MCP connectors already in place ({}).", r.total)),
            SeedStatus::UnsafePath => {
                warn("This data folder's claude_desktop_config.json resolves outside the profile or has a broken link; left it alone.");
                info(format!("  Its connectors stay as they are. {} re-checks this.", command("claude-profiles doctor")));
            }
            SeedStatus::Foreign => {
                warn("This data folder's claude_desktop_config.json could not be parsed; left it alone.");
                info(format!("  Its connectors stay as they are. {} re-checks this.", command("claude-profiles doctor")));
            }
            SeedStatus::None => {}
        }
    }

    // Every Desktop profile gets its own copy of Claude.app, coloured or not.
    let mut color = d.color.clone();
    let clone = match ensure_colored_clone(&d.name, &d.claude_app, color.as_deref(), false) {
        Ok(p) => {
            ok(match &color {
                Some(c) => format!("Claude clone ready ({c})."),
                None => "Claude clone ready for this profile.".into(),
            });
            Some(p)
        }
        Err(e) => {
            warn(format!("Could not build this profile's Claude clone: {e}"));
            warn("Falling back to the shared Claude.app. Each click will open a new window.");
            color = None;
            None
        }
    };

    let target = match &clone {
        Some(p) => LaunchTarget { claude_app: p.clone(), dedicated: true, source: Some(d.claude_app.clone()), color: color.clone() },
        None => LaunchTarget { claude_app: d.claude_app.clone(), dedicated: false, source: None, color: None },
    };
    compile_app(&CompileSpec {
        name: &d.name,
        data_dir: &d.data_dir,
        app_path: &d.app_path,
        target: &target,
        code_config_dir,
        gh_config_dir,
    })?;
    ok("Launcher .app compiled.");

    // The copy does not update itself; its launcher keeps it current.
    if clone.is_some() {
        match block_updates(&d.data_dir) {
            Ok(BlockResult::Foreign) => {
                warn("This data folder already has its own Claude configuration; left it alone.");
                info("  The copy may update itself. `claude-profiles doctor` explains what that means.");
            }
            Ok(_) => ok("Updates managed by the launcher: the copy is refreshed when your main Claude updates."),
            Err(e) => warn(format!("Could not set up managed updates: {e}")),
        }
    }

    if d.apply_icon {
        if copy_claude_icon(&d.app_path, &d.claude_app) {
            ok("Claude icon applied to launcher.");
        } else {
            warn("Could not locate a Claude icon to copy. Default AppleScript icon left in place.");
        }
        // Tint the launcher too, so the icon you pin matches the window it opens.
        if let (Some(c), Some(_)) = (&color, &clone) {
            if apply_color(&d.app_path, &d.claude_app, c).is_ok() {
                ok("Launcher tinted to match.");
            }
        }
    }

    Ok(DesktopResult { data_dir: d.data_dir.clone(), app_path: d.app_path.clone(), claude_app: d.claude_app.clone(), color })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec<'a>(data: &'a Path, app: &'a Path, code: Option<&'a Path>, gh: Option<&'a Path>) -> LaunchSpec<'a> {
        LaunchSpec { data_dir: data, claude_app: app, code_config_dir: code, gh_config_dir: gh, dedicated: false, helper: None }
    }

    #[test]
    fn shared_app_gets_n_and_env_before_args() {
        let s = build_launch_applescript(&spec(Path::new("/U/Claude-W"), Path::new("/Applications/Claude.app"), Some(Path::new("/U/.claude-w")), None));
        assert_eq!(
            s,
            r#"do shell script "open -n -a '/Applications/Claude.app' --env 'CLAUDE_CONFIG_DIR=/U/.claude-w' --args --user-data-dir='/U/Claude-W' > /dev/null 2>&1 &""#
        );
        let none = build_launch_applescript(&spec(Path::new("/d"), Path::new("/a.app"), None, None));
        assert!(!none.contains("--env"));
        let empty = build_launch_applescript(&spec(Path::new("/d"), Path::new("/a.app"), Some(Path::new("")), None));
        assert!(!empty.contains("--env"), "an empty folder counts as none");
        let both = build_launch_applescript(&spec(Path::new("/d"), Path::new("/a.app"), Some(Path::new("/c")), Some(Path::new("/c/gh"))));
        assert!(both.contains("--env 'CLAUDE_CONFIG_DIR=/c' --env 'GH_CONFIG_DIR=/c/gh' --args"));
    }

    #[test]
    fn apostrophes_survive_both_quoting_layers() {
        let s = build_launch_applescript(&spec(Path::new("/Users/o'brien/Claude-W"), Path::new("/a.app"), Some(Path::new("/Users/o'brien/.claude-\"w\"")), None));
        assert!(s.contains(r#"--user-data-dir='/Users/o'\\''brien/Claude-W'"#));
        assert_eq!(env_var_in_script(&s, "CLAUDE_CONFIG_DIR").as_deref(), Some("/Users/o'brien/.claude-\"w\""));
        assert_eq!(env_var_in_script(&s, "GH_CONFIG_DIR"), None);
    }

    #[test]
    fn a_dedicated_copy_goes_through_the_helper_and_waits_for_it() {
        let helper = HelperSpec {
            path: PathBuf::from("/U/Library/Application Support/claude-profiles/bin/claude-profiles"),
            source: PathBuf::from("/Applications/Claude.app"),
            hue: "210".into(),
            label: "Claude W".into(),
        };
        let clone = PathBuf::from("/U/apps/Claude w.app");
        let s = build_launch_applescript(&LaunchSpec {
            data_dir: Path::new("/U/Claude-W"),
            claude_app: &clone,
            code_config_dir: Some(Path::new("/U/.claude-w")),
            gh_config_dir: None,
            dedicated: true,
            helper: Some(&helper),
        });
        assert!(s.contains(HELPER_MARK));
        assert!(s.contains("__launch '/U/apps/Claude w.app' '/Applications/Claude.app' '210' '/U/Claude-W' 'Claude W' '-a'"));
        assert!(s.contains("> /dev/null 2>&1; else open -a"), "waits, with a plain open as the fallback");
        assert!(!s.contains("open -n"), "a dedicated copy is focused, not duplicated");
        assert!(!s.contains("2>&1 & else "));
        assert_eq!(env_var_in_script(&s, "CLAUDE_CONFIG_DIR").as_deref(), Some("/U/.claude-w"));
    }

    #[test]
    fn data_dir_in_use_is_exact() {
        let ps = "1 /x/Claude --user-data-dir=/U/Claude-WORK2\n2 /x/Claude --user-data-dir=/U/Claude-A --flag\n";
        assert!(!data_dir_in_use_in(ps, Path::new("/U/Claude-WORK")));
        assert!(data_dir_in_use_in(ps, Path::new("/U/Claude-WORK2")));
        assert!(data_dir_in_use_in(ps, Path::new("/U/Claude-A")));
    }

    #[test]
    fn bundle_ids_are_sanitised() {
        assert_eq!(unique_bundle_id("work"), "com.claude-profiles.work");
        assert_eq!(unique_bundle_id("Client ACME!"), "com.claude-profiles.client-acme");
        assert_eq!(unique_bundle_id("!!!"), "com.claude-profiles.profile");
    }

    #[test]
    fn account_uuid_reads_and_treats_absence_as_none() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(desktop_account_uuid(dir.path()), None);
        std::fs::write(dir.path().join("config.json"), r#"{"lastKnownAccountUuid":"abc-123"}"#).unwrap();
        assert_eq!(desktop_account_uuid(dir.path()).as_deref(), Some("abc-123"));
        std::fs::write(dir.path().join("config.json"), "{ broken").unwrap();
        assert_eq!(desktop_account_uuid(dir.path()), None);
    }

    fn write(p: &Path, s: &str) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, s).unwrap();
    }

    #[test]
    fn connectors_are_added_never_replaced_and_preferences_survive() {
        let root = tempfile::tempdir().unwrap();
        let src = root.path().join("default/claude_desktop_config.json");
        write(&src, r#"{"mcpServers":{"github":{"env":{"T":"personal"}},"fs":{}}}"#);
        let data = root.path().join("Claude-W");
        write(&desktop_config_file(&data), r#"{"preferences":{"x":1},"mcpServers":{"github":{"env":{"T":"work"}}}}"#);
        let r = seed_desktop_mcp_servers(&data, &src, None);
        assert_eq!(r.status, SeedStatus::Seeded);
        assert_eq!(r.added, vec!["fs"]);
        assert_eq!(r.total, 2);
        let v = read_json(&desktop_config_file(&data)).unwrap();
        assert_eq!(v["mcpServers"]["github"]["env"]["T"], "work");
        assert_eq!(v["preferences"]["x"], 1);
        assert_eq!(seed_desktop_mcp_servers(&data, &src, None).status, SeedStatus::Current);
        assert_eq!(seed_desktop_mcp_servers(&data, &root.path().join("none.json"), None).status, SeedStatus::None);
    }

    #[test]
    fn connectors_only_and_unparseable_and_unsafe() {
        let root = tempfile::tempdir().unwrap();
        let src = root.path().join("src.json");
        write(&src, r#"{"mcpServers":{"a":{},"b":{},"c":{}}}"#);
        let fresh = root.path().join("Fresh");
        std::fs::create_dir_all(&fresh).unwrap();
        let r = seed_desktop_mcp_servers(&fresh, &src, Some(&["b".to_string()]));
        assert_eq!(r.added, vec!["b"]);

        let broken = root.path().join("Broken");
        write(&desktop_config_file(&broken), "{ nope");
        assert_eq!(seed_desktop_mcp_servers(&broken, &src, None).status, SeedStatus::Foreign);
        assert_eq!(std::fs::read_to_string(desktop_config_file(&broken)).unwrap(), "{ nope");

        let linked = root.path().join("Linked");
        std::fs::create_dir_all(&linked).unwrap();
        let outside = root.path().join("outside.json");
        write(&outside, "{}");
        std::os::unix::fs::symlink(&outside, desktop_config_file(&linked)).unwrap();
        assert_eq!(seed_desktop_mcp_servers(&linked, &src, None).status, SeedStatus::UnsafePath);
        assert_eq!(std::fs::read_to_string(&outside).unwrap(), "{}");
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn compiled_launchers_read_back_keep_their_bundle_and_their_name() {
        let root = tempfile::tempdir().unwrap();
        let app = root.path().join("Claude T.app");
        let target = LaunchTarget { claude_app: PathBuf::from("/Applications/Claude.app"), dedicated: false, source: None, color: None };
        let spec = |code: &'static str| CompileSpec {
            name: "t",
            data_dir: Path::new("/tmp/Claude-T"),
            app_path: &app,
            target: &target,
            code_config_dir: Some(Path::new(code)),
            gh_config_dir: None,
        };
        compile_app(&spec("/tmp/o'brien/.claude-t")).unwrap();
        assert_eq!(launcher_code_config_dir(&app), Some(Some("/tmp/o'brien/.claude-t".into())));
        assert_eq!(get_bundle_id(&app).as_deref(), Some("com.claude-profiles.t"));
        assert_eq!(get_bundle_name(&app).as_deref(), Some("Claude T"));
        assert!(!has_asset_catalog(&app));
        assert_eq!(launcher_uses_helper(&app), Some(false));

        // Rebuilding swaps the script inside the same bundle.
        std::fs::write(app.join("Contents/marker"), "x").unwrap();
        compile_app(&spec("/tmp/.claude-t2")).unwrap();
        assert!(app.join("Contents/marker").exists());
        assert_eq!(launcher_code_config_dir(&app), Some(Some("/tmp/.claude-t2".into())));

        assert!(set_bundle_name(&app, "Claude T Renamed"));
        assert_eq!(get_bundle_name(&app).as_deref(), Some("Claude T Renamed"));
        assert!(set_ui_element(&app, true));
        assert_eq!(is_ui_element(&app), Some(true));
        assert!(set_ui_element(&app, false));
        assert_eq!(is_ui_element(&app), Some(false));
    }

    #[test]
    fn refuses_a_launcher_that_is_the_app_it_opens() {
        let target = LaunchTarget { claude_app: PathBuf::from("/Applications/Claude.app"), dedicated: false, source: None, color: None };
        let r = compile_app(&CompileSpec {
            name: "x",
            data_dir: Path::new("/d"),
            app_path: Path::new("/applications/claude.APP"),
            target: &target,
            code_config_dir: None,
            gh_config_dir: None,
        });
        assert!(r.unwrap_err().to_string().contains("same as the app it opens"));
    }
}
