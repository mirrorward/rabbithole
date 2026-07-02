//! RSS 2.0 / Atom 1.0 → normalized [`Feed`] model.
//!
//! A single entry point ([`parse`] / [`parse_with_options`]) sniffs the
//! root element (`<rss>` vs `<feed>`, namespace prefixes ignored) and
//! runs the matching reader over the token stream from [`crate::xml`].
//! Both readers are lenient recursive-descent loops: known children are
//! captured, unknown subtrees are skipped by depth counting, and a
//! truncated document simply yields whatever was complete — the only
//! error is [`FeedError::NotAFeed`] when no feed root exists at all.
//!
//! Normalization decisions:
//! - Titles/links/authors get XML entity decoding + whitespace collapse.
//! - Bodies (RSS `description` falling back to `content:encoded`; Atom
//!   `summary` falling back to `content`) run through
//!   [`crate::text::html_to_text`], capped by
//!   [`ParseOptions::summary_max_chars`].
//! - RSS `dc:creator` wins over `<author>` (which is an email per spec);
//!   Atom authors use `<author><name>`.
//! - Dates: RSS `pubDate`/`dc:date`, Atom `published` falling back to
//!   `updated` — each tried against both RFC 2822 and RFC 3339.
//! - Atom links prefer `rel="alternate"` (or no `rel`) over `self` etc.

use std::fmt;

use crate::date::parse_date_lenient;
use crate::text::{collapse_whitespace, html_to_text};
use crate::xml::{attr, Reader, Token};

/// A parsed feed (channel-level metadata plus items), normalized across
/// RSS 2.0 and Atom 1.0. Missing fields are empty strings.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Feed {
    pub title: String,
    pub link: String,
    pub description: String,
    pub items: Vec<FeedItem>,
}

/// One item/entry. Missing fields are empty strings; `guid` holds the
/// RSS `<guid>` or Atom `<id>`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FeedItem {
    pub title: String,
    pub link: String,
    pub guid: String,
    pub author: String,
    /// RSS `pubDate` / Atom `published` (falling back to `updated`),
    /// as unix seconds; `None` when absent or unparseable.
    pub published_unix: Option<i64>,
    /// Item body converted to plain text and capped
    /// (see [`ParseOptions::summary_max_chars`]).
    pub summary_text: String,
}

/// Knobs for [`parse_with_options`].
#[derive(Debug, Clone)]
pub struct ParseOptions {
    /// Character cap applied to `summary_text` (and the channel
    /// description). Truncation lands on a char boundary, marked `…`.
    pub summary_max_chars: usize,
}

impl Default for ParseOptions {
    fn default() -> Self {
        Self {
            summary_max_chars: 480,
        }
    }
}

/// Parse failure. Malformed-but-recognizable feeds do *not* error — they
/// yield a partial [`Feed`] — so this only reports unrecognizable input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FeedError {
    /// No `<rss>` or `<feed>` root element was found.
    NotAFeed,
}

impl fmt::Display for FeedError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FeedError::NotAFeed => write!(f, "input contains no RSS or Atom feed root"),
        }
    }
}

impl std::error::Error for FeedError {}

/// Parse with default options.
pub fn parse(input: &str) -> Result<Feed, FeedError> {
    parse_with_options(input, &ParseOptions::default())
}

/// Parse an RSS 2.0 or Atom 1.0 document.
pub fn parse_with_options(input: &str, opts: &ParseOptions) -> Result<Feed, FeedError> {
    let mut r = Reader::new(input);
    while let Some(tok) = r.next_token() {
        if let Token::Open {
            name, self_closing, ..
        } = tok
        {
            let root = local_name(name);
            if root.eq_ignore_ascii_case("rss") {
                return Ok(if self_closing {
                    Feed::default()
                } else {
                    parse_rss(&mut r, opts)
                });
            }
            if root.eq_ignore_ascii_case("feed") {
                return Ok(if self_closing {
                    Feed::default()
                } else {
                    parse_atom(&mut r, opts)
                });
            }
            // Unknown wrapper element: keep scanning inside it.
        }
    }
    Err(FeedError::NotAFeed)
}

/// Strip a namespace prefix: `atom:link` → `link`.
fn local_name(name: &str) -> &str {
    name.rsplit(':').next().unwrap_or(name)
}

/// Decode-collapse for single-line fields (entities were already decoded
/// by `collect_text`).
fn clean_inline(s: &str) -> String {
    collapse_whitespace(s)
}

/// Gather all text/CDATA inside the element just opened, until its close
/// (generic depth counting, so nested markup like inline XHTML
/// contributes its text). XML entities in text nodes are decoded; CDATA
/// is kept raw — the HTML layer, if any, is decoded later by
/// `html_to_text`.
fn collect_text(r: &mut Reader) -> String {
    let mut depth = 1usize;
    let mut out = String::new();
    while let Some(tok) = r.next_token() {
        match tok {
            Token::Open { self_closing, .. } => {
                if !self_closing {
                    depth += 1;
                }
                out.push(' ');
            }
            Token::Close(_) => {
                depth -= 1;
                if depth == 0 {
                    break;
                }
                out.push(' ');
            }
            Token::Text(t) => out.push_str(&crate::text::decode_entities(t)),
            Token::CData(c) => out.push_str(c),
        }
    }
    out
}

/// Skip the element just opened (depth counting; tolerant of truncation).
fn skip_element(r: &mut Reader) {
    let mut depth = 1usize;
    while let Some(tok) = r.next_token() {
        match tok {
            Token::Open { self_closing, .. } => {
                if !self_closing {
                    depth += 1;
                }
            }
            Token::Close(_) => {
                depth -= 1;
                if depth == 0 {
                    return;
                }
            }
            _ => {}
        }
    }
}

// ---------------------------------------------------------------- RSS 2.0

fn parse_rss(r: &mut Reader, opts: &ParseOptions) -> Feed {
    let mut feed = Feed::default();
    while let Some(tok) = r.next_token() {
        if let Token::Open {
            name, self_closing, ..
        } = tok
        {
            if self_closing {
                continue;
            }
            if local_name(name).eq_ignore_ascii_case("channel") {
                parse_rss_channel(r, &mut feed, opts);
            } else {
                skip_element(r);
            }
        }
    }
    feed
}

fn parse_rss_channel(r: &mut Reader, feed: &mut Feed, opts: &ParseOptions) {
    while let Some(tok) = r.next_token() {
        match tok {
            Token::Open {
                name, self_closing, ..
            } => {
                if self_closing {
                    continue;
                }
                // Full qualified names: `atom:link` must NOT hijack the
                // channel `<link>`, so namespaced tags are matched exactly.
                match name.to_ascii_lowercase().as_str() {
                    "title" => feed.title = clean_inline(&collect_text(r)),
                    "link" => feed.link = clean_inline(&collect_text(r)),
                    "description" => {
                        feed.description = html_to_text(&collect_text(r), opts.summary_max_chars);
                    }
                    "item" => {
                        let item = parse_rss_item(r, opts);
                        feed.items.push(item);
                    }
                    _ => skip_element(r),
                }
            }
            Token::Close(_) => return, // </channel>
            _ => {}
        }
    }
}

fn parse_rss_item(r: &mut Reader, opts: &ParseOptions) -> FeedItem {
    let mut item = FeedItem::default();
    let mut author_email = String::new();
    let mut creator = String::new();
    let mut description = String::new();
    let mut content_encoded = String::new();
    while let Some(tok) = r.next_token() {
        match tok {
            Token::Open {
                name, self_closing, ..
            } => {
                if self_closing {
                    continue;
                }
                match name.to_ascii_lowercase().as_str() {
                    "title" => item.title = clean_inline(&collect_text(r)),
                    "link" => item.link = clean_inline(&collect_text(r)),
                    "guid" => item.guid = clean_inline(&collect_text(r)),
                    "author" => author_email = clean_inline(&collect_text(r)),
                    "dc:creator" => creator = clean_inline(&collect_text(r)),
                    "pubdate" | "dc:date" => {
                        if item.published_unix.is_none() {
                            item.published_unix = parse_date_lenient(collect_text(r).trim());
                        } else {
                            skip_element(r);
                        }
                    }
                    "description" => description = collect_text(r),
                    "content:encoded" => content_encoded = collect_text(r),
                    _ => skip_element(r),
                }
            }
            Token::Close(_) => break, // </item>
            _ => {}
        }
    }
    item.author = if creator.is_empty() {
        author_email
    } else {
        creator
    };
    let body = if description.trim().is_empty() {
        content_encoded
    } else {
        description
    };
    item.summary_text = html_to_text(&body, opts.summary_max_chars);
    item
}

// --------------------------------------------------------------- Atom 1.0

fn parse_atom(r: &mut Reader, opts: &ParseOptions) -> Feed {
    let mut feed = Feed::default();
    while let Some(tok) = r.next_token() {
        match tok {
            Token::Open {
                name,
                attrs,
                self_closing,
            } => {
                let tag = local_name(name).to_ascii_lowercase();
                if tag == "link" {
                    pick_atom_link(attrs, &mut feed.link);
                    if !self_closing {
                        skip_element(r);
                    }
                    continue;
                }
                if self_closing {
                    continue;
                }
                match tag.as_str() {
                    "title" => feed.title = clean_inline(&collect_text(r)),
                    "subtitle" => {
                        feed.description = html_to_text(&collect_text(r), opts.summary_max_chars);
                    }
                    "entry" => {
                        let entry = parse_atom_entry(r, opts);
                        feed.items.push(entry);
                    }
                    _ => skip_element(r),
                }
            }
            Token::Close(_) => break, // </feed>
            _ => {}
        }
    }
    feed
}

fn parse_atom_entry(r: &mut Reader, opts: &ParseOptions) -> FeedItem {
    let mut item = FeedItem::default();
    let mut published = String::new();
    let mut updated = String::new();
    let mut summary = String::new();
    let mut content = String::new();
    while let Some(tok) = r.next_token() {
        match tok {
            Token::Open {
                name,
                attrs,
                self_closing,
            } => {
                let tag = local_name(name).to_ascii_lowercase();
                if tag == "link" {
                    pick_atom_link(attrs, &mut item.link);
                    if !self_closing {
                        skip_element(r);
                    }
                    continue;
                }
                if self_closing {
                    continue;
                }
                match tag.as_str() {
                    "title" => item.title = clean_inline(&collect_text(r)),
                    "id" => item.guid = clean_inline(&collect_text(r)),
                    "author" => item.author = parse_atom_person(r),
                    "published" => published = collect_text(r),
                    "updated" => updated = collect_text(r),
                    "summary" => summary = collect_text(r),
                    "content" => content = collect_text(r),
                    _ => skip_element(r),
                }
            }
            Token::Close(_) => break, // </entry>
            _ => {}
        }
    }
    item.published_unix =
        parse_date_lenient(published.trim()).or_else(|| parse_date_lenient(updated.trim()));
    let body = if summary.trim().is_empty() {
        content
    } else {
        summary
    };
    item.summary_text = html_to_text(&body, opts.summary_max_chars);
    item
}

/// `<author><name>...</name></author>` — the display name; other person
/// fields (`email`, `uri`) are skipped.
fn parse_atom_person(r: &mut Reader) -> String {
    let mut display = String::new();
    while let Some(tok) = r.next_token() {
        match tok {
            Token::Open {
                name, self_closing, ..
            } => {
                if self_closing {
                    continue;
                }
                if local_name(name).eq_ignore_ascii_case("name") {
                    display = clean_inline(&collect_text(r));
                } else {
                    skip_element(r);
                }
            }
            Token::Close(_) => break,
            _ => {}
        }
    }
    display
}

/// Atom link selection: `rel="alternate"` (or no rel) wins; any other rel
/// (self, edit, enclosure…) only fills an empty slot as a fallback.
fn pick_atom_link(attrs: &str, slot: &mut String) {
    let Some(href) = attr(attrs, "href") else {
        return;
    };
    let href = collapse_whitespace(&href);
    if href.is_empty() {
        return;
    }
    let rel = attr(attrs, "rel").unwrap_or_default();
    if rel.is_empty() || rel.eq_ignore_ascii_case("alternate") || slot.is_empty() {
        *slot = href;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dedup::dedup_id;

    /// Real-world-shaped RSS 2.0: CDATA, escaped-HTML descriptions,
    /// content:encoded, dc:creator, atom:link noise, unknown elements.
    const RSS_FIXTURE: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0" xmlns:atom="http://www.w3.org/2005/Atom"
     xmlns:dc="http://purl.org/dc/elements/1.1/"
     xmlns:content="http://purl.org/rss/1.0/modules/content/">
  <channel>
    <title>Warren &amp; Burrow</title>
    <link>https://warren.example/blog</link>
    <atom:link href="https://warren.example/feed.xml" rel="self" type="application/rss+xml"/>
    <description>Dispatches from &lt;em&gt;the&lt;/em&gt; warren</description>
    <language>en-us</language>
    <lastBuildDate>Tue, 10 Jun 2003 09:41:01 GMT</lastBuildDate>
    <image><url>https://warren.example/logo.png</url><title>logo title (must not clobber)</title></image>
    <item>
      <title>Star City opens &#x1F407;</title>
      <link>https://warren.example/blog/star-city</link>
      <guid isPermaLink="false">urn:warren:post:1001</guid>
      <author>editor@warren.example (The Editor)</author>
      <dc:creator>Fiver</dc:creator>
      <pubDate>Tue, 10 Jun 2003 04:00:00 GMT</pubDate>
      <description><![CDATA[<p>How do Americans get to the <b>park</b>?</p><p>They just take an elevator &amp; go.</p>]]></description>
      <category>space</category>
    </item>
    <item>
      <title>Second post, sparse</title>
      <link>https://warren.example/blog/second</link>
      <pubDate>Wed, 02 Jul 2003 05:00:00 -0700</pubDate>
      <description></description>
      <content:encoded><![CDATA[Full <i>body</i> used when description is empty.]]></content:encoded>
    </item>
    <item>
      <title>No guid, no link — title/date only</title>
      <pubDate>bogus date text</pubDate>
      <description>plain text body, no markup at all</description>
    </item>
  </channel>
</rss>"#;

    /// Real-world-shaped Atom 1.0: rel-qualified links, published+updated,
    /// type="html" summary, xhtml content, multi-field author.
    const ATOM_FIXTURE: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<feed xmlns="http://www.w3.org/2005/Atom">
  <title>Lapine Signal</title>
  <subtitle type="html">News &amp;amp; notes</subtitle>
  <link rel="self" href="https://lapine.example/feed.atom"/>
  <link rel="alternate" type="text/html" href="https://lapine.example/"/>
  <updated>2003-12-13T18:30:02Z</updated>
  <id>urn:uuid:60a76c80-d399-11d9-b93C-0003939e0af6</id>
  <entry>
    <title>Atom-Powered Robots Run Amok</title>
    <link rel="enclosure" href="https://lapine.example/audio.mp3"/>
    <link href="https://lapine.example/2003/12/13/robots"/>
    <id>tag:lapine.example,2003:3.2397</id>
    <published>2003-12-13T08:29:29-04:00</published>
    <updated>2003-12-13T18:30:02Z</updated>
    <author><name>Hazel Rah</name><email>hazel@lapine.example</email></author>
    <summary type="html">Some &lt;b&gt;bold&lt;/b&gt; text.</summary>
    <content type="xhtml"><div xmlns="http://www.w3.org/1999/xhtml"><p>ignored: summary wins</p></div></content>
  </entry>
  <entry>
    <title>Updated-only entry</title>
    <link rel="alternate" href="https://lapine.example/2004/01/02/second"/>
    <id>tag:lapine.example,2004:9.1111</id>
    <updated>2004-01-02t03:04:05z</updated>
    <content type="xhtml"><div xmlns="http://www.w3.org/1999/xhtml">Inline <em>xhtml</em> body.</div></content>
  </entry>
</feed>"#;

    /// Sparse, scruffy RSS: no XML declaration, uppercase-ish tags
    /// tolerated, missing optional fields, entity-heavy title.
    const SCRUFFY_RSS_FIXTURE: &str = "<rss version=\"0.91\"><channel>\
<title>Ye Olde BBS &quot;Files&quot; &amp; More</title>\
<link>gopher://old.example</link>\
<description>desc</description>\
<item><title>it&#39;s alive</title><link>gopher://old.example/1</link></item>\
</channel></rss>";

    #[test]
    fn parses_rss_fixture() {
        let feed = parse(RSS_FIXTURE).unwrap();
        assert_eq!(feed.title, "Warren & Burrow");
        assert_eq!(
            feed.link, "https://warren.example/blog",
            "atom:link must not clobber <link>"
        );
        assert_eq!(feed.description, "Dispatches from the warren");
        assert_eq!(feed.items.len(), 3);

        let a = &feed.items[0];
        assert_eq!(a.title, "Star City opens \u{1F407}");
        assert_eq!(a.link, "https://warren.example/blog/star-city");
        assert_eq!(a.guid, "urn:warren:post:1001");
        assert_eq!(a.author, "Fiver", "dc:creator wins over author email");
        assert_eq!(a.published_unix, Some(1_055_217_600));
        assert_eq!(
            a.summary_text,
            "How do Americans get to the park? They just take an elevator & go."
        );

        let b = &feed.items[1];
        assert_eq!(b.author, "");
        assert_eq!(
            b.published_unix,
            Some(1_057_147_200),
            "-0700 offset applied: 2003-07-02 12:00:00 UTC"
        );
        assert_eq!(
            b.summary_text, "Full body used when description is empty.",
            "content:encoded fallback"
        );

        let c = &feed.items[2];
        assert_eq!(c.guid, "");
        assert_eq!(c.link, "");
        assert_eq!(c.published_unix, None, "bogus date is None, not an error");
        assert_eq!(c.summary_text, "plain text body, no markup at all");
    }

    #[test]
    fn parses_atom_fixture() {
        let feed = parse(ATOM_FIXTURE).unwrap();
        assert_eq!(feed.title, "Lapine Signal");
        assert_eq!(
            feed.link, "https://lapine.example/",
            "alternate wins over self"
        );
        assert_eq!(
            feed.description, "News & notes",
            "html-typed subtitle decoded twice"
        );
        assert_eq!(feed.items.len(), 2);

        let a = &feed.items[0];
        assert_eq!(a.title, "Atom-Powered Robots Run Amok");
        assert_eq!(
            a.link, "https://lapine.example/2003/12/13/robots",
            "rel-less link wins over enclosure"
        );
        assert_eq!(a.guid, "tag:lapine.example,2003:3.2397");
        assert_eq!(a.author, "Hazel Rah");
        // 08:29:29-04:00 == 12:29:29Z; date -u -d "2003-12-13T12:29:29Z" +%s
        assert_eq!(
            a.published_unix,
            Some(1_071_318_569),
            "published wins over updated"
        );
        assert_eq!(
            a.summary_text, "Some bold text.",
            "summary wins over content"
        );

        let b = &feed.items[1];
        assert_eq!(
            b.published_unix,
            crate::date::parse_rfc3339("2004-01-02T03:04:05Z"),
            "updated fallback, lowercase t/z"
        );
        assert_eq!(
            b.summary_text, "Inline xhtml body.",
            "xhtml content flattened"
        );
    }

    #[test]
    fn parses_scruffy_rss_fixture() {
        let feed = parse(SCRUFFY_RSS_FIXTURE).unwrap();
        assert_eq!(feed.title, "Ye Olde BBS \"Files\" & More");
        assert_eq!(feed.items.len(), 1);
        assert_eq!(feed.items[0].title, "it's alive");
        assert_eq!(feed.items[0].published_unix, None);
        assert_eq!(feed.items[0].summary_text, "");
    }

    #[test]
    fn summary_cap_is_configurable() {
        let opts = ParseOptions {
            summary_max_chars: 10,
        };
        let feed = parse_with_options(RSS_FIXTURE, &opts).unwrap();
        for item in &feed.items {
            assert!(
                item.summary_text.chars().count() <= 10,
                "{:?}",
                item.summary_text
            );
        }
        assert!(feed.items[0].summary_text.ends_with('…'));
    }

    #[test]
    fn rejects_non_feeds() {
        for junk in [
            "",
            "hello world",
            "<html><body>not a feed</body></html>",
            "<?xml version=\"1.0\"?><opml></opml>",
            "\u{0}\u{1}\u{2}binary\u{fffd}garbage>>>",
            "{\"json\": true}",
        ] {
            assert_eq!(parse(junk), Err(FeedError::NotAFeed), "input {junk:?}");
        }
    }

    #[test]
    fn truncation_never_panics_and_degrades_gracefully() {
        // Every char-boundary prefix of both fixtures must parse without
        // panicking (result may be Ok-partial or NotAFeed).
        for fixture in [RSS_FIXTURE, ATOM_FIXTURE] {
            for i in 0..=fixture.len() {
                if fixture.is_char_boundary(i) {
                    let _ = parse(&fixture[..i]);
                }
            }
        }
        // A cut mid-item still yields the complete earlier item.
        let cut = RSS_FIXTURE.find("<item>").unwrap() + "<item>".len();
        let head = parse(&RSS_FIXTURE[..RSS_FIXTURE.find("Second post").unwrap()]).unwrap();
        assert_eq!(head.title, "Warren & Burrow");
        assert!(!head.items.is_empty());
        assert_eq!(head.items[0].guid, "urn:warren:post:1001");
        let barely = parse(&RSS_FIXTURE[..cut]).unwrap();
        assert_eq!(barely.title, "Warren & Burrow");
    }

    #[test]
    fn junk_bytes_are_tolerated() {
        let raw: Vec<u8> = (0u8..=255).cycle().take(4096).collect();
        let lossy = String::from_utf8_lossy(&raw);
        assert_eq!(parse(&lossy), Err(FeedError::NotAFeed));
        // Junk *around* a valid feed still parses.
        let framed = format!("\u{fffd}\u{0}<<>&#\n{SCRUFFY_RSS_FIXTURE}\u{fffd}");
        assert!(parse(&framed).is_ok());
    }

    #[test]
    fn dedup_ids_from_fixtures_are_stable_and_distinct() {
        let feed = parse(RSS_FIXTURE).unwrap();
        let ids: Vec<String> = feed.items.iter().map(dedup_id).collect();
        // Distinct across items.
        assert_eq!(ids.len(), 3);
        assert_ne!(ids[0], ids[1]);
        assert_ne!(ids[1], ids[2]);
        // Stable across a re-parse.
        let again = parse(RSS_FIXTURE).unwrap();
        let ids2: Vec<String> = again.items.iter().map(dedup_id).collect();
        assert_eq!(ids, ids2);
    }
}
