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

    /// Change what an area is called and says about itself. The slug stays.
    /// Returns whether there was such an area.
    pub async fn update_area(
        &self,
        slug: &str,
        title: &str,
        description: &str,
    ) -> Result<bool, StoreError> {
        Ok(
            sqlx::query("UPDATE file_areas SET title = ?, description = ? WHERE slug = ?")
                .bind(title)
                .bind(description)
                .bind(slug)
                .execute(self.0)
                .await?
                .rows_affected()
                > 0,
        )
    }

    /// How many nodes (folders, files, aliases) an area holds.
    pub async fn area_node_count(&self, area_id: i64) -> Result<i64, StoreError> {
        Ok(
            sqlx::query("SELECT COUNT(*) AS n FROM file_nodes WHERE area_id = ?")
                .bind(area_id)
                .fetch_one(self.0)
                .await?
                .get("n"),
        )
    }

    /// Remove an area. The caller has made sure it is empty.
    pub async fn delete_area(&self, area_id: i64) -> Result<bool, StoreError> {
        Ok(sqlx::query("DELETE FROM file_areas WHERE id = ?")
            .bind(area_id)
            .execute(self.0)
            .await?
            .rows_affected()
            > 0)
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

    /// Atomically validate an upload's pinned destination and insert without
    /// overwriting an occupied path. No earlier lookup grants publication:
    /// both the area and folder must still match at this statement's write.
    #[allow(clippy::too_many_arguments)]
    pub async fn create_file_if_target(
        &self,
        area_id: i64,
        parent_id: Option<i64>,
        area_slug: &str,
        parent_path: &str,
        name: &str,
        path: &str,
        blob_id: &[u8; 32],
        size: i64,
        mime: &str,
        icon: &str,
        comment: &str,
        uploader: &str,
        uploader_id: i64,
    ) -> Result<Option<i64>, StoreError> {
        Ok(sqlx::query(
            "INSERT INTO file_nodes
                 (area_id, parent_id, kind, name, path, blob_id, size, mime, icon,
                  comment, uploader, uploader_id, created_at)
             SELECT ?1, ?2, 1, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, unixepoch()
             WHERE EXISTS (SELECT 1 FROM file_areas WHERE id = ?1 AND slug = ?12)
               AND ((?2 IS NULL AND ?13 = '') OR EXISTS (
                    SELECT 1 FROM file_nodes
                    WHERE id = ?2 AND area_id = ?1 AND kind = 0 AND path = ?13))
             ON CONFLICT (area_id, path) DO NOTHING
             RETURNING id",
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
        .bind(area_slug)
        .bind(parent_path)
        .fetch_optional(self.0)
        .await?
        .map(|row| row.get("id")))
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

    /// Every file node holding this content, newest first, at most `limit`:
    /// one blob can be filed in several places (and several areas). The
    /// caller decides which of them the person asking may have.
    pub async fn nodes_with_blob(
        &self,
        blob_id: &[u8; 32],
        limit: i64,
    ) -> Result<Vec<FileNodeRow>, StoreError> {
        self.nodes_with_blob_after(blob_id, 0, limit).await
    }

    /// The same, one page at a time: `skip` rows in, at most `limit` rows,
    /// in the same order. For walking past copies the asker may not have
    /// without reading every one of them at once.
    pub async fn nodes_with_blob_after(
        &self,
        blob_id: &[u8; 32],
        skip: i64,
        limit: i64,
    ) -> Result<Vec<FileNodeRow>, StoreError> {
        let sql = format!(
            "{NODE_SELECT} WHERE n.blob_id = ? AND n.kind = 1 ORDER BY n.id DESC LIMIT ? OFFSET ?"
        );
        Ok(sqlx::query(&sql)
            .bind(&blob_id[..])
            .bind(limit)
            .bind(skip)
            .fetch_all(self.0)
            .await?
            .iter()
            .map(row_to_node)
            .collect())
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

    /// Move and/or rename a node in one transaction: its own row, then the
    /// path of everything below it (`old_path/…` becomes `new_path/…`).
    /// Returns whether the node existed.
    pub async fn relocate_node(
        &self,
        id: i64,
        area_id: i64,
        parent_id: Option<i64>,
        name: &str,
        old_path: &str,
        new_path: &str,
    ) -> Result<bool, StoreError> {
        let mut tx = self.0.begin().await?;
        let moved = sqlx::query(
            "UPDATE file_nodes SET parent_id = ?, name = ?, path = ? WHERE id = ? AND area_id = ?",
        )
        .bind(parent_id)
        .bind(name)
        .bind(new_path)
        .bind(id)
        .bind(area_id)
        .execute(&mut *tx)
        .await?
        .rows_affected()
            > 0;
        if moved {
            // SQLite's substr counts characters, so the prefix length must too.
            let old_prefix = format!("{old_path}/");
            let new_prefix = format!("{new_path}/");
            let chars = old_prefix.chars().count() as i64;
            sqlx::query(
                "UPDATE file_nodes SET path = ? || substr(path, ?)
                 WHERE area_id = ? AND substr(path, 1, ?) = ?",
            )
            .bind(&new_prefix)
            .bind(chars + 1)
            .bind(area_id)
            .bind(chars)
            .bind(&old_prefix)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(moved)
    }

    /// Record where a pulled file came from: the source burrow's name and
    /// server key.
    pub async fn set_provenance(
        &self,
        id: i64,
        burrow: &str,
        key: &[u8; 32],
    ) -> Result<bool, StoreError> {
        Ok(
            sqlx::query("UPDATE file_nodes SET source_burrow = ?, source_key = ? WHERE id = ?")
                .bind(burrow)
                .bind(key.as_slice())
                .bind(id)
                .execute(self.0)
                .await?
                .rows_affected()
                > 0,
        )
    }

    /// Spend a pull grant's nonce: `false` when it was spent before and its
    /// grant has not lapsed. Lapsed ones are forgotten on the way.
    pub async fn spend_pull_grant(
        &self,
        nonce: &[u8; 16],
        expires_unix: i64,
        now_unix: i64,
    ) -> Result<bool, StoreError> {
        sqlx::query("DELETE FROM s2s_spent_grants WHERE expires_unix <= ?")
            .bind(now_unix)
            .execute(self.0)
            .await?;
        let inserted = sqlx::query(
            "INSERT OR IGNORE INTO s2s_spent_grants (nonce, expires_unix) VALUES (?, ?)",
        )
        .bind(nonce.as_slice())
        .bind(expires_unix)
        .execute(self.0)
        .await?
        .rows_affected();
        Ok(inserted == 1)
    }

    /// Where a file came from, if it was pulled from another burrow.
    pub async fn provenance(&self, id: i64) -> Result<Option<(String, [u8; 32])>, StoreError> {
        let row = sqlx::query("SELECT source_burrow, source_key FROM file_nodes WHERE id = ?")
            .bind(id)
            .fetch_optional(self.0)
            .await?;
        Ok(row.and_then(|r| {
            let burrow: String = r.get("source_burrow");
            let key: Option<Vec<u8>> = r.get("source_key");
            let key: [u8; 32] = key?.try_into().ok()?;
            (!burrow.is_empty()).then_some((burrow, key))
        }))
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

    /// Total bytes an account has uploaded across all libraries (files only)
    /// — the figure a storage quota is checked against.
    pub async fn uploaded_bytes(&self, account_id: i64) -> Result<i64, StoreError> {
        Ok(sqlx::query(
            "SELECT COALESCE(SUM(size), 0) AS total FROM file_nodes
             WHERE kind = 1 AND uploader_id = ?",
        )
        .bind(account_id)
        .fetch_one(self.0)
        .await?
        .get("total"))
    }

    /// Search files by name/comment/uploader substring, newest first.
    /// Callers needing authorization before the result limit use search_page.
    pub async fn search(
        &self,
        area_id: Option<i64>,
        query: &str,
        limit: i64,
    ) -> Result<Vec<FileNodeRow>, StoreError> {
        self.search_candidates(area_id, query, limit, None).await
    }

    /// A bounded candidate page in (created_at DESC, id DESC) order. The
    /// cursor is the last *examined* row, including invisible matches. This
    /// lets callers fill a visible result limit without loading every match
    /// or letting hidden rows consume it. No OFFSET drift on deletions.
    pub async fn search_page(
        &self,
        area_id: Option<i64>,
        query: &str,
        limit: i64,
        before: Option<(i64, i64)>,
    ) -> Result<Vec<FileNodeRow>, StoreError> {
        self.search_candidates(area_id, query, limit.clamp(1, 200), before)
            .await
    }

    async fn search_candidates(
        &self,
        area_id: Option<i64>,
        query: &str,
        limit: i64,
        before: Option<(i64, i64)>,
    ) -> Result<Vec<FileNodeRow>, StoreError> {
        // Keep the old literal-substring contract, including stripping LIKE
        // wildcards. No user input is interpreted as FTS MATCH syntax. LIKE
        // retains its ASCII case-folding; short queries still work by scan.
        let like = format!("%{}%", query.replace(['%', '_'], ""));
        // Separate indexed branches matter: an OR across FTS columns falls
        // back to scanning the virtual table. UNION also deduplicates a file
        // whose name, comment and uploader all match.
        let sql = format!(
            "{NODE_SELECT} WHERE n.kind = 1
             AND (?1 IS NULL OR n.area_id = ?1)
             AND (?3 IS NULL OR (n.created_at, n.id) < (?3, ?4))
             AND n.id IN (
                 SELECT rowid FROM file_search WHERE name LIKE ?2
                 UNION SELECT rowid FROM file_search WHERE comment LIKE ?2
                 UNION SELECT rowid FROM file_search WHERE uploader LIKE ?2
             )
             ORDER BY n.created_at DESC, n.id DESC LIMIT ?5"
        );
        let rows = sqlx::query(&sql)
            .bind(area_id)
            .bind(like)
            .bind(before.map(|c| c.0))
            .bind(before.map(|c| c.1))
            .bind(limit)
            .fetch_all(self.0)
            .await?;
        Ok(rows.iter().map(row_to_node).collect())
    }

    /// Reads may reach nested descendants without opening their folders.
    /// Walk every ancestor, not just the immediate parent. UNION makes even
    /// a malformed cyclic tree finite. A missing node or a chain which never
    /// reaches a root produces RowNotFound, rather than a public result.
    pub async fn has_dropbox_ancestor(&self, node_id: i64) -> Result<bool, StoreError> {
        let row = sqlx::query(
            "WITH RECURSIVE parents(id, parent_id, is_dropbox) AS (
                 SELECT p.id, p.parent_id, p.is_dropbox FROM file_nodes n
                 JOIN file_nodes p ON p.id = n.parent_id WHERE n.id = ?1
                 UNION
                 SELECT p.id, p.parent_id, p.is_dropbox FROM file_nodes p
                 JOIN parents c ON p.id = c.parent_id
             ) SELECT EXISTS(SELECT 1 FROM parents WHERE is_dropbox != 0) AS hidden
               FROM file_nodes n WHERE n.id = ?1
               AND (n.parent_id IS NULL OR
                    EXISTS(SELECT 1 FROM parents WHERE parent_id IS NULL))",
        )
        .bind(node_id)
        .fetch_one(self.0)
        .await?;
        Ok(row.get("hidden"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::open_in_memory;

    #[tokio::test]
    async fn ancestry_rejects_missing_nodes_and_cycles() {
        let pool = open_in_memory().await.unwrap();
        let repo = FilesRepo(&pool);
        let area = repo.create_area("a", "A", "").await.unwrap();
        let a = repo
            .create_folder(area.id, None, "a", "a", false)
            .await
            .unwrap();
        let b = repo
            .create_folder(area.id, Some(a.id), "b", "a/b", false)
            .await
            .unwrap();
        assert!(!repo.has_dropbox_ancestor(b.id).await.unwrap());
        assert!(repo.has_dropbox_ancestor(i64::MAX).await.is_err());
        sqlx::query("UPDATE file_nodes SET parent_id = ? WHERE id = ?")
            .bind(b.id)
            .bind(a.id)
            .execute(&pool)
            .await
            .unwrap();
        assert!(repo.has_dropbox_ancestor(b.id).await.is_err());
    }

    #[tokio::test]
    async fn one_content_is_found_wherever_it_is_filed() {
        let pool = open_in_memory().await.unwrap();
        let repo = FilesRepo(&pool);
        let music = repo.create_area("music", "Music", "").await.unwrap();
        let other = repo.create_area("demos", "Demos", "").await.unwrap();
        let shared = [3u8; 32];
        async fn file(pool: &sqlx::SqlitePool, area: i64, name: &str, blob: [u8; 32]) -> i64 {
            FilesRepo(pool)
                .create_file(
                    area,
                    None,
                    name,
                    name,
                    &blob,
                    12,
                    "application/octet-stream",
                    "",
                    "",
                    "x@y",
                    1,
                )
                .await
                .unwrap()
                .id
        }
        let first = file(&pool, music.id, "tape.bin", shared).await;
        let again = file(&pool, other.id, "same-tape.bin", shared).await;
        file(&pool, music.id, "other.bin", [9u8; 32]).await;
        repo.create_folder(music.id, None, "box", "box", false)
            .await
            .unwrap();

        // Both places the content is filed, newest first; nothing else.
        let found = repo.nodes_with_blob(&shared, 10).await.unwrap();
        assert_eq!(
            found.iter().map(|n| n.id).collect::<Vec<_>>(),
            vec![again, first]
        );
        assert!(found.iter().all(|n| n.blob_id == Some(shared)));
        // Content filed nowhere is found nowhere, and the limit holds.
        assert!(repo
            .nodes_with_blob(&[4u8; 32], 10)
            .await
            .unwrap()
            .is_empty());
        assert_eq!(repo.nodes_with_blob(&shared, 1).await.unwrap().len(), 1);
    }

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
