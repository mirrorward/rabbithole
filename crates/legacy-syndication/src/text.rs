//! HTML-to-text summarization and entity decoding.
//!
//! Feed item bodies are usually HTML (RSS `description`, Atom
//! `type="html"` constructs). Boards want plain text, so
//! [`html_to_text`] strips tags — block-ish tags (`<p>`, `<br>`, `<li>`,
//! …) become a space so words in adjacent blocks don't fuse, while inline
//! formatting tags (`<b>`, `<em>`, `<a>`, …) vanish so `park<b>?</b>`
//! stays `park?` — drops `<script>`/`<style>` bodies and comments
//! entirely, decodes entities, collapses all whitespace runs to single
//! spaces, and caps the result at a char count — truncation always lands
//! on a char boundary and is marked with `…`.
//!
//! Stripping happens *before* decoding on purpose: `&lt;b&gt;` in HTML is
//! visible text ("<b>"), not markup, and must survive.

/// Decode the common XML/HTML entities: `&amp;` `&lt;` `&gt;` `&quot;`
/// `&apos;` `&nbsp;` plus numeric `&#39;` / `&#x27;` forms. `&nbsp;`
/// becomes a plain space. Unknown or unterminated entities are left
/// verbatim (lenient), and decoding is single-pass, so `&amp;lt;`
/// yields the literal text `&lt;`.
pub fn decode_entities(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        let cand = &rest[i..];
        match parse_entity(cand) {
            Some((ch, len)) => {
                out.push(ch);
                rest = &cand[len..];
            }
            None => {
                out.push('&');
                rest = &cand[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// Decode one entity at the start of `s` (which begins with `&`).
/// Returns the char and the total byte length consumed.
fn parse_entity(s: &str) -> Option<(char, usize)> {
    let semi = s[1..].find(';')?;
    if semi == 0 || semi > 10 {
        return None; // empty or implausibly long — treat '&' as literal
    }
    let body = &s[1..1 + semi];
    let ch = match body {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" => '\'',
        "nbsp" => ' ',
        _ => {
            let num = body.strip_prefix('#')?;
            let cp = match num.strip_prefix(['x', 'X']) {
                Some(hex) => u32::from_str_radix(hex, 16).ok()?,
                None => num.parse::<u32>().ok()?,
            };
            char::from_u32(cp).filter(|c| *c != '\0')?
        }
    };
    Some((ch, semi + 2))
}

/// Collapse every run of whitespace (including NBSP and newlines) to a
/// single space and trim the ends.
pub fn collapse_whitespace(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for word in input.split_whitespace() {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(word);
    }
    out
}

/// Cap `text` at `max_chars` characters (not bytes). When truncation
/// happens the last kept char is replaced by `…`, so the result never
/// exceeds `max_chars` chars and always ends on a char boundary.
pub fn cap_chars(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    if max_chars == 0 {
        return String::new();
    }
    let mut out: String = text.chars().take(max_chars - 1).collect();
    while out.ends_with(' ') {
        out.pop();
    }
    out.push('…');
    out
}

/// Convert an HTML fragment into collapsed plain text capped at
/// `max_chars` characters. See the module docs for the pipeline.
pub fn html_to_text(html: &str, max_chars: usize) -> String {
    let mut stripped = String::with_capacity(html.len());
    let mut rest = html;
    loop {
        let Some(i) = rest.find('<') else {
            stripped.push_str(rest);
            break;
        };
        stripped.push_str(&rest[..i]);
        let tag = &rest[i..];
        // Inline formatting tags vanish; everything else (blocks,
        // comments, script/style containers) becomes a word separator.
        if !is_inline_tag(tag) {
            stripped.push(' ');
        }
        if let Some(r) = tag.strip_prefix("<!--") {
            match r.find("-->") {
                Some(j) => rest = &r[j + 3..],
                None => break, // truncated comment: drop the tail
            }
            continue;
        }
        if let Some(r) = skip_container(tag, "script").or_else(|| skip_container(tag, "style")) {
            rest = r;
            continue;
        }
        match tag.find('>') {
            Some(j) => rest = &tag[j + 1..],
            None => break, // truncated tag: drop the tail
        }
    }
    cap_chars(&collapse_whitespace(&decode_entities(&stripped)), max_chars)
}

/// Does `tag` (starting with `<`) open or close an inline formatting
/// element? Those are dropped without inserting a word separator.
fn is_inline_tag(tag: &str) -> bool {
    const INLINE: &[&str] = &[
        "a", "abbr", "b", "cite", "code", "del", "em", "i", "ins", "kbd", "mark", "q", "s",
        "small", "span", "strong", "sub", "sup", "time", "u", "var", "wbr",
    ];
    let body = tag[1..].strip_prefix('/').unwrap_or(&tag[1..]);
    let end = body
        .find(|c: char| !c.is_ascii_alphanumeric())
        .unwrap_or(body.len());
    let name = &body[..end];
    !name.is_empty() && INLINE.iter().any(|t| t.eq_ignore_ascii_case(name))
}

/// If `tag` (starting with `<`) opens `name`, skip past the matching
/// `</name ...>` (ASCII case-insensitive) and return the remainder; the
/// element's contents are discarded. Unclosed containers swallow the rest
/// of the input (returning `""`).
fn skip_container<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
    let body = tag.get(1..1 + name.len())?;
    if !body.eq_ignore_ascii_case(name) {
        return None;
    }
    // Next char must end the tag name ("<script>", "<script src=..."),
    // so "<scripted>" doesn't match.
    match tag[1 + name.len()..].chars().next() {
        Some(c) if c.is_alphanumeric() => return None,
        None => return Some(""), // truncated right after the name
        _ => {}
    }
    let close = format!("</{name}");
    let Some(i) = find_ascii_ci(tag, &close) else {
        return Some(""); // never closed: discard the tail
    };
    let after = &tag[i..];
    match after.find('>') {
        Some(j) => Some(&after[j + 1..]),
        None => Some(""),
    }
}

/// Byte-wise ASCII case-insensitive substring search (needle must be
/// ASCII, which both callers guarantee).
fn find_ascii_ci(hay: &str, needle: &str) -> Option<usize> {
    hay.as_bytes()
        .windows(needle.len())
        .position(|w| w.eq_ignore_ascii_case(needle.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_named_and_numeric_entities() {
        assert_eq!(
            decode_entities("Tom &amp; Jerry &lt;3 &gt; &quot;hi&quot; &apos;yo&apos;"),
            "Tom & Jerry <3 > \"hi\" 'yo'"
        );
        assert_eq!(decode_entities("it&#39;s &#x41;&#X42;"), "it's AB");
        assert_eq!(decode_entities("a&nbsp;b"), "a b");
        assert_eq!(decode_entities("&#x1F407;"), "\u{1F407}"); // rabbit
    }

    #[test]
    fn leaves_unknown_and_broken_entities_alone() {
        assert_eq!(decode_entities("&unknown; &mdash;"), "&unknown; &mdash;");
        assert_eq!(decode_entities("100% &; & plain"), "100% &; & plain");
        assert_eq!(decode_entities("dangling &amp"), "dangling &amp");
        assert_eq!(
            decode_entities("&#xZZ; &#; &#0; &#x110000;"),
            "&#xZZ; &#; &#0; &#x110000;"
        );
    }

    #[test]
    fn decoding_is_single_pass() {
        // Double-escaped input decodes exactly one layer.
        assert_eq!(decode_entities("&amp;lt;b&amp;gt;"), "&lt;b&gt;");
    }

    #[test]
    fn strips_tags_and_separates_blocks() {
        assert_eq!(
            html_to_text("<p>Hello <b>world</b></p><p>again</p>", 100),
            "Hello world again"
        );
        assert_eq!(
            html_to_text("<a href=\"http://x\">link</a> text", 100),
            "link text"
        );
    }

    #[test]
    fn drops_comments_scripts_and_styles() {
        assert_eq!(html_to_text("a<!-- hidden <b>bold</b> -->b", 100), "a b");
        assert_eq!(
            html_to_text("before<script>var x = '<p>evil</p>';</script>after", 100),
            "before after"
        );
        assert_eq!(
            html_to_text("x<STYLE type=\"text/css\">p { color: red }</STYLE>y", 100),
            "x y"
        );
        // "<scripted>" is a normal (unknown) tag, not a script container.
        assert_eq!(html_to_text("<scripted>keep</scripted>", 100), "keep");
        // Unclosed script swallows the tail rather than leaking code.
        assert_eq!(html_to_text("keep<script>var leak;", 100), "keep");
    }

    #[test]
    fn inline_tags_do_not_split_words() {
        assert_eq!(html_to_text("park<b>?</b> now", 100), "park? now");
        assert_eq!(html_to_text("wor<em>l</em>d", 100), "world");
        assert_eq!(
            html_to_text("see <a href=\"http://x\">this</a>.", 100),
            "see this."
        );
        // Blocks and line breaks still separate.
        assert_eq!(html_to_text("line1<br/>line2", 100), "line1 line2");
        assert_eq!(html_to_text("<li>one</li><li>two</li>", 100), "one two");
    }

    #[test]
    fn escaped_markup_survives_as_text() {
        // &lt;b&gt; is *visible* text in HTML and must not be stripped.
        assert_eq!(
            html_to_text("use the &lt;b&gt; tag", 100),
            "use the <b> tag"
        );
    }

    #[test]
    fn collapses_whitespace() {
        assert_eq!(html_to_text("  a\n\n\t b&nbsp;&nbsp;c\r\n ", 100), "a b c");
    }

    #[test]
    fn caps_on_char_boundary() {
        // 4 chars, multibyte: truncating must never split a char.
        assert_eq!(cap_chars("héllo wörld", 6), "héllo…");
        assert_eq!(cap_chars("héllo", 5), "héllo"); // exact fit: untouched
        assert_eq!(cap_chars("日本語のテキスト", 4), "日本語…");
        assert_eq!(cap_chars("abc", 0), "");
        assert_eq!(cap_chars("", 10), "");
        let capped = html_to_text("<p>aaaa bbbb cccc dddd</p>", 10);
        assert!(capped.chars().count() <= 10, "{capped:?}");
        assert!(capped.ends_with('…'));
    }

    #[test]
    fn never_panics_on_garbage() {
        for junk in [
            "<",
            "<<",
            "<a",
            "&",
            "&#",
            "<!--",
            "<script",
            "<script>",
            "\u{0}\u{fffd}<>&#x;<![CDATA[",
            "a<b>c<",
            "&&&&&&",
            "<>",
        ] {
            let _ = html_to_text(junk, 16);
            let _ = decode_entities(junk);
        }
    }
}
