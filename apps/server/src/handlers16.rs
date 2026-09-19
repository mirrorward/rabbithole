//! Federation peers, trusted origins and backups over the wire (ADMIN
//! 47..60): what `ctl` could do from the burrow's own machine and the console
//! could not. Every operation requires `CONFIG_ADMIN`; the backup ones also
//! require the Admin role, because a snapshot is the whole burrow, identity
//! keys included, and removing one is for good.

use std::sync::Arc;

use rabbithole_net::Connection;
use rabbithole_proto::{admin as padm, ErrorCode, Frame};
use rabbithole_server_core::{Caps, PeerState, Role};

use crate::fed_flood::OriginTrust;
use crate::federation::{self, PeerRefusal};
use crate::handlers15::audit;
use crate::session::SessionCtx;
use crate::Shared;

fn peer_state_wire(state: PeerState) -> u8 {
    match state {
        PeerState::Pending => padm::peer_state::PENDING,
        PeerState::Disconnected => padm::peer_state::DISCONNECTED,
        PeerState::Connected => padm::peer_state::CONNECTED,
    }
}

fn origin_trust_wire(trust: OriginTrust) -> u8 {
    match trust {
        OriginTrust::DirectPeer => padm::origin_trust::DIRECT_PEER,
        OriginTrust::Operator => padm::origin_trust::OPERATOR,
    }
}

fn entry_of(info: crate::backup::SnapshotInfo) -> padm::BackupEntry {
    padm::BackupEntry::new(
        info.name,
        info.created_at,
        info.workspace_version,
        info.files,
        info.total_bytes,
    )
}

pub async fn handle(
    conn: &mut Box<dyn Connection>,
    frame: &Frame,
    shared: &Arc<Shared>,
    ctx: &mut SessionCtx,
) -> anyhow::Result<bool> {
    macro_rules! fail {
        ($code:expr) => {{
            conn.send(Frame::error_reply(frame, $code)).await?;
            return Ok(true);
        }};
    }
    macro_rules! config_admins_only {
        () => {
            if !ctx.allows(shared, "admin", Caps::CONFIG_ADMIN) {
                fail!(ErrorCode::Forbidden)
            }
        };
    }
    /// A snapshot is the whole burrow: `CONFIG_ADMIN` granted through a
    /// class is not enough on its own.
    macro_rules! backup_operators_only {
        () => {
            config_admins_only!();
            if ctx.role < Role::Admin {
                fail!(ErrorCode::Forbidden)
            }
        };
    }

    if frame.decode::<padm::PeerListRequest>().is_some() {
        config_admins_only!();
        let peers = shared
            .peers
            .snapshot()
            .into_iter()
            .map(|peer| {
                let configured = federation::is_configured_peer(
                    shared,
                    &peer.server_key,
                    peer.origin.as_deref(),
                );
                padm::PeerEntry::new(
                    peer.server_key,
                    peer.name,
                    peer.origin,
                    peer.addr,
                    peer_state_wire(peer.state),
                    peer.approved,
                    configured,
                )
            })
            .collect();
        conn.send(Frame::reply_to(frame, &padm::PeerList::new(peers))?)
            .await?;
        return Ok(true);
    }

    if let Some(Ok(req)) = frame.decode::<padm::PeerApprove>() {
        config_admins_only!();
        match federation::approve_peer(shared, req.key, req.origin) {
            Ok((origin, _)) => {
                audit(
                    shared,
                    &ctx.login,
                    "peer-approve",
                    format!("{origin} {}", hex::encode(req.key)),
                );
                conn.send(Frame::ack(frame)).await?;
            }
            Err(PeerRefusal::Persist(error)) => return Err(error),
            Err(refusal) => {
                tracing::info!(%refusal, "peer approval refused");
                fail!(ErrorCode::BadRequest);
            }
        }
        return Ok(true);
    }

    if let Some(Ok(req)) = frame.decode::<padm::PeerRevoke>() {
        config_admins_only!();
        match federation::revoke_peer(shared, req.key) {
            Ok(true) => {
                audit(shared, &ctx.login, "peer-revoke", hex::encode(req.key));
                conn.send(Frame::ack(frame)).await?;
            }
            Ok(false) => fail!(ErrorCode::NotFound),
            Err(PeerRefusal::Persist(error)) => return Err(error),
            Err(_) => fail!(ErrorCode::BadRequest),
        }
        return Ok(true);
    }

    if frame.decode::<padm::OriginListRequest>().is_some() {
        config_admins_only!();
        let mut pins = shared.fed_flood.pins();
        pins.sort_by(|a, b| a.origin.cmp(&b.origin));
        let origins = pins
            .into_iter()
            .map(|pin| padm::OriginEntry::new(pin.origin, pin.key, origin_trust_wire(pin.trust)))
            .collect();
        conn.send(Frame::reply_to(frame, &padm::OriginList::new(origins))?)
            .await?;
        return Ok(true);
    }

    if let Some(Ok(req)) = frame.decode::<padm::OriginPin>() {
        config_admins_only!();
        let origin = req.origin.trim().to_string();
        // Everything `trust_operator` refuses is the operator's input: a bad
        // name, a key already bound elsewhere, a full registry.
        if shared.fed_flood.trust_operator(&origin, req.key).is_err() {
            fail!(ErrorCode::BadRequest);
        }
        audit(
            shared,
            &ctx.login,
            "origin-pin",
            format!("{origin} {}", hex::encode(req.key)),
        );
        conn.send(Frame::ack(frame)).await?;
        return Ok(true);
    }

    if frame.decode::<padm::BackupListRequest>().is_some() {
        backup_operators_only!();
        let dir = crate::backup::backups_dir(&shared.config.read());
        let snapshots = tokio::task::spawn_blocking({
            let dir = dir.clone();
            move || crate::backup::list_snapshots(&dir)
        })
        .await?
        .into_iter()
        .map(entry_of)
        .collect();
        conn.send(Frame::reply_to(
            frame,
            &padm::BackupList::new(dir.display().to_string(), snapshots),
        )?)
        .await?;
        return Ok(true);
    }

    if frame.decode::<padm::BackupCreate>().is_some() {
        backup_operators_only!();
        let dir = crate::backup::backups_dir(&shared.config.read());
        let outcome = crate::backup::snapshot(shared, &dir).await?;
        let name = outcome
            .dir
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .to_string();
        audit(
            shared,
            &ctx.login,
            "backup",
            format!(
                "{} files={} bytes={}",
                outcome.dir.display(),
                outcome.files,
                outcome.total_bytes
            ),
        );
        let Some(info) = crate::backup::snapshot_info(&dir, &name) else {
            fail!(ErrorCode::Internal);
        };
        conn.send(Frame::reply_to(
            frame,
            &padm::BackupMade::new(entry_of(info)),
        )?)
        .await?;
        return Ok(true);
    }

    if let Some(Ok(req)) = frame.decode::<padm::BackupVerify>() {
        backup_operators_only!();
        let dir = crate::backup::backups_dir(&shared.config.read());
        if crate::backup::snapshot_info(&dir, &req.name).is_none() {
            fail!(ErrorCode::NotFound);
        }
        let checked = crate::backup::check_snapshot(dir.join(&req.name)).await?;
        conn.send(Frame::reply_to(
            frame,
            &padm::BackupVerified::new(
                req.name,
                checked.ok,
                checked.detail,
                checked.files,
                checked.total_bytes,
            ),
        )?)
        .await?;
        return Ok(true);
    }

    if let Some(Ok(req)) = frame.decode::<padm::BackupDelete>() {
        backup_operators_only!();
        let dir = crate::backup::backups_dir(&shared.config.read());
        let removed = tokio::task::spawn_blocking({
            let dir = dir.clone();
            let name = req.name.clone();
            move || crate::backup::delete_snapshot(&dir, &name)
        })
        .await??;
        if !removed {
            fail!(ErrorCode::NotFound);
        }
        audit(shared, &ctx.login, "backup-delete", req.name);
        conn.send(Frame::ack(frame)).await?;
        return Ok(true);
    }

    Ok(false)
}
