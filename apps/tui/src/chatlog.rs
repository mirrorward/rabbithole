//! The chat log's scrollback model: which slice of the buffer is on screen.
//!
//! The lobby pane used to map *every* line into a `List` and render it with no
//! state. Ratatui draws a stateless list from index 0, so a client that seeds
//! 50 lines of history showed the **oldest** 50 and every new message landed
//! below the fold, invisible — on any terminal shorter than the backlog. The
//! comment above it said "tail to fit", which is what it was supposed to do.
//!
//! So the window is computed here, as arithmetic, and tested. Two rules:
//!
//! * **Follow by default.** A chat log that doesn't show the newest line is
//!   broken; you should have to *choose* to leave the bottom.
//! * **Scrolling back pins.** Once you scroll up, incoming messages must not
//!   yank you away from what you're reading. Returning to the bottom resumes
//!   following.

/// How many lines the buffer keeps. Beyond this the oldest are dropped: a
/// terminal client is a window on a conversation, not an archive, and an
/// unbounded `Vec` in a long-lived session is just a slow leak.
pub const MAX_LINES: usize = 2_000;

/// The scrollback position for one log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Scroll {
    /// Lines scrolled up from the newest. `0` = pinned to the bottom.
    from_bottom: usize,
}

impl Default for Scroll {
    fn default() -> Self {
        Self::new()
    }
}

impl Scroll {
    pub const fn new() -> Self {
        Self { from_bottom: 0 }
    }

    /// Is the view pinned to the newest line (and therefore following)?
    pub fn at_bottom(&self) -> bool {
        self.from_bottom == 0
    }

    /// The window of `[first, last)` indices to render for a `len`-line buffer
    /// in a pane `height` rows tall.
    ///
    /// Clamped on every axis: a buffer shorter than the pane starts at 0, and
    /// a scroll position stranded by trimming (the buffer shrank under it)
    /// resolves to the oldest line rather than panicking on a bad range.
    pub fn window(&self, len: usize, height: usize) -> (usize, usize) {
        if len == 0 || height == 0 {
            return (0, 0);
        }
        let visible = height.min(len);
        let max_scroll = len - visible;
        let up = self.from_bottom.min(max_scroll);
        let first = max_scroll - up;
        (first, first + visible)
    }

    /// Scroll up (toward older lines) by `n`, stopping at the oldest.
    pub fn up(&mut self, n: usize, len: usize, height: usize) {
        let max_scroll = len.saturating_sub(height.min(len));
        self.from_bottom = (self.from_bottom + n).min(max_scroll);
    }

    /// Scroll down (toward newer lines) by `n`, stopping at — and re-pinning
    /// to — the bottom.
    pub fn down(&mut self, n: usize) {
        self.from_bottom = self.from_bottom.saturating_sub(n);
    }

    /// Jump to the oldest line.
    pub fn jump_top(&mut self, len: usize, height: usize) {
        self.from_bottom = len.saturating_sub(height.min(len));
    }

    /// Jump to the newest line and resume following.
    pub fn jump_bottom(&mut self) {
        self.from_bottom = 0;
    }

    /// Account for `n` new lines arriving.
    ///
    /// Following (at the bottom) stays at the bottom — that's the point.
    /// Scrolled back, the position shifts with the content so the line you
    /// were reading stays under your eyes instead of sliding away.
    pub fn on_appended(&mut self, n: usize, len_before: usize) {
        if self.from_bottom == 0 {
            return;
        }
        // Cap by what the buffer can actually hold: once trimming starts,
        // holding position for every appended line would walk off the top.
        let ceiling = len_before.min(MAX_LINES).saturating_sub(1);
        self.from_bottom = (self.from_bottom + n).min(ceiling);
    }
}

/// Push a line, trimming to [`MAX_LINES`]. Returns how many were dropped off
/// the front, so a scrolled-back view can compensate.
pub fn push_trimmed<T>(buf: &mut Vec<T>, line: T) -> usize {
    buf.push(line);
    if buf.len() > MAX_LINES {
        let excess = buf.len() - MAX_LINES;
        buf.drain(..excess);
        return excess;
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_log_shows_the_newest_lines_not_the_oldest() {
        // The shipped bug: 50 lines of seeded history in a 10-row pane
        // rendered lines 0..10 — the oldest — and every new message landed
        // out of sight below.
        let s = Scroll::new();
        assert_eq!(s.window(50, 10), (40, 50), "the last 10 lines");
        assert!(s.at_bottom());
    }

    #[test]
    fn a_buffer_shorter_than_the_pane_starts_at_the_top() {
        let s = Scroll::new();
        assert_eq!(s.window(3, 10), (0, 3));
        assert_eq!(s.window(0, 10), (0, 0), "empty log renders nothing");
        assert_eq!(s.window(50, 0), (0, 0), "no room, no window");
    }

    #[test]
    fn scrolling_up_walks_back_and_stops_at_the_oldest() {
        let mut s = Scroll::new();
        s.up(5, 50, 10);
        assert_eq!(s.window(50, 10), (35, 45));
        assert!(!s.at_bottom(), "scrolled back is not following");
        // Past the top clamps rather than underflowing.
        s.up(1_000, 50, 10);
        assert_eq!(s.window(50, 10), (0, 10), "the oldest 10");
        s.up(1, 50, 10);
        assert_eq!(s.window(50, 10), (0, 10), "already there");
    }

    #[test]
    fn scrolling_down_returns_to_the_bottom_and_resumes_following() {
        let mut s = Scroll::new();
        s.up(20, 50, 10);
        s.down(5);
        assert_eq!(s.window(50, 10), (25, 35));
        s.down(1_000);
        assert!(s.at_bottom(), "clamps to the bottom, and follows again");
        assert_eq!(s.window(50, 10), (40, 50));
    }

    #[test]
    fn home_and_end_jump() {
        let mut s = Scroll::new();
        s.jump_top(50, 10);
        assert_eq!(s.window(50, 10), (0, 10));
        s.jump_bottom();
        assert_eq!(s.window(50, 10), (40, 50));
        assert!(s.at_bottom());
    }

    #[test]
    fn following_stays_at_the_bottom_when_messages_arrive() {
        // The whole point of following.
        let mut s = Scroll::new();
        s.on_appended(3, 50);
        assert!(s.at_bottom());
        assert_eq!(s.window(53, 10), (43, 53), "the newest, including the new");
    }

    #[test]
    fn reading_back_is_not_yanked_away_by_new_messages() {
        // Scrolled up to read something, three messages land. The lines you
        // were reading must stay put, or a busy room makes the backlog
        // unreadable.
        let mut s = Scroll::new();
        s.up(20, 50, 10);
        let before = s.window(50, 10);
        s.on_appended(3, 50);
        assert_eq!(s.window(53, 10), before, "same lines under the eyes");
        assert!(!s.at_bottom());
    }

    #[test]
    fn the_buffer_is_a_window_not_an_archive() {
        // Below the cap nothing is dropped.
        let mut buf: Vec<u32> = (0..(MAX_LINES as u32 - 1)).collect();
        assert_eq!(push_trimmed(&mut buf, 111), 0, "still room");
        assert_eq!(buf.len(), MAX_LINES);
        assert_eq!(buf[0], 0, "nothing trimmed yet");

        // At the cap, one in means one out — and it's the OLDEST that goes.
        assert_eq!(push_trimmed(&mut buf, 999), 1, "full: one falls off");
        assert_eq!(buf.len(), MAX_LINES, "held at the cap");
        assert_eq!(*buf.last().unwrap(), 999, "the newest is kept");
        assert_eq!(buf[0], 1, "the oldest went");
    }

    #[test]
    fn a_scroll_position_stranded_by_trimming_still_renders() {
        // The buffer can shrink under a scrolled-back reader (trimming). The
        // window must clamp, not panic on a reversed range.
        let mut s = Scroll::new();
        s.up(500, 600, 10);
        assert_eq!(s.window(20, 10), (0, 10), "clamped to what exists");
    }
}
