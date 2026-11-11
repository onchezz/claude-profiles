// The Sessions tab: every account's past Claude Code sessions in one list,
// each resumable in a terminal or in Claude Desktop.
//
// The command finds and merges them (see src/commands/sessions.rs); this
// filters, groups by day and hands a resume back to it.

use crate::prelude::*;

/// Rows shown before "Show more", and how many each click adds.
const PAGE: usize = 60;

pub(crate) struct History {
    pub(crate) list: Option<Value>,
    pub(crate) loading: bool,
    pub(crate) search: Entity<InputState>,
    /// One account's sessions only; None for all.
    pub(crate) account: Option<String>,
    pub(crate) archived: bool,
    pub(crate) shown: usize,
    _watch: Subscription,
}

impl History {
    pub(crate) fn new(window: &mut Window, cx: &mut Context<Profiles>) -> History {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search titles, prompts and folders"));
        let watch = cx.subscribe(&search, |this: &mut Profiles, _, _: &InputEvent, cx| {
            this.history.shown = PAGE;
            cx.notify();
        });
        History { list: None, loading: false, search, account: None, archived: false, shown: PAGE, _watch: watch }
    }
}

/// One row, with the account it belongs to.
struct Row {
    account: String,
    s: Value,
}

/// "3m ago", "2h ago", "Tue 14:05", "Nov 2".
fn when(secs: u64) -> String {
    let Some(t) = chrono::DateTime::from_timestamp(secs as i64, 0) else { return String::new() };
    let t = t.with_timezone(&chrono::Local);
    let now = chrono::Local::now();
    let mins = (now - t).num_minutes();
    if mins < 1 {
        "just now".into()
    } else if mins < 60 {
        format!("{mins}m ago")
    } else if mins < 12 * 60 {
        format!("{}h ago", mins / 60)
    } else if (now.date_naive() - t.date_naive()).num_days() < 7 {
        t.format("%a %H:%M").to_string()
    } else {
        t.format("%b %-d").to_string()
    }
}

/// The heading a day's sessions sit under.
fn day(secs: u64) -> String {
    let Some(t) = chrono::DateTime::from_timestamp(secs as i64, 0) else { return "Earlier".into() };
    let date = t.with_timezone(&chrono::Local).date_naive();
    let days = (chrono::Local::now().date_naive() - date).num_days();
    match days {
        0 => "Today".into(),
        1 => "Yesterday".into(),
        2..=6 => date.format("%A").to_string(),
        _ if date.format("%Y").to_string() == chrono::Local::now().format("%Y").to_string() => date.format("%B %-d").to_string(),
        _ => date.format("%B %-d, %Y").to_string(),
    }
}

fn matches(s: &Value, needle: &str) -> bool {
    needle.is_empty()
        || ["title", "firstPrompt", "lastPrompt", "cwd", "branch"]
            .iter()
            .any(|k| s[*k].as_str().is_some_and(|v| v.to_lowercase().contains(needle)))
}

impl Profiles {
    pub(crate) fn load_history(&mut self, cx: &mut Context<Self>) {
        if !self.history.loading {
            self.history.loading = true;
            self.call(Job::Sessions, vec!["sessions".into()], cx);
        }
    }

    fn account_color(&self, state: &Value, name: &str, cx: &App) -> Hsla {
        let c = state["profiles"].as_array().into_iter().flatten().find(|p| p["name"] == name).and_then(|p| p["desktop"]["color"].as_str());
        named(c, cx)
    }

    fn history_rows(&self, cx: &App) -> Vec<Row> {
        let h = &self.history;
        let needle = h.search.read(cx).value().trim().to_lowercase();
        let accounts: Vec<Value> = h.list.as_ref().and_then(|l| l["accounts"].as_array().cloned()).unwrap_or_default();
        let mut rows: Vec<Row> = accounts
            .iter()
            .filter(|a| h.account.as_deref().is_none_or(|f| a["name"] == f))
            .flat_map(|a| {
                let name = a["name"].as_str().unwrap_or("").to_string();
                sessions_of(&a["sessions"]).into_iter().map(move |s| Row { account: name.clone(), s })
            })
            .filter(|r| h.archived || r.s["archived"] != true)
            .filter(|r| matches(&r.s, &needle))
            .collect();
        rows.sort_by_key(|r| std::cmp::Reverse(r.s["lastActive"].as_u64().unwrap_or(0)));
        rows
    }

    /// Search and filters: kept above the list, so they stay in reach.
    pub(crate) fn render_history_head(&self, cx: &mut Context<Self>) -> Div {
        let h = &self.history;
        let accounts: Vec<Value> = h.list.as_ref().and_then(|l| l["accounts"].as_array().cloned()).unwrap_or_default();
        let count = self.history_rows(cx).len();
        let shown_label = match &h.account {
            None => "All accounts".to_string(),
            Some(a) if a == "default" => "Default".to_string(),
            Some(a) => a.clone(),
        };
        let mut entries = vec![item("All accounts", |this, _, cx| {
            this.history.account = None;
            this.history.shown = PAGE;
            cx.notify();
        })];
        for a in &accounts {
            let name = a["name"].as_str().unwrap_or("").to_string();
            let n = sessions_of(&a["sessions"]).len();
            let label = format!("{} ({n})", if name == "default" { "Default" } else { &name });
            entries.push(item(label, move |this, _, cx| {
                this.history.account = Some(name.clone());
                this.history.shown = PAGE;
                cx.notify();
            }));
        }

        div()
            .v_flex()
            .gap_1p5()
            .child(
                div()
                    .h_flex()
                    .gap_2()
                    .child(div().flex_1().min_w_0().child(Input::new(&h.search).small()))
                    .child(self.menu(quiet("sessions-account", &shown_label, cx).icon(icon("chevron-down")), entries, cx))
                    .child(icon_button("sessions-reload", "refresh-cw", "Look again", cx).loading(h.loading).on_click(cx.listener(|this, _, _, cx| this.load_history(cx)))),
            )
            .child(
                div()
                    .h_flex()
                    .gap_2()
                    .child(secondary(
                        match &h.list {
                            None if h.loading => "Reading sessions…".to_string(),
                            None => String::new(),
                            Some(_) => format!("{count} session{}", if count == 1 { "" } else { "s" }),
                        },
                        cx,
                    ))
                    .child(
                        div()
                            .id("sessions-how")
                            .child(icon("circle-ellipsis").xsmall().text_color(cx.theme().muted_foreground))
                            .tooltip(|window, cx| {
                                gpui_component::tooltip::Tooltip::new(
                                    "Terminal resumes with the account's own claude command, in the session's folder. App opens it in that account's Claude Desktop, adding a terminal session to its Code tab.",
                                )
                                .build(window, cx)
                            }),
                    )
                    .child(div().flex_1())
                    .child(
                        Checkbox::new("sessions-archived").label("Show archived").checked(h.archived).on_click(cx.listener(|this, v: &bool, _, cx| {
                            this.history.archived = *v;
                            cx.notify();
                        })),
                    ),
            )
    }

    pub(crate) fn render_history(&self, state: &Value, cx: &mut Context<Self>) -> Vec<Div> {
        let h = &self.history;
        if h.list.is_none() {
            return Vec::new();
        }
        let needle = h.search.read(cx).value().trim().to_lowercase();
        let rows = self.history_rows(cx);
        let mut cards = Vec::new();
        // Each day, then each account within it, the most recent first.
        let mut days: Vec<(String, Vec<(String, Vec<(usize, &Row)>)>)> = Vec::new();
        for (n, r) in rows.iter().take(h.shown).enumerate() {
            let heading = day(r.s["lastActive"].as_u64().unwrap_or(0));
            if days.last().is_none_or(|(d, _)| *d != heading) {
                days.push((heading, Vec::new()));
            }
            let groups = &mut days.last_mut().expect("just pushed").1;
            match groups.iter_mut().find(|(a, _)| *a == r.account) {
                Some((_, list)) => list.push((n, r)),
                None => groups.push((r.account.clone(), vec![(n, r)])),
            }
        }
        for (heading, groups) in days {
            let mut card = self.card(&format!("sessions-{heading}"), cx).gap_1().child(div().font_semibold().child(heading.clone()));
            for (account, list) in groups {
                card = card.child(self.render_history_account(&account, list.len(), state, cx));
                for (n, r) in list {
                    card = card.child(self.render_history_row(n, r, cx));
                }
            }
            cards.push(card);
        }
        if rows.len() > h.shown {
            cards.push(
                div().h_flex().justify_center().mb_2().child(
                    quiet("sessions-more", &format!("Show more ({} left)", rows.len() - h.shown), cx).on_click(cx.listener(|this, _, _, cx| {
                        this.history.shown += PAGE;
                        cx.notify();
                    })),
                ),
            );
        }
        if rows.is_empty() {
            cards.push(self.card("sessions-none", cx).child(secondary(if needle.is_empty() { "No sessions yet." } else { "Nothing matches." }, cx)));
        }
        cards
    }

    /// An account's heading inside a day: its colour, name and who it is.
    fn render_history_account(&self, account: &str, count: usize, state: &Value, cx: &mut Context<Self>) -> Div {
        let code = if account == "default" {
            &state["default"]["code"]["account"]
        } else {
            state["profiles"].as_array().into_iter().flatten().find(|p| p["name"] == account).map_or(&Value::Null, |p| &p["code"]["account"])
        };
        div()
            .h_flex()
            .gap_1p5()
            .mt_1()
            .child(dot(self.account_color(state, account, cx)))
            .child(div().text_xs().font_weight(FontWeight::SEMIBOLD).child(if account == "default" { "Default".to_string() } else { account.to_string() }))
            .children(account_label(code).map(|w| secondary(w, cx).min_w_0().truncate()))
            .child(div().flex_1())
            .child(secondary(count.to_string(), cx))
    }

    fn render_history_row(&self, n: usize, r: &Row, cx: &mut Context<Self>) -> Div {
        let s = &r.s;
        let title = s["title"].as_str().filter(|t| !t.is_empty()).unwrap_or("Untitled session").to_string();
        let cwd = s["cwd"].as_str().map(String::from);
        let id = s["id"].as_str().map(String::from);
        let app_id = s["appId"].as_str().map(String::from);
        let cli_target = s["cliTarget"].as_str().map(String::from);
        let app_target = s["appTarget"].as_str().map(String::from).filter(|_| self.is_mac());

        let mut meta = Vec::new();
        if let Some(c) = &cwd {
            meta.push(short_path(c));
        }
        if let Some(b) = s["branch"].as_str() {
            meta.push(b.to_string());
        }
        let prompt = s["lastPrompt"].as_str().or(s["firstPrompt"].as_str()).filter(|p| *p != title).map(String::from);

        let terminal = {
            let (target, id) = (cli_target.clone(), id.clone());
            let can = target.is_some() && id.is_some();
            quiet(id_for("resume-cli", n), "Terminal", cx)
                .icon(icon("square-terminal"))
                .xsmall()
                .disabled(!can)
                .tooltip(if can { "Resume in a terminal with this account's claude" } else { "This session has no terminal transcript to resume" })
                .on_click(cx.listener(move |this, _, _, cx| {
                    let (Some(t), Some(i)) = (&target, &id) else { return };
                    let term = this.terminal();
                    this.act(format!("Opening {term}…"), &["resume-cli", t, i, "--terminal", &term], cx);
                }))
        };
        let app = {
            let target = app_target.clone();
            // A Desktop session is continued; a terminal one is imported.
            let session = app_id.clone().or(id.clone());
            let can = target.is_some() && session.is_some();
            quiet(id_for("resume-app", n), "App", cx)
                .icon(icon("app-window"))
                .xsmall()
                .disabled(!can)
                .tooltip(match (&target, &app_id) {
                    (None, _) => "No Claude Desktop runs on this session's account",
                    (Some(_), Some(_)) => "Continue in this account's Claude Desktop",
                    (Some(_), None) => "Open in this account's Claude Desktop (adds it to its Code tab)",
                })
                .on_click(cx.listener(move |this, _, _, cx| {
                    let (Some(t), Some(i)) = (&target, &session) else { return };
                    this.act("Opening in Claude…", &["resume-app", t, i], cx);
                }))
        };

        div()
            .h_flex()
            .items_start()
            .gap_2()
            .py(px(5.))
            .border_t_1()
            .border_color(cx.theme().foreground.opacity(0.05))
            .pl(px(15.))
            .child(
                div()
                    .v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_0p5()
                    .child(
                        div()
                            .h_flex()
                            .gap_1p5()
                            .child(div().min_w_0().truncate().font_weight(FontWeight::MEDIUM).child(title))
                            .when(s["inApp"] == true, |d| d.child(kind_tag(true, s["id"].is_string(), cx)))
                            .when(s["archived"] == true, |d| d.child(chip("archived", None, cx))),
                    )
                    .child(secondary(meta.join(" · "), cx).truncate())
                    .children(prompt.map(|p| secondary(format!("“{p}”"), cx).truncate())),
            )
            .child(
                div()
                    .v_flex()
                    .items_end()
                    .gap_1()
                    .flex_none()
                    .child(secondary(when(s["lastActive"].as_u64().unwrap_or(0)), cx))
                    .child(
                        div()
                            .h_flex()
                            .gap_1()
                            .child(terminal)
                            .child(app)
                            .when_some(cwd, |d, c| {
                                d.child(
                                    icon_button(id_for("resume-dir", n), "folder", "Show the folder", cx)
                                        .xsmall()
                                        .on_click(move |_, _, cx| cx.open_with_system(Path::new(&c))),
                                )
                            }),
                    ),
            )
    }
}

fn id_for(what: &str, n: usize) -> ElementId {
    id(format!("{what}-{n}"))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    #[test]
    fn search_looks_at_titles_prompts_and_folders() {
        let s = json!({ "title": "Login fix", "cwd": "/p/helapoint", "lastPrompt": "and the tests" });
        assert!(super::matches(&s, ""));
        assert!(super::matches(&s, "login"));
        assert!(super::matches(&s, "helapoint"));
        assert!(super::matches(&s, "tests"));
        assert!(!super::matches(&s, "payroll"));
    }
}
