pub mod add;
pub mod app;
pub mod doctor;
pub mod extensions;
pub mod list;
pub mod remove;
pub mod rename;
pub mod repair;
pub mod selfupdate;
pub mod status;
pub mod upgrade;

use std::ffi::OsStr;
use std::path::Path;

use crate::util::{run_quiet, LSREGISTER};

/// Profile names the tool refuses outright, with what each would collide with.
pub const RESERVED_NAMES: &[(&str, &str)] = &[
    ("mem", "claude-mem (~/.claude-mem)"),
    // ~/.claude-profiles belongs to an unrelated tool of the same name.
    ("profiles", "another tool's folder (~/.claude-profiles)"),
    ("multiprofile", "claude-multiprofile (~/.claude-multiprofile)"),
    ("code", "reserved (~/.claude-code)"),
    ("desktop", "reserved (~/.claude-desktop)"),
    // Short names are uppercased for the Desktop data folder, so "3p" would
    // get Claude-3P, which on a case-insensitive disk IS Claude-3p, where the
    // main Claude keeps its local configuration.
    ("3p", "the main Claude's local configuration folder (~/Library/Application Support/Claude-3p)"),
];

pub fn reserved(name: &str) -> Option<&'static str> {
    RESERVED_NAMES.iter().find(|(n, _)| *n == name).map(|(_, why)| *why)
}

/// Everything the tool names already starts with Claude, so a name that
/// starts with "claude-" doubles it ("Claude Claude Qpg.app"). Returns the
/// name without the prefix, or None.
pub fn without_claude_prefix(name: &str) -> Option<String> {
    let rest = name.strip_prefix("claude")?;
    let trimmed = rest.trim_start_matches('-');
    (rest.starts_with('-') && !trimmed.is_empty()).then(|| trimmed.to_string())
}

pub fn lsregister(flag: &str, app: &Path) -> bool {
    run_quiet(LSREGISTER, [OsStr::new(flag), app.as_os_str()])
}

/// Print an error and stop with status 1.
pub fn fail(msg: impl AsRef<str>) -> ! {
    crate::ui::err(msg);
    std::process::exit(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_prefix_is_spotted() {
        assert_eq!(without_claude_prefix("claude-qpg").as_deref(), Some("qpg"));
        assert_eq!(without_claude_prefix("claude--x").as_deref(), Some("x"));
        assert_eq!(without_claude_prefix("claude"), None);
        assert_eq!(without_claude_prefix("claude-"), None);
        assert_eq!(without_claude_prefix("claudette"), None);
        assert_eq!(without_claude_prefix("work"), None);
    }
}
