//! Classic finger output formatting, defensively rendered.
//!
//! Formatters build responses with `\n` endings; [`to_wire`] is the single
//! choke point that sanitizes control characters, normalizes line endings to
//! CRLF, and caps total size before bytes leave the host. Profile fields are
//! member-controlled, so a hostile `.plan` (or screen name) must not be able
//! to smuggle terminal escape sequences into the *requester's* terminal:
//! everything below 0x20 except CR/LF/tab is stripped, as is DEL.

use crate::directory::{Profile, WhoEntry};

/// Hard cap on the size of any single finger response, in bytes on the wire.
pub const MAX_RESPONSE_BYTES: usize = 32 * 1024;

const TRUNCATION_NOTICE: &str = "*** output truncated ***\r\n";

/// Strip control characters that could carry terminal escapes.
///
/// Everything below 0x20 is dropped except CR, LF, and tab; DEL (0x7F) is
/// dropped too. This runs over member-controlled text (screen names,
/// profile fields, `.plan`) and again over the whole response in
/// [`to_wire`].
pub fn sanitize(input: &str) -> String {
    input
        .chars()
        .filter(|&c| c == '\t' || c == '\r' || c == '\n' || (c >= '\x20' && c != '\x7f'))
        .collect()
}

/// Sanitize a field that must stay on one line: control characters are
/// stripped and any embedded line breaks collapse to single spaces, so a
/// hostile field can't forge extra header lines or table rows.
fn sanitize_inline(input: &str) -> String {
    let clean = sanitize(input);
    let mut out = String::with_capacity(clean.len());
    let mut in_break = false;
    for c in clean.chars() {
        if c == '\r' || c == '\n' {
            if !in_break {
                out.push(' ');
                in_break = true;
            }
        } else {
            out.push(c);
            in_break = false;
        }
    }
    out
}

/// Render an idle time the classic terse way: `-` when active, then
/// minutes, hours+minutes, days+hours.
fn format_idle(secs: u64) -> String {
    if secs < 60 {
        "-".to_string()
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else if secs < 86_400 {
        format!("{}h{:02}m", secs / 3600, (secs % 3600) / 60)
    } else {
        format!("{}d{:02}h", secs / 86_400, (secs % 86_400) / 3600)
    }
}

/// Format the who's-online listing as a columned table (`\n` endings;
/// [`to_wire`] converts to CRLF).
pub fn format_who(entries: &[WhoEntry]) -> String {
    if entries.is_empty() {
        return "No one is currently in the warren.\n".to_string();
    }

    let rows: Vec<(String, String, String)> = entries
        .iter()
        .map(|e| {
            (
                sanitize_inline(&e.screen_name),
                format_idle(e.idle_secs),
                sanitize_inline(e.location.as_deref().unwrap_or("")),
            )
        })
        .collect();

    let login_w = rows
        .iter()
        .map(|(l, _, _)| l.chars().count())
        .chain(std::iter::once("Login".len()))
        .max()
        .unwrap_or(0)
        + 2;
    let idle_w = rows
        .iter()
        .map(|(_, i, _)| i.chars().count())
        .chain(std::iter::once("Idle".len()))
        .max()
        .unwrap_or(0)
        + 2;

    let mut out = String::new();
    out.push_str(&format!(
        "{:login_w$}{:idle_w$}{}\n",
        "Login", "Idle", "Location"
    ));
    for (login, idle, location) in &rows {
        let line = format!("{login:login_w$}{idle:idle_w$}{location}");
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out
}

/// Format a member profile: a header block of the shared fields, then the
/// `.plan` verbatim under a `Plan:` heading — or `No Plan.` when absent.
pub fn format_profile(profile: &Profile) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "Login: {}\n",
        sanitize_inline(&profile.screen_name)
    ));

    let fields: [(&str, &Option<String>); 5] = [
        ("Real name", &profile.real_name),
        ("Pronouns", &profile.pronouns),
        ("Location", &profile.location),
        ("Interests", &profile.interests),
        ("Quote", &profile.quote),
    ];
    for (label, value) in fields {
        if let Some(value) = value {
            out.push_str(&format!("{label}: {}\n", sanitize_inline(value)));
        }
    }

    out.push('\n');
    match &profile.plan {
        Some(plan) => {
            out.push_str("Plan:\n");
            out.push_str(&sanitize(plan));
            if !out.ends_with('\n') {
                out.push('\n');
            }
        }
        None => out.push_str("No Plan.\n"),
    }
    out
}

/// The polite brush-off for a name that isn't in the directory.
pub fn format_unknown(user: &str) -> String {
    format!("finger: {}: no such user.\n", sanitize_inline(user))
}

/// The polite refusal for `user@host` forwarding requests (RFC 1288 §3.2.1).
pub fn format_forward_refused() -> String {
    "finger: forwarding service denied.\n".to_string()
}

/// Prepare a formatted response for the wire: sanitize control characters,
/// normalize every line ending to CRLF, and cap the total size at
/// [`MAX_RESPONSE_BYTES`] (appending a truncation notice when the cap bites).
pub fn to_wire(text: &str) -> String {
    let clean = sanitize(text);
    let mut wire = String::with_capacity(clean.len() + 16);
    for c in clean.chars() {
        match c {
            // Bare CRs are dropped; CRLF pairs collapse into the LF arm.
            '\r' => {}
            '\n' => wire.push_str("\r\n"),
            _ => wire.push(c),
        }
    }

    if wire.len() > MAX_RESPONSE_BYTES {
        let mut cut = MAX_RESPONSE_BYTES - TRUNCATION_NOTICE.len();
        while cut > 0 && !wire.is_char_boundary(cut) {
            cut -= 1;
        }
        wire.truncate(cut);
        // Never leave a dangling CR from a split CRLF pair.
        if wire.ends_with('\r') {
            wire.pop();
        }
        wire.push_str(TRUNCATION_NOTICE);
    }
    wire
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, idle: u64, loc: Option<&str>) -> WhoEntry {
        WhoEntry {
            screen_name: name.to_string(),
            idle_secs: idle,
            location: loc.map(str::to_string),
        }
    }

    #[test]
    fn sanitize_strips_escapes_but_keeps_whitespace() {
        let hostile = "safe\x1b[31mred\x07bell\x00nul\x7fdel\tok\r\nline";
        assert_eq!(sanitize(hostile), "safe[31mredbellnuldel\tok\r\nline");
    }

    #[test]
    fn inline_fields_cannot_forge_lines() {
        assert_eq!(
            sanitize_inline("Wonderland\r\nPlan:\nfake"),
            "Wonderland Plan: fake"
        );
    }

    #[test]
    fn idle_formatting_is_terse() {
        assert_eq!(format_idle(0), "-");
        assert_eq!(format_idle(59), "-");
        assert_eq!(format_idle(60), "1m");
        assert_eq!(format_idle(59 * 60), "59m");
        assert_eq!(format_idle(2 * 3600 + 5 * 60), "2h05m");
        assert_eq!(format_idle(3 * 86_400 + 4 * 3600), "3d04h");
    }

    #[test]
    fn who_table_is_columned() {
        let out = format_who(&[
            entry("alice", 0, Some("Wonderland")),
            entry("madhatter", 2 * 3600, None),
        ]);
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 3);
        assert!(lines[0].starts_with("Login"));
        assert!(lines[0].contains("Idle"));
        assert!(lines[0].contains("Location"));
        // Columns line up: "Idle" starts at the same offset in every row.
        let idle_col = lines[0].find("Idle").unwrap();
        assert_eq!(&lines[1][idle_col..idle_col + 1], "-");
        assert_eq!(&lines[2][idle_col..idle_col + 2], "2h");
        assert!(lines[1].ends_with("Wonderland"));
        assert_eq!(lines[2].trim_end(), lines[2], "no trailing whitespace");
    }

    #[test]
    fn who_table_handles_nobody_home() {
        assert_eq!(format_who(&[]), "No one is currently in the warren.\n");
    }

    #[test]
    fn profile_renders_header_and_plan_verbatim() {
        let p = Profile {
            screen_name: "alice".into(),
            real_name: Some("Alice Liddell".into()),
            pronouns: Some("she/her".into()),
            location: Some("Wonderland".into()),
            interests: Some("croquet, tea".into()),
            quote: Some("Curiouser and curiouser!".into()),
            plan: Some("1. Follow the white rabbit\n2. Tea at six".into()),
        };
        let out = format_profile(&p);
        assert!(out.starts_with("Login: alice\n"));
        assert!(out.contains("Real name: Alice Liddell\n"));
        assert!(out.contains("Pronouns: she/her\n"));
        assert!(out.contains("Quote: Curiouser and curiouser!\n"));
        assert!(out.contains("\nPlan:\n1. Follow the white rabbit\n2. Tea at six\n"));
    }

    #[test]
    fn profile_without_plan_says_no_plan() {
        let p = Profile {
            screen_name: "dormouse".into(),
            ..Profile::default()
        };
        let out = format_profile(&p);
        assert_eq!(out, "Login: dormouse\n\nNo Plan.\n");
        assert!(!out.contains("Plan:\n"));
    }

    #[test]
    fn hostile_plan_is_defanged() {
        let p = Profile {
            screen_name: "mole".into(),
            plan: Some("\x1b]0;pwned\x07\x1b[2Jclean text".into()),
            ..Profile::default()
        };
        let out = format_profile(&p);
        assert!(!out.contains('\x1b'));
        assert!(!out.contains('\x07'));
        assert!(out.contains("clean text"));
    }

    #[test]
    fn unknown_user_echo_is_sanitized() {
        let out = format_unknown("evil\x1b[31m\r\nname");
        assert_eq!(out, "finger: evil[31m name: no such user.\n");
    }

    #[test]
    fn to_wire_uses_crlf_everywhere() {
        let wire = to_wire("a\nb\r\nc\rd\n");
        assert_eq!(wire, "a\r\nb\r\ncd\r\n");
    }

    #[test]
    fn to_wire_caps_response_size() {
        let big = "x".repeat(MAX_RESPONSE_BYTES) + "\ntail";
        let wire = to_wire(&big);
        assert!(wire.len() <= MAX_RESPONSE_BYTES);
        assert!(wire.ends_with(TRUNCATION_NOTICE));
        assert!(!wire.contains("tail"));
    }
}
