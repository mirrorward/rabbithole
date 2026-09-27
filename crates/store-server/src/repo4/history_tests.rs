use super::*;

fn id(n: u16) -> [u8; 32] {
    let mut value = [0; 32];
    value[30..].copy_from_slice(&n.to_be_bytes());
    value
}

#[tokio::test]
async fn history_pages_are_bounded_and_complete_across_ties_and_ineligible_boards() {
    let pool = crate::open_in_memory().await.unwrap();
    let boards = BoardsRepo(&pool);
    boards
        .create("Alpha", "Alpha", "", 2, None, 1)
        .await
        .unwrap();
    boards
        .create("category", "Category", "", 0, None, 1)
        .await
        .unwrap();
    let posts = PostsRepo(&pool);
    let followups = FollowupsRepo(&pool);
    for n in 1..=300 {
        let board = match n % 3 {
            0 => "ALPHA",
            1 => "category",
            _ => "removed",
        };
        posts
            .insert(&PostRow {
                event_id: id(n),
                board_slug: board.into(),
                root_id: Some(id(n)),
                parent_id: None,
                author: "alice@home".into(),
                subject: "history".into(),
                body: "body".into(),
                mime: "text/plain".into(),
                // Deliberately opposite content-ID order: timestamps must not
                // choose the cursor, including when remote times are untrusted.
                created_at: i64::from(301 - n),
                edited: false,
                tombstoned: n % 2 == 0,
                event_blob: vec![0; 1024],
            })
            .await
            .unwrap();
        followups
            .insert(&FollowupRow {
                event_id: id(n),
                target_id: id(n),
                root_id: id(n),
                board_slug: board.into(),
                kind: 1,
                origin: "home".into(),
                applied: n % 2 == 0,
                created_at: -i64::from(n),
                event_blob: vec![1; 1024],
            })
            .await
            .unwrap();
    }
    assert!(posts.history_page(None, 0).await.unwrap().is_empty());
    assert_eq!(
        posts.history_page(None, usize::MAX).await.unwrap().len(),
        256
    );
    let mut cursor = None;
    let mut collected = Vec::new();
    for expected_count in [256, 256, 88, 0] {
        let page = posts.history_page(cursor, usize::MAX).await.unwrap();
        assert_eq!(page.len(), expected_count);
        for row in &page {
            assert!(cursor.is_none_or(|previous| row.cursor > previous));
            let number = u16::from_be_bytes(row.cursor.event_id[30..].try_into().unwrap());
            assert_eq!(row.root_id, id(number));
            assert_eq!(row.target_id, id(number));
            match number % 3 {
                0 => {
                    assert_eq!(row.board, "Alpha");
                    assert!(row.postable);
                }
                1 => {
                    assert_eq!(row.board, "category");
                    assert!(!row.postable);
                }
                _ => {
                    assert_eq!(row.board, "removed");
                    assert!(!row.postable);
                }
            }
            cursor = Some(row.cursor);
            collected.push(row.cursor);
        }
    }
    let expected: Vec<_> = (1..=300)
        .flat_map(|n| {
            [
                HistoryCursor {
                    event_id: id(n),
                    kind: 0,
                },
                HistoryCursor {
                    event_id: id(n),
                    kind: 1,
                },
            ]
        })
        .collect();
    assert_eq!(collected, expected, "neither table can lose a same-ID tie");
    let after_post = posts
        .history_page(
            Some(HistoryCursor {
                event_id: id(128),
                kind: 0,
            }),
            1,
        )
        .await
        .unwrap();
    assert_eq!(
        after_post[0].cursor,
        HistoryCursor {
            event_id: id(128),
            kind: 1
        }
    );
    assert_eq!(
        posts.history_page(None, 1).await.unwrap()[0].cursor,
        expected[0],
        "wrapping starts at the first position again"
    );
}
