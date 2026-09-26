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
use rabbithole_server_core::{Caps, FileError, FileService, ServerEvent};
use rabbithole_store_server::repo6::{FileAreaRow, FileNodeRow};

use crate::handlers15::audit;
use crate::session::SessionCtx;
use crate::Shared;

#[cfg(test)]
mod metadata_tests;
#[cfg(test)]
mod search_tests;

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
        FileError::BadName | FileError::NotAFile | FileError::NotEmpty => ErrorCode::BadRequest,
        FileError::NotAFolder | FileError::IntoItself => ErrorCode::BadRequest,
        FileError::Store(_) => ErrorCode::Internal,
    }
}

/// Metadata describes what can be seen, independently of download rights.
/// Check each returned resource so a parent's listing never overrides a
/// child's nearer ACL. The evaluator supplies normal nearest-rule inheritance.
fn visible_areas(
    areas: &[FileAreaRow],
    allows: impl Fn(&str, Caps) -> bool,
) -> Vec<pf::FileAreaView> {
    areas
        .iter()
        .filter(|a| allows(&resource(&a.slug, None), Caps::SEE | Caps::FILE_LIST))
        .map(|a| pf::FileAreaView::new(&a.slug, &a.title, &a.description))
        .collect()
}

async fn metadata_visible(
    files: &FileService,
    node: &FileNodeRow,
    allows: &impl Fn(&str, Caps) -> bool,
    quarantined: &impl Fn(Option<&[u8; 32]>) -> bool,
) -> Result<bool, FileError> {
    let res = resource(&node.area, Some(&node.path));
    if !allows(&res, Caps::SEE | Caps::FILE_LIST)
        || (quarantined(node.blob_id.as_ref()) && !allows("moderation", Caps::MODERATE))
    {
        return Ok(false);
    }
    Ok(allows(&res, Caps::DROPBOX_VIEW)
        || allows(&resource(&node.area, None), Caps::FILE_MANAGE)
        || !files.has_dropbox_ancestor(node.id).await?)
}

async fn node_metadata(
    files: &FileService,
    id: i64,
    allows: impl Fn(&str, Caps) -> bool,
    quarantined: impl Fn(Option<&[u8; 32]>) -> bool,
) -> Result<pf::FileNodeView, ErrorCode> {
    let node = files
        .node(id)
        .await
        .map_err(map_err)?
        .ok_or(ErrorCode::NotFound)?;
    if !metadata_visible(files, &node, &allows, &quarantined)
        .await
        .map_err(map_err)?
    {
        // A known ID must not distinguish hidden metadata from absent data.
        return Err(ErrorCode::NotFound);
    }
    Ok(view(&node))
}

async fn folder_metadata(
    files: &FileService,
    area: &str,
    path: Option<&str>,
    allows: impl Fn(&str, Caps) -> bool,
    quarantined: impl Fn(Option<&[u8; 32]>) -> bool,
) -> Result<Vec<pf::FileNodeView>, ErrorCode> {
    // Area lookup is case-insensitive, ACL resources are not. Authorize the
    // stored spelling so a caller cannot select a different rule with casing.
    let canonical_area = files.area(area).await.map_err(map_err)?.slug;
    let area = canonical_area.as_str();
    let res = resource(area, path);
    if !allows(&res, Caps::SEE) {
        return Err(ErrorCode::NotFound);
    }
    if !allows(&res, Caps::FILE_LIST) {
        return Err(ErrorCode::Forbidden);
    }
    if let Some(path) = path.filter(|path| !path.is_empty()) {
        let node = files
            .node_by_path(area, path)
            .await
            .map_err(map_err)?
            .ok_or(ErrorCode::NotFound)?;
        if !metadata_visible(files, &node, &allows, &quarantined)
            .await
            .map_err(map_err)?
        {
            return Err(ErrorCode::NotFound);
        }
        // The drop box itself remains visible as an upload destination.
        // Its contents are empty to people without inspection rights;
        // descendants of a hidden box are absent even by a known path/ID.
        if node.is_dropbox
            && !allows(&res, Caps::DROPBOX_VIEW)
            && !allows(&resource(area, None), Caps::FILE_MANAGE)
        {
            return Ok(Vec::new());
        }
    }
    let mut visible = Vec::new();
    for node in files.list(area, path).await.map_err(map_err)? {
        if metadata_visible(files, &node, &allows, &quarantined)
            .await
            .map_err(map_err)?
        {
            visible.push(view(&node));
        }
    }
    Ok(visible)
}

/// Keep authorization ahead of the result limit. Permissions are supplied by
/// the live session; the service supplies only bounded pages of candidates.
async fn visible_search(
    files: &FileService,
    area: Option<&str>,
    query: &str,
    limit: usize,
    allows: impl Fn(&str, Caps) -> bool,
    quarantined: impl Fn(Option<&[u8; 32]>) -> bool,
) -> Result<Vec<pf::FileNodeView>, FileError> {
    let limit = limit.clamp(1, 200);
    let mut found = Vec::new();
    let mut before = None;
    loop {
        let page = files.search_page(area, query, 64, before).await?;
        let Some(last) = page.last() else {
            return Ok(found);
        };
        before = Some((last.created_at, last.id));
        for node in page {
            if !metadata_visible(files, &node, &allows, &quarantined).await? {
                continue;
            }
            found.push(view(&node));
            if found.len() == limit {
                return Ok(found);
            }
        }
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
        if !ctx.allows(shared, "files", Caps::SEE | Caps::FILE_LIST) {
            fail!(ErrorCode::Forbidden);
        }
        let areas = visible_areas(&try_file!(shared.files.areas().await), |res, caps| {
            ctx.allows(shared, res, caps)
        });
        reply!(&pf::AreaList::new(areas));
        return Ok(true);
    }

    // ---- List a folder ---------------------------------------------------
    if let Some(Ok(req)) = frame.decode::<pf::FolderListRequest>() {
        let nodes = match folder_metadata(
            &shared.files,
            &req.area,
            req.path.as_deref(),
            |res, caps| ctx.allows(shared, res, caps),
            |blob| shared.moderation.file_quarantined(blob),
        )
        .await
        {
            Ok(nodes) => nodes,
            Err(code) => fail!(code),
        };
        reply!(&pf::NodeList::new(nodes));
        return Ok(true);
    }

    // ---- Node metadata ---------------------------------------------------
    if let Some(Ok(req)) = frame.decode::<pf::NodeGet>() {
        let node = match node_metadata(
            &shared.files,
            req.id,
            |res, caps| ctx.allows(shared, res, caps),
            |blob| shared.moderation.file_quarantined(blob),
        )
        .await
        {
            Ok(node) => node,
            Err(code) => fail!(code),
        };
        reply!(&pf::NodeReply::new(node));
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
        audit(shared, &ctx.login, "area-create", area.slug.clone());
        reply!(&pf::AreaReply::new(pf::FileAreaView::new(
            area.slug,
            area.title,
            area.description
        )));
        return Ok(true);
    }

    // ---- Edit or remove an area -------------------------------------------
    if let Some(Ok(req)) = frame.decode::<pf::AreaUpdate>() {
        if !ctx.allows(shared, "files", Caps::FILE_MANAGE) {
            fail!(ErrorCode::Forbidden);
        }
        try_file!(
            shared
                .files
                .update_area(&req.slug, &req.title, &req.description)
                .await
        );
        audit(shared, &ctx.login, "area-update", req.slug.clone());
        conn.send(Frame::ack(frame)).await?;
        return Ok(true);
    }

    if let Some(Ok(req)) = frame.decode::<pf::AreaDelete>() {
        if !ctx.allows(shared, "files", Caps::FILE_MANAGE) {
            fail!(ErrorCode::Forbidden);
        }
        try_file!(shared.files.delete_area(&req.slug).await);
        audit(shared, &ctx.login, "area-delete", req.slug.clone());
        conn.send(Frame::ack(frame)).await?;
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
        audit(
            shared,
            &ctx.login,
            if req.is_dropbox {
                "dropbox-create"
            } else {
                "folder-create"
            },
            format!("{} in {}", node.name, req.area),
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
        // The largest file and the account's space, enforced here too so
        // neither can be sidestepped with small inline uploads. Held until
        // the file is recorded, so two uploads cannot both fit the last of
        // the space.
        let _commit = crate::upload_gate::commit_lock(shared).await;
        if let Err(refused) =
            crate::upload_gate::check(shared, ctx.account_id, req.bytes.len() as u64).await
        {
            fail!(refused.code());
        }
        // Hash-deny gate: refuse denied content before it touches the blob
        // store (the blob id IS the blake3 of the bytes).
        if shared
            .moderation
            .is_denied(blake3::hash(&req.bytes).as_bytes())
        {
            fail!(ErrorCode::Forbidden);
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
        // Quarantined content is not served to non-moderators.
        if shared.moderation.file_quarantined(target.blob_id.as_ref())
            && !ctx.allows(shared, "moderation", Caps::MODERATE)
        {
            fail!(ErrorCode::NotFound);
        }
        if !ctx.allows(
            shared,
            &resource(&target.area, Some(&target.path)),
            Caps::FILE_DOWNLOAD,
        ) {
            fail!(ErrorCode::Forbidden);
        }
        // Both the requested placement and an alias's resolved target must
        // permit access; a public alias cannot expose a hidden target, nor can
        // a hidden alias be opened by knowing its ID. RH-169 shares the same
        // recursive ancestor rule across byte-serving surfaces.
        for placement in [&node, &target] {
            if try_file!(shared.files.in_dropbox(placement).await)
                && !ctx.allows(
                    shared,
                    &resource(&placement.area, Some(&placement.path)),
                    Caps::DROPBOX_VIEW,
                )
                && !ctx.allows(shared, &resource(&placement.area, None), Caps::FILE_MANAGE)
            {
                fail!(ErrorCode::Forbidden);
            }
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
        // Somebody clearing out their own upload is their business; a
        // manager removing somebody else's is the record's.
        if !is_owner {
            audit(
                shared,
                &ctx.login,
                "node-delete",
                format!("{} in {}", node.name, node.area),
            );
        }
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

    // ---- What the caller may upload --------------------------------------
    if frame.decode::<pf::UploadLimitsRequest>().is_some() {
        let (max_file, quota) = {
            let config = shared.config.read();
            (config.upload_max_file_bytes, config.upload_quota_bytes)
        };
        let used = if ctx.is_guest {
            0
        } else {
            crate::upload_gate::used_bytes(shared, ctx.account_id).await
        };
        reply!(&pf::UploadLimits::new(max_file, quota, used));
        return Ok(true);
    }

    // ---- Rename and move -------------------------------------------------
    if let Some(Ok(req)) = frame.decode::<pf::NodeRename>() {
        let Some(node) = try_file!(shared.files.node(req.id).await) else {
            fail!(ErrorCode::NotFound)
        };
        let is_owner = node.uploader_id == Some(ctx.account_id);
        if !is_owner && !ctx.allows(shared, &resource(&node.area, None), Caps::FILE_MANAGE) {
            fail!(ErrorCode::Forbidden);
        }
        let was = node.name.clone();
        let node = try_file!(shared.files.rename(node.id, &req.name).await);
        if !is_owner {
            audit(
                shared,
                &ctx.login,
                "node-rename",
                format!("{was} to {} in {}", node.name, node.area),
            );
        }
        reply!(&pf::NodeReply::new(view(&node)));
        return Ok(true);
    }

    if let Some(Ok(req)) = frame.decode::<pf::NodeMove>() {
        let Some(node) = try_file!(shared.files.node(req.id).await) else {
            fail!(ErrorCode::NotFound)
        };
        let dest = req.folder.as_deref().filter(|f| !f.is_empty());
        // Where it is and where it goes: both are the manager's to touch.
        if !ctx.allows(shared, &resource(&node.area, None), Caps::FILE_MANAGE)
            || !ctx.allows(shared, &resource(&node.area, dest), Caps::FILE_MANAGE)
        {
            fail!(ErrorCode::Forbidden);
        }
        audit(
            shared,
            &ctx.login,
            "node-move",
            format!("{} in {}", node.name, node.area),
        );
        let node = try_file!(shared.files.move_to(node.id, dest).await);
        reply!(&pf::NodeReply::new(view(&node)));
        return Ok(true);
    }

    // ---- Search ----------------------------------------------------------
    if let Some(Ok(req)) = frame.decode::<pf::SearchRequest>() {
        let res = match req.area.as_deref() {
            Some(a) => resource(a, None),
            None => "files".to_string(),
        };
        if !ctx.allows(shared, &res, Caps::SEE | Caps::FILE_LIST) {
            fail!(ErrorCode::Forbidden);
        }
        let nodes = try_file!(
            visible_search(
                &shared.files,
                req.area.as_deref(),
                &req.query,
                req.limit.clamp(1, 200) as usize,
                |res, caps| ctx.allows(shared, res, caps),
                |blob| shared.moderation.file_quarantined(blob),
            )
            .await
        );
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
        audit(
            shared,
            &ctx.login,
            "alias-create",
            format!("{} to {} in {}", node.name, req.target_path, req.area),
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
