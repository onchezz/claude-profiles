// The window's look, in one place: icons, the button family, chips and
// colours. Everything else builds from these, so the panel stays consistent.
//
// Buttons come in three kinds, each with one job:
//   action   soft-filled, icon + label: the things you do to an account
//   quiet    no fill until hovered: secondary row actions (Open, Show, Quit)
//   icon     square, icon only, with a tooltip: copy, refresh, settings, ⋯
// plus danger tints of action and icon for things that end something.

use std::borrow::Cow;

use gpui::*;
use gpui_component::button::{Button, ButtonCustomVariant, ButtonRounded, ButtonVariants};
use gpui_component::{ActiveTheme, Icon, Sizable};

// ---- Icons ---------------------------------------------------------------------------------

macro_rules! icons {
    ($($name:literal),* $(,)?) => {
        &[$((concat!("icons/", $name, ".svg"), include_bytes!(concat!("../assets/icons/", $name, ".svg")))),*]
    };
}

const ICONS: &[(&str, &[u8])] = icons![
    "app-window", "arrow-down", "check", "chevron-down", "chevron-left", "chevron-right", "chevron-up",
    "circle-ellipsis", "circle-plus", "circle-stop", "circle-user", "circle-x", "close", "copy", "download",
    "ellipsis", "external-link", "eye", "folder", "link", "loader-circle", "log-in", "log-out", "minus",
    "pencil", "plus", "power", "refresh-cw", "settings", "square-terminal", "triangle-alert", "x",
];

/// The icons, compiled in, for GPUI and for gpui-component's own controls.
pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> anyhow::Result<Option<Cow<'static, [u8]>>> {
        Ok(ICONS.iter().find(|(p, _)| *p == path).map(|(_, bytes)| Cow::Borrowed(*bytes)))
    }

    fn list(&self, path: &str) -> anyhow::Result<Vec<SharedString>> {
        Ok(ICONS.iter().filter(|(p, _)| p.starts_with(path)).map(|(p, _)| SharedString::from(*p)).collect())
    }
}

pub fn icon(name: &str) -> Icon {
    Icon::default().path(SharedString::from(format!("icons/{name}.svg")))
}

// ---- Colours -----------------------------------------------------------------------------------

/// Whether the window is see-through: macOS and Windows blur what is behind
/// it; on Linux without a compositor transparency shows as black, so it stays
/// solid there.
pub const GLASS: bool = cfg!(any(target_os = "macos", windows));

pub fn hex(rgb: u32) -> Hsla {
    Rgba { r: ((rgb >> 16) & 0xff) as f32 / 255., g: ((rgb >> 8) & 0xff) as f32 / 255., b: (rgb & 0xff) as f32 / 255., a: 1. }.into()
}

/// The accent, used sparingly: usage bars and the selected tab.
pub fn accent() -> Hsla {
    hex(0x2F7CF6)
}

pub fn green() -> Hsla {
    hex(0x2EA44F)
}

pub fn orange() -> Hsla {
    hex(0xE8790C)
}

pub fn red() -> Hsla {
    hex(0xE5484D)
}

/// A Desktop profile's Dock colour, as its dot.
pub fn named(c: Option<&str>, cx: &App) -> Hsla {
    match c {
        Some("orange") => orange(),
        Some("red") => red(),
        Some("yellow") => hex(0xE5B400),
        Some("green") => green(),
        Some("teal") => hex(0x14A3A8),
        Some("blue") => accent(),
        Some("purple") => hex(0x8E5BD9),
        Some("pink") => hex(0xE0569B),
        _ => cx.theme().muted_foreground,
    }
}

/// The window behind everything.
pub fn window_bg(cx: &App) -> Hsla {
    cx.theme().background.opacity(if GLASS { 0.78 } else { 1.0 })
}

/// A card: a slightly raised, rounded panel on the glass.
pub fn card_bg(cx: &App) -> Hsla {
    cx.theme().foreground.opacity(if cx.theme().mode.is_dark() { 0.06 } else { 0.035 })
}

fn soft(cx: &App, tint: Hsla, strength: f32) -> ButtonCustomVariant {
    ButtonCustomVariant::new(cx)
        .color(tint.opacity(0.09 * strength))
        .hover(tint.opacity(0.15 * strength))
        .active(tint.opacity(0.22 * strength))
        .foreground(tint)
        .border(gpui::transparent_black())
        .shadow(false)
}

fn clear(cx: &App, tint: Hsla) -> ButtonCustomVariant {
    ButtonCustomVariant::new(cx)
        .color(gpui::transparent_black())
        .hover(cx.theme().foreground.opacity(0.08))
        .active(cx.theme().foreground.opacity(0.14))
        .foreground(tint)
        .border(gpui::transparent_black())
        .shadow(false)
}

// ---- Buttons --------------------------------------------------------------------------------

/// What you do to an account: soft-filled, icon and label.
pub fn action(id: impl Into<ElementId>, icon_name: &str, label: &str, cx: &App) -> Button {
    Button::new(id)
        .small()
        .icon(icon(icon_name))
        .label(label.to_string())
        .rounded(ButtonRounded::Size(px(7.)))
        .custom(soft(cx, cx.theme().foreground, 1.0))
}

/// A secondary row action: plain text until hovered.
pub fn quiet(id: impl Into<ElementId>, label: &str, cx: &App) -> Button {
    Button::new(id)
        .small()
        .label(label.to_string())
        .rounded(ButtonRounded::Size(px(7.)))
        .custom(soft(cx, cx.theme().foreground, 0.8))
}

/// Square, icon only, explained by its tooltip.
pub fn icon_button(id: impl Into<ElementId>, icon_name: &str, tip: &str, cx: &App) -> Button {
    Button::new(id)
        .small()
        .icon(icon(icon_name))
        .tooltip(tip.to_string())
        .rounded(ButtonRounded::Size(px(7.)))
        .custom(clear(cx, cx.theme().muted_foreground))
}

/// Ends something (a session, a profile): tinted red, never filled solid.
pub fn danger_icon(id: impl Into<ElementId>, icon_name: &str, tip: &str, cx: &App) -> Button {
    Button::new(id)
        .xsmall()
        .icon(icon(icon_name))
        .tooltip(tip.to_string())
        .rounded(ButtonRounded::Size(px(6.)))
        .custom(soft(cx, red(), 1.0))
}

/// A dialog's confirming button.
pub fn confirm(id: impl Into<ElementId>, label: &str, danger: bool, cx: &App) -> Button {
    let b = Button::new(id).small().label(label.to_string()).rounded(ButtonRounded::Size(px(7.)));
    if danger {
        b.danger()
    } else {
        b.custom(
            ButtonCustomVariant::new(cx)
                .color(accent())
                .hover(accent().opacity(0.9))
                .active(accent().opacity(0.8))
                .foreground(gpui::white())
                .border(gpui::transparent_black())
                .shadow(false),
        )
    }
}

// ---- Chips and text ---------------------------------------------------------------------------

/// A small rounded label: versions, kinds, counts.
pub fn chip(text: impl Into<SharedString>, tint: Option<Hsla>, cx: &App) -> Div {
    let fg = tint.unwrap_or(cx.theme().muted_foreground);
    div()
        .flex_none()
        .px(px(7.))
        .py(px(1.))
        .rounded_full()
        .bg(fg.opacity(0.13))
        .text_color(fg)
        .text_xs()
        .font_weight(FontWeight::MEDIUM)
        .child(text.into())
}

/// Which Claude a row is about, in words as well as an icon: the two icons
/// alone look alike at this size.
pub fn kind_tag(app: bool, cli: bool, cx: &App) -> Div {
    let (name, label) = match (app, cli) {
        (true, true) => ("app-window", "App + CLI"),
        (true, false) => ("app-window", "App"),
        _ => ("square-terminal", "CLI"),
    };
    let fg = cx.theme().muted_foreground;
    div()
        .flex_none()
        .flex()
        .items_center()
        .gap_1()
        .px(px(5.))
        .py(px(2.))
        .rounded(px(4.))
        .bg(fg.opacity(0.12))
        .text_color(fg)
        .child(icon("app-window").path(SharedString::from(format!("icons/{name}.svg"))).xsmall())
        .child(div().text_size(px(10.)).font_weight(FontWeight::SEMIBOLD).child(label))
}

pub fn dot(color: Hsla) -> Div {
    div().flex_none().size(px(9.)).rounded_full().bg(color)
}

pub fn secondary(text: impl Into<SharedString>, cx: &App) -> Div {
    div().text_xs().text_color(cx.theme().muted_foreground).child(text.into())
}
