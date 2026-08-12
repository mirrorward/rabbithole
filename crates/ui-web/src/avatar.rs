//! **Warren marks** — the little picture beside your name.
//!
//! Hotline gave every user an icon from a library of hand-drawn pictures: a
//! fish, a skull, a cat. Half the personality of a server was reading the
//! roster. That's the thing worth keeping, and it's the thing a hash-shaped
//! blob can't do — procedural identicons are *distinct*, but nobody has ever
//! said "the one with the orange squiggle" and been understood.
//!
//! So this is a drawn set, not a generated one: sixteen 8×8 sprites, each a
//! silhouette that survives being 20px tall in a chat line. Which one you get
//! is derived from your identity, so it's stable across sessions and devices
//! without anything being stored, and it's *your* mark on every burrow you
//! join. Two people can share a sprite; the colour and the name beside it are
//! what separate them.
//!
//! The whole module is pure and host-tested — the sprites, the assignment, and
//! the SVG — because an avatar that changes between renders would quietly break
//! the one property it exists to provide.

/// Colours a mark can be drawn in. Mid-tone on purpose: each has to stay legible
/// on both a near-white and a near-black background, since a burrow's theme pack
/// can put either behind it.
pub const PALETTE: [&str; 8] = [
    "#c2643c", // clay
    "#3f8f6f", // moss
    "#4a6fa5", // slate blue
    "#9a5b9c", // plum
    "#b08434", // brass
    "#4d8f97", // teal
    "#a85566", // rose
    "#6b7f3a", // olive
];

/// The sprite library. `#` is the mark's colour, `o` a dark detail (eyes,
/// mouths), `*` a light one (a highlight, a glint), `.` is empty.
///
/// Drawn small deliberately: at 8×8 you can only state the silhouette, which is
/// exactly what reads at 20px in a chat line. Detail added here disappears at
/// the size it's actually used.
const GLYPHS: [(&str, [&str; 8]); 16] = [
    (
        "rabbit",
        [
            ".#....#.", ".##..##.", ".##..##.", "..####..", ".#o##o#.", ".######.", ".#.##.#.",
            "..####..",
        ],
    ),
    (
        "cat",
        [
            "#......#", "##....##", "########", "#o####o#", "##.##.##", "#..oo..#", "#.####.#",
            ".######.",
        ],
    ),
    (
        "fox",
        [
            "#......#", "##....##", "########", "#o####o#", "########", ".#o##o#.", "..#oo#..",
            "...##...",
        ],
    ),
    (
        "owl",
        [
            ".#....#.", ".######.", "#**##**#", "#*o##o*#", "##o##o##", "#.####.#", ".######.", "..#..#..",
        ],
    ),
    (
        "frog",
        [
            ".##..##.", "#oo##oo#", "########", "########", "#.####.#", ".######.", "##....##",
            "#......#",
        ],
    ),
    (
        "fish",
        [
            "........", "..###...", ".#####.#", "#o######", "#o######", ".#####.#", "..###...",
            "........",
        ],
    ),
    (
        "bee",
        [
            "..o..o..", "*.####.*", "**####**", "..oooo..", "..####..", "..oooo..", "..####..", "...##...",
        ],
    ),
    (
        "bird",
        [
            "........", "##....##", ".##..##.", "..####..", "...##...", "........", "........", "........",
        ],
    ),
    (
        "mushroom",
        [
            "..####..", ".#*##*#.", "########", "#*####*#", ".######.", "...##...", "...##...", "..####..",
        ],
    ),
    (
        "heart",
        [
            ".##..##.", "########", "########", "########", ".######.", "..####..", "...##...", "........",
        ],
    ),
    (
        "moon",
        [
            "..####..", ".#####..", "####....", "####....", "####....", "####....", ".#####..",
            "..####..",
        ],
    ),
    (
        "star",
        [
            "...##...", "...##...", "..####..", "########", "########", "..####..", "...##...", "...##...",
        ],
    ),
    (
        "key",
        [
            ".####...", "#o..o#..", "#....#..", ".####...", "..##....", "..###...", "..##....",
            "..###...",
        ],
    ),
    (
        "rocket",
        [
            "...##...", "..####..", "..#oo#..", "..####..", ".######.", "#.####.#", "#..##..#",
            "..#..#..",
        ],
    ),
    (
        "crown",
        [
            "........", "#..##..#", "##.##.##", "########", "########", "#*#**#*#", "########", "........",
        ],
    ),
    (
        "ghost",
        [
            "..####..", ".######.", "#o####o#", "#o####o#", "########", "########", "########",
            "#.#.#.#.",
        ],
    ),
];

/// Which sprite, in which colour, someone gets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mark {
    /// Index into [`GLYPHS`].
    pub glyph: usize,
    /// Index into [`PALETTE`].
    pub color: usize,
}

impl Mark {
    /// The sprite's name — "rabbit", "owl". Useful for a label, and for the
    /// picker, where people choose by name.
    pub fn name(&self) -> &'static str {
        GLYPHS[self.glyph].0
    }
}

/// How many sprites there are. Public so a picker can enumerate them without
/// reaching into the table.
pub const GLYPH_COUNT: usize = GLYPHS.len();

/// The name of sprite `i`, wrapping if out of range.
pub fn glyph_name(i: usize) -> &'static str {
    GLYPHS[i % GLYPH_COUNT].0
}

/// The mark for a seed. Deterministic: the same identity always draws the same
/// sprite in the same colour, on every device, with nothing persisted.
pub fn mark(seed: &str) -> Mark {
    let h = blake3::hash(seed.trim().to_lowercase().as_bytes());
    let b = h.as_bytes();
    // Separate bytes for sprite and colour so two people sharing a sprite are
    // unlikely to also share its colour.
    Mark {
        glyph: b[0] as usize % GLYPH_COUNT,
        color: b[1] as usize % PALETTE.len(),
    }
}

/// Render a mark as a self-contained inline SVG, `size` pixels square.
///
/// Every pixel is its own `<rect>` — 64 at most, cheaper than the string
/// concatenation around it — and `shape-rendering=crispEdges` keeps the grid
/// from being antialiased into mush at small sizes. `aria-hidden` because the
/// name always sits beside it; an avatar announced to a screen reader is noise.
pub fn mark_svg(seed: &str, size: u32) -> String {
    let m = mark(seed);
    glyph_svg(m.glyph, m.color, size)
}

/// Render sprite `i` in colour `c`. Both indices wrap, so a stored choice from
/// a future build with more sprites degrades to a real mark instead of a panic.
pub fn glyph_svg(i: usize, c: usize, size: u32) -> String {
    let color = PALETTE[c % PALETTE.len()];
    let (dark, light) = (shade(color, 0.45), tint(color, 0.62));
    let rows = &GLYPHS[i % GLYPH_COUNT].1;
    let mut out = format!(
        "<svg viewBox=\"0 0 8 8\" width=\"{size}\" height=\"{size}\" \
         shape-rendering=\"crispEdges\" aria-hidden=\"true\" focusable=\"false\">\
         <rect width=\"8\" height=\"8\" rx=\"1.6\" fill=\"{color}\" fill-opacity=\".16\"/>"
    );
    for (y, row) in rows.iter().enumerate() {
        for (x, cell) in row.chars().enumerate() {
            let fill = match cell {
                '#' => color,
                'o' => dark.as_str(),
                '*' => light.as_str(),
                _ => continue,
            };
            out.push_str(&format!(
                "<rect x=\"{x}\" y=\"{y}\" width=\"1\" height=\"1\" fill=\"{fill}\"/>"
            ));
        }
    }
    out.push_str("</svg>");
    out
}

/// Darken a `#rrggbb` toward black. Used for eyes and mouths, so they read as
/// detail *within* the mark rather than as a second colour.
fn shade(hex: &str, factor: f32) -> String {
    let (r, g, b) = rgb(hex);
    format!(
        "#{:02x}{:02x}{:02x}",
        (r as f32 * factor) as u8,
        (g as f32 * factor) as u8,
        (b as f32 * factor) as u8
    )
}

/// Lighten a `#rrggbb` toward white, for highlights.
fn tint(hex: &str, factor: f32) -> String {
    let (r, g, b) = rgb(hex);
    let up = |c: u8| (c as f32 + (255.0 - c as f32) * factor) as u8;
    format!("#{:02x}{:02x}{:02x}", up(r), up(g), up(b))
}

/// Split `#rrggbb` into components. Only ever called on [`PALETTE`] entries,
/// which a test pins to that exact shape.
fn rgb(hex: &str) -> (u8, u8, u8) {
    let h = hex.trim_start_matches('#');
    let p = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).unwrap_or(0);
    (p(0), p(2), p(4))
}

/// Your own chosen mark, when you've picked one instead of taking the mark
/// your key derives. Stored locally.
///
/// Honest limit: this is a **local** preference. The wire carries no mark
/// field, so other people still see the mark your identity derives — the
/// picker says so rather than implying a change nobody else can see.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChosenMark {
    pub glyph: usize,
    pub color: usize,
}

#[cfg(target_arch = "wasm32")]
pub mod chosen {
    //! `localStorage` persistence for a picked mark.
    use super::ChosenMark;

    const KEY: &str = "rh.mark.v1";

    fn store() -> Option<web_sys::Storage> {
        web_sys::window()?.local_storage().ok()?
    }

    pub fn load() -> Option<ChosenMark> {
        let raw = store()?.get_item(KEY).ok()??;
        let (g, c) = raw.split_once(':')?;
        Some(ChosenMark {
            glyph: g.parse().ok()?,
            color: c.parse().ok()?,
        })
    }

    pub fn save(m: Option<ChosenMark>) {
        let Some(s) = store() else { return };
        match m {
            Some(m) => {
                let _ = s.set_item(KEY, &format!("{}:{}", m.glyph, m.color));
            }
            None => {
                let _ = s.remove_item(KEY);
            }
        }
    }
}

/// The seed to draw someone's mark from: their verified identity key when they
/// have one, else their handle.
///
/// Keying on the identity means your mark follows you across burrows and
/// survives a rename — and that someone taking your handle on another server
/// doesn't inherit your face.
pub fn seed_for(key: Option<&str>, handle: &str) -> String {
    match key {
        Some(k) if !k.trim().is_empty() => k.trim().to_lowercase(),
        _ => handle.trim().to_lowercase(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_sprite_is_a_well_formed_eight_by_eight() {
        for (name, rows) in GLYPHS {
            assert_eq!(rows.len(), 8, "{name} is not 8 rows");
            for (y, row) in rows.iter().enumerate() {
                assert_eq!(row.chars().count(), 8, "{name} row {y} is not 8 cells: {row:?}");
                for c in row.chars() {
                    assert!(
                        matches!(c, '.' | '#' | 'o' | '*'),
                        "{name} row {y} has an unknown cell {c:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn no_sprite_is_blank_solid_or_a_duplicate() {
        // A blank sprite is an invisible user; a solid one is a colour swatch;
        // a duplicate claims two people differ when they don't.
        let mut seen: Vec<&[&str; 8]> = Vec::new();
        for (name, rows) in &GLYPHS {
            let filled = rows.iter().flat_map(|r| r.chars()).filter(|c| *c != '.').count();
            assert!(filled > 8, "{name} is nearly blank ({filled} cells)");
            assert!(filled < 60, "{name} is nearly solid ({filled} cells)");
            assert!(!seen.contains(&rows), "{name} duplicates another sprite");
            seen.push(rows);
        }
        assert_eq!(GLYPH_COUNT, 16);
    }

    #[test]
    fn every_sprite_has_a_distinct_name() {
        let mut names: Vec<&str> = GLYPHS.iter().map(|(n, _)| *n).collect();
        names.sort_unstable();
        let before = names.len();
        names.dedup();
        assert_eq!(names.len(), before, "two sprites share a name");
    }

    #[test]
    fn a_mark_is_stable_and_case_insensitive() {
        // The whole point of deriving it: no storage, same face everywhere.
        assert_eq!(mark("alice"), mark("alice"));
        assert_eq!(mark("Alice"), mark("alice"));
        assert_eq!(mark(" alice "), mark("alice"));
        assert_eq!(mark_svg("alice", 24), mark_svg("alice", 24));
    }

    #[test]
    fn marks_spread_across_the_whole_library() {
        // A hash that favoured a few sprites would make the roster look like
        // everyone picked the same one.
        let mut glyphs = [0usize; GLYPH_COUNT];
        let mut colors = [0usize; PALETTE.len()];
        for i in 0..600 {
            let m = mark(&format!("user{i}"));
            glyphs[m.glyph] += 1;
            colors[m.color] += 1;
        }
        assert!(glyphs.iter().all(|n| *n > 0), "some sprite is never chosen: {glyphs:?}");
        assert!(colors.iter().all(|n| *n > 0), "some colour is never chosen: {colors:?}");
    }

    #[test]
    fn the_identity_key_wins_over_the_handle() {
        // Your mark follows your key, so a rename keeps your face and someone
        // else taking your handle elsewhere doesn't get it.
        assert_eq!(seed_for(Some("ABCD"), "alice"), "abcd");
        assert_eq!(seed_for(None, "Alice"), "alice");
        assert_eq!(seed_for(Some("  "), "alice"), "alice", "a blank key is no key");
    }

    #[test]
    fn the_svg_is_self_contained_and_silent_to_screen_readers() {
        let svg = mark_svg("alice", 28);
        assert!(svg.starts_with("<svg") && svg.ends_with("</svg>"));
        assert!(!svg.contains("http") && !svg.contains("<image"), "reaches off-page");
        assert!(svg.contains("aria-hidden=\"true\""), "the name beside it is the label");
        assert!(svg.contains("width=\"28\" height=\"28\""));
        assert!(svg.contains("crispEdges"), "8x8 art must not be blurred");
    }

    #[test]
    fn the_picker_renders_the_same_art_as_the_seeded_path() {
        // Two renderers drawing the library differently would make the picker
        // show you something other than what everyone else sees.
        let m = mark("alice");
        assert_eq!(glyph_svg(m.glyph, m.color, 28), mark_svg("alice", 28));
        // And both wrap rather than panic on an out-of-range choice.
        assert_eq!(glyph_svg(GLYPH_COUNT, 0, 16), glyph_svg(0, 0, 16));
        assert_eq!(glyph_name(GLYPH_COUNT + 1), glyph_name(1));
    }

    #[test]
    fn palette_entries_are_parseable_hex_and_shades_stay_in_range() {
        for c in PALETTE {
            assert!(c.len() == 7 && c.starts_with('#'), "{c} is not #rrggbb");
            let (r, g, b) = rgb(c);
            assert!(r as u16 + g as u16 + b as u16 > 0, "{c} parsed as black");
            // Detail colours must actually differ from the body, or eyes vanish.
            assert_ne!(shade(c, 0.45), c.to_string());
            assert_ne!(tint(c, 0.62), c.to_string());
            assert!(shade(c, 0.45).len() == 7 && tint(c, 0.62).len() == 7);
        }
    }
}
