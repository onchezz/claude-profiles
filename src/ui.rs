// Terminal output and interactive prompts.
//
// Output helpers have semantic names (`ok`, `warn`, `info`) rather than colour
// names, so call sites read like prose. Styling is skipped when stdout is not
// a terminal, so piped output stays clean.
//
// Prompts wrap dialoguer. Ctrl+C at any prompt becomes `Cancelled`, which
// main turns into a clean "Cancelled." and exit status 130 instead of a stack
// of error text.

use std::fmt;
use std::io::IsTerminal;

use anyhow::Result;
use dialoguer::theme::ColorfulTheme;

use std::sync::atomic::{AtomicBool, Ordering};

/// Set by the machine interface (`claude-profiles app ...`): progress text
/// then goes to stderr, keeping stdout for the one JSON reply.
static QUIET: AtomicBool = AtomicBool::new(false);

pub fn set_quiet() {
    QUIET.store(true, Ordering::Relaxed);
}

macro_rules! say {
    ($($arg:tt)*) => {
        if QUIET.load(Ordering::Relaxed) { eprintln!($($arg)*) } else { println!($($arg)*) }
    };
}

fn styled(s: &str, codes: &[&str]) -> String {
    if QUIET.load(Ordering::Relaxed) || !std::io::stdout().is_terminal() {
        return s.to_string();
    }
    let mut out = String::new();
    for c in codes {
        out.push_str(match *c {
            "bold" => "\x1b[1m",
            "dim" => "\x1b[2m",
            "red" => "\x1b[31m",
            "green" => "\x1b[32m",
            "yellow" => "\x1b[33m",
            "blue" => "\x1b[34m",
            "cyan" => "\x1b[36m",
            "gray" => "\x1b[90m",
            _ => "",
        });
    }
    out.push_str(s);
    out.push_str("\x1b[0m");
    out
}

pub fn ok(msg: impl AsRef<str>) {
    say!("{}{}", styled("✓ ", &["green", "bold"]), msg.as_ref());
}

pub fn warn(msg: impl AsRef<str>) {
    say!("{}{}", styled("⚠ ", &["yellow", "bold"]), msg.as_ref());
}

pub fn err(msg: impl AsRef<str>) {
    eprintln!("{}{}", styled("✗ ", &["red", "bold"]), msg.as_ref());
}

pub fn info(msg: impl AsRef<str>) {
    say!("{}{}", styled("ℹ ", &["blue"]), msg.as_ref());
}

/// Between major phases, so the user can tell where we are.
pub fn step(msg: impl AsRef<str>) {
    say!("\n{}", styled(&format!("→ {}", msg.as_ref()), &["cyan", "bold"]));
}

/// Once at the top of a command.
pub fn header(msg: &str) {
    let bar = "─".repeat((msg.chars().count() + 4).min(60));
    say!("\n{}", styled(&bar, &["gray"]));
    say!("  {}", styled(msg, &["bold"]));
    say!("{}\n", styled(&bar, &["gray"]));
}

/// Multi-line commentary, dimmed so it reads as explanation rather than
/// instruction. Leading and trailing blank lines are dropped and the common
/// indentation removed, so call sites can indent the text with the code.
pub fn explain(text: &str) {
    let lines: Vec<&str> = text.trim_matches('\n').lines().collect();
    let indent = lines
        .iter()
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.len() - l.trim_start().len())
        .min()
        .unwrap_or(0);
    for line in lines {
        let shown = if line.len() >= indent { &line[indent..] } else { line.trim_start() };
        say!("{}", styled(shown, &["dim"]));
    }
    say!("");
}

/// A shell command the user could run.
pub fn command(cmd: impl AsRef<str>) -> String {
    styled(cmd.as_ref(), &["cyan"])
}

/// A filesystem path, styled the same way everywhere.
pub fn path_str(p: impl AsRef<str>) -> String {
    styled(p.as_ref(), &["yellow"])
}

/// Secondary or annotative text.
pub fn dim(s: impl AsRef<str>) -> String {
    styled(s.as_ref(), &["dim"])
}

pub fn blank() {
    say!("");
}

// ---- Prompts ----------------------------------------------------------------

/// Ctrl+C at a prompt.
#[derive(Debug)]
pub struct Cancelled;

impl fmt::Display for Cancelled {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Cancelled.")
    }
}

impl std::error::Error for Cancelled {}

fn map_err(e: dialoguer::Error) -> anyhow::Error {
    let dialoguer::Error::IO(io) = &e;
    if io.kind() == std::io::ErrorKind::Interrupted {
        // dialoguer leaves the cursor hidden when interrupted.
        let _ = console::Term::stderr().show_cursor();
        return Cancelled.into();
    }
    e.into()
}

fn theme() -> ColorfulTheme {
    ColorfulTheme::default()
}

pub fn confirm(message: &str, default: bool) -> Result<bool> {
    dialoguer::Confirm::with_theme(&theme())
        .with_prompt(message)
        .default(default)
        .interact()
        .map_err(map_err)
}

/// Free text, with an optional default and a validator that returns the
/// message to show when the input is not acceptable.
pub fn input(
    message: &str,
    default: Option<&str>,
    validate: impl Fn(&str) -> std::result::Result<(), String>,
) -> Result<String> {
    let theme = theme();
    let mut p = dialoguer::Input::<String>::with_theme(&theme).with_prompt(message);
    if let Some(d) = default {
        p = p.default(d.to_string());
    }
    // An Input with no default refuses empty answers outright, which would
    // stop "leave blank to skip" questions from ever being skipped.
    p = p.allow_empty(true);
    p.validate_with(|v: &String| validate(v)).interact_text().map_err(map_err)
}

/// One choice from a list. `items` are (label, value) pairs.
pub fn select<T: Clone>(message: &str, items: &[(String, T)], default: usize) -> Result<T> {
    let labels: Vec<&str> = items.iter().map(|(l, _)| l.as_str()).collect();
    let i = dialoguer::Select::with_theme(&theme())
        .with_prompt(message)
        .items(&labels)
        .default(default)
        .max_length(14)
        .interact()
        .map_err(map_err)?;
    Ok(items[i].1.clone())
}

/// Any number of choices. `items` are (label, value, checked) triples.
pub fn checkbox<T: Clone>(message: &str, items: &[(String, T, bool)]) -> Result<Vec<T>> {
    let labels: Vec<&str> = items.iter().map(|(l, _, _)| l.as_str()).collect();
    let checked: Vec<bool> = items.iter().map(|(_, _, c)| *c).collect();
    let picked = dialoguer::MultiSelect::with_theme(&theme())
        .with_prompt(message)
        .items(&labels)
        .defaults(&checked)
        .max_length(15)
        .interact()
        .map_err(map_err)?;
    Ok(picked.into_iter().map(|i| items[i].1.clone()).collect())
}
