// The Files tab's cleanup: scan, choose, move to the Trash.
//
// The command decides what may be cleaned and checks each path again when
// it runs (see src/commands/cleanup.rs); this only shows the list, keeps the
// selection and asks before anything moves.

use crate::prelude::*;

/// The order groups are shown in: the surest first.
const GROUPS: &[&str] = &["Claude", "App caches", "Developer", "Logs", "Project build folders", "Downloads", "Large files"];

/// What the cleanup part of the Files tab is showing.
#[derive(Default)]
pub(crate) struct Cleanup {
    pub(crate) scan: Option<Value>,
    pub(crate) scanning: bool,
    pub(crate) running: bool,
    pub(crate) include_downloads: bool,
    pub(crate) include_large: bool,
    pub(crate) selected: HashSet<String>,
    /// Groups shown in full rather than their largest few.
    pub(crate) open_groups: HashSet<String>,
    pub(crate) last_result: Option<Value>,
}

impl Cleanup {
    fn items(&self) -> Vec<Value> {
        self.scan.as_ref().and_then(|s| s["items"].as_array().cloned()).unwrap_or_default()
    }

    fn selected_bytes(&self) -> u64 {
        self.items().iter().filter(|i| self.selected.contains(i["path"].as_str().unwrap_or(""))).map(|i| i["bytes"].as_u64().unwrap_or(0)).sum()
    }

    /// After a scan: tick what is safe and not in use; review items wait for you.
    pub(crate) fn take_scan(&mut self, v: Value) {
        self.selected = v["items"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|i| i["safe"] == true && i["inUse"] != true)
            .filter_map(|i| i["path"].as_str().map(String::from))
            .collect();
        self.scan = Some(v);
    }
}

pub(crate) fn bytes(b: u64) -> String {
    let gb = b as f64 / (1024.0 * 1024.0 * 1024.0);
    if gb >= 1.0 {
        format!("{gb:.1} GB")
    } else {
        format!("{} MB", b / (1024 * 1024))
    }
}

impl Profiles {
    pub(crate) fn scan_cleanup(&mut self, cx: &mut Context<Self>) {
        self.cleanup.scanning = true;
        let mut args = vec!["cleanup-scan".to_string()];
        if self.cleanup.include_downloads {
            args.push("--downloads".into());
        }
        if self.cleanup.include_large {
            args.push("--large".into());
        }
        self.call(Job::CleanupScan, args, cx);
    }

    /// The Files tab's way in: what the last scan found, and the page.
    pub(crate) fn render_cleanup_entry(&self, cx: &mut Context<Self>) -> Div {
        let c = &self.cleanup;
        let summary = match &c.scan {
            _ if c.scanning => "Scanning…".to_string(),
            Some(_) => {
                let free: u64 = c.items().iter().filter(|i| i["inUse"] != true).map(|i| i["bytes"].as_u64().unwrap_or(0)).sum();
                format!("{} can be cleaned · {} selected", bytes(free), bytes(c.selected_bytes()))
            }
            None => "Free up space: caches, logs and build output across your apps, not just Claude.".to_string(),
        };
        self.card("cleanup-entry", cx).child(
            div()
                .h_flex()
                .gap_2()
                .child(icon("trash").small())
                .child(div().v_flex().flex_1().min_w_0().child(div().font_semibold().child("Clean up")).child(secondary(summary, cx)))
                .child(action("cleanup-open", "chevron-right", "Open", cx).on_click(cx.listener(|this, _, _, cx| {
                    this.screen = Some(Screen::Cleanup);
                    if this.cleanup.scan.is_none() && !this.cleanup.scanning {
                        this.scan_cleanup(cx);
                    }
                    cx.notify();
                }))),
        )
    }

    pub(crate) fn render_cleanup(&self, cx: &mut Context<Self>) -> Vec<Div> {
        let c = &self.cleanup;
        let mut head = self
            .card("cleanup", cx)
            .child(secondary(
                "Finds caches, logs and build output that rebuild themselves, across your apps, not just Claude. Nothing in use is touched, and everything goes to the Trash, so you can take it back.",
                cx,
            ))
            .child(
                div()
                    .h_flex()
                    .gap_2()
                    .child(
                        // The options stack, so the button keeps its place
                        // however narrow the window.
                        div()
                            .v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_1p5()
                            .child(
                                Checkbox::new("cleanup-downloads")
                                    .label("Include old downloads and installers")
                                    .checked(c.include_downloads)
                                    .on_click(cx.listener(|this, v: &bool, _, cx| {
                                        this.cleanup.include_downloads = *v;
                                        cx.notify();
                                    })),
                            )
                            .child(
                                Checkbox::new("cleanup-large")
                                    .label("Large files (500 MB+)")
                                    .checked(c.include_large)
                                    .on_click(cx.listener(|this, v: &bool, _, cx| {
                                        this.cleanup.include_large = *v;
                                        cx.notify();
                                    })),
                            ),
                    )
                    .child(
                        action("cleanup-scan", "search", if c.scanning { "Scanning…" } else if c.scan.is_some() { "Scan again" } else { "Scan" }, cx)
                            .loading(c.scanning)
                            .disabled(c.scanning)
                            .flex_none()
                            .on_click(cx.listener(|this, _, _, cx| this.scan_cleanup(cx))),
                    ),
            );
        if c.include_large {
            head = head.child(secondary("Large files are your own files, not caches: none is ticked, and each goes to the Trash only if you choose it.", cx));
        }
        if c.include_downloads && cfg!(target_os = "macos") {
            head = head.child(secondary("macOS asks once before an app can read Downloads.", cx));
        }
        if c.scanning {
            head = head.child(secondary("Measuring… big folders take a little while.", cx));
        }
        if let Some(r) = &c.last_result {
            let skipped = r["skipped"].as_array().map(Vec::len).unwrap_or(0);
            let failed = r["failed"].as_array().map(Vec::len).unwrap_or(0);
            let mut line = format!("Moved {} to the Trash, freeing {}.", r["moved"].as_array().map(Vec::len).unwrap_or(0), bytes(r["freedBytes"].as_u64().unwrap_or(0)));
            if skipped > 0 {
                line += &format!(" {skipped} skipped (in use or changed since the scan).");
            }
            if failed > 0 {
                line += &format!(" {failed} could not be moved.");
            }
            head = head.child(div().text_xs().text_color(if failed > 0 { orange() } else { green() }).child(line));
        }

        let Some(scan) = &c.scan else { return vec![head] };
        let items = c.items();
        let total: u64 = items.iter().filter(|i| i["inUse"] != true).map(|i| i["bytes"].as_u64().unwrap_or(0)).sum();
        let chosen = c.selected.len();
        head = head.child(
            div()
                .h_flex()
                .gap_2()
                .child(secondary(format!("{} can be cleaned · {} selected", bytes(total), bytes(c.selected_bytes())), cx).flex_1())
                .child(
                    confirm("cleanup-go", &format!("Move {chosen} to Trash"), false, cx)
                        .disabled(chosen == 0 || c.running)
                        .loading(c.running)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.screen = Some(Screen::CleanupConfirm);
                            cx.notify();
                        })),
                ),
        );

        let mut cards = vec![head];
        for group in GROUPS {
            let rows: Vec<&Value> = items.iter().filter(|i| i["group"] == *group).collect();
            if rows.is_empty() {
                continue;
            }
            cards.push(self.render_group(group, &rows, cx));
        }
        if let Some(tools) = scan["tools"].as_array().filter(|t| !t.is_empty()) {
            let mut card = self
                .card("cleanup-tools", cx)
                .child(div().font_semibold().child("Cleaned by their own tools"))
                .child(secondary("These know their own data best. Each opens in your terminal, where you see what it does.", cx));
            for t in tools {
                let tool = t["id"].as_str().unwrap_or("").to_string();
                card = card.child(
                    div()
                        .h_flex()
                        .gap_2()
                        .child(
                            div()
                                .v_flex()
                                .flex_1()
                                .min_w_0()
                                .child(div().text_sm().child(t["label"].as_str().unwrap_or("").to_string()))
                                .child(secondary(format!("{} — {}", t["command"].as_str().unwrap_or(""), t["note"].as_str().unwrap_or("")), cx)),
                        )
                        .child(quiet(id(format!("tool-{tool}")), "Run…", cx).on_click(cx.listener(move |this, _, _, cx| {
                            let term = this.terminal();
                            this.act(format!("Opening {term}…"), &["cleanup-tool", &tool, "--terminal", &term], cx);
                        }))),
                );
            }
            cards.push(card);
        }
        cards
    }

    fn render_group(&self, group: &str, rows: &[&Value], cx: &mut Context<Self>) -> Div {
        const SHOWN: usize = 6;
        let c = &self.cleanup;
        let open = c.open_groups.contains(group);
        let free: Vec<String> = rows.iter().filter(|i| i["inUse"] != true).filter_map(|i| i["path"].as_str().map(String::from)).collect();
        let all_on = !free.is_empty() && free.iter().all(|p| c.selected.contains(p));
        let size: u64 = rows.iter().map(|i| i["bytes"].as_u64().unwrap_or(0)).sum();
        let g = group.to_string();

        let mut card = self.card(&format!("cleanup-{group}"), cx).child(
            div()
                .h_flex()
                .gap_2()
                .child(div().font_semibold().child(group.to_string()))
                .child(secondary(bytes(size), cx))
                .child(div().flex_1())
                .child(quiet(id(format!("all-{group}")), if all_on { "Select none" } else { "Select all" }, cx).xsmall().on_click(cx.listener(
                    move |this, _, _, cx| {
                        for p in &free {
                            if all_on {
                                this.cleanup.selected.remove(p);
                            } else {
                                this.cleanup.selected.insert(p.clone());
                            }
                        }
                        cx.notify();
                    },
                ))),
        );
        for (n, i) in rows.iter().take(if open { usize::MAX } else { SHOWN }).enumerate() {
            let path = i["path"].as_str().unwrap_or("").to_string();
            let in_use = i["inUse"] == true;
            let mut meta = i["note"].as_str().unwrap_or("").to_string();
            if let Some(d) = i["daysOld"].as_u64() {
                meta += &format!(" Last changed {d} days ago.");
            }
            let p = path.clone();
            card = card.child(
                div()
                    .h_flex()
                    .items_start()
                    .gap_2()
                    .child(
                        Checkbox::new(id(format!("clean-{group}-{n}")))
                            .checked(c.selected.contains(&path))
                            .disabled(in_use)
                            .on_click(cx.listener(move |this, v: &bool, _, cx| {
                                if *v {
                                    this.cleanup.selected.insert(p.clone());
                                } else {
                                    this.cleanup.selected.remove(&p);
                                }
                                cx.notify();
                            })),
                    )
                    .child(
                        div()
                            .v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .h_flex()
                                    .gap_1p5()
                                    .child(div().min_w_0().truncate().text_sm().child(i["label"].as_str().unwrap_or("").to_string()))
                                    .when(in_use, |d| d.child(chip("in use", Some(orange()), cx)))
                                    .when(!in_use && i["safe"] != true, |d| d.child(chip("review", None, cx))),
                            )
                            .child(secondary(meta, cx))
                            .child(secondary(short_path(&path), cx).truncate()),
                    )
                    .child(div().text_sm().whitespace_nowrap().child(bytes(i["bytes"].as_u64().unwrap_or(0))))
                    .child({
                        let reveal = path.clone();
                        icon_button(id(format!("clean-show-{group}-{n}")), "folder", "Show in file manager", cx)
                            .xsmall()
                            .on_click(move |_, _, cx| cx.reveal_path(Path::new(&reveal)))
                    }),
            );
        }
        if rows.len() > SHOWN {
            card = card.child(
                div()
                    .id(id(format!("more-clean-{group}")))
                    .text_xs()
                    .text_color(accent())
                    .cursor_pointer()
                    .child(if open { "Show fewer".to_string() } else { format!("Show all {}", rows.len()) })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if !this.cleanup.open_groups.remove(&g) {
                            this.cleanup.open_groups.insert(g.clone());
                        }
                        cx.notify();
                    })),
            );
        }
        card
    }

    /// The last look before anything moves.
    pub(crate) fn render_cleanup_confirm(&mut self, cx: &mut Context<Self>) -> Div {
        let c = &self.cleanup;
        let items: Vec<Value> = c.items().into_iter().filter(|i| c.selected.contains(i["path"].as_str().unwrap_or(""))).collect();
        let review = items.iter().filter(|i| i["safe"] != true).count();
        let mut frame = self
            .screen_frame(format!("Move {} to the Trash?", items.len()), cx)
            .child(secondary(
                format!(
                    "This frees about {}. Each item is checked again first, and anything opened since the scan is skipped. You can take things back out of the Trash until you empty it.",
                    bytes(c.selected_bytes())
                ),
                cx,
            ));
        if review > 0 {
            frame = frame.child(warning(&format!(
                "{review} of these are review items (downloads, large files, packages or project build folders). Make sure you no longer need them."
            )));
        }
        for i in items.iter().take(12) {
            frame = frame.child(
                div()
                    .h_flex()
                    .gap_2()
                    .text_xs()
                    .child(div().flex_1().min_w_0().truncate().child(i["label"].as_str().unwrap_or("").to_string()))
                    .child(secondary(bytes(i["bytes"].as_u64().unwrap_or(0)), cx)),
            );
        }
        if items.len() > 12 {
            frame = frame.child(secondary(format!("and {} more", items.len() - 12), cx));
        }
        let paths: Vec<String> = items.iter().filter_map(|i| i["path"].as_str().map(String::from)).collect();
        frame.child(self.screen_buttons(
            "Move to Trash",
            false,
            !paths.is_empty(),
            move |this, cx| {
                this.cleanup.running = true;
                let mut args = vec!["cleanup-run".to_string()];
                args.extend(paths.iter().cloned());
                this.call(Job::CleanupRun, args, cx);
                this.back(cx);
            },
            cx,
        ))
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn sizes_read_naturally() {
        assert_eq!(super::bytes(512 * 1024 * 1024), "512 MB");
        assert_eq!(super::bytes(3 * 1024 * 1024 * 1024 / 2), "1.5 GB");
    }
}
