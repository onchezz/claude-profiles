// Turning the command's JSON into words: sizes, paths, accounts, times.

use crate::prelude::*;

pub(crate) fn memory(mb: u64) -> String {
    if mb >= 1024 {
        format!("{:.1} GB", mb as f64 / 1024.0)
    } else {
        format!("{mb} MB")
    }
}

pub(crate) fn short_path(p: &str) -> String {
    let home = cli::home().to_string_lossy().to_string();
    match p.strip_prefix(&home) {
        Some(rest) if !home.is_empty() => format!("~{rest}"),
        _ => p.to_string(),
    }
}

/// "email · organisation", leaving out an organisation that only repeats who.
pub(crate) fn account_label(a: &Value) -> Option<String> {
    let who = a["email"].as_str().or(a["name"].as_str())?;
    match a["organization"].as_str() {
        Some(org) if !org.is_empty() && !org.to_lowercase().contains(&who.to_lowercase()) => Some(format!("{who} · {org}")),
        _ => Some(who.to_string()),
    }
}

/// Claude Desktop records only an account ID; name it as best we can.
pub(crate) fn desktop_account_label(a: &Value) -> String {
    if a.is_null() {
        return "Not signed in".into();
    }
    if let Some(m) = account_label(&a["matched"]) {
        return m;
    }
    if let Some(n) = a["nickname"].as_str().filter(|n| !n.is_empty()) {
        return n.to_string();
    }
    format!("Account {}", a["accountUuid"].as_str().unwrap_or("").chars().take(8).collect::<String>())
}

pub(crate) fn resets(at: &Value) -> String {
    let Some(t) = at.as_str().and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok()) else { return String::new() };
    let local = t.with_timezone(&chrono::Local);
    let today = local.date_naive() == chrono::Local::now().date_naive();
    format!("resets {}", local.format(if today { "%-I:%M %p" } else { "%a %-I:%M %p" }))
}

pub(crate) fn sessions_of(v: &Value) -> Vec<Value> {
    v.as_array().cloned().unwrap_or_default()
}

/// What a typed profile name becomes: lowercase, spaces and underscores as
/// hyphens, anything else dropped (the same as the menu bar app).
pub(crate) fn profile_name(typed: &str) -> String {
    let mut out = String::new();
    for ch in typed.to_lowercase().chars() {
        if ch == ' ' || ch == '_' || ch == '-' {
            if !out.ends_with('-') {
                out.push('-');
            }
        } else if ch.is_ascii_alphanumeric() {
            out.push(ch);
        }
    }
    out.trim_matches('-').to_string()
}


#[cfg(test)]
mod tests {
    #[test]
    fn typed_names_become_profile_names() {
        assert_eq!(super::profile_name("Client ACME"), "client-acme");
        assert_eq!(super::profile_name("  work__2 "), "work-2");
        assert_eq!(super::profile_name("é!"), "");
    }

    #[test]
    fn organisations_that_repeat_the_person_are_left_out() {
        let a = serde_json::json!({ "email": "me@x.com", "organization": "me@x.com's Organization" });
        assert_eq!(super::account_label(&a).as_deref(), Some("me@x.com"));
        let b = serde_json::json!({ "email": "me@x.com", "organization": "Acme" });
        assert_eq!(super::account_label(&b).as_deref(), Some("me@x.com · Acme"));
    }
}
