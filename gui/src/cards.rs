// The Accounts tab: the Default card and one card per profile.

use crate::prelude::*;

impl Profiles {
    // ---- Accounts tab ---------------------------------------------------------------------

    pub(crate) fn cli_buttons(&self, target: &str, cx: &mut Context<Self>) -> Div {
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

    pub(crate) fn cli_entries(target: &str, signed_in: bool, config_dir: Option<String>, label: Option<String>) -> Vec<Entry> {
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

    pub(crate) fn render_usage(&self, target: &str, cx: &mut Context<Self>) -> Div {
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
        // The itemised list when the command has it (it names model-scoped
        // allowances); the fixed fields otherwise.
        let mut bars: Vec<(String, Value)> = Vec::new();
        for l in u["limits"].as_array().into_iter().flatten() {
            let label = match (l["kind"].as_str(), l["model"].as_str()) {
                (Some("session"), _) => "5 hours".to_string(),
                (Some("weekly_all"), _) => "Week".to_string(),
                (_, Some(model)) => format!("{model} week"),
                (Some(other), None) => other.replace('_', " "),
                _ => continue,
            };
            bars.push((label, l.clone()));
        }
        if bars.is_empty() {
            for (key, label) in [("fiveHour", "5 hours"), ("sevenDay", "Week"), ("sevenDayOpus", "Opus week"), ("sevenDaySonnet", "Sonnet week")] {
                if u[key].is_object() {
                    bars.push((label.to_string(), u[key].clone()));
                }
            }
        }
        for (label, l) in bars {
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
                    .child(div().w(px(132.)).flex_none().whitespace_nowrap().text_color(cx.theme().muted_foreground).child(resets(&l["resetsAt"]))),
            );
        }
        // Limit resets and extra usage are offered on claude.ai, for the
        // account signed in there; this app can only read the numbers.
        out.child(
            div()
                .id(id(format!("usage-web-{target}")))
                .h_flex()
                .gap_1()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .cursor_pointer()
                .hover(|d| d.text_color(accent()))
                .child(icon("external-link").xsmall())
                .child("Limit resets and extra usage on claude.ai")
                .on_click(|_, _, cx| cx.open_url("https://claude.ai/settings/usage")),
        )
    }

    pub(crate) fn render_sessions(&self, key: &str, sessions: &[Value], cx: &mut Context<Self>) -> Div {
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

    pub(crate) fn render_default(&self, s: &Value, cx: &mut Context<Self>) -> Div {
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
                    .when(!sessions_of(&code["desktopSessions"]).is_empty(), |d| {
                        d.child(desktop_tasks("tasks-default", sessions_of(&code["desktopSessions"]).len(), cx))
                    })
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

    pub(crate) fn render_profile(&self, p: &Value, s: &Value, cx: &mut Context<Self>) -> Div {
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

    pub(crate) fn render_desktop_part(&self, name: &str, d: &Value, code: &Value, merged: bool, cli_running: usize, cx: &mut Context<Self>) -> Div {
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
        let from_app = sessions_of(&code["desktopSessions"]).len();
        if running || cli_here > 0 || from_app > 0 {
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
                    .when(cli_here > 0, |r| r.child(chip(format!("{cli_here} CLI running"), Some(green()), cx)))
                    .when(from_app > 0, |r| r.child(desktop_tasks(&format!("tasks-{name}"), from_app, cx))),
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

    pub(crate) fn render_details(&self, name: &str, p: &Value, s: &Value, cx: &App) -> Div {
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

}
