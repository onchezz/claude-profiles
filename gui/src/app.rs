// The window's state, the background calls that fill it, and the actions
// the cards share. Rendering lives in view.rs and the files beside it.

use crate::prelude::*;

pub(crate) const STATE_EVERY: Duration = Duration::from_secs(15);
pub(crate) const USAGE_EVERY: Duration = Duration::from_secs(300);
pub(crate) const TICK: Duration = Duration::from_secs(2);

// ---- Background calls ---------------------------------------------------------------------

pub(crate) enum Job {
    State,
    Usage,
    Sizes,
    Importable,
    CopyCommand,
    CleanupScan,
    CleanupRun,
    Sessions,
    /// A change, with what to say once it worked (empty: say nothing).
    Action(String),
}

// ---- Settings the window keeps --------------------------------------------------------

pub(crate) struct Settings {
    pub(crate) terminal: String,
}

impl Settings {
    pub(crate) fn path() -> std::path::PathBuf {
        cli::config_dir().join("gui.json")
    }

    pub(crate) fn load() -> Settings {
        let v: Value = std::fs::read_to_string(Self::path()).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
        Settings { terminal: v["terminal"].as_str().unwrap_or("").to_string() }
    }

    pub(crate) fn save(&self) {
        let p = Self::path();
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(p, serde_json::to_string_pretty(&json!({ "terminal": self.terminal })).unwrap_or_default());
    }
}

// ---- Screens ------------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Tab {
    Accounts,
    Running,
    Sessions,
    Files,
}

/// What replaces the list for a moment, as the menu bar panel's screens do.
pub(crate) enum Screen {
    Add { name: Entity<InputState>, targets: &'static str, seed: bool, gh: bool, color: String, _watch: Subscription },
    Rename { old: String, desktop_running: bool, new: Entity<InputState> },
    Remove { name: String, part: Option<&'static str>, desktop: bool, code: bool, delete_desktop: bool, delete_code: bool },
    NameAccount { uuid: String, name: Entity<InputState> },
    Import,
    Confirm { title: String, text: String, button: String, danger: bool, args: Vec<String>, done: String },
    Cleanup,
    CleanupConfirm,
}

// ---- Menus --------------------------------------------------------------------------------

pub(crate) type Act = Rc<dyn Fn(&mut Profiles, &mut Window, &mut Context<Profiles>)>;

pub(crate) enum Entry {
    Item(SharedString, Act),
    Check(SharedString, bool, Act),
    Label(SharedString),
    Sep,
}

pub(crate) fn item(label: impl Into<SharedString>, f: impl Fn(&mut Profiles, &mut Window, &mut Context<Profiles>) + 'static) -> Entry {
    Entry::Item(label.into(), Rc::new(f))
}

// ---- The view -----------------------------------------------------------------------------

pub(crate) struct Profiles {
    /// The open page's scroll position, and which page it belongs to, so
    /// each page opens at its top.
    pub(crate) screen_scroll: ScrollHandle,
    pub(crate) shown_screen: Option<std::mem::Discriminant<Screen>>,
    pub(crate) cleanup: Cleanup,
    pub(crate) history: crate::history::History,
    pub(crate) state: Option<Value>,
    pub(crate) usage: Option<Value>,
    pub(crate) sizes: Option<Value>,
    pub(crate) importable: Vec<Value>,
    pub(crate) error: Option<String>,
    pub(crate) loading_state: bool,
    pub(crate) loading_usage: bool,
    pub(crate) last_state: Option<Instant>,
    pub(crate) last_usage: Option<Instant>,
    pub(crate) busy: Vec<String>,
    pub(crate) toast: Option<(String, bool, Instant)>,
    pub(crate) settings: Settings,
    pub(crate) tab: Tab,
    pub(crate) screen: Option<Screen>,
    pub(crate) expanded: HashSet<String>,
    pub(crate) all_sessions: HashSet<String>,
    pub(crate) highlighted: Option<(String, Instant)>,
    pub(crate) scroll: ScrollHandle,
}

impl Profiles {
    pub(crate) fn new(window: &mut Window, cx: &mut Context<Self>) -> Profiles {
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
            cleanup: Cleanup::default(),
            history: crate::history::History::new(window, cx),
            screen_scroll: ScrollHandle::new(),
            shown_screen: None,
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

    pub(crate) fn tick(&mut self, cx: &mut Context<Self>) {
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

    pub(crate) fn call(&mut self, job: Job, args: Vec<String>, cx: &mut Context<Self>) {
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

    pub(crate) fn refresh(&mut self, cx: &mut Context<Self>) {
        if !self.loading_state {
            self.call(Job::State, vec!["state".into()], cx);
        }
    }

    /// Run a change; everything it may have touched is reread afterwards.
    /// `done` is the progress text, ending in "…"; once it worked, the same
    /// words without the dots say so.
    pub(crate) fn act(&mut self, done: impl Into<String>, args: &[&str], cx: &mut Context<Self>) {
        self.call(Job::Action(done.into()), args.iter().map(|s| s.to_string()).collect(), cx);
    }

    pub(crate) fn notify(&mut self, text: impl Into<String>, error: bool) {
        self.toast = Some((text.into(), error, Instant::now()));
    }

    pub(crate) fn receive(&mut self, job: Job, result: Result<Value, String>, cx: &mut Context<Self>) {
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
            Job::CleanupScan => {
                self.cleanup.scanning = false;
                match result {
                    Ok(v) => self.cleanup.take_scan(v),
                    Err(e) => self.notify(e, true),
                }
            }
            Job::CleanupRun => {
                self.cleanup.running = false;
                match result {
                    Ok(v) => {
                        self.notify(format!("Freed {}", crate::cleanup::bytes(v["freedBytes"].as_u64().unwrap_or(0))), false);
                        self.cleanup.last_result = Some(v);
                        // What is left, measured afresh.
                        self.scan_cleanup(cx);
                    }
                    Err(e) => self.notify(e, true),
                }
            }
            Job::Sessions => {
                self.history.loading = false;
                match result {
                    Ok(v) => self.history.list = Some(v),
                    Err(e) => self.notify(e, true),
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

    pub(crate) fn load_sizes(&mut self, cx: &mut Context<Self>) {
        self.call(Job::Sizes, vec!["sizes".into()], cx);
    }

    pub(crate) fn terminals(&self) -> Vec<String> {
        self.state
            .as_ref()
            .and_then(|s| s["terminals"].as_array())
            .map(|a| a.iter().filter_map(|t| t.as_str().map(String::from)).collect())
            .unwrap_or_default()
    }

    pub(crate) fn terminal(&self) -> String {
        let list = self.terminals();
        if list.contains(&self.settings.terminal) {
            self.settings.terminal.clone()
        } else {
            list.first().cloned().unwrap_or_default()
        }
    }

    pub(crate) fn is_mac(&self) -> bool {
        self.state.as_ref().is_some_and(|s| s["isMac"].as_bool() == Some(true))
    }

    pub(crate) fn usage_for(&self, name: &str) -> Option<Value> {
        let u = self.usage.as_ref()?;
        if name == "default" {
            return Some(u["default"].clone());
        }
        u["profiles"].as_array()?.iter().find(|p| p["name"] == name).map(|p| p["usage"].clone())
    }

    pub(crate) fn size_for(&self, name: &str) -> Value {
        let Some(s) = &self.sizes else { return Value::Null };
        if name == "default" {
            return s["default"].clone();
        }
        s["profiles"].as_array().and_then(|a| a.iter().find(|p| p["name"] == name).cloned()).unwrap_or(Value::Null)
    }

    /// The first card signed in to this Claude Code account: profiles first,
    /// since the default install is the one people tend to share.
    pub(crate) fn first_owner(&self, uuid: &str) -> Option<String> {
        let s = self.state.as_ref()?;
        for p in s["profiles"].as_array().into_iter().flatten() {
            if p["code"]["account"]["accountUuid"].as_str() == Some(uuid) {
                return p["name"].as_str().map(String::from);
            }
        }
        (s["default"]["code"]["account"]["accountUuid"].as_str() == Some(uuid)).then(|| "default".to_string())
    }

    pub(crate) fn running_count(&self) -> usize {
        let Some(s) = &self.state else { return 0 };
        let mut n = sessions_of(&s["default"]["code"]["sessions"]).len() + usize::from(s["default"]["desktop"]["running"] == true);
        for p in s["profiles"].as_array().into_iter().flatten() {
            n += sessions_of(&p["code"]["sessions"]).len() + usize::from(p["desktop"]["clone"]["running"] == true);
        }
        n + s["otherSessions"].as_array().into_iter().flatten().map(|o| sessions_of(&o["sessions"]).len()).sum::<usize>()
    }

    // ---- Actions the cards share ----------------------------------------------------------

    pub(crate) fn open_cli(&mut self, target: &str, in_folder: bool, cx: &mut Context<Self>) {
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

    pub(crate) fn sign_in(&mut self, target: &str, cx: &mut Context<Self>) {
        let term = self.terminal();
        self.act(format!("Opening {term} to sign in…"), &["open-cli", target, "--login", "--terminal", &term], cx);
    }

    pub(crate) fn copy_command(&mut self, target: &str, cx: &mut Context<Self>) {
        self.call(Job::CopyCommand, vec!["cli-command".into(), target.into()], cx);
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn confirm(&mut self, title: String, text: String, button: &str, danger: bool, args: &[&str], done: String, cx: &mut Context<Self>) {
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

    pub(crate) fn name_account(&mut self, uuid: String, current: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        let name = cx.new(|cx| InputState::new(window, cx).placeholder("e.g. Collins").default_value(current.unwrap_or_default()));
        self.screen = Some(Screen::NameAccount { uuid, name });
        cx.notify();
    }

    /// Scroll to a profile's card and light it up for a moment.
    pub(crate) fn reveal(&mut self, owner: &str, cx: &mut Context<Self>) {
        let Some(s) = &self.state else { return };
        let ix = s["profiles"].as_array().and_then(|a| a.iter().position(|p| p["name"] == owner)).map(|i| i + 1).unwrap_or(0);
        self.tab = Tab::Accounts;
        self.scroll.scroll_to_top_of_item(ix);
        self.highlighted = Some((owner.to_string(), Instant::now()));
        cx.notify();
    }

    pub(crate) fn open_add(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let name = cx.new(|cx| InputState::new(window, cx).placeholder("work"));
        let watch = cx.subscribe(&name, |_, _, _: &InputEvent, cx| cx.notify());
        let targets = if self.is_mac() { "both" } else { "code" };
        self.screen = Some(Screen::Add { name, targets, seed: true, gh: false, color: String::new(), _watch: watch });
        cx.notify();
    }

    pub(crate) fn back(&mut self, cx: &mut Context<Self>) {
        // The cleanup confirmation returns to the cleanup page; everything
        // else to the tabs.
        self.screen = match self.screen {
            Some(Screen::CleanupConfirm) => Some(Screen::Cleanup),
            _ => None,
        };
        cx.notify();
    }
}
