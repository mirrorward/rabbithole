//! **Warren marks** — a deterministic pixel icon for every person.
//!
//! Hotline's most-loved detail was the user icon: a little pixel creature beside
//! every name, so you knew who was talking before you read the handle. We keep
//! that soul without asking anyone to pick from a sprite sheet: a mark is derived
//! from the person's identity, so it is stable everywhere they appear and
//! identical for everyone looking at them.
//!
//! The seed is the **verified identity key** when the burrow reports one (so the
//! same human wears the same face across burrows, even under different handles),
//! else the handle. Pairs with the People view's key-based coalescing.
//!
//! Pure and host-tested: the grid and colour are a function of the seed alone.
//! Only the SVG string is consumed by the view layer.

/// A person's pixel mark: a 5×5 grid (mirrored left-to-right, so it reads as a
/// face/creature rather than noise) plus a palette colour.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mark {
    /// Row-major 5×5 cells; `true` = filled.
    pub cells: [[bool; 5]; 5],
    /// Index into [`PALETTE`].
    pub color: usize,
}

/// Curated mark colours — warm and cool tones chosen to sit on both the light
/// and dark grounds without vibrating, and to stay distinct from the accent
/// (which the UI reserves for *your* actions).
pub const PALETTE: [&str; 8] = [
    "#c2643c", // ember
    "#3f8f6f", // moss
    "#4a6fa5", // slate blue
    "#9a5b9c", // heather
    "#b08434", // brass
    "#4d8f97", // teal
    "#a85566", // rose
    "#6b7f3a", // olive
];

/// Build the mark for a seed (an identity key hex, or a handle).
///
/// The grid is symmetric: columns 0–2 come from the hash, columns 3–4 mirror
/// columns 1–0. Fill density is nudged toward the middle so marks never come out
/// blank or solid — both extremes are unrecognisable.
pub fn mark(seed: &str) -> Mark {
    let digest = blake3::hash(seed.trim().to_lowercase().as_bytes());
    let bytes = digest.as_bytes();

    let mut cells = [[false; 5]; 5];
    // 15 decisions (5 rows × 3 columns), one byte each — plenty of entropy.
    for (row, cells_row) in cells.iter_mut().enumerate() {
        for col in 0..3 {
            let b = bytes[row * 3 + col];
            // Bias the centre column slightly denser: it forms the spine, which
            // keeps a mark reading as a figure rather than two loose halves.
            let threshold = if col == 2 { 140 } else { 128 };
            let on = b < threshold;
            cells_row[col] = on;
            // Mirror: col 0 -> 4, col 1 -> 3 (col 2 is the axis).
            if col < 2 {
                cells_row[4 - col] = on;
            }
        }
    }

    // Guard the degenerate extremes: an all-empty or all-full mark identifies
    // no one. Flip the centre cell of the middle row to break the tie.
    let filled = cells.iter().flatten().filter(|c| **c).count();
    if filled == 0 || filled == 25 {
        cells[2][2] = filled == 0;
    }

    Mark {
        cells,
        color: (bytes[31] as usize) % PALETTE.len(),
    }
}

/// Render a mark as a self-contained inline SVG at `size` px — crisp at any
/// scale, no image requests, and it inherits nothing from the page so it looks
/// the same in both themes.
pub fn mark_svg(seed: &str, size: u32) -> String {
    let m = mark(seed);
    let colour = PALETTE[m.color];
    let mut rects = String::new();
    for (row, cells_row) in m.cells.iter().enumerate() {
        for (col, on) in cells_row.iter().enumerate() {
            if *on {
                rects.push_str(&format!(r#"<rect x="{col}" y="{row}" width="1" height="1"/>"#));
            }
        }
    }
    format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 5 5" width="{size}" height="{size}" role="img" aria-hidden="true" shape-rendering="crispEdges"><rect width="5" height="5" rx="1.1" fill="{colour}" opacity="0.16"/><g fill="{colour}">{rects}</g></svg>"#
    )
}

/// The seed for a person: their verified identity key when the burrow reports
/// one (stable across burrows and handle changes), else their handle.
pub fn seed_for(key: Option<&str>, handle: &str) -> String {
    match key {
        Some(k) if !k.is_empty() => k.to_string(),
        _ => handle.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn filled(m: &Mark) -> usize {
        m.cells.iter().flatten().filter(|c| **c).count()
    }

    #[test]
    fn marks_are_deterministic_and_case_insensitive() {
        // The same person always wears the same face, everywhere.
        assert_eq!(mark("alice"), mark("alice"));
        assert_eq!(mark("Alice"), mark("alice"), "handle case doesn't change it");
        assert_eq!(mark(" alice "), mark("alice"), "whitespace doesn't either");
        // Different people look different.
        assert_ne!(mark("alice"), mark("bob"));
    }

    #[test]
    fn marks_are_mirror_symmetric() {
        for who in ["alice", "bob", "carol", "the rabbit", ""] {
            let m = mark(who);
            for row in 0..5 {
                assert_eq!(m.cells[row][0], m.cells[row][4], "{who}: col 0/4 mirror");
                assert_eq!(m.cells[row][1], m.cells[row][3], "{who}: col 1/3 mirror");
            }
        }
    }

    #[test]
    fn marks_are_never_blank_or_solid() {
        // A blank or solid mark identifies nobody — check a broad sample.
        for i in 0..500 {
            let m = mark(&format!("user{i}"));
            let n = filled(&m);
            assert!(n > 0 && n < 25, "user{i} produced a degenerate mark ({n} cells)");
        }
    }

    #[test]
    fn marks_spread_across_the_palette() {
        // All eight colours should show up over a realistic population.
        let mut seen = [false; PALETTE.len()];
        for i in 0..400 {
            seen[mark(&format!("user{i}")).color] = true;
        }
        assert!(seen.iter().all(|s| *s), "every palette colour is reachable");
    }

    #[test]
    fn the_identity_key_wins_over_the_handle() {
        // The same human under two handles keeps one face when their key is known…
        let k = Some("deadbeef");
        assert_eq!(
            mark(&seed_for(k, "rabbit")),
            mark(&seed_for(k, "mr_rabbit"))
        );
        // …and two strangers sharing a handle look different when keyed.
        assert_ne!(
            mark(&seed_for(Some("aaaa"), "rabbit")),
            mark(&seed_for(Some("bbbb"), "rabbit"))
        );
        // Unkeyed people fall back to the handle.
        assert_eq!(mark(&seed_for(None, "rabbit")), mark("rabbit"));
        assert_eq!(mark(&seed_for(Some(""), "rabbit")), mark("rabbit"));
    }

    #[test]
    fn svg_is_self_contained_and_sized() {
        let svg = mark_svg("alice", 24);
        assert!(svg.starts_with("<svg"));
        assert!(svg.contains(r#"width="24""#) && svg.contains(r#"height="24""#));
        assert!(svg.contains(r#"viewBox="0 0 5 5""#));
        // No external references — nothing to fetch, nothing to leak.
        assert!(!svg.contains("http://") || svg.contains("www.w3.org/2000/svg"));
        assert!(!svg.contains("<image"));
        // Decorative: the name beside it carries the meaning for screen readers.
        assert!(svg.contains(r#"aria-hidden="true""#));
    }
}
