// Claude Profiles: a desktop window for claude-profiles, built with GPUI so
// one codebase runs on macOS, Linux and Windows. It follows the macOS menu
// bar panel: every account at a glance with who it is signed in as, its plan
// usage and what is running, plus starting, adding, importing, renaming and
// removing accounts.
//
// All of it goes through `claude-profiles app …` (see cli.rs), on a
// background thread, so a slow call never freezes the window. The look lives
// in design.rs.

#![cfg_attr(windows, windows_subsystem = "windows")]

mod cli;
mod design;

use std::collections::HashSet;
use std::path::Path;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::checkbox::Checkbox;
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::menu::{DropdownMenu, PopupMenuItem};
use gpui_component::progress::Progress;
use gpui_component::{ActiveTheme, Disableable, Root, Sizable, StyledExt, Theme};
use serde_json::{json, Value};

use design::{accent, action, card_bg, chip, confirm, danger_icon, dot, green, icon, icon_button, kind_tag, named, orange, pill, quiet, red, secondary, window_bg, Assets, GLASS};

const STATE_EVERY: Duration = Duration::from_secs(15);
const USAGE_EVERY: Duration = Duration::from_secs(300);
const TICK: Duration = Duration::from_secs(2);

fn main() {
    Application::new().with_assets(Assets).run(|cx: &mut App| {
        gpui_component::init(cx);
        // Linux and Windows keep running with no window unless told to stop.
        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        let bounds = Bounds::centered(None, size(px(430.), px(800.)), cx);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(TitlebarOptions { title: Some("Claude Profiles".into()), ..Default::default() }),
            window_min_size: Some(size(px(380.), px(480.))),
            window_background: if GLASS { WindowBackgroundAppearance::Blurred } else { WindowBackgroundAppearance::Opaque },
            app_id: Some("claude-profiles".into()),
            ..Default::default()
        };
        cx.open_window(options, |window, cx| {
            Theme::sync_system_appearance(Some(window), cx);
            let view = cx.new(Profiles::new);
            cx.new(|cx| Root::new(view, window, cx))
        })
        .expect("could not open the window");
        cx.activate(true);
    });
}

fn id(s: impl Into<String>) -> ElementId {
    ElementId::Name(s.into().into())
}

// ---- Reading the state ----------------------------------------------------------------------

fn memory(mb: u64) -> String {
    if mb >= 1024 {
        format!("{:.1} GB", mb as f64 / 1024.0)
    } else {
        format!("{mb} MB")
    }
}

fn short_path(p: &str) -> String {
    let home = cli::home().to_string_lossy().to_string();
    match p.strip_prefix(&home) {
        Some(rest) if !home.is_empty() => format!("~{rest}"),
        _ => p.to_string(),
    }
}

/// "email · organisation", leaving out an organisation that only repeats who.
fn account_label(a: &Value) -> Option<String> {
    let who = a["email"].as_str().or(a["name"].as_str())?;
    match a["organization"].as_str() {
        Some(org) if !org.is_empty() && !org.to_lowercase().contains(&who.to_lowercase()) => Some(format!("{who} · {org}")),
        _ => Some(who.to_string()),
    }
}

/// Claude Desktop records only an account ID; name it as best we can.
fn desktop_account_label(a: &Value) -> String {
    if a.is_null() {
        return "Not signed in".into();
    }
    if let Some(m) = account_label(&a["matched"]) {
        return m;
    }
    if let Some(n) = a["nickname"].as_str().filter(|n| !n.is_empty()) {
        return n.to_string();
    }
    format!("Account {}", a["accountUuid"].as_str().unwrap_or("").chars().take(8).collect::<String>())
}

fn resets(at: &Value) -> String {
    let Some(t) = at.as_str().and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok()) else { return String::new() };
    let local = t.with_timezone(&chrono::Local);
    let today = local.date_naive() == chrono::Local::now().date_naive();
    format!("resets {}", local.format(if today { "%H:%M" } else { "%a %H:%M" }))
}

fn sessions_of(v: &Value) -> Vec<Value> {
    v.as_array().cloned().unwrap_or_default()
}

/// What a typed profile name becomes: lowercase, spaces and underscores as
/// hyphens, anything else dropped (the same as the menu bar app).
fn profile_name(typed: &str) -> String {
    let mut out = String::new();
    for ch in typed.to_lowercase().chars() {
        if ch == ' ' || ch == '_' || ch == '-' {
            if !out.ends_with('-') {
                out.push('-');
            }
        } else if ch.is_ascii_alphanumeric() {
            out.push(ch);
        }
    }
    out.trim_matches('-').to_string()
}

// ---- Background calls ---------------------------------------------------------------------

enum Job {
    State,
    Usage,
    Sizes,
    Importable,
    CopyCommand,
    /// A change, with what to say once it worked (empty: say nothing).
    Action(String),
}

// ---- Settings the window keeps --------------------------------------------------------

struct Settings {
    terminal: String,
}

impl Settings {
    fn path() -> std::path::PathBuf {
        cli::config_dir().join("gui.json")
    }

    fn load() -> Settings {
        let v: Value = std::fs::read_to_string(Self::path()).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
        Settings { terminal: v["terminal"].as_str().unwrap_or("").to_string() }
    }

    fn save(&self) {
        let p = Self::path();
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(p, serde_json::to_string_pretty(&json!({ "terminal": self.terminal })).unwrap_or_default());
    }
}

// ---- Screens ------------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq)]
enum Tab {
    Accounts,
    Running,
    Files,
}

/// What replaces the list for a moment, as the menu bar panel's screens do.
enum Screen {
    Add { name: Entity<InputState>, targets: &'static str, seed: bool, gh: bool, color: String, _watch: Subscription },
    Rename { old: String, desktop_running: bool, new: Entity<InputState> },
    Remove { name: String, part: Option<&'static str>, desktop: bool, code: bool, delete_desktop: bool, delete_code: bool },
    NameAccount { uuid: String, name: Entity<InputState> },
    Import,
    Confirm { title: String, text: String, button: String, danger: bool, args: Vec<String>, done: String },
}

// ---- Menus --------------------------------------------------------------------------------

type Act = Rc<dyn Fn(&mut Profiles, &mut Window, &mut Context<Profiles>)>;

enum Entry {
    Item(SharedString, Act),
    Check(SharedString, bool, Act),
    Label(SharedString),
    Sep,
}

fn item(label: impl Into<SharedString>, f: impl Fn(&mut Profiles, &mut Window, &mut Context<Profiles>) + 'static) -> Entry {
    Entry::Item(label.into(), Rc::new(f))
}

// ---- The view -----------------------------------------------------------------------------

struct Profiles {
    state: Option<Value>,
    usage: Option<Value>,
    sizes: Option<Value>,
    importable: Vec<Value>,
    error: Option<String>,
    loading_state: bool,
    loading_usage: bool,
    last_state: Option<Instant>,
    last_usage: Option<Instant>,
    busy: Vec<String>,
    toast: Option<(String, bool, Instant)>,
    settings: Settings,
    tab: Tab,
    screen: Option<Screen>,
    expanded: HashSet<String>,
    all_sessions: HashSet<String>,
    highlighted: Option<(String, Instant)>,
    scroll: ScrollHandle,
}

impl Profiles {
    fn new(cx: &mut Context<Self>) -> Profiles {
        let mut this = Profiles {
            state: None,
            usage: None,
            sizes: None,
            importable: Vec::new(),
            error: None,
            loading_state: false,
            loading_usage: false,
            last_state: None,
            last_usage: None,
            busy: Vec::new(),
            toast: None,
            settings: Settings::load(),
            tab: Tab::Accounts,
            screen: None,
            expanded: HashSet::new(),
            all_sessions: HashSet::new(),
            highlighted: None,
            scroll: ScrollHandle::new(),
        };
        this.call(Job::Action(String::new()), vec!["startup".into()], cx);
        this.refresh(cx);
        this.call(Job::Usage, vec!["usage".into()], cx);
        this.call(Job::Importable, vec!["importable".into()], cx);
        cx.spawn(async move |this, cx| loop {
            cx.background_executor().timer(TICK).await;
            if this.update(cx, |this, cx| this.tick(cx)).is_err() {
                break;
            }
        })
        .detach();
        this
    }

    fn tick(&mut self, cx: &mut Context<Self>) {
        let now = Instant::now();
        if self.last_state.is_some_and(|t| now - t > STATE_EVERY) {
            self.refresh(cx);
        }
        if !self.loading_usage && self.last_usage.is_some_and(|t| now - t > USAGE_EVERY) {
            self.call(Job::Usage, vec!["usage".into()], cx);
        }
        if self.toast.as_ref().is_some_and(|(_, error, at)| at.elapsed() > Duration::from_secs(if *error { 20 } else { 5 })) {
            self.toast = None;
            cx.notify();
        }
        if self.highlighted.as_ref().is_some_and(|(_, at)| at.elapsed() > Duration::from_millis(1600)) {
            self.highlighted = None;
            cx.notify();
        }
    }

    fn call(&mut self, job: Job, args: Vec<String>, cx: &mut Context<Self>) {
        match &job {
            Job::State => self.loading_state = true,
            Job::Usage => self.loading_usage = true,
            Job::Action(done) if !done.is_empty() => self.busy.push(done.clone()),
            _ => {}
        }
        let task = cx.background_executor().spawn(async move { cli::run(&args) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.receive(job, result, cx);
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        if !self.loading_state {
            self.call(Job::State, vec!["state".into()], cx);
        }
    }

    /// Run a change; everything it may have touched is reread afterwards.
    /// `done` is the progress text, ending in "…"; once it worked, the same
    /// words without the dots say so.
    fn act(&mut self, done: impl Into<String>, args: &[&str], cx: &mut Context<Self>) {
        self.call(Job::Action(done.into()), args.iter().map(|s| s.to_string()).collect(), cx);
    }

    fn notify(&mut self, text: impl Into<String>, error: bool) {
        self.toast = Some((text.into(), error, Instant::now()));
    }

    fn receive(&mut self, job: Job, result: Result<Value, String>, cx: &mut Context<Self>) {
        match job {
            Job::State => {
                self.loading_state = false;
                self.last_state = Some(Instant::now());
                match result {
                    Ok(v) => {
                        self.state = Some(v);
                        self.error = None;
                    }
                    Err(e) => self.error = Some(e),
                }
            }
            Job::Usage => {
                self.loading_usage = false;
                self.last_usage = Some(Instant::now());
                if let Ok(v) = result {
                    self.usage = Some(v);
                }
            }
            Job::Sizes => {
                if let Ok(v) = result {
                    self.sizes = Some(v);
                }
            }
            Job::Importable => {
                if let Ok(v) = result {
                    self.importable = v["candidates"].as_array().cloned().unwrap_or_default();
                }
            }
            Job::CopyCommand => match result {
                Ok(v) => {
                    cx.write_to_clipboard(ClipboardItem::new_string(v["command"].as_str().unwrap_or("").to_string()));
                    self.notify("Command copied", false);
                }
                Err(e) => self.notify(e, true),
            },
            Job::Action(done) => {
                self.busy.retain(|b| *b != done);
                match result {
                    Ok(_) if done.is_empty() => {}
                    Ok(_) => self.notify(done.trim_end_matches('…').to_string(), false),
                    Err(e) => self.notify(e, true),
                }
                if !done.is_empty() {
                    self.refresh(cx);
                    self.call(Job::Importable, vec!["importable".into()], cx);
                }
            }
        }
    }

    fn load_sizes(&mut self, cx: &mut Context<Self>) {
        self.call(Job::Sizes, vec!["sizes".into()], cx);
    }

    fn terminals(&self) -> Vec<String> {
        self.state
            .as_ref()
            .and_then(|s| s["terminals"].as_array())
            .map(|a| a.iter().filter_map(|t| t.as_str().map(String::from)).collect())
            .unwrap_or_default()
    }

    fn terminal(&self) -> String {
        let list = self.terminals();
        if list.contains(&self.settings.terminal) {
            self.settings.terminal.clone()
        } else {
            list.first().cloned().unwrap_or_default()
        }
    }

    fn is_mac(&self) -> bool {
        self.state.as_ref().is_some_and(|s| s["isMac"].as_bool() == Some(true))
    }

    fn usage_for(&self, name: &str) -> Option<Value> {
        let u = self.usage.as_ref()?;
        if name == "default" {
            return Some(u["default"].clone());
        }
        u["profiles"].as_array()?.iter().find(|p| p["name"] == name).map(|p| p["usage"].clone())
    }

    fn size_for(&self, name: &str) -> Value {
        let Some(s) = &self.sizes else { return Value::Null };
        if name == "default" {
            return s["default"].clone();
        }
        s["profiles"].as_array().and_then(|a| a.iter().find(|p| p["name"] == name).cloned()).unwrap_or(Value::Null)
    }

    /// The first card signed in to this Claude Code account: profiles first,
    /// since the default install is the one people tend to share.
    fn first_owner(&self, uuid: &str) -> Option<String> {
        let s = self.state.as_ref()?;
        for p in s["profiles"].as_array().into_iter().flatten() {
            if p["code"]["account"]["accountUuid"].as_str() == Some(uuid) {
                return p["name"].as_str().map(String::from);
            }
        }
        (s["default"]["code"]["account"]["accountUuid"].as_str() == Some(uuid)).then(|| "default".to_string())
    }

    fn running_count(&self) -> usize {
        let Some(s) = &self.state else { return 0 };
        let mut n = sessions_of(&s["default"]["code"]["sessions"]).len() + usize::from(s["default"]["desktop"]["running"] == true);
        for p in s["profiles"].as_array().into_iter().flatten() {
            n += sessions_of(&p["code"]["sessions"]).len() + usize::from(p["desktop"]["clone"]["running"] == true);
        }
        n + s["otherSessions"].as_array().into_iter().flatten().map(|o| sessions_of(&o["sessions"]).len()).sum::<usize>()
    }

    // ---- Actions the cards share ----------------------------------------------------------

    fn open_cli(&mut self, target: &str, in_folder: bool, cx: &mut Context<Self>) {
        if !in_folder {
            let term = self.terminal();
            self.act(format!("Opening {term}…"), &["open-cli", target, "--terminal", &term], cx);
            return;
        }
        let picked = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Start Claude Code here".into()),
        });
        let target = target.to_string();
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = picked.await else { return };
            let Some(dir) = paths.first().map(|p| p.to_string_lossy().to_string()) else { return };
            let _ = this.update(cx, |this, cx| {
                let term = this.terminal();
                this.act(format!("Opening {term}…"), &["open-cli", &target, "--dir", &dir, "--terminal", &term], cx);
            });
        })
        .detach();
    }

    fn sign_in(&mut self, target: &str, cx: &mut Context<Self>) {
        let term = self.terminal();
        self.act(format!("Opening {term} to sign in…"), &["open-cli", target, "--login", "--terminal", &term], cx);
    }

    fn copy_command(&mut self, target: &str, cx: &mut Context<Self>) {
        self.call(Job::CopyCommand, vec!["cli-command".into(), target.into()], cx);
    }

    #[allow(clippy::too_many_arguments)]
    fn confirm(&mut self, title: String, text: String, button: &str, danger: bool, args: &[&str], done: String, cx: &mut Context<Self>) {
        self.screen = Some(Screen::Confirm {
            title,
            text,
            button: button.into(),
            danger,
            args: args.iter().map(|s| s.to_string()).collect(),
            done,
        });
        cx.notify();
    }

    fn name_account(&mut self, uuid: String, current: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        let name = cx.new(|cx| InputState::new(window, cx).placeholder("e.g. Collins").default_value(current.unwrap_or_default()));
        self.screen = Some(Screen::NameAccount { uuid, name });
        cx.notify();
    }

    /// Scroll to a profile's card and light it up for a moment.
    fn reveal(&mut self, owner: &str, cx: &mut Context<Self>) {
        let Some(s) = &self.state else { return };
        let ix = s["profiles"].as_array().and_then(|a| a.iter().position(|p| p["name"] == owner)).map(|i| i + 1).unwrap_or(0);
        self.tab = Tab::Accounts;
        self.scroll.scroll_to_top_of_item(ix);
        self.highlighted = Some((owner.to_string(), Instant::now()));
        cx.notify();
    }

    fn back(&mut self, cx: &mut Context<Self>) {
        self.screen = None;
        cx.notify();
    }
}

// ---- Rendering ----------------------------------------------------------------------------

impl Render for Profiles {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let content: AnyElement = match &self.screen {
            Some(_) => div().id("screen").flex_1().min_h_0().overflow_y_scroll().child(self.render_screen(cx)).into_any_element(),
            None => self.render_main(cx).into_any_element(),
        };
        div()
            .size_full()
            .v_flex()
            .gap_2()
            .p_3()
            .bg(window_bg(cx))
            .text_color(cx.theme().foreground)
            .text_sm()
            .child(content)
            .child(self.render_status(cx))
    }
}

impl Profiles {
    fn menu(&self, trigger: gpui_component::button::Button, entries: Vec<Entry>, cx: &mut Context<Self>) -> impl IntoElement {
        let weak = cx.weak_entity();
        let entries = Rc::new(entries);
        trigger.dropdown_menu_with_anchor(Corner::TopRight, move |mut menu, _, _| {
            for e in entries.iter() {
                menu = match e {
                    Entry::Item(label, f) | Entry::Check(label, _, f) => {
                        let (f, w) = (f.clone(), weak.clone());
                        let checked = matches!(e, Entry::Check(_, true, _));
                        menu.item(PopupMenuItem::new(label.clone()).checked(checked).on_click(move |_, window, cx| {
                            let _ = w.update(cx, |this, cx| f(this, window, cx));
                        }))
                    }
                    Entry::Label(label) => menu.item(PopupMenuItem::label(label.clone())),
                    Entry::Sep => menu.separator(),
                };
            }
            menu
        })
    }

    fn render_main(&mut self, cx: &mut Context<Self>) -> Div {
        let header = div()
            .h_flex()
            .gap_1()
            .child(div().text_xl().font_bold().child("Claude Profiles"))
            .child(div().flex_1())
            .child(icon_button("refresh", "refresh-cw", "Refresh", cx).loading(self.loading_state).on_click(cx.listener(|this, _, _, cx| {
                this.refresh(cx);
                this.call(Job::Usage, vec!["usage".into()], cx);
                this.call(Job::Importable, vec!["importable".into()], cx);
            })))
            .child(self.settings_menu(cx));

        let mut main = div().flex_1().min_h_0().v_flex().gap_2().child(header);
        let Some(state) = self.state.clone() else {
            return main.child(match &self.error {
                Some(e) => div().text_color(red()).child(e.clone()),
                None => secondary("Reading your accounts…", cx),
            });
        };
        main = main.child(self.render_versions(&state, cx)).child(self.render_tabs(cx));

        // Block layout, not a flex column: GPUI starts an overflowing flex column
        // part-way down. Cards space themselves with a bottom margin.
        let list = div().id("list").flex_1().min_h_0().overflow_y_scroll().track_scroll(&self.scroll);
        let list = match self.tab {
            Tab::Accounts => {
                let mut l = list.child(self.render_default(&state, cx));
                for p in state["profiles"].as_array().cloned().unwrap_or_default() {
                    l = l.child(self.render_profile(&p, &state, cx));
                }
                l
            }
            Tab::Running => list.children(self.render_running(&state, cx)),
            Tab::Files => list.children(self.render_files(&state, cx)),
        };
        main = main.child(list);

        if self.tab == Tab::Accounts {
            let mut row = div().h_flex().gap_2().child(pill("add", "circle-plus", "Add an account", cx).on_click(cx.listener(
                |this, _, window, cx| {
                    let name = cx.new(|cx| InputState::new(window, cx).placeholder("work"));
                    let watch = cx.subscribe(&name, |_, _, _: &InputEvent, cx| cx.notify());
                    let targets = if this.is_mac() { "both" } else { "code" };
                    this.screen = Some(Screen::Add { name, targets, seed: true, gh: false, color: String::new(), _watch: watch });
                    cx.notify();
                },
            )));
            if !self.importable.is_empty() {
                row = row.child(
                    pill("import", "download", &format!("Import {}", self.importable.len()), cx)
                        .tooltip("Make profiles from Claude accounts already on this computer")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.screen = Some(Screen::Import);
                            cx.notify();
                        })),
                );
            }
            main = main.child(row);
        }
        main
    }

    fn settings_menu(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mut entries = vec![
            item("Check everything (doctor)…", |this, _, cx| {
                let t = this.terminal();
                this.act("Opening doctor…", &["in-terminal", "doctor", "--terminal", &t], cx)
            }),
            item("Repair what doctor finds (doctor --fix)", |this, _, cx| this.act("doctor --fix finished…", &["doctor-fix"], cx)),
            item("Health check (status)…", |this, _, cx| {
                let t = this.terminal();
                this.act("Opening status…", &["in-terminal", "status", "--terminal", &t], cx)
            }),
        ];
        if self.is_mac() {
            entries.push(item("Copy extensions between profiles…", |this, _, cx| {
                let t = this.terminal();
                this.act("Opening…", &["in-terminal", "extensions", "--terminal", &t], cx)
            }));
        }
        entries.push(item("Full add wizard (custom folders, linking)…", |this, _, cx| {
            let t = this.terminal();
            this.act("Opening…", &["in-terminal", "add", "--terminal", &t], cx)
        }));
        entries.push(Entry::Sep);
        entries.push(Entry::Label("Open Claude Code in".into()));
        let chosen = self.terminal();
        for t in self.terminals() {
            let checked = t == chosen;
            entries.push(Entry::Check(
                t.clone().into(),
                checked,
                Rc::new(move |this: &mut Profiles, _: &mut Window, cx: &mut Context<Profiles>| {
                    this.settings.terminal = t.clone();
                    this.settings.save();
                    cx.notify();
                }),
            ));
        }
        self.menu(icon_button("settings", "settings", "Settings", cx), entries, cx)
    }

    fn render_versions(&self, s: &Value, cx: &App) -> Div {
        let mut row = div().h_flex().gap_1p5();
        if let Some(v) = s["claude"]["desktopApp"]["version"].as_str() {
            row = row.child(chip(format!("Desktop {v}"), None, cx));
        }
        row = row.child(match s["claude"]["cli"]["version"].as_str() {
            Some(v) => {
                let shadowed = s["claude"]["cli"]["shadowed"].as_array().is_some_and(|a| !a.is_empty());
                chip(format!("Code {}", v.split_whitespace().next().unwrap_or(v)), shadowed.then(orange), cx)
            }
            None => chip("Code not on PATH", Some(orange()), cx),
        });
        let has_desktop = s["profiles"].as_array().into_iter().flatten().any(|p| !p["desktop"].is_null());
        if s["helper"].as_str() != Some("current") && has_desktop {
            row = row.child(chip("run doctor --fix", Some(orange()), cx));
        }
        row.child(div().flex_1()).child(secondary(format!("v{}", s["version"].as_str().unwrap_or("?")), cx))
    }

    /// A segmented control: the selected segment sits raised on the track.
    fn segmented<T: Copy + PartialEq + 'static>(
        &self,
        key: &str,
        options: Vec<(T, String)>,
        selected: T,
        on_pick: impl Fn(&mut Profiles, T, &mut Context<Profiles>) + 'static,
        cx: &mut Context<Self>,
    ) -> Div {
        let on_pick = Rc::new(on_pick);
        let mut bar = div().h_flex().p(px(3.)).gap(px(2.)).rounded(px(9.)).bg(cx.theme().foreground.opacity(0.06));
        for (i, (value, label)) in options.into_iter().enumerate() {
            let is = value == selected;
            let on_pick = on_pick.clone();
            bar = bar.child(
                div()
                    .id(id(format!("{key}-{i}")))
                    .flex_1()
                    .py(px(4.))
                    .rounded(px(7.))
                    .text_center()
                    .cursor_pointer()
                    .when(is, |d| d.bg(cx.theme().background).shadow_sm().font_weight(FontWeight::MEDIUM))
                    .when(!is, |d| d.text_color(cx.theme().muted_foreground).hover(|d| d.bg(cx.theme().foreground.opacity(0.05))))
                    .child(label)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        on_pick(this, value, cx);
                        cx.notify();
                    })),
            );
        }
        bar
    }

    fn render_tabs(&self, cx: &mut Context<Self>) -> Div {
        let running = self.running_count();
        let tabs = vec![
            (Tab::Accounts, "Accounts".to_string()),
            (Tab::Running, if running > 0 { format!("Running ({running})") } else { "Running".into() }),
            (Tab::Files, "Files".to_string()),
        ];
        self.segmented(
            "tab",
            tabs,
            self.tab,
            |this, tab, cx| {
                this.tab = tab;
                if tab == Tab::Files {
                    this.load_sizes(cx);
                }
            },
            cx,
        )
    }

    fn render_status(&self, cx: &App) -> Div {
        let row = div().h_flex().gap_2().min_h(px(18.)).text_xs();
        if let Some(b) = self.busy.last() {
            return row.child(icon("loader-circle").xsmall().text_color(cx.theme().muted_foreground)).child(secondary(b.clone(), cx));
        }
        match &self.toast {
            Some((text, true, _)) => row.child(icon("triangle-alert").xsmall().text_color(red())).child(div().text_color(red()).child(text.clone())),
            Some((text, false, _)) => row.child(icon("check").xsmall().text_color(green())).child(div().text_color(green()).child(text.clone())),
            None => row,
        }
    }

    fn card(&self, name: &str, cx: &App) -> Div {
        let lit = self.highlighted.as_ref().is_some_and(|(n, _)| n == name);
        div()
            .flex_shrink_0()
            .mb_2()
            .v_flex()
            .gap_2()
            .p_3()
            .rounded(px(12.))
            .bg(card_bg(cx))
            .border_1()
            .border_color(if lit { accent() } else { cx.theme().foreground.opacity(0.06) })
    }

    // ---- Accounts tab ---------------------------------------------------------------------

    fn cli_buttons(&self, target: &str, cx: &mut Context<Self>) -> Div {
        let (t1, t2, t3) = (target.to_string(), target.to_string(), target.to_string());
        div()
            .h_flex()
            .gap_1p5()
            .child(action(id(format!("start-{target}")), "square-terminal", "Start Claude Code", cx).on_click(cx.listener(
                move |this, _, _, cx| this.open_cli(&t1, false, cx),
            )))
            .child(action(id(format!("folder-{target}")), "folder", "In folder…", cx).on_click(cx.listener(move |this, _, _, cx| {
                this.open_cli(&t2, true, cx)
            })))
            .child(
                icon_button(id(format!("copy-{target}")), "copy", "Copy the command that starts this account", cx)
                    .on_click(cx.listener(move |this, _, _, cx| this.copy_command(&t3, cx))),
            )
    }

    fn cli_entries(target: &str, signed_in: bool, config_dir: Option<String>, label: Option<String>) -> Vec<Entry> {
        let t = target.to_string();
        let mut e = vec![
            item("Start Claude Code", {
                let t = t.clone();
                move |this, _, cx| this.open_cli(&t, false, cx)
            }),
            item("Start in folder…", {
                let t = t.clone();
                move |this, _, cx| this.open_cli(&t, true, cx)
            }),
            item("Copy command", {
                let t = t.clone();
                move |this, _, cx| this.copy_command(&t, cx)
            }),
            Entry::Sep,
        ];
        if !signed_in {
            e.push(item("Sign in…", {
                let t = t.clone();
                move |this, _, cx| this.sign_in(&t, cx)
            }));
        } else if t != "default" {
            let who = label.unwrap_or_else(|| "its account".into());
            e.push(item("Sign out…", {
                let t = t.clone();
                move |this, _, cx| {
                    this.confirm(
                        format!("Sign {t}'s Claude Code out?"),
                        format!("Its CLI signs out of {who}. Its folder, chats and settings stay, and the App part is not touched. Sign in again any time."),
                        "Sign Out",
                        true,
                        &["sign-out", &t],
                        format!("Signed out {t}'s CLI…"),
                        cx,
                    )
                }
            }));
            e.push(item("Switch account…", {
                let t = t.clone();
                move |this, _, cx| this.sign_in(&t, cx)
            }));
        }
        if let Some(dir) = config_dir {
            e.push(Entry::Sep);
            e.push(item("Show config folder", move |_, _, cx| cx.open_with_system(Path::new(&dir))));
        }
        e
    }

    fn render_usage(&self, target: &str, cx: &mut Context<Self>) -> Div {
        let out = div().v_flex().gap_1();
        let Some(u) = self.usage_for(target) else {
            return out.when(self.loading_usage, |d| d.child(secondary("Reading plan usage…", cx)));
        };
        let note = |t: &str| secondary(t.to_string(), cx);
        match u["status"].as_str() {
            Some("ok") => {}
            Some("expired") => return out.child(note("Usage shows after its next Claude Code session refreshes the sign-in.")),
            Some("unavailable") => return out.child(note("Usage unavailable right now.")),
            Some("signed-out") => {
                let t = target.to_string();
                return out.child(
                    div()
                        .h_flex()
                        .gap_2()
                        .child(note("Its saved sign-in is missing, so usage can't be read.").flex_1())
                        .child(quiet(id(format!("usage-login-{t}")), "Sign in", cx).on_click(cx.listener(move |this, _, _, cx| this.sign_in(&t, cx)))),
                );
            }
            _ => return out,
        }
        let mut out = out;
        for (key, label) in [("fiveHour", "5 hours"), ("sevenDay", "Week"), ("sevenDayOpus", "Opus week"), ("sevenDaySonnet", "Sonnet week")] {
            let l = &u[key];
            if !l.is_object() {
                continue;
            }
            let pct = l["percent"].as_f64().unwrap_or(0.0).clamp(0.0, 100.0) as f32;
            let tint = if pct >= 90.0 { red() } else if pct >= 70.0 { orange() } else { accent() };
            out = out.child(
                div()
                    .h_flex()
                    .gap_2()
                    .text_xs()
                    .child(div().w(px(70.)).flex_none().text_color(cx.theme().muted_foreground).child(label))
                    .child(div().flex_1().child(Progress::new().value(pct).bg(tint).h(px(6.))))
                    .child(div().w(px(34.)).flex_none().text_right().child(format!("{}%", pct.round() as i64)))
                    .child(div().w(px(96.)).flex_none().whitespace_nowrap().text_color(cx.theme().muted_foreground).child(resets(&l["resetsAt"]))),
            );
        }
        out
    }

    fn render_sessions(&self, key: &str, sessions: &[Value], cx: &mut Context<Self>) -> Div {
        const SHOWN: usize = 3;
        let all = self.all_sessions.contains(key);
        let mut out = div().v_flex().gap_0p5();
        for s in sessions.iter().take(if all { usize::MAX } else { SHOWN }) {
            let pid = s["pid"].as_i64().unwrap_or(0).to_string();
            let cwd = s["cwd"].as_str().map(String::from);
            let from_desktop = s["fromDesktop"] == true;
            let shown = cwd.as_deref().map(short_path).unwrap_or_else(|| "?".into());
            let ask = {
                let (pid, shown) = (pid.clone(), shown.clone());
                cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.confirm(
                        "End this Claude Code session?".into(),
                        format!("Process {pid} in {shown}. Anything it is in the middle of stops."),
                        "End Session",
                        true,
                        &["stop-session", &pid],
                        "Session ended…".into(),
                        cx,
                    )
                })
            };
            out = out.child(
                div()
                    .h_flex()
                    .gap_1p5()
                    .text_xs()
                    .child(icon(if from_desktop { "app-window" } else { "square-terminal" }).xsmall().text_color(cx.theme().muted_foreground))
                    .child(div().flex_1().min_w_0().truncate().child(shown))
                    .child(secondary(format!("{} · {}", memory(s["memoryMB"].as_u64().unwrap_or(0)), s["uptime"].as_str().unwrap_or("")), cx))
                    .when_some(cwd, |d, c| {
                        d.child(
                            icon_button(id(format!("sess-dir-{key}-{pid}")), "folder", "Show this session's folder", cx)
                                .xsmall()
                                .on_click(move |_, _, cx| cx.open_with_system(Path::new(&c))),
                        )
                    })
                    .child(danger_icon(id(format!("sess-stop-{key}-{pid}")), "circle-stop", &format!("End this session (pid {pid})"), cx).on_click(ask)),
            );
        }
        if sessions.len() > SHOWN {
            let key = key.to_string();
            out = out.child(
                div()
                    .id(id(format!("more-{key}")))
                    .text_xs()
                    .text_color(accent())
                    .cursor_pointer()
                    .child(if all { "Show fewer".to_string() } else { format!("Show all {}", sessions.len()) })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if !this.all_sessions.remove(&key) {
                            this.all_sessions.insert(key.clone());
                        }
                        cx.notify();
                    })),
            );
        }
        out
    }

    fn render_default(&self, s: &Value, cx: &mut Context<Self>) -> Div {
        let (code, desktop) = (&s["default"]["code"], &s["default"]["desktop"]);
        let code_acct = &code["account"];
        let d_acct = &desktop["account"];
        let merged = !d_acct.is_null() && d_acct["accountUuid"] == code_acct["accountUuid"];
        let sessions = sessions_of(&code["sessions"]);

        let mut card = self.card("default", cx).child(
            div()
                .h_flex()
                .gap_2()
                .child(icon("circle-user").small().text_color(cx.theme().muted_foreground))
                .child(div().text_base().font_semibold().child("Default"))
                .child(
                    div()
                        .id("not-managed")
                        .child(chip("not managed", None, cx))
                        .tooltip(|window, cx| gpui_component::tooltip::Tooltip::new("Your existing Claude install. This app shows it but never changes it.").build(window, cx)),
                ),
        );
        if !d_acct.is_null() && !code_acct.is_null() && !merged {
            card = card.child(secondary("Claude Desktop and Claude Code are signed in to different accounts here.", cx));
        }

        if !desktop.is_null() {
            let running = desktop["running"] == true;
            let label = if merged { account_label(code_acct).unwrap_or_default() } else { desktop_account_label(d_acct) };
            let unmatched = !merged && !d_acct.is_null() && d_acct["matched"].is_null();
            let uuid = d_acct["accountUuid"].as_str().map(String::from);
            let nickname = d_acct["nickname"].as_str().map(String::from);
            let data_dir = desktop["dataDir"].as_str().unwrap_or("").to_string();
            let mut entries = vec![item("Open", |this, _, cx| this.act("Opening Claude…", &["open-default-desktop"], cx))];
            if let (true, Some(u)) = (unmatched, uuid.clone()) {
                let n = nickname.clone();
                entries.push(item(if n.is_none() { "Name this account…" } else { "Rename this account…" }, move |this, window, cx| {
                    this.name_account(u.clone(), n.clone(), window, cx)
                }));
            }
            entries.push(Entry::Sep);
            entries.push(item("Show data folder", move |_, _, cx| cx.open_with_system(Path::new(&data_dir))));

            card = card.child(
                div()
                    .h_flex()
                    .gap_1p5()
                    .child(kind_tag(true, merged, cx))
                    .child(div().min_w_0().truncate().child(label))
                    .when_some(uuid.filter(|_| unmatched), |d, u| {
                        let n = nickname.clone();
                        d.child(
                            icon_button("name-default", "pencil", "Name this account (Claude Desktop only records its ID)", cx)
                                .xsmall()
                                .on_click(cx.listener(move |this, _, window, cx| this.name_account(u.clone(), n.clone(), window, cx))),
                        )
                    })
                    .child(div().flex_1())
                    .when(running, |d| {
                        d.child(secondary(
                            format!("{} · {:.0}%", memory(desktop["memoryMB"].as_u64().unwrap_or(0)), desktop["cpu"].as_f64().unwrap_or(0.0)),
                            cx,
                        ))
                    })
                    .when(merged && !sessions.is_empty(), |d| d.child(chip(format!("{} running", sessions.len()), Some(green()), cx)))
                    .child(
                        quiet("open-default", if d_acct.is_null() && !running { "Sign in" } else { "Open" }, cx)
                            .on_click(cx.listener(|this, _, _, cx| this.act("Opening Claude…", &["open-default-desktop"], cx))),
                    )
                    .child(self.menu(icon_button("menu-default-app", "ellipsis", "More for this part", cx), entries, cx)),
            );
        }

        if !merged {
            let label = account_label(code_acct);
            let entries = Self::cli_entries("default", !code_acct.is_null(), code["configDir"].as_str().map(String::from), label.clone());
            card = card.child(
                div()
                    .h_flex()
                    .gap_1p5()
                    .child(kind_tag(false, true, cx))
                    .child(div().min_w_0().truncate().child(label.unwrap_or_else(|| "Claude Code: not signed in".into())))
                    .child(div().flex_1())
                    .when(!sessions.is_empty(), |d| d.child(chip(format!("{} running", sessions.len()), Some(green()), cx)))
                    .when(code_acct.is_null(), |d| {
                        d.child(quiet("login-default", "Sign in", cx).on_click(cx.listener(|this, _, _, cx| this.sign_in("default", cx))))
                    })
                    .child(self.menu(icon_button("menu-default-cli", "ellipsis", "More for this part", cx), entries, cx)),
            );
        }

        let owner = code_acct["accountUuid"].as_str().and_then(|u| self.first_owner(u)).filter(|o| o != "default");
        if let Some(owner) = owner {
            card = card.child(
                div()
                    .h_flex()
                    .gap_1p5()
                    .child(icon("link").xsmall().text_color(cx.theme().muted_foreground))
                    .child(secondary(format!("Usage: same account as {owner}"), cx).flex_1())
                    .child(
                        action("view-owner", "arrow-down", "View", cx)
                            .tooltip(format!("Scroll to {owner}'s usage"))
                            .on_click(cx.listener(move |this, _, _, cx| this.reveal(&owner, cx))),
                    ),
            );
        } else if !code_acct.is_null() {
            card = card.child(self.render_usage("default", cx));
        }
        card = card.child(self.cli_buttons("default", cx));
        if !sessions.is_empty() {
            card = card.child(self.render_sessions("default", &sessions, cx));
        }
        card
    }

    fn render_profile(&self, p: &Value, s: &Value, cx: &mut Context<Self>) -> Div {
        let name = p["name"].as_str().unwrap_or("").to_string();
        let (code, desktop) = (&p["code"], &p["desktop"]);
        let code_acct = &code["account"];
        let d_acct = &desktop["account"];
        let merged = !d_acct.is_null() && !code_acct.is_null() && d_acct["accountUuid"] == code_acct["accountUuid"];
        let sessions = sessions_of(&code["sessions"]);
        let expanded = self.expanded.contains(&name);
        let kind = match p["kind"].as_str() {
            Some("both") => "Desktop + Code",
            Some("desktop") => "Desktop",
            _ => "Code",
        };

        // The card's own menu.
        let mut entries = vec![item("Rename…", {
            let (n, running) = (name.clone(), desktop["clone"]["running"] == true);
            move |this, window, cx| {
                let new = cx.new(|cx| InputState::new(window, cx).default_value(n.clone()));
                this.screen = Some(Screen::Rename { old: n.clone(), desktop_running: running, new });
                cx.notify();
            }
        })];
        if !desktop.is_null() {
            let self_updating = desktop["updates"] == "self";
            let n = name.clone();
            entries.push(item(if self_updating { "Let the launcher keep it current" } else { "Let Claude update it itself" }, move |this, _, cx| {
                this.act("Updated…", &["self-update", &n, if self_updating { "off" } else { "on" }], cx)
            }));
            let n = name.clone();
            entries.push(item("Repair launcher", move |this, _, cx| this.act("Launcher repaired…", &["repair", &n], cx)));
            let dir = desktop["dataDir"].as_str().unwrap_or("").to_string();
            entries.push(item("Show data folder", move |_, _, cx| cx.open_with_system(Path::new(&dir))));
        }
        if let Some(dir) = code["configDir"].as_str().map(String::from) {
            entries.push(item("Show config folder", move |_, _, cx| cx.open_with_system(Path::new(&dir))));
        }
        if !code_acct.is_null() {
            let (n, who) = (name.clone(), account_label(code_acct).unwrap_or_else(|| "its account".into()));
            entries.push(Entry::Sep);
            entries.push(item("Sign out of Claude Code (CLI)…", move |this, _, cx| {
                this.confirm(
                    format!("Sign {n}'s Claude Code out?"),
                    format!("Its CLI signs out of {who}. Its folder, chats and settings stay, and the App part is not touched. Sign in again any time."),
                    "Sign Out",
                    true,
                    &["sign-out", &n],
                    format!("Signed out {n}'s CLI…"),
                    cx,
                )
            }));
        }
        entries.push(Entry::Sep);
        let has = (!desktop.is_null(), !code.is_null());
        if has.0 && has.1 {
            for (label, part) in [("Remove only the App part…", "desktop"), ("Remove only the CLI part…", "code")] {
                let n = name.clone();
                entries.push(item(label, move |this, _, cx| {
                    this.screen =
                        Some(Screen::Remove { name: n.clone(), part: Some(part), desktop: has.0, code: has.1, delete_desktop: false, delete_code: false });
                    cx.notify();
                }));
            }
        }
        {
            let n = name.clone();
            entries.push(item("Remove…", move |this, _, cx| {
                this.screen = Some(Screen::Remove { name: n.clone(), part: None, desktop: has.0, code: has.1, delete_desktop: false, delete_code: false });
                cx.notify();
            }));
        }

        let toggle = {
            let n = name.clone();
            icon_button(id(format!("details-{name}")), if expanded { "chevron-up" } else { "chevron-down" }, "Details", cx).on_click(cx.listener(
                move |this, _, _, cx| {
                    if !this.expanded.remove(&n) {
                        this.expanded.insert(n.clone());
                        this.load_sizes(cx);
                    }
                    cx.notify();
                },
            ))
        };
        let mut card = self.card(&name, cx).child(
            div()
                .h_flex()
                .gap_2()
                .child(dot(named(desktop["color"].as_str(), cx)))
                .child(div().text_base().font_semibold().child(name.clone()))
                .child(chip(kind, None, cx))
                .child(div().flex_1())
                .child(toggle)
                .child(self.menu(icon_button(id(format!("menu-{name}")), "circle-ellipsis", "More", cx), entries, cx)),
        );

        if !desktop.is_null() {
            card = card.child(self.render_desktop_part(&name, desktop, code, merged, sessions.len(), cx));
        }
        if !code.is_null() {
            if !merged {
                let label = account_label(code_acct);
                let entries = Self::cli_entries(&name, !code_acct.is_null(), code["configDir"].as_str().map(String::from), label.clone());
                let n = name.clone();
                card = card.child(
                    div()
                        .h_flex()
                        .gap_1p5()
                        .child(kind_tag(false, true, cx))
                        .child(div().min_w_0().truncate().child(label.unwrap_or_else(|| "Not signed in".into())))
                        .child(div().flex_1())
                        .when(!sessions.is_empty(), |d| d.child(chip(format!("{} running", sessions.len()), Some(green()), cx)))
                        .when(code_acct.is_null(), |d| {
                            d.child(quiet(id(format!("login-{name}")), "Sign in", cx).on_click(cx.listener(move |this, _, _, cx| this.sign_in(&n, cx))))
                        })
                        .child(self.menu(icon_button(id(format!("menu-cli-{name}")), "ellipsis", "More for this part", cx), entries, cx)),
                );
            }
            if !code_acct.is_null() {
                card = card.child(self.render_usage(&name, cx));
                if let Some(owner) = code_acct["accountUuid"].as_str().and_then(|u| self.first_owner(u)).filter(|o| *o != name) {
                    card = card.child(
                        div()
                            .h_flex()
                            .gap_1p5()
                            .child(icon("link").xsmall().text_color(cx.theme().muted_foreground))
                            .child(secondary(format!("Same account as {owner} CLI, so the same usage"), cx)),
                    );
                }
            }
            card = card.child(self.cli_buttons(&name, cx));
            if !sessions.is_empty() && expanded {
                card = card.child(self.render_sessions(&name, &sessions, cx));
            }
        }
        if expanded {
            card = card.child(self.render_details(&name, p, s, cx));
        }
        card
    }

    fn render_desktop_part(&self, name: &str, d: &Value, code: &Value, merged: bool, cli_running: usize, cx: &mut Context<Self>) -> Div {
        let clone = &d["clone"];
        let running = clone["running"] == true;
        let acct = &d["account"];
        let label = if merged { account_label(&code["account"]).unwrap_or_default() } else { desktop_account_label(acct) };
        let unmatched = !merged && !acct.is_null() && acct["matched"].is_null();
        let uuid = acct["accountUuid"].as_str().map(String::from);
        let nickname = acct["nickname"].as_str().map(String::from);
        let n = name.to_string();

        let mut entries = Vec::new();
        if merged {
            entries.push(Entry::Label("App".into()));
        }
        entries.push(item(if running { "Show" } else { "Open" }, {
            let n = n.clone();
            move |this, _, cx| this.act(format!("Opening {n}…"), &["open-desktop", &n], cx)
        }));
        if running {
            let n = n.clone();
            entries.push(item("Quit", move |this, _, cx| this.act("Quit…", &["quit-desktop", &n], cx)));
        }
        if let (true, Some(u)) = (unmatched, uuid.clone()) {
            let nk = nickname.clone();
            entries.push(item(if nk.is_none() { "Name this account…" } else { "Rename this account…" }, move |this, window, cx| {
                this.name_account(u.clone(), nk.clone(), window, cx)
            }));
        }
        if !acct.is_null() {
            let n = n.clone();
            entries.push(item("Sign out…", move |this, _, cx| {
                // Claude Desktop's sign-in is its own; this app cannot end it.
                this.confirm(
                    "Sign out in Claude Desktop".into(),
                    format!("Claude opens for {n}. Click your name at the bottom of its sidebar and choose Log out."),
                    "Open Claude",
                    false,
                    &["open-desktop", &n],
                    format!("Opening {n}…"),
                    cx,
                )
            }));
        }
        entries.push(Entry::Sep);
        let app_path = d["appPath"].as_str().unwrap_or("").to_string();
        entries.push(item("Show launcher", move |_, _, cx| cx.reveal_path(Path::new(&app_path))));
        if merged {
            entries.push(Entry::Sep);
            entries.push(Entry::Label("CLI".into()));
            entries.extend(Self::cli_entries(name, true, code["configDir"].as_str().map(String::from), account_label(&code["account"])));
        }

        let mut part = div().v_flex().gap_1().child(
            div()
                .h_flex()
                .gap_1p5()
                .child(kind_tag(true, merged, cx))
                .child(div().min_w_0().truncate().child(label))
                .when_some(uuid.filter(|_| unmatched), |row, u| {
                    let nk = nickname.clone();
                    row.child(
                        icon_button(id(format!("name-{name}")), "pencil", "Name this account (Claude Desktop only records its ID)", cx)
                            .xsmall()
                            .on_click(cx.listener(move |this, _, window, cx| this.name_account(u.clone(), nk.clone(), window, cx))),
                    )
                })
                .child(div().flex_1())
                .when(running, |row| {
                    let n = n.clone();
                    row.child(quiet(id(format!("quit-{name}")), "Quit", cx).on_click(cx.listener(move |this, _, _, cx| this.act("Quit…", &["quit-desktop", &n], cx))))
                })
                .child({
                    let n = n.clone();
                    quiet(id(format!("open-{name}")), if running { "Show" } else if acct.is_null() { "Sign in" } else { "Open" }, cx)
                        .on_click(cx.listener(move |this, _, _, cx| this.act(format!("Opening {n}…"), &["open-desktop", &n], cx)))
                })
                .child(self.menu(icon_button(id(format!("menu-app-{name}")), "ellipsis", "More for this part", cx), entries, cx)),
        );
        // Stats on their own line: beside the buttons they squeeze the name.
        let cli_here = if merged { cli_running } else { 0 };
        if running || cli_here > 0 {
            part = part.child(
                div()
                    .h_flex()
                    .gap_1p5()
                    .when(running, |r| {
                        r.child(secondary(
                            format!("App {} · {:.0}% CPU", memory(clone["memoryMB"].as_u64().unwrap_or(0)), clone["cpu"].as_f64().unwrap_or(0.0)),
                            cx,
                        ))
                    })
                    .when(cli_here > 0, |r| r.child(chip(format!("{cli_here} CLI running"), Some(green()), cx))),
            );
        }
        if clone["wrongAccountPids"].as_array().is_some_and(|w| !w.is_empty()) {
            part = part.child(warning("Running without its profile, so it shows your default account. Quit it and open it here."));
        }
        if d["launcherExists"] == false {
            part = part.child(warning("Its launcher is missing. Run Repair or doctor --fix."));
        }
        part
    }

    fn render_details(&self, name: &str, p: &Value, s: &Value, cx: &App) -> Div {
        let size = self.size_for(name);
        let mb = |k: &str| size[k].as_u64().map(|m| format!(" · {}", memory(m))).unwrap_or_default();
        let mut rows: Vec<(String, String)> = Vec::new();
        let d = &p["desktop"];
        if !d.is_null() {
            let c = &d["clone"];
            let installed = s["claude"]["desktopApp"]["version"].as_str().unwrap_or("?");
            let v = c["version"].as_str().unwrap_or("?");
            let copy = if c["exists"] != true {
                "missing (doctor --fix builds it)".to_string()
            } else {
                match c["state"].as_str() {
                    Some("behind") => format!("{v}, updates to {installed} on next open"),
                    Some("ahead") => format!("{v}, newer than installed {installed}"),
                    _ => format!("{v}, current"),
                }
            };
            rows.push(("Claude copy".into(), copy));
            let updates = match d["updates"].as_str() {
                Some("launcher") => "kept current by its launcher",
                Some("self") => "Claude updates it itself",
                Some("own-config") => "its own Claude configuration decides",
                _ => "not set up yet (doctor --fix)",
            };
            rows.push(("Updates".into(), updates.into()));
            if !d["color"].is_null() && c["colorIntact"] == false {
                rows.push(("Colour".into(), "lost after an update; doctor --fix restores it".into()));
            }
            rows.push(("Data".into(), short_path(d["dataDir"].as_str().unwrap_or("")) + &mb("desktopDataMB")));
            rows.push(("Launcher".into(), short_path(d["appPath"].as_str().unwrap_or(""))));
        }
        let c = &p["code"];
        if !c.is_null() {
            rows.push(("Config".into(), short_path(c["configDir"].as_str().unwrap_or("")) + &mb("codeConfigMB")));
            rows.push(("Command".into(), c["alias"].as_str().unwrap_or("").into()));
            if let Some(gh) = c["ghConfigDir"].as_str() {
                rows.push(("GitHub CLI".into(), short_path(gh)));
            }
            if let Some(plan) = c["account"]["plan"].as_str() {
                rows.push(("Plan".into(), plan.into()));
            }
        }
        if let Some(created) = p["createdAt"].as_str().filter(|c| !c.is_empty()) {
            rows.push(("Created".into(), created.chars().take(10).collect()));
        }
        let mut out = div().v_flex().gap_1().pt_2().border_t_1().border_color(cx.theme().foreground.opacity(0.08)).text_xs();
        for (k, v) in rows {
            out = out.child(
                div()
                    .h_flex()
                    .items_start()
                    .gap_2()
                    .child(div().w(px(76.)).flex_none().text_color(cx.theme().muted_foreground).child(k))
                    .child(div().min_w_0().child(v)),
            );
        }
        out
    }

    // ---- Running and Files tabs -----------------------------------------------------------

    fn render_running(&self, s: &Value, cx: &mut Context<Self>) -> Vec<Div> {
        let mut desktops = self.card("running-desktop", cx).child(div().font_semibold().child("Claude Desktop"));
        let mut open: Vec<(String, Option<String>, String, u64, f64)> = Vec::new();
        let dd = &s["default"]["desktop"];
        if dd["running"] == true {
            open.push(("default".into(), None, desktop_account_label(&dd["account"]), dd["memoryMB"].as_u64().unwrap_or(0), dd["cpu"].as_f64().unwrap_or(0.0)));
        }
        for p in s["profiles"].as_array().into_iter().flatten() {
            let d = &p["desktop"];
            if d["clone"]["running"] == true {
                open.push((
                    p["name"].as_str().unwrap_or("").into(),
                    d["color"].as_str().map(String::from),
                    desktop_account_label(&d["account"]),
                    d["clone"]["memoryMB"].as_u64().unwrap_or(0),
                    d["clone"]["cpu"].as_f64().unwrap_or(0.0),
                ));
            }
        }
        if open.is_empty() {
            desktops = desktops.child(secondary("No Claude Desktop windows open.", cx));
        }
        for (name, color, who, mem, cpu) in open {
            let is_default = name == "default";
            let n = name.clone();
            desktops = desktops.child(
                div()
                    .h_flex()
                    .gap_1p5()
                    .child(dot(named(color.as_deref(), cx)).size(px(8.)))
                    .child(div().font_weight(FontWeight::MEDIUM).child(if is_default { "Default".to_string() } else { name.clone() }))
                    .child(secondary(who, cx).min_w_0().truncate())
                    .child(div().flex_1())
                    .child(secondary(format!("{} · {cpu:.0}%", memory(mem)), cx))
                    .when(!is_default, |r| {
                        let n = n.clone();
                        r.child(quiet(id(format!("rq-{name}")), "Quit", cx).on_click(cx.listener(move |this, _, _, cx| this.act("Quit…", &["quit-desktop", &n], cx))))
                    })
                    .child(quiet(id(format!("rs-{name}")), "Show", cx).on_click(cx.listener(move |this, _, _, cx| {
                        if n == "default" {
                            this.act("Opening…", &["open-default-desktop"], cx)
                        } else {
                            this.act("Opening…", &["open-desktop", &n], cx)
                        }
                    }))),
            );
        }

        let mut groups: Vec<(String, String, Option<String>, Vec<Value>)> = Vec::new();
        let dc = &s["default"]["code"];
        groups.push(("default".into(), "Default".into(), account_label(&dc["account"]), sessions_of(&dc["sessions"])));
        for p in s["profiles"].as_array().into_iter().flatten() {
            let name = p["name"].as_str().unwrap_or("").to_string();
            groups.push((name.clone(), name, account_label(&p["code"]["account"]), sessions_of(&p["code"]["sessions"])));
        }
        for o in s["otherSessions"].as_array().into_iter().flatten() {
            let dir = o["configDir"].as_str().unwrap_or("").to_string();
            groups.push((format!("other-{dir}"), short_path(&dir), Some("not one of your profiles".into()), sessions_of(&o["sessions"])));
        }
        groups.retain(|g| !g.3.is_empty());
        let mut cli = self.card("running-cli", cx).child(div().font_semibold().child("Claude Code"));
        if groups.is_empty() {
            cli = cli.child(secondary("No Claude Code sessions running.", cx));
        }
        for (key, title, who, sessions) in groups {
            cli = cli.child(
                div()
                    .v_flex()
                    .gap_1()
                    .child(
                        div()
                            .h_flex()
                            .gap_1()
                            .child(div().text_xs().font_weight(FontWeight::MEDIUM).child(title))
                            .children(who.map(|w| secondary(w, cx).min_w_0().truncate())),
                    )
                    .child(self.render_sessions(&format!("run-{key}"), &sessions, cx)),
            );
        }
        vec![desktops, cli]
    }

    fn file_row(&self, label: &str, path: &str, mb: Option<u64>, key: String, cx: &mut Context<Self>) -> Div {
        let p = path.to_string();
        div()
            .h_flex()
            .gap_1p5()
            .text_xs()
            .child(div().w(px(82.)).flex_none().text_color(cx.theme().muted_foreground).child(label.to_string()))
            .child(div().flex_1().min_w_0().truncate().child(short_path(path)))
            .children(mb.map(|m| secondary(memory(m), cx)))
            .child(icon_button(id(key), "folder", "Show in file manager", cx).xsmall().on_click(move |_, _, cx| cx.reveal_path(Path::new(&p))))
    }

    fn render_files(&self, s: &Value, cx: &mut Context<Self>) -> Vec<Div> {
        let mut out = Vec::new();
        if let Some(sz) = &self.sizes {
            // Everything but the Claude copies, which share their blocks with
            // Claude.app and take next to nothing.
            let total: u64 = std::iter::once(&sz["default"])
                .chain(sz["profiles"].as_array().into_iter().flatten())
                .map(|row| row["desktopDataMB"].as_u64().unwrap_or(0) + row["codeConfigMB"].as_u64().unwrap_or(0))
                .sum();
            out.push(secondary(format!("{} on disk in all", memory(total)), cx).mb_2());
        } else {
            out.push(secondary("Measuring folders…", cx).mb_2());
        }
        let size = self.size_for("default");
        let mut def = self.card("files-default", cx).child(div().font_semibold().child("Default"));
        if let Some(d) = s["default"]["desktop"]["dataDir"].as_str() {
            def = def.child(self.file_row("Desktop data", d, size["desktopDataMB"].as_u64(), "f-default-d".into(), cx));
        }
        if let Some(c) = s["default"]["code"]["configDir"].as_str() {
            def = def.child(self.file_row("Code config", c, size["codeConfigMB"].as_u64(), "f-default-c".into(), cx));
        }
        out.push(def);
        for p in s["profiles"].as_array().cloned().unwrap_or_default() {
            let name = p["name"].as_str().unwrap_or("").to_string();
            let size = self.size_for(&name);
            let mut card = self.card(&format!("files-{name}"), cx).child(
                div()
                    .h_flex()
                    .gap_1p5()
                    .child(dot(named(p["desktop"]["color"].as_str(), cx)).size(px(8.)))
                    .child(div().font_semibold().child(name.clone())),
            );
            let d = &p["desktop"];
            if !d.is_null() {
                card = card
                    .child(self.file_row("Desktop data", d["dataDir"].as_str().unwrap_or(""), size["desktopDataMB"].as_u64(), format!("f-{name}-d"), cx))
                    .child(self.file_row("Claude copy", d["clone"]["path"].as_str().unwrap_or(""), size["cloneMB"].as_u64(), format!("f-{name}-k"), cx))
                    .child(self.file_row("Launcher", d["appPath"].as_str().unwrap_or(""), None, format!("f-{name}-l"), cx));
            }
            let c = &p["code"];
            if !c.is_null() {
                card = card.child(self.file_row("Code config", c["configDir"].as_str().unwrap_or(""), size["codeConfigMB"].as_u64(), format!("f-{name}-c"), cx));
                if let Some(gh) = c["ghConfigDir"].as_str() {
                    card = card.child(self.file_row("GitHub CLI", gh, None, format!("f-{name}-g"), cx));
                }
            }
            out.push(card);
        }
        out
    }

    // ---- Screens --------------------------------------------------------------------------

    fn screen_frame(&self, title: String, cx: &mut Context<Self>) -> Div {
        div().v_flex().gap_3().child(
            div()
                .h_flex()
                .gap_1()
                .child(icon_button("back", "chevron-left", "Back", cx).on_click(cx.listener(|this, _, _, cx| this.back(cx))))
                .child(div().text_lg().font_semibold().child(title)),
        )
    }

    fn screen_buttons(
        &self,
        ok: &str,
        danger: bool,
        enabled: bool,
        on_ok: impl Fn(&mut Profiles, &mut Context<Profiles>) + 'static,
        cx: &mut Context<Self>,
    ) -> Div {
        div()
            .h_flex()
            .gap_2()
            .mt_1()
            .child(div().flex_1())
            .child(quiet("cancel", "Cancel", cx).on_click(cx.listener(|this, _, _, cx| this.back(cx))))
            .child(confirm("ok", ok, danger, cx).disabled(!enabled).on_click(cx.listener(move |this, _, _, cx| on_ok(this, cx))))
    }

    fn render_screen(&mut self, cx: &mut Context<Self>) -> Div {
        match self.screen.as_ref() {
            Some(Screen::Add { .. }) => self.render_add(cx),
            Some(Screen::Rename { old, desktop_running, new }) => {
                let (old, running, new) = (old.clone(), *desktop_running, new.clone());
                self.screen_frame(format!("Rename {old}"), cx)
                    .child(Input::new(&new))
                    .child(secondary("Its folders, command and launcher move to match. Chats, skills, settings and its Claude Code sign-in move with it.", cx))
                    .when(running, |d| d.child(warning("Quit its Claude Desktop window first.")))
                    .child(self.screen_buttons(
                        "Rename",
                        false,
                        !running,
                        move |this, cx| {
                            let n = profile_name(&new.read(cx).value());
                            if !n.is_empty() && n != old {
                                this.act(format!("Renamed {old} to {n}…"), &["rename", &old, &n], cx);
                            }
                            this.back(cx);
                        },
                        cx,
                    ))
            }
            Some(Screen::Remove { name, part, desktop, code, delete_desktop, delete_code }) => {
                let (name, part, desktop, code, dd, dc) = (name.clone(), *part, *desktop, *code, *delete_desktop, *delete_code);
                let title = match part {
                    Some("desktop") => format!("Remove {name}'s App part?"),
                    Some(_) => format!("Remove {name}'s CLI part?"),
                    None => format!("Remove {name}?"),
                };
                let text = match part {
                    Some("desktop") => "Removes its launcher and its copy of Claude. Its Claude Code part stays.",
                    Some(_) => "Removes its terminal command. Its Claude Desktop part stays, and its launcher stops pointing Desktop's Code tab at this folder.",
                    None => "Removes its launcher, its copy of Claude, its terminal command and its entry, plus the data ticked below.",
                };
                let show_code = code && part != Some("desktop");
                let show_desktop = desktop && part != Some("code");
                self.screen_frame(title, cx)
                    .child(secondary(text, cx))
                    .when(show_code, |d| {
                        d.child(Checkbox::new("del-code").label("Also delete its Claude Code folder (settings, history, sign-in)").checked(dc).on_click(
                            cx.listener(|this, v: &bool, _, cx| {
                                if let Some(Screen::Remove { delete_code, .. }) = &mut this.screen {
                                    *delete_code = *v;
                                }
                                cx.notify();
                            }),
                        ))
                    })
                    .when(show_desktop, |d| {
                        d.child(Checkbox::new("del-desktop").label("Also delete its Claude Desktop data (chats cache, sign-in)").checked(dd).on_click(
                            cx.listener(|this, v: &bool, _, cx| {
                                if let Some(Screen::Remove { delete_desktop, .. }) = &mut this.screen {
                                    *delete_desktop = *v;
                                }
                                cx.notify();
                            }),
                        ))
                    })
                    .child(self.screen_buttons(
                        "Remove",
                        true,
                        true,
                        move |this, cx| {
                            match part {
                                Some(p) => {
                                    let wipe = if p == "desktop" { dd } else { dc };
                                    let mut args = vec!["remove-part", name.as_str(), p];
                                    if wipe {
                                        args.push("--delete-data");
                                    }
                                    this.act(format!("Removed {name}'s {} part…", if p == "desktop" { "App" } else { "CLI" }), &args, cx);
                                }
                                None => {
                                    let mut args = vec!["remove", name.as_str()];
                                    if dd {
                                        args.push("--delete-desktop-data");
                                    }
                                    if dc {
                                        args.push("--delete-code-data");
                                    }
                                    this.act(format!("Removed {name}…"), &args, cx);
                                }
                            }
                            this.back(cx);
                        },
                        cx,
                    ))
            }
            Some(Screen::NameAccount { uuid, name }) => {
                let (uuid, name) = (uuid.clone(), name.clone());
                self.screen_frame("Name this Claude Desktop account".into(), cx)
                    .child(secondary(
                        format!(
                            "Claude Desktop keeps only the account's ID ({}…) where this app can read it. Give it the name you know it by. Leave empty to clear.",
                            uuid.chars().take(8).collect::<String>()
                        ),
                        cx,
                    ))
                    .child(Input::new(&name))
                    .child(self.screen_buttons(
                        "Save",
                        false,
                        true,
                        move |this, cx| {
                            let n = name.read(cx).value().to_string();
                            this.act("Saved…", &["name-account", &uuid, &n], cx);
                            this.back(cx);
                        },
                        cx,
                    ))
            }
            Some(Screen::Import) => self.render_import(cx),
            Some(Screen::Confirm { title, text, button, danger, args, done }) => {
                let (title, text, button, danger, args, done) = (title.clone(), text.clone(), button.clone(), *danger, args.clone(), done.clone());
                self.screen_frame(title, cx).child(secondary(text, cx)).child(self.screen_buttons(
                    &button,
                    danger,
                    true,
                    move |this, cx| {
                        this.call(Job::Action(done.clone()), args.clone(), cx);
                        this.back(cx);
                    },
                    cx,
                ))
            }
            None => div(),
        }
    }

    fn render_add(&mut self, cx: &mut Context<Self>) -> Div {
        let Some(Screen::Add { name, targets, seed, gh, color, .. }) = &self.screen else { return div() };
        let (name, targets, seed, gh, color) = (name.clone(), *targets, *seed, *gh, color.clone());
        let mac = self.is_mac();
        let state = self.state.clone().unwrap_or_default();
        let typed = profile_name(&name.read(cx).value());

        let mut form = self
            .screen_frame("Add an account".into(), cx)
            .child(secondary("A profile is a separate sign-in with its own settings and history, running beside your default one.", cx))
            .child(Input::new(&name))
            .child(secondary(
                format!("Lowercase letters, numbers and hyphens. Becomes the command claude-{}.", if typed.is_empty() { "name" } else { &typed }),
                cx,
            ));

        // Which Claude: Desktop only where it exists.
        let options: Vec<(&'static str, String)> = if mac {
            vec![("both", "Both".into()), ("desktop", "Desktop".into()), ("code", "Code".into())]
        } else {
            vec![("code", "Claude Code".into())]
        };
        form = form.child(self.segmented(
            "target",
            options,
            targets,
            |this, value, _| {
                if let Some(Screen::Add { targets, .. }) = &mut this.screen {
                    *targets = value;
                }
            },
            cx,
        ));
        if !mac {
            form = form.child(secondary("Claude Desktop profiles need macOS; Claude Code profiles work everywhere.", cx));
        }

        if mac && targets != "code" {
            let mut swatches = div().h_flex().gap_2().child(secondary("Dock colour", cx).w(px(76.)));
            let colors =
                std::iter::once(String::new()).chain(state["colors"].as_array().into_iter().flatten().filter_map(|c| c.as_str().map(String::from)));
            for c in colors {
                let selected = c == color;
                let tint = named(if c.is_empty() { None } else { Some(c.as_str()) }, cx);
                let key = if c.is_empty() { "none".to_string() } else { c.clone() };
                swatches = swatches.child(
                    div()
                        .id(id(format!("swatch-{key}")))
                        .size(px(20.))
                        .rounded_full()
                        .p(px(2.))
                        .border_2()
                        .border_color(if selected { cx.theme().foreground.opacity(0.7) } else { gpui::transparent_black() })
                        .cursor_pointer()
                        .child(div().size_full().rounded_full().bg(if c.is_empty() { tint.opacity(0.35) } else { tint }))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if let Some(Screen::Add { color, .. }) = &mut this.screen {
                                *color = c.clone();
                            }
                            cx.notify();
                        })),
                );
            }
            form = form.child(swatches);
        }
        if targets != "desktop" {
            form = form.child(
                Checkbox::new("seed").label("Copy my Claude Code setup (settings, skills, plugins, MCP servers)").checked(seed).on_click(cx.listener(
                    |this, v: &bool, _, cx| {
                        if let Some(Screen::Add { seed, .. }) = &mut this.screen {
                            *seed = *v;
                        }
                        cx.notify();
                    },
                )),
            );
            if state["ghAvailable"] == true {
                form = form.child(Checkbox::new("gh").label("Its own GitHub CLI login").checked(gh).on_click(cx.listener(|this, v: &bool, _, cx| {
                    if let Some(Screen::Add { gh, .. }) = &mut this.screen {
                        *gh = *v;
                    }
                    cx.notify();
                })));
            }
        }
        form.child(secondary("Nothing from using your default account comes along: no chats, history or sign-in.", cx)).child(self.screen_buttons(
            "Add",
            false,
            !typed.is_empty(),
            move |this, cx| {
                let n = profile_name(&name.read(cx).value());
                if n.is_empty() {
                    return;
                }
                let mut args = vec!["add", "--name", n.as_str(), "--targets", targets];
                if seed && targets != "desktop" {
                    args.push("--seed-code");
                }
                if gh && targets != "desktop" {
                    args.push("--gh");
                }
                if !color.is_empty() && targets != "code" {
                    args.extend(["--color", color.as_str()]);
                }
                this.act(format!("Added {n}. Sign in from its card…"), &args, cx);
                this.back(cx);
            },
            cx,
        ))
    }

    fn render_import(&mut self, cx: &mut Context<Self>) -> Div {
        let mut out = self.screen_frame("Import existing accounts".into(), cx).child(secondary(
            "Claude folders already on this computer that no profile manages. Importing keeps them exactly as they are, sign-in, chats and settings included, and adds a command.",
            cx,
        ));
        if self.importable.is_empty() {
            out = out.child(secondary("Nothing left to import.", cx));
        }
        for (i, c) in self.importable.clone().into_iter().enumerate() {
            let name = c["name"].as_str().unwrap_or("").to_string();
            let mut args = vec!["import".to_string(), "--name".into(), name.clone()];
            for (key, flag) in [("codeDir", "--code-dir"), ("desktopDir", "--desktop-dir")] {
                if let Some(d) = c[key].as_str() {
                    args.extend([flag.to_string(), d.to_string()]);
                }
            }
            let mut info = div().v_flex().flex_1().min_w_0().gap_0p5().child(
                div()
                    .h_flex()
                    .gap_1p5()
                    .child(div().font_semibold().child(name.clone()))
                    .when(c["running"] == true, |d| d.child(chip("in use", Some(green()), cx))),
            );
            if let Some(who) = account_label(&c["codeAccount"]) {
                info = info.child(div().text_xs().child(who));
            }
            for key in ["codeDir", "desktopDir"] {
                if let Some(d) = c[key].as_str() {
                    info = info.child(secondary(short_path(d), cx).truncate());
                }
            }
            out = out.child(self.card(&format!("import-{i}"), cx).child(div().h_flex().gap_2().child(info).child(
                action(id(format!("import-{i}")), "download", "Import", cx).on_click(cx.listener(move |this, _, _, cx| {
                    this.call(Job::Action(format!("Imported {name}…")), args.clone(), cx);
                })),
            )));
        }
        out
    }
}

fn warning(text: &str) -> Div {
    div()
        .h_flex()
        .items_start()
        .gap_1p5()
        .text_xs()
        .text_color(orange())
        .child(icon("triangle-alert").xsmall())
        .child(div().flex_1().child(text.to_string()))
}

#[cfg(test)]
mod tests {
    #[test]
    fn typed_names_become_profile_names() {
        assert_eq!(super::profile_name("Client ACME"), "client-acme");
        assert_eq!(super::profile_name("  work__2 "), "work-2");
        assert_eq!(super::profile_name("é!"), "");
    }

    #[test]
    fn organisations_that_repeat_the_person_are_left_out() {
        let a = serde_json::json!({ "email": "me@x.com", "organization": "me@x.com's Organization" });
        assert_eq!(super::account_label(&a).as_deref(), Some("me@x.com"));
        let b = serde_json::json!({ "email": "me@x.com", "organization": "Acme" });
        assert_eq!(super::account_label(&b).as_deref(), Some("me@x.com · Acme"));
    }
}
