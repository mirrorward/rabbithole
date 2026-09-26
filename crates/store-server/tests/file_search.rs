//! RH-5: the index is part of the file projection, not an eventual cache.

use rabbithole_store_server::repo6::{FileNodeRow, FilesRepo};
use rabbithole_store_server::{open, open_in_memory, SqlitePool};
use sqlx::Row;

async fn add(pool: &SqlitePool, area: i64, name: &str, comment: &str) -> FileNodeRow {
    FilesRepo(pool)
        .create_file(
            area,
            None,
            name,
            name,
            &[7; 32],
            1,
            "",
            "",
            comment,
            "Alice@Home",
            1,
        )
        .await
        .unwrap()
}

fn ids(nodes: Vec<FileNodeRow>) -> Vec<i64> {
    nodes.into_iter().map(|n| n.id).collect()
}

async fn old_search(pool: &SqlitePool, area: Option<i64>, query: &str) -> Vec<i64> {
    sqlx::query_scalar(
        "SELECT id FROM file_nodes WHERE kind = 1 AND (?1 IS NULL OR area_id = ?1)
         AND (name LIKE ?2 OR comment LIKE ?2 OR uploader LIKE ?2)
         ORDER BY created_at DESC, id DESC LIMIT 200",
    )
    .bind(area)
    .bind(format!("%{}%", query.replace(['%', '_'], "")))
    .fetch_all(pool)
    .await
    .unwrap()
}

#[tokio::test]
async fn substring_queries_match_the_previous_search_and_page_without_duplicates() {
    let pool = open_in_memory().await.unwrap();
    let repo = FilesRepo(&pool);
    let a = repo.create_area("a", "A", "").await.unwrap();
    let b = repo.create_area("b", "B", "").await.unwrap();
    for name in [
        "readme.txt",
        "prefix-install-suffix",
        "100%_done.zip",
        "café.bin",
        "CAFÉ.bin",
        "quote\"AND OR.bin",
        "bracket[one].zip",
        "back\\slash.txt",
        "日本語.txt",
        "😀smile.txt",
    ] {
        add(&pool, a.id, name, "install notes; punctuation: é_日本語").await;
    }
    add(&pool, b.id, "elsewhere.zip", "other").await;
    repo.create_folder(a.id, None, "not-a-file", "not-a-file", false)
        .await
        .unwrap();
    let target = add(&pool, a.id, "target.bin", "target").await;
    repo.create_alias(
        a.id,
        None,
        "not-a-file-alias",
        "not-a-file-alias",
        target.id,
    )
    .await
    .unwrap();
    for query in [
        "",
        "e",
        "re",
        "read",
        "INSTALL",
        ".zip",
        "%_",
        "100%_done",
        "AND OR",
        "\"",
        "[one]",
        "\\",
        "café",
        "CAFÉ",
        "é",
        "日本",
        "日本語",
        "😀",
        "Alice@",
        "not-a-file",
        "no-match",
        "install\0ignored",
    ] {
        for area in [None, Some(a.id), Some(b.id)] {
            assert_eq!(
                ids(repo.search(area, query, 200).await.unwrap()),
                old_search(&pool, area, query).await,
                "query={query:?}, area={area:?}"
            );
        }
    }
    let mut before = None;
    let mut walked = Vec::new();
    loop {
        let page = repo.search_page(None, "", 2, before).await.unwrap();
        let Some(last) = page.last() else { break };
        before = Some((last.created_at, last.id));
        walked.extend(ids(page));
    }
    assert_eq!(walked, old_search(&pool, None, "").await);
}

#[tokio::test]
async fn an_existing_database_is_backfilled_and_stays_indexed_after_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("store.db");
    let pool = open(&path).await.unwrap();
    // Recreate the exact pre-0020 schema state, then store old-version rows.
    sqlx::raw_sql(
        "DROP TRIGGER file_search_insert; DROP TRIGGER file_search_delete;
         DROP TRIGGER file_search_update; DROP TABLE file_search;
         DROP VIEW file_search_content; DELETE FROM _sqlx_migrations WHERE version = 20;",
    )
    .execute(&pool)
    .await
    .unwrap();
    let a = FilesRepo(&pool)
        .create_area("old", "Old", "")
        .await
        .unwrap();
    let file = add(&pool, a.id, "old-file.txt", "before upgrade").await;
    pool.close().await;

    let pool = open(&path).await.unwrap();
    assert_eq!(
        ids(FilesRepo(&pool).search(None, "upgrade", 20).await.unwrap()),
        vec![file.id]
    );
    FilesRepo(&pool)
        .set_metadata(file.id, "", "after upgrade")
        .await
        .unwrap();
    pool.close().await;
    let pool = open(&path).await.unwrap();
    assert_eq!(
        ids(FilesRepo(&pool)
            .search(None, "after upgrade", 20)
            .await
            .unwrap()),
        vec![file.id]
    );
    assert!(FilesRepo(&pool)
        .search(None, "before", 20)
        .await
        .unwrap()
        .is_empty());
    pool.close().await;
}

#[tokio::test]
async fn metadata_rename_rollback_and_cascade_deletion_keep_the_index_consistent() {
    let pool = open_in_memory().await.unwrap();
    let repo = FilesRepo(&pool);
    let a = repo.create_area("a", "A", "").await.unwrap();
    let file = add(&pool, a.id, "original.bin", "before").await;
    repo.set_metadata(file.id, "icon", "after").await.unwrap();
    assert!(repo.search(None, "before", 10).await.unwrap().is_empty());
    assert_eq!(
        ids(repo.search(None, "after", 10).await.unwrap()),
        vec![file.id]
    );
    sqlx::query("UPDATE file_nodes SET name = 'renamed.bin', path = 'renamed.bin', uploader = 'NewSender' WHERE id = ?")
        .bind(file.id).execute(&pool).await.unwrap();
    assert!(repo.search(None, "original", 10).await.unwrap().is_empty());
    assert!(repo.search(None, "Alice", 10).await.unwrap().is_empty());
    assert_eq!(
        ids(repo.search(None, "NewSender", 10).await.unwrap()),
        vec![file.id]
    );

    let mut tx = pool.begin().await.unwrap();
    sqlx::query("UPDATE file_nodes SET name = 'rolled-back.bin' WHERE id = ?")
        .bind(file.id)
        .execute(&mut *tx)
        .await
        .unwrap();
    let indexed: i64 =
        sqlx::query_scalar("SELECT count(*) FROM file_search WHERE name LIKE '%rolled-back%'")
            .fetch_one(&mut *tx)
            .await
            .unwrap();
    assert_eq!(indexed, 1, "index participates in the same transaction");
    tx.rollback().await.unwrap();
    assert!(repo
        .search(None, "rolled-back", 10)
        .await
        .unwrap()
        .is_empty());
    assert_eq!(
        ids(repo.search(None, "renamed", 10).await.unwrap()),
        vec![file.id]
    );

    let folder = repo
        .create_folder(a.id, None, "drop", "drop", true)
        .await
        .unwrap();
    let nested = repo
        .create_folder(a.id, Some(folder.id), "nested", "drop/nested", false)
        .await
        .unwrap();
    let child = repo
        .create_file(
            a.id,
            Some(nested.id),
            "child.bin",
            "drop/nested/child.bin",
            &[8; 32],
            1,
            "",
            "",
            "",
            "",
            1,
        )
        .await
        .unwrap();
    assert!(repo.has_dropbox_ancestor(child.id).await.unwrap());
    assert!(!repo.has_dropbox_ancestor(file.id).await.unwrap());
    repo.delete_node(folder.id).await.unwrap();
    assert!(repo.search(None, "child", 10).await.unwrap().is_empty());
    repo.delete_area(a.id).await.unwrap();
    assert!(repo.search(None, "", 10).await.unwrap().is_empty());
    // rank=1 checks the external content against the index, not just its
    // internal b-trees. Ghost entries from cascade/rollback would fail this.
    sqlx::query("INSERT INTO file_search(file_search, rank) VALUES('integrity-check', 1)")
        .execute(&pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn indexed_selective_search_matches_a_bounded_scan_comparison() {
    let pool = open_in_memory().await.unwrap();
    let repo = FilesRepo(&pool);
    let a = repo.create_area("bench", "Bench", "").await.unwrap();
    sqlx::query(
        "WITH RECURSIVE seq(n) AS (VALUES(1) UNION ALL SELECT n+1 FROM seq WHERE n < 12000)
         INSERT INTO file_nodes(area_id, kind, name, path, comment, uploader, created_at)
         SELECT ?1, 1, printf('document-%05d.zip', n), printf('document-%05d.zip', n),
         CASE WHEN n % 997 = 0 THEN 'selective-needle-here' ELSE 'ordinary archived file' END,
         'uploader', n FROM seq",
    )
    .bind(a.id)
    .execute(&pool)
    .await
    .unwrap();
    let plan = sqlx::query(
        "EXPLAIN QUERY PLAN SELECT rowid FROM file_search WHERE comment LIKE '%needle%'",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert!(
        plan.iter()
            .any(|r| r.get::<String, _>("detail").contains("L1")),
        "trigram LIKE must have an indexed column constraint"
    );
    let expected = old_search(&pool, None, "needle").await;
    assert_eq!(expected.len(), 12);
    let began = std::time::Instant::now();
    for _ in 0..20 {
        assert_eq!(
            ids(repo.search(None, "needle", 200).await.unwrap()),
            expected
        );
    }
    let indexed = began.elapsed();
    let began = std::time::Instant::now();
    for _ in 0..20 {
        assert_eq!(old_search(&pool, None, "needle").await, expected);
    }
    // Timing is evidence, not a flaky CI threshold. Query-plan and result
    // assertions enforce the useful invariant on every supported platform.
    eprintln!(
        "RH-5 12000 rows / 20 searches: FTS5 {indexed:?}; previous LIKE {:?}",
        began.elapsed()
    );
}
