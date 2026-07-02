//! **AreaFix** — netmail-driven echo subscription management (FSC-0057 style).
//!
//! A downlink self-manages its echo subscriptions by sending a netmail to the
//! reserved user name `AreaFix` (or `AreaMgr`) on the uplink. The message *body*
//! is a small command language; the tosser parses it, applies it to that node's
//! subscription list, and replies by netmail. This module implements the pure
//! halves: parsing the body into commands and computing the reply.
//!
//! # Body grammar
//!
//! ```text
//!   <password>          first content line: the shared AreaFix password
//!   +AREA.TAG           subscribe (link) to an area
//!   -AREA.TAG           unsubscribe (unlink) from an area
//!   AREA.TAG            bare tag: toggle subscription
//!   %LIST               list all areas the uplink offers
//!   %QUERY              list the areas this node is subscribed to
//!   %HELP               return the command help text
//!   %<other>            any other percent command (kept verbatim)
//!   --- tearline        ends command parsing; origin/SEEN-BY/kludges ignored
//! ```
//!
//! Percent commands are case-insensitive; area tags are compared
//! case-insensitively but preserved as written. [`parse`] never fails — it
//! extracts whatever it can and ignores noise, because AreaFix bodies are
//! human-typed. [`process`] applies a parsed request against an
//! [`AreaFixConfig`] and returns the resulting subscription set plus the reply
//! body a tosser would send back.

/// One parsed AreaFix command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// Subscribe (link) to an area (`+TAG`).
    Subscribe(String),
    /// Unsubscribe (unlink) from an area (`-TAG`).
    Unsubscribe(String),
    /// Toggle subscription for a bare area tag.
    Toggle(String),
    /// `%LIST` — enumerate all offered areas.
    List,
    /// `%QUERY` — enumerate this node's subscriptions.
    Query,
    /// `%HELP` — return help text.
    Help,
    /// Any other `%command`, stored without the leading `%`, uppercased.
    Other(String),
}

/// A parsed AreaFix request: the password line plus the ordered commands.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AreaFixRequest {
    /// The password taken from the first content line, if any.
    pub password: Option<String>,
    /// Commands in the order they appeared.
    pub commands: Vec<Command>,
}

/// Uplink-side configuration used to apply a request.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AreaFixConfig {
    /// The password this node must supply.
    pub password: String,
    /// Every area tag the uplink offers, in presentation order.
    pub available: Vec<String>,
    /// The tags this node is currently subscribed to.
    pub subscribed: Vec<String>,
}

/// The outcome of applying a request: the new subscription set and the reply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AreaFixResult {
    /// Whether the supplied password matched (`false` ⇒ nothing was changed).
    pub authenticated: bool,
    /// The resulting subscription list (unchanged when unauthenticated).
    pub subscribed: Vec<String>,
    /// The netmail reply body a tosser would send back (CR-free, `\n` lines).
    pub response_body: String,
}

/// Parse a raw netmail body into an [`AreaFixRequest`]. Total: never panics.
///
/// Accepts `\r`, `\n`, or `\r\n` line breaks. The first non-empty content line
/// becomes the [`password`](AreaFixRequest::password) unless it is itself a
/// command (`+`, `-`, or `%`), in which case there is no password line. SOH
/// (`\x01`) kludge lines, the tearline (`---`), the origin, and `SEEN-BY:` /
/// `PATH:` control lines terminate or are skipped so trailing metadata never
/// parses as commands.
pub fn parse(body: &[u8]) -> AreaFixRequest {
    let mut req = AreaFixRequest::default();
    let mut want_password = true;

    // Split on raw CR/LF *before* CP437 decoding: the decoder maps 0x0D/0x0A to
    // glyphs (♪/◙), so line breaks must be found in the byte stream.
    for seg in body.split(|&b| b == b'\r' || b == b'\n') {
        // SOH (0x01) kludge lines are detected on the raw byte, since CP437
        // decoding would turn 0x01 into a visible glyph (☺).
        if seg.first() == Some(&0x01) {
            continue;
        }
        let decoded = crate::cp437::decode(seg);
        let line = decoded.trim();
        if line.is_empty() {
            continue;
        }
        // Trailer control lines are skipped so they never parse as commands.
        if line.starts_with("SEEN-BY:") {
            continue;
        }
        if line.starts_with("---") || line.starts_with("* Origin:") {
            break;
        }

        let is_command = line.starts_with('+') || line.starts_with('-') || line.starts_with('%');
        if want_password {
            want_password = false;
            if !is_command {
                req.password = Some(line.to_string());
                continue;
            }
        }

        if let Some(cmd) = parse_command(line) {
            req.commands.push(cmd);
        }
    }
    req
}

fn parse_command(line: &str) -> Option<Command> {
    if let Some(rest) = line.strip_prefix('+') {
        let tag = rest.trim();
        return (!tag.is_empty()).then(|| Command::Subscribe(tag.to_string()));
    }
    if let Some(rest) = line.strip_prefix('-') {
        let tag = rest.trim();
        return (!tag.is_empty()).then(|| Command::Unsubscribe(tag.to_string()));
    }
    if let Some(rest) = line.strip_prefix('%') {
        let word = rest.trim().to_ascii_uppercase();
        return Some(match word.as_str() {
            "LIST" => Command::List,
            "QUERY" => Command::Query,
            "HELP" => Command::Help,
            _ => Command::Other(word),
        });
    }
    // A bare token (possibly followed by garbage) is a toggle on the first word.
    let tag = line.split_whitespace().next()?;
    Some(Command::Toggle(tag.to_string()))
}

/// Apply a parsed request against `cfg`, returning the new subscription set and
/// the reply body. Pure: `cfg` is not mutated.
///
/// Subscriptions only change when the password matches (case-sensitively, as
/// FidoNet passwords are). An area can only be subscribed if the uplink offers
/// it (present in [`AreaFixConfig::available`], compared case-insensitively).
pub fn process(req: &AreaFixRequest, cfg: &AreaFixConfig) -> AreaFixResult {
    let authenticated = req.password.as_deref() == Some(cfg.password.as_str());
    let mut lines: Vec<String> = Vec::new();

    if !authenticated {
        lines.push("Incorrect or missing password. No commands were processed.".to_string());
        return AreaFixResult {
            authenticated: false,
            subscribed: cfg.subscribed.clone(),
            response_body: finish(lines),
        };
    }

    let mut subs = cfg.subscribed.clone();
    for cmd in &req.commands {
        match cmd {
            Command::Subscribe(tag) => lines.push(do_subscribe(&mut subs, cfg, tag)),
            Command::Unsubscribe(tag) => lines.push(do_unsubscribe(&mut subs, tag)),
            Command::Toggle(tag) => {
                if contains_ci(&subs, tag) {
                    lines.push(do_unsubscribe(&mut subs, tag));
                } else {
                    lines.push(do_subscribe(&mut subs, cfg, tag));
                }
            }
            Command::List => {
                lines.push("Available areas (* = linked):".to_string());
                for a in &cfg.available {
                    let mark = if contains_ci(&subs, a) { '*' } else { ' ' };
                    lines.push(format!("{mark} {a}"));
                }
            }
            Command::Query => {
                lines.push("Your linked areas:".to_string());
                if subs.is_empty() {
                    lines.push("  (none)".to_string());
                } else {
                    for a in &subs {
                        lines.push(format!("  {a}"));
                    }
                }
            }
            Command::Help => lines.extend(help_lines()),
            Command::Other(word) => lines.push(format!("Unknown command: %{word}")),
        }
    }

    if req.commands.is_empty() {
        lines.push("No commands were given.".to_string());
    }

    AreaFixResult {
        authenticated: true,
        subscribed: subs,
        response_body: finish(lines),
    }
}

fn do_subscribe(subs: &mut Vec<String>, cfg: &AreaFixConfig, tag: &str) -> String {
    if !contains_ci(&cfg.available, tag) {
        return format!("-{tag}: not available here");
    }
    if contains_ci(subs, tag) {
        return format!("+{tag}: already linked");
    }
    subs.push(tag.to_string());
    format!("+{tag}: linked")
}

fn do_unsubscribe(subs: &mut Vec<String>, tag: &str) -> String {
    if let Some(pos) = subs.iter().position(|a| a.eq_ignore_ascii_case(tag)) {
        subs.remove(pos);
        format!("-{tag}: unlinked")
    } else {
        format!("-{tag}: not linked")
    }
}

fn contains_ci(haystack: &[String], needle: &str) -> bool {
    haystack.iter().any(|a| a.eq_ignore_ascii_case(needle))
}

fn help_lines() -> Vec<String> {
    vec![
        "AreaFix commands:".to_string(),
        "  +TAG     link (subscribe to) an area".to_string(),
        "  -TAG     unlink (unsubscribe from) an area".to_string(),
        "  TAG      toggle an area".to_string(),
        "  %LIST    list all available areas".to_string(),
        "  %QUERY   list your linked areas".to_string(),
        "  %HELP    show this help".to_string(),
    ]
}

fn finish(lines: Vec<String>) -> String {
    let mut body = lines.join("\n");
    body.push('\n');
    body
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> AreaFixConfig {
        AreaFixConfig {
            password: "s3cret".to_string(),
            available: vec!["R20.GENERAL".into(), "R20.CHAT".into(), "FIDO.SYSOP".into()],
            subscribed: vec!["R20.GENERAL".into()],
        }
    }

    #[test]
    fn parses_password_and_commands() {
        let body = b"s3cret\r+R20.CHAT\r-R20.GENERAL\r%LIST\r%QUERY\r%HELP\rFIDO.SYSOP\r";
        let req = parse(body);
        assert_eq!(req.password.as_deref(), Some("s3cret"));
        assert_eq!(
            req.commands,
            vec![
                Command::Subscribe("R20.CHAT".into()),
                Command::Unsubscribe("R20.GENERAL".into()),
                Command::List,
                Command::Query,
                Command::Help,
                Command::Toggle("FIDO.SYSOP".into()),
            ]
        );
    }

    #[test]
    fn parse_handles_crlf_and_trailers() {
        let body = b"pw\r\n+AREA.A\r\n--- tearline\r\n+SHOULD.NOT.APPEAR\r\n";
        let req = parse(body);
        assert_eq!(req.password.as_deref(), Some("pw"));
        assert_eq!(req.commands, vec![Command::Subscribe("AREA.A".into())]);
    }

    #[test]
    fn parse_skips_kludge_and_seenby() {
        let body = b"pw\r\x01MSGID: 2:280/464 1\r+AREA.A\rSEEN-BY: 280/464\r";
        let req = parse(body);
        assert_eq!(req.commands, vec![Command::Subscribe("AREA.A".into())]);
    }

    #[test]
    fn parse_first_line_command_means_no_password() {
        let req = parse(b"+AREA.A\r-AREA.B\r");
        assert_eq!(req.password, None);
        assert_eq!(
            req.commands,
            vec![
                Command::Subscribe("AREA.A".into()),
                Command::Unsubscribe("AREA.B".into())
            ]
        );
    }

    #[test]
    fn parse_never_panics_on_junk() {
        for junk in [
            &b""[..],
            b"\x01\x01",
            b"+",
            b"-",
            b"%",
            b"---",
            &[0xffu8; 40],
        ] {
            let _ = parse(junk);
        }
    }

    #[test]
    fn process_rejects_bad_password() {
        let req = AreaFixRequest {
            password: Some("wrong".into()),
            commands: vec![Command::Subscribe("R20.CHAT".into())],
        };
        let res = process(&req, &cfg());
        assert!(!res.authenticated);
        assert_eq!(res.subscribed, vec!["R20.GENERAL".to_string()]);
        assert!(res.response_body.contains("password"));
    }

    #[test]
    fn process_subscribe_unsubscribe_toggle() {
        let req = AreaFixRequest {
            password: Some("s3cret".into()),
            commands: vec![
                Command::Subscribe("R20.CHAT".into()),
                Command::Unsubscribe("R20.GENERAL".into()),
                Command::Toggle("FIDO.SYSOP".into()), // not linked -> link
                Command::Subscribe("NO.SUCH.AREA".into()),
                Command::Subscribe("R20.CHAT".into()), // already linked now
            ],
        };
        let res = process(&req, &cfg());
        assert!(res.authenticated);
        assert_eq!(
            res.subscribed,
            vec!["R20.CHAT".to_string(), "FIDO.SYSOP".to_string()]
        );
        assert!(res.response_body.contains("+R20.CHAT: linked"));
        assert!(res.response_body.contains("-R20.GENERAL: unlinked"));
        assert!(res.response_body.contains("+FIDO.SYSOP: linked"));
        assert!(res
            .response_body
            .contains("NO.SUCH.AREA: not available here"));
        assert!(res.response_body.contains("+R20.CHAT: already linked"));
    }

    #[test]
    fn process_list_marks_subscribed() {
        let req = AreaFixRequest {
            password: Some("s3cret".into()),
            commands: vec![Command::List],
        };
        let res = process(&req, &cfg());
        assert!(res.response_body.contains("* R20.GENERAL"));
        assert!(res.response_body.contains("  R20.CHAT"));
    }

    #[test]
    fn process_query_lists_subscriptions() {
        let req = AreaFixRequest {
            password: Some("s3cret".into()),
            commands: vec![Command::Query],
        };
        let res = process(&req, &cfg());
        assert!(res.response_body.contains("R20.GENERAL"));
    }

    #[test]
    fn process_toggle_off_when_linked() {
        let req = AreaFixRequest {
            password: Some("s3cret".into()),
            commands: vec![Command::Toggle("r20.general".into())], // case-insensitive
        };
        let res = process(&req, &cfg());
        assert!(res.subscribed.is_empty());
        assert!(res.response_body.contains("unlinked"));
    }

    #[test]
    fn end_to_end_parse_then_process() {
        let req = parse(b"s3cret\r+R20.CHAT\r%QUERY\r");
        let res = process(&req, &cfg());
        assert!(res.subscribed.contains(&"R20.CHAT".to_string()));
    }
}
