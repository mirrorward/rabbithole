//! Wave 4.1 handlers: file libraries (family 5, low types).
//!
//! Areas → folders → files/aliases, with hide-vs-deny ACLs on
//! `files/<area>/<path>` resources, drop boxes (write-only unless
//! DROPBOX_VIEW), ratings, download counters, and indexed search. Bytes ride
//! the content-addressed blob store; small files transfer inline here, large
//! ones get dedicated streams in Wave 4.2.

use std::sync::Arc;

use rabbithole_blobs::BlobId;
use rabbithole_net::Connection;
use rabbithole_proto::filelib as pf;
use rabbithole_proto::{ErrorCode, Frame};
use rabbithole_server_core::files::KIND_FILE;
use rabbithole_server_core::{Caps, FileError, ServerEvent};
use rabbithole_store_server::repo6::FileNodeRow;

use crate::session::SessionCtx;
use crate::Shared;

/// Inline upload cap: control frames are capped at 1 MiB, so keep a margin
/// for the surrounding fields. Larger files use W4.2 streaming.
const MAX_INLINE_UPLOAD: usize = 768 * 1024;

pub(crate) fn view(row: &FileNodeRow) -> pf::FileNodeView {
    let mut v = pf::FileNodeView::new(
        row.id,
        row.area.clone(),
        row.kind,
        row.name.clone(),
        row.path.clone(),
    );
    v.is_dropbox = row.is_dropbox;
    v.blob_id = row.blob_id;
    v.size = row.size;
    v.mime = row.mime.clone();
    v.icon = row.icon.clone();
    v.comment = row.comment.clone();
    v.uploader = row.uploader.clone();
    v.downloads = row.downloads;
    v.rating_avg = row.rating_avg;
    v.rating_count = row.rating_count;
    v.created_at_unix = row.created_at;
    v
}

/// The ACL resource string for a node/area path.
fn resource(area: &str, path: Option<&str>) -> String {
    match path {
        Some(p) if !p.is_empty() => format!("files/{area}/{p}"),
        _ => format!("files/{area}"),
    }
}

fn map_err(e: FileError) -> ErrorCode {
    match e {
        FileError::NoSuchArea | FileError::NoSuchNode => ErrorCode::NotFound,
        FileError::Exists => ErrorCode::AlreadyExists,
        FileError::BadName | FileError::NotAFile => ErrorCode::BadRequest,
        FileError::NotAFolder => ErrorCode::BadRequest,
        FileError::Store(_) => ErrorCode::Internal,
    }
}

pub async fn handle(
    conn: &mut Box<dyn Connection>,
    frame: &Frame,
    shared: &Arc<Shared>,
    ctx: &mut SessionCtx,
) -> anyhow::Result<bool> {
    macro_rules! reply {
        ($msg:expr) => {
            conn.send(Frame::reply_to(frame, $msg)?).await?
        };
    }
    macro_rules! fail {
        ($code:expr) => {{
            conn.send(Frame::error_reply(frame, $code)).await?;
            return Ok(true);
        }};
    }
    macro_rules! try_file {
        ($e:expr) => {
            match $e {
                Ok(v) => v,
                Err(e) => fail!(map_err(e)),
            }
        };
    }

    // ---- List areas ------------------------------------------------------
    if frame.decode::<pf::AreaListRequest>().is_some() {
        if !ctx.allows(shared, "files", Caps::FILE_LIST) {
            fail!(ErrorCode::Forbidden);
        }
        let areas = try_file!(shared.files.areas().await)
            .iter()
            .map(|a| pf::FileAreaView::new(&a.slug, &a.title, &a.description))
            .collect();
        reply!(&pf::AreaList::new(areas));
        return Ok(true);
    }

    // ---- List a folder ---------------------------------------------------
    if let Some(Ok(req)) = frame.decode::<pf::FolderListRequest>() {
        if !ctx.allows(
            shared,
            &resource(&req.area, req.path.as_deref()),
            Caps::FILE_LIST,
        ) {
            fail!(ErrorCode::Forbidden);
        }
        // A drop box hides its contents unless you can view drop boxes.
        if let Some(path) = req.path.as_deref() {
            if let Some(node) = try_file!(shared.files.node_by_path(&req.area, path).await) {
                if node.is_dropbox
                    && !ctx.allows(
                        shared,
                        &resource(&req.area, req.path.as_deref()),
                        Caps::DROPBOX_VIEW,
                    )
                    && !ctx.allows(shared, &resource(&req.area, None), Caps::FILE_MANAGE)
                {
                    reply!(&pf::NodeList::new(vec![]));
                    return Ok(true);
                }
            }
        }
        let nodes = try_file!(shared.files.list(&req.area, req.path.as_deref()).await)
            .iter()
            .map(view)
            .collect();
        reply!(&pf::NodeList::new(nodes));
        return Ok(true);
    }

    // ---- Node metadata ---------------------------------------------------
    if let Some(Ok(req)) = frame.decode::<pf::NodeGet>() {
        let Some(node) = try_file!(shared.files.node(req.id).await) else {
            fail!(ErrorCode::NotFound)
        };
        if !ctx.allows(
            shared,
            &resource(&node.area, Some(&node.path)),
            Caps::FILE_LIST,
        ) {
            fail!(ErrorCode::Forbidden);
        }
        reply!(&pf::NodeReply::new(view(&node)));
        return Ok(true);
    }

    // ---- Create an area --------------------------------------------------
    if let Some(Ok(req)) = frame.decode::<pf::AreaCreate>() {
        if !ctx.allows(shared, "files", Caps::FILE_MANAGE) {
            fail!(ErrorCode::Forbidden);
        }
        let area = try_file!(
            shared
                .files
                .create_area(&req.slug, &req.title, &req.description)
                .await
        );
        reply!(&pf::AreaReply::new(pf::FileAreaView::new(
            area.slug,
            area.title,
            area.description
        )));
        return Ok(true);
    }

    // ---- Create a folder -------------------------------------------------
    if let Some(Ok(req)) = frame.decode::<pf::FolderCreate>() {
        if !ctx.allows(
            shared,
            &resource(&req.area, req.parent.as_deref()),
            Caps::FILE_MANAGE,
        ) {
            fail!(ErrorCode::Forbidden);
        }
        let node = try_file!(
            shared
                .files
                .mkdir(&req.area, req.parent.as_deref(), &req.name, req.is_dropbox)
                .await
        );
        reply!(&pf::NodeReply::new(view(&node)));
        return Ok(true);
    }

    // ---- Upload a file (inline bytes) ------------------------------------
    if let Some(Ok(req)) = frame.decode::<pf::FileUpload>() {
        if !ctx.allows(
            shared,
            &resource(&req.area, req.parent.as_deref()),
            Caps::FILE_UPLOAD,
        ) {
            fail!(ErrorCode::Forbidden);
        }
        if req.bytes.len() > MAX_INLINE_UPLOAD {
            fail!(ErrorCode::TooLarge);
        }
        let blobs = shared.blobs.clone();
        let bytes = req.bytes.clone();
        let size = bytes.len() as i64;
        let blob_id = match tokio::task::spawn_blocking(move || blobs.put(&bytes)).await? {
            Ok(id) => id.0,
            Err(_) => fail!(ErrorCode::Internal),
        };
        let uploader = format!("{}@{}", ctx.screen_name, shared.origin_name());
        let node = try_file!(
            shared
                .files
                .add_file(
                    &req.area,
                    req.parent.as_deref(),
                    &req.name,
                    &blob_id,
                    size,
                    &req.mime,
                    &req.icon,
                    &req.comment,
                    &uploader,
                    ctx.account_id,
                )
                .await
        );
        shared.bus.publish(ServerEvent::FileAdded {
            area: req.area.clone(),
            id: node.id,
        });
        reply!(&pf::NodeReply::new(view(&node)));
        return Ok(true);
    }

    // ---- Download a file -------------------------------------------------
    if let Some(Ok(req)) = frame.decode::<pf::FileDownloadRequest>() {
        let Some(node) = try_file!(shared.files.node(req.id).await) else {
            fail!(ErrorCode::NotFound)
        };
        let target = try_file!(shared.files.resolve(node.id).await);
        if target.kind != KIND_FILE {
            fail!(ErrorCode::BadRequest);
        }
        if !ctx.allows(
            shared,
            &resource(&target.area, Some(&target.path)),
            Caps::FILE_DOWNLOAD,
        ) {
            fail!(ErrorCode::Forbidden);
        }
        // Drop-boxed content is not downloadable without view rights.
        if try_file!(shared.files.in_dropbox(&target).await)
            && !ctx.allows(
                shared,
                &resource(&target.area, Some(&target.path)),
                Caps::DROPBOX_VIEW,
            )
            && !ctx.allows(shared, &resource(&target.area, None), Caps::FILE_MANAGE)
        {
            fail!(ErrorCode::Forbidden);
        }
        let Some(blob_id) = target.blob_id else {
            fail!(ErrorCode::NotFound)
        };
        let served = try_file!(shared.files.record_download(node.id).await);
        let blobs = shared.blobs.clone();
        let bytes = match tokio::task::spawn_blocking(move || blobs.get(&BlobId(blob_id))).await? {
            Ok(b) => b,
            Err(_) => fail!(ErrorCode::NotFound),
        };
        reply!(&pf::FileContent::new(view(&served), bytes));
        return Ok(true);
    }

    // ---- Delete a node ---------------------------------------------------
    if let Some(Ok(req)) = frame.decode::<pf::NodeDelete>() {
        let Some(node) = try_file!(shared.files.node(req.id).await) else {
            fail!(ErrorCode::NotFound)
        };
        let is_owner = node.uploader_id == Some(ctx.account_id);
        if !is_owner && !ctx.allows(shared, &resource(&node.area, None), Caps::FILE_MANAGE) {
            fail!(ErrorCode::Forbidden);
        }
        try_file!(shared.files.delete(node.id).await);
        conn.send(Frame::ack(frame)).await?;
        return Ok(true);
    }

    // ---- Edit metadata ---------------------------------------------------
    if let Some(Ok(req)) = frame.decode::<pf::SetMetadata>() {
        let Some(node) = try_file!(shared.files.node(req.id).await) else {
            fail!(ErrorCode::NotFound)
        };
        let is_owner = node.uploader_id == Some(ctx.account_id);
        if !is_owner && !ctx.allows(shared, &resource(&node.area, None), Caps::FILE_MANAGE) {
            fail!(ErrorCode::Forbidden);
        }
        let node = try_file!(
            shared
                .files
                .set_metadata(node.id, &req.icon, &req.comment)
                .await
        );
        reply!(&pf::NodeReply::new(view(&node)));
        return Ok(true);
    }

    // ---- Search ----------------------------------------------------------
    if let Some(Ok(req)) = frame.decode::<pf::SearchRequest>() {
        let res = match req.area.as_deref() {
            Some(a) => resource(a, None),
            None => "files".to_string(),
        };
        if !ctx.allows(shared, &res, Caps::FILE_LIST) {
            fail!(ErrorCode::Forbidden);
        }
        let limit = req.limit.clamp(1, 200) as i64;
        let nodes = try_file!(
            shared
                .files
                .search(req.area.as_deref(), &req.query, limit)
                .await
        )
        .iter()
        .map(view)
        .collect();
        reply!(&pf::SearchResults::new(nodes));
        return Ok(true);
    }

    // ---- Rate ------------------------------------------------------------
    if let Some(Ok(req)) = frame.decode::<pf::RateFile>() {
        if ctx.is_guest {
            fail!(ErrorCode::Forbidden);
        }
        let Some(node) = try_file!(shared.files.node(req.id).await) else {
            fail!(ErrorCode::NotFound)
        };
        if !ctx.allows(
            shared,
            &resource(&node.area, Some(&node.path)),
            Caps::FILE_DOWNLOAD,
        ) {
            fail!(ErrorCode::Forbidden);
        }
        let node = try_file!(shared.files.rate(node.id, ctx.account_id, req.stars).await);
        reply!(&pf::NodeReply::new(view(&node)));
        return Ok(true);
    }

    // ---- Create an alias -------------------------------------------------
    if let Some(Ok(req)) = frame.decode::<pf::AliasCreate>() {
        if !ctx.allows(
            shared,
            &resource(&req.area, req.parent.as_deref()),
            Caps::FILE_MANAGE,
        ) {
            fail!(ErrorCode::Forbidden);
        }
        let node = try_file!(
            shared
                .files
                .add_alias(
                    &req.area,
                    req.parent.as_deref(),
                    &req.name,
                    &req.target_path
                )
                .await
        );
        reply!(&pf::NodeReply::new(view(&node)));
        return Ok(true);
    }

    Ok(false)
}

/// Project a FileAdded bus event into a push.
pub(crate) fn file_push(event: &ServerEvent) -> Option<Frame> {
    if let ServerEvent::FileAdded { area, id } = event {
        Frame::push(&pf::FileAdded::new(area.clone(), *id)).ok()
    } else {
        None
    }
}
