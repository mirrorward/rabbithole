//! A small, total JSON reader.
//!
//! The discovery parsers used to find fields by splitting on `{` and searching
//! for `"key":`, which works only while every object is flat. The Looking
//! Glass nests (`endpoints: {quic, ws}`, and a whole `descriptor` object per
//! row), and under that heuristic a nested object starts a new "row" and its
//! fields leak into the wrong entry. So the shape is actually parsed.
//!
//! `serde_json` isn't used here because this crate is compiled into the wasm
//! SPA, where one endpoint's worth of parsing is not worth serde's derive
//! machinery in the bundle. What is needed is small: read a document, walk to a
//! field, read a string or an array of strings.
//!
//! Total by construction: every entry point returns `Result`/`Option`, the
//! parser is depth-limited so a hostile reply can't blow the stack, and no
//! path can panic on arbitrary bytes.

/// Nesting a reply may reach. Discovery documents are three or four levels
/// deep; anything beyond this is someone trying to exhaust the stack.
const MAX_DEPTH: usize = 32;

/// A parsed JSON value. Numbers keep their `f64` value — nothing here needs
/// integer precision beyond what a percentage or a timestamp requires.
#[derive(Debug, Clone, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<Json>),
    /// Object members in document order. A `Vec` rather than a map: these
    /// documents have a handful of keys and lookup is by name once.
    Obj(Vec<(String, Json)>),
}

impl Json {
    /// The value of object field `name`, if this is an object that has it.
    pub fn get(&self, name: &str) -> Option<&Json> {
        match self {
            Json::Obj(fields) => fields.iter().find(|(k, _)| k == name).map(|(_, v)| v),
            _ => None,
        }
    }

    /// Field `name` as a string, if it is one.
    pub fn str_field(&self, name: &str) -> Option<&str> {
        match self.get(name)? {
            Json::Str(s) => Some(s.as_str()),
            _ => None,
        }
    }

    /// Field `name` as an array's items, if it is an array.
    pub fn arr_field(&self, name: &str) -> &[Json] {
        match self.get(name) {
            Some(Json::Arr(items)) => items,
            _ => &[],
        }
    }

    /// Field `name` as a list of strings, skipping any item that isn't one.
    pub fn str_array_field(&self, name: &str) -> Vec<String> {
        self.arr_field(name)
            .iter()
            .filter_map(|v| match v {
                Json::Str(s) => Some(s.clone()),
                _ => None,
            })
            .collect()
    }

    /// This value as a string, if it is one.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Json::Str(s) => Some(s.as_str()),
            _ => None,
        }
    }
}

/// Parse a complete JSON document. Trailing whitespace is fine; trailing
/// content is not.
pub fn parse(text: &str) -> Result<Json, String> {
    let bytes = text.as_bytes();
    let mut p = Parser { bytes, i: 0 };
    p.skip_ws();
    let value = p.value(0)?;
    p.skip_ws();
    if p.i != bytes.len() {
        return Err(format!("trailing content at byte {}", p.i));
    }
    Ok(value)
}

struct Parser<'a> {
    bytes: &'a [u8],
    i: usize,
}

impl Parser<'_> {
    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.i).copied()
    }

    fn skip_ws(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.i += 1;
        }
    }

    fn expect(&mut self, want: u8) -> Result<(), String> {
        if self.peek() == Some(want) {
            self.i += 1;
            Ok(())
        } else {
            Err(format!("expected {:?} at byte {}", want as char, self.i))
        }
    }

    fn literal(&mut self, word: &str, value: Json) -> Result<Json, String> {
        if self.bytes[self.i..].starts_with(word.as_bytes()) {
            self.i += word.len();
            Ok(value)
        } else {
            Err(format!("unexpected input at byte {}", self.i))
        }
    }

    fn value(&mut self, depth: usize) -> Result<Json, String> {
        if depth > MAX_DEPTH {
            return Err("nested too deeply".to_string());
        }
        match self.peek() {
            Some(b'{') => self.object(depth),
            Some(b'[') => self.array(depth),
            Some(b'"') => self.string().map(Json::Str),
            Some(b't') => self.literal("true", Json::Bool(true)),
            Some(b'f') => self.literal("false", Json::Bool(false)),
            Some(b'n') => self.literal("null", Json::Null),
            Some(_) => self.number(),
            None => Err("unexpected end of input".to_string()),
        }
    }

    fn object(&mut self, depth: usize) -> Result<Json, String> {
        self.expect(b'{')?;
        let mut fields = Vec::new();
        self.skip_ws();
        if self.peek() == Some(b'}') {
            self.i += 1;
            return Ok(Json::Obj(fields));
        }
        loop {
            self.skip_ws();
            let key = self.string()?;
            self.skip_ws();
            self.expect(b':')?;
            self.skip_ws();
            let value = self.value(depth + 1)?;
            fields.push((key, value));
            self.skip_ws();
            match self.peek() {
                Some(b',') => self.i += 1,
                Some(b'}') => {
                    self.i += 1;
                    return Ok(Json::Obj(fields));
                }
                _ => return Err(format!("expected ',' or '}}' at byte {}", self.i)),
            }
        }
    }

    fn array(&mut self, depth: usize) -> Result<Json, String> {
        self.expect(b'[')?;
        let mut items = Vec::new();
        self.skip_ws();
        if self.peek() == Some(b']') {
            self.i += 1;
            return Ok(Json::Arr(items));
        }
        loop {
            self.skip_ws();
            items.push(self.value(depth + 1)?);
            self.skip_ws();
            match self.peek() {
                Some(b',') => self.i += 1,
                Some(b']') => {
                    self.i += 1;
                    return Ok(Json::Arr(items));
                }
                _ => return Err(format!("expected ',' or ']' at byte {}", self.i)),
            }
        }
    }

    fn string(&mut self) -> Result<String, String> {
        self.expect(b'"')?;
        let mut out = String::new();
        loop {
            let Some(b) = self.peek() else {
                return Err("unterminated string".to_string());
            };
            self.i += 1;
            match b {
                b'"' => return Ok(out),
                b'\\' => {
                    let Some(esc) = self.peek() else {
                        return Err("unterminated escape".to_string());
                    };
                    self.i += 1;
                    match esc {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{8}'),
                        b'f' => out.push('\u{c}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => out.push(self.unicode_escape()?),
                        _ => return Err(format!("bad escape at byte {}", self.i)),
                    }
                }
                // Multi-byte UTF-8 arrives as its own bytes; copy them through
                // untouched rather than reinterpreting each as a char.
                _ => {
                    let start = self.i - 1;
                    while self
                        .peek()
                        .is_some_and(|n| n != b'"' && n != b'\\' && n >= 0x80)
                    {
                        self.i += 1;
                    }
                    match std::str::from_utf8(&self.bytes[start..self.i]) {
                        Ok(s) => out.push_str(s),
                        Err(_) => return Err(format!("invalid UTF-8 at byte {start}")),
                    }
                }
            }
        }
    }

    /// A `\uXXXX` escape, joining a surrogate pair when one follows.
    fn unicode_escape(&mut self) -> Result<char, String> {
        let hi = self.hex4()?;
        // A lone surrogate is not a character. Pair it if the partner is
        // there, and otherwise substitute rather than fail: a display name
        // with a broken escape shouldn't cost the whole listing.
        if (0xD800..0xDC00).contains(&hi) {
            if self.bytes[self.i..].starts_with(b"\\u") {
                let save = self.i;
                self.i += 2;
                let lo = self.hex4()?;
                if (0xDC00..0xE000).contains(&lo) {
                    let c = 0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00);
                    return Ok(char::from_u32(c).unwrap_or('\u{fffd}'));
                }
                self.i = save;
            }
            return Ok('\u{fffd}');
        }
        Ok(char::from_u32(hi).unwrap_or('\u{fffd}'))
    }

    fn hex4(&mut self) -> Result<u32, String> {
        let end = self.i + 4;
        let slice = self
            .bytes
            .get(self.i..end)
            .ok_or_else(|| "truncated \\u escape".to_string())?;
        let text = std::str::from_utf8(slice).map_err(|_| "bad \\u escape".to_string())?;
        let value = u32::from_str_radix(text, 16).map_err(|_| "bad \\u escape".to_string())?;
        self.i = end;
        Ok(value)
    }

    fn number(&mut self) -> Result<Json, String> {
        let start = self.i;
        while self
            .peek()
            .is_some_and(|b| matches!(b, b'0'..=b'9' | b'-' | b'+' | b'.' | b'e' | b'E'))
        {
            self.i += 1;
        }
        let text = std::str::from_utf8(&self.bytes[start..self.i])
            .map_err(|_| format!("bad number at byte {start}"))?;
        text.parse::<f64>()
            .map(Json::Num)
            .map_err(|_| format!("bad number {text:?} at byte {start}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_shapes_both_services_serve() {
        let v = parse(
            r#"{"ok":true,"n":3,"burrows":[{"name":"a","endpoints":{"ws":"ws://x:1"},
               "listeners":["quic","ws"],"status":"online"}]}"#,
        )
        .expect("parses");
        assert_eq!(v.get("ok"), Some(&Json::Bool(true)));
        assert_eq!(v.get("n"), Some(&Json::Num(3.0)));
        let row = &v.arr_field("burrows")[0];
        assert_eq!(row.str_field("name"), Some("a"));
        // The nesting the old brace-splitting heuristic got wrong.
        assert_eq!(
            row.get("endpoints").and_then(|e| e.str_field("ws")),
            Some("ws://x:1")
        );
        assert_eq!(row.str_array_field("listeners"), vec!["quic", "ws"]);
    }

    #[test]
    fn a_nested_object_does_not_leak_into_its_neighbour() {
        // Precisely the bug: with `endpoints` nested, splitting on `{` made the
        // inner object look like the next burrow, and the burrow after it
        // inherited fields it never declared.
        let v = parse(
            r#"{"burrows":[
                 {"name":"first","endpoints":{"ws":"ws://first:1"}},
                 {"name":"second"}
               ]}"#,
        )
        .unwrap();
        let rows = v.arr_field("burrows");
        assert_eq!(rows.len(), 2, "two burrows, not three");
        assert_eq!(rows[1].str_field("name"), Some("second"));
        assert!(
            rows[1].get("endpoints").is_none(),
            "the second burrow declared no endpoints and must not borrow any"
        );
    }

    #[test]
    fn strings_survive_escapes_and_non_ascii() {
        let v = parse(r#"{"s":"a \"q\" line\nwith ü and 😀 \\ /"}"#).unwrap();
        assert_eq!(v.str_field("s"), Some("a \"q\" line\nwith ü and 😀 \\ /"));
        // Non-ASCII arriving raw, not escaped.
        let v = parse("{\"s\":\"ünïcödé 🐇\"}").unwrap();
        assert_eq!(v.str_field("s"), Some("ünïcödé 🐇"));
    }

    #[test]
    fn a_lone_surrogate_is_substituted_rather_than_failing_the_listing() {
        let v = parse(r#"{"s":"x\ud800y"}"#).unwrap();
        assert_eq!(v.str_field("s"), Some("x\u{fffd}y"));
    }

    #[test]
    fn malformed_input_is_an_error_not_a_panic() {
        for bad in [
            "",
            "{",
            "}",
            "[",
            r#"{"a"}"#,
            r#"{"a":}"#,
            r#"{"a":1,}"#,
            r#"["#,
            "tru",
            r#"{"a":"unterminated"#,
            r#"{"a":01f}"#,
            "{} extra",
            r#"{"a":"\u00"}"#,
        ] {
            assert!(parse(bad).is_err(), "{bad:?} should not parse");
        }
    }

    #[test]
    fn deep_nesting_is_refused_rather_than_blowing_the_stack() {
        let deep = format!("{}{}", "[".repeat(500), "]".repeat(500));
        assert!(parse(&deep).is_err());
    }

    #[test]
    fn arbitrary_bytes_never_panic() {
        // A discovery client reads replies from hosts it did not choose.
        let mut seed = 0x12345678u32;
        for _ in 0..2_000 {
            let len = (seed % 40) as usize;
            let s: String = (0..len)
                .map(|_| {
                    seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                    const ALPHABET: &[u8] = b"{}[]\":,0123456789truefalsnul \\/\xe9";
                    ALPHABET[(seed >> 16) as usize % ALPHABET.len()] as char
                })
                .collect();
            let _ = parse(&s);
        }
    }

    #[test]
    fn accessors_are_total_over_the_wrong_shape() {
        let v = parse(r#"{"a":1,"b":"x","c":[1,"two",null]}"#).unwrap();
        assert!(v.get("missing").is_none());
        assert!(v.str_field("a").is_none(), "a number is not a string");
        assert!(v.arr_field("b").is_empty(), "a string is not an array");
        assert_eq!(v.str_array_field("c"), vec!["two"], "non-strings skipped");
        assert!(Json::Num(1.0).get("anything").is_none());
    }
}
