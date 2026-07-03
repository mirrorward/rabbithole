//! NNTP peer-feed (transit) listener (Wave 10): the surface a *peer news
//! server* pushes articles into, as opposed to [`crate::nntp`], which serves
//! newsreaders. It speaks the classic `IHAVE` offer (RFC 3977 §6.3.2) and the
//! RFC 4644 streaming feed (`MODE STREAM`, `CHECK`, `TAKETHIS`), plus
//! `NEWNEWS` so a peer can pull the message-ids of what appeared here since a
//! date.
//!
//! The `rabbithole-legacy-nntp` crate supplies the pure pieces — the
//! [`Exchange`] transit state machine renders every 235/238/239/335/431/435/
//! 436/437/438/439 reply (echoing the message-id for the streaming verbs),
//! [`wildmat`] and [`datetime`] decode the `NEWNEWS` arguments, and
//! [`new_articles_block`] frames its body. This module is the bridge to
//! [`Shared`]: peers authenticate, offers are deduped, and accepted articles
//! land as real board posts.
//!
//! # Peer authentication
//!
//! Peers authenticate with `AUTHINFO USER`/`PASS` against the TOML-only
//! `nntp_feed_peers` allowlist (user → password). An **empty allowlist
//! refuses every peer** (fail safe), and every transit verb — `MODE STREAM`,
//! `IHAVE`, `CHECK`, `TAKETHIS`, `NEWNEWS` — answers `480` until the peer has
//! authenticated.
//!
//! # TLS
//!
//! The feed shares the reader surface's TLS plumbing ([`crate::nntp`]): an
//! optional implicit-TLS listener (`nntp_feed_tls_enabled` /
//! `nntp_feed_tls_addr`), `STARTTLS` on the plaintext listener (RFC 4642 —
//! `382`, handshake, fresh session state, refused with `502` once secure),
//! and the RFC 4643 `AUTHINFO` gate: with `nntp_auth_require_tls` (the
//! default) a plaintext `AUTHINFO` answers `483` and the capability list
//! omits `AUTHINFO` until the connection is secured.
//!
//! # Dedupe
//!
//! Offered Message-IDs are checked against the shared [`DedupStore`] using the
//! **existing** [`SeenKey::MessageId`] namespace — it is exactly the
//! "Usenet/NNTP Message-ID" identity this surface trades in, so no new variant
//! is minted. An id is recorded once its article is *settled* (accepted **or**
//! rejected — our rejections are permanent validation failures, and 437/439
//! mean "do not retry"). Native ids (`<hex(event id)@…>` that resolve to a
//! stored post) are refused without consulting the window, so a peer echoing
//! our own articles back never duplicates them even after the window expires.
//!
//! # Gateway authorship
//!
//! Accepted articles are posted through [`BoardService`] with a deterministic
//! gateway author seed namespaced away from native per-account seeds
//! (mirroring `ftn.rs`). Authors render as `{name}@usenet`, which never ends
//! in `@{origin}` — the same origin-suffix discipline that keeps the FTN
//! gateway from re-scanning injected content.
//!
//! [`BoardService`]: rabbithole_server_core::BoardService
//! [`DedupStore`]: rabbithole_server_core::DedupStore
//! [`Exchange`]: rabbithole_legacy_nntp::Exchange
//! [`wildmat`]: rabbithole_legacy_nntp::wildmat
//! [`datetime`]: rabbithole_legacy_nntp::datetime
//! [`new_articles_block`]: rabbithole_legacy_nntp::new_articles_block

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::Result;
use chrono::Datelike;
use rabbithole_legacy_nntp::{
    datablock, datetime, new_articles_block, wildmat, Command, DateTimeSpec, Exchange, MessageId,
    OfferVerb, Response, Status,
};
use rabbithole_server_core::{SeenKey, ServerEvent};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;
use tokio_rustls::TlsAcceptor;

use rabbithole_server_core::ratelimit::{class as rl, Scope};

use crate::nntp::{
    event_id_from_message_id, group_articles, message_id_for, ParsedArticle, SessionEnd,
    SessionSecurity,
};
use crate::Shared;

/// Bind + serve the NNTP peer-feed surface. Returns the bound address (useful
/// when the config asked for port 0) and the accept-loop task handle. Mirrors
/// [`crate::nntp::spawn_nntp`]. The `acceptor` serves `STARTTLS` upgrades
/// (RFC 4642).
pub async fn spawn_nntp_feed(
    shared: Arc<Shared>,
    addr: SocketAddr,
    acceptor: TlsAcceptor,
) -> Result<(SocketAddr, JoinHandle<()>)> {
    let listener = TcpListener::bind(addr).await?;
    let local = listener.local_addr()?;
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
            let acceptor = acceptor.clone();
            tokio::spawn(async move {
                if let Err(e) = serve_plain(sock, shared, Some(peer.ip()), acceptor).await {
                    tracing::debug!("nntp feed session error: {e}");
                }
            });
        }
    });
    Ok((local, handle))
}

/// Bind + serve the peer-feed surface over implicit TLS — the transit
/// counterpart of [`crate::nntp::spawn_nntps`].
pub async fn spawn_nntp_feed_tls(
    shared: Arc<Shared>,
    addr: SocketAddr,
    acceptor: TlsAcceptor,
) -> Result<(SocketAddr, JoinHandle<()>)> {
    let listener = TcpListener::bind(addr).await?;
    let local = listener.local_addr()?;
    let handle = tokio::spawn(async move {
        loop {
            let Ok((sock, peer)) = listener.accept().await else {
                break;
            };
            if !shared.rate_allow(Scope::Ip(peer.ip()), rl::CONN) {
                continue;
            }
            let shared = shared.clone();
            let acceptor = acceptor.clone();
            tokio::spawn(async move {
                // A failed handshake dooms only this connection.
                match acceptor.accept(sock).await {
                    Ok(tls) => {
                        if let Err(e) =
                            session_loop(tls, shared, Some(peer.ip()), SessionSecurity::ImplicitTls)
                                .await
                        {
                            tracing::debug!("nntp feed tls session error: {e}");
                        }
                    }
                    Err(e) => tracing::debug!("nntp feed tls handshake failed: {e}"),
                }
            });
        }
    });
    Ok((local, handle))
}

/// One plaintext peer connection: run the session loop, and if it asks for a
/// TLS upgrade, handshake and run a fresh secured loop (RFC 4642).
async fn serve_plain(
    sock: tokio::net::TcpStream,
    shared: Arc<Shared>,
    peer_ip: Option<std::net::IpAddr>,
    acceptor: TlsAcceptor,
) -> Result<()> {
    match session_loop(sock, shared.clone(), peer_ip, SessionSecurity::Plain).await? {
        SessionEnd::Closed => {}
        SessionEnd::StartTls(sock) => {
            let tls = acceptor.accept(sock).await?;
            // A secured loop refuses STARTTLS, so it can only end Closed.
            session_loop(tls, shared, peer_ip, SessionSecurity::UpgradedTls).await?;
        }
    }
    Ok(())
}

/// Per-connection peer state.
struct FeedSession {
    /// The allowlist user once `AUTHINFO USER`/`PASS` succeeds.
    authed: Option<String>,
    /// Username captured by `AUTHINFO USER`, awaiting `AUTHINFO PASS`.
    pending_user: Option<String>,
}

/// A stable author signing seed for feed-gateway board posts, namespaced away
/// from the native per-account seed and the FTN gateway seed so none collide.
fn feed_author_seed(shared: &Shared, key: &str) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"rabbithole-nntp-feed-author-seed-v1");
    hasher.update(&shared.server_signing_seed);
    hasher.update(key.as_bytes());
    *hasher.finalize().as_bytes()
}

/// The gateway author string for an inbound article's `From` header:
/// `{name}@usenet`, preferring the display-name half of a `Name <addr>` form.
/// Any `@` in the name is replaced so the result carries exactly one origin
/// separator and never ends in `@{origin}` (loop-breaking, like `@fidonet`).
fn gateway_author(from: Option<&str>) -> String {
    let raw = from.unwrap_or("").trim();
    let name = raw.split('<').next().unwrap_or("").trim();
    let name = if name.is_empty() { raw } else { name };
    let name = if name.is_empty() { "unknown" } else { name };
    format!("{}@usenet", name.replace('@', "."))
}

/// Whether this message-id has already been processed: either recorded in the
/// shared dedupe window, or a native id that resolves to a stored post.
async fn already_have(shared: &Shared, mid: &MessageId) -> bool {
    if shared
        .dedup
        .seen(&SeenKey::MessageId(mid.as_str().to_string()))
    {
        return true;
    }
    if let Some(id) = event_id_from_message_id(mid) {
        if let Ok(Some(_)) = shared.boards.post_by_id(&id).await {
            return true;
        }
    }
    false
}

/// Record a settled offer (and the article's own `Message-ID` header when it
/// differs) so later re-offers are refused.
fn record_seen(shared: &Shared, offered: &MessageId, article: Option<&ParsedArticle>) {
    let now = chrono::Utc::now().timestamp_millis();
    shared
        .dedup
        .check_and_record(SeenKey::MessageId(offered.as_str().to_string()), now);
    if let Some(header_mid) = article
        .and_then(|a| a.header("message-id"))
        .and_then(|v| MessageId::new(v.trim()).ok())
    {
        if header_mid.as_str() != offered.as_str() {
            shared
                .dedup
                .check_and_record(SeenKey::MessageId(header_mid.as_str().to_string()), now);
        }
    }
}

/// Validate an inbound article and post it to its board. `Ok(true)` = posted;
/// `Ok(false)` = rejected (unknown/non-postable group, or the store refused).
async fn ingest_article(shared: &Shared, article: &ParsedArticle) -> Result<bool> {
    let Some(board_slug) = article.newsgroup() else {
        return Ok(false); // no Newsgroups header: malformed for our purposes
    };
    match shared.boards.board(&board_slug).await {
        Ok(Some(b)) if b.kind == 2 => {}
        _ => return Ok(false), // only postable boards accept articles
    }

    // Immediate parent: the last References entry that resolves to a post.
    let mut parent = None;
    for r in article.references().iter().rev() {
        if let Ok(mid) = MessageId::new(r.as_str()) {
            if let Some(id) = event_id_from_message_id(&mid) {
                if let Ok(Some(p)) = shared.boards.post_by_id(&id).await {
                    parent = Some(p.event_id);
                    break;
                }
            }
        }
    }

    let author = gateway_author(article.header("from"));
    let seed = feed_author_seed(shared, &author);
    let subject = article
        .subject()
        .unwrap_or_else(|| "(no subject)".to_string());
    let body = article.body();
    let now = chrono::Utc::now().timestamp_millis();

    match shared
        .boards
        .post(
            &board_slug,
            parent,
            &author,
            &seed,
            &subject,
            &body,
            "text/plain",
            now,
        )
        .await
    {
        Ok(row) => {
            shared.bus.publish(ServerEvent::BoardPost {
                board: row.board_slug.clone(),
                id: row.event_id,
                root: row.root_id,
            });
            Ok(true)
        }
        Err(_) => Ok(false),
    }
}

/// Read a dot-terminated article data block, undoing dot-stuffing. Returns
/// `None` if the peer hung up mid-article.
async fn read_article<R>(reader: &mut BufReader<R>) -> Result<Option<Vec<String>>>
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut lines: Vec<String> = Vec::new();
    let mut buf = String::new();
    loop {
        buf.clear();
        if reader.read_line(&mut buf).await? == 0 {
            return Ok(None); // connection dropped mid-article
        }
        let content = buf.trim_end_matches(['\r', '\n']);
        if content == "." {
            return Ok(Some(lines));
        }
        lines.push(content.strip_prefix('.').unwrap_or(content).to_string());
    }
}

/// Convert a validated `NEWNEWS` date-time spec to Unix epoch milliseconds.
///
/// Both the `GMT` and server-local forms are interpreted as UTC — the burrow
/// keeps all timestamps in UTC, so "server-local" *is* UTC here. A leap second
/// (`:60`) is clamped to `:59`.
fn spec_to_millis(spec: &DateTimeSpec) -> Option<i64> {
    let date =
        chrono::NaiveDate::from_ymd_opt(spec.year, u32::from(spec.month), u32::from(spec.day))?;
    let time = chrono::NaiveTime::from_hms_opt(
        u32::from(spec.hour),
        u32::from(spec.minute),
        u32::from(spec.second.min(59)),
    )?;
    Some(date.and_time(time).and_utc().timestamp_millis())
}

/// The message-ids of posts created at or after `since_ms` in boards matching
/// `pattern`, sorted for a deterministic block.
async fn new_ids_since(shared: &Shared, pattern: &str, since_ms: i64) -> Result<Vec<MessageId>> {
    let origin = shared.origin_name();
    let boards = shared.boards.boards().await.map_err(anyhow::Error::msg)?;
    let mut ids = Vec::new();
    for b in boards {
        if b.kind != 2 || !wildmat::matches(pattern, &b.slug) {
            continue;
        }
        for post in group_articles(shared, &b.slug).await? {
            if post.created_at >= since_ms {
                ids.push(message_id_for(&post.event_id, &origin));
            }
        }
    }
    ids.sort_by(|a, b| a.as_str().cmp(b.as_str()));
    ids.dedup_by(|a, b| a.as_str() == b.as_str());
    Ok(ids)
}

/// The command loop for one peer connection, generic over the byte stream —
/// a plain [`tokio::net::TcpStream`] or a server-side TLS stream. `peer_ip`
/// keys the per-IP auth/legacy rate buckets (`None` = unlimited). Writes go
/// through the read buffer's inner stream (`reader.get_mut()`): the protocol
/// is lockstep, so nothing is ever buffered on the write side.
async fn session_loop<S>(
    stream: S,
    shared: Arc<Shared>,
    peer_ip: Option<std::net::IpAddr>,
    security: SessionSecurity,
) -> Result<SessionEnd<S>>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut reader = BufReader::new(stream);

    let mut session = FeedSession {
        authed: None,
        pending_user: None,
    };

    // Greeting. Posting (transit) is the whole point of this surface; the
    // peer still authenticates before any transit verb is honoured. A
    // STARTTLS upgrade resumes without a greeting (RFC 4642 §2.2.2).
    if security.greets() {
        reader
            .get_mut()
            .write_all(Status::PostingAllowed.response().render().as_bytes())
            .await?;
    }

    let mut line = String::new();
    loop {
        // Push any buffered TLS records out before blocking on the next
        // command (a no-op for plain TCP; every `continue` passes through
        // here, so no response can be left stranded in the record buffer).
        reader.get_mut().flush().await?;
        line.clear();
        if reader.read_line(&mut line).await? == 0 {
            break; // peer hung up
        }
        // Coarse per-IP legacy command budget: refuse the command (400
        // service unavailable), keep the session.
        if let Some(ip) = peer_ip {
            if !shared.rate_allow(Scope::Ip(ip), rl::LEGACY) {
                reader
                    .get_mut()
                    .write_all(Status::ServiceUnavailable.response().render().as_bytes())
                    .await?;
                continue;
            }
        }
        let cmd = match Command::parse(&line) {
            Ok(c) => c,
            Err(_) => {
                reader
                    .get_mut()
                    .write_all(Status::SyntaxError.response().render().as_bytes())
                    .await?;
                continue;
            }
        };

        // RFC 4643 §2.3.1: AUTHINFO carries credentials, so it is refused on
        // an unsecured connection while the gate is up (483).
        if cmd.requires_secure_transport()
            && !security.secure()
            && shared.config.read().nntp_auth_require_tls
        {
            reader
                .get_mut()
                .write_all(Status::EncryptionRequired.response().render().as_bytes())
                .await?;
            continue;
        }

        match cmd {
            Command::Quit => {
                reader
                    .get_mut()
                    .write_all(Status::ConnectionClosing.response().render().as_bytes())
                    .await?;
                break;
            }

            Command::StartTls => {
                if security.secure() {
                    // RFC 4642 §2.1: once TLS is active, STARTTLS is refused.
                    reader
                        .get_mut()
                        .write_all(Status::CommandUnavailable.response().render().as_bytes())
                        .await?;
                } else {
                    reader
                        .get_mut()
                        .write_all(Status::ContinueTls.response().render().as_bytes())
                        .await?;
                    reader.get_mut().flush().await?;
                    // Hand the raw stream back for the handshake; the read
                    // buffer (any pipelined plaintext) and peer auth state
                    // are discarded (RFC 4642 §2.2.1).
                    return Ok(SessionEnd::StartTls(reader.into_inner()));
                }
            }

            Command::Capabilities => {
                reader
                    .get_mut()
                    .write_all(Status::CapabilitiesFollow.response().render().as_bytes())
                    .await?;
                let mut caps = vec!["VERSION 2", "IHAVE", "STREAMING", "MODE STREAM", "NEWNEWS"];
                // STARTTLS is only offered before TLS is up (RFC 4642 §2.2.2);
                // AUTHINFO only where it would be honoured (RFC 4643 §2.2).
                if !security.secure() {
                    caps.push("STARTTLS");
                }
                if security.secure() || !shared.config.read().nntp_auth_require_tls {
                    caps.push("AUTHINFO USER");
                }
                reader
                    .get_mut()
                    .write_all(datablock::encode_lines(&caps).as_bytes())
                    .await?;
            }

            Command::Date => {
                let now = chrono::Utc::now().format("%Y%m%d%H%M%S");
                reader
                    .get_mut()
                    .write_all(
                        Response::with_text(Status::DateFollows, now.to_string())
                            .render()
                            .as_bytes(),
                    )
                    .await?;
            }

            Command::AuthInfoUser(user) => {
                session.pending_user = Some(user);
                reader
                    .get_mut()
                    .write_all(Status::MoreAuthRequired.response().render().as_bytes())
                    .await?;
            }

            Command::AuthInfoPass(pass) => {
                // Failed attempts drain the per-IP auth budget; an empty
                // bucket refuses the attempt outright and closes.
                if let Some(ip) = peer_ip {
                    if !shared.rate_probe(Scope::Ip(ip), rl::AUTH) {
                        reader
                            .get_mut()
                            .write_all(Status::AuthRejected.response().render().as_bytes())
                            .await?;
                        break;
                    }
                }
                let status = match session.pending_user.take() {
                    None => Status::AuthSequenceError,
                    Some(user) => {
                        // An empty allowlist refuses everyone (fail safe).
                        let peers = shared.config.read().nntp_feed_peers;
                        if peers.get(&user).is_some_and(|want| *want == pass) {
                            session.authed = Some(user);
                            Status::AuthAccepted
                        } else {
                            Status::AuthRejected
                        }
                    }
                };
                reader
                    .get_mut()
                    .write_all(status.response().render().as_bytes())
                    .await?;
                if status == Status::AuthRejected {
                    if let Some(ip) = peer_ip {
                        if !shared.rate_allow(Scope::Ip(ip), rl::AUTH) {
                            break; // budget exhausted: close the connection
                        }
                    }
                }
            }

            Command::ModeStream => {
                let status = if session.authed.is_some() {
                    Status::StreamingPermitted
                } else {
                    Status::AuthRequired
                };
                reader
                    .get_mut()
                    .write_all(status.response().render().as_bytes())
                    .await?;
            }

            Command::IHave(mid) => {
                if session.authed.is_none() {
                    reader
                        .get_mut()
                        .write_all(Status::AuthRequired.response().render().as_bytes())
                        .await?;
                    continue;
                }
                let mut ex = Exchange::open(OfferVerb::IHave, mid);
                if already_have(&shared, ex.message_id()).await {
                    let r = ex.refuse().expect("offered -> refused"); // 435
                    reader.get_mut().write_all(r.render().as_bytes()).await?;
                    continue;
                }
                let r = ex.want().expect("offered -> wanted"); // 335
                reader.get_mut().write_all(r.render().as_bytes()).await?;
                // The peer waits for the 335 before sending the article —
                // push it past any TLS record buffering before blocking.
                reader.get_mut().flush().await?;
                let Some(lines) = read_article(&mut reader).await? else {
                    break; // peer dropped mid-article
                };
                let article = ParsedArticle::from_lines(&lines);
                let r = if ingest_article(&shared, &article).await? {
                    ex.accept().expect("wanted -> transferred") // 235
                } else {
                    ex.reject().expect("wanted -> rejected") // 437
                };
                record_seen(&shared, ex.message_id(), Some(&article));
                reader.get_mut().write_all(r.render().as_bytes()).await?;
            }

            Command::Check(mid) => {
                if session.authed.is_none() {
                    reader
                        .get_mut()
                        .write_all(Status::AuthRequired.response().render().as_bytes())
                        .await?;
                    continue;
                }
                let mut ex = Exchange::open(OfferVerb::Check, mid);
                let r = if already_have(&shared, ex.message_id()).await {
                    ex.refuse().expect("offered -> refused") // 438 <mid>
                } else {
                    ex.want().expect("offered -> wanted") // 238 <mid>
                };
                reader.get_mut().write_all(r.render().as_bytes()).await?;
            }

            Command::TakeThis(mid) => {
                // TAKETHIS carries its article unconditionally: consume the
                // data block *before* deciding anything, or its lines would be
                // misread as commands.
                let Some(lines) = read_article(&mut reader).await? else {
                    break; // peer dropped mid-article
                };
                if session.authed.is_none() {
                    reader
                        .get_mut()
                        .write_all(Status::AuthRequired.response().render().as_bytes())
                        .await?;
                    continue;
                }
                let mut ex = Exchange::open(OfferVerb::TakeThis, mid);
                let article = ParsedArticle::from_lines(&lines);
                let dupe = already_have(&shared, ex.message_id()).await;
                let r = if !dupe && ingest_article(&shared, &article).await? {
                    ex.accept().expect("wanted -> transferred") // 239 <mid>
                } else {
                    ex.reject().expect("wanted -> rejected") // 439 <mid>
                };
                record_seen(&shared, ex.message_id(), Some(&article));
                reader.get_mut().write_all(r.render().as_bytes()).await?;
            }

            Command::NewNews {
                wildmat: pattern,
                date,
                time,
                gmt,
            } => {
                if session.authed.is_none() {
                    reader
                        .get_mut()
                        .write_all(Status::AuthRequired.response().render().as_bytes())
                        .await?;
                    continue;
                }
                let reference_year = chrono::Utc::now().year();
                let since = datetime::parse(&date, &time, gmt, reference_year)
                    .ok()
                    .and_then(|spec| spec_to_millis(&spec));
                let Some(since_ms) = since else {
                    reader
                        .get_mut()
                        .write_all(Status::SyntaxError.response().render().as_bytes())
                        .await?;
                    continue;
                };
                let ids = new_ids_since(&shared, &pattern, since_ms).await?;
                reader
                    .get_mut()
                    .write_all(Status::NewArticlesFollow.response().render().as_bytes())
                    .await?;
                reader
                    .get_mut()
                    .write_all(new_articles_block(&ids).as_bytes())
                    .await?;
            }

            // Reader verbs belong to the reader surface (`crate::nntp`); this
            // listener is transit-only.
            Command::ModeReader
            | Command::Group(_)
            | Command::ListGroup { .. }
            | Command::Article(_)
            | Command::Head(_)
            | Command::Body(_)
            | Command::Stat(_)
            | Command::Next
            | Command::Last
            | Command::List(_)
            | Command::Over(_)
            | Command::Xover(_)
            | Command::NewGroups { .. }
            | Command::Post => {
                reader
                    .get_mut()
                    .write_all(Status::CommandUnavailable.response().render().as_bytes())
                    .await?;
            }

            Command::Unknown(_) => {
                reader
                    .get_mut()
                    .write_all(Status::UnknownCommand.response().render().as_bytes())
                    .await?;
            }
        }
    }

    // Deliver the final response (205, or a budget-exhausted 481) before the
    // stream drops — TLS may still hold it in the record buffer.
    reader.get_mut().flush().await?;
    Ok(SessionEnd::Closed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gateway_author_shapes() {
        assert_eq!(gateway_author(Some("alice")), "alice@usenet");
        assert_eq!(
            gateway_author(Some("Alice Doe <alice@example.org>")),
            "Alice Doe@usenet"
        );
        // An embedded @ is folded so exactly one origin separator remains.
        assert_eq!(
            gateway_author(Some("alice@example.org")),
            "alice.example.org@usenet"
        );
        assert_eq!(gateway_author(None), "unknown@usenet");
        assert_eq!(gateway_author(Some("   ")), "unknown@usenet");
    }

    #[test]
    fn spec_to_millis_epoch_and_clamped_leap_second() {
        let spec = datetime::parse("19700101", "000000", true, 2026).unwrap();
        assert_eq!(spec_to_millis(&spec), Some(0));
        // :60 clamps to :59 rather than failing.
        let leap = datetime::parse("20240229", "235960", true, 2024).unwrap();
        let plain = datetime::parse("20240229", "235959", true, 2024).unwrap();
        assert_eq!(spec_to_millis(&leap), spec_to_millis(&plain));
    }
}
