//! Markdown → HTML, for message bodies.
//!
//! # Why markdown is the stored form
//!
//! This app is not the only reader of a post. A burrow also serves telnet, the
//! `rabbit` CLI, finger, and NNTP/FTN/QWK syndication — surfaces that will never
//! render a `<strong>`. Markdown degrades to exactly what the author typed;
//! stored HTML would arrive at those clients as tag soup. The board wire has
//! carried a body MIME (`text/plain` | `text/markdown` | `text/x-ansi`) since
//! Wave 3 for precisely this reason, so posting markdown is what the protocol
//! was already designed for — the client simply never rendered it.
//!
//! # Why this is written rather than pulled in
//!
//! The renderer's real job is a security boundary: message bodies are attacker
//! controlled, and they end up in `inner_html`. So the rule here is absolute and
//! it's the first thing that happens — **every input byte is HTML-escaped before
//! anything else looks at it**, and markdown syntax is recognised only in the
//! escaped text. There is no path by which a `<` in a message becomes a `<` in
//! the DOM, because by the time any rule runs there are no `<` characters left.
//! Raw-HTML passthrough, the feature that makes most markdown libraries a
//! liability here, therefore cannot be switched on by accident.
//!
//! Link targets get a second gate: an allowlist of `http`, `https` and `mailto`.
//! Anything else renders as plain text rather than a link, so `javascript:` and
//! `data:` never reach an `href`.
//!
//! The supported subset is deliberately small — bold, italic, strikethrough,
//! code (inline and fenced), links, autolinks, headings, blockquotes, lists,
//! rules. Everything a person writes in a chat message, and nothing whose
//! parsing subtleties would be a place for bugs to hide.

/// Escape the five characters that let text become markup. Everything the
/// renderer does downstream operates on the output of this function.
fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 16);
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

/// Schemes a link may use. An allowlist, not a blocklist: the ways of spelling
/// `javascript:` are open-ended, and the set of schemes anyone needs in a
/// message is not.
const SAFE_SCHEMES: [&str; 3] = ["http://", "https://", "mailto:"];

/// The `href` for a link target, or `None` if it isn't one we'll link to.
///
/// Takes already-escaped text, so quotes are `&quot;` and can't close the
/// attribute. The scheme check runs on a lowercased, trimmed copy so
/// `  JaVaScRiPt:` doesn't slip through.
fn safe_href(escaped: &str) -> Option<String> {
    let probe = escaped.trim().to_ascii_lowercase();
    // Whitespace inside a scheme is another way to write `java\nscript:`; a URL
    // has no business containing any.
    if probe.is_empty() || probe.chars().any(char::is_whitespace) {
        return None;
    }
    SAFE_SCHEMES
        .iter()
        .any(|s| probe.starts_with(s))
        .then(|| escaped.trim().to_string())
}

/// Render inline markdown within one already-escaped line.
///
/// Inline code is matched first: text inside backticks is literal, so a `*` in
/// a code span must not become emphasis.
fn inline(escaped: &str) -> String {
    let b: Vec<char> = escaped.chars().collect();
    let mut out = String::with_capacity(escaped.len() + 32);
    let mut i = 0;
    while i < b.len() {
        // `code`
        if b[i] == '`' {
            if let Some(end) = find(&b, i + 1, &['`']) {
                out.push_str("<code>");
                out.extend(b[i + 1..end].iter());
                out.push_str("</code>");
                i = end + 1;
                continue;
            }
        }
        // **strong** and ~~strike~~ — two-character fences. `break` only leaves
        // this inner loop, so whether it consumed input is tracked explicitly
        // rather than inferred from what the output happens to end with.
        let mut consumed = false;
        for (marker, tag) in [('*', "strong"), ('~', "del")] {
            if b[i] == marker && b.get(i + 1) == Some(&marker) {
                if let Some(end) = find_pair(&b, i + 2, marker) {
                    out.push_str(&format!("<{tag}>"));
                    out.push_str(&inline(&b[i + 2..end].iter().collect::<String>()));
                    out.push_str(&format!("</{tag}>"));
                    i = end + 2;
                    consumed = true;
                    break;
                }
            }
        }
        if consumed {
            continue;
        }
        match b[i] {
            // *emphasis* / _emphasis_ — single character, and not a `**` run.
            '*' | '_' if b.get(i + 1) != Some(&b[i]) => {
                if let Some(end) = find(&b, i + 1, &[b[i]]) {
                    out.push_str("<em>");
                    out.push_str(&inline(&b[i + 1..end].iter().collect::<String>()));
                    out.push_str("</em>");
                    i = end + 1;
                    continue;
                }
                out.push(b[i]);
                i += 1;
            }
            // [text](target)
            '[' => {
                if let Some((text, target, next)) = link_at(&b, i) {
                    match safe_href(&target) {
                        // A rejected scheme still shows its text — dropping the
                        // message silently would be worse than not linking it.
                        Some(href) => out.push_str(&format!(
                            "<a href=\"{href}\" rel=\"noopener noreferrer nofollow\" \
                             target=\"_blank\">{}</a>",
                            inline(&text)
                        )),
                        None => out.push_str(&inline(&text)),
                    }
                    i = next;
                    continue;
                }
                out.push('[');
                i += 1;
            }
            // Bare URLs become links too — people paste them constantly.
            'h' | 'H' if starts_url(&b, i) => {
                let end = url_end(&b, i);
                let url: String = b[i..end].iter().collect();
                match safe_href(&url) {
                    Some(href) => out.push_str(&format!(
                        "<a href=\"{href}\" rel=\"noopener noreferrer nofollow\" \
                         target=\"_blank\">{url}</a>"
                    )),
                    None => out.push_str(&url),
                }
                i = end;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    out
}

/// Index of the next occurrence of any of `chars` at or after `from`.
fn find(b: &[char], from: usize, chars: &[char]) -> Option<usize> {
    (from..b.len()).find(|i| chars.contains(&b[*i]))
}

/// Index of the next doubled `c` (`**`, `~~`) at or after `from`.
fn find_pair(b: &[char], from: usize, c: char) -> Option<usize> {
    (from..b.len().saturating_sub(1)).find(|i| b[*i] == c && b[*i + 1] == c)
}

/// Parse `[text](target)` at `i`, returning the parts and the index after it.
///
/// The target's parentheses are balanced rather than ended at the first `)`.
/// Plenty of real URLs contain them — Wikipedia's disambiguated titles are the
/// classic case — and stopping early truncates the link *and* leaves a stray
/// `)` in the message text.
fn link_at(b: &[char], i: usize) -> Option<(String, String, usize)> {
    let close = find(b, i + 1, &[']'])?;
    if b.get(close + 1) != Some(&'(') {
        return None;
    }
    let mut depth = 1usize;
    let mut end = close + 2;
    while end < b.len() {
        match b[end] {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    break;
                }
            }
            _ => {}
        }
        end += 1;
    }
    // Unclosed: not a link, so the text stays as typed.
    if depth != 0 {
        return None;
    }
    Some((
        b[i + 1..close].iter().collect(),
        b[close + 2..end].iter().collect(),
        end + 1,
    ))
}

/// Does a bare URL start at `i`?
fn starts_url(b: &[char], i: usize) -> bool {
    let rest: String = b[i..b.len().min(i + 8)]
        .iter()
        .collect::<String>()
        .to_ascii_lowercase();
    rest.starts_with("http://") || rest.starts_with("https://")
}

/// Where a bare URL ends: at whitespace, or at trailing punctuation that is far
/// more likely to be the sentence's than the URL's.
fn url_end(b: &[char], i: usize) -> usize {
    let mut end = i;
    while end < b.len() && !b[end].is_whitespace() {
        end += 1;
    }
    while end > i && matches!(b[end - 1], '.' | ',' | ')' | ';' | ':' | '!' | '?') {
        end -= 1;
    }
    end
}

/// Render a full message body: block structure plus inline formatting.
pub fn to_html(src: &str) -> String {
    let escaped = escape(src);
    let lines: Vec<&str> = escaped.lines().collect();
    let mut out = String::with_capacity(escaped.len() + 64);
    let mut i = 0;
    // Which list, if any, is currently open.
    let mut list: Option<&'static str> = None;

    let close_list = |out: &mut String, list: &mut Option<&'static str>| {
        if let Some(tag) = list.take() {
            out.push_str(&format!("</{tag}>"));
        }
    };

    while i < lines.len() {
        let line = lines[i];
        let trimmed = line.trim_start();

        // ``` fenced code — everything to the closing fence is literal.
        if let Some(rest) = trimmed.strip_prefix("```") {
            close_list(&mut out, &mut list);
            let lang = rest.trim();
            let mut body = String::new();
            i += 1;
            while i < lines.len() && !lines[i].trim_start().starts_with("```") {
                body.push_str(lines[i]);
                body.push('\n');
                i += 1;
            }
            i += 1; // consume the closing fence (or run off the end)
                    // The language becomes a class name, so take only its leading word
                    // and cap it. Filtering non-word characters out of the whole info
                    // string instead keeps every stray letter after them, which turned
                    // `rust" onload="alert(1)` into the class `lang-rustonloadalert1` —
                    // harmless, since the quotes were gone, but nonsense in the DOM.
            let safe: String = lang
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '+')
                .take(16)
                .collect();
            let cls = if safe.is_empty() {
                String::new()
            } else {
                format!(" class=\"lang-{safe}\"")
            };
            out.push_str(&format!("<pre{cls}><code>{}</code></pre>", body.trim_end()));
            continue;
        }

        // Blank line: ends a list, separates blocks.
        if trimmed.is_empty() {
            close_list(&mut out, &mut list);
            i += 1;
            continue;
        }

        // Horizontal rule.
        if matches!(trimmed, "---" | "***" | "___") {
            close_list(&mut out, &mut list);
            out.push_str("<hr/>");
            i += 1;
            continue;
        }

        // # Heading — h1..h3, capped so a message can't set page-level type.
        if let Some(level) = heading_level(trimmed) {
            close_list(&mut out, &mut list);
            let text = trimmed[level..].trim();
            let h = level.min(3);
            out.push_str(&format!("<h{h}>{}</h{h}>", inline(text)));
            i += 1;
            continue;
        }

        // > Blockquote — consecutive lines become one quote.
        if let Some(first) = trimmed.strip_prefix("&gt;") {
            close_list(&mut out, &mut list);
            let mut body = vec![first.trim().to_string()];
            i += 1;
            while i < lines.len() {
                match lines[i].trim_start().strip_prefix("&gt;") {
                    Some(more) => {
                        body.push(more.trim().to_string());
                        i += 1;
                    }
                    None => break,
                }
            }
            out.push_str(&format!(
                "<blockquote>{}</blockquote>",
                inline(&body.join("<br/>"))
            ));
            continue;
        }

        // - bullet / 1. ordered
        if let Some((tag, item)) = list_item(trimmed) {
            if list != Some(tag) {
                close_list(&mut out, &mut list);
                out.push_str(&format!("<{tag}>"));
                list = Some(tag);
            }
            out.push_str(&format!("<li>{}</li>", inline(item)));
            i += 1;
            continue;
        }

        // Paragraph: run of non-blank lines, single newlines kept as breaks
        // because in a message they're almost always deliberate.
        close_list(&mut out, &mut list);
        let mut body = vec![line.trim_end().to_string()];
        i += 1;
        while i < lines.len() {
            let l = lines[i].trim_start();
            if l.is_empty()
                || l.starts_with("```")
                || l.starts_with("&gt;")
                || heading_level(l).is_some()
                || list_item(l).is_some()
                || matches!(l, "---" | "***" | "___")
            {
                break;
            }
            body.push(lines[i].trim_end().to_string());
            i += 1;
        }
        out.push_str(&format!("<p>{}</p>", inline(&body.join("<br/>"))));
    }
    close_list(&mut out, &mut list);
    out
}

/// Render a single line with inline formatting only — no paragraphs, no lists.
/// Chat is a stream of lines, and wrapping each one in a `<p>` would fight the
/// scrollback's own spacing.
pub fn inline_to_html(src: &str) -> String {
    inline(&escape(src))
}

/// `#`-count for a heading line, if it is one.
fn heading_level(line: &str) -> Option<usize> {
    let n = line.chars().take_while(|c| *c == '#').count();
    (1..=6)
        .contains(&n)
        .then_some(n)
        .filter(|n| line.chars().nth(*n) == Some(' '))
}

/// The list tag and item text for a list line, if it is one.
fn list_item(line: &str) -> Option<(&'static str, &str)> {
    for p in ["- ", "* ", "+ "] {
        if let Some(rest) = line.strip_prefix(p) {
            return Some(("ul", rest));
        }
    }
    // `1. `, `2. ` …
    let digits = line.chars().take_while(char::is_ascii_digit).count();
    if digits > 0 && line[digits..].starts_with(". ") {
        return Some(("ol", &line[digits + 2..]));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- The security boundary. These are the tests that matter most. ----

    /// Every tag this renderer is allowed to emit. Anything else in the output
    /// must have come from the input, which is the one thing this module exists
    /// to prevent.
    const ALLOWED: [&str; 15] = [
        "p",
        "br",
        "strong",
        "em",
        "del",
        "code",
        "pre",
        "a",
        "h1",
        "h2",
        "h3",
        "blockquote",
        "ul",
        "ol",
        "li",
    ];

    /// Tag names appearing in `html`, opening and closing.
    fn tags(html: &str) -> Vec<String> {
        let b: Vec<char> = html.chars().collect();
        let mut out = Vec::new();
        for (i, c) in b.iter().enumerate() {
            if *c != '<' {
                continue;
            }
            let name: String = b[i + 1..]
                .iter()
                .skip_while(|c| **c == '/')
                .take_while(|c| c.is_ascii_alphanumeric())
                .collect();
            if !name.is_empty() {
                out.push(name.to_ascii_lowercase());
            }
        }
        out
    }

    #[test]
    fn markup_in_a_message_never_becomes_markup_in_the_dom() {
        for attack in [
            "<script>alert(1)</script>",
            "<img src=x onerror=alert(1)>",
            "<iframe src=evil></iframe>",
            "</p><script>alert(1)</script><p>",
            "<svg/onload=alert(1)>",
            "<style>body{display:none}</style>",
            "<!-- comment -->",
            "<a href=\"javascript:alert(1)\">x</a>",
            "<div onclick=alert(1)>x</div>",
            "<textarea></textarea><script>alert(1)</script>",
        ] {
            for html in [to_html(attack), inline_to_html(attack)] {
                // The invariant that matters is not "no <script>" — it's that
                // no tag reaches the DOM except the ones we chose to emit.
                // Asserting on substrings like "onerror" instead would fail on
                // an *escaped* payload, where that word is harmless text.
                for t in tags(&html) {
                    assert!(
                        ALLOWED.contains(&t.as_str()),
                        "{attack} produced <{t}>: {html}"
                    );
                }
                // The text is still shown — escaped, not swallowed.
                assert!(html.contains("&lt;"), "{attack} => {html}");
            }
        }
    }

    #[test]
    fn ordinary_formatting_only_ever_emits_allowed_tags() {
        // The allowlist above only means something if it also covers everything
        // the renderer legitimately produces.
        let doc = "# H\n\ntext **b** *i* ~~s~~ `c` [l](https://x.test)\n\n- a\n\n1. b\n\n\
                   > q\n\n```rust\nfn main(){}\n```\n\n---\n\nhttps://x.test/bare";
        let html = to_html(doc);
        for t in tags(&html) {
            assert!(
                ALLOWED.contains(&t.as_str()) || t == "hr",
                "unexpected <{t}>: {html}"
            );
        }
        for expect in [
            "<h1>",
            "<strong>",
            "<em>",
            "<del>",
            "<code>",
            "<pre",
            "<a href",
            "<ul>",
            "<ol>",
            "<blockquote>",
            "<hr/>",
        ] {
            assert!(html.contains(expect), "missing {expect}: {html}");
        }
    }

    #[test]
    fn only_http_https_and_mailto_ever_reach_an_href() {
        for bad in [
            "javascript:alert(1)",
            "JaVaScRiPt:alert(1)",
            "  javascript:alert(1)",
            "data:text/html;base64,PHNjcmlwdD4=",
            "vbscript:msgbox",
            "file:///etc/passwd",
            "java\tscript:alert(1)",
        ] {
            let html = inline_to_html(&format!("[click]({bad})"));
            assert!(!html.contains("href"), "{bad} produced a link: {html}");
            // The label survives as text, so the message isn't silently gutted.
            assert!(html.contains("click"), "{bad} lost its text: {html}");
        }
        for good in ["http://x.test/a", "https://x.test/a", "mailto:a@x.test"] {
            let html = inline_to_html(&format!("[click]({good})"));
            assert!(
                html.contains(&format!("href=\"{good}\"")),
                "{good} => {html}"
            );
            // External links must not hand the opener over.
            assert!(html.contains("rel=\"noopener noreferrer nofollow\""));
        }
    }

    #[test]
    fn a_link_target_cannot_break_out_of_the_attribute() {
        let html = inline_to_html("[x](https://x.test/\" onmouseover=\"alert(1))");
        // The quote is escaped, so the attribute can't be closed early.
        assert!(!html.contains("onmouseover=\"alert"), "{html}");
        assert!(!html.contains("\" onmouseover"), "{html}");
    }

    #[test]
    fn a_code_fence_language_cannot_inject_a_class_or_attribute() {
        let html = to_html("```rust\" onload=\"alert(1)\ncode\n```");
        assert_eq!(html, "<pre class=\"lang-rust\"><code>code</code></pre>");
    }

    // ---- Formatting. ----

    #[test]
    fn inline_formatting_renders() {
        assert_eq!(inline_to_html("**bold**"), "<strong>bold</strong>");
        assert_eq!(inline_to_html("*italic*"), "<em>italic</em>");
        assert_eq!(inline_to_html("_italic_"), "<em>italic</em>");
        assert_eq!(inline_to_html("~~gone~~"), "<del>gone</del>");
        assert_eq!(inline_to_html("`code`"), "<code>code</code>");
        assert_eq!(inline_to_html("**a _b_**"), "<strong>a <em>b</em></strong>");
    }

    #[test]
    fn code_spans_are_literal() {
        // The single most common way a naive renderer mangles a message.
        assert_eq!(inline_to_html("`a * b * c`"), "<code>a * b * c</code>");
        assert_eq!(
            inline_to_html("`**not bold**`"),
            "<code>**not bold**</code>"
        );
        // …and markup inside a code span is still escaped.
        let html = inline_to_html("`<script>`");
        assert_eq!(html, "<code>&lt;script&gt;</code>");
    }

    #[test]
    fn unclosed_markers_stay_as_typed() {
        // Half-typed emphasis is normal while composing; it must not eat the
        // rest of the message.
        assert_eq!(inline_to_html("2 * 3 = 6"), "2 * 3 = 6");
        assert_eq!(inline_to_html("**unclosed"), "**unclosed");
        assert_eq!(inline_to_html("a `b"), "a `b");
        assert_eq!(inline_to_html("[text](unclosed"), "[text](unclosed");
    }

    #[test]
    fn a_link_target_may_contain_balanced_parentheses() {
        // Found by sending a message in the running app: the parser stopped at
        // the first `)`, which truncated the URL *and* left the leftover `)`
        // sitting in the message text.
        let url = "https://en.wikipedia.org/wiki/Rust_(programming_language)";
        let html = inline_to_html(&format!("[Rust]({url})"));
        assert_eq!(
            html,
            format!(
                "<a href=\"{url}\" rel=\"noopener noreferrer nofollow\" \
                 target=\"_blank\">Rust</a>"
            )
        );
        // A rejected scheme with parentheses leaves no debris behind either.
        assert_eq!(inline_to_html("[click](javascript:alert(3))"), "click");
        // Genuinely unbalanced input isn't a link, so the syntax stays visible
        // as typed. (The bare URL inside it still autolinks — that rule doesn't
        // stop applying just because the surrounding brackets went nowhere.)
        let html = inline_to_html("[a](https://x.test/(");
        assert!(
            html.starts_with("[a]("),
            "the literal text survives: {html}"
        );
        assert!(!html.contains("</a>a"), "no truncated link debris: {html}");
    }

    #[test]
    fn bare_urls_become_links_without_swallowing_punctuation() {
        let html = inline_to_html("see https://x.test/a, then stop.");
        assert!(html.contains("href=\"https://x.test/a\""), "{html}");
        assert!(html.contains(">https://x.test/a</a>, then stop."), "{html}");
    }

    #[test]
    fn block_structure_renders() {
        assert_eq!(to_html("# Title"), "<h1>Title</h1>");
        assert_eq!(to_html("#### Deep"), "<h3>Deep</h3>", "capped at h3");
        assert_eq!(to_html("#nospace"), "<p>#nospace</p>");
        assert_eq!(to_html("- a\n- b"), "<ul><li>a</li><li>b</li></ul>");
        assert_eq!(to_html("1. a\n2. b"), "<ol><li>a</li><li>b</li></ol>");
        assert_eq!(to_html("> quoted"), "<blockquote>quoted</blockquote>");
        assert_eq!(to_html("---"), "<hr/>");
        assert_eq!(to_html("plain"), "<p>plain</p>");
    }

    #[test]
    fn a_fenced_block_keeps_its_contents_verbatim() {
        let html = to_html("```\n# not a heading\n- not a list\n```");
        assert!(html.starts_with("<pre><code>"), "{html}");
        assert!(html.contains("# not a heading"), "{html}");
        assert!(!html.contains("<h1>"), "{html}");
        assert!(!html.contains("<li>"), "{html}");
    }

    #[test]
    fn an_unclosed_fence_still_terminates() {
        // Otherwise a stray ``` while typing would hang the renderer.
        let html = to_html("```\nstuck");
        assert!(
            html.contains("stuck") && html.starts_with("<pre>"),
            "{html}"
        );
    }

    #[test]
    fn lists_and_quotes_close_properly() {
        let html = to_html("- a\n\nafter");
        assert_eq!(html, "<ul><li>a</li></ul><p>after</p>");
        let html = to_html("- a\n# H");
        assert_eq!(
            html, "<ul><li>a</li></ul><h1>H</h1>",
            "a heading ends the list"
        );
        let html = to_html("> a\n> b\n\nafter");
        assert_eq!(html, "<blockquote>a<br/>b</blockquote><p>after</p>");
    }

    #[test]
    fn single_newlines_inside_a_paragraph_are_breaks() {
        // In a message a newline is deliberate; collapsing it loses meaning.
        assert_eq!(to_html("one\ntwo"), "<p>one<br/>two</p>");
        assert_eq!(to_html("one\n\ntwo"), "<p>one</p><p>two</p>");
    }

    #[test]
    fn plain_text_survives_untouched() {
        // The overwhelmingly common case: someone types a sentence.
        assert_eq!(inline_to_html("hello there"), "hello there");
        assert_eq!(to_html("hello there"), "<p>hello there</p>");
        assert_eq!(inline_to_html(""), "");
        assert_eq!(to_html(""), "");
    }

    #[test]
    fn ampersands_and_quotes_round_trip_as_text() {
        assert_eq!(inline_to_html("Tom & Jerry"), "Tom &amp; Jerry");
        assert_eq!(inline_to_html("say \"hi\""), "say &quot;hi&quot;");
        // Double-escaping would show a literal `&amp;` to the user.
        assert!(!inline_to_html("a & b").contains("&amp;amp;"));
    }

    #[test]
    fn deeply_nested_input_does_not_blow_the_stack() {
        // `inline` recurses through emphasis; a pathological message shouldn't
        // be able to crash the tab.
        let bomb = "*".repeat(2000);
        let _ = inline_to_html(&bomb);
        let bomb = "**a".repeat(500);
        let _ = inline_to_html(&bomb);
        let bomb = format!("{}x{}", "[".repeat(500), "]".repeat(500));
        let _ = inline_to_html(&bomb);
    }
}
