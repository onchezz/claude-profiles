// Cleaning up disk space, safely: `claude-profiles app cleanup-scan` and
// `app cleanup-run PATH...`.
//
// The rules, in layers, so a warning is never the only thing standing
// between a click and lost data:
//
//   1. Only regenerable things are ever listed: caches, logs, build output,
//      package-manager downloads, old copies of tools. Documents, mail,
//      photos, cloud folders, keychains, browser profiles, model stores and
//      conversation histories are never candidates.
//   2. Anything in use (an open app's cache, a running Claude Code version,
//      a project mid-build) is listed as in use and cannot be selected.
//   3. Things worth a look before going (old downloads, project build
//      folders) are "review" items and start unticked.
//   4. A run only accepts paths the last scan offered, and checks each again
//      at that moment: still there, not a link, still inside its area, not
//      in use now.
//   5. Everything goes to the Trash (Recycle Bin on Windows), one item at a
//      time, and the reply says what was freed, skipped and failed.
//   6. Where a tool has its own cleanup (Homebrew, Xcode simulators, Docker),
//      that is offered instead, run in a terminal where you see it.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::{bail, Result};
use serde_json::{json, Value};

use super::app::{processes, Proc};
use crate::claudebin::{default_claude_config_dir, default_desktop_data_dir};
use crate::registry;
use crate::util::{compare_versions, home, is_mac, path_string, read_json, run_output, to_json_pretty, write_private_file};

const MB: u64 = 1024 * 1024;

// ---- What a candidate is ---------------------------------------------------------------

struct Item {
    group: &'static str,
    label: String,
    path: PathBuf,
    note: String,
    /// Regenerable without a second thought: starts ticked.
    safe: bool,
    in_use: bool,
    bytes: u64,
    days_old: Option<u64>,
}

impl Item {
    fn new(group: &'static str, label: impl Into<String>, path: PathBuf, note: impl Into<String>, safe: bool) -> Item {
        Item { group, label: label.into(), path, note: note.into(), safe, in_use: false, bytes: 0, days_old: None }
    }
}

// ---- Never touched ---------------------------------------------------------------------

/// Folder names that hold data people cannot get back, wherever they appear:
/// cloud mirrors, model stores, credentials, personal libraries.
const NEVER: &[&str] = &[
    "CloudStorage",
    "Mobile Documents",
    "Keychains",
    "Containers",
    "Group Containers",
    "Mail",
    "Messages",
    "Photos Library.photoslibrary",
    "huggingface",
    "ollama",
    ".ollama",
    "lm-studio",
    ".lmstudio",
    "torch",
    "whisper",
];

fn never(p: &Path) -> bool {
    p.components().any(|c| NEVER.contains(&c.as_os_str().to_string_lossy().as_ref()))
}

/// What a candidate's path must stay inside, so a hand-made run can never
/// reach elsewhere.
fn allowed_roots() -> Vec<PathBuf> {
    let h = home();
    let mut roots = vec![h.join("Library/Caches"), h.join("Library/Logs"), h.join("Library/Developer"), h.join("Library/Application Support")];
    roots.extend([
        h.join(".cache"),
        h.join(".npm"),
        h.join(".pnpm-store"),
        h.join(".yarn"),
        h.join(".bun"),
        h.join(".pub-cache"),
        h.join(".dartServer"),
        h.join(".android/avd"),
        h.join("Library/Android/sdk/system-images"),
        h.join(".gradle"),
        h.join(".cargo"),
        h.join(".local/share/claude"),
    ]);
    roots.extend(project_roots());
    roots.push(h.join("Downloads"));
    if let Some(l) = std::env::var_os("LOCALAPPDATA") {
        roots.push(PathBuf::from(l));
    }
    for p in claude_config_dirs() {
        roots.push(p);
    }
    roots
}

// ---- Sizes and ages ------------------------------------------------------------------------

fn size_of(p: &Path) -> u64 {
    if is_mac() || cfg!(target_os = "linux") {
        // du is far faster than walking in Rust on big trees.
        if let Some(out) = run_output("du", [std::ffi::OsStr::new("-sk"), p.as_os_str()]) {
            if let Some(kb) = out.split_whitespace().next().and_then(|n| n.parse::<u64>().ok()) {
                return kb * 1024;
            }
        }
    }
    tree_bytes(p)
}

fn tree_bytes(p: &Path) -> u64 {
    let Ok(m) = std::fs::symlink_metadata(p) else { return 0 };
    if !m.is_dir() {
        return m.len();
    }
    std::fs::read_dir(p).map(|it| it.flatten().map(|e| tree_bytes(&e.path())).sum()).unwrap_or(0)
}

fn days_since_modified(p: &Path) -> Option<u64> {
    let m = std::fs::metadata(p).ok()?.modified().ok()?;
    Some(SystemTime::now().duration_since(m).ok()?.as_secs() / 86_400)
}

/// Sizes for every item, eight at a time.
fn measure(items: &mut [Item]) {
    for chunk in items.chunks_mut(8) {
        std::thread::scope(|s| {
            for it in chunk.iter_mut() {
                s.spawn(move || it.bytes = size_of(&it.path));
            }
        });
    }
}

fn subdirs(p: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = std::fs::read_dir(p)
        .map(|it| it.flatten().filter(|e| e.file_type().is_ok_and(|t| t.is_dir())).map(|e| e.path()).collect())
        .unwrap_or_default();
    out.sort();
    out
}

fn name_of(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()
}

// ---- What is running ---------------------------------------------------------------------

struct Running {
    procs: Vec<Proc>,
    /// Bundle IDs of open apps (macOS), lowercased.
    bundles: Vec<String>,
}

impl Running {
    fn now() -> Running {
        let bundles = if is_mac() {
            run_output("/usr/bin/lsappinfo", ["list"])
                .unwrap_or_default()
                .lines()
                .filter_map(|l| l.split("bundleID=\"").nth(1).and_then(|r| r.split('"').next()))
                .map(|b| b.to_lowercase())
                .collect()
        } else {
            Vec::new()
        };
        // Not this process or what started it: their command lines name the
        // very paths being cleaned, which is no sign anything uses them.
        let mut procs = processes();
        let mut mine = HashSet::new();
        let mut pid = std::process::id() as i32;
        for _ in 0..16 {
            if !mine.insert(pid) {
                break;
            }
            match procs.iter().find(|p| p.pid == pid) {
                Some(p) => pid = p.ppid,
                None => break,
            }
        }
        procs.retain(|p| !mine.contains(&p.pid));
        Running { procs, bundles }
    }

    /// A process that runs from, or names, this path.
    fn uses(&self, p: &Path) -> bool {
        let s = path_string(p);
        self.procs.iter().any(|pr| pr.command.contains(&s))
    }

    /// An open app owns this cache folder: named for its bundle ID
    /// ("com.spotify.client") or its vendor ("Google" for Chrome), or for
    /// the program itself.
    fn owns_cache(&self, folder: &str) -> bool {
        let f = folder.to_lowercase();
        if f.len() < 3 {
            return false;
        }
        // Words in a bundle ID that name nothing in particular.
        const GENERIC: &[&str] = &["com", "org", "net", "app", "apps", "desktop", "client", "helper", "browser", "mac", "macos"];
        let by_bundle = self.bundles.iter().any(|b| {
            *b == f
                || b.starts_with(&format!("{f}."))
                // A vendor folder ("BraveSoftware") for an app's bundle
                // ("com.brave.Browser"). Matching loosely only ever leaves
                // more caches alone.
                || b.split('.').any(|part| part == f || (part.len() >= 4 && !GENERIC.contains(&part) && f.contains(part)))
        });
        let by_program = self.procs.iter().any(|pr| {
            let comm = pr.comm.to_lowercase();
            comm == f || comm.trim_end_matches(".exe") == f
        });
        by_bundle || by_program
    }

    fn desktop_open(&self, data_dir: &Path) -> bool {
        if *data_dir == default_desktop_data_dir() {
            // The default Claude runs without --user-data-dir.
            return self.procs.iter().any(|p| p.command.contains("/Claude.app/Contents/MacOS/Claude") && !p.command.contains("--user-data-dir"));
        }
        self.uses(data_dir)
    }
}

// ---- Claude --------------------------------------------------------------------------------

/// Chromium caches Claude Desktop rebuilds on its next start.
const DESKTOP_CACHES: &[&str] = &["Cache", "Code Cache", "GPUCache", "DawnGraphiteCache", "DawnWebGPUCache", "ShaderCache", "GrShaderCache"];

fn claude_config_dirs() -> Vec<PathBuf> {
    let mut dirs = vec![default_claude_config_dir()];
    dirs.extend(registry::load().profiles.iter().filter_map(|p| p.code.as_ref().map(|c| c.config_dir.clone())));
    dirs
}

fn desktop_data_dirs() -> Vec<(String, PathBuf)> {
    let mut dirs = vec![("Default".to_string(), default_desktop_data_dir())];
    dirs.extend(registry::load().profiles.iter().filter_map(|p| p.desktop.as_ref().map(|d| (p.name.clone(), d.data_dir.clone()))));
    dirs.retain(|(_, d)| d.is_dir());
    dirs
}

/// Version folders, newest first.
fn versions_in(dir: &Path) -> Vec<PathBuf> {
    let mut v = subdirs(dir);
    v.retain(|p| name_of(p).chars().next().is_some_and(|c| c.is_ascii_digit()));
    v.sort_by(|a, b| compare_versions(&name_of(b), &name_of(a)));
    v
}

fn claude_items(run: &Running) -> Vec<Item> {
    let mut out = Vec::new();
    for (who, data) in desktop_data_dirs() {
        let open = run.desktop_open(&data);
        for c in DESKTOP_CACHES {
            let p = data.join(c);
            if p.is_dir() {
                let mut it = Item::new("Claude", format!("Claude Desktop {c} ({who})"), p, "Rebuilt when Claude Desktop next starts.", true);
                it.in_use = open;
                out.push(it);
            }
        }
        // Desktop keeps a Claude Code per version it has used; only the
        // newest is needed.
        for old in versions_in(&data.join("claude-code")).into_iter().skip(1) {
            let mut it = Item::new("Claude", format!("Old Claude Code {} in Claude Desktop ({who})", name_of(&old)), old.clone(), "An older copy Claude Desktop no longer uses.", true);
            it.in_use = run.uses(&old);
            out.push(it);
        }
    }
    // Claude Code's own installs: keep the newest.
    for dir in [home().join(".local/share/claude/versions")] {
        for old in versions_in(&dir).into_iter().skip(1) {
            let mut it = Item::new("Claude", format!("Old Claude Code {}", name_of(&old)), old.clone(), "An older Claude Code install; the newest stays.", true);
            it.in_use = run.uses(&old);
            out.push(it);
        }
    }
    // Per config folder: debug logs and shell snapshots, never history.
    for cfg in claude_config_dirs() {
        let who = if cfg == default_claude_config_dir() { "Default".to_string() } else { name_of(&cfg) };
        for (sub, note) in [("debug", "Claude Code debug logs."), ("shell-snapshots", "Shell environment snapshots Claude Code takes again as needed.")] {
            let p = cfg.join(sub);
            if p.is_dir() {
                out.push(Item::new("Claude", format!("Claude Code {sub} ({who})"), p, note, true));
            }
        }
    }
    out
}

// ---- Caches, logs and developer tools ----------------------------------------------------

fn cache_items(run: &Running) -> Vec<Item> {
    let mut out = Vec::new();
    let h = home();
    let root = if is_mac() { h.join("Library/Caches") } else if cfg!(windows) { local_appdata().join("Temp") } else { h.join(".cache") };
    if cfg!(windows) {
        // Temp: only what has sat a day, since fresh files can be drafts or
        // installers mid-way.
        for e in std::fs::read_dir(&root).into_iter().flatten().flatten() {
            let p = e.path();
            if days_since_modified(&p).is_some_and(|d| d >= 1) {
                out.push(Item::new("App caches", format!("Temp: {}", name_of(&p)), p, "A temporary file over a day old.", true));
            }
        }
        for (rel, label) in [("npm-cache", "npm download cache"), ("pip\\Cache", "pip download cache"), ("CrashDumps", "Crash dumps")] {
            let p = local_appdata().join(rel);
            if p.is_dir() {
                out.push(Item::new("Developer", label, p, "Downloaded again when needed.", true));
            }
        }
        return out;
    }
    for p in subdirs(&root) {
        let name = name_of(&p);
        // Apple's own caches belong to the system, and Claude's are listed
        // under Claude; tools with an owner command are offered that instead.
        if name.starts_with("com.apple.") || name.starts_with('.') || never(&p) || name == "Homebrew" || name.starts_with("claude") {
            continue;
        }
        let mut it = Item::new("App caches", name.clone(), p, "Rebuilt by its app. Quit the app first.", true);
        it.in_use = run.owns_cache(&name);
        out.push(it);
    }
    out
}

fn local_appdata() -> PathBuf {
    std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(|| home().join("AppData/Local"))
}

fn log_items() -> Vec<Item> {
    let root = if is_mac() { home().join("Library/Logs") } else { return Vec::new() };
    subdirs(&root)
        .into_iter()
        .filter(|p| !name_of(p).starts_with("DiagnosticReports"))
        .map(|p| Item::new("Logs", name_of(&p), p, "App logs, written again as apps run.", true))
        .collect()
}

fn developer_items(run: &Running) -> Vec<Item> {
    let h = home();
    let mut out = Vec::new();
    let mut add = |rel: &str, label: &str, note: &str, safe: bool| {
        let p = h.join(rel);
        if p.is_dir() {
            out.push(Item::new("Developer", label, p, note, safe));
        }
    };
    if is_mac() {
        add("Library/Developer/Xcode/DerivedData", "Xcode DerivedData", "Build output; Xcode rebuilds it (the next build is slower).", true);
        add("Library/Developer/CoreSimulator/Caches", "Simulator caches", "Rebuilt by the simulator.", true);
        add("Library/Developer/Xcode/iOS DeviceSupport", "iOS DeviceSupport", "Symbols for devices you have debugged; downloaded again when one connects.", false);
    }
    add(".npm/_cacache", "npm download cache", "Packages download again when needed.", true);
    add(".npm/_npx", "npx cache", "Tools npx ran; fetched again the next time you run them.", true);
    add(".bun/install/cache", "Bun download cache", "Packages download again when needed.", true);
    add(".pub-cache/hosted", "Dart and Flutter packages (pub cache)", "Downloaded again by `flutter pub get`; projects need the network to build until then.", false);
    add(".pub-cache/git", "Dart and Flutter git packages (pub cache)", "Cloned again by `flutter pub get`.", false);
    add(".pnpm-store", "pnpm store", "Packages download again when needed; projects relink on install.", false);
    add(".yarn/berry/cache", "Yarn cache", "Packages download again when needed.", true);
    add(".gradle/caches", "Gradle caches", "Dependencies download again on the next build.", true);
    add(".gradle/wrapper/dists", "Gradle distributions", "Each project's Gradle version downloads again on its next build.", true);
    add(".dartServer", "Dart analysis cache", "The Dart and Flutter editor tooling rebuilds it; the first analysis afterwards is slower.", true);
    if is_mac() {
        add("Library/Android/sdk/system-images", "Android system images", "Emulator system images; Android Studio's SDK Manager downloads them again. Emulators that use them stop working until then.", false);
    }
    add(".cargo/registry/cache", "Cargo download cache", "Crates download again when needed.", true);
    // Android emulators, each on its own: deleting one loses its apps and data.
    for avd in subdirs(&h.join(".android/avd")).into_iter().filter(|p| name_of(p).ends_with(".avd")) {
        let name = name_of(&avd).trim_end_matches(".avd").replace('_', " ");
        let mut it = Item::new("Developer", format!("Android emulator: {name}"), avd.clone(), "An emulator and everything installed on it. Android Studio can create it again, empty.", false);
        it.days_old = days_since_modified(&avd);
        it.in_use = run.uses(&avd) || run.procs.iter().any(|p| p.comm.starts_with("qemu-system") && p.command.contains(&name_of(&avd).trim_end_matches(".avd").to_string()));
        out.push(it);
    }
    for it in out.iter_mut() {
        if it.label == "Dart analysis cache" {
            it.in_use = run.procs.iter().any(|p| p.command.contains("analysis_server") || p.command.contains("language-server"));
        }
        // Xcode open: leave its build data alone.
        if it.label.contains("Xcode") {
            it.in_use = run.owns_cache("xcode") || run.procs.iter().any(|p| p.comm == "Xcode");
        }
    }
    out
}

// ---- Projects -------------------------------------------------------------------------------

/// The same folder however it is spelled: on a case-insensitive disk
/// ~/Projects and ~/projects are one, which canonicalize does not show.
fn identity(p: &Path) -> Option<(u64, u64)> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        std::fs::metadata(p).ok().map(|m| (m.dev(), m.ino()))
    }
    #[cfg(not(unix))]
    {
        use std::hash::{Hash, Hasher};
        let c = std::fs::canonicalize(p).ok()?;
        let mut h = std::collections::hash_map::DefaultHasher::new();
        c.to_string_lossy().to_lowercase().hash(&mut h);
        Some((0, h.finish()))
    }
}

fn project_roots() -> Vec<PathBuf> {
    let h = home();
    // On a case-insensitive disk ~/Projects and ~/projects are one folder:
    // keep each real folder once.
    let mut seen = HashSet::new();
    ["Projects", "projects", "Developer", "dev", "code", "Code", "src", "repos", "GitHub", "work"]
        .iter()
        .map(|d| h.join(d))
        .filter(|p| p.is_dir() && identity(p).is_some_and(|id| seen.insert(id)))
        .collect()
}

/// Whether `sub` (inside project folder `dir`) is rebuildable output, judged
/// by the manifest beside it; with what brings it back.
fn artifact_note(dir: &Path, sub: &Path) -> Option<&'static str> {
    match name_of(sub).as_str() {
        "node_modules" if dir.join("package.json").is_file() => Some("Node packages; `npm install` brings them back."),
        ".next" if dir.join("package.json").is_file() => Some("Next.js build output; `next build` rebuilds it."),
        ".dart_tool" if dir.join("pubspec.yaml").is_file() => Some("Dart tool data; `flutter pub get` brings it back."),
        "build" if dir.join("pubspec.yaml").is_file() => Some("Flutter build output; the next `flutter build` or run rebuilds it."),
        "build" if dir.join("build.gradle").is_file() || dir.join("build.gradle.kts").is_file() => Some("Gradle build output; the next build rebuilds it."),
        "Pods" if dir.join("Podfile").is_file() => Some("CocoaPods dependencies; `pod install` brings them back."),
        "target" if dir.join("Cargo.toml").is_file() => Some("Rust build output; `cargo build` rebuilds it."),
        ".venv" | "venv" if sub.join("pyvenv.cfg").is_file() => Some("Python virtual environment; recreate it from requirements."),
        _ => None,
    }
}

/// Build output and installed dependencies inside project folders: things a
/// project rebuilds from its manifest. Found by the marker beside them, as
/// kondo and npkill do.
fn project_items(run: &Running) -> Vec<Item> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    let mut queue: Vec<(PathBuf, usize)> = project_roots().into_iter().map(|r| (r, 0)).collect();
    let mut visited = 0;
    while let Some((dir, depth)) = queue.pop() {
        visited += 1;
        if visited > 20_000 || !identity(&dir).is_some_and(|id| seen.insert(id)) {
            continue;
        }
        for sub in subdirs(&dir) {
            let name = name_of(&sub);
            if let Some(note) = artifact_note(&dir, &sub) {
                let project = name_of(&dir);
                let mut it = Item::new("Project build folders", format!("{project}/{name}"), sub.clone(), note, false);
                it.days_old = days_since_modified(&dir);
                // A build or install running in the project.
                it.in_use = run.procs.iter().any(|p| p.command.contains(&path_string(&dir)));
                out.push(it);
                continue;
            }
            if depth < 4 && !name.starts_with('.') && sub.is_dir() && !std::fs::symlink_metadata(&sub).is_ok_and(|m| m.file_type().is_symlink()) {
                queue.push((sub, depth + 1));
            }
        }
    }
    out
}

// ---- Downloads (opt-in: macOS asks before an app reads it) -----------------------------

const INSTALLERS: &[&str] = &["dmg", "pkg", "msi", "exe", "iso", "appimage", "deb", "rpm"];

fn download_items() -> Vec<Item> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(home().join("Downloads")).into_iter().flatten().flatten() {
        let p = e.path();
        let name = name_of(&p);
        if name.starts_with('.') {
            continue;
        }
        let days = days_since_modified(&p).unwrap_or(0);
        let ext = p.extension().map(|x| x.to_string_lossy().to_lowercase()).unwrap_or_default();
        let note = if INSTALLERS.contains(&ext.as_str()) && days >= 7 {
            "An installer, usually not needed once installed."
        } else if days >= 90 {
            "Not changed in three months."
        } else {
            continue;
        };
        let mut it = Item::new("Downloads", name, p, note, false);
        it.days_old = Some(days);
        out.push(it);
    }
    out
}

// ---- Large files (opt-in, review only) --------------------------------------------------

/// Big enough to be worth a look.
const LARGE: u64 = 500 * MB;

/// Where a large file may be offered from: your own folders, not app data
/// (~/Library), not the Trash, not anything never() protects.
fn large_file_allowed(p: &Path) -> bool {
    let h = home();
    p.starts_with(&h)
        && !p.starts_with(h.join("Library"))
        && !p.starts_with(h.join(".Trash"))
        && !never(p)
        // Inside an app or a library bundle, the file is part of it.
        && !p.components().any(|c| {
            let c = c.as_os_str().to_string_lossy();
            c.ends_with(".app") || c.ends_with(".photoslibrary") || c.ends_with(".musiclibrary") || c.ends_with(".fcpbundle")
        })
}

/// Every file of LARGE or more in your home folder. Spotlight answers at
/// once on macOS; elsewhere `find`, kept to this disk.
fn large_items(run: &Running) -> Vec<Item> {
    let h = home();
    let listed = if is_mac() {
        run_output("/usr/bin/mdfind", [std::ffi::OsStr::new("-onlyin"), h.as_os_str(), std::ffi::OsStr::new(&format!("kMDItemFSSize >= {LARGE}"))])
    } else if cfg!(target_os = "linux") {
        run_output("find", [h.as_os_str(), std::ffi::OsStr::new("-xdev"), std::ffi::OsStr::new("-type"), std::ffi::OsStr::new("f"), std::ffi::OsStr::new("-size"), std::ffi::OsStr::new("+500M")])
    } else {
        None
    };
    let mut out = Vec::new();
    for line in listed.unwrap_or_default().lines() {
        let p = PathBuf::from(line);
        let Ok(m) = std::fs::symlink_metadata(&p) else { continue };
        if !m.is_file() || m.len() < LARGE || !large_file_allowed(&p) {
            continue;
        }
        let mut it = Item::new("Large files", name_of(&p), p.clone(), "A large file. It does not come back, so be sure you no longer need it.", false);
        it.bytes = m.len();
        it.days_old = days_since_modified(&p);
        it.in_use = run.uses(&p);
        out.push(it);
    }
    out
}

// ---- Scan -----------------------------------------------------------------------------------

/// The paths the last scan offered, kept owner-only beside the registry. A
/// run accepts only these.
fn offered_path() -> PathBuf {
    registry::config_dir().join("cleanup-offered.json")
}

/// Tools that clean up after themselves, offered as commands to run.
fn owner_tools() -> Vec<Value> {
    let mut t = Vec::new();
    let has = |c: &str| !crate::platform::which_all(c).is_empty();
    if has("brew") {
        t.push(json!({ "id": "brew", "label": "Homebrew", "command": "brew cleanup --prune=all", "note": "Removes old versions and downloads Homebrew keeps." }));
    }
    if is_mac() && has("xcrun") {
        t.push(json!({ "id": "simctl", "label": "Xcode simulators", "command": "xcrun simctl delete unavailable", "note": "Deletes simulators for runtimes no longer installed." }));
    }
    if has("docker") {
        t.push(json!({ "id": "docker", "label": "Docker", "command": "docker system prune", "note": "Removes stopped containers, unused networks and dangling images. It asks first." }));
    }
    if has("flutter") {
        t.push(json!({ "id": "pub", "label": "Dart and Flutter packages", "command": "flutter pub cache clean", "note": "Empties the pub cache; each project downloads what it needs on its next `flutter pub get`. It asks first." }));
    } else if has("dart") {
        t.push(json!({ "id": "pub", "label": "Dart packages", "command": "dart pub cache clean", "note": "Empties the pub cache; each project downloads what it needs on its next `dart pub get`. It asks first." }));
    }
    if has("npm") {
        t.push(json!({ "id": "npm", "label": "npm", "command": "npm cache verify", "note": "Checks npm's cache and clears out what is no longer needed." }));
    }
    if has("pnpm") {
        t.push(json!({ "id": "pnpm", "label": "pnpm", "command": "pnpm store prune", "note": "Removes packages no project uses any more from pnpm's store." }));
    }
    t
}

pub fn scan(include_downloads: bool, include_large: bool) -> Result<Value> {
    let run = Running::now();
    let mut items = claude_items(&run);
    items.extend(cache_items(&run));
    items.extend(log_items());
    items.extend(developer_items(&run));
    items.extend(project_items(&run));
    if include_downloads {
        items.extend(download_items());
    }
    items.retain(|it| !never(&it.path) && !is_link(&it.path));
    measure(&mut items);
    if include_large {
        // Measured already; a file inside something listed above is covered there.
        let large: Vec<Item> = large_items(&run).into_iter().filter(|l| !items.iter().any(|it| l.path.starts_with(&it.path))).collect();
        items.extend(large);
    }
    // Small things are not worth a decision.
    items.retain(|it| it.bytes >= if it.group == "Claude" { MB } else { 20 * MB });
    items.sort_by(|a, b| a.group.cmp(b.group).then(b.bytes.cmp(&a.bytes)));

    let offered: Vec<String> = items.iter().map(|it| path_string(&it.path)).collect();
    if let Some(dir) = offered_path().parent() {
        std::fs::create_dir_all(dir)?;
    }
    write_private_file(&offered_path(), &to_json_pretty(&offered))?;

    Ok(json!({
        "items": items.iter().map(|it| json!({
            "group": it.group,
            "label": it.label,
            "path": it.path,
            "note": it.note,
            "safe": it.safe,
            "inUse": it.in_use,
            "bytes": it.bytes,
            "daysOld": it.days_old,
        })).collect::<Vec<_>>(),
        "tools": owner_tools(),
        "downloadsIncluded": include_downloads,
        "largeIncluded": include_large,
    }))
}

fn is_link(p: &Path) -> bool {
    std::fs::symlink_metadata(p).is_ok_and(|m| m.file_type().is_symlink())
}

// ---- Run ------------------------------------------------------------------------------------

/// Move the chosen paths to the Trash. Each is checked again first, and the
/// reply accounts for every one.
pub fn run(paths: &[String]) -> Result<Value> {
    if paths.is_empty() {
        bail!("Nothing was chosen.");
    }
    let offered: HashSet<String> = read_json(&offered_path())
        .and_then(|v| v.as_array().cloned())
        .unwrap_or_default()
        .into_iter()
        .filter_map(|v| v.as_str().map(String::from))
        .collect();
    let roots = allowed_roots();
    let run = Running::now();
    let mut trash = trash::TrashContext::default();
    #[cfg(target_os = "macos")]
    {
        use trash::macos::{DeleteMethod, TrashContextExtMacos};
        // No Finder automation prompt; items can still be dragged back out.
        trash.set_delete_method(DeleteMethod::NsFileManager);
    }

    let (mut freed, mut moved, mut skipped, mut failed) = (0u64, Vec::new(), Vec::new(), Vec::new());
    for raw in paths {
        let p = PathBuf::from(raw);
        let why_not = if !offered.contains(raw) {
            Some("not offered by the last scan")
        } else if !p.exists() {
            Some("already gone")
        } else if is_link(&p) || never(&p) {
            Some("protected")
        } else if p == home() || !(roots.iter().any(|r| p.starts_with(r) && p != *r) || (p.is_file() && large_file_allowed(&p))) {
            Some("outside the areas cleanup may touch")
        } else if run.uses(&p) || desktop_cache_open(&p, &run) {
            Some("in use right now")
        } else {
            None
        };
        if let Some(why) = why_not {
            skipped.push(json!({ "path": raw, "reason": why }));
            continue;
        }
        let bytes = size_of(&p);
        match trash.delete(&p) {
            Ok(()) => {
                freed += bytes;
                moved.push(raw.clone());
            }
            Err(e) => failed.push(json!({ "path": raw, "reason": e.to_string() })),
        }
    }
    Ok(json!({ "freedBytes": freed, "moved": moved, "skipped": skipped, "failed": failed }))
}

/// A Claude Desktop cache folder whose app has been opened since the scan.
fn desktop_cache_open(p: &Path, run: &Running) -> bool {
    desktop_data_dirs().iter().any(|(_, d)| p.starts_with(d) && run.desktop_open(d))
}

/// Open one owner tool's cleanup in a terminal, so its own prompts and output
/// show. Only the fixed commands owner_tools lists.
pub fn run_tool(id: &str, terminal: &str) -> Result<Value> {
    let tools = owner_tools();
    let Some(t) = tools.iter().find(|t| t["id"] == id) else { bail!("\"{id}\" is not a cleanup tool here.") };
    let command = t["command"].as_str().unwrap_or_default();
    crate::platform::run_in_terminal(command, terminal)?;
    Ok(json!({ "command": command }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn irreplaceable_folders_are_never_candidates() {
        assert!(never(Path::new("/Users/me/Library/CloudStorage/Dropbox/x")));
        assert!(never(Path::new("/Users/me/.cache/huggingface/hub")));
        assert!(never(Path::new("/Users/me/Library/Containers/com.x/Data")));
        assert!(!never(Path::new("/Users/me/Library/Caches/com.spotify.client")));
    }

    #[test]
    fn large_files_stay_out_of_app_data() {
        let h = home();
        assert!(large_file_allowed(&h.join("Movies/render.mov")));
        assert!(!large_file_allowed(&h.join("Library/Containers/com.docker.docker/Data/Docker.raw")));
        assert!(!large_file_allowed(&h.join("Applications/Big.app/Contents/Resources/x.bin")));
        assert!(!large_file_allowed(&h.join("Library/CloudStorage/Dropbox/video.mp4")));
        assert!(!large_file_allowed(Path::new("/opt/big.iso")));
    }

    #[test]
    fn versions_sort_newest_first() {
        let root = tempfile::tempdir().unwrap();
        for v in ["2.1.9", "2.1.321", "2.1.40", "notes"] {
            std::fs::create_dir_all(root.path().join(v)).unwrap();
        }
        let names: Vec<String> = versions_in(root.path()).iter().map(|p| name_of(p)).collect();
        assert_eq!(names, vec!["2.1.321", "2.1.40", "2.1.9"]);
    }

    #[test]
    fn project_artifacts_need_their_marker() {
        let root = tempfile::tempdir().unwrap();
        let with = root.path().join("app");
        let without = root.path().join("notes");
        std::fs::create_dir_all(with.join("node_modules")).unwrap();
        std::fs::write(with.join("package.json"), "{}").unwrap();
        std::fs::create_dir_all(without.join("target")).unwrap();
        assert!(artifact_note(&with, &with.join("node_modules")).is_some());
        assert!(artifact_note(&without, &without.join("target")).is_none(), "a target folder with no Cargo.toml is not build output");
        let flutter = root.path().join("mobile");
        std::fs::create_dir_all(flutter.join("build")).unwrap();
        assert!(artifact_note(&flutter, &flutter.join("build")).is_none(), "a build folder needs a manifest beside it");
        std::fs::write(flutter.join("pubspec.yaml"), "name: x").unwrap();
        assert!(artifact_note(&flutter, &flutter.join("build")).is_some());
        assert!(artifact_note(&flutter, &flutter.join(".dart_tool")).is_some());
    }

    #[test]
    fn running_apps_own_their_caches() {
        let run = Running { procs: Vec::new(), bundles: vec!["com.spotify.client".into(), "com.google.chrome".into()] };
        assert!(run.owns_cache("com.spotify.client"));
        assert!(run.owns_cache("Google"));
        assert!(!run.owns_cache("com.figma.Desktop"));
        let brave = Running { procs: Vec::new(), bundles: vec!["com.brave.browser".into()] };
        assert!(brave.owns_cache("BraveSoftware"));
        assert!(!brave.owns_cache("org.swift.swiftpm"));
    }
}
