//! Wildmat newsgroup-pattern matching (RFC 3977 §4).
//!
//! A *wildmat* is the comma-separated pattern language used by `NEWNEWS`,
//! `NEWGROUPS`, and the `LIST ACTIVE`/`LIST NEWSGROUPS` wildmat arguments to
//! select newsgroups. This module implements the deliberately restricted RFC
//! 3977 dialect:
//!
//! * A pattern is a comma-separated list of *items* matched left to right; the
//!   **last** item that matches a name decides the outcome (this is how a broad
//!   pattern can be narrowed with a following exception).
//! * An item prefixed with `!` is *negated*: when it matches, the name is
//!   excluded rather than included.
//! * Within an item the only metacharacters are `*` (matches any run of
//!   characters, including none) and `?` (matches exactly one character). Per
//!   RFC 3977 §4.2 there are **no** `[...]` character classes, ranges, or `\`
//!   escapes — every other character is literal and matching is
//!   case-sensitive, anchored to the whole name.
//!
//! Matching a name against no matching item yields `false`. The matcher is
//! iterative and allocation-light, so it never panics or recurses without bound
//! on adversarial input such as `********************`.

/// Test whether `name` matches the wildmat `pattern` (RFC 3977 §4).
///
/// The comma-separated items of `pattern` are evaluated left to right; the last
/// one whose glob matches `name` decides the result, with a leading `!` on that
/// item inverting the sense. If no item matches, the result is `false`.
///
/// Matching is case-sensitive and anchored (the whole name must be consumed).
/// Never panics, including on empty or pathological input.
///
/// # Examples
///
/// ```
/// use rabbithole_legacy_nntp::wildmat::matches;
///
/// assert!(matches("rabbit.*", "rabbit.general"));
/// assert!(!matches("rabbit.*", "warren.general"));
/// // Last match wins: include the tree but drop one branch.
/// assert!(!matches("rabbit.*,!rabbit.binaries", "rabbit.binaries"));
/// assert!(matches("rabbit.*,!rabbit.binaries", "rabbit.general"));
/// ```
#[must_use]
pub fn matches(pattern: &str, name: &str) -> bool {
    let name_chars: Vec<char> = name.chars().collect();
    let mut result = false;
    for item in pattern.split(',') {
        let (negated, body) = match item.strip_prefix('!') {
            Some(rest) => (true, rest),
            None => (false, item),
        };
        let pat_chars: Vec<char> = body.chars().collect();
        if glob_match(&pat_chars, &name_chars) {
            result = !negated;
        }
    }
    result
}

/// Match a single glob item (`*`/`?` only) against `text`, anchored.
///
/// Uses the classic linear two-pointer algorithm with a single backtracking
/// point for `*`, so it runs without recursion and stays well-behaved on inputs
/// with many `*` metacharacters.
fn glob_match(pat: &[char], text: &[char]) -> bool {
    let (mut p, mut t) = (0usize, 0usize);
    // Remembered position to resume from after a `*` when a later mismatch
    // forces the `*` to consume one more character of `text`.
    let mut star_pat: Option<usize> = None;
    let mut star_text = 0usize;

    while t < text.len() {
        if p < pat.len() && (pat[p] == '?' || pat[p] == text[t]) {
            p += 1;
            t += 1;
        } else if p < pat.len() && pat[p] == '*' {
            // Tentatively let `*` match nothing; remember where to backtrack.
            star_pat = Some(p);
            star_text = t;
            p += 1;
        } else if let Some(sp) = star_pat {
            // Mismatch after a `*`: let the `*` swallow one more character.
            p = sp + 1;
            star_text += 1;
            t = star_text;
        } else {
            return false;
        }
    }

    // Any trailing pattern must be all `*` to match the empty remainder.
    while p < pat.len() && pat[p] == '*' {
        p += 1;
    }
    p == pat.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literal_match_and_mismatch() {
        assert!(matches("rabbit.general", "rabbit.general"));
        assert!(!matches("rabbit.general", "rabbit.binaries"));
        assert!(!matches("rabbit.general", "rabbit.general.sub"));
    }

    #[test]
    fn star_matches_any_run_including_empty() {
        assert!(matches("*", "anything.at.all"));
        assert!(matches("*", ""));
        assert!(matches("rabbit.*", "rabbit."));
        assert!(matches("rabbit.*", "rabbit.general"));
        assert!(matches("*.general", "rabbit.general"));
        assert!(matches("rabbit.*.d", "rabbit.a.b.c.d"));
        assert!(!matches("rabbit.*.d", "rabbit.a.b.c.e"));
    }

    #[test]
    fn question_matches_exactly_one() {
        assert!(matches("rabbit.?", "rabbit.a"));
        assert!(!matches("rabbit.?", "rabbit.ab"));
        assert!(!matches("rabbit.?", "rabbit."));
    }

    #[test]
    fn is_case_sensitive() {
        assert!(!matches("Rabbit.*", "rabbit.general"));
        assert!(matches("Rabbit.*", "Rabbit.general"));
    }

    #[test]
    fn comma_list_last_match_wins() {
        // Include the whole tree, then exclude one branch.
        let pat = "rabbit.*,!rabbit.binaries.*";
        assert!(matches(pat, "rabbit.general"));
        assert!(!matches(pat, "rabbit.binaries.pics"));
        // Re-include after the exclusion.
        let pat2 = "rabbit.*,!rabbit.binaries.*,rabbit.binaries.text";
        assert!(matches(pat2, "rabbit.binaries.text"));
        assert!(!matches(pat2, "rabbit.binaries.pics"));
    }

    #[test]
    fn negation_only_excludes_when_it_matches() {
        // A lone negation never turns into a positive match.
        assert!(!matches("!rabbit.*", "rabbit.general"));
        assert!(!matches("!rabbit.*", "warren.general"));
    }

    #[test]
    fn no_matching_item_is_false() {
        assert!(!matches("warren.*,misc.*", "rabbit.general"));
    }

    #[test]
    fn metacharacters_are_only_star_and_question() {
        // `[` and `]` are literal in the RFC 3977 dialect.
        assert!(matches("group[1]", "group[1]"));
        assert!(!matches("group[1]", "group1"));
        // Backslash is literal, not an escape.
        assert!(matches("a\\b", "a\\b"));
    }

    #[test]
    fn empty_pattern_and_empty_name() {
        assert!(matches("", ""));
        assert!(!matches("", "x"));
        assert!(matches("*", ""));
    }

    #[test]
    fn never_panics_on_pathological_input() {
        for pat in [
            "",
            "*",
            "********************",
            "*a*a*a*a*a*a*b",
            "!,!,!",
            "?",
            ",,,,",
            "\0*\0",
        ] {
            for name in ["", "a", "aaaaaaaaaaaaaaaaaaaa", "rabbit.general", "\0\0"] {
                let _ = matches(pat, name);
            }
        }
    }
}
