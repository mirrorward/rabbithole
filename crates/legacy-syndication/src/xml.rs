//! Minimal, lenient XML pull tokenizer.
//!
//! Feeds in the wild are routinely malformed (unclosed tags, stray `&`,
//! truncated downloads), so this is a *scanner*, not a validator: it emits
//! a best-effort token stream and simply stops at end of input. It never
//! allocates for tokens (all borrows), never panics, and treats anything
//! it cannot understand as skippable. Comments, processing instructions,
//! and DOCTYPE declarations are consumed silently; CDATA is surfaced raw
//! (no entity decoding); text nodes are surfaced raw and entity-decoded by
//! the caller, so the XML and HTML decode layers stay distinct.

/// One pull event from the scanner. Borrowed from the input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Token<'a> {
    /// `<name attrs...>` or `<name attrs.../>`. `attrs` is the raw,
    /// unparsed attribute region — use [`attr`] to look values up.
    Open {
        name: &'a str,
        attrs: &'a str,
        self_closing: bool,
    },
    /// `</name>`.
    Close(&'a str),
    /// Raw character data between tags; entities are *not* decoded.
    Text(&'a str),
    /// Contents of a `<![CDATA[...]]>` section, raw.
    CData(&'a str),
}

/// Pull reader over a `&str`. All position arithmetic uses byte indices
/// returned by `find` on the same slices, so slicing stays on char
/// boundaries by construction.
pub(crate) struct Reader<'a> {
    src: &'a str,
    pos: usize,
}

impl<'a> Reader<'a> {
    pub(crate) fn new(src: &'a str) -> Self {
        Self { src, pos: 0 }
    }

    /// Next token, or `None` at (possibly premature) end of input.
    pub(crate) fn next_token(&mut self) -> Option<Token<'a>> {
        loop {
            if self.pos >= self.src.len() {
                return None;
            }
            let rest = &self.src[self.pos..];
            if !rest.starts_with('<') {
                // Text run up to the next tag (or EOF).
                let end = rest.find('<').unwrap_or(rest.len());
                self.pos += end;
                return Some(Token::Text(&rest[..end]));
            }
            if let Some(r) = rest.strip_prefix("<!--") {
                match r.find("-->") {
                    Some(i) => {
                        self.pos += 4 + i + 3;
                        continue;
                    }
                    // Truncated comment: drop the tail.
                    None => {
                        self.pos = self.src.len();
                        return None;
                    }
                }
            }
            if let Some(r) = rest.strip_prefix("<![CDATA[") {
                match r.find("]]>") {
                    Some(i) => {
                        self.pos += 9 + i + 3;
                        return Some(Token::CData(&r[..i]));
                    }
                    // Truncated CDATA: surface what we have.
                    None => {
                        self.pos = self.src.len();
                        return Some(Token::CData(r));
                    }
                }
            }
            if rest.starts_with("<!") || rest.starts_with("<?") {
                // DOCTYPE / processing instruction: skip to the next '>'.
                match rest.find('>') {
                    Some(i) => {
                        self.pos += i + 1;
                        continue;
                    }
                    None => {
                        self.pos = self.src.len();
                        return None;
                    }
                }
            }
            if let Some(r) = rest.strip_prefix("</") {
                match r.find('>') {
                    Some(i) => {
                        self.pos += 2 + i + 1;
                        return Some(Token::Close(r[..i].trim()));
                    }
                    None => {
                        self.pos = self.src.len();
                        return None;
                    }
                }
            }
            // Start tag. Lenient: a '>' inside a quoted attribute value would
            // end the tag early; well-formed feeds escape it.
            let body = &rest[1..];
            match body.find('>') {
                Some(i) => {
                    self.pos += 1 + i + 1;
                    let inner = &body[..i];
                    let (inner, self_closing) = match inner.strip_suffix('/') {
                        Some(s) => (s, true),
                        None => (inner, false),
                    };
                    let inner = inner.trim();
                    if inner.is_empty() {
                        continue; // "<>" or "</>" junk
                    }
                    let name_end = inner
                        .find(|c: char| c.is_whitespace())
                        .unwrap_or(inner.len());
                    let (name, attrs) = (&inner[..name_end], inner[name_end..].trim());
                    return Some(Token::Open {
                        name,
                        attrs,
                        self_closing,
                    });
                }
                None => {
                    self.pos = self.src.len();
                    return None;
                }
            }
        }
    }
}

/// Look up an attribute (ASCII case-insensitive) in a raw attribute
/// region. Values may be single- or double-quoted (an unterminated quote
/// runs to end of region); bare `name=value` is tolerated. The value is
/// XML-entity-decoded.
pub(crate) fn attr(attrs: &str, want: &str) -> Option<String> {
    let mut rest = attrs;
    loop {
        rest = rest.trim_start();
        if rest.is_empty() {
            return None;
        }
        let name_end = rest
            .find(|c: char| c.is_whitespace() || c == '=')
            .unwrap_or(rest.len());
        let name = &rest[..name_end];
        rest = rest[name_end..].trim_start();
        let Some(after_eq) = rest.strip_prefix('=') else {
            // Bare attribute with no value.
            if name.eq_ignore_ascii_case(want) {
                return Some(String::new());
            }
            if name.is_empty() {
                return None; // junk like a lone '=' — avoid spinning
            }
            continue;
        };
        let after_eq = after_eq.trim_start();
        let (value, next) = match after_eq.chars().next() {
            Some(q @ ('"' | '\'')) => {
                let body = &after_eq[q.len_utf8()..];
                match body.find(q) {
                    Some(i) => (&body[..i], &body[i + 1..]),
                    None => (body, ""),
                }
            }
            _ => {
                let end = after_eq
                    .find(|c: char| c.is_whitespace())
                    .unwrap_or(after_eq.len());
                (&after_eq[..end], &after_eq[end..])
            }
        };
        if name.eq_ignore_ascii_case(want) {
            return Some(crate::text::decode_entities(value));
        }
        rest = next;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all(src: &str) -> Vec<Token<'_>> {
        let mut r = Reader::new(src);
        let mut out = Vec::new();
        while let Some(t) = r.next_token() {
            out.push(t);
        }
        out
    }

    #[test]
    fn tokenizes_elements_text_and_cdata() {
        let toks = all("<a href=\"x\">hi<br/><![CDATA[<raw> & stuff]]></a>");
        assert_eq!(
            toks,
            vec![
                Token::Open {
                    name: "a",
                    attrs: "href=\"x\"",
                    self_closing: false
                },
                Token::Text("hi"),
                Token::Open {
                    name: "br",
                    attrs: "",
                    self_closing: true
                },
                Token::CData("<raw> & stuff"),
                Token::Close("a"),
            ]
        );
    }

    #[test]
    fn skips_comments_pi_and_doctype() {
        let toks = all("<?xml version=\"1.0\"?><!DOCTYPE html><!-- <fake> -->real<x/>");
        assert_eq!(
            toks,
            vec![
                Token::Text("real"),
                Token::Open {
                    name: "x",
                    attrs: "",
                    self_closing: true
                },
            ]
        );
    }

    #[test]
    fn truncated_input_never_panics() {
        for src in [
            "<",
            "<a",
            "<a href=\"",
            "</",
            "</a",
            "<!--",
            "<!-- never closed",
            "<![CDATA[",
            "<![CDATA[dangling",
            "<?xml",
            "<!DOCT",
            "text then <",
            "<>",
            "</>",
        ] {
            let _ = all(src); // must not panic
        }
        // Truncated CDATA still surfaces its payload.
        assert_eq!(all("<![CDATA[tail"), vec![Token::CData("tail")]);
    }

    #[test]
    fn attr_lookup_is_lenient() {
        let attrs = "href='http://e.com/?a=1&amp;b=2' rel=\"alternate\" checked broken=";
        assert_eq!(
            attr(attrs, "href").as_deref(),
            Some("http://e.com/?a=1&b=2")
        );
        assert_eq!(attr(attrs, "REL").as_deref(), Some("alternate"));
        assert_eq!(attr(attrs, "checked").as_deref(), Some(""));
        assert_eq!(attr(attrs, "missing"), None);
        assert_eq!(
            attr("a=\"unterminated", "a").as_deref(),
            Some("unterminated")
        );
        assert_eq!(attr("= = =", "x"), None); // junk must terminate
    }
}
