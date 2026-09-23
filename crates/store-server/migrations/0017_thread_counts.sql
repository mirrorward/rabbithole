-- 0.266.0: counting a board's threads without reading its posts.
--
-- A thread is a post with no parent. The console asks what every board
-- keeps and how many threads it holds, and `posts_board` is (board_slug,
-- created_at), so counting roots meant fetching every post row of every
-- board to look at `parent_id`. This index holds the roots alone.
CREATE INDEX posts_roots ON posts(board_slug) WHERE parent_id IS NULL;
