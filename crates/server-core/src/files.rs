//! File library service: the browsable/searchable tree over the content-
//! addressed blob store. This service owns path derivation, name validation,
//! and tree invariants; the actual bytes are put/served by the session
//! handler (which holds the blob store). Authorization is the caller's job
//! (the handler consults the permission evaluator with `files/<area>/<path>`
//! resource strings — nearest-ancestor, hide-vs-deny).

use rabbithole_store_server::repo6::{FileAreaRow, FileNodeRow, FilesRepo};
use rabbithole_store_server::{SqlitePool, StoreError};

#[derive(Debug, thiserror::Error)]
pub enum FileError {
    #[error("no such area")]
    NoSuchArea,
    #[error("no such node")]
    NoSuchNode,
    #[error("parent is not a folder")]
    NotAFolder,
    #[error("name already exists here")]
    Exists,
    #[error("bad name")]
    BadName,
    #[error("not a file")]
    NotAFile,
    #[error("store: {0}")]
    Store(#[from] StoreError),
}

/// Node kinds on the wire and in the store.
pub const KIND_FOLDER: u8 = 0;
pub const KIND_FILE: u8 = 1;
pub const KIND_ALIAS: u8 = 2;

pub struct FileService {
    pool: SqlitePool,
}

/// Validate a single path component (no separators, sane length).
fn clean_name(name: &str) -> Result<String, FileError> {
    let name = name.trim();
    if name.is_empty() || name.len() > 128 || name.contains('/') || name == "." || name == ".." {
        return Err(FileError::BadName);
    }
    Ok(name.to_string())
}

fn child_path(parent_path: Option<&str>, name: &str) -> String {
    match parent_path {
        Some(p) if !p.is_empty() => format!("{p}/{name}"),
        _ => name.to_string(),
    }
}

impl FileService {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    fn repo(&self) -> FilesRepo<'_> {
        FilesRepo(&self.pool)
    }

    // ---- Areas -----------------------------------------------------------

    pub async fn create_area(
        &self,
        slug: &str,
        title: &str,
        description: &str,
    ) -> Result<FileAreaRow, FileError> {
        let slug = clean_name(slug)?;
        if self.repo().area_by_slug(&slug).await?.is_some() {
            return Err(FileError::Exists);
        }
        Ok(self.repo().create_area(&slug, title, description).await?)
    }

    pub async fn areas(&self) -> Result<Vec<FileAreaRow>, FileError> {
        Ok(self.repo().areas().await?)
    }

    async fn area(&self, slug: &str) -> Result<FileAreaRow, FileError> {
        self.repo()
            .area_by_slug(slug)
            .await?
            .ok_or(FileError::NoSuchArea)
    }

    /// Resolve a folder within an area by its virtual path. `None`/empty =
    /// the area root. Returns `(area_id, parent_id)`.
    async fn resolve_parent(
        &self,
        area: &FileAreaRow,
        folder_path: Option<&str>,
    ) -> Result<Option<i64>, FileError> {
        match folder_path {
            Some(p) if !p.is_empty() => {
                let node = self
                    .repo()
                    .node_by_path(area.id, p)
                    .await?
                    .ok_or(FileError::NoSuchNode)?;
                if node.kind != KIND_FOLDER {
                    return Err(FileError::NotAFolder);
                }
                Ok(Some(node.id))
            }
            _ => Ok(None),
        }
    }

    // ---- Tree mutation ---------------------------------------------------

    pub async fn mkdir(
        &self,
        area_slug: &str,
        parent_path: Option<&str>,
        name: &str,
        is_dropbox: bool,
    ) -> Result<FileNodeRow, FileError> {
        let name = clean_name(name)?;
        let area = self.area(area_slug).await?;
        let parent_id = self.resolve_parent(&area, parent_path).await?;
        let path = child_path(parent_path, &name);
        if self.repo().node_by_path(area.id, &path).await?.is_some() {
            return Err(FileError::Exists);
        }
        Ok(self
            .repo()
            .create_folder(area.id, parent_id, &name, &path, is_dropbox)
            .await?)
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn add_file(
        &self,
        area_slug: &str,
        parent_path: Option<&str>,
        name: &str,
        blob_id: &[u8; 32],
        size: i64,
        mime: &str,
        icon: &str,
        comment: &str,
        uploader: &str,
        uploader_id: i64,
    ) -> Result<FileNodeRow, FileError> {
        let name = clean_name(name)?;
        let area = self.area(area_slug).await?;
        let parent_id = self.resolve_parent(&area, parent_path).await?;
        let path = child_path(parent_path, &name);
        if self.repo().node_by_path(area.id, &path).await?.is_some() {
            return Err(FileError::Exists);
        }
        Ok(self
            .repo()
            .create_file(
                area.id,
                parent_id,
                &name,
                &path,
                blob_id,
                size,
                mime,
                icon,
                comment,
                uploader,
                uploader_id,
            )
            .await?)
    }

    pub async fn add_alias(
        &self,
        area_slug: &str,
        parent_path: Option<&str>,
        name: &str,
        target_path: &str,
    ) -> Result<FileNodeRow, FileError> {
        let name = clean_name(name)?;
        let area = self.area(area_slug).await?;
        let target = self
            .repo()
            .node_by_path(area.id, target_path)
            .await?
            .ok_or(FileError::NoSuchNode)?;
        let parent_id = self.resolve_parent(&area, parent_path).await?;
        let path = child_path(parent_path, &name);
        if self.repo().node_by_path(area.id, &path).await?.is_some() {
            return Err(FileError::Exists);
        }
        Ok(self
            .repo()
            .create_alias(area.id, parent_id, &name, &path, target.id)
            .await?)
    }

    // ---- Browse / read ---------------------------------------------------

    pub async fn list(
        &self,
        area_slug: &str,
        folder_path: Option<&str>,
    ) -> Result<Vec<FileNodeRow>, FileError> {
        let area = self.area(area_slug).await?;
        let parent_id = self.resolve_parent(&area, folder_path).await?;
        Ok(self.repo().children(area.id, parent_id).await?)
    }

    pub async fn node(&self, id: i64) -> Result<Option<FileNodeRow>, FileError> {
        Ok(self.repo().node_by_id(id).await?)
    }

    pub async fn node_by_path(
        &self,
        area_slug: &str,
        path: &str,
    ) -> Result<Option<FileNodeRow>, FileError> {
        let area = self.area(area_slug).await?;
        Ok(self.repo().node_by_path(area.id, path).await?)
    }

    /// Whether a node's immediate parent folder is a drop box (its contents
    /// are hidden without DROPBOX_VIEW).
    pub async fn in_dropbox(&self, node: &FileNodeRow) -> Result<bool, FileError> {
        let Some(parent_id) = node.parent_id else {
            return Ok(false);
        };
        Ok(self
            .repo()
            .node_by_id(parent_id)
            .await?
            .map(|p| p.is_dropbox)
            .unwrap_or(false))
    }

    /// Resolve a node, following one alias hop to its target.
    pub async fn resolve(&self, id: i64) -> Result<FileNodeRow, FileError> {
        let node = self
            .repo()
            .node_by_id(id)
            .await?
            .ok_or(FileError::NoSuchNode)?;
        if node.kind == KIND_ALIAS {
            if let Some(target) = node.target_id {
                return self
                    .repo()
                    .node_by_id(target)
                    .await?
                    .ok_or(FileError::NoSuchNode);
            }
        }
        Ok(node)
    }

    pub async fn delete(&self, id: i64) -> Result<(), FileError> {
        if !self.repo().delete_node(id).await? {
            return Err(FileError::NoSuchNode);
        }
        Ok(())
    }

    pub async fn set_metadata(
        &self,
        id: i64,
        icon: &str,
        comment: &str,
    ) -> Result<FileNodeRow, FileError> {
        if !self.repo().set_metadata(id, icon, comment).await? {
            return Err(FileError::NoSuchNode);
        }
        self.repo()
            .node_by_id(id)
            .await?
            .ok_or(FileError::NoSuchNode)
    }

    /// Record a download against a file (following aliases). Returns the
    /// resolved file row (with the bumped count) so the handler can stream
    /// its blob.
    pub async fn record_download(&self, id: i64) -> Result<FileNodeRow, FileError> {
        let file = self.resolve(id).await?;
        if file.kind != KIND_FILE {
            return Err(FileError::NotAFile);
        }
        self.repo().bump_download(file.id).await?;
        self.repo()
            .node_by_id(file.id)
            .await?
            .ok_or(FileError::NoSuchNode)
    }

    pub async fn rate(
        &self,
        id: i64,
        account_id: i64,
        stars: u8,
    ) -> Result<FileNodeRow, FileError> {
        let file = self.resolve(id).await?;
        if file.kind != KIND_FILE {
            return Err(FileError::NotAFile);
        }
        self.repo().rate(file.id, account_id, stars).await?;
        self.repo()
            .node_by_id(file.id)
            .await?
            .ok_or(FileError::NoSuchNode)
    }

    pub async fn search(
        &self,
        area_slug: Option<&str>,
        query: &str,
        limit: i64,
    ) -> Result<Vec<FileNodeRow>, FileError> {
        let area_id = match area_slug {
            Some(slug) => Some(self.area(slug).await?.id),
            None => None,
        };
        Ok(self.repo().search(area_id, query, limit).await?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rabbithole_store_server::open_in_memory;

    async fn service() -> FileService {
        FileService::new(open_in_memory().await.unwrap())
    }

    #[tokio::test]
    async fn tree_paths_derive_from_parents() {
        let svc = service().await;
        svc.create_area("warez", "Warez", "").await.unwrap();
        let utils = svc.mkdir("warez", None, "utils", false).await.unwrap();
        assert_eq!(utils.path, "utils");
        let sub = svc
            .mkdir("warez", Some("utils"), "zip", false)
            .await
            .unwrap();
        assert_eq!(sub.path, "utils/zip");

        let f = svc
            .add_file(
                "warez",
                Some("utils/zip"),
                "a.lha",
                &[1u8; 32],
                10,
                "app/x",
                "disk",
                "hi",
                "u@h",
                1,
            )
            .await
            .unwrap();
        assert_eq!(f.path, "utils/zip/a.lha");

        // Listing the subfolder shows the file.
        let kids = svc.list("warez", Some("utils/zip")).await.unwrap();
        assert_eq!(kids.len(), 1);
        assert_eq!(kids[0].name, "a.lha");
    }

    #[tokio::test]
    async fn duplicate_and_bad_names_rejected() {
        let svc = service().await;
        svc.create_area("a", "A", "").await.unwrap();
        svc.mkdir("a", None, "dup", false).await.unwrap();
        assert!(matches!(
            svc.mkdir("a", None, "dup", false).await,
            Err(FileError::Exists)
        ));
        assert!(matches!(
            svc.mkdir("a", None, "bad/name", false).await,
            Err(FileError::BadName)
        ));
        assert!(matches!(
            svc.mkdir("a", None, "  ", false).await,
            Err(FileError::BadName)
        ));
        // Can't nest under a file.
        svc.add_file("a", None, "f", &[1u8; 32], 1, "", "", "", "u@h", 1)
            .await
            .unwrap();
        assert!(matches!(
            svc.mkdir("a", Some("f"), "x", false).await,
            Err(FileError::NotAFolder)
        ));
    }

    #[tokio::test]
    async fn download_follows_alias_and_counts() {
        let svc = service().await;
        svc.create_area("a", "A", "").await.unwrap();
        let f = svc
            .add_file("a", None, "real", &[9u8; 32], 5, "", "", "", "u@h", 1)
            .await
            .unwrap();
        let alias = svc.add_alias("a", None, "link", "real").await.unwrap();
        // Downloading the alias records against and returns the real file.
        let served = svc.record_download(alias.id).await.unwrap();
        assert_eq!(served.id, f.id);
        assert_eq!(served.blob_id, Some([9u8; 32]));
        assert_eq!(served.downloads, 1);
    }
}
