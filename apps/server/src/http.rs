//! Embedded HTTP server (Wave 8): the web SPA shell + the `/files/...`
//! download handoff that telnet's `get` command mints links for.
//!
//! Opt-in via config (`http_enabled`, default **off**) on `http_addr`
//! (default 0.0.0.0:8080). Two surfaces share the one listener:
//!
//! - **`/files/<area>/<percent-encoded-path>`** — the out-of-band transfer
//!   target for [`crate::telnet`]'s `get` command (whose link format this
//!   route must match; `files_http_base` only *mints* the links, this module
//!   serves them). Requests are anonymous, so authorization is the federation
//!   catalog's public-listability discipline (see [`crate::fed_catalog`]):
//!   the bare-guest [`public_subject`] must hold `SEE | FILE_LIST` on both
//!   the area and the file's `files/<area>/<path>` resource. Drop-box
//!   contents, quarantined blobs ([`ModerationService::file_quarantined`])
//!   and deny-listed hashes ([`ModerationService::is_denied`]) are refused.
//!   **Everything non-public is a plain 404** — the response never
//!   distinguishes "hidden" from "missing" from "moderated", so existence
//!   can't be probed. A successful `GET` counts the download
//!   ([`FileService::record_download`]); the telnet slice documented that
//!   counting happens here, at the byte-serving hop, not at link minting.
//!
//! - **the SPA shell** — when `http_web_root` is set, files under it are
//!   served at `/` (with `index.html` answering `/` itself), plus a
//!   generated `/manifest.webmanifest` (name from the server config,
//!   standalone display) when the web root doesn't provide one. This module
//!   never builds the wasm bundle: serving whatever is in the directory is
//!   the contract — point `http_web_root` at a `trunk build` output dir.
//!   With `http_web_root` unset only the `/files/...` route answers.
//!
//! # The HTTP/1.1 server
//!
//! Deliberately minimal and hand-rolled over a tokio `TcpStream`, in the
//! same spirit as [`crate::syndication`]'s hand-rolled *client* (no new
//! dependencies): `GET` and `HEAD` only (405 otherwise), one request per
//! connection (`Connection: close` framing — no keep-alive state machine),
//! a hard [`MAX_HEAD_BYTES`] cap on the request head, `Content-Length` on
//! every response, and a `Content-Type` chosen by a small extension map.
//! Connections pass the `conn` rate class at accept and a per-IP request
//! budget (the `legacy` class) per request, like the other legacy surfaces.
//!
//! # Security notes
//!
//! - **Strict path sanitization** ([`sanitize_path`]): percent-decoding is
//!   applied per segment *after* splitting on `/`, so an encoded slash can't
//!   mint new segments; `..` (plain or encoded as `%2e%2e`), `.`,
//!   backslashes, NUL bytes and malformed escapes are all rejected with 400.
//! - **No directory listings**: a path that resolves to a directory is 404.
//! - **Symlink containment**: static paths are canonicalized and must stay
//!   under the canonicalized web root; a symlink escaping the root is 404.
//! - **`HEAD` mirrors `GET`** — identical status and headers (including
//!   `Content-Length`), no body. `HEAD` does not bump download counters.
//! - **Close discipline**: responses end with an explicit FIN + bounded
//!   drain to the peer's FIN (the [`crate::hotline`] `serve_htxf`
//!   discipline), so buffered bytes are delivered rather than discarded by
//!   an RST from a bare socket drop.

use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use rabbithole_blobs::BlobId;
use rabbithole_server_core::files::{FileError, KIND_FILE};
use rabbithole_server_core::ratelimit::{class as rl, Scope};
use rabbithole_server_core::Caps;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;

use crate::fed_catalog::public_subject;
use crate::Shared;

/// Request head cap (request line + headers). Anything larger is a 400.
pub const MAX_HEAD_BYTES: usize = 8 * 1024;

/// Whole-request deadline: read the head, do the work, write the response.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

/// Post-response drain deadline (see the module close-discipline note).
const DRAIN_TIMEOUT: Duration = Duration::from_secs(30);

/// Bind + serve the embedded HTTP surface. Returns the bound address (useful
/// when the config asked for port 0) and the accept-loop task handle.
/// `web_root` is the already-resolved static asset directory (`None` = no
/// static serving).
pub async fn spawn_http(
    shared: Arc<Shared>,
    addr: SocketAddr,
    web_root: Option<PathBuf>,
) -> Result<(SocketAddr, JoinHandle<()>)> {
    let listener = TcpListener::bind(addr).await?;
    let local = listener.local_addr()?;
    let web_root = Arc::new(web_root);
    let handle = tokio::spawn(async move {
        loop {
            let Ok((sock, peer)) = listener.accept().await else {
                break;
            };
            // Over the per-IP connection budget: drop it on the floor.
            if !shared.rate_allow(Scope::Ip(peer.ip()), rl::CONN) {
                continue;
            }
            let shared = shared.clone();
            let web_root = web_root.clone();
            tokio::spawn(async move {
                let served = tokio::time::timeout(
                    REQUEST_TIMEOUT,
                    serve_conn(sock, &shared, web_root.as_deref(), peer.ip()),
                )
                .await;
                if let Ok(Err(e)) = served {
                    tracing::debug!(%peer, "http connection error: {e}");
                }
            });
        }
    });
    Ok((local, handle))
}

/// One connection = one request. Reads a capped head, routes it, writes the
/// response, then closes with the FIN + drain discipline.
async fn serve_conn(
    mut sock: TcpStream,
    shared: &Arc<Shared>,
    web_root: Option<&Path>,
    peer_ip: IpAddr,
) -> Result<()> {
    sock.set_nodelay(true).ok();
    let response = match read_head(&mut sock).await? {
        Some(head) => respond(&head, shared, web_root, peer_ip).await,
        None => Response::text(400, "Bad Request", "request head too large or malformed\n"),
    };
    sock.write_all(&response.to_bytes()).await?;
    // FIN, then drain to the peer's FIN so buffered bytes are delivered
    // rather than discarded by an RST from a bare drop (serve_htxf rule).
    let _ = sock.shutdown().await;
    let mut sink = [0u8; 1024];
    let drain = async {
        while let Ok(n) = sock.read(&mut sink).await {
            if n == 0 {
                break;
            }
        }
    };
    let _ = tokio::time::timeout(DRAIN_TIMEOUT, drain).await;
    Ok(())
}

/// Read the request head (through the blank line), capped at
/// [`MAX_HEAD_BYTES`]. `Ok(None)` = over the cap or EOF before the blank
/// line — answer 400. Any body after the head is ignored (GET/HEAD have
/// none, and everything else is refused with 405 anyway).
async fn read_head(sock: &mut TcpStream) -> Result<Option<Vec<u8>>> {
    let mut head = Vec::with_capacity(1024);
    let mut buf = [0u8; 1024];
    loop {
        if let Some(end) = find_subslice(&head, b"\r\n\r\n") {
            head.truncate(end);
            return Ok(Some(head));
        }
        if head.len() >= MAX_HEAD_BYTES {
            return Ok(None);
        }
        let n = sock.read(&mut buf).await?;
        if n == 0 {
            return Ok(None);
        }
        head.extend_from_slice(&buf[..n]);
    }
}

/// First index of `needle` in `haystack`.
fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

// ---------------------------------------------------------------------------
// Request parsing + routing
// ---------------------------------------------------------------------------

/// A parsed request line: method + raw path (query already stripped).
struct Request {
    method: Method,
    /// Decoded, sanitized path segments (`/` = empty vec).
    segments: Vec<String>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Method {
    Get,
    Head,
}

/// Route the request. Every outcome — including malformed requests — is a
/// complete [`Response`]; `HEAD` gets the same status/headers as `GET` with
/// the body dropped at serialization time.
async fn respond(
    head: &[u8],
    shared: &Arc<Shared>,
    web_root: Option<&Path>,
    peer_ip: IpAddr,
) -> Response {
    // Per-IP request budget: the same coarse `legacy` class as the other
    // legacy surfaces (telnet, Hotline, NNTP).
    if !shared.rate_allow(Scope::Ip(peer_ip), rl::LEGACY) {
        return Response::text(429, "Too Many Requests", "rate limited; slow down\n");
    }
    let text = String::from_utf8_lossy(head);
    let request_line = text.split("\r\n").next().unwrap_or_default();
    let mut parts = request_line.split_whitespace();
    let (Some(method), Some(target), Some(version)) = (parts.next(), parts.next(), parts.next())
    else {
        return Response::text(400, "Bad Request", "malformed request line\n");
    };
    if !version.starts_with("HTTP/1.") {
        return Response::text(400, "Bad Request", "HTTP/1.x only\n");
    }
    let method = match method {
        "GET" => Method::Get,
        "HEAD" => Method::Head,
        _ => {
            let mut r = Response::text(405, "Method Not Allowed", "GET and HEAD only\n");
            r.headers.push(("Allow".into(), "GET, HEAD".into()));
            return r;
        }
    };
    // Strip the query string; sanitize + decode the path.
    let raw_path = target.split('?').next().unwrap_or_default();
    let Some(segments) = sanitize_path(raw_path) else {
        return Response::text(400, "Bad Request", "bad path\n");
    };
    let req = Request { method, segments };

    let mut resp = if req.segments.first().map(String::as_str) == Some("files") {
        serve_file_download(&req, shared).await
    } else {
        serve_static(&req, shared, web_root).await
    };
    if req.method == Method::Head {
        resp.head_only = true;
    }
    resp
}

// ---------------------------------------------------------------------------
// Path sanitization
// ---------------------------------------------------------------------------

/// Decode + sanitize a request path into its segments. `None` = refuse
/// (answer 400). Rules:
///
/// - must be absolute (start with `/`);
/// - split on `/` **before** percent-decoding, so an encoded `/` (`%2F`)
///   stays inside its segment and is then rejected below;
/// - each segment percent-decodes strictly (malformed `%` escapes refuse)
///   and must be valid UTF-8;
/// - decoded segments may not be `.` / `..`, may not contain `/`, `\` or
///   NUL — this rejects plain and encoded (`%2e%2e`, `%5C`, `%00`)
///   traversal alike;
/// - empty segments (from `//` or a trailing `/`) collapse away, so a
///   trailing slash can't alias a second resource name.
pub fn sanitize_path(raw: &str) -> Option<Vec<String>> {
    let rest = raw.strip_prefix('/')?;
    let mut segments = Vec::new();
    for part in rest.split('/') {
        if part.is_empty() {
            continue; // collapse `//` and trailing `/`
        }
        let decoded = percent_decode(part)?;
        let decoded = String::from_utf8(decoded).ok()?;
        if decoded == "." || decoded == ".." {
            return None;
        }
        if decoded.contains(['/', '\\', '\0']) {
            return None;
        }
        segments.push(decoded);
    }
    Some(segments)
}

/// Strict percent-decoding: `%XX` with two hex digits, everything else
/// verbatim (`+` is *not* a space in paths). `None` on a malformed escape.
pub fn percent_decode(s: &str) -> Option<Vec<u8>> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hi = char::from(*bytes.get(i + 1)?).to_digit(16)?;
            let lo = char::from(*bytes.get(i + 2)?).to_digit(16)?;
            out.push((hi * 16 + lo) as u8);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    Some(out)
}

// ---------------------------------------------------------------------------
// /files/<area>/<path...>: the anonymous download handoff
// ---------------------------------------------------------------------------

/// Serve one publicly-listable library file. Anything not public — missing,
/// hidden, drop-boxed, quarantined, denied, folder, blob-less — is the same
/// plain 404: no existence distinctions leak past this function.
async fn serve_file_download(req: &Request, shared: &Arc<Shared>) -> Response {
    let not_found = || Response::text(404, "Not Found", "no such file\n");
    // segments = ["files", area, path components...]
    let Some(area) = req.segments.get(1) else {
        return not_found();
    };
    let path_parts = req.segments.get(2..).unwrap_or_default();
    if path_parts.is_empty() {
        return not_found(); // no folder indexes, no area listings
    }
    let path = path_parts.join("/");
    let public = public_subject();
    let need = Caps::SEE | Caps::FILE_LIST;
    // The whole area must be publicly listable (fed_catalog discipline).
    if !shared.perms.allows(&public, &format!("files/{area}"), need) {
        return not_found();
    }
    let node = match shared.files.node_by_path(area, &path).await {
        Ok(Some(n)) => n,
        _ => return not_found(),
    };
    if !shared
        .perms
        .allows(&public, &format!("files/{}/{}", area, node.path), need)
    {
        return not_found();
    }
    // Follow one alias hop to the real file; the target must be public too.
    let target = match shared.files.resolve(node.id).await {
        Ok(t) => t,
        Err(_) => return not_found(),
    };
    if target.kind != KIND_FILE {
        return not_found();
    }
    if target.id != node.id
        && !shared.perms.allows(
            &public,
            &format!("files/{}/{}", target.area, target.path),
            need,
        )
    {
        return not_found();
    }
    // Drop-box contents are never served anonymously.
    if shared.files.in_dropbox(&target).await.unwrap_or(true) {
        return not_found();
    }
    let Some(blob_id) = target.blob_id else {
        return not_found();
    };
    // Moderation: quarantined-for-review and deny-listed content both
    // refuse. Blob ids are the blake3 of the content, so one hash serves
    // both checks.
    if shared.moderation.file_quarantined(Some(&blob_id)) || shared.moderation.is_denied(&blob_id) {
        return not_found();
    }
    // Count the download (this is the byte-serving hop the telnet slice
    // deferred counting to) — but only for GET: HEAD serves no bytes.
    let served = if req.method == Method::Get {
        match shared.files.record_download(node.id).await {
            Ok(s) => s,
            Err(FileError::NoSuchNode) => return not_found(),
            Err(e) => {
                tracing::warn!("http download counter failed: {e}");
                return Response::text(500, "Internal Server Error", "try again later\n");
            }
        }
    } else {
        target.clone()
    };
    let blobs = shared.blobs.clone();
    let bytes = match tokio::task::spawn_blocking(move || blobs.get(&BlobId(blob_id))).await {
        Ok(Ok(b)) => b,
        _ => return not_found(),
    };
    let mime = if served.mime.trim().is_empty() {
        content_type_for(&served.name).to_string()
    } else {
        served.mime.clone()
    };
    let mut resp = Response::new(200, "OK", mime, bytes);
    resp.headers.push((
        "Content-Disposition".into(),
        format!(
            "attachment; filename=\"{}\"",
            disposition_name(&served.name)
        ),
    ));
    resp
}

/// A `Content-Disposition` filename token: quotes, backslashes and control
/// bytes are replaced so the quoted-string can't be broken out of.
fn disposition_name(name: &str) -> String {
    name.chars()
        .map(|c| match c {
            '"' | '\\' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Static SPA shell + generated manifest
// ---------------------------------------------------------------------------

/// Serve the SPA shell out of the web root: `/` answers `index.html`,
/// `/manifest.webmanifest` falls back to a generated one, everything else is
/// a plain file lookup. No web root configured = 404 for all of it.
async fn serve_static(req: &Request, shared: &Arc<Shared>, web_root: Option<&Path>) -> Response {
    let not_found = || Response::text(404, "Not Found", "not found\n");
    let Some(root) = web_root else {
        return not_found();
    };
    let rel: Vec<&str> = if req.segments.is_empty() {
        vec!["index.html"] // index fallback for `/`
    } else {
        req.segments.iter().map(String::as_str).collect()
    };
    match read_under_root(root, &rel).await {
        Some(bytes) => {
            let name = rel.last().unwrap_or(&"");
            Response::new(200, "OK", content_type_for(name).to_string(), bytes)
        }
        // The PWA manifest is generated when the web root doesn't ship one.
        None if rel == ["manifest.webmanifest"] => {
            let cfg = shared.config.read();
            Response::new(
                200,
                "OK",
                content_type_for("manifest.webmanifest").to_string(),
                web_manifest(&cfg.name, &cfg.theme_accent).into_bytes(),
            )
        }
        None => not_found(),
    }
}

/// Read `rel` under `root`, refusing anything that escapes it: the joined
/// path is canonicalized (resolving symlinks) and must still start with the
/// canonicalized root, and must be a regular file — directories are 404
/// (no listings). `None` = not served.
async fn read_under_root(root: &Path, rel: &[&str]) -> Option<Vec<u8>> {
    let mut path = root.to_path_buf();
    for part in rel {
        path.push(part); // parts are sanitized: no `..`, `/`, `\`, NUL
    }
    let root = root.to_path_buf();
    // Filesystem work off the async runtime, in one blocking hop.
    tokio::task::spawn_blocking(move || {
        let canon_root = std::fs::canonicalize(&root).ok()?;
        let canon = std::fs::canonicalize(&path).ok()?;
        if !canon.starts_with(&canon_root) {
            return None; // symlink escaped the root
        }
        if !std::fs::metadata(&canon).ok()?.is_file() {
            return None; // no directory listings
        }
        std::fs::read(&canon).ok()
    })
    .await
    .ok()
    .flatten()
}

/// The generated PWA manifest: server name, standalone display, theme colors
/// from the configured accent (falling back to a neutral dark).
pub fn web_manifest(server_name: &str, theme_accent: &str) -> String {
    let accent = theme_accent.trim().trim_start_matches('#');
    let color = if accent.len() == 6 && accent.bytes().all(|b| b.is_ascii_hexdigit()) {
        format!("#{}", accent.to_ascii_lowercase())
    } else {
        "#1d1d28".to_string()
    };
    serde_json::json!({
        "name": server_name,
        "short_name": server_name,
        "start_url": "/",
        "display": "standalone",
        "background_color": "#1d1d28",
        "theme_color": color,
    })
    .to_string()
}

/// `Content-Type` by file extension (lowercased); unknown = octet-stream.
pub fn content_type_for(name: &str) -> &'static str {
    let ext = name.rsplit_once('.').map(|(_, e)| e).unwrap_or("");
    match ext.to_ascii_lowercase().as_str() {
        "html" | "htm" => "text/html; charset=utf-8",
        "css" => "text/css",
        "js" => "text/javascript",
        "wasm" => "application/wasm",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "svg" => "image/svg+xml",
        "ico" => "image/x-icon",
        "json" => "application/json",
        "webmanifest" => "application/manifest+json",
        "txt" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

// ---------------------------------------------------------------------------
// Response serialization
// ---------------------------------------------------------------------------

/// One complete response. `Content-Length` reflects the body even for
/// `HEAD` (`head_only` drops the bytes at serialization, per RFC 9110).
struct Response {
    status: u16,
    reason: &'static str,
    content_type: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
    head_only: bool,
}

impl Response {
    fn new(status: u16, reason: &'static str, content_type: String, body: Vec<u8>) -> Response {
        Response {
            status,
            reason,
            content_type,
            headers: Vec::new(),
            body,
            head_only: false,
        }
    }

    /// A plain-text response (errors mostly).
    fn text(status: u16, reason: &'static str, body: &str) -> Response {
        Response::new(
            status,
            reason,
            "text/plain; charset=utf-8".into(),
            body.as_bytes().to_vec(),
        )
    }

    /// Serialize head + (unless `head_only`) body. `Connection: close`
    /// always — one request per connection keeps the framing trivial.
    fn to_bytes(&self) -> Vec<u8> {
        let mut head = format!(
            "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n",
            self.status,
            self.reason,
            self.content_type,
            self.body.len(),
        );
        for (name, value) in &self.headers {
            head.push_str(name);
            head.push_str(": ");
            head.push_str(value);
            head.push_str("\r\n");
        }
        head.push_str("\r\n");
        let mut out = head.into_bytes();
        if !self.head_only {
            out.extend_from_slice(&self.body);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_accepts_normal_paths() {
        assert_eq!(sanitize_path("/"), Some(vec![]));
        assert_eq!(
            sanitize_path("/files/warez/cool.zip"),
            Some(vec!["files".into(), "warez".into(), "cool.zip".into()])
        );
        // Percent-decoding applies per segment; `+` stays literal.
        assert_eq!(
            sanitize_path("/files/a/b%20c+d.txt"),
            Some(vec!["files".into(), "a".into(), "b c+d.txt".into()])
        );
        // Doubled and trailing slashes collapse instead of aliasing.
        assert_eq!(
            sanitize_path("//app//main.js/"),
            Some(vec!["app".into(), "main.js".into()])
        );
        // UTF-8 percent escapes decode.
        assert_eq!(sanitize_path("/caf%C3%A9"), Some(vec!["café".into()]));
    }

    #[test]
    fn sanitize_rejects_traversal_and_junk() {
        // Plain traversal, current-dir, and *encoded* traversal.
        assert_eq!(sanitize_path("/../etc/passwd"), None);
        assert_eq!(sanitize_path("/files/a/.."), None);
        assert_eq!(sanitize_path("/files/a/./b"), None);
        assert_eq!(sanitize_path("/files/%2e%2e/secret"), None);
        assert_eq!(sanitize_path("/files/%2E%2E/secret"), None);
        assert_eq!(sanitize_path("/%2e%2e%2fetc/passwd"), None, "encoded ../");
        // Encoded slash, backslash (plain + encoded), NUL.
        assert_eq!(sanitize_path("/a%2Fb"), None);
        assert_eq!(sanitize_path("/a\\b"), None);
        assert_eq!(sanitize_path("/a%5Cb"), None);
        assert_eq!(sanitize_path("/a%00b"), None);
        // Malformed escapes and relative (non-absolute) targets.
        assert_eq!(sanitize_path("/a%2"), None);
        assert_eq!(sanitize_path("/a%zz"), None);
        assert_eq!(sanitize_path("relative/path"), None);
        assert_eq!(sanitize_path(""), None);
        // Invalid UTF-8 after decoding.
        assert_eq!(sanitize_path("/%ff%fe"), None);
    }

    #[test]
    fn percent_decode_strictness() {
        assert_eq!(percent_decode("plain"), Some(b"plain".to_vec()));
        assert_eq!(percent_decode("a%20b"), Some(b"a b".to_vec()));
        assert_eq!(percent_decode("%41%6a"), Some(b"Aj".to_vec()));
        assert_eq!(percent_decode("%"), None);
        assert_eq!(percent_decode("%4"), None);
        assert_eq!(percent_decode("%G0"), None);
    }

    #[test]
    fn content_type_map() {
        assert_eq!(content_type_for("index.html"), "text/html; charset=utf-8");
        assert_eq!(content_type_for("app.CSS"), "text/css");
        assert_eq!(content_type_for("main.js"), "text/javascript");
        assert_eq!(content_type_for("app_bg.wasm"), "application/wasm");
        assert_eq!(content_type_for("logo.png"), "image/png");
        assert_eq!(content_type_for("photo.JPG"), "image/jpeg");
        assert_eq!(content_type_for("photo.jpeg"), "image/jpeg");
        assert_eq!(content_type_for("icon.svg"), "image/svg+xml");
        assert_eq!(content_type_for("favicon.ico"), "image/x-icon");
        assert_eq!(content_type_for("data.json"), "application/json");
        assert_eq!(
            content_type_for("manifest.webmanifest"),
            "application/manifest+json"
        );
        assert_eq!(content_type_for("readme.txt"), "text/plain; charset=utf-8");
        assert_eq!(content_type_for("blob.bin"), "application/octet-stream");
        assert_eq!(content_type_for("no-extension"), "application/octet-stream");
    }

    #[test]
    fn manifest_shape() {
        let m: serde_json::Value =
            serde_json::from_str(&web_manifest("The Warren", "A1B2C3")).unwrap();
        assert_eq!(m["name"], "The Warren");
        assert_eq!(m["short_name"], "The Warren");
        assert_eq!(m["start_url"], "/");
        assert_eq!(m["display"], "standalone");
        assert_eq!(m["theme_color"], "#a1b2c3");
        // No accent configured: the neutral default, never invalid JSON.
        let m: serde_json::Value = serde_json::from_str(&web_manifest("X", "")).unwrap();
        assert_eq!(m["theme_color"], "#1d1d28");
        let m: serde_json::Value = serde_json::from_str(&web_manifest("X", "nope")).unwrap();
        assert_eq!(m["theme_color"], "#1d1d28");
    }

    #[test]
    fn disposition_names_cannot_break_the_quoted_string() {
        assert_eq!(disposition_name("plain.zip"), "plain.zip");
        assert_eq!(disposition_name("we\"ird\\name\n.txt"), "we_ird_name_.txt");
    }

    #[test]
    fn responses_carry_length_and_head_drops_the_body() {
        let mut r = Response::text(200, "OK", "hello");
        let full = r.to_bytes();
        let text = String::from_utf8(full.clone()).unwrap();
        assert!(text.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(text.contains("Content-Length: 5\r\n"));
        assert!(text.contains("Connection: close\r\n"));
        assert!(text.ends_with("\r\n\r\nhello"));
        r.head_only = true;
        let head = r.to_bytes();
        assert!(String::from_utf8(head.clone())
            .unwrap()
            .ends_with("\r\n\r\n"));
        assert_eq!(&full[..full.len() - 5], &head[..], "same head, no body");
    }
}
