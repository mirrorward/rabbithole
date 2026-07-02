//! Mid-stream metadata updates: the `/admin/metadata` (Icecast 2) and
//! `admin.cgi` (SHOUTcast v1) title-change request, plus the server's replies.
//!
//! Encoders that push audio over one socket announce track changes over a
//! *second*, short-lived HTTP request:
//!
//! ```text
//! GET /admin/metadata?pass=hackme&mode=updinfo&mount=/live&song=Daft+Punk+-+Da+Funk HTTP/1.0
//! Authorization: Basic c291cmNlOmhhY2ttZQ==      <- alternative to pass=...
//! User-Agent: (Mozilla Compatible)
//! ```
//!
//! The SHOUTcast v1 spelling is the same query on a different path (and has no
//! `mount` — the station is implied by the port):
//!
//! ```text
//! GET /admin.cgi?pass=hackme&mode=updinfo&song=Daft+Punk+-+Da+Funk HTTP/1.0
//! ```
//!
//! [`parse_metadata_update`] decodes both, percent-decoding the query
//! ([`percent_decode`]; `+` means space) and accepting credentials either as
//! `pass=`/`user=` query parameters or an `Authorization: Basic` header
//! (decoded via [`crate::parse_basic_auth`]; the header, when decodable, wins).
//! Parsing is total: malformed input yields an [`IcyError`], never a panic.
//!
//! ## Reply convention
//!
//! The response builders follow **Icecast 2's admin convention**: an
//! `HTTP/1.0` status line and a tiny `text/xml` body,
//! `<iceresponse><message>…</message><return>1|0</return></iceresponse>`.
//! Classic SHOUTcast v1 encoders only inspect the status line (their
//! `admin.cgi` returned an HTML page nothing ever parsed), so the XML body is
//! the most useful common denominator: the `HTTP/1.0 200` satisfies v1
//! encoders while Icecast-style tooling gets the machine-readable `<return>`
//! flag. Failures that are *policy* failures (bad mount, no such stream) keep
//! the `200` status with `<return>0</return>` — again matching Icecast —
//! while failed authentication answers `401` with a Basic challenge.

use crate::http::RequestHead;
use crate::source::parse_basic_auth;
use crate::IcyError;

/// A parsed `/admin/metadata` / `admin.cgi` metadata (title) update request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MetadataUpdate {
    /// Mount the update targets (`mount=`), e.g. `/live`. `None` for the
    /// SHOUTcast `admin.cgi` form, where the station is implied by the port.
    pub mount: Option<String>,
    /// Username, from `Authorization: Basic` (or a `user=` query parameter).
    /// SHOUTcast v1 encoders send only a password.
    pub user: Option<String>,
    /// Password, from `pass=` or `Authorization: Basic`. The header, when
    /// present and decodable, overrides the query form.
    pub pass: Option<String>,
    /// The new track title (`song=`), percent-decoded. May be empty — that is
    /// a legitimate "clear the title" update.
    pub song: String,
    /// Optional track/station URL (`url=`), percent-decoded. Empty values are
    /// normalised to `None`.
    pub url: Option<String>,
}

/// Parses a metadata-update request from its raw request bytes (request line
/// plus headers, up to and optionally including the blank-line terminator).
///
/// Accepts `GET /admin/metadata?...` (Icecast 2) and `GET /admin.cgi?...`
/// (SHOUTcast v1); the path and all query parameter names are matched
/// case-insensitively, and repeated parameters take the last value.
///
/// Errors:
/// - [`IcyError::UnsupportedMethod`] for a non-`GET` request,
/// - [`IcyError::UnsupportedTarget`] when the path is neither admin endpoint,
/// - [`IcyError::UnsupportedMode`] when `mode` is missing or not `updinfo`,
/// - [`IcyError::MissingParameter`] when `song` is absent (an *empty* `song=`
///   is fine — it clears the title),
/// - plus the usual [`IcyError`] variants for an empty/malformed request line.
///
/// Missing credentials are tolerated (both `user` and `pass` come back
/// `None`) so the caller owns the auth decision and can answer with
/// [`metadata_update_unauthorized`].
pub fn parse_metadata_update(raw: &[u8]) -> Result<MetadataUpdate, IcyError> {
    let head = RequestHead::parse(raw)?;
    if !head.method.eq_ignore_ascii_case("GET") {
        return Err(IcyError::UnsupportedMethod(head.method));
    }

    let (path, query) = match head.target.split_once('?') {
        Some((p, q)) => (p, q),
        None => (head.target.as_str(), ""),
    };
    if !path.eq_ignore_ascii_case("/admin/metadata") && !path.eq_ignore_ascii_case("/admin.cgi") {
        return Err(IcyError::UnsupportedTarget(path.to_string()));
    }

    let mut mode = None;
    let mut mount = None;
    let mut user = None;
    let mut pass = None;
    let mut song = None;
    let mut url = None;
    for pair in query.split('&').filter(|p| !p.is_empty()) {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        let key = percent_decode(key).to_ascii_lowercase();
        let value = percent_decode(value);
        match key.as_str() {
            "mode" => mode = Some(value),
            "mount" => mount = Some(value),
            "user" => user = Some(value),
            "pass" => pass = Some(value),
            "song" => song = Some(value),
            "url" => url = Some(value),
            _ => {}
        }
    }

    match mode.as_deref() {
        Some(m) if m.eq_ignore_ascii_case("updinfo") => {}
        other => return Err(IcyError::UnsupportedMode(other.unwrap_or("").to_string())),
    }

    // Basic auth header (Icecast form) wins over query-string credentials.
    if let Some((u, p)) = head
        .header("authorization")
        .as_deref()
        .and_then(parse_basic_auth)
    {
        user = Some(u);
        pass = Some(p);
    }

    let song = song.ok_or_else(|| IcyError::MissingParameter("song".to_string()))?;

    Ok(MetadataUpdate {
        mount: mount.filter(|m| !m.is_empty()),
        user,
        pass,
        song,
        url: url.filter(|u| !u.is_empty()),
    })
}

/// Percent-decodes a URL query component. Total: never fails.
///
/// `+` decodes to a space (this is a query string, not a path) and `%XX`
/// decodes the two hex digits to a byte; a `%` not followed by two hex digits
/// passes through verbatim rather than erroring, and any resulting non-UTF-8
/// byte sequences are replaced lossily (U+FFFD).
pub fn percent_decode(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => match (hex_val(bytes[i + 1]), hex_val(bytes[i + 2])) {
                (Some(hi), Some(lo)) => {
                    out.push((hi << 4) | lo);
                    i += 3;
                }
                _ => {
                    out.push(b'%');
                    i += 1;
                }
            },
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Decodes one ASCII hex digit, or `None`.
fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Builds the success reply for an accepted metadata update: `HTTP/1.0 200 OK`
/// with the Icecast-convention `<iceresponse>…<return>1</return>` XML body
/// (see the [module docs](self) for why this shape).
pub fn metadata_update_ok() -> String {
    respond("200 OK", "", 1, "Metadata update successful")
}

/// Builds the policy-failure reply (unknown mount, stream not connected, …):
/// per Icecast convention still `HTTP/1.0 200 OK`, but with
/// `<return>0</return>` and the given human-readable message (XML-escaped).
pub fn metadata_update_failed(message: &str) -> String {
    respond("200 OK", "", 0, message)
}

/// Builds the `401 Unauthorized` reply for an update that failed auth,
/// including a `WWW-Authenticate: Basic` challenge (same realm as
/// [`crate::source_unauthorized`]) and a `<return>0</return>` body.
pub fn metadata_update_unauthorized() -> String {
    respond(
        "401 Unauthorized",
        "WWW-Authenticate: Basic realm=\"RabbitHole Radio\"\r\n",
        0,
        "Authentication Required",
    )
}

/// Renders a full `HTTP/1.0` response with an `<iceresponse>` XML body.
fn respond(status: &str, extra_headers: &str, ret: u8, message: &str) -> String {
    let body = format!(
        "<?xml version=\"1.0\"?>\n\
         <iceresponse><message>{}</message><return>{ret}</return></iceresponse>\n",
        xml_escape(message)
    );
    format!(
        "HTTP/1.0 {status}\r\n\
         {extra_headers}\
         Content-Type: text/xml\r\n\
         Content-Length: {}\r\n\
         \r\n\
         {body}",
        body.len()
    )
}

/// Escapes the five XML-special characters so caller-supplied failure messages
/// cannot break out of the `<message>` element.
fn xml_escape(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for c in raw.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '\'' => out.push_str("&apos;"),
            '"' => out.push_str("&quot;"),
            other => out.push(other),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use data_encoding::BASE64;

    fn b64(s: &str) -> String {
        BASE64.encode(s.as_bytes())
    }

    #[test]
    fn parses_icecast_form_with_query_pass() {
        let raw = b"GET /admin/metadata?pass=hackme&mode=updinfo&mount=/live&song=Daft+Punk+-+Da+Funk&url=http%3A%2F%2Fradio.example%2F HTTP/1.0\r\n\r\n";
        let upd = parse_metadata_update(raw).unwrap();
        assert_eq!(upd.mount.as_deref(), Some("/live"));
        assert_eq!(upd.user, None);
        assert_eq!(upd.pass.as_deref(), Some("hackme"));
        assert_eq!(upd.song, "Daft Punk - Da Funk");
        assert_eq!(upd.url.as_deref(), Some("http://radio.example/"));
    }

    #[test]
    fn parses_shoutcast_admin_cgi_spelling() {
        let raw = b"GET /admin.cgi?pass=changeme&mode=updinfo&song=Old+School HTTP/1.0\r\nUser-Agent: (Mozilla Compatible)\r\n\r\n";
        let upd = parse_metadata_update(raw).unwrap();
        assert_eq!(upd.mount, None); // implied by the port in SHOUTcast v1
        assert_eq!(upd.pass.as_deref(), Some("changeme"));
        assert_eq!(upd.song, "Old School");
        assert_eq!(upd.url, None);
    }

    #[test]
    fn basic_auth_header_wins_over_query_pass() {
        let raw = format!(
            "GET /admin/metadata?pass=stale&mode=updinfo&mount=/live&song=X HTTP/1.0\r\n\
             Authorization: Basic {}\r\n\r\n",
            b64("source:hackme")
        );
        let upd = parse_metadata_update(raw.as_bytes()).unwrap();
        assert_eq!(upd.user.as_deref(), Some("source"));
        assert_eq!(upd.pass.as_deref(), Some("hackme"));
    }

    #[test]
    fn undecodable_auth_header_keeps_query_credentials() {
        let raw =
            b"GET /admin/metadata?pass=ok&mode=updinfo&song=X HTTP/1.0\r\nAuthorization: Basic !!!\r\n\r\n";
        let upd = parse_metadata_update(raw).unwrap();
        assert_eq!(upd.user, None);
        assert_eq!(upd.pass.as_deref(), Some("ok"));
    }

    #[test]
    fn user_query_parameter_is_accepted() {
        let raw = b"GET /admin.cgi?user=admin&pass=p&mode=updinfo&song=S HTTP/1.0\r\n\r\n";
        let upd = parse_metadata_update(raw).unwrap();
        assert_eq!(upd.user.as_deref(), Some("admin"));
        assert_eq!(upd.pass.as_deref(), Some("p"));
    }

    #[test]
    fn path_mode_and_keys_are_case_insensitive() {
        let raw = b"GET /Admin.CGI?PASS=p&MODE=UpdInfo&SONG=Loud HTTP/1.0\r\n\r\n";
        let upd = parse_metadata_update(raw).unwrap();
        assert_eq!(upd.song, "Loud");
        assert_eq!(upd.pass.as_deref(), Some("p"));
    }

    #[test]
    fn empty_song_clears_title() {
        let raw = b"GET /admin/metadata?pass=p&mode=updinfo&mount=/m&song= HTTP/1.0\r\n\r\n";
        let upd = parse_metadata_update(raw).unwrap();
        assert_eq!(upd.song, "");
    }

    #[test]
    fn missing_song_errors() {
        let raw = b"GET /admin/metadata?pass=p&mode=updinfo&mount=/m HTTP/1.0\r\n\r\n";
        assert_eq!(
            parse_metadata_update(raw).unwrap_err(),
            IcyError::MissingParameter("song".to_string())
        );
    }

    #[test]
    fn missing_or_wrong_mode_errors() {
        let raw = b"GET /admin/metadata?pass=p&song=S HTTP/1.0\r\n\r\n";
        assert_eq!(
            parse_metadata_update(raw).unwrap_err(),
            IcyError::UnsupportedMode(String::new())
        );
        let raw = b"GET /admin/metadata?pass=p&mode=viewlog&song=S HTTP/1.0\r\n\r\n";
        assert_eq!(
            parse_metadata_update(raw).unwrap_err(),
            IcyError::UnsupportedMode("viewlog".to_string())
        );
    }

    #[test]
    fn wrong_path_errors() {
        let raw = b"GET /live?mode=updinfo&song=S HTTP/1.0\r\n\r\n";
        assert_eq!(
            parse_metadata_update(raw).unwrap_err(),
            IcyError::UnsupportedTarget("/live".to_string())
        );
    }

    #[test]
    fn non_get_errors() {
        let raw = b"POST /admin/metadata?mode=updinfo&song=S HTTP/1.0\r\n\r\n";
        assert!(matches!(
            parse_metadata_update(raw).unwrap_err(),
            IcyError::UnsupportedMethod(_)
        ));
    }

    #[test]
    fn empty_request_errors() {
        assert_eq!(
            parse_metadata_update(b"").unwrap_err(),
            IcyError::EmptyRequest
        );
    }

    #[test]
    fn repeated_parameters_take_the_last_value() {
        let raw = b"GET /admin.cgi?mode=updinfo&song=First&song=Second&pass=p HTTP/1.0\r\n\r\n";
        let upd = parse_metadata_update(raw).unwrap();
        assert_eq!(upd.song, "Second");
    }

    #[test]
    fn percent_encoded_keys_and_empty_url_and_mount() {
        // `so%6Eg` decodes to `song`; empty url/mount normalise to None.
        let raw = b"GET /admin/metadata?mode=updinfo&so%6Eg=Hi&url=&mount= HTTP/1.0\r\n\r\n";
        let upd = parse_metadata_update(raw).unwrap();
        assert_eq!(upd.song, "Hi");
        assert_eq!(upd.url, None);
        assert_eq!(upd.mount, None);
    }

    #[test]
    fn percent_decode_vectors() {
        for (input, want) in [
            ("", ""),
            ("plain", "plain"),
            ("a+b", "a b"),
            ("%20", " "),
            ("%2F%2f", "//"),
            ("Daft+Punk+-+Da+Funk", "Daft Punk - Da Funk"),
            ("100%25", "100%"),
            ("%E3%81%82", "あ"),          // multi-byte UTF-8
            ("%", "%"),                   // bare percent at end
            ("%2", "%2"),                 // truncated escape
            ("%zz", "%zz"),               // non-hex escape passes through
            ("%1G", "%1G"),               // half-hex escape passes through
            ("a%2Bb", "a+b"),             // encoded plus stays a plus
            ("tag%00end", "tag\u{0}end"), // NUL byte decodes
        ] {
            assert_eq!(percent_decode(input), want, "input={input:?}");
        }
    }

    #[test]
    fn percent_decode_invalid_utf8_is_lossy_not_panicky() {
        assert_eq!(percent_decode("%FF%FE"), "\u{FFFD}\u{FFFD}");
    }

    #[test]
    fn ok_response_shape() {
        let resp = metadata_update_ok();
        assert!(resp.starts_with("HTTP/1.0 200 OK\r\n"));
        assert!(resp.contains("Content-Type: text/xml\r\n"));
        assert!(resp.contains("<return>1</return>"));
        assert!(resp.contains("Metadata update successful"));
        // Head and body are separated by exactly one blank line.
        assert_eq!(resp.matches("\r\n\r\n").count(), 1);
        // Content-Length matches the body.
        let (head, body) = resp.split_once("\r\n\r\n").unwrap();
        assert!(head.contains(&format!("Content-Length: {}", body.len())));
    }

    #[test]
    fn failed_response_escapes_message() {
        let resp = metadata_update_failed("no <mount> & 'no' \"stream\"");
        assert!(resp.starts_with("HTTP/1.0 200 OK\r\n"));
        assert!(resp.contains("<return>0</return>"));
        assert!(resp.contains("no &lt;mount&gt; &amp; &apos;no&apos; &quot;stream&quot;"));
        assert!(!resp.contains("<mount>"));
    }

    #[test]
    fn unauthorized_response_challenges_basic() {
        let resp = metadata_update_unauthorized();
        assert!(resp.starts_with("HTTP/1.0 401 Unauthorized\r\n"));
        assert!(resp.contains("WWW-Authenticate: Basic realm=\"RabbitHole Radio\"\r\n"));
        assert!(resp.contains("<return>0</return>"));
    }
}
