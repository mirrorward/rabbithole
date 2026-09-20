//! A burrow as a source of a fetch's units.
//!
//! The burrow a file is downloaded from, and any other burrow the person is
//! signed in to that holds the same content, answer for byte ranges with
//! their Bao proofs ([`ProvedRangeRequest`](rabbithole_proto::transfer::ProvedRangeRequest)),
//! which the fetch checks against the file's root like any peer's. Both the
//! app and the command line drive downloads this way, so the rules for
//! which burrows may be asked, how long each is waited for, and what may be
//! passed on afterwards live here rather than in either of them.
//!
//! Only built for the clients that have a session to ask over (the `client`
//! feature); a burrow serving ranges needs none of this.

use std::sync::Arc;

use rabbithole_core::{Client, ClientError};

use crate::peer::{BaoPiece, HaveMap, PeerError, RangeSource, STATUS_BUSY, STATUS_NOT_FOUND};

/// Whether a download may ask the person's other burrows at all: it does
/// only when they left the choice of sources to the app.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AskOthers {
    Yes,
    No,
}

/// How long a burrow may take over one proved range before it is let go as
/// a source for this download.
pub const ASK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);
/// How long a burrow's session is waited for when something else is using
/// it (another download, an advert): past that the unit goes to somebody
/// else and this burrow keeps its place.
pub const SESSION_WAIT: std::time::Duration = std::time::Duration::from_secs(2);
/// How soon a burrow still making a file's proofs is asked again: at first,
/// and at most.
const BUSY_RETRY: std::time::Duration = std::time::Duration::from_millis(250);
const BUSY_RETRY_MAX: std::time::Duration = std::time::Duration::from_secs(2);
/// The slowest a burrow makes a file's proofs, for how long the burrow a
/// file is downloaded from is waited for (a minute, and the file at that
/// rate). Another burrow, which is a bonus rather than the source, is given
/// [`HELPER_BUSY_FOR`] and then left to catch up on its own.
const ORIGIN_PROOF_RATE: u64 = 64 * 1024 * 1024;
const HELPER_BUSY_FOR: std::time::Duration = std::time::Duration::from_secs(10);
/// The most other burrows one download asks. Each is a session doing one
/// thing at a time, so a handful is plenty and a crowd is only noise.
pub const OTHER_BURROWS_MAX: usize = 4;

/// One burrow's native session, shared with whatever else the app is doing
/// on that burrow (a download takes it for the length of one ask).
pub type Session = Arc<tokio::sync::Mutex<Client>>;

/// Whether a burrow's software sends proved ranges (0.230 on). One that
/// does not is never asked, so a download it could not serve opens and
/// counts nothing there.
pub fn serves_proved_ranges(version: &str) -> bool {
    let mut parts = version.split('.').map(|p| p.parse::<u64>().ok());
    match (parts.next().flatten(), parts.next().flatten()) {
        (Some(major), Some(minor)) => (major, minor) >= (0, 230),
        _ => false,
    }
}

/// Whether a burrow's software can say which of its files holds a content
/// (0.232 on), which is what lets a download on one burrow take chunks from
/// another.
pub fn finds_by_content(version: &str) -> bool {
    let mut parts = version.split('.').map(|p| p.parse::<u64>().ok());
    match (parts.next().flatten(), parts.next().flatten()) {
        (Some(major), Some(minor)) => (major, minor) >= (0, 232),
        _ => false,
    }
}

/// A burrow as one of a fetch's sources: it holds the whole file and sends
/// each range with its proof (`ProvedRange`), which the fetch checks against
/// the root like any peer's. It takes units like any other source
/// (work-stealing), so it carries what the peers do not hold and whatever it
/// is quicker to.
///
/// Two kinds, told apart by [`BurrowSource::lent`]: **the burrow the file is
/// being downloaded from**, and **another burrow the person is signed in to**
/// that holds the same content and lets them download it there. What another
/// burrow sends is never offered on to this one's swarm: it was lent to this
/// person, not given to this burrow.
pub struct BurrowSource {
    label: String,
    session: Session,
    node_id: i64,
    /// Opened when the burrow is first asked (the burrow of the download),
    /// or when it answered for the content (another burrow), so a burrow
    /// that serves nothing opens and counts nothing.
    ticket: tokio::sync::Mutex<Option<rabbithole_proto::transfer::TransferTicket>>,
    /// How long it is waited for while it makes the file's proofs.
    busy_for: std::time::Duration,
    lent: bool,
}

impl BurrowSource {
    /// The burrow the download is from: asked for ranges of `node_id`, its
    /// ticket opened on the first ask.
    pub fn origin(label: String, session: Session, node_id: i64, size: u64) -> Self {
        BurrowSource {
            label,
            session,
            node_id,
            ticket: tokio::sync::Mutex::new(None),
            busy_for: std::time::Duration::from_secs(60 + size / ORIGIN_PROOF_RATE),
            lent: false,
        }
    }

    /// Another burrow, which has already said it holds the content and given
    /// a ticket for it.
    pub fn helper(
        label: String,
        session: Session,
        node_id: i64,
        ticket: rabbithole_proto::transfer::TransferTicket,
    ) -> Self {
        BurrowSource {
            label,
            session,
            node_id,
            ticket: tokio::sync::Mutex::new(Some(ticket)),
            busy_for: HELPER_BUSY_FOR,
            lent: true,
        }
    }

    /// Whether this is another burrow's, lending rather than giving.
    pub fn lent(&self) -> bool {
        self.lent
    }

    /// Take the ticket out without closing it, for a caller that will use
    /// it for something else (the download falling back to this burrow's
    /// own stream, which is one download, not two).
    pub async fn take_ticket(&self) -> Option<rabbithole_proto::transfer::TransferTicket> {
        self.ticket.lock().await.take()
    }

    /// Give back the ticket, if one was opened: a burrow does not hold a
    /// transfer slot for a download that has finished with it.
    pub async fn close(&self) {
        let ticket = { self.ticket.lock().await.take() };
        let Some(ticket) = ticket else { return };
        if let Ok(mut client) = tokio::time::timeout(SESSION_WAIT, self.session.lock()).await {
            let _ =
                tokio::time::timeout(ASK_TIMEOUT, client.close_transfer(ticket.transfer_id)).await;
        }
    }

    /// One ask. `Ok(None)`: not now — the burrow is making the file's
    /// proofs, or its session is busy with something else. `Err`: it is out,
    /// for this download.
    async fn ask(
        &self,
        at: u64,
        part: u32,
    ) -> Result<Option<rabbithole_proto::transfer::ProvedRange>, PeerError> {
        let gone = || PeerError::Refused(STATUS_NOT_FOUND);
        let answer = {
            let Ok(mut client) = tokio::time::timeout(SESSION_WAIT, self.session.lock()).await
            else {
                return Ok(None);
            };
            let mut held = self.ticket.lock().await;
            let id = match held.as_ref() {
                Some(ticket) => ticket.transfer_id,
                None => {
                    let opened =
                        tokio::time::timeout(ASK_TIMEOUT, client.download_ticket(self.node_id))
                            .await
                            .ok()
                            .and_then(Result::ok);
                    match opened {
                        Some(ticket) => {
                            let id = ticket.transfer_id;
                            *held = Some(ticket);
                            id
                        }
                        None => return Err(gone()),
                    }
                }
            };
            drop(held);
            tokio::time::timeout(ASK_TIMEOUT, client.proved_range(id, at, part)).await
        };
        match answer {
            Ok(Ok(range)) => Ok(Some(range)),
            // Still making the file's proofs: ask again shortly.
            Ok(Err(ClientError::Refused(rabbithole_proto::ErrorCode::Unavailable))) => Ok(None),
            // Anything else, and this burrow is done sending ranges for
            // this download. The ticket is left in the books rather than
            // closed: the download may yet fall back to this burrow's own
            // stream, and that is the same download, on the same ticket.
            // Whoever finishes with it gives it back ([`Self::close`]).
            _ => Err(gone()),
        }
    }
}

#[async_trait::async_trait]
impl RangeSource for BurrowSource {
    fn label(&self) -> String {
        self.label.clone()
    }

    async fn have(&self) -> Result<Option<HaveMap>, PeerError> {
        Ok(None)
    }

    fn shareable(&self) -> bool {
        !self.lent
    }

    async fn bao(&self, offset: u64, len: u64) -> Result<Vec<BaoPiece>, PeerError> {
        // Its messages are smaller than a unit: the range comes in parts.
        let step = rabbithole_proto::transfer::PROVED_RANGE_MAX as u64;
        let mut pieces = Vec::new();
        let mut at = offset;
        let deadline = tokio::time::Instant::now() + self.busy_for;
        while at < offset + len {
            let part = (offset + len - at).min(step);
            let mut pause = BUSY_RETRY;
            let range = loop {
                match self.ask(at, part as u32).await? {
                    Some(range) => break range,
                    None if tokio::time::Instant::now() + pause < deadline => {
                        tokio::time::sleep(pause).await;
                        pause = (pause * 2).min(BUSY_RETRY_MAX);
                    }
                    // Not now, and waited long enough: somebody else takes
                    // this unit and this burrow keeps its place, with
                    // nothing assumed about what it holds, so a slow burrow
                    // costs the download nothing but its turn.
                    None => return Err(PeerError::Refused(STATUS_BUSY)),
                }
            };
            pieces.push(BaoPiece {
                offset: at,
                len: part,
                size: range.size,
                stream: range.stream,
            });
            // The last part of the file is shorter than asked for.
            if at + part >= range.size {
                break;
            }
            at += part;
        }
        Ok(pieces)
    }
}

/// A burrow the app is signed in to, as a possible source for a download
/// running on another one.
pub struct BurrowLink {
    /// What the Transfers row calls it (its endpoint).
    pub label: String,
    pub session: Session,
    /// Its identity key, so the same burrow reached two ways is one burrow.
    pub server_key: [u8; 32],
    /// The software it reports, so one too old to ask is not asked.
    pub version: String,
}

/// Which of the app's other sessions a download may ask, by their place in
/// `sessions` (`(endpoint, server_key, version)`): never the burrow the
/// download is from, never the same burrow twice however it was reached,
/// never one too old to answer, only when the person left the choice to the
/// app, and never more than `max`. Pure, so the rule is tested rather than
/// hoped for.
pub fn other_burrows(
    mode: AskOthers,
    sessions: &[(String, [u8; 32], String)],
    origin_endpoint: &str,
    origin_key: [u8; 32],
    max: usize,
) -> Vec<usize> {
    if mode != AskOthers::Yes {
        return Vec::new();
    }
    let mut seen: Vec<[u8; 32]> = vec![origin_key];
    let mut picked = Vec::new();
    for (i, (endpoint, key, version)) in sessions.iter().enumerate() {
        if picked.len() >= max {
            break;
        }
        if endpoint == origin_endpoint || seen.contains(key) || !finds_by_content(version) {
            continue;
        }
        seen.push(*key);
        picked.push(i);
    }
    picked
}

/// Ask each of the app's other burrows whether it holds `root` and whether
/// this person may download it there, and take a ticket from the ones that
/// do. Only these join a download; a burrow that says nothing, says no, or
/// is too old to be asked is simply not a source, and costs the download
/// one bounded round trip.
pub async fn confirm_helpers(links: &[BurrowLink], root: [u8; 32]) -> Vec<Arc<BurrowSource>> {
    let mut helpers = Vec::new();
    for link in links {
        let Ok(mut client) = tokio::time::timeout(SESSION_WAIT, link.session.lock()).await else {
            continue;
        };
        let found = tokio::time::timeout(ASK_TIMEOUT, client.file_by_content(root))
            .await
            .ok()
            .and_then(Result::ok);
        let Some(found) = found else { continue };
        let ticket = tokio::time::timeout(ASK_TIMEOUT, client.download_ticket(found.node_id))
            .await
            .ok()
            .and_then(Result::ok);
        let Some(ticket) = ticket else { continue };
        drop(client);
        helpers.push(Arc::new(BurrowSource::helper(
            link.label.clone(),
            link.session.clone(),
            found.node_id,
            ticket,
        )));
    }
    helpers
}
