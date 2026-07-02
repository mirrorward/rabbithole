//! The client-side transfer-queue driver (feature `native`).
//!
//! Drains the persistent [`rabbithole_store_client`] transfer queue over a
//! live [`Client`]: highest priority first, each item marked `ACTIVE` while it
//! runs and `DONE`/`FAILED` afterwards. The transfers themselves resume from
//! disk (download) or the server's staged prefix (upload), so a driver that
//! is interrupted and restarted picks up where it left off. Bandwidth is
//! capped via [`Client::set_rate_limit`] before the drain begins.

use std::path::Path;

use rabbithole_proto::ErrorCode;
use rabbithole_store_client::transfers::{self as tq, TransferItem, TransferQueue};
use rabbithole_store_client::{Connection, StoreError};

use crate::{Client, ClientError};

/// Outcome of one [`drain`] pass.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct DrainReport {
    pub completed: usize,
    pub failed: usize,
}

/// Run every `QUEUED` item to completion in priority order. `now` supplies the
/// caller's unix clock (injectable for tests). A failed item is recorded with
/// its error and the drain continues with the next; the pass returns once no
/// `QUEUED` item remains. `PAUSED`/`FAILED` items are left untouched.
pub async fn drain(
    client: &mut Client,
    store: &Connection,
    now: impl Fn() -> i64,
) -> Result<DrainReport, ClientError> {
    let mut report = DrainReport::default();
    while let Some(item) = TransferQueue(store).next_queued().map_err(store_err)? {
        TransferQueue(store)
            .set_state(item.id, tq::ACTIVE, now())
            .map_err(store_err)?;
        match run_one(client, &item).await {
            Ok(bytes) => {
                let q = TransferQueue(store);
                q.set_progress(item.id, bytes, now()).map_err(store_err)?;
                q.set_state(item.id, tq::DONE, now()).map_err(store_err)?;
                report.completed += 1;
            }
            Err(e) => {
                TransferQueue(store)
                    .fail(item.id, &e.to_string(), now())
                    .map_err(store_err)?;
                report.failed += 1;
            }
        }
    }
    Ok(report)
}

/// Execute a single queued item, returning the transferred byte count.
async fn run_one(client: &mut Client, item: &TransferItem) -> Result<i64, ClientError> {
    match item.direction {
        tq::DIR_DOWNLOAD => {
            let node_id = item
                .node_id
                .ok_or(ClientError::Refused(ErrorCode::BadRequest))?;
            let n = client
                .transfer_download(node_id, Path::new(&item.local_path))
                .await?;
            Ok(n as i64)
        }
        tq::DIR_UPLOAD => {
            let area = item.area.clone().unwrap_or_default();
            let name = item.name.clone().unwrap_or_default();
            let node = client
                .transfer_upload(
                    &area,
                    item.parent.clone(),
                    &name,
                    Path::new(&item.local_path),
                    &item.mime,
                    &item.comment,
                )
                .await?;
            Ok(node.size)
        }
        _ => Err(ClientError::Refused(ErrorCode::BadRequest)),
    }
}

/// The queue store lives on the same disk as the transfer; surface a store
/// failure through the client's single IO error channel.
fn store_err(e: StoreError) -> ClientError {
    ClientError::Io(std::io::Error::other(e.to_string()))
}
