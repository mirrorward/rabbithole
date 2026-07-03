//! Radio playback handoff: the TUI **never decodes audio**. Per the plan the
//! terminal client does "now-playing + external player handoff" (PLAN.md,
//! client players), so this module owns exactly that handoff:
//!
//! - [`base_is_valid`] / [`stream_url`] — join a user-set delivery base and a
//!   station slug into `<base>/<station>`, mirroring `ui-web`'s validation
//!   rules exactly (scheme allowlist `http`/`https` with a non-empty host,
//!   trimmed padding, collapsed slashes, no whitespace in the slug).
//! - [`player_command`] — resolve the `$RABBIT_PLAYER` handoff command line:
//!   a program plus optional arguments, with the stream URL appended last
//!   (`RABBIT_PLAYER="mpv --no-video"` → `mpv --no-video <url>`).
//! - [`spawn_player`] — the one process edge: detach the player via
//!   `std::process::Command::spawn` with all stdio nulled so it can never
//!   scribble on the terminal; a reaper thread waits on the child so no
//!   zombies pile up. Failures come back as strings for the status line —
//!   never a panic, never a crashed TUI.
//!
//! The **radio base** itself is session-local: this crate has no settings
//! file or config persistence yet, so the base is typed per session (`b` in
//! the radio view) and forgotten on exit — the UI says so. When the TUI
//! grows a config store, persist the base there (ui-web keeps the same value
//! in `localStorage` under `rh-radio`).

use std::process::Stdio;

/// Environment variable naming the external player command line.
pub const PLAYER_ENV: &str = "RABBIT_PLAYER";

/// Whether `base` is a usable stream delivery address: an `http://` or
/// `https://` URL (scheme allowlist) with a non-empty host part. Mirrors
/// `ui-web`'s rule exactly.
pub fn base_is_valid(base: &str) -> bool {
    let base = base.trim().trim_end_matches('/');
    base.strip_prefix("http://")
        .or_else(|| base.strip_prefix("https://"))
        .is_some_and(|rest| !rest.is_empty())
}

/// Join the delivery `base` and a station slug into the stream URL the
/// player is handed: `<base>/<station>`. Returns `None` when the base is
/// invalid (see [`base_is_valid`]) or the slug is empty/whitespace. Mirrors
/// `ui-web`'s join exactly so both clients derive identical URLs.
pub fn stream_url(base: &str, station: &str) -> Option<String> {
    if !base_is_valid(base) {
        return None;
    }
    let base = base.trim().trim_end_matches('/');
    let station = station.trim().trim_matches('/');
    if station.is_empty() || station.contains(char::is_whitespace) {
        return None;
    }
    Some(format!("{base}/{station}"))
}

/// Split a `$RABBIT_PLAYER` value into program + arguments and append the
/// stream URL as the final argument. `None` when the value is blank.
pub fn player_command(spec: &str, url: &str) -> Option<(String, Vec<String>)> {
    let mut parts = spec.split_whitespace();
    let program = parts.next()?.to_string();
    let mut args: Vec<String> = parts.map(str::to_string).collect();
    args.push(url.to_string());
    Some((program, args))
}

/// Spawn the player detached (nulled stdio, no waiting on the UI thread).
/// A background thread reaps the child when it exits. Errors are returned
/// as display strings for the status line — this must never take the TUI
/// down.
pub fn spawn_player(program: &str, args: &[String]) -> Result<(), String> {
    match std::process::Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(mut child) => {
            std::thread::spawn(move || {
                let _ = child.wait();
            });
            Ok(())
        }
        Err(err) => Err(format!("player spawn failed ({program}): {err}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The join/validation vectors mirror ui-web's `radio::stream_url` tests
    // verbatim so the two clients cannot drift apart.

    #[test]
    fn stream_url_joins_base_and_mount() {
        assert_eq!(
            stream_url("http://host:8000", "live"),
            Some("http://host:8000/live".into())
        );
        // Trailing slashes and padding collapse.
        assert_eq!(
            stream_url(" http://host:8000/ ", "/live/"),
            Some("http://host:8000/live".into())
        );
        assert_eq!(
            stream_url("https://radio.example", "ambient"),
            Some("https://radio.example/ambient".into())
        );
    }

    #[test]
    fn stream_url_enforces_the_scheme_allowlist() {
        assert_eq!(stream_url("ftp://host:8000", "live"), None);
        assert_eq!(stream_url("host:8000", "live"), None);
        assert_eq!(stream_url("http://", "live"), None);
        assert_eq!(stream_url("", "live"), None);
        assert!(!base_is_valid("ws://host:9000"));
        assert!(base_is_valid("http://host:8000"));
        assert!(base_is_valid("https://host"));
    }

    #[test]
    fn stream_url_rejects_bad_stations() {
        assert_eq!(stream_url("http://host:8000", ""), None);
        assert_eq!(stream_url("http://host:8000", "  "), None);
        assert_eq!(stream_url("http://host:8000", "a b"), None);
    }

    #[test]
    fn player_command_splits_program_args_and_appends_url() {
        assert_eq!(
            player_command("mpv", "http://h:8000/live"),
            Some(("mpv".into(), vec!["http://h:8000/live".into()]))
        );
        assert_eq!(
            player_command("mpv --no-video --volume=50", "http://h:8000/live"),
            Some((
                "mpv".into(),
                vec![
                    "--no-video".into(),
                    "--volume=50".into(),
                    "http://h:8000/live".into()
                ]
            ))
        );
    }

    #[test]
    fn player_command_rejects_blank_specs() {
        assert_eq!(player_command("", "http://h/live"), None);
        assert_eq!(player_command("   ", "http://h/live"), None);
    }

    #[test]
    fn spawn_player_reports_missing_binaries_instead_of_crashing() {
        let err = spawn_player(
            "rabbithole-definitely-not-a-real-player",
            &["http://h/live".into()],
        )
        .unwrap_err();
        assert!(err.contains("player spawn failed"), "got: {err}");
    }
}
