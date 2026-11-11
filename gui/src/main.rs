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

mod app;
mod cards;
mod cleanup;
mod cli;
mod design;
mod format;
mod history;
mod prelude;
mod screens;
mod tabs;
mod view;

use prelude::*;

actions!(claude_profiles, [Quit]);

fn main() {
    Application::new().with_assets(Assets).run(|cx: &mut App| {
        gpui_component::init(cx);
        cx.on_action(|_: &Quit, cx| cx.quit());
        cx.bind_keys([KeyBinding::new("cmd-q", Quit, None), KeyBinding::new("ctrl-q", Quit, None)]);
        cx.set_menus(vec![Menu { name: "Claude Profiles".into(), items: vec![MenuItem::action("Quit Claude Profiles", Quit)] }]);
        // Linux and Windows keep running with no window unless told to stop.
        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        let bounds = Bounds::centered(None, size(px(540.), px(820.)), cx);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(TitlebarOptions { title: Some("Claude Profiles".into()), ..Default::default() }),
            window_min_size: Some(size(px(540.), px(620.))),
            window_background: if GLASS { WindowBackgroundAppearance::Blurred } else { WindowBackgroundAppearance::Opaque },
            app_id: Some("claude-profiles".into()),
            ..Default::default()
        };
        cx.open_window(options, |window, cx| {
            Theme::sync_system_appearance(Some(window), cx);
            let view = cx.new(|cx| Profiles::new(window, cx));
            cx.new(|cx| Root::new(view, window, cx))
        })
        .expect("could not open the window");
        cx.activate(true);
        start_menu_bar_helper();
    });
}

/// In the macOS app, start the menu bar panel that ships inside it
/// (Contents/Helpers), unless it is already running, so opening the app gives
/// both the window and the menu bar icon.
fn start_menu_bar_helper() {
    if !cfg!(target_os = "macos") {
        return;
    }
    let Some(contents) = std::env::current_exe().ok().and_then(|e| Some(e.parent()?.parent()?.to_path_buf())) else { return };
    let helper = contents.join("Helpers/Claude Profiles Menu.app");
    if !helper.exists() {
        return;
    }
    let running = std::process::Command::new("/usr/bin/pgrep").args(["-x", "ClaudeProfilesMenu"]).output().is_ok_and(|o| o.status.success());
    if !running {
        // -g: in the background, so the window keeps the focus.
        let _ = std::process::Command::new("/usr/bin/open").arg("-g").arg(&helper).spawn();
    }
}

pub(crate) fn id(s: impl Into<String>) -> ElementId {
    ElementId::Name(s.into().into())
}
