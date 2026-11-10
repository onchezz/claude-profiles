// The screens that replace the list for a moment: add, rename, remove,
// name an account, import, and confirmations.

use crate::prelude::*;

impl Profiles {
    // ---- Screens --------------------------------------------------------------------------

    pub(crate) fn screen_frame(&self, title: String, cx: &mut Context<Self>) -> Div {
        div().v_flex().gap_3().child(
            div()
                .h_flex()
                .gap_1()
                .child(icon_button("back", "chevron-left", "Back", cx).on_click(cx.listener(|this, _, _, cx| this.back(cx))))
                .child(div().text_lg().font_semibold().child(title)),
        )
    }

    pub(crate) fn screen_buttons(
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

    pub(crate) fn render_screen(&mut self, cx: &mut Context<Self>) -> Div {
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

    pub(crate) fn render_add(&mut self, cx: &mut Context<Self>) -> Div {
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

    pub(crate) fn render_import(&mut self, cx: &mut Context<Self>) -> Div {
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
