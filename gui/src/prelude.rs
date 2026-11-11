// What every part of the window imports, in one place.

pub(crate) use std::collections::HashSet;
pub(crate) use std::path::Path;
pub(crate) use std::rc::Rc;
pub(crate) use std::time::{Duration, Instant};

pub(crate) use gpui::prelude::FluentBuilder;
pub(crate) use gpui::*;
pub(crate) use gpui_component::checkbox::Checkbox;
pub(crate) use gpui_component::input::{Input, InputEvent, InputState};
pub(crate) use gpui_component::menu::{DropdownMenu, PopupMenuItem};
pub(crate) use gpui_component::progress::Progress;
pub(crate) use gpui_component::{ActiveTheme, Disableable, Root, Sizable, StyledExt, Theme};
pub(crate) use serde_json::{json, Value};

// Named, not globbed: gpui also has red() and green().
pub(crate) use crate::design::{
    accent, action, card_bg, chip, confirm, danger_icon, dot, green, icon, icon_button, kind_tag, named, orange, quiet, red,
    secondary, warn_action, window_bg, Assets, GLASS,
};

pub(crate) use crate::app::*;
pub(crate) use crate::cleanup::Cleanup;
pub(crate) use crate::cli;
pub(crate) use crate::format::*;
pub(crate) use crate::id;
pub(crate) use crate::view::{desktop_tasks, warning};
