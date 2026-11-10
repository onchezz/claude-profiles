// Each Desktop profile's own copy of Claude.app.
//
// The launcher starts Claude and exits, so the running window belongs to
// Claude's process, not the launcher. A per-profile COPY of Claude.app gives
// each profile a process of its own: clicking the launcher can focus that
// profile's window instead of opening another, and the copy can carry a
// colour on its Dock tile.
//
// Cheap and safe:
//   - `cp -Rc` makes an APFS clone that shares blocks with the original.
//   - The colour is Finder metadata (an `Icon\r` file plus the custom-icon
//     flag), not an edit inside the bundle, so Anthropic's signature and
//     identity survive and Gatekeeper accepts it. `codesign --verify --deep
//     --strict` does fail on it, as on any app with a custom icon.
//
// The copy is a real Claude, so Claude's updater runs inside it unless it is
// blocked (see updates.rs). A copy can therefore be AHEAD of the installed
// Claude; rebuilding it then would be a downgrade, so versions are compared
// numerically and only a copy that is behind is rebuilt.

use std::cmp::Ordering;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use anyhow::{bail, Result};

use crate::util::{clone_copy, compare_versions, file_exists, home, remove_all, run_output};

pub fn tool_support_dir() -> PathBuf {
    home().join("Library").join("Application Support").join("claude-profiles")
}

/// Outside ~/Applications, so they do not clutter the app list; the launcher
/// is the thing meant to be seen.
pub fn clone_parent() -> PathBuf {
    tool_support_dir().join("apps")
}

pub fn clone_path_for(profile_name: &str) -> PathBuf {
    clone_parent().join(format!("Claude {profile_name}.app"))
}

/// Hue rotations, in degrees, applied to Claude's own icon.
pub const COLORS: &[(&str, u32)] = &[
    ("orange", 25),
    ("red", 330),
    ("yellow", 55),
    ("green", 110),
    ("teal", 160),
    ("blue", 210),
    ("purple", 265),
    ("pink", 300),
];

pub fn hue_for(color: &str) -> Option<u32> {
    COLORS.iter().find(|(c, _)| *c == color).map(|(_, h)| *h)
}

// ---- Reading a bundle --------------------------------------------------------------

pub fn plist_value(app: &Path, key: &str) -> Option<String> {
    let plist = app.join("Contents").join("Info.plist");
    if !file_exists(&plist) {
        return None;
    }
    run_output("/usr/bin/plutil", [OsStr::new("-extract"), OsStr::new(key), OsStr::new("raw"), OsStr::new("-o"), OsStr::new("-"), plist.as_os_str()])
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

pub fn app_version(app: &Path) -> Option<String> {
    plist_value(app, "CFBundleShortVersionString")
}

/// Claude ships one .icns in Resources; its name has changed across releases.
pub fn find_app_icns(app: &Path) -> Option<PathBuf> {
    let res = app.join("Contents").join("Resources");
    let mut names: Vec<_> = std::fs::read_dir(&res).ok()?.flatten().map(|e| e.file_name()).collect();
    names.sort();
    names
        .into_iter()
        .find(|n| n.to_string_lossy().to_lowercase().ends_with(".icns"))
        .map(|n| res.join(n))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloneState {
    Missing,
    Current,
    /// The installed Claude is newer: rebuilding is an upgrade.
    Behind,
    /// The copy updated itself past the installed Claude: keep it.
    Ahead,
    /// A version could not be read: leave it alone.
    Unknown,
}

pub fn compare_clone_versions(clone: Option<&str>, source: Option<&str>) -> CloneState {
    match (clone, source) {
        (Some(c), Some(s)) => match compare_versions(c, s) {
            Ordering::Equal => CloneState::Current,
            Ordering::Less => CloneState::Behind,
            Ordering::Greater => CloneState::Ahead,
        },
        _ => CloneState::Unknown,
    }
}

pub fn clone_state(clone: &Path, source: &Path) -> CloneState {
    if !file_exists(clone) {
        return CloneState::Missing;
    }
    compare_clone_versions(app_version(clone).as_deref(), app_version(source).as_deref())
}

pub fn clone_is_stale(clone: &Path, source: &Path) -> bool {
    matches!(clone_state(clone, source), CloneState::Missing | CloneState::Behind)
}

// ---- Is a copy open? ------------------------------------------------------------------
//
// Rebuilding deletes the copy, and deleting an app while it runs can crash it;
// rename moves the data folder a running Claude would keep writing to. So
// anything destructive checks first.

pub fn ps_snapshot() -> String {
    // -ww: never truncate, or the --user-data-dir at the end could be cut off.
    run_output("/bin/ps", ["-axww", "-o", "pid=,command="]).unwrap_or_default()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Running {
    pub pid: i32,
    pub command: String,
}

/// Main processes of a copy. Its helper processes live under
/// Contents/Frameworks, never Contents/MacOS, so the needle excludes them
/// without guessing from names (a profile may well be called "Helper").
pub fn parse_running_copies(ps: &str, copy: &Path) -> Vec<Running> {
    let needle = format!("{}/Contents/MacOS/", copy.display());
    ps.lines()
        .filter(|l| l.contains(&needle))
        .filter_map(|l| {
            let t = l.trim();
            let end = t.find(char::is_whitespace)?;
            let pid = t[..end].parse().ok()?;
            Some(Running { pid, command: t[end..].trim().to_string() })
        })
        .collect()
}

pub fn running_copies(copy: &Path) -> Vec<Running> {
    parse_running_copies(&ps_snapshot(), copy)
}

pub fn clone_is_running(copy: &Path) -> bool {
    !running_copies(copy).is_empty()
}

// ---- Building and colouring ---------------------------------------------------------

/// Create, or refresh when stale (or with `force`), a profile's copy, and
/// apply its colour. Never rebuilds a copy that is open.
pub fn ensure_colored_clone(name: &str, claude_app: &Path, color: Option<&str>, force: bool) -> Result<PathBuf> {
    if let Some(c) = color {
        if hue_for(c).is_none() {
            bail!("Unknown colour: {c}");
        }
    }
    if !file_exists(claude_app) {
        bail!("Claude.app not found at {}", claude_app.display());
    }
    let clone = clone_path_for(name);
    let rebuild = (force || clone_is_stale(&clone, claude_app)) && !clone_is_running(&clone);
    if rebuild {
        std::fs::create_dir_all(clone_parent())?;
        remove_all(&clone)?;
        clone_copy(claude_app, &clone)?;
    }
    // From the copy's own icon: a copy that updated itself can be newer than
    // the installed Claude, and its icon is the one that belongs on its tile.
    if let Some(c) = color {
        apply_color(&clone, &clone, c)?;
    }
    Ok(clone)
}

/// Did the colour survive? Finder needs both an `Icon\r` file in the bundle
/// and the kHasCustomIcon bit (0x04 in byte 8 of FinderInfo); an update swaps
/// the whole folder and drops both.
pub fn finder_info_has_custom_icon(hex_dump: &str) -> bool {
    let bytes: Vec<&str> = hex_dump.split_whitespace().collect();
    if bytes.len() < 10 {
        return false;
    }
    u8::from_str_radix(bytes[8], 16).map(|b| b & 0x04 != 0).unwrap_or(false)
}

pub fn has_custom_icon(app: &Path) -> bool {
    if !file_exists(app.join("Icon\r")) {
        return false;
    }
    run_output("/usr/bin/xattr", [OsStr::new("-px"), OsStr::new("com.apple.FinderInfo"), app.as_os_str()])
        .is_some_and(|h| finder_info_has_custom_icon(&h))
}

/// Tints an .icns with Core Image's hue filter and sets it as a file's custom
/// icon. JavaScript for Automation, because that is how to reach Core Image
/// and NSWorkspace without a compiler toolchain. The script is fixed; paths
/// and the angle arrive as arguments, never as script text.
const TINT_JXA: &str = r#"
ObjC.import('AppKit');
ObjC.import('CoreImage');
function run(argv) {
  var icns = argv[0], deg = parseFloat(argv[1]), target = argv[2];
  var data = $.NSData.dataWithContentsOfFile($(icns));
  if (data.isNil()) return 'ERR no-data';
  var ci = $.CIImage.imageWithData(data);
  if (ci.isNil()) return 'ERR not-an-image';
  var f = $.CIFilter.filterWithName($('CIHueAdjust'));
  f.setDefaults;
  f.setValueForKey(ci, $('inputImage'));
  f.setValueForKey($.NSNumber.numberWithDouble(deg * Math.PI / 180), $('inputAngle'));
  var rep = $.NSCIImageRep.imageRepWithCIImage(f.valueForKey($('outputImage')));
  var img = $.NSImage.alloc.initWithSize(rep.size);
  img.addRepresentation(rep);
  var bmp = $.NSBitmapImageRep.imageRepWithData(img.TIFFRepresentation);
  var out = $.NSImage.alloc.initWithSize(bmp.size);
  out.addRepresentation(bmp);
  return $.NSWorkspace.sharedWorkspace.setIconForFileOptions(out, $(target), 0) ? 'OK' : 'ERR set-failed';
}
"#;

/// Tint `icon_source`'s icon by `hue` degrees and put it on `target`.
pub fn tint(icon_source: &Path, hue: u32, target: &Path) -> Result<()> {
    let Some(icns) = find_app_icns(icon_source) else { bail!("No .icns found inside {}", icon_source.display()) };
    let out = run_output(
        "/usr/bin/osascript",
        [
            OsStr::new("-l"),
            OsStr::new("JavaScript"),
            OsStr::new("-e"),
            OsStr::new(TINT_JXA),
            icns.as_os_str(),
            OsStr::new(&hue.to_string()),
            target.as_os_str(),
        ],
    )
    .unwrap_or_else(|| "ERR osascript-failed".into());
    if out.trim() != "OK" {
        bail!("Could not tint the icon: {}", out.trim());
    }
    Ok(())
}

pub fn apply_color(target: &Path, icon_source: &Path, color: &str) -> Result<()> {
    let Some(hue) = hue_for(color) else { bail!("Unknown colour: {color}") };
    tint(icon_source, hue, target)
}

pub fn remove_clone(name: &str) -> Result<bool> {
    let p = clone_path_for(name);
    if !file_exists(&p) {
        return Ok(false);
    }
    remove_all(&p)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colour_table_and_clone_paths() {
        assert_eq!(hue_for("blue"), Some(210));
        assert_eq!(hue_for("magenta"), None);
        assert!(clone_path_for("work").ends_with("claude-profiles/apps/Claude work.app"));
    }

    #[test]
    fn a_copy_ahead_of_its_source_is_not_stale() {
        assert_eq!(compare_clone_versions(Some("2.9.0"), Some("2.10.0")), CloneState::Behind);
        assert_eq!(compare_clone_versions(Some("2.10.0"), Some("2.10.0")), CloneState::Current);
        assert_eq!(compare_clone_versions(Some("2.11.0"), Some("2.10.0")), CloneState::Ahead);
        assert_eq!(compare_clone_versions(None, Some("1")), CloneState::Unknown);
    }

    #[test]
    fn a_missing_clone_counts_as_stale() {
        let dir = tempfile::tempdir().unwrap();
        assert!(clone_is_stale(&dir.path().join("nope.app"), Path::new("/Applications/Claude.app")));
    }

    #[test]
    fn running_copies_are_found_by_their_main_process() {
        let copy = Path::new("/U/apps/Claude w.app");
        let ps = "  101 /U/apps/Claude w.app/Contents/MacOS/Claude\n\
                  102 /U/apps/Claude w.app/Contents/MacOS/Claude --user-data-dir=/U/Claude-W\n\
                  103 /U/apps/Claude w.app/Contents/Frameworks/Claude Helper.app/Contents/MacOS/Claude Helper\n\
                  104 /U/apps/Claude work.app/Contents/MacOS/Claude\n";
        let r = parse_running_copies(ps, copy);
        assert_eq!(r.len(), 2);
        assert_eq!(r[0], Running { pid: 101, command: "/U/apps/Claude w.app/Contents/MacOS/Claude".into() });
        assert!(r[1].command.ends_with("--user-data-dir=/U/Claude-W"));
        let helper = Path::new("/U/apps/Claude Helper.app");
        assert_eq!(parse_running_copies("9 /U/apps/Claude Helper.app/Contents/MacOS/Claude\n", helper).len(), 1);
    }

    #[test]
    fn finder_info_custom_icon_bit() {
        let with = "00 00 00 00 00 00 00 00 04 00 00 00 00 00 00 00\n00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00";
        let without = "00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00";
        assert!(finder_info_has_custom_icon(with));
        assert!(!finder_info_has_custom_icon(without));
        assert!(!finder_info_has_custom_icon(""));
    }

    #[test]
    fn no_icon_file_means_no_colour() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!has_custom_icon(dir.path()));
    }
}
