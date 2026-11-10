// The frame: header, version chips, tabs, status line, menus and cards.

use crate::prelude::*;

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
    pub(crate) fn menu(&self, trigger: gpui_component::button::Button, entries: Vec<Entry>, cx: &mut Context<Self>) -> impl IntoElement {
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

    pub(crate) fn render_main(&mut self, cx: &mut Context<Self>) -> Div {
        let header = div()
            .h_flex()
            .gap_1()
            .child(div().text_xl().font_bold().child("Claude Profiles"))
            .child(div().flex_1())
            .when(!self.importable.is_empty() && self.state.is_some(), |d| {
                d.child(
                    action("import", "download", &format!("Import {}", self.importable.len()), cx)
                        .tooltip("Make profiles from Claude accounts already on this computer")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.screen = Some(Screen::Import);
                            cx.notify();
                        })),
                )
            })
            .when(self.state.is_some(), |d| {
                d.child(action("add a new account", "plus", "Add a new account", cx).tooltip("Add an account").on_click(cx.listener(|this, _, window, cx| this.open_add(window, cx))))
            })
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

        main
    }

    pub(crate) fn settings_menu(&self, cx: &mut Context<Self>) -> impl IntoElement {
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

    pub(crate) fn render_versions(&self, s: &Value, cx: &App) -> Div {
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
    pub(crate) fn segmented<T: Copy + PartialEq + 'static>(
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

    pub(crate) fn render_tabs(&self, cx: &mut Context<Self>) -> Div {
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

    pub(crate) fn render_status(&self, cx: &App) -> Div {
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

    pub(crate) fn card(&self, name: &str, cx: &App) -> Div {
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

}

/// Claude Code sessions that Claude Desktop runs for its own Code features:
/// counted with the app, never as CLI sessions, since you did not start them.
pub(crate) fn desktop_tasks(key: &str, n: usize, cx: &App) -> impl IntoElement {
    div()
        .id(id(key.to_string()))
        .child(secondary(format!("· {n} Code session{} in the app", if n == 1 { "" } else { "s" }), cx))
        .tooltip(|window, cx| {
            gpui_component::tooltip::Tooltip::new("Claude Code sessions that Claude Desktop runs for its Code tab and tasks. See them in Running.").build(window, cx)
        })
}

pub(crate) fn warning(text: &str) -> Div {
    div()
        .h_flex()
        .items_start()
        .gap_1p5()
        .text_xs()
        .text_color(orange())
        .child(icon("triangle-alert").xsmall())
        .child(div().flex_1().child(text.to_string()))
}
