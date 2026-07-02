//! Feed → board-post drafts.
//!
//! [`to_post_drafts`] turns a parsed [`Feed`] into a list of [`PostDraft`]s,
//! one per [`FeedItem`], ready for a later wave to sign and hand to
//! `BoardService`. This is a pure, deterministic transform — no clock, no
//! network, no board wiring — so it can be tested exhaustively on the host.
//!
//! Each draft carries the item's stable [`dedup_id`](crate::dedup::dedup_id)
//! so that re-ingesting the same feed (feeds are polled forever) never
//! double-posts: the seen-set layer ([`crate::seen`]) filters drafts whose
//! id was already stored.
//!
//! Field derivation, mirroring how the parser already normalizes things:
//! - **subject** ← item title, capped at [`BoardMapping::max_subject_chars`];
//!   an empty title becomes [`BoardMapping::untitled_subject`].
//! - **body** ← the item's plain-text summary (already tag-stripped by the
//!   parser), with a canonical source link appended when
//!   [`BoardMapping::append_source_link`] is set.
//! - **author** ← item author, else [`BoardMapping::default_author`], else the
//!   feed's own title (its display name) as a last resort.
//! - **source_link** ← item link, else the channel/feed link.

use crate::dedup::dedup_id;
use crate::feed::{Feed, FeedItem};
use crate::text::cap_chars;

/// A board post ready to be created from a feed item. Presentation fields are
/// already normalized to plain text by the parser; `dedup_id` is the stable
/// identity used to suppress re-posts.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PostDraft {
    /// Stable per-item id (see [`dedup_id`]); the same item always maps to
    /// the same value across re-fetches.
    pub dedup_id: String,
    /// Target board slug (copied from the [`BoardMapping`]).
    pub board: String,
    /// Post subject line.
    pub subject: String,
    /// Post body (plain text; may include an appended source link).
    pub body: String,
    /// Display author.
    pub author: String,
    /// Canonical link back to the source item (item link, else feed link).
    pub source_link: String,
    /// Original publish time as unix seconds, when the feed provided one.
    pub published_unix: Option<i64>,
}

/// How a feed maps onto a board. Constructed via [`BoardMapping::new`] then
/// tweaked field-by-field; [`Default`] targets an empty board name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoardMapping {
    /// Destination board slug for every draft.
    pub board: String,
    /// Author used when neither the item nor (below) the feed names one.
    pub default_author: String,
    /// Append a `label link` line to the body when the item has a link.
    pub append_source_link: bool,
    /// Label placed before the appended source link (e.g. `"Source:"`); an
    /// empty label appends the bare link.
    pub source_link_label: String,
    /// Subject used when the item has no (non-whitespace) title.
    pub untitled_subject: String,
    /// Character cap for subjects (truncation lands on a char boundary and is
    /// marked `…`, via [`cap_chars`]).
    pub max_subject_chars: usize,
}

impl Default for BoardMapping {
    fn default() -> Self {
        Self {
            board: String::new(),
            default_author: String::new(),
            append_source_link: true,
            source_link_label: "Source:".to_string(),
            untitled_subject: "(untitled)".to_string(),
            max_subject_chars: 200,
        }
    }
}

impl BoardMapping {
    /// A mapping onto `board` with sensible defaults.
    pub fn new(board: impl Into<String>) -> Self {
        Self {
            board: board.into(),
            ..Self::default()
        }
    }
}

/// Convert every item of `feed` into a [`PostDraft`] using `mapping`. Order is
/// preserved and the transform is total: malformed or empty items yield
/// well-formed (if sparse) drafts rather than errors.
pub fn to_post_drafts(feed: &Feed, mapping: &BoardMapping) -> Vec<PostDraft> {
    feed.items
        .iter()
        .map(|item| item_to_draft(item, feed, mapping))
        .collect()
}

/// Map a single item (borrowing its feed for the author/link fallbacks).
fn item_to_draft(item: &FeedItem, feed: &Feed, mapping: &BoardMapping) -> PostDraft {
    let subject = if item.title.trim().is_empty() {
        mapping.untitled_subject.clone()
    } else {
        cap_chars(&item.title, mapping.max_subject_chars)
    };
    let source_link = if item.link.is_empty() {
        feed.link.clone()
    } else {
        item.link.clone()
    };
    PostDraft {
        dedup_id: dedup_id(item),
        board: mapping.board.clone(),
        subject,
        body: build_body(&item.summary_text, &source_link, mapping),
        author: resolve_author(item, feed, mapping),
        source_link,
        published_unix: item.published_unix,
    }
}

/// item author → mapping default → feed title (display name).
fn resolve_author(item: &FeedItem, feed: &Feed, mapping: &BoardMapping) -> String {
    if !item.author.is_empty() {
        item.author.clone()
    } else if !mapping.default_author.is_empty() {
        mapping.default_author.clone()
    } else {
        feed.title.clone()
    }
}

/// Body = summary, optionally followed by a blank line and the source link.
fn build_body(summary: &str, source_link: &str, mapping: &BoardMapping) -> String {
    let mut body = summary.trim().to_string();
    if mapping.append_source_link && !source_link.is_empty() {
        if !body.is_empty() {
            body.push_str("\n\n");
        }
        if mapping.source_link_label.is_empty() {
            body.push_str(source_link);
        } else {
            body.push_str(&mapping.source_link_label);
            body.push(' ');
            body.push_str(source_link);
        }
    }
    body
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse;

    const RSS: &str = r#"<rss version="2.0">
      <channel>
        <title>The Warren</title>
        <link>https://warren.example/</link>
        <description>desc</description>
        <item>
          <title>First &amp; foremost</title>
          <link>https://warren.example/1</link>
          <guid>urn:warren:1</guid>
          <dc:creator>Fiver</dc:creator>
          <pubDate>Tue, 10 Jun 2003 04:00:00 GMT</pubDate>
          <description><![CDATA[<p>Body <b>one</b>.</p>]]></description>
        </item>
        <item>
          <link>https://warren.example/2</link>
          <guid>urn:warren:2</guid>
          <description>no title here</description>
        </item>
        <item>
          <title>Third, no link, no author</title>
          <guid>urn:warren:3</guid>
        </item>
      </channel>
    </rss>"#;

    fn feed() -> Feed {
        parse(RSS).unwrap()
    }

    #[test]
    fn maps_every_item_in_order() {
        let drafts = to_post_drafts(&feed(), &BoardMapping::new("general"));
        assert_eq!(drafts.len(), 3);
        assert!(drafts.iter().all(|d| d.board == "general"));
        assert_eq!(drafts[0].subject, "First & foremost");
        assert_eq!(drafts[0].author, "Fiver");
        assert_eq!(drafts[0].source_link, "https://warren.example/1");
        assert_eq!(drafts[0].published_unix, Some(1_055_217_600));
        assert_eq!(
            drafts[0].body,
            "Body one.\n\nSource: https://warren.example/1"
        );
    }

    #[test]
    fn empty_title_uses_untitled_placeholder() {
        let drafts = to_post_drafts(&feed(), &BoardMapping::new("b"));
        assert_eq!(drafts[1].subject, "(untitled)");
    }

    #[test]
    fn author_falls_back_to_default_then_feed_title() {
        // Item 2 has no author; default provided → default wins.
        let mut m = BoardMapping::new("b");
        m.default_author = "Syndication Bot".into();
        let drafts = to_post_drafts(&feed(), &m);
        assert_eq!(drafts[1].author, "Syndication Bot");

        // No default → the feed's title stands in as the author.
        let drafts = to_post_drafts(&feed(), &BoardMapping::new("b"));
        assert_eq!(drafts[1].author, "The Warren");
    }

    #[test]
    fn source_link_falls_back_to_feed_link() {
        // Item 3 has no link → the channel link is used.
        let drafts = to_post_drafts(&feed(), &BoardMapping::new("b"));
        assert_eq!(drafts[2].source_link, "https://warren.example/");
        assert!(drafts[2].body.ends_with("https://warren.example/"));
    }

    #[test]
    fn source_link_appending_is_optional_and_labelable() {
        let mut m = BoardMapping::new("b");
        m.append_source_link = false;
        let drafts = to_post_drafts(&feed(), &m);
        assert_eq!(drafts[0].body, "Body one.");

        let mut m = BoardMapping::new("b");
        m.source_link_label = String::new();
        let drafts = to_post_drafts(&feed(), &m);
        assert_eq!(drafts[0].body, "Body one.\n\nhttps://warren.example/1");
    }

    #[test]
    fn empty_summary_body_is_just_the_source_link() {
        // Item 3 has no description at all.
        let drafts = to_post_drafts(&feed(), &BoardMapping::new("b"));
        assert_eq!(drafts[2].body, "Source: https://warren.example/");
    }

    #[test]
    fn subject_is_capped_on_char_boundary() {
        let mut m = BoardMapping::new("b");
        m.max_subject_chars = 6;
        let drafts = to_post_drafts(&feed(), &m);
        assert!(drafts[0].subject.chars().count() <= 6);
        assert!(drafts[0].subject.ends_with('…'));
    }

    #[test]
    fn dedup_id_matches_the_item_and_is_stable_across_reparse() {
        let f1 = feed();
        let drafts1 = to_post_drafts(&f1, &BoardMapping::new("b"));
        for (item, draft) in f1.items.iter().zip(&drafts1) {
            assert_eq!(draft.dedup_id, dedup_id(item));
        }
        // Re-parsing the identical bytes yields identical draft ids.
        let drafts2 = to_post_drafts(&feed(), &BoardMapping::new("b"));
        let ids1: Vec<_> = drafts1.iter().map(|d| &d.dedup_id).collect();
        let ids2: Vec<_> = drafts2.iter().map(|d| &d.dedup_id).collect();
        assert_eq!(ids1, ids2);
        // And they are distinct per item.
        assert_ne!(drafts1[0].dedup_id, drafts1[1].dedup_id);
        assert_ne!(drafts1[1].dedup_id, drafts1[2].dedup_id);
    }

    #[test]
    fn totally_empty_feed_yields_no_drafts() {
        let empty = Feed::default();
        assert!(to_post_drafts(&empty, &BoardMapping::new("b")).is_empty());
    }
}
