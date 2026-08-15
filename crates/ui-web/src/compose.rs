//! The formatting bar's text edits.
//!
//! Every button on the composer's toolbar does the same kind of thing: take the
//! draft, a selection, and a format, and return a new draft plus where the
//! selection should end up. That's a pure function, so it lives here and is
//! host-tested — the DOM part is only reading `selectionStart`, writing the
//! value back, and restoring the selection.
//!
//! Two behaviours are worth stating, because they're what separate a formatting
//! bar that feels right from one that fights you:
//!
//! * **The buttons toggle.** Pressing Bold on text that is already bold removes
//!   the markers. Every editor works this way and its absence is immediately
//!   noticeable.
//! * **The cursor lands where you'd keep typing.** With no selection, Bold
//!   inserts `****` and puts the caret in the middle. With a selection, the
//!   selection stays selected so you can hit Italic next.
//!
//! Offsets are **character** indices, not bytes: they come from and go back to
//! the DOM's selection API, which counts UTF-16 units. For the BMP characters
//! that make up ordinary text these agree, and the alternative — byte offsets —
//! would panic on the first accented character someone types.

/// A formatting action from the toolbar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Bold,
    Italic,
    Strike,
    Code,
    Link,
    Quote,
    Bullet,
    Heading,
}

impl Format {
    /// The marker this format wraps a selection in, if it's an inline one.
    fn marker(self) -> Option<&'static str> {
        match self {
            Format::Bold => Some("**"),
            Format::Italic => Some("*"),
            Format::Strike => Some("~~"),
            Format::Code => Some("`"),
            _ => None,
        }
    }

    /// The prefix this format puts on each selected line, if it's a line one.
    fn line_prefix(self) -> Option<&'static str> {
        match self {
            Format::Quote => Some("> "),
            Format::Bullet => Some("- "),
            Format::Heading => Some("# "),
            _ => None,
        }
    }

    /// Button label, accessible name, and keyboard shortcut key (with ⌘/Ctrl).
    pub fn button(self) -> (&'static str, &'static str, Option<char>) {
        match self {
            Format::Bold => ("B", "Bold", Some('b')),
            Format::Italic => ("I", "Italic", Some('i')),
            Format::Strike => ("S", "Strikethrough", None),
            Format::Code => ("<>", "Code", Some('e')),
            Format::Link => ("\u{1f517}", "Link", Some('k')),
            Format::Quote => ("\u{201c}", "Quote", None),
            Format::Bullet => ("\u{2022}", "Bulleted list", None),
            Format::Heading => ("H", "Heading", None),
        }
    }
}

/// The toolbar, in order.
pub const TOOLBAR: [Format; 8] = [
    Format::Bold,
    Format::Italic,
    Format::Strike,
    Format::Code,
    Format::Link,
    Format::Quote,
    Format::Bullet,
    Format::Heading,
];

/// The result of a formatting action: the new draft and the selection to
/// restore.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edit {
    pub text: String,
    pub start: usize,
    pub end: usize,
}

/// Apply `fmt` to `text` over the selection `[start, end)`.
///
/// Out-of-range or reversed selections are clamped rather than rejected: they
/// come from the DOM, and a formatting button should never be the thing that
/// panics a tab.
pub fn apply(text: &str, start: usize, end: usize, fmt: Format) -> Edit {
    let chars: Vec<char> = text.chars().collect();
    let (start, end) = clamp(chars.len(), start, end);

    if let Some(prefix) = fmt.line_prefix() {
        return apply_line_prefix(&chars, start, end, prefix);
    }
    if fmt == Format::Link {
        return apply_link(&chars, start, end);
    }
    let marker = fmt.marker().expect("every non-line format has a marker");
    apply_marker(&chars, start, end, marker)
}

/// Keep a selection inside the text and the right way round.
fn clamp(len: usize, start: usize, end: usize) -> (usize, usize) {
    let (a, b) = if start <= end {
        (start, end)
    } else {
        (end, start)
    };
    (a.min(len), b.min(len))
}

fn slice(chars: &[char], a: usize, b: usize) -> String {
    chars[a..b].iter().collect()
}

/// Wrap (or unwrap) the selection in an inline marker.
fn apply_marker(chars: &[char], start: usize, end: usize, marker: &str) -> Edit {
    let m: Vec<char> = marker.chars().collect();
    let n = m.len();
    let selected = slice(chars, start, end);

    // Already wrapped, markers *inside* the selection: `**bold**` selected whole.
    if selected.starts_with(marker)
        && selected.ends_with(marker)
        && selected.chars().count() >= 2 * n
    {
        let inner = slice(chars, start + n, end - n);
        let text = format!(
            "{}{inner}{}",
            slice(chars, 0, start),
            slice(chars, end, chars.len())
        );
        return Edit {
            text,
            start,
            end: end - 2 * n,
        };
    }
    // Already wrapped, markers *outside* the selection: `bold` selected, `**`
    // sitting either side of it.
    if start >= n
        && end + n <= chars.len()
        && slice(chars, start - n, start) == marker
        && slice(chars, end, end + n) == marker
    {
        let text = format!(
            "{}{selected}{}",
            slice(chars, 0, start - n),
            slice(chars, end + n, chars.len())
        );
        return Edit {
            text,
            start: start - n,
            end: end - n,
        };
    }

    let text = format!(
        "{}{marker}{selected}{marker}{}",
        slice(chars, 0, start),
        slice(chars, end, chars.len())
    );
    // Empty selection: park the caret between the markers so typing lands
    // inside them. Otherwise keep the selection, so formats can be stacked.
    Edit {
        text,
        start: start + n,
        end: end + n,
    }
}

/// Wrap the selection in a link, leaving the caret in the URL slot — which is
/// the part you always have to fill in.
fn apply_link(chars: &[char], start: usize, end: usize) -> Edit {
    let selected = slice(chars, start, end);
    let label = if selected.is_empty() {
        "text"
    } else {
        &selected
    };
    let text = format!(
        "{}[{label}](){}",
        slice(chars, 0, start),
        slice(chars, end, chars.len())
    );
    // `[label](` is label.len() + 3 characters; the caret goes after it.
    let caret = start + label.chars().count() + 3;
    Edit {
        text,
        start: caret,
        end: caret,
    }
}

/// Add — or, if every line already has it, remove — a prefix on each line the
/// selection touches.
fn apply_line_prefix(chars: &[char], start: usize, end: usize, prefix: &str) -> Edit {
    let (from, to) = (line_start(chars, start), line_end(chars, end));
    let block = slice(chars, from, to);
    let lines: Vec<&str> = block.split('\n').collect();
    let all_prefixed = lines.iter().all(|l| l.starts_with(prefix));

    let rebuilt: Vec<String> = lines
        .iter()
        .map(|l| {
            if all_prefixed {
                l.strip_prefix(prefix).unwrap_or(l).to_string()
            } else {
                format!("{prefix}{l}")
            }
        })
        .collect();
    let rebuilt = rebuilt.join("\n");

    let delta = rebuilt.chars().count() as isize - block.chars().count() as isize;
    let text = format!(
        "{}{rebuilt}{}",
        slice(chars, 0, from),
        slice(chars, to, chars.len())
    );
    // Move the selection with the text so it still covers the same words.
    let shift = if all_prefixed {
        -(prefix.chars().count() as isize)
    } else {
        prefix.chars().count() as isize
    };
    Edit {
        text,
        start: (start as isize + shift).max(from as isize) as usize,
        end: (end as isize + delta).max(0) as usize,
    }
}

/// Index of the start of the line containing `i`.
fn line_start(chars: &[char], i: usize) -> usize {
    chars[..i]
        .iter()
        .rposition(|c| *c == '\n')
        .map(|p| p + 1)
        .unwrap_or(0)
}

/// Index of the end of the line containing `i`.
fn line_end(chars: &[char], i: usize) -> usize {
    chars[i..]
        .iter()
        .position(|c| *c == '\n')
        .map(|p| i + p)
        .unwrap_or(chars.len())
}

/// Should this keystroke send the message, rather than insert a newline?
///
/// Enter sends, Shift+Enter makes a new line. This is the convention every chat
/// client uses, and getting it backwards is instantly infuriating.
pub fn sends_on_enter(key: &str, shift: bool) -> bool {
    key == "Enter" && !shift
}

/// The `Format` bound to a keyboard shortcut, if any.
pub fn shortcut(key: &str) -> Option<Format> {
    let k = key.to_ascii_lowercase();
    TOOLBAR
        .into_iter()
        .find(|f| f.button().2 == k.chars().next())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Apply a format to `text` where the selection is marked by `|…|`.
    ///
    /// Positions are counted in **characters**, matching what the DOM hands us.
    /// `str::find` would return a byte offset and quietly mis-place the
    /// selection the moment the text contains anything non-ASCII.
    fn at(text: &str, fmt: Format) -> String {
        let pos = |s: &str| {
            s.chars()
                .position(|c| c == '|')
                .expect("mark the selection")
        };
        let start = pos(text);
        let rest = text.replacen('|', "", 1);
        let end = pos(&rest);
        let clean = rest.replacen('|', "", 1);
        let e = apply(&clean, start, end, fmt);
        // Render the result with the new selection marked, so tests read as
        // before/after rather than as index arithmetic.
        let mut out: Vec<char> = e.text.chars().collect();
        out.insert(e.end, '|');
        out.insert(e.start, '|');
        out.into_iter().collect()
    }

    #[test]
    fn wrapping_a_selection_keeps_it_selected() {
        // So you can hit Bold then Italic without reselecting.
        assert_eq!(
            at("say |hello| there", Format::Bold),
            "say **|hello|** there"
        );
        assert_eq!(
            at("say |hello| there", Format::Italic),
            "say *|hello|* there"
        );
        assert_eq!(at("say |hello| there", Format::Code), "say `|hello|` there");
        assert_eq!(
            at("say |hello| there", Format::Strike),
            "say ~~|hello|~~ there"
        );
    }

    #[test]
    fn an_empty_selection_parks_the_caret_between_the_markers() {
        // Pressing Bold with nothing selected should let you just start typing.
        assert_eq!(at("a||b", Format::Bold), "a**||**b");
        assert_eq!(at("||", Format::Italic), "*||*");
    }

    #[test]
    fn pressing_the_same_button_again_removes_the_formatting() {
        // Selection covering the markers…
        assert_eq!(
            at("say |**hello**| there", Format::Bold),
            "say |hello| there"
        );
        // …and selection covering only the text between them.
        assert_eq!(
            at("say **|hello|** there", Format::Bold),
            "say |hello| there"
        );
        assert_eq!(at("say `|code|` there", Format::Code), "say |code| there");
    }

    #[test]
    fn bold_does_not_mistake_italic_markers_for_its_own() {
        // `*x*` selected with Bold should wrap, not unwrap: the markers differ.
        assert_eq!(at("|*x*|", Format::Bold), "**|*x*|**");
    }

    #[test]
    fn a_link_puts_the_caret_in_the_url() {
        // The label is usually already typed; the URL never is.
        assert_eq!(at("see |docs| now", Format::Link), "see [docs](||) now");
        // With nothing selected, a placeholder label so the result is valid.
        assert_eq!(at("see || now", Format::Link), "see [text](||) now");
    }

    #[test]
    fn line_formats_prefix_every_line_they_touch() {
        assert_eq!(at("|one\ntwo|", Format::Quote), "> |one\n> two|");
        assert_eq!(at("|one\ntwo|", Format::Bullet), "- |one\n- two|");
        assert_eq!(at("|title|", Format::Heading), "# |title|");
    }

    #[test]
    fn a_line_format_applies_to_the_whole_line_not_just_the_selection() {
        // You don't select the line start before hitting Quote.
        assert_eq!(at("hello |there|", Format::Quote), "> hello |there|");
    }

    #[test]
    fn line_formats_toggle_off_when_every_line_already_has_them() {
        assert_eq!(at("> |one\n> two|", Format::Quote), "|one\ntwo|");
        assert_eq!(at("- |a|", Format::Bullet), "|a|");
        // Mixed: adding is the sensible move, not removing.
        let mixed = apply("> a\nb", 0, 5, Format::Quote);
        assert_eq!(mixed.text, "> > a\n> b");
    }

    #[test]
    fn selections_from_the_dom_can_never_panic() {
        // Reversed, past the end, both — all of these arrive from real browsers.
        assert_eq!(apply("abc", 3, 0, Format::Bold).text, "**abc**");
        assert_eq!(apply("abc", 99, 99, Format::Bold).text, "abc****");
        assert_eq!(apply("", 5, 2, Format::Italic).text, "**");
        assert_eq!(apply("abc", 0, 99, Format::Quote).text, "> abc");
        for f in TOOLBAR {
            let _ = apply("", 0, 0, f);
            let _ = apply("x", 9, 9, f);
        }
    }

    #[test]
    fn multibyte_text_is_indexed_by_character_not_byte() {
        // Byte offsets would slice through the é and panic. The DOM gives
        // character offsets, so this is the real case, not an exotic one.
        assert_eq!(at("caf|é| au lait", Format::Bold), "caf**|é|** au lait");
        assert_eq!(at("|日本語|", Format::Italic), "*|日本語|*");
        let e = apply("héllo wörld", 0, 5, Format::Bold);
        assert_eq!(e.text, "**héllo** wörld");
    }

    #[test]
    fn enter_sends_and_shift_enter_does_not() {
        assert!(sends_on_enter("Enter", false));
        assert!(
            !sends_on_enter("Enter", true),
            "Shift+Enter makes a newline"
        );
        assert!(!sends_on_enter("a", false));
    }

    #[test]
    fn shortcuts_map_to_the_buttons_that_advertise_them() {
        // The tooltip promises ⌘B; this is what makes it true.
        assert_eq!(shortcut("b"), Some(Format::Bold));
        assert_eq!(shortcut("B"), Some(Format::Bold));
        assert_eq!(shortcut("i"), Some(Format::Italic));
        assert_eq!(shortcut("k"), Some(Format::Link));
        assert_eq!(shortcut("z"), None, "unbound keys stay unbound");
    }

    #[test]
    fn every_toolbar_button_has_a_label_and_a_name() {
        let mut names = Vec::new();
        for f in TOOLBAR {
            let (label, name, _) = f.button();
            assert!(!label.is_empty() && !name.is_empty(), "{f:?}");
            assert!(!names.contains(&name), "{name} is used twice");
            names.push(name);
        }
        assert_eq!(TOOLBAR.len(), 8);
    }

    #[test]
    fn formatted_output_round_trips_through_the_renderer() {
        // The bar exists to produce markdown that renders. If a button emitted
        // syntax the renderer doesn't accept, both halves would still pass their
        // own tests and the feature would be broken.
        let e = apply("hello", 0, 5, Format::Bold);
        assert_eq!(
            crate::markdown::inline_to_html(&e.text),
            "<strong>hello</strong>"
        );
        let e = apply("hello", 0, 5, Format::Italic);
        assert_eq!(crate::markdown::inline_to_html(&e.text), "<em>hello</em>");
        let e = apply("hello", 0, 5, Format::Code);
        assert_eq!(
            crate::markdown::inline_to_html(&e.text),
            "<code>hello</code>"
        );
        let e = apply("hello", 0, 5, Format::Strike);
        assert_eq!(crate::markdown::inline_to_html(&e.text), "<del>hello</del>");
        let e = apply("hello", 0, 5, Format::Quote);
        assert_eq!(
            crate::markdown::to_html(&e.text),
            "<blockquote>hello</blockquote>"
        );
        let e = apply("hello", 0, 5, Format::Bullet);
        assert_eq!(crate::markdown::to_html(&e.text), "<ul><li>hello</li></ul>");
        let e = apply("hello", 0, 5, Format::Heading);
        assert_eq!(crate::markdown::to_html(&e.text), "<h1>hello</h1>");
        // The link button's output is a link once a URL is typed into its slot.
        let e = apply("docs", 0, 4, Format::Link);
        let filled = e.text.replace("()", "(https://x.test)");
        assert!(crate::markdown::inline_to_html(&filled).contains("href=\"https://x.test\""));
    }
}
