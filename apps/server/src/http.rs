//! Embedded HTTP server (Wave 8): the web SPA shell + the `/files/...`
//! download handoff that telnet's `get` command mints links for.
//!
//! Opt-in via config (`http_enabled`, default **off**) on `http_addr`
//! (default 0.0.0.0:8080). Three surfaces share the one listener:
//!
//! - **`/.well-known/rabbithole/server`** — the signed, self-certifying
//!   discovery descriptor ([`crate::well_known`]): a public JSON statement of
//!   the server's identity key, addresses, and features that any peer,
//!   tracker, or client can fetch and verify without a round trip.
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
//!   standalone display) when the web root doesn't provide one. Supported
//!   client routes fall back to `index.html` for direct visits and reloads;
//!   missing assets and unknown routes stay 404. This module
//!   never builds the wasm bundle: serving whatever is in the directory is
//!   the contract — point `http_web_root` at a `trunk build` output dir.
//!   With `http_web_root` unset the shell is unavailable; downloads and
//!   discovery still answer.
//!
//! # The HTTP/1.1 server
//!
//! Deliberately minimal and hand-rolled over a tokio `TcpStream`, in the
//! same spirit as [`crate::syndication`]'s hand-rolled *client* (no new
//! dependencies): `GET` and `HEAD` only (405 otherwise), with bounded,
//! sequential HTTP/1.1 connection reuse. Pipelined requests retain their
//! order. HTTP/1.0 and `Connection: close` requests close after one response.
//! Request bodies and ambiguous framing are refused, closing the connection
//! without processing trailing bytes. Each connection has an idle/head-read
//! deadline, byte budget and request-count limit. Every response carries
//! `Content-Length` and a `Content-Type` chosen by a small extension map.
//! Connections pass the `conn` rate class at accept and a per-IP request
//! budget (the `legacy` class) per request, like the other legacy surfaces.
//!
//! File downloads and actual static assets support one byte range on GET.
//! Closed, open-ended and suffix ranges return 206; unsatisfiable ranges
//! return 416. Malformed, oversized, overflowing, repeated or multipart byte
//! ranges return 400, only after the ordinary path/access checks succeed.
//! Per RFC 9110 section 14.2, HEAD, unknown range units and empty content
//! ignore Range and return the normal full response. With no validators
//! advertised, If-Range conservatively selects the full response (section
//! 13.1.5). Generated documents and SPA shell navigation ignore Range too.
//!
//! # Security notes
//!
//! - **The burrow's own folders are never served** ([`private_dirs`]): the
//!   data directory and the snapshot folder are refused whatever the web
//!   root is set to, checked on the canonicalized path so a symlink into
//!   them is refused too. `http_web_root` is an ordinary config key that
//!   `CONFIG_ADMIN` sets live; pointed at the data directory, this route
//!   would otherwise have answered `GET /identity/server_ed25519.seed` for
//!   anybody at all, with no session and nothing in the audit log. The key
//!   is also validated when it is set, and `data_dir` cannot be set from a
//!   console at all — but this check is the one that holds when neither ran.
//! - **Strict path sanitization** ([`sanitize_path`]): percent-decoding is
//!   applied per segment *after* splitting on `/`, so an encoded slash can't
//!   mint new segments; `..` (plain or encoded as `%2e%2e`), `.`,
//!   backslashes, NUL bytes and malformed escapes are all rejected with 400.
//! - **No directory listings**: directories are never listed. A recognized
//!   client route may instead serve the separately checked SPA index.
//! - **Symlink containment**: every served static file is canonicalized and
//!   must stay under the canonicalized web root and outside private folders,
//!   including an index served as a client-route fallback.
//! - **`HEAD` mirrors `GET`** — identical status and headers (including
//!   `Content-Length`), no body. `HEAD` does not bump download counters.
//! - **Close discipline**: completed connections end with an explicit FIN + bounded
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
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;

use crate::fed_catalog::public_subject;
use crate::Shared;

/// Request head cap, including its terminating blank line. Larger = 400 + close.
pub const MAX_HEAD_BYTES: usize = 8 * 1024;

/// Maximum completed requests on one connection, including errors.
pub const MAX_CONNECTION_REQUESTS: usize = 100;

/// Total incoming request-head bytes on one connection. Bodies are unsupported.
pub const MAX_CONNECTION_BYTES: usize = 64 * 1024;

/// Maximum idle time / time to finish a whole head, not reset by short reads.
pub const KEEP_ALIVE_IDLE_TIMEOUT: Duration = Duration::from_secs(15);

/// Per-response deadline: route the request and write the complete response.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

/// Post-response drain deadline (see the module close-discipline note).
const DRAIN_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_DRAIN_BYTES: u64 = 64 * 1024;

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
                if let Err(e) = serve_conn(sock, &shared, web_root.as_deref(), peer.ip()).await {
                    tracing::debug!(%peer, "http connection error: {e}");
                }
            });
        }
    });
    Ok((local, handle))
}

/// Serve requests in order, preserving bytes read ahead for the next head.
/// Any framing refusal closes rather than trying to find another request in
/// potentially body-bearing bytes (RFC 9112 section 9.3).
async fn serve_conn(
    mut sock: TcpStream,
    shared: &Arc<Shared>,
    web_root: Option<&Path>,
    peer_ip: IpAddr,
) -> Result<()> {
    sock.set_nodelay(true).ok();
    let mut pending = Vec::with_capacity(MAX_HEAD_BYTES);
    let mut consumed = 0;
    let result: Result<()> = async {
        for count in 1..=MAX_CONNECTION_REQUESTS {
            let read = read_head(
                &mut sock,
                &mut pending,
                MAX_CONNECTION_BYTES - consumed,
                KEEP_ALIVE_IDLE_TIMEOUT,
            )
            .await;
            let (head, refusal) = match read {
                Ok(HeadRead::Complete(head)) => {
                    consumed += head.len() + 4;
                    (head, None)
                }
                Ok(HeadRead::Closed) => break,
                Ok(HeadRead::Rejected) => (
                    std::mem::take(&mut pending),
                    Some(Response::text(
                        400,
                        "Bad Request",
                        "request head too large or incomplete\n",
                    )),
                ),
                Err(e) => return Err(e.into()),
                Ok(HeadRead::TimedOut) if pending.is_empty() => break, // quiet idle close
                Ok(HeadRead::TimedOut) => (
                    std::mem::take(&mut pending),
                    Some(Response::text(
                        408,
                        "Request Timeout",
                        "request head timed out\n",
                    )),
                ),
            };
            // Even a refused/rate-limited HEAD has no response body. Do this
            // before parsing so malformed headers cannot break HEAD framing.
            let head_only = head.split(|b| b.is_ascii_whitespace()).next() == Some(b"HEAD");
            let reuse = tokio::time::timeout(REQUEST_TIMEOUT, async {
                let mut keep_alive = false;
                // Connection reuse must not bypass the per-IP request budget.
                let mut response = if !shared.rate_allow(Scope::Ip(peer_ip), rl::LEGACY) {
                    Response::text(429, "Too Many Requests", "rate limited; slow down\n")
                } else if let Some(refusal) = refusal {
                    refusal
                } else {
                    match parse_request(&head) {
                        Ok(req) => {
                            keep_alive = req.keep_alive
                                && count < MAX_CONNECTION_REQUESTS
                                && consumed < MAX_CONNECTION_BYTES;
                            respond(&req, shared, web_root).await
                        }
                        Err(refusal) => refusal,
                    }
                };
                response.head_only = head_only;
                response.close = !keep_alive;
                sock.write_all(&response.to_bytes()).await?;
                Ok::<_, std::io::Error>(keep_alive)
            })
            .await??;
            if !reuse {
                break;
            }
        }
        Ok(())
    }
    .await;
    // FIN, then drain to the peer's FIN so buffered bytes are delivered
    // rather than discarded by an RST. Both time and discarded bytes are
    // bounded; drained bytes are never parsed or routed.
    let _ = sock.shutdown().await;
    let _ = tokio::time::timeout(
        DRAIN_TIMEOUT,
        tokio::io::copy(&mut sock.take(MAX_DRAIN_BYTES), &mut tokio::io::sink()),
    )
    .await;
    result
}

enum HeadRead {
    Complete(Vec<u8>),
    Closed,
    Rejected,
    TimedOut,
}

/// Retain pipelined bytes after one head. Never read beyond the per-head
/// buffer cap or the connection's remaining incoming byte budget. A single
/// deadline covers the whole head, so trickled bytes cannot keep it alive.
async fn read_head(
    sock: &mut (impl AsyncRead + Unpin),
    pending: &mut Vec<u8>,
    remaining: usize,
    deadline: Duration,
) -> std::io::Result<HeadRead> {
    match tokio::time::timeout(deadline, read_head_inner(sock, pending, remaining)).await {
        Ok(result) => result,
        Err(_) => Ok(HeadRead::TimedOut),
    }
}

async fn read_head_inner(
    sock: &mut (impl AsyncRead + Unpin),
    pending: &mut Vec<u8>,
    remaining: usize,
) -> std::io::Result<HeadRead> {
    let limit = MAX_HEAD_BYTES.min(remaining);
    let mut buf = [0u8; 1024];
    loop {
        if let Some(end) = find_subslice(pending, b"\r\n\r\n") {
            let size = end + 4;
            if size > limit {
                return Ok(HeadRead::Rejected);
            }
            let head = pending[..end].to_vec();
            pending.drain(..size);
            return Ok(HeadRead::Complete(head));
        }
        if pending.len() >= limit {
            return Ok(HeadRead::Rejected);
        }
        let take = buf.len().min(limit - pending.len());
        let n = sock.read(&mut buf[..take]).await?;
        if n == 0 {
            return Ok(if pending.is_empty() {
                HeadRead::Closed
            } else {
                HeadRead::Rejected
            });
        }
        pending.extend_from_slice(&buf[..n]);
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
    keep_alive: bool,
    range: RangeRequest,
    if_range: bool,
    /// Decoded, sanitized path segments (`/` = empty vec).
    segments: Vec<String>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Method {
    Get,
    Head,
}

/// One small range expression, bounded independently of the HTTP head. Keep
/// invalidity until resource lookup so a range cannot reveal a hidden length.
#[derive(Default)]
enum RangeRequest {
    #[default]
    Absent,
    Bytes(String),
    UnknownUnit,
    Invalid,
}

const MAX_RANGE_SPEC_BYTES: usize = 128;

impl RangeRequest {
    fn from_header(value: &str) -> Self {
        let Some((unit, spec)) = value.split_once('=') else {
            return Self::Invalid;
        };
        if unit.is_empty() || !unit.bytes().all(is_token_byte) {
            return Self::Invalid;
        }
        if !unit.eq_ignore_ascii_case("bytes") {
            return Self::UnknownUnit;
        }
        if spec.len() > MAX_RANGE_SPEC_BYTES || spec.contains(',') {
            return Self::Invalid;
        }
        Self::Bytes(spec.trim_matches([' ', '\t']).to_string())
    }
}

/// Parse one complete, bodyless origin-form request. Strict whitespace and
/// framing avoid disagreeing with a proxy about where the next request starts.
/// A refusal always terminates the connection; no trailing bytes are executed.
fn parse_request(head: &[u8]) -> std::result::Result<Request, Response> {
    let bad = || {
        Response::text(
            400,
            "Bad Request",
            "malformed or unsupported request framing\n",
        )
    };
    let text = std::str::from_utf8(head).map_err(|_| bad())?;
    let mut lines = text.split("\r\n");
    let mut parts = lines.next().unwrap_or_default().split(' ');
    let (Some(method), Some(target), Some(version)) = (parts.next(), parts.next(), parts.next())
    else {
        return Err(bad());
    };
    if parts.next().is_some()
        || method.is_empty()
        || !method.bytes().all(is_token_byte)
        || target.is_empty()
        || target.bytes().any(|b| b.is_ascii_control() || b == b'#')
        || !matches!(version, "HTTP/1.0" | "HTTP/1.1")
    {
        return Err(bad());
    }
    let mut host = false;
    let mut content_length = false;
    let mut close = version == "HTTP/1.0";
    let mut range = RangeRequest::Absent;
    let mut if_range = false;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            return Err(bad());
        };
        if name.is_empty()
            || !name.bytes().all(is_token_byte)
            || value.bytes().any(|b| (b < 0x20 && b != b'\t') || b == 0x7f)
        {
            return Err(bad());
        }
        let value = value.trim_matches([' ', '\t']);
        if name.eq_ignore_ascii_case("host") {
            if host
                || value.is_empty()
                || value
                    .bytes()
                    .any(|b| b.is_ascii_whitespace() || b",/\\?#@".contains(&b))
            {
                return Err(bad());
            }
            host = true;
        } else if name.eq_ignore_ascii_case("content-length") {
            // Reject duplicates even when equal. Only a single decimal zero
            // is bodyless; signs, lists, overflow and positive lengths refuse.
            if content_length || value.is_empty() || !value.bytes().all(|b| b == b'0') {
                return Err(bad());
            }
            content_length = true;
        } else if name.eq_ignore_ascii_case("transfer-encoding")
            || name.eq_ignore_ascii_case("upgrade")
        {
            return Err(bad());
        } else if name.eq_ignore_ascii_case("expect") {
            return Err(Response::text(
                417,
                "Expectation Failed",
                "request expectations are unsupported\n",
            ));
        } else if name.eq_ignore_ascii_case("range") {
            range = match range {
                RangeRequest::Absent => RangeRequest::from_header(value),
                _ => RangeRequest::Invalid,
            };
        } else if name.eq_ignore_ascii_case("if-range") {
            // We do not advertise an ETag or Last-Modified validator. Never
            // splice a possibly changed representation into a client's copy.
            if_range = true;
        } else if name.eq_ignore_ascii_case("connection") {
            for option in value.split(',').map(|s| s.trim_matches([' ', '\t'])) {
                if option.is_empty() || !option.bytes().all(is_token_byte) {
                    return Err(bad());
                }
                close |= option.eq_ignore_ascii_case("close");
                if option.eq_ignore_ascii_case("upgrade") {
                    return Err(bad());
                }
            }
        }
    }
    if version == "HTTP/1.1" && !host {
        return Err(bad());
    }
    let method = match method {
        "GET" => Method::Get,
        "HEAD" => Method::Head,
        _ => {
            let mut r = Response::text(405, "Method Not Allowed", "GET and HEAD only\n");
            r.headers.push(("Allow".into(), "GET, HEAD".into()));
            return Err(r);
        }
    };
    // Strip the query string; sanitize + decode the path.
    let raw_path = target.split('?').next().unwrap_or_default();
    let Some(segments) = sanitize_path(raw_path) else {
        return Err(Response::text(400, "Bad Request", "bad path\n"));
    };
    Ok(Request {
        method,
        keep_alive: !close,
        range,
        if_range,
        segments,
    })
}

/// RFC token grammar for method names, header names and Connection options.
fn is_token_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b)
}

/// Route an already validated request. HEAD suppression is applied uniformly
/// by the connection loop, including parser errors and rate-limit responses.
async fn respond(req: &Request, shared: &Arc<Shared>, web_root: Option<&Path>) -> Response {
    match req.segments.first().map(String::as_str) {
        // `/files` is the client page. Its descendants belong exclusively
        // to the authorized download handoff, never to the SPA fallback.
        Some("files") if req.segments.len() > 1 => serve_file_download(req, shared).await,
        Some(".well-known") => serve_well_known(req, shared),
        _ => serve_static(req, shared, web_root).await,
    }
}

// ---------------------------------------------------------------------------
// /.well-known/rabbithole/server: the signed discovery descriptor
// ---------------------------------------------------------------------------

/// Serve the signed [`crate::well_known`] descriptor as JSON. Only the exact
/// `/.well-known/rabbithole/server` path answers; anything else under
/// `/.well-known/` is a plain 404. Anonymous and unrate-gated beyond the
/// per-IP `legacy` request budget already spent in [`serve_conn`] — the document
/// is public by design.
fn serve_well_known(req: &Request, shared: &Arc<Shared>) -> Response {
    if req
        .segments
        .iter()
        .map(String::as_str)
        .ne([".well-known", "rabbithole", "server"])
    {
        return Response::text(404, "Not Found", "not found\n");
    }
    match crate::well_known::descriptor_json(shared, now_unix_millis()) {
        Some(json) => Response::new(200, "OK", "application/json".into(), json.into_bytes()),
        None => Response::text(500, "Internal Server Error", "descriptor unavailable\n"),
    }
}

/// Wall-clock unix milliseconds for the descriptor's `issued_at` (0 before the
/// epoch, which never happens).
fn now_unix_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
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
    let blobs = shared.blobs.clone();
    let bytes = match tokio::task::spawn_blocking(move || blobs.get(&BlobId(blob_id))).await {
        Ok(Ok(b)) => b,
        _ => return not_found(),
    };
    // MIME is stored from upload metadata. Never let control bytes turn its
    // Content-Type into extra headers or a forged next pipelined response.
    let mime = if target.mime.trim().is_empty() || target.mime.bytes().any(|b| b.is_ascii_control())
    {
        content_type_for(&target.name).to_string()
    } else {
        target.mime.clone()
    };
    // Range selection follows authorization and verified blob reading. Count
    // successful full/partial GETs once, never HEAD or a rejected range.
    let mut resp = file_response(req, mime, bytes);
    if resp.status >= 400 {
        return resp;
    }
    if req.method == Method::Get {
        match shared.files.record_download(node.id).await {
            Ok(_) => {}
            Err(FileError::NoSuchNode) => return not_found(),
            Err(e) => {
                tracing::warn!("http download counter failed: {e}");
                return Response::text(500, "Internal Server Error", "try again later\n");
            }
        }
    }
    resp.headers.push((
        "Content-Disposition".into(),
        format!(
            "attachment; filename=\"{}\"",
            disposition_name(&target.name)
        ),
    ));
    resp
}

enum RangeError {
    Invalid,
    Unsatisfiable,
}

/// Resolve one inclusive wire range into a bounded, end-exclusive slice.
/// RFC 9110 sections 14.1.2 / 14.2: only GET has range semantics, and an
/// unknown unit or zero-length representation may use the full response.
fn selected_range(req: &Request, len: usize) -> Result<Option<std::ops::Range<usize>>, RangeError> {
    if req.method != Method::Get || req.if_range || len == 0 {
        return Ok(None);
    }
    let spec = match &req.range {
        RangeRequest::Absent | RangeRequest::UnknownUnit => return Ok(None),
        RangeRequest::Invalid => return Err(RangeError::Invalid),
        RangeRequest::Bytes(spec) => spec,
    };
    let Some((first, last)) = spec.split_once('-') else {
        return Err(RangeError::Invalid);
    };
    let decimal = |value: &str| -> Result<u64, RangeError> {
        if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
            return Err(RangeError::Invalid);
        }
        value.parse().map_err(|_| RangeError::Invalid)
    };
    let len64 = len as u64;
    if first.is_empty() {
        let suffix = decimal(last)?;
        if suffix == 0 {
            return Err(RangeError::Unsatisfiable);
        }
        return Ok(Some(len64.saturating_sub(suffix) as usize..len));
    }
    let first = decimal(first)?;
    let last = if last.is_empty() {
        None
    } else {
        Some(decimal(last)?)
    };
    if last.is_some_and(|last| last < first) {
        return Err(RangeError::Invalid);
    }
    if first >= len64 {
        return Err(RangeError::Unsatisfiable);
    }
    // Clip before adding one, so a u64::MAX end cannot overflow. Cast only
    // values already bounded by a real in-memory representation length.
    let end = last.unwrap_or(len64 - 1).min(len64 - 1) + 1;
    Ok(Some(first as usize..end as usize))
}

/// Build a self-delimiting full, partial or refused file response. Selection
/// never amplifies the requested range into allocations or multipart work.
fn file_response(req: &Request, mime: String, mut bytes: Vec<u8>) -> Response {
    let len = bytes.len();
    let mut response = match selected_range(req, len) {
        Ok(None) => Response::new(200, "OK", mime, bytes),
        Ok(Some(range)) => {
            let content_range = format!("bytes {}-{}/{len}", range.start, range.end - 1);
            bytes.truncate(range.end);
            bytes.drain(..range.start);
            let mut response = Response::new(206, "Partial Content", mime, bytes);
            response
                .headers
                .push(("Content-Range".into(), content_range));
            response
        }
        Err(RangeError::Invalid) => {
            Response::text(400, "Bad Request", "one valid byte range is required\n")
        }
        Err(RangeError::Unsatisfiable) => {
            let mut response =
                Response::text(416, "Range Not Satisfiable", "range not satisfiable\n");
            response
                .headers
                .push(("Content-Range".into(), format!("bytes */{len}")));
            response
        }
    };
    response
        .headers
        .push(("Accept-Ranges".into(), "bytes".into()));
    response
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
/// `/manifest.webmanifest` falls back to a generated one, and known client
/// routes fall back to `index.html`. Existing static files take precedence;
/// unknown routes and missing assets stay 404. No web root = no shell.
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
    match read_under_root(root, &rel, private_dirs(shared)).await {
        Some(bytes) => {
            let name = rel.last().unwrap_or(&"");
            if req.segments.is_empty() {
                Response::new(200, "OK", content_type_for(name).to_string(), bytes)
            } else {
                file_response(req, content_type_for(name).to_string(), bytes)
            }
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
        None if is_client_route(&req.segments) => {
            // Use the same containment/private-directory checks as assets.
            // An index resolving outside the root or into private storage
            // must not become a shell.
            match read_under_root(root, &["index.html"], private_dirs(shared)).await {
                Some(bytes) => {
                    Response::new(200, "OK", content_type_for("index.html").to_string(), bytes)
                }
                None => not_found(),
            }
        }
        None => not_found(),
    }
}

/// Non-root routes owned by the SPA router in `ui-web/src/app.rs`. Keep this
/// list in step with that router, rather than treating every missing file as
/// a navigation. Dynamic parameters can contain dots (board slugs and person
/// handles do), so a file-extension heuristic would reject valid routes.
fn is_client_route(segments: &[String]) -> bool {
    match segments {
        [page] => matches!(
            page.as_str(),
            "about"
                | "settings"
                | "people"
                | "transfers"
                | "you"
                | "lobby"
                | "boards"
                | "dms"
                | "directory"
                | "files"
                | "radio"
                | "servers"
                | "art"
                | "wishing-well"
                | "admin"
        ),
        [page, _] => matches!(page.as_str(), "people" | "boards" | "admin"),
        _ => false,
    }
}

/// The directories this server must never serve out of, whatever the web
/// root is set to: the burrow's own data directory — the signing seed, the
/// TLS key, the database with every password hash, every blob — and
/// wherever its snapshots go, which hold copies of all of it.
///
/// `http_web_root` is an ordinary config key, settable live by anyone with
/// `CONFIG_ADMIN`, and nothing about a path says what is under it. Pointed
/// at the data directory (or at any parent of it), the static route would
/// answer `GET /identity/server_ed25519.seed` for anybody at all, with no
/// session and nothing in the audit log. The web root is checked when it is
/// set; this is the check that holds when it was not — a config file edited
/// by hand, a flag on the command line, a directory that became private
/// afterwards.
fn private_dirs(shared: &Arc<Shared>) -> Vec<PathBuf> {
    let cfg = shared.config.read();
    vec![cfg.data_dir.clone(), crate::backup::backups_dir(&cfg)]
}

/// Read `rel` under `root`, refusing anything that escapes it: the joined
/// path is canonicalized (resolving symlinks) and must still start with the
/// canonicalized root, must be a regular file — directories are 404 (no
/// listings) — and must not be inside any of `private` ([`private_dirs`]).
/// `None` = not served.
async fn read_under_root(root: &Path, rel: &[&str], private: Vec<PathBuf>) -> Option<Vec<u8>> {
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
        // The burrow's own directories are never served, however the web
        // root was arrived at. Component-wise, so a sibling named like a
        // private directory is not mistaken for one.
        for dir in &private {
            if let Ok(canon_private) = std::fs::canonicalize(dir) {
                if canon.starts_with(&canon_private) {
                    return None;
                }
            }
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
        // The SPA ships its display face.
        "woff2" => "font/woff2",
        "woff" => "font/woff",
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
    close: bool,
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
            close: true,
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

    /// Serialize a self-delimiting response. HEAD keeps the GET length but
    /// emits no body, so a following pipelined response starts immediately.
    fn to_bytes(&self) -> Vec<u8> {
        let mut head = format!(
            "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: {}\r\n",
            self.status,
            self.reason,
            self.content_type,
            self.body.len(),
            if self.close { "close" } else { "keep-alive" },
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

    #[tokio::test]
    async fn idle_or_unfinished_heads_have_a_bounded_deadline() {
        // A short test-only deadline exercises the production read path
        // without waiting through its fifteen-second idle allowance.
        for prefix in [b"".as_slice(), b"HEAD / HTTP/1.1\r\nHost: partial"] {
            let (mut peer, mut reader) = tokio::io::duplex(64);
            peer.write_all(prefix).await.unwrap();
            let mut pending = Vec::new();
            let result = read_head(
                &mut reader,
                &mut pending,
                MAX_CONNECTION_BYTES,
                Duration::from_millis(10),
            )
            .await
            .unwrap();
            assert!(matches!(result, HeadRead::TimedOut));
            assert_eq!(
                pending, prefix,
                "short reads remain available for HEAD/error handling"
            );
        }
    }

    #[tokio::test]
    async fn head_reader_preserves_pipeline_and_distinguishes_clean_eof() {
        let first = b"GET / HTTP/1.1\r\nHost: test\r\n\r\n";
        let second = b"HEAD /lobby HTTP/1.1\r\nHost: test\r\n\r\n";
        let (mut peer, mut reader) = tokio::io::duplex(128);
        peer.write_all(first).await.unwrap();
        peer.write_all(second).await.unwrap();
        peer.shutdown().await.unwrap();
        let mut pending = Vec::new();
        for expected in [first.as_slice(), second.as_slice()] {
            let result = read_head(
                &mut reader,
                &mut pending,
                MAX_CONNECTION_BYTES,
                Duration::from_secs(1),
            )
            .await
            .unwrap();
            let HeadRead::Complete(actual) = result else {
                panic!("complete head expected")
            };
            assert_eq!(actual, expected[..expected.len() - 4]);
        }
        assert!(matches!(
            read_head(
                &mut reader,
                &mut pending,
                MAX_CONNECTION_BYTES,
                Duration::from_secs(1)
            )
            .await
            .unwrap(),
            HeadRead::Closed
        ));
    }

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
        assert_eq!(content_type_for("space-grotesk-latin.woff2"), "font/woff2");
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
