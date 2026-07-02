//! Legacy-protocol NNTP listener (Wave 10.2): a reader + poster news gateway
//! that projects the burrow's message boards as Usenet newsgroups, adapted onto
//! the same accounts/personas/permissions the native server uses.
//!
//! The `rabbithole-legacy-nntp` crate is a transport-only codec (command
//! parsing, status responses, dot-stuffed data blocks, overview records); here
//! we bridge it to [`Shared`] — the [`BoardService`](rabbithole_server_core::BoardService)
//! for reads/posts and the [`AuthService`](rabbithole_server_core::AuthService)
//! for `AUTHINFO`. It is opt-in via config (`nntp_enabled`) and off by default.
//!
//! # Group ↔ board mapping
//!
//! Board slugs are already dot-separated tokens (e.g. `rabbit.general`), which
//! is exactly the newsgroup name grammar, so the mapping is the **identity**:
//! newsgroup `rabbit.general` is the board with slug `rabbit.general`. Only
//! *postable* boards (`kind == 2`) are exposed as groups — categories and
//! bundles hold no articles.
//!
//! # Article numbering
//!
//! NNTP requires a per-group monotonic article number. We derive it from the
//! post ordering: every post in a board (thread roots plus their replies) is
//! sorted by `(created_at, event_id)` and numbered `1..=N`. The numbering is
//! stable for a fixed set of posts; retention drops shift it, which is
//! acceptable for a read gateway.
//!
//! # Message-IDs
//!
//! Each post has a globally unique blake3 event id. We render it as the stable
//! Message-ID `<{hex}@{origin}>` where `origin` is the server origin name, and
//! recover the event id by hex-decoding the local part — so `ARTICLE
//! <hex@origin>` round-trips to a post regardless of the selected group.

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::Result;
use rabbithole_legacy_nntp::{
    datablock, overview::overview_fmt_block, ArticleRef, Command, MessageId, OverRef, Overview,
    Range, Response, Status,
};
use rabbithole_server_core::{AuthedUser, Caps, Role, ServerEvent, Subject};
use rabbithole_store_server::repo4::PostRow;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

use crate::Shared;

/// Bind + serve the NNTP surface. Returns the bound address (useful when the
/// config asked for port 0) and the accept-loop task handle. Mirrors the
/// telnet/finger spawn helpers in [`crate::legacy`].
pub async fn spawn_nntp(
    shared: Arc<Shared>,
    addr: SocketAddr,
) -> Result<(SocketAddr, JoinHandle<()>)> {
    let listener = TcpListener::bind(addr).await?;
    let local = listener.local_addr()?;
    let handle = tokio::spawn(async move {
        loop {
            let Ok((sock, _peer)) = listener.accept().await else {
                break;
            };
            let shared = shared.clone();
            tokio::spawn(async move {
                if let Err(e) = serve(sock, shared).await {
                    tracing::debug!("nntp session error: {e}");
                }
            });
        }
    });
    Ok((local, handle))
}

/// Per-connection state.
struct NntpSession {
    shared: Arc<Shared>,
    /// The authenticated persona once `AUTHINFO USER`/`PASS` succeeds.
    authed: Option<AuthedUser>,
    /// Username captured by `AUTHINFO USER`, awaiting `AUTHINFO PASS`.
    pending_user: Option<String>,
    /// Currently selected group (board slug), if any.
    group: Option<String>,
    /// Current article number within the selected group (RFC 3977 pointer).
    article: Option<u64>,
}

impl NntpSession {
    /// The subject used for permission checks: the authed user's, or a guest
    /// baseline (guests hold `BOARD_READ` by role default; an operator ACL on
    /// the `board` resource can still deny it).
    fn subject(&self) -> Subject {
        match &self.authed {
            Some(u) => u.subject,
            None => Subject {
                account_id: -1,
                role: Role::Guest,
                class_id: None,
                class_mask: 0,
                grant_mask: 0,
                revoke_mask: 0,
            },
        }
    }

    fn can_read(&self) -> bool {
        self.shared
            .perms
            .allows(&self.subject(), "board", Caps::BOARD_READ)
    }

    fn can_post(&self) -> bool {
        self.authed.is_some()
            && self
                .shared
                .perms
                .allows(&self.subject(), "board", Caps::BOARD_POST)
    }
}

/// A stable per-account author signing seed — identical derivation to the
/// native board handler so an NNTP-authored post is indistinguishable from a
/// natively-authored one.
fn author_seed(shared: &Shared, account_id: i64) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"rabbithole-author-seed-v1");
    hasher.update(&shared.server_signing_seed);
    hasher.update(&account_id.to_le_bytes());
    *hasher.finalize().as_bytes()
}

/// The stable Message-ID for a post: `<{hex(event_id)}@{origin}>`.
fn message_id_for(event_id: &[u8; 32], origin: &str) -> MessageId {
    // hex of 32 bytes + a sane origin is always well within the 250-octet
    // limit and printable US-ASCII, so construction cannot fail.
    MessageId::new(format!("<{}@{origin}>", hex::encode(event_id))).expect("valid message-id")
}

/// Recover a post's event id from a Message-ID by hex-decoding the local part.
fn event_id_from_message_id(mid: &MessageId) -> Option<[u8; 32]> {
    let inner = mid.as_str().trim_start_matches('<').trim_end_matches('>');
    let local = inner.split('@').next()?;
    let bytes = hex::decode(local).ok()?;
    bytes.try_into().ok()
}

/// Every post in a board, numbered `1..=N` by `(created_at, event_id)`.
async fn group_articles(shared: &Shared, slug: &str) -> Result<Vec<PostRow>> {
    let roots = shared
        .boards
        .threads(slug, 100_000)
        .await
        .map_err(anyhow::Error::msg)?;
    let mut all = Vec::new();
    for (root, _replies, _last) in roots {
        let posts = shared
            .boards
            .thread(&root.event_id, 100_000)
            .await
            .map_err(anyhow::Error::msg)?;
        all.extend(posts);
    }
    all.sort_by(|a, b| {
        a.created_at
            .cmp(&b.created_at)
            .then_with(|| a.event_id.cmp(&b.event_id))
    });
    all.dedup_by(|a, b| a.event_id == b.event_id);
    Ok(all)
}

/// The RFC 5322 header lines for a post, rendered as a netnews article head.
fn header_lines(post: &PostRow, group: &str, origin: &str) -> Vec<String> {
    let date = chrono::DateTime::from_timestamp_millis(post.created_at)
        .unwrap_or_default()
        .to_rfc2822();
    let subject = if post.subject.trim().is_empty() {
        "(no subject)".to_string()
    } else {
        post.subject.clone()
    };
    let mut lines = vec![
        format!("From: {}", post.author),
        format!("Newsgroups: {group}"),
        format!("Subject: {subject}"),
        format!("Date: {date}"),
        format!("Message-ID: {}", message_id_for(&post.event_id, origin)),
    ];
    // References: thread root first, then the immediate parent (RFC 5536
    // orders ancestors oldest-first), skipping self and duplicates.
    let mut refs: Vec<String> = Vec::new();
    if let Some(root) = post.root_id {
        if root != post.event_id {
            refs.push(message_id_for(&root, origin).into_inner());
        }
    }
    if let Some(parent) = post.parent_id {
        if parent != post.event_id && post.root_id != Some(parent) {
            refs.push(message_id_for(&parent, origin).into_inner());
        }
    }
    if !refs.is_empty() {
        lines.push(format!("References: {}", refs.join(" ")));
    }
    lines.push(format!(
        "Content-Type: {}; charset=utf-8",
        if post.mime.is_empty() {
            "text/plain"
        } else {
            &post.mime
        }
    ));
    lines
}

fn body_lines(post: &PostRow) -> Vec<String> {
    post.body.lines().map(str::to_string).collect()
}

/// The `OVER`/`XOVER` overview record for a numbered post.
fn overview_for(post: &PostRow, number: u64, origin: &str) -> Overview {
    let date = chrono::DateTime::from_timestamp_millis(post.created_at)
        .unwrap_or_default()
        .to_rfc2822();
    let mut references = Vec::new();
    if let Some(root) = post.root_id {
        if root != post.event_id {
            references.push(message_id_for(&root, origin));
        }
    }
    if let Some(parent) = post.parent_id {
        if parent != post.event_id && post.root_id != Some(parent) {
            references.push(message_id_for(&parent, origin));
        }
    }
    Overview {
        number,
        subject: if post.subject.trim().is_empty() {
            "(no subject)".to_string()
        } else {
            post.subject.clone()
        },
        from: post.author.clone(),
        date,
        message_id: message_id_for(&post.event_id, origin),
        references,
        bytes: post.body.len() as u64,
        lines: post.body.lines().count() as u64,
    }
}

/// Resolve the numeric bounds of an `OVER`/`LISTGROUP` range against `count`.
fn range_bounds(range: Range, count: u64) -> (u64, u64) {
    let low = range.low.max(1);
    let high = range.high.unwrap_or(count).min(count);
    (low, high)
}

/// The accept-loop handler for one client.
async fn serve(mut sock: tokio::net::TcpStream, shared: Arc<Shared>) -> Result<()> {
    let (read_half, mut write) = sock.split();
    let mut reader = BufReader::new(read_half);
    let origin = shared.origin_name();

    let mut session = NntpSession {
        shared: shared.clone(),
        authed: None,
        pending_user: None,
        group: None,
        article: None,
    };

    // Greeting: posting is offered (the client authenticates before POST).
    write
        .write_all(Status::PostingAllowed.response().render().as_bytes())
        .await?;

    let mut line = String::new();
    loop {
        line.clear();
        if reader.read_line(&mut line).await? == 0 {
            break; // client hung up
        }
        let cmd = match Command::parse(&line) {
            Ok(c) => c,
            Err(_) => {
                write
                    .write_all(Status::SyntaxError.response().render().as_bytes())
                    .await?;
                continue;
            }
        };

        match cmd {
            Command::Quit => {
                write
                    .write_all(Status::ConnectionClosing.response().render().as_bytes())
                    .await?;
                break;
            }

            Command::Capabilities => {
                write
                    .write_all(Status::CapabilitiesFollow.response().render().as_bytes())
                    .await?;
                let caps = [
                    "VERSION 2",
                    "READER",
                    "POST",
                    "LIST ACTIVE NEWSGROUPS OVERVIEW.FMT",
                    "OVER",
                    "XOVER",
                    "AUTHINFO USER",
                ];
                write
                    .write_all(datablock::encode_lines(&caps).as_bytes())
                    .await?;
            }

            Command::ModeReader => {
                write
                    .write_all(Status::PostingAllowed.response().render().as_bytes())
                    .await?;
            }

            Command::Date => {
                let now = chrono::Utc::now().format("%Y%m%d%H%M%S");
                write
                    .write_all(
                        Response::with_text(Status::DateFollows, now.to_string())
                            .render()
                            .as_bytes(),
                    )
                    .await?;
            }

            Command::AuthInfoUser(user) => {
                session.pending_user = Some(user);
                write
                    .write_all(Status::MoreAuthRequired.response().render().as_bytes())
                    .await?;
            }

            Command::AuthInfoPass(pass) => {
                let status = match session.pending_user.take() {
                    None => Status::AuthSequenceError,
                    Some(user) => match shared.auth.login_password(&user, &pass, None).await {
                        Ok(u) => {
                            session.authed = Some(u);
                            Status::AuthAccepted
                        }
                        Err(_) => Status::AuthRejected,
                    },
                };
                write
                    .write_all(status.response().render().as_bytes())
                    .await?;
            }

            Command::List(keyword) => {
                handle_list(&mut write, &session, keyword).await?;
            }

            Command::Group(name) => {
                handle_group(&mut write, &mut session, &name).await?;
            }

            Command::ListGroup { group, range } => {
                handle_listgroup(&mut write, &mut session, group, range).await?;
            }

            Command::Article(r) => {
                handle_article(&mut write, &mut session, r, &origin, ArticlePart::All).await?;
            }
            Command::Head(r) => {
                handle_article(&mut write, &mut session, r, &origin, ArticlePart::Head).await?;
            }
            Command::Body(r) => {
                handle_article(&mut write, &mut session, r, &origin, ArticlePart::Body).await?;
            }
            Command::Stat(r) => {
                handle_article(&mut write, &mut session, r, &origin, ArticlePart::Stat).await?;
            }

            Command::Over(r) | Command::Xover(r) => {
                handle_over(&mut write, &mut session, r, &origin).await?;
            }

            Command::Next => {
                handle_step(&mut write, &mut session, &origin, true).await?;
            }
            Command::Last => {
                handle_step(&mut write, &mut session, &origin, false).await?;
            }

            Command::Post => {
                handle_post(&mut reader, &mut write, &mut session, &origin).await?;
            }

            // Recognised but unsupported here (not advertised in CAPABILITIES).
            Command::NewNews { .. } | Command::NewGroups { .. } => {
                write
                    .write_all(Status::FeatureNotSupported.response().render().as_bytes())
                    .await?;
            }

            // Transit/peering verbs (MODE STREAM, IHAVE, CHECK, TAKETHIS) are a
            // peer-feed concern — this listener is a reader server, so they are
            // refused rather than half-implemented. The peering service slice
            // will run these on its own session type.
            Command::ModeStream | Command::IHave(_) | Command::Check(_) | Command::TakeThis(_) => {
                write
                    .write_all(Status::CommandUnavailable.response().render().as_bytes())
                    .await?;
            }

            Command::Unknown(_) => {
                write
                    .write_all(Status::UnknownCommand.response().render().as_bytes())
                    .await?;
            }
        }
    }

    Ok(())
}

/// Which part(s) of an article a retrieval command wants.
#[derive(Clone, Copy)]
enum ArticlePart {
    All,
    Head,
    Body,
    Stat,
}

async fn handle_list<W: AsyncWriteExt + Unpin>(
    write: &mut W,
    session: &NntpSession,
    keyword: rabbithole_legacy_nntp::ListKeyword,
) -> Result<()> {
    use rabbithole_legacy_nntp::ListKeyword as K;
    match keyword {
        K::OverviewFmt => {
            write
                .write_all(Status::InformationFollows.response().render().as_bytes())
                .await?;
            write.write_all(overview_fmt_block().as_bytes()).await?;
        }
        K::Active(_) | K::Newsgroups(_) => {
            if !session.can_read() {
                write
                    .write_all(deny_read(session).response().render().as_bytes())
                    .await?;
                return Ok(());
            }
            let newsgroups = matches!(keyword, K::Newsgroups(_));
            let boards = session
                .shared
                .boards
                .boards()
                .await
                .map_err(anyhow::Error::msg)?;
            let mut lines = Vec::new();
            for b in boards {
                if b.kind != 2 {
                    continue; // only postable boards are groups
                }
                if newsgroups {
                    // `group <tab> description`
                    let desc = if b.description.is_empty() {
                        b.title.clone()
                    } else {
                        b.description.clone()
                    };
                    lines.push(format!("{}\t{desc}", b.slug));
                } else {
                    let arts = group_articles(&session.shared, &b.slug).await?;
                    let high = arts.len() as u64;
                    let low = if high > 0 { 1 } else { 0 };
                    // `group high low status` — "y" = posting permitted.
                    lines.push(format!("{} {high} {low} y", b.slug));
                }
            }
            write
                .write_all(Status::InformationFollows.response().render().as_bytes())
                .await?;
            write
                .write_all(datablock::encode_lines(&lines).as_bytes())
                .await?;
        }
        K::Other(_) => {
            write
                .write_all(Status::FeatureNotSupported.response().render().as_bytes())
                .await?;
        }
    }
    Ok(())
}

async fn handle_group<W: AsyncWriteExt + Unpin>(
    write: &mut W,
    session: &mut NntpSession,
    name: &str,
) -> Result<()> {
    if !session.can_read() {
        write
            .write_all(deny_read(session).response().render().as_bytes())
            .await?;
        return Ok(());
    }
    let board = session
        .shared
        .boards
        .board(name)
        .await
        .map_err(anyhow::Error::msg)?;
    match board {
        Some(b) if b.kind == 2 => {
            let arts = group_articles(&session.shared, name).await?;
            let count = arts.len() as u64;
            let (low, high) = if count > 0 { (1, count) } else { (0, 0) };
            session.group = Some(name.to_string());
            session.article = (count > 0).then_some(1);
            write
                .write_all(
                    Response::with_text(
                        Status::GroupSelected,
                        format!("{count} {low} {high} {name}"),
                    )
                    .render()
                    .as_bytes(),
                )
                .await?;
        }
        _ => {
            write
                .write_all(Status::NoSuchGroup.response().render().as_bytes())
                .await?;
        }
    }
    Ok(())
}

async fn handle_listgroup<W: AsyncWriteExt + Unpin>(
    write: &mut W,
    session: &mut NntpSession,
    group: Option<String>,
    range: Option<Range>,
) -> Result<()> {
    if !session.can_read() {
        write
            .write_all(deny_read(session).response().render().as_bytes())
            .await?;
        return Ok(());
    }
    let slug = match group.or_else(|| session.group.clone()) {
        Some(s) => s,
        None => {
            write
                .write_all(Status::NoGroupSelected.response().render().as_bytes())
                .await?;
            return Ok(());
        }
    };
    let board = session
        .shared
        .boards
        .board(&slug)
        .await
        .map_err(anyhow::Error::msg)?;
    if !matches!(board, Some(b) if b.kind == 2) {
        write
            .write_all(Status::NoSuchGroup.response().render().as_bytes())
            .await?;
        return Ok(());
    }
    let arts = group_articles(&session.shared, &slug).await?;
    let count = arts.len() as u64;
    let (low, high) = if count > 0 { (1, count) } else { (0, 0) };
    session.group = Some(slug.clone());
    session.article = (count > 0).then_some(1);

    let (rlow, rhigh) = match range {
        Some(r) => range_bounds(r, count),
        None => (low.max(1), count),
    };
    let mut lines = Vec::new();
    for n in rlow..=rhigh {
        if n >= 1 && n <= count {
            lines.push(n.to_string());
        }
    }
    write
        .write_all(
            Response::with_text(
                Status::GroupSelected,
                format!("{count} {low} {high} {slug}"),
            )
            .render()
            .as_bytes(),
        )
        .await?;
    write
        .write_all(datablock::encode_lines(&lines).as_bytes())
        .await?;
    Ok(())
}

/// Resolve an [`ArticleRef`] to `(number, post, group_name)`.
///
/// `number` is `0` when the article was addressed by Message-ID and is not in
/// the selected group (RFC 3977: "0" means "no article number available").
async fn resolve_article(
    session: &NntpSession,
    r: &ArticleRef,
) -> Result<Option<(u64, PostRow, String)>> {
    match r {
        ArticleRef::MessageId(mid) => {
            let Some(id) = event_id_from_message_id(mid) else {
                return Ok(None);
            };
            let Some(post) = session
                .shared
                .boards
                .post_by_id(&id)
                .await
                .map_err(anyhow::Error::msg)?
            else {
                return Ok(None);
            };
            // Prefer the article number if this post is in the current group.
            let mut number = 0;
            if session.group.as_deref() == Some(post.board_slug.as_str()) {
                let arts = group_articles(&session.shared, &post.board_slug).await?;
                if let Some(idx) = arts.iter().position(|p| p.event_id == id) {
                    number = idx as u64 + 1;
                }
            }
            let group = post.board_slug.clone();
            Ok(Some((number, post, group)))
        }
        ArticleRef::Number(n) => resolve_by_number(session, Some(*n)).await,
        ArticleRef::Current => resolve_by_number(session, session.article).await,
    }
}

async fn resolve_by_number(
    session: &NntpSession,
    number: Option<u64>,
) -> Result<Option<(u64, PostRow, String)>> {
    let Some(slug) = session.group.clone() else {
        return Ok(None);
    };
    let Some(n) = number else {
        return Ok(None);
    };
    let arts = group_articles(&session.shared, &slug).await?;
    if n >= 1 && (n as usize) <= arts.len() {
        let post = arts[(n - 1) as usize].clone();
        Ok(Some((n, post, slug)))
    } else {
        Ok(None)
    }
}

async fn handle_article<W: AsyncWriteExt + Unpin>(
    write: &mut W,
    session: &mut NntpSession,
    r: ArticleRef,
    origin: &str,
    part: ArticlePart,
) -> Result<()> {
    if !session.can_read() {
        write
            .write_all(deny_read(session).response().render().as_bytes())
            .await?;
        return Ok(());
    }
    // A bare selector with no current group/article gets the right diagnostic.
    let by_id = matches!(r, ArticleRef::MessageId(_));
    if matches!(r, ArticleRef::Current) && session.group.is_none() {
        write
            .write_all(Status::NoGroupSelected.response().render().as_bytes())
            .await?;
        return Ok(());
    }

    let resolved = resolve_article(session, &r).await?;
    let Some((number, post, group)) = resolved else {
        let status = if by_id {
            Status::NoArticleWithMessageId
        } else if session.group.is_none() {
            Status::NoGroupSelected
        } else {
            Status::NoArticleWithNumber
        };
        write
            .write_all(status.response().render().as_bytes())
            .await?;
        return Ok(());
    };

    // Selecting by number in the current group moves the pointer.
    if !by_id && number > 0 {
        session.article = Some(number);
    }

    let mid = message_id_for(&post.event_id, origin);
    let (status, block): (Status, Option<String>) = match part {
        ArticlePart::Stat => (Status::ArticleExists, None),
        ArticlePart::Head => {
            let lines = header_lines(&post, &group, origin);
            (Status::HeadFollows, Some(datablock::encode_lines(&lines)))
        }
        ArticlePart::Body => {
            let lines = body_lines(&post);
            (Status::BodyFollows, Some(datablock::encode_lines(&lines)))
        }
        ArticlePart::All => {
            let mut lines = header_lines(&post, &group, origin);
            lines.push(String::new());
            lines.extend(body_lines(&post));
            (
                Status::ArticleFollows,
                Some(datablock::encode_lines(&lines)),
            )
        }
    };
    write
        .write_all(
            Response::with_text(status, format!("{number} {mid}"))
                .render()
                .as_bytes(),
        )
        .await?;
    if let Some(block) = block {
        write.write_all(block.as_bytes()).await?;
    }
    Ok(())
}

async fn handle_over<W: AsyncWriteExt + Unpin>(
    write: &mut W,
    session: &mut NntpSession,
    r: OverRef,
    origin: &str,
) -> Result<()> {
    if !session.can_read() {
        write
            .write_all(deny_read(session).response().render().as_bytes())
            .await?;
        return Ok(());
    }

    // Message-ID form: a single overview record for that article.
    if let OverRef::MessageId(mid) = &r {
        let Some(id) = event_id_from_message_id(mid) else {
            write
                .write_all(
                    Status::NoArticleWithMessageId
                        .response()
                        .render()
                        .as_bytes(),
                )
                .await?;
            return Ok(());
        };
        let post = session
            .shared
            .boards
            .post_by_id(&id)
            .await
            .map_err(anyhow::Error::msg)?;
        match post {
            Some(post) => {
                write
                    .write_all(Status::OverviewFollows.response().render().as_bytes())
                    .await?;
                let ov = overview_for(&post, 0, origin);
                write
                    .write_all(datablock::encode_lines(&[ov.encode()]).as_bytes())
                    .await?;
            }
            None => {
                write
                    .write_all(
                        Status::NoArticleWithMessageId
                            .response()
                            .render()
                            .as_bytes(),
                    )
                    .await?;
            }
        }
        return Ok(());
    }

    let Some(slug) = session.group.clone() else {
        write
            .write_all(Status::NoGroupSelected.response().render().as_bytes())
            .await?;
        return Ok(());
    };
    let arts = group_articles(&session.shared, &slug).await?;
    let count = arts.len() as u64;
    let (low, high) = match r {
        OverRef::Range(range) => range_bounds(range, count),
        OverRef::Current => match session.article {
            Some(n) => (n, n),
            None => {
                write
                    .write_all(Status::CurrentArticleInvalid.response().render().as_bytes())
                    .await?;
                return Ok(());
            }
        },
        OverRef::MessageId(_) => unreachable!("handled above"),
    };
    if low > count || high == 0 || low > high {
        write
            .write_all(Status::NoArticleWithNumber.response().render().as_bytes())
            .await?;
        return Ok(());
    }
    let mut lines = Vec::new();
    for n in low..=high {
        if n >= 1 && (n as usize) <= arts.len() {
            lines.push(overview_for(&arts[(n - 1) as usize], n, origin).encode());
        }
    }
    write
        .write_all(Status::OverviewFollows.response().render().as_bytes())
        .await?;
    write
        .write_all(datablock::encode_lines(&lines).as_bytes())
        .await?;
    Ok(())
}

async fn handle_step<W: AsyncWriteExt + Unpin>(
    write: &mut W,
    session: &mut NntpSession,
    origin: &str,
    forward: bool,
) -> Result<()> {
    let Some(slug) = session.group.clone() else {
        write
            .write_all(Status::NoGroupSelected.response().render().as_bytes())
            .await?;
        return Ok(());
    };
    let Some(current) = session.article else {
        write
            .write_all(Status::CurrentArticleInvalid.response().render().as_bytes())
            .await?;
        return Ok(());
    };
    let arts = group_articles(&session.shared, &slug).await?;
    let count = arts.len() as u64;
    let next = if forward {
        current + 1
    } else {
        current.wrapping_sub(1)
    };
    if forward && next > count {
        write
            .write_all(Status::NoNextArticle.response().render().as_bytes())
            .await?;
        return Ok(());
    }
    if !forward && (current <= 1) {
        write
            .write_all(Status::NoPreviousArticle.response().render().as_bytes())
            .await?;
        return Ok(());
    }
    session.article = Some(next);
    let post = &arts[(next - 1) as usize];
    let mid = message_id_for(&post.event_id, origin);
    write
        .write_all(
            Response::with_text(Status::ArticleExists, format!("{next} {mid}"))
                .render()
                .as_bytes(),
        )
        .await?;
    Ok(())
}

async fn handle_post<R, W>(
    reader: &mut BufReader<R>,
    write: &mut W,
    session: &mut NntpSession,
    origin: &str,
) -> Result<()>
where
    R: tokio::io::AsyncRead + Unpin,
    W: AsyncWriteExt + Unpin,
{
    if session.authed.is_none() {
        write
            .write_all(Status::AuthRequired.response().render().as_bytes())
            .await?;
        return Ok(());
    }
    if !session.can_post() {
        write
            .write_all(Status::PostingNotPermitted.response().render().as_bytes())
            .await?;
        return Ok(());
    }
    write
        .write_all(Status::SendArticle.response().render().as_bytes())
        .await?;

    // Read the article until the "." terminator, undoing dot-stuffing.
    let mut lines: Vec<String> = Vec::new();
    let mut buf = String::new();
    loop {
        buf.clear();
        if reader.read_line(&mut buf).await? == 0 {
            // Connection dropped mid-article.
            return Ok(());
        }
        let content = buf.trim_end_matches(['\r', '\n']);
        if content == "." {
            break;
        }
        lines.push(content.strip_prefix('.').unwrap_or(content).to_string());
    }

    let article = ParsedArticle::from_lines(&lines);
    let Some(board_slug) = article.newsgroup() else {
        write
            .write_all(Status::PostingFailed.response().render().as_bytes())
            .await?;
        return Ok(());
    };

    // Only postable boards accept articles.
    match session.shared.boards.board(&board_slug).await {
        Ok(Some(b)) if b.kind == 2 => {}
        _ => {
            write
                .write_all(Status::PostingFailed.response().render().as_bytes())
                .await?;
            return Ok(());
        }
    }

    // Immediate parent: the last References entry that resolves to a post.
    let mut parent = None;
    for r in article.references().iter().rev() {
        if let Ok(mid) = MessageId::new(r.as_str()) {
            if let Some(id) = event_id_from_message_id(&mid) {
                if let Ok(Some(p)) = session.shared.boards.post_by_id(&id).await {
                    parent = Some(p.event_id);
                    break;
                }
            }
        }
    }

    let authed = session.authed.as_ref().expect("authed checked above");
    let seed = author_seed(&session.shared, authed.account.id);
    let author = format!("{}@{origin}", authed.persona.screen_name);
    let subject = article
        .subject()
        .unwrap_or_else(|| "(no subject)".to_string());
    let body = article.body();
    let now = chrono::Utc::now().timestamp_millis();

    match session
        .shared
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
            session.shared.bus.publish(ServerEvent::BoardPost {
                board: row.board_slug.clone(),
                id: row.event_id,
                root: row.root_id,
            });
            write
                .write_all(Status::PostingOk.response().render().as_bytes())
                .await?;
        }
        Err(_) => {
            write
                .write_all(Status::PostingFailed.response().render().as_bytes())
                .await?;
        }
    }
    Ok(())
}

/// A minimally-parsed inbound netnews article: headers until the first blank
/// line, everything after is the body.
struct ParsedArticle {
    headers: Vec<(String, String)>,
    body: Vec<String>,
}

impl ParsedArticle {
    fn from_lines(lines: &[String]) -> ParsedArticle {
        let mut headers = Vec::new();
        let mut idx = 0;
        while idx < lines.len() {
            let line = &lines[idx];
            idx += 1;
            if line.is_empty() {
                break; // end of headers
            }
            if let Some((name, value)) = line.split_once(':') {
                headers.push((name.trim().to_ascii_lowercase(), value.trim().to_string()));
            }
        }
        let body = lines[idx.min(lines.len())..].to_vec();
        ParsedArticle { headers, body }
    }

    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }

    fn subject(&self) -> Option<String> {
        self.header("subject")
            .map(str::to_string)
            .filter(|s| !s.trim().is_empty())
    }

    /// The first newsgroup named in the `Newsgroups` header (a board slug).
    fn newsgroup(&self) -> Option<String> {
        self.header("newsgroups")
            .and_then(|v| v.split(',').next())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }

    fn references(&self) -> Vec<String> {
        self.header("references")
            .map(|v| v.split_whitespace().map(str::to_string).collect())
            .unwrap_or_default()
    }

    fn body(&self) -> String {
        self.body.join("\n")
    }
}

/// The response for a denied read: prompt for auth if the client is a guest,
/// otherwise report the command as unavailable to it.
fn deny_read(session: &NntpSession) -> Status {
    if session.authed.is_some() {
        Status::CommandUnavailable
    } else {
        Status::AuthRequired
    }
}
