//! The network edge: HTTPS to `rabbithole.directory`, TCP to a tracker.
//!
//! Native only. A browser tab has no TCP and reaches the directory through its
//! own `fetch`, so this whole module sits behind the `native` feature and the
//! wasm build never sees rustls.
//!
//! The HTTP client here is deliberately small — one GET, no redirects, no
//! conditional requests, no cookies — because that is the entire requirement.
//! Everything it reads is size-capped and time-bounded: discovery talks to
//! hosts you have not chosen and cannot vouch for, so an unbounded read is a
//! way for a stranger to hang your client.

use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::{
    host_header, parse_directory_json_with, parse_glass_json, parse_tracker_index,
    pick_live_listing, split_authority, status_addr, DirectoryServer, DirectorySource,
    DIRECTORY_URL, TRACKER_URL,
};

/// Overall budget for one source. Discovery is something a person is waiting
/// on, so a slow source has to lose to the fallback rather than stall the view.
const TIMEOUT: Duration = Duration::from_secs(8);

/// Response cap. The directory serves a few hundred rows; anything past this is
/// not a listing.
const MAX_RESPONSE: usize = 1024 * 1024;

/// What a discovery attempt produced.
pub struct Listing {
    pub servers: Vec<DirectoryServer>,
    pub source: DirectorySource,
    /// Why the wider source was not used, when a narrower one answered. Worth
    /// showing: "the directory is down" and "there is nobody out there" look
    /// identical in a list otherwise.
    pub fallback_reason: Option<String>,
}

/// Ask who is out there: `rabbithole.directory` first, then the standard
/// Looking Glass behind it.
///
/// `tracker` names a coordinator to ask **instead**. Naming one is a choice, so
/// it gets no fallback — quietly answering from somewhere else would be a
/// different answer. A named coordinator is tried as HTTPS `/api/burrows` and
/// then on its line-oriented status port, which is what a self-hosted
/// `looking-glass` serves.
///
/// `endpoint_fields` selects which URIs count as dialable, in preference order
/// — a WebSocket-only client passes `["ws"]`, a QUIC-speaking one
/// `["ws", "quic"]`. (The directory spells these `wsUri`/`quicUri`; the
/// suffix is added here so callers state the protocol once.)
pub async fn discover(tracker: Option<&str>, endpoint_fields: &[&str]) -> Result<Listing, String> {
    let key = discovery_cache_key(tracker, endpoint_fields);
    match discover_live(tracker, endpoint_fields).await {
        Ok(mut listing) => {
            let previous = read_cached(&key);
            save_cached(&key, &mut listing, previous.as_ref());
            Ok(listing)
        }
        Err(error) => match read_cached(&key) {
            Some(saved) => {
                let mut servers = saved.servers;
                // Saved observations are not current population or uptime.
                for row in &mut servers {
                    row.users_online = None;
                    row.uptime_pct = None;
                }
                Ok(Listing {
                    servers,
                    source: DirectorySource::Cached(format!(
                        "saved {} · {}",
                        saved.source_label,
                        saved_age_label(saved.fetched_at_ms, now_ms())
                    )),
                    fallback_reason: Some(error),
                })
            }
            None => Err(error),
        },
    }
}

async fn discover_live(tracker: Option<&str>, endpoint_fields: &[&str]) -> Result<Listing, String> {
    if let Some(named) = tracker.map(str::trim).filter(|t| !t.is_empty()) {
        return named_tracker(named, endpoint_fields).await;
    }

    let mut directory = match fetch_directory(endpoint_fields).await {
        Ok(servers) => Ok((servers, DirectorySource::Directory)),
        Err(e) => Err(e),
    };
    // Ask only the configured standard glass for complete statements. The
    // public aggregate may omit them; no row-supplied URL is ever fetched.
    let glass = match fetch_glass(TRACKER_URL, endpoint_fields).await {
        Ok(servers) => Some(Ok((servers, DirectorySource::standard_glass()))),
        Err(error) => Some(Err(error)),
    };
    if let (Ok((rows, _)), Some(Ok((proof_rows, _)))) = (&mut directory, &glass) {
        crate::verification::with_proofs_from(rows, proof_rows, now_ms());
    }

    let directory_error = directory.as_ref().err().cloned();
    let glass_error = glass.as_ref().and_then(|g| g.as_ref().err().cloned());
    let answers = std::iter::once(directory).chain(glass);
    pick_live_listing(answers)
        .map(|live| Listing {
            servers: live.servers,
            source: live.source,
            fallback_reason: live.fallback_reason,
        })
        .ok_or_else(|| {
            let directory_error = directory_error.unwrap_or_else(|| "directory silent".into());
            match glass_error {
                Some(glass_error) => {
                    format!("{directory_error} The tracker didn't answer either: {glass_error}")
                }
                None => directory_error,
            }
        })
}

/// Ask one named coordinator. HTTPS first (what a hosted glass serves), then
/// its status port (what the self-hosted `looking-glass` binary serves).
async fn named_tracker(entry: &str, endpoint_fields: &[&str]) -> Result<Listing, String> {
    let https_error = if entry.starts_with("http://") {
        // An explicit plaintext entry is a local coordinator; skip straight to
        // the status port rather than pretending we tried HTTPS.
        "not tried (plaintext entry)".to_string()
    } else {
        let url = if entry.contains("://") {
            let e = entry.trim().trim_end_matches('/');
            if e.contains("/api/burrows") {
                e.to_string()
            } else {
                format!("{e}/api/burrows")
            }
        } else {
            format!("https://{}/api/burrows", entry.trim_end_matches('/'))
        };
        match fetch_glass(&url, endpoint_fields).await {
            Ok(servers) if !servers.is_empty() => {
                return Ok(Listing {
                    servers,
                    source: DirectorySource::looking_glass(entry),
                    fallback_reason: None,
                })
            }
            Ok(_) => "listed nobody".to_string(),
            Err(e) => e,
        }
    };

    let https = if https_error == "not tried (plaintext entry)" {
        Err(https_error.clone())
    } else if https_error == "listed nobody" {
        Ok((Vec::new(), DirectorySource::looking_glass(entry)))
    } else {
        Err(https_error.clone())
    };

    let addr = tracker_addr(entry);
    let tcp = match query_tracker(&addr, "INDEX").await {
        Ok(text) => parse_tracker_index(&text)
            .map(|servers| (servers, DirectorySource::looking_glass(entry))),
        Err(e) => Err(e),
    };

    pick_live_listing([https, tcp])
        .map(|live| Listing {
            servers: live.servers,
            source: live.source,
            fallback_reason: live
                .fallback_reason
                .filter(|_| https_error != "not tried (plaintext entry)"),
        })
        .ok_or_else(|| format!("{entry} didn't answer over HTTPS ({https_error}) or on {addr}"))
}

/// Fetch and parse a Looking Glass listing over HTTPS.
pub async fn fetch_glass(
    url: &str,
    endpoint_kinds: &[&str],
) -> Result<Vec<DirectoryServer>, String> {
    let body = https_get(url).await?;
    parse_glass_json(&body, endpoint_kinds)
}

/// Fetch and parse the directory snapshot over HTTPS.
pub async fn fetch_directory(endpoint_fields: &[&str]) -> Result<Vec<DirectoryServer>, String> {
    let body = https_get(DIRECTORY_URL).await?;
    // The directory names its endpoints `wsUri` / `quicUri`; callers state the
    // protocol once and the suffix is applied here.
    let fields: Vec<String> = endpoint_fields.iter().map(|k| format!("{k}Uri")).collect();
    let refs: Vec<&str> = fields.iter().map(String::as_str).collect();
    parse_directory_json_with(&body, &refs)
}

/// Normalize a tracker entry into `host:port`, appending the status port when
/// the user typed a bare host. Loopback uses 5497 (`just up`); everyone else
/// uses the public 4655.
pub fn tracker_addr(entry: &str) -> String {
    status_addr(entry)
}

/// One command/reply exchange with a tracker's status port.
pub async fn query_tracker(addr: &str, command: &str) -> Result<String, String> {
    let run = async {
        let mut sock = TcpStream::connect(addr)
            .await
            .map_err(|e| format!("connect {addr}: {e}"))?;
        sock.write_all(format!("{command}\n").as_bytes())
            .await
            .map_err(|e| format!("send: {e}"))?;
        let mut buf = Vec::new();
        // The status port is one-shot: it answers and closes, so reading to EOF
        // is the framing. The cap is what keeps a hostile "tracker" from
        // streaming forever.
        let mut chunk = [0u8; 16 * 1024];
        loop {
            let n = sock
                .read(&mut chunk)
                .await
                .map_err(|e| format!("read: {e}"))?;
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&chunk[..n]);
            if buf.len() > MAX_RESPONSE {
                return Err("the tracker's reply is too large to be a listing".to_string());
            }
        }
        Ok(String::from_utf8_lossy(&buf).into_owned())
    };
    tokio::time::timeout(TIMEOUT, run)
        .await
        .map_err(|_| format!("{addr} did not answer within {}s", TIMEOUT.as_secs()))?
}

/// A single HTTPS GET, returning the body of a 2xx response.
async fn https_get(url: &str) -> Result<String, String> {
    let (host, port, path) = split_url(url)?;
    let run = async {
        let tcp = TcpStream::connect((host.as_str(), port))
            .await
            .map_err(|e| format!("connect {host}:{port}: {e}"))?;
        let request = format!(
            "GET {path} HTTP/1.1\r\nHost: {}\r\nUser-Agent: rabbithole/{}\r\nAccept: application/json\r\nConnection: close\r\n\r\n",
            host_header(&host, port, 443),
            env!("CARGO_PKG_VERSION"),
        );
        let raw = tls_exchange(tcp, &host, request.as_bytes()).await?;
        let (status, body) = split_response(&raw)?;
        if !(200..300).contains(&status) {
            return Err(format!("{host} answered {status}"));
        }
        Ok(body)
    };
    tokio::time::timeout(TIMEOUT, run)
        .await
        .map_err(|_| format!("{host} did not answer within {}s", TIMEOUT.as_secs()))?
}

/// `https://host[:port]/path` → `(host, port, path)`. Only HTTPS: discovery
/// endpoints are public URLs we ship, and accepting a plaintext one here would
/// make a downgrade a config typo away.
fn split_url(url: &str) -> Result<(String, u16, String), String> {
    let rest = url
        .strip_prefix("https://")
        .ok_or_else(|| format!("{url} is not an https:// URL"))?;
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let (host, port) = split_authority(authority, 443).map_err(|e| format!("{url}: {e}"))?;
    if host.is_empty() {
        return Err(format!("{url} has no host"));
    }
    Ok((host, port, path.to_string()))
}

/// The shared rustls client config: webpki (Mozilla) roots, no client auth.
fn tls_config() -> Arc<rustls::ClientConfig> {
    static CONFIG: std::sync::OnceLock<Arc<rustls::ClientConfig>> = std::sync::OnceLock::new();
    CONFIG
        .get_or_init(|| {
            let _ = rustls::crypto::ring::default_provider().install_default();
            let mut roots = rustls::RootCertStore::empty();
            roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
            Arc::new(
                rustls::ClientConfig::builder()
                    .with_root_certificates(roots)
                    .with_no_client_auth(),
            )
        })
        .clone()
}

async fn tls_exchange(tcp: TcpStream, host: &str, request: &[u8]) -> Result<Vec<u8>, String> {
    let server_name = rustls_pki_types::ServerName::try_from(host.to_string())
        .map_err(|_| format!("{host} is not a valid TLS server name"))?;
    let connector = tokio_rustls::TlsConnector::from(tls_config());
    let mut stream = connector
        .connect(server_name, tcp)
        .await
        .map_err(|e| format!("tls handshake with {host}: {e}"))?;
    stream
        .write_all(request)
        .await
        .map_err(|e| format!("send: {e}"))?;
    let mut response = Vec::new();
    let mut buf = [0u8; 16 * 1024];
    loop {
        match stream.read(&mut buf).await {
            Ok(0) => break,
            Ok(n) => {
                response.extend_from_slice(&buf[..n]);
                if response.len() > MAX_RESPONSE {
                    return Err("the reply is too large to be a listing".to_string());
                }
            }
            // A close without `close_notify`. rustls reports it as an error
            // because in general it could be a truncation attack, but plenty
            // of real servers and CDNs just close the socket — including the
            // ones we have to read. The response is framed by Content-Length
            // or chunked encoding, and `split_response` rejects a body that
            // doesn't match, so a genuine truncation still fails there rather
            // than here.
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(format!("read: {e}")),
        }
    }
    let _ = stream.shutdown().await;
    Ok(response)
}

/// Split a raw HTTP/1.1 response into `(status, body)`, decoding a chunked
/// body. Total over arbitrary bytes: a malformed reply is an `Err`.
///
/// Framing is done on **bytes**, then the assembled body is decoded as UTF-8.
/// Interpreting the stream as a string first would both corrupt a UTF-8
/// character split across chunks (lossy replacement) and panic if a chunk
/// size landed mid-character on the already-decoded `&str`.
fn split_response(raw: &[u8]) -> Result<(u16, String), String> {
    let head_end = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or_else(|| "the reply has no header/body break".to_string())?;
    let head = String::from_utf8_lossy(&raw[..head_end]);
    let mut lines = head.lines();
    let status: u16 = lines
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| "the reply has no status line".to_string())?;
    let headers: Vec<String> = lines.map(|l| l.to_ascii_lowercase()).collect();
    let body = &raw[head_end + 4..];
    let chunked = headers
        .iter()
        .any(|l| l.starts_with("transfer-encoding:") && l.contains("chunked"));
    if chunked {
        // The terminating zero-length chunk is the framing; `dechunk` errors
        // without it, so a truncated stream cannot read as a complete body.
        return Ok((
            status,
            String::from_utf8_lossy(&dechunk(body)?).into_owned(),
        ));
    }
    // Not chunked: `Content-Length` is the framing, and a short body means the
    // connection was cut mid-reply. Accepting it would hand a truncated
    // listing to the parser as though it were the whole thing.
    if let Some(len) = headers
        .iter()
        .find_map(|l| l.strip_prefix("content-length:"))
        .and_then(|v| v.trim().parse::<usize>().ok())
    {
        if body.len() < len {
            return Err(format!(
                "the reply was cut short: {} of {len} bytes",
                body.len()
            ));
        }
        return Ok((status, String::from_utf8_lossy(&body[..len]).into_owned()));
    }
    Ok((status, String::from_utf8_lossy(body).into_owned()))
}

/// Decode a `Transfer-Encoding: chunked` body. Chunk sizes are byte counts
/// of the original stream, so this stays on `[u8]`.
fn dechunk(body: &[u8]) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    let mut rest = body;
    loop {
        let line_end = rest
            .windows(2)
            .position(|w| w == b"\r\n")
            .ok_or_else(|| "truncated chunk header".to_string())?;
        let size_line =
            std::str::from_utf8(&rest[..line_end]).map_err(|_| "bad chunk header".to_string())?;
        // A chunk size may carry `;extensions`, which are not part of the size.
        let size_hex = size_line.split(';').next().unwrap_or("").trim();
        let size = usize::from_str_radix(size_hex, 16)
            .map_err(|_| format!("bad chunk size {size_hex:?}"))?;
        rest = &rest[line_end + 2..];
        if size == 0 {
            return Ok(out);
        }
        if rest.len() < size {
            return Err("truncated chunk body".to_string());
        }
        out.extend_from_slice(&rest[..size]);
        rest = rest[size..].strip_prefix(b"\r\n").unwrap_or(&rest[size..]);
    }
}

fn saved_age_label(saved_at: i64, now: i64) -> String {
    let minutes = now.saturating_sub(saved_at).max(0) / 60_000;
    match minutes {
        0 => "just now".into(),
        1..=59 => format!("{minutes} min ago"),
        60..=1439 => format!("{} h ago", minutes / 60),
        _ => format!("{} days ago", minutes / 1440),
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis().min(i64::MAX as u128) as i64)
        .unwrap_or(0)
}

fn cache_path(key: &str) -> Option<std::path::PathBuf> {
    if key.len() > 256 {
        return None;
    }
    let base = std::env::var_os("XDG_CACHE_HOME")
        .filter(|s| !s.is_empty())
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|home| std::path::PathBuf::from(home).join(".cache"))
        })?;
    Some(base.join("rabbithole").join(format!(
        "directory-{}.json",
        blake3::hash(key.as_bytes()).to_hex()
    )))
}

fn read_cached(key: &str) -> Option<crate::cache::CachedListing> {
    read_cached_file(&cache_path(key)?, key)
}

fn read_cached_file(path: &std::path::Path, key: &str) -> Option<crate::cache::CachedListing> {
    use std::io::Read;
    let file = std::fs::File::open(path).ok()?;
    let mut text = String::new();
    file.take(crate::cache::MAX_CACHE_BYTES as u64 + 1)
        .read_to_string(&mut text)
        .ok()?;
    crate::cache::decode(key, &text)
}

fn discovery_cache_key(tracker: Option<&str>, protocols: &[&str]) -> String {
    // Structured source variants cannot collide with an explicitly named
    // tracker called "default", or delimiter characters in a hostname.
    serde_json::to_string(&(tracker.map(str::trim).filter(|s| !s.is_empty()), protocols))
        .expect("strings encode")
}

fn save_cached(key: &str, listing: &mut Listing, previous: Option<&crate::cache::CachedListing>) {
    let Some(text) = crate::cache::update(
        key,
        listing.source.label(),
        now_ms(),
        &mut listing.servers,
        previous,
    ) else {
        return;
    };
    let Some(path) = cache_path(key) else {
        return;
    };
    let _ = write_cached_file(&path, &text);
}

fn write_cached_file(path: &std::path::Path, text: &str) -> std::io::Result<()> {
    use std::io::Write;
    let parent = path
        .parent()
        .ok_or_else(|| std::io::Error::other("cache has no parent"))?;
    std::fs::create_dir_all(parent)?;
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let temporary = path.with_extension(format!("{}-{nonce}.tmp", std::process::id()));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let result = (|| {
        let mut file = options.open(&temporary)?;
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
        std::fs::rename(&temporary, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(temporary);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bare_tracker_host_gets_the_status_port() {
        assert_eq!(
            tracker_addr("tracker.rabbit.direct"),
            "tracker.rabbit.direct:4655"
        );
        assert_eq!(tracker_addr(" glass.example "), "glass.example:4655");
        assert_eq!(tracker_addr("glass.example:9000"), "glass.example:9000");
        // A URL is a coordinator name, not a TCP address. The scheme colon
        // must not be read as "already has a port".
        assert_eq!(
            tracker_addr("https://tracker.rabbit.direct/api/burrows"),
            "tracker.rabbit.direct:4655"
        );
        assert_eq!(tracker_addr("http://127.0.0.1:3000/"), "127.0.0.1:3000");
        let local = crate::local_status_port();
        assert_eq!(tracker_addr("localhost"), format!("localhost:{local}"));
        assert_eq!(tracker_addr("127.0.0.1"), format!("127.0.0.1:{local}"));
    }

    #[test]
    fn ipv6_literals_keep_their_hextets() {
        // Splitting on the last colon would turn `::1` into host `:` port `1`.
        // Loopback without a port is the local stack, bracketed so
        // TcpStream::connect can parse it.
        let local = crate::local_status_port();
        assert_eq!(tracker_addr("::1"), format!("[::1]:{local}"));
        assert_eq!(tracker_addr("[::1]:4655"), "[::1]:4655");
    }

    #[test]
    fn urls_split_into_host_port_and_path() {
        assert_eq!(
            split_url(DIRECTORY_URL).unwrap(),
            ("rabbithole.directory".into(), 443, "/api/burrows".into())
        );
        assert_eq!(
            split_url("https://glass.example:8443").unwrap(),
            ("glass.example".into(), 8443, "/".into())
        );
        assert_eq!(
            split_url("https://[::1]/api/burrows").unwrap(),
            ("::1".into(), 443, "/api/burrows".into())
        );
        assert_eq!(
            split_url("https://[2001:db8::1]:8443/api").unwrap(),
            ("2001:db8::1".into(), 8443, "/api".into())
        );
        // Plaintext would make a downgrade one config typo away.
        assert!(split_url("http://rabbithole.directory/api/burrows").is_err());
        assert!(split_url("https://").is_err());
    }

    #[test]
    fn responses_split_into_status_and_body() {
        let raw = b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n{\"ok\":true}";
        assert_eq!(split_response(raw).unwrap(), (200, "{\"ok\":true}".into()));

        let raw = b"HTTP/1.1 503 Service Unavailable\r\n\r\nnope";
        assert_eq!(split_response(raw).unwrap().0, 503);

        assert!(split_response(b"garbage").is_err());
        assert!(split_response(b"not a status line\r\n\r\nbody").is_err());
    }

    #[test]
    fn chunked_bodies_are_reassembled() {
        // The directory is served through a CDN, which chunks; a client that
        // couldn't read that would parse chunk-size lines as JSON.
        let raw = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\n{\"ok\"\r\nc\r\n:true,\"a\":1}\r\n0\r\n\r\n";
        assert_eq!(
            split_response(raw).unwrap(),
            (200, "{\"ok\":true,\"a\":1}".into())
        );
    }

    #[test]
    fn a_truncated_chunked_body_is_an_error_not_a_panic() {
        assert!(dechunk(b"5\r\nab").is_err(), "body shorter than declared");
        assert!(dechunk(b"zz\r\n").is_err(), "size is not hex");
        assert!(dechunk(b"").is_err(), "no chunk header at all");
    }

    #[test]
    fn a_utf8_character_split_across_chunks_survives() {
        // `é` is `c3 a9`. A CDN that chunks on byte 1 used to hand a `&str`
        // slice that was not a char boundary — panic — or, after lossy
        // conversion, a replacement character instead of the letter.
        let raw = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n1\r\n\xc3\r\n1\r\n\xa9\r\n0\r\n\r\n";
        assert_eq!(split_response(raw).unwrap(), (200, "é".into()));
    }

    /// Read the real `rabbithole.directory` and the real tracker.
    ///
    /// Ignored by default — it needs the network, and CI shouldn't depend on a
    /// third-party service being up. It exists because the parsers are pinned
    /// against a *copy* of a reply, and a copy stops being evidence the moment
    /// the service changes shape. Run with
    /// `cargo test -p rabbithole-directory --features native -- --ignored --nocapture`.
    #[tokio::test]
    #[ignore = "requires network access to rabbithole.directory"]
    async fn the_live_services_still_serve_what_we_parse() {
        let listing = discover(None, &["ws", "quic"])
            .await
            .expect("one of the two sources answers");
        println!(
            "source: {} ({} burrows){}",
            listing.source.label(),
            listing.servers.len(),
            listing
                .fallback_reason
                .as_ref()
                .map(|r| format!(" — fell back: {r}"))
                .unwrap_or_default()
        );
        for s in listing.servers.iter().take(5) {
            println!(
                "  {:<28} {:<38} {}",
                s.name,
                s.endpoint,
                s.uptime_pct
                    .map(|p| format!("{p}%"))
                    .unwrap_or_else(|| "-".into())
            );
        }
        assert!(!listing.servers.is_empty());
        assert!(
            listing.servers.iter().all(|s| !s.endpoint.is_empty()),
            "every row is dialable"
        );

        // And the fallback path independently, so a working directory doesn't
        // hide a broken tracker until the day the directory goes down.
        // And the fallback path independently, so a working directory doesn't
        // hide a broken glass until the day the directory goes down.
        let body = https_get(TRACKER_URL).await.expect("the glass answers");
        println!("glass /api/burrows: {} bytes", body.len());
        match parse_glass_json(&body, &["ws", "quic"]) {
            Ok(rows) => println!("  {} dialable", rows.len()),
            // An empty glass is a legitimate state (nobody has announced) and
            // not this test's business; a *malformed* reply would be.
            Err(e) => assert!(
                e.contains("no burrows this client can dial"),
                "the glass reply did not parse: {e}"
            ),
        }
        assert!(
            crate::json::parse(&body).is_ok(),
            "the glass still serves JSON we can read"
        );
    }

    #[tokio::test]
    async fn an_unreachable_tracker_reports_rather_than_hangs() {
        // Port 1 on loopback refuses immediately — the error path, fast.
        let err = query_tracker("127.0.0.1:1", "INDEX").await.unwrap_err();
        assert!(err.contains("connect"), "{err}");
    }
}

#[cfg(test)]
mod cache_file_tests {
    use super::*;

    #[test]
    fn explicit_tracker_and_automatic_sources_have_distinct_cache_keys() {
        assert_ne!(
            discovery_cache_key(None, &["ws"]),
            discovery_cache_key(Some("default"), &["ws"])
        );
        assert_ne!(
            discovery_cache_key(Some("a|ws"), &["quic"]),
            discovery_cache_key(Some("a"), &["ws", "quic"])
        );
        assert_ne!(
            discovery_cache_key(None, &["ws"]),
            discovery_cache_key(None, &["quic"])
        );
    }

    #[test]
    fn disk_cache_roundtrip_is_atomic_bounded_and_source_bound() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "rabbithole-directory-cache-{}-{nonce}",
            std::process::id()
        ));
        let path = dir.join("listing.json");
        let text = crate::cache::encode("fixture", 123, &[]).unwrap();
        write_cached_file(&path, &text).unwrap();
        assert_eq!(
            read_cached_file(&path, "fixture").unwrap().fetched_at_ms,
            123
        );
        assert!(read_cached_file(&path, "different").is_none());
        write_cached_file(&path, &crate::cache::encode("fixture", 456, &[]).unwrap()).unwrap();
        assert_eq!(
            read_cached_file(&path, "fixture").unwrap().fetched_at_ms,
            456
        );
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
        std::fs::write(&path, " ".repeat(crate::cache::MAX_CACHE_BYTES + 1)).unwrap();
        assert!(read_cached_file(&path, "fixture").is_none());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
