//! Wave 4.1 repository: file libraries — areas, the folder/file/alias tree,
//! ratings, and search. Bytes live in the blob store; these rows are the
//! browsable projection.

use sqlx::Row;

use crate::{SqlitePool, StoreError};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileAreaRow {
    pub id: i64,
    pub slug: String,
    pub title: String,
    pub description: String,
    pub created_at: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FileNodeRow {
    pub id: i64,
    pub area_id: i64,
    /// The owning area's slug (joined in for display/federation).
    pub area: String,
    pub parent_id: Option<i64>,
    /// 0 folder, 1 file, 2 alias.
    pub kind: u8,
    pub name: String,
    pub path: String,
    pub is_dropbox: bool,
    pub blob_id: Option<[u8; 32]>,
    pub size: i64,
    pub mime: String,
    pub icon: String,
    pub comment: String,
    pub uploader: String,
    pub uploader_id: Option<i64>,
    pub downloads: i64,
    pub target_id: Option<i64>,
    pub created_at: i64,
    pub rating_avg: f64,
    pub rating_count: i64,
}

fn opt_id(bytes: Option<Vec<u8>>) -> Option<[u8; 32]> {
    bytes.and_then(|b| b.try_into().ok())
}

fn row_to_area(r: &sqlx::sqlite::SqliteRow) -> FileAreaRow {
    FileAreaRow {
        id: r.get("id"),
        slug: r.get("slug"),
        title: r.get("title"),
        description: r.get("description"),
        created_at: r.get("created_at"),
    }
}

fn row_to_node(r: &sqlx::sqlite::SqliteRow) -> FileNodeRow {
    FileNodeRow {
        id: r.get("id"),
        area_id: r.get("area_id"),
        area: r.get("area_slug"),
        parent_id: r.get("parent_id"),
        kind: r.get::<i64, _>("kind") as u8,
        name: r.get("name"),
        path: r.get("path"),
        is_dropbox: r.get::<i64, _>("is_dropbox") != 0,
        blob_id: opt_id(r.get("blob_id")),
        size: r.get("size"),
        mime: r.get("mime"),
        icon: r.get("icon"),
        comment: r.get("comment"),
        uploader: r.get("uploader"),
        uploader_id: r.get("uploader_id"),
        downloads: r.get("downloads"),
        target_id: r.get("target_id"),
        created_at: r.get("created_at"),
        rating_avg: r.try_get("rating_avg").unwrap_or(0.0),
        rating_count: r.try_get("rating_count").unwrap_or(0),
    }
}

/// Reusable node projection: base columns plus a rating average/count so a
/// single read carries display-ready metadata.
const NODE_SELECT: &str = "SELECT n.*, a.slug AS area_slug,
    COALESCE((SELECT AVG(stars) FROM file_ratings r WHERE r.node_id = n.id), 0.0) AS rating_avg,
    (SELECT COUNT(*) FROM file_ratings r WHERE r.node_id = n.id) AS rating_count
    FROM file_nodes n JOIN file_areas a ON a.id = n.area_id";

pub struct FilesRepo<'a>(pub &'a SqlitePool);

impl FilesRepo<'_> {
    // ---- Areas -----------------------------------------------------------

    pub async fn create_area(
        &self,
        slug: &str,
        title: &str,
        description: &str,
    ) -> Result<FileAreaRow, StoreError> {
        let id: i64 = sqlx::query(
            "INSERT INTO file_areas (slug, title, description, created_at)
             VALUES (?, ?, ?, unixepoch()) RETURNING id",
        )
        .bind(slug)
        .bind(title)
        .bind(description)
        .fetch_one(self.0)
        .await?
        .get("id");
        Ok(self.area_by_id(id).await?.expect("just inserted"))
    }

    pub async fn area_by_id(&self, id: i64) -> Result<Option<FileAreaRow>, StoreError> {
        Ok(sqlx::query("SELECT * FROM file_areas WHERE id = ?")
            .bind(id)
            .fetch_optional(self.0)
            .await?
            .map(|r| row_to_area(&r)))
    }

    pub async fn area_by_slug(&self, slug: &str) -> Result<Option<FileAreaRow>, StoreError> {
        Ok(sqlx::query("SELECT * FROM file_areas WHERE slug = ?")
            .bind(slug)
            .fetch_optional(self.0)
            .await?
            .map(|r| row_to_area(&r)))
    }

    pub async fn areas(&self) -> Result<Vec<FileAreaRow>, StoreError> {
        let rows = sqlx::query("SELECT * FROM file_areas ORDER BY slug")
            .fetch_all(self.0)
            .await?;
        Ok(rows.iter().map(row_to_area).collect())
    }

    // ---- Nodes -----------------------------------------------------------

    #[allow(clippy::too_many_arguments)]
    pub async fn create_folder(
        &self,
        area_id: i64,
        parent_id: Option<i64>,
        name: &str,
        path: &str,
        is_dropbox: bool,
    ) -> Result<FileNodeRow, StoreError> {
        let id: i64 = sqlx::query(
            "INSERT INTO file_nodes (area_id, parent_id, kind, name, path, is_dropbox, created_at)
             VALUES (?, ?, 0, ?, ?, ?, unixepoch()) RETURNING id",
        )
        .bind(area_id)
        .bind(parent_id)
        .bind(name)
        .bind(path)
        .bind(is_dropbox as i64)
        .fetch_one(self.0)
        .await?
        .get("id");
        Ok(self.node_by_id(id).await?.expect("just inserted"))
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn create_file(
        &self,
        area_id: i64,
        parent_id: Option<i64>,
        name: &str,
        path: &str,
        blob_id: &[u8; 32],
        size: i64,
        mime: &str,
        icon: &str,
        comment: &str,
        uploader: &str,
        uploader_id: i64,
    ) -> Result<FileNodeRow, StoreError> {
        let id: i64 = sqlx::query(
            "INSERT INTO file_nodes
                 (area_id, parent_id, kind, name, path, blob_id, size, mime, icon,
                  comment, uploader, uploader_id, created_at)
             VALUES (?, ?, 1, ?, ?, ?, ?, ?, ?, ?, ?, ?, unixepoch()) RETURNING id",
        )
        .bind(area_id)
        .bind(parent_id)
        .bind(name)
        .bind(path)
        .bind(&blob_id[..])
        .bind(size)
        .bind(mime)
        .bind(icon)
        .bind(comment)
        .bind(uploader)
        .bind(uploader_id)
        .fetch_one(self.0)
        .await?
        .get("id");
        Ok(self.node_by_id(id).await?.expect("just inserted"))
    }

    pub async fn create_alias(
        &self,
        area_id: i64,
        parent_id: Option<i64>,
        name: &str,
        path: &str,
        target_id: i64,
    ) -> Result<FileNodeRow, StoreError> {
        let id: i64 = sqlx::query(
            "INSERT INTO file_nodes (area_id, parent_id, kind, name, path, target_id, created_at)
             VALUES (?, ?, 2, ?, ?, ?, unixepoch()) RETURNING id",
        )
        .bind(area_id)
        .bind(parent_id)
        .bind(name)
        .bind(path)
        .bind(target_id)
        .fetch_one(self.0)
        .await?
        .get("id");
        Ok(self.node_by_id(id).await?.expect("just inserted"))
    }

    pub async fn node_by_id(&self, id: i64) -> Result<Option<FileNodeRow>, StoreError> {
        let sql = format!("{NODE_SELECT} WHERE n.id = ?");
        Ok(sqlx::query(&sql)
            .bind(id)
            .fetch_optional(self.0)
            .await?
            .map(|r| row_to_node(&r)))
    }

    pub async fn node_by_path(
        &self,
        area_id: i64,
        path: &str,
    ) -> Result<Option<FileNodeRow>, StoreError> {
        let sql = format!("{NODE_SELECT} WHERE n.area_id = ? AND n.path = ?");
        Ok(sqlx::query(&sql)
            .bind(area_id)
            .bind(path)
            .fetch_optional(self.0)
            .await?
            .map(|r| row_to_node(&r)))
    }

    /// Direct children of a folder (`parent_id` None = the area root),
    /// folders first then files/aliases, each alphabetically.
    pub async fn children(
        &self,
        area_id: i64,
        parent_id: Option<i64>,
    ) -> Result<Vec<FileNodeRow>, StoreError> {
        let order = "ORDER BY n.kind = 0 DESC, n.name COLLATE NOCASE";
        let rows = match parent_id {
            Some(pid) => {
                let sql = format!("{NODE_SELECT} WHERE n.area_id = ? AND n.parent_id = ? {order}");
                sqlx::query(&sql)
                    .bind(area_id)
                    .bind(pid)
                    .fetch_all(self.0)
                    .await?
            }
            None => {
                let sql =
                    format!("{NODE_SELECT} WHERE n.area_id = ? AND n.parent_id IS NULL {order}");
                sqlx::query(&sql).bind(area_id).fetch_all(self.0).await?
            }
        };
        Ok(rows.iter().map(row_to_node).collect())
    }

    pub async fn delete_node(&self, id: i64) -> Result<bool, StoreError> {
        Ok(sqlx::query("DELETE FROM file_nodes WHERE id = ?")
            .bind(id)
            .execute(self.0)
            .await?
            .rows_affected()
            > 0)
    }

    pub async fn set_metadata(
        &self,
        id: i64,
        icon: &str,
        comment: &str,
    ) -> Result<bool, StoreError> {
        Ok(
            sqlx::query("UPDATE file_nodes SET icon = ?, comment = ? WHERE id = ?")
                .bind(icon)
                .bind(comment)
                .bind(id)
                .execute(self.0)
                .await?
                .rows_affected()
                > 0,
        )
    }

    /// Bump a file's download counter; returns the new total.
    pub async fn bump_download(&self, id: i64) -> Result<i64, StoreError> {
        sqlx::query("UPDATE file_nodes SET downloads = downloads + 1 WHERE id = ?")
            .bind(id)
            .execute(self.0)
            .await?;
        Ok(sqlx::query("SELECT downloads FROM file_nodes WHERE id = ?")
            .bind(id)
            .fetch_one(self.0)
            .await?
            .get("downloads"))
    }

    /// Rate a node 1..5 (idempotent per account — re-rating overwrites).
    pub async fn rate(&self, node_id: i64, account_id: i64, stars: u8) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO file_ratings (node_id, account_id, stars) VALUES (?, ?, ?)
             ON CONFLICT(node_id, account_id) DO UPDATE SET stars = excluded.stars",
        )
        .bind(node_id)
        .bind(account_id)
        .bind(stars.clamp(1, 5) as i64)
        .execute(self.0)
        .await?;
        Ok(())
    }

    /// Search files (kind = 1) by name/comment/uploader substring, optionally
    /// scoped to one area, newest first.
    pub async fn search(
        &self,
        area_id: Option<i64>,
        query: &str,
        limit: i64,
    ) -> Result<Vec<FileNodeRow>, StoreError> {
        let like = format!("%{}%", query.replace(['%', '_'], ""));
        let rows = match area_id {
            Some(aid) => {
                let sql = format!(
                    "{NODE_SELECT} WHERE n.kind = 1 AND n.area_id = ?
                     AND (n.name LIKE ?2 OR n.comment LIKE ?2 OR n.uploader LIKE ?2)
                     ORDER BY n.created_at DESC LIMIT ?3"
                );
                sqlx::query(&sql)
                    .bind(aid)
                    .bind(&like)
                    .bind(limit)
                    .fetch_all(self.0)
                    .await?
            }
            None => {
                let sql = format!(
                    "{NODE_SELECT} WHERE n.kind = 1
                     AND (n.name LIKE ?1 OR n.comment LIKE ?1 OR n.uploader LIKE ?1)
                     ORDER BY n.created_at DESC LIMIT ?2"
                );
                sqlx::query(&sql)
                    .bind(&like)
                    .bind(limit)
                    .fetch_all(self.0)
                    .await?
            }
        };
        Ok(rows.iter().map(row_to_node).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::open_in_memory;

    #[tokio::test]
    async fn area_tree_files_and_metadata() {
        let pool = open_in_memory().await.unwrap();
        let repo = FilesRepo(&pool);
        let area = repo
            .create_area("warez", "Warez", "the good stuff")
            .await
            .unwrap();
        let utils = repo
            .create_folder(area.id, None, "utils", "utils", false)
            .await
            .unwrap();
        assert_eq!(utils.kind, 0);

        let file = repo
            .create_file(
                area.id,
                Some(utils.id),
                "zip.lha",
                "utils/zip.lha",
                &[7u8; 32],
                1024,
                "application/x-lha",
                "disk",
                "the classic",
                "alice@home",
                1,
            )
            .await
            .unwrap();
        assert_eq!(file.kind, 1);
        assert_eq!(file.blob_id, Some([7u8; 32]));
        assert_eq!(file.size, 1024);

        // Children: root shows the folder; folder shows the file.
        let root = repo.children(area.id, None).await.unwrap();
        assert_eq!(root.len(), 1);
        assert_eq!(root[0].name, "utils");
        let kids = repo.children(area.id, Some(utils.id)).await.unwrap();
        assert_eq!(kids.len(), 1);
        assert_eq!(kids[0].name, "zip.lha");

        // Download counter + metadata edit.
        assert_eq!(repo.bump_download(file.id).await.unwrap(), 1);
        assert_eq!(repo.bump_download(file.id).await.unwrap(), 2);
        repo.set_metadata(file.id, "star", "even better")
            .await
            .unwrap();
        let f = repo.node_by_id(file.id).await.unwrap().unwrap();
        assert_eq!(f.downloads, 2);
        assert_eq!(f.comment, "even better");
        assert_eq!(f.icon, "star");
    }

    #[tokio::test]
    async fn ratings_average_is_honest() {
        let pool = open_in_memory().await.unwrap();
        let repo = FilesRepo(&pool);
        let area = repo.create_area("a", "A", "").await.unwrap();
        let file = repo
            .create_file(area.id, None, "f", "f", &[1u8; 32], 1, "", "", "", "u@h", 1)
            .await
            .unwrap();
        repo.rate(file.id, 10, 5).await.unwrap();
        repo.rate(file.id, 11, 3).await.unwrap();
        repo.rate(file.id, 10, 4).await.unwrap(); // re-rate lowers 10's vote
        let f = repo.node_by_id(file.id).await.unwrap().unwrap();
        assert_eq!(f.rating_count, 2);
        assert!((f.rating_avg - 3.5).abs() < 1e-9, "avg of 4 and 3");
    }

    #[tokio::test]
    async fn search_matches_name_and_comment() {
        let pool = open_in_memory().await.unwrap();
        let repo = FilesRepo(&pool);
        let a = repo.create_area("a", "A", "").await.unwrap();
        repo.create_file(
            a.id,
            None,
            "readme.txt",
            "readme.txt",
            &[1u8; 32],
            1,
            "",
            "",
            "install notes",
            "u@h",
            1,
        )
        .await
        .unwrap();
        repo.create_file(
            a.id, None, "game.exe", "game.exe", &[2u8; 32], 1, "", "", "fun", "u@h", 1,
        )
        .await
        .unwrap();
        assert_eq!(repo.search(None, "readme", 10).await.unwrap().len(), 1);
        assert_eq!(
            repo.search(None, "install", 10).await.unwrap().len(),
            1,
            "comment match"
        );
        assert_eq!(repo.search(Some(a.id), "e", 10).await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn aliases_point_at_a_target() {
        let pool = open_in_memory().await.unwrap();
        let repo = FilesRepo(&pool);
        let a = repo.create_area("a", "A", "").await.unwrap();
        let file = repo
            .create_file(
                a.id, None, "orig", "orig", &[1u8; 32], 1, "", "", "", "u@h", 1,
            )
            .await
            .unwrap();
        let alias = repo
            .create_alias(a.id, None, "shortcut", "shortcut", file.id)
            .await
            .unwrap();
        assert_eq!(alias.kind, 2);
        assert_eq!(alias.target_id, Some(file.id));

        // Deleting the file cascades the alias away (FK ON DELETE CASCADE).
        assert!(repo.delete_node(file.id).await.unwrap());
        assert!(repo.node_by_id(alias.id).await.unwrap().is_none());
    }
}
