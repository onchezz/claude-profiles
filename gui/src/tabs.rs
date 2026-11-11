// The Running and Files tabs.

use crate::prelude::*;

/// One open Claude Desktop, as the Running tab lists it.
struct OpenApp {
    name: String,
    color: Option<String>,
    who: String,
    memory_mb: u64,
    cpu: f64,
    /// Claude Code sessions it started for its own Code features.
    tasks: Vec<Value>,
}

impl Profiles {
    // ---- Running and Files tabs -----------------------------------------------------------

    pub(crate) fn render_running(&self, s: &Value, cx: &mut Context<Self>) -> Vec<Div> {
        let mut desktops = self.card("running-desktop", cx).child(div().font_semibold().child("Claude Desktop"));
        let mut open: Vec<OpenApp> = Vec::new();
        let dd = &s["default"]["desktop"];
        if dd["running"] == true {
            open.push(OpenApp {
                name: "default".into(),
                color: None,
                who: desktop_account_label(&dd["account"]),
                memory_mb: dd["memoryMB"].as_u64().unwrap_or(0),
                cpu: dd["cpu"].as_f64().unwrap_or(0.0),
                tasks: sessions_of(&s["default"]["code"]["desktopSessions"]),
            });
        }
        for p in s["profiles"].as_array().into_iter().flatten() {
            let d = &p["desktop"];
            if d["clone"]["running"] == true {
                open.push(OpenApp {
                    name: p["name"].as_str().unwrap_or("").into(),
                    color: d["color"].as_str().map(String::from),
                    who: desktop_account_label(&d["account"]),
                    memory_mb: d["clone"]["memoryMB"].as_u64().unwrap_or(0),
                    cpu: d["clone"]["cpu"].as_f64().unwrap_or(0.0),
                    tasks: sessions_of(&p["code"]["desktopSessions"]),
                });
            }
        }
        if open.is_empty() {
            desktops = desktops.child(secondary("No Claude Desktop windows open.", cx));
        }
        for OpenApp { name, color, who, memory_mb: mem, cpu, tasks } in open {
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
            if !tasks.is_empty() {
                desktops = desktops
                    .child(secondary(format!("Claude Code sessions this app started ({})", tasks.len()), cx).pl(px(14.)))
                    .child(div().pl(px(14.)).child(self.render_sessions(&format!("app-{name}"), &tasks, cx)));
            }
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

    pub(crate) fn file_row(&self, label: &str, path: &str, mb: Option<u64>, key: String, cx: &mut Context<Self>) -> Div {
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

    pub(crate) fn render_files(&self, s: &Value, cx: &mut Context<Self>) -> Vec<Div> {
        let mut out = vec![self.render_cleanup_entry(cx)];
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

}
