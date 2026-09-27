//! Optional live catalog exchange. The dialer probes only after the legacy
//! exchange; a listener never sends extension traffic to an old dialer.
use super::*;
use crate::fed_catalog::CatalogFetch;
use rabbithole_federation::SignedCatalog;
use std::time::Duration;
use tokio::time::Instant;

const MT_SUPPORT: u16 = 12;
const MT_ANNOUNCE: u16 = 13;
const MT_GET: u16 = 14;
const MT_REPLY: u16 = 15;
const CAPABILITY: &str = "catalog-sync-v1";
/// Content checks cover deletes, moves and policy changes, which need not
/// produce a file-added event. Each edge checks at most once per interval.
const CHECK_INTERVAL: Duration = Duration::from_secs(30);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_CONTROL: usize = 128;

pub(super) fn handles(message_type: u16) -> bool {
    (MT_SUPPORT..=MT_REPLY).contains(&message_type)
}

fn frame<T: Serialize>(kind: FrameKind, message_type: u16, id: RequestId, msg: &T) -> Frame {
    Frame {
        id,
        ..fed_frame(kind, message_type, msg)
    }
}

struct Pending {
    id: RequestId,
    deadline: Instant,
    announced: CatalogAnnounceMsg,
    fetch: CatalogFetch,
}

pub(super) struct CatalogSync {
    enabled: bool,
    probe: Option<(RequestId, Instant)>,
    next_id: u64,
    next_check: Option<Instant>,
    next_fetch: Option<Instant>,
    announced: Option<CatalogAnnounceMsg>,
    remote: Option<CatalogAnnounceMsg>,
    pending: Option<Pending>,
}

impl CatalogSync {
    pub(super) fn new() -> Self {
        Self {
            enabled: false,
            probe: None,
            next_id: 1,
            next_check: None,
            next_fetch: None,
            announced: None,
            remote: None,
            pending: None,
        }
    }

    fn request_id(&mut self) -> Result<RequestId> {
        let id = self.next_id;
        self.next_id = id
            .checked_add(1)
            .ok_or_else(|| anyhow!("catalog request IDs exhausted"))?;
        Ok(RequestId(id))
    }

    pub(super) async fn probe(&mut self, conn: &mut dyn Connection, now: Instant) -> Result<()> {
        let id = self.request_id()?;
        self.probe = Some((id, now + REQUEST_TIMEOUT));
        conn.send(frame(FrameKind::Request, MT_SUPPORT, id, &CAPABILITY))
            .await?;
        Ok(())
    }

    fn expire(&mut self, now: Instant) {
        if self.probe.is_some_and(|(_, deadline)| now >= deadline) {
            self.probe = None;
        }
        if self
            .pending
            .as_ref()
            .is_some_and(|pending| now >= pending.deadline)
        {
            self.pending = None;
        }
    }

    fn check_due(&mut self, now: Instant) -> bool {
        if !self.enabled || self.next_check.is_some_and(|deadline| now < deadline) {
            return false;
        }
        self.next_check = Some(now + CHECK_INTERVAL);
        true
    }

    pub(super) async fn tick(
        &mut self,
        conn: &mut dyn Connection,
        shared: &Arc<Shared>,
        key: &[u8; 32],
        origin: &str,
        now: Instant,
    ) -> Result<()> {
        self.expire(now);
        if self.check_due(now) {
            self.announce_local(conn, shared, key, origin).await?;
        }
        self.request_if_needed(conn, shared, key, now).await
    }

    async fn announce_local(
        &mut self,
        conn: &mut dyn Connection,
        shared: &Arc<Shared>,
        key: &[u8; 32],
        origin: &str,
    ) -> Result<()> {
        let mine = match crate::fed_catalog::local_catalog(shared).await {
            Ok(mine) => mine,
            Err(error) => {
                tracing::warn!(%error, "cannot build live catalog; retrying next interval");
                return Ok(());
            }
        };
        if !shared.peers.is_approved_origin(key, origin) {
            bail!("peer approval revoked during catalog build");
        }
        let announcement = CatalogAnnounceMsg {
            catalog_id: mine.catalog_id().map_err(|e| anyhow!("catalog id: {e}"))?,
            generation: mine.catalog.generation,
        };
        if self.announced != Some(announcement) {
            conn.send(frame(
                FrameKind::Push,
                MT_ANNOUNCE,
                RequestId::PUSH,
                &announcement,
            ))
            .await?;
            self.announced = Some(announcement);
        }
        Ok(())
    }

    async fn request_if_needed(
        &mut self,
        conn: &mut dyn Connection,
        shared: &Arc<Shared>,
        key: &[u8; 32],
        now: Instant,
    ) -> Result<()> {
        if !self.enabled
            || self.pending.is_some()
            || self.next_fetch.is_some_and(|deadline| now < deadline)
        {
            return Ok(());
        }
        let Some(announced) = self.remote else {
            return Ok(());
        };
        if !shared.catalogs.wants(key, announced.generation) {
            return Ok(());
        }
        // Capture approval before the send await, and keep it with exactly this
        // request. Revocation/reapproval cannot authorize its eventual reply.
        self.next_fetch = Some(now + CHECK_INTERVAL);
        let fetch = match shared.catalogs.begin_fetch(&shared.peers, *key) {
            Ok(fetch) => fetch,
            Err(error) => {
                tracing::warn!(%error, "cannot admit live catalog fetch; retrying next interval");
                return Ok(());
            }
        };
        let id = self.request_id()?;
        self.pending = Some(Pending {
            id,
            deadline: now + REQUEST_TIMEOUT,
            announced,
            fetch,
        });
        conn.send(frame(FrameKind::Request, MT_GET, id, &CatalogGetMsg {}))
            .await?;
        Ok(())
    }

    pub(super) async fn handle(
        &mut self,
        conn: &mut dyn Connection,
        shared: &Arc<Shared>,
        key: &[u8; 32],
        origin: &str,
        incoming: &Frame,
        now: Instant,
    ) -> Result<()> {
        if incoming.family != Family::FEDERATION || !shared.peers.is_approved_origin(key, origin) {
            bail!("unapproved or non-federation catalog frame");
        }
        self.expire(now);
        if incoming.message_type == MT_SUPPORT {
            // A support reply must match our still-live probe. An old endpoint
            // simply ignores the request; no retry or unsolicited announcement.
            match incoming.kind {
                FrameKind::Request if incoming.id != RequestId::PUSH => {
                    let capability: String = decode_fed_bounded(incoming, MT_SUPPORT, MAX_CONTROL)?;
                    if capability == CAPABILITY && incoming.error.is_none() {
                        conn.send(frame(
                            FrameKind::Reply,
                            MT_SUPPORT,
                            incoming.id,
                            &CAPABILITY,
                        ))
                        .await?;
                        self.enabled = true;
                    }
                }
                FrameKind::Reply if self.probe.is_some_and(|(id, _)| id == incoming.id) => {
                    if incoming.error.is_some() {
                        self.probe = None;
                        return Ok(());
                    }
                    let capability: String = decode_fed_bounded(incoming, MT_SUPPORT, MAX_CONTROL)?;
                    if capability == CAPABILITY && incoming.error.is_none() {
                        self.probe = None;
                        self.enabled = true;
                    }
                }
                _ => {}
            }
            return Ok(());
        }
        if !self.enabled {
            return Ok(());
        }
        match incoming.message_type {
            MT_ANNOUNCE => {
                if incoming.kind != FrameKind::Push
                    || incoming.id != RequestId::PUSH
                    || incoming.error.is_some()
                {
                    bail!("invalid live catalog announcement envelope");
                }
                let announcement: CatalogAnnounceMsg =
                    decode_fed_bounded(incoming, MT_ANNOUNCE, MAX_CONTROL)?;
                // Remember only the newest announcement, including while one
                // GET is in flight. One peer cannot grow an unbounded queue.
                if self
                    .remote
                    .is_none_or(|old| announcement.generation > old.generation)
                {
                    self.remote = Some(announcement);
                }
                self.request_if_needed(conn, shared, key, now).await?;
            }
            MT_GET => {
                if incoming.kind != FrameKind::Request
                    || incoming.id == RequestId::PUSH
                    || incoming.error.is_some()
                {
                    bail!("invalid live catalog request envelope");
                }
                let _: CatalogGetMsg = decode_fed_bounded(incoming, MT_GET, MAX_CONTROL)?;
                let built = match crate::fed_catalog::local_catalog(shared).await {
                    Ok(mine) => catalog_reply(incoming.id, mine.to_bytes()),
                    Err(error) => Err(error),
                };
                let reply = match built {
                    Ok(reply) => reply,
                    Err(error) => {
                        tracing::warn!(%error, "live catalog temporarily unavailable");
                        let mut reply = frame(FrameKind::Reply, MT_REPLY, incoming.id, &());
                        reply.error = Some(rabbithole_proto::ErrorCode::Unavailable);
                        reply
                    }
                };
                // The library may have changed since our announcement. A full
                // newer signed catalog is valid, and avoids pinning old copies.
                if !shared.peers.is_approved_origin(key, origin) {
                    bail!("peer approval revoked during catalog build");
                }
                conn.send(reply).await?;
            }
            MT_REPLY => {
                if incoming.kind != FrameKind::Reply
                    || !self
                        .pending
                        .as_ref()
                        .is_some_and(|pending| pending.id == incoming.id)
                {
                    return Ok(()); // wrong/late/unsolicited frames never consume a pending GET
                }
                if incoming.error.is_some() {
                    if incoming.payload.0.len() > MAX_CONTROL {
                        bail!("catalog error frame too large")
                    }
                    self.pending = None;
                    return Ok(());
                }
                let msg: CatalogMsg = decode_fed_bounded(incoming, MT_REPLY, MAX_CATALOG)?;
                let pending = self.pending.as_ref().expect("matching pending request");
                let signed = verify_announced(&msg.bytes, &pending.announced)?;
                signed
                    .verify(&PublicKey(*key))
                    .map_err(|e| anyhow!("catalog rejected: {e}"))?;
                let pending = self.pending.take().expect("matching pending request");
                if shared.catalogs.wants(key, signed.catalog.generation) {
                    if let Err(error) = crate::fed_catalog::ingest_fetched_catalog(
                        shared,
                        pending.fetch,
                        &msg.bytes,
                    ) {
                        // Admission, disk failure, concurrent ingest or changed
                        // approval never commits a partial cache. Retry later;
                        // none should take healthy board traffic down with it.
                        tracing::warn!(%error, "cannot retain live peer catalog; retrying next interval");
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }
}

fn catalog_reply(id: RequestId, bytes: Vec<u8>) -> Result<Frame> {
    if bytes.len() > MAX_CATALOG {
        bail!("catalog exceeds live reply limit")
    }
    let reply = frame(FrameKind::Reply, MT_REPLY, id, &CatalogMsg { bytes });
    if reply.payload.0.len() > MAX_CATALOG {
        bail!("catalog exceeds live frame limit")
    }
    Ok(reply)
}

fn verify_announced(bytes: &[u8], announced: &CatalogAnnounceMsg) -> Result<SignedCatalog> {
    let signed = SignedCatalog::from_bytes(bytes).ok_or_else(|| anyhow!("malformed catalog"))?;
    if signed.catalog.generation < announced.generation
        || (signed.catalog.generation == announced.generation
            && signed
                .catalog_id()
                .map_err(|e| anyhow!("catalog id: {e}"))?
                != announced.catalog_id)
    {
        bail!("catalog reply does not match announcement");
    }
    Ok(signed)
}

#[cfg(test)]
mod tests;
