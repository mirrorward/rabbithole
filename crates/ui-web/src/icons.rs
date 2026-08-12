//! Line icons for the section nav.
//!
//! Inline SVG, drawn on a 24×24 grid in `currentColor` so a single set works in
//! every theme pack and both light and dark modes — no icon font, no sprite
//! sheet, no second network request that can arrive after the paint.
//!
//! They are deliberately plain: a nav icon's whole job is to be recognised at a
//! glance at 18px and then get out of the way. The personality in this app
//! belongs to the warren marks and the burrow itself, not to the furniture.
//!
//! The strings are pure and host-tested, so the guarantees below are checked
//! rather than hoped for: every icon is self-contained, sized by its viewBox,
//! inherits colour, and is hidden from assistive tech (the link's own text is
//! the accessible name — an icon repeating it is just noise in a screen reader).

/// Shared attributes: no fill, stroked in the inherited colour, round joins.
/// `aria-hidden` because every icon here sits beside a visible text label.
const OPEN: &str = concat!(
    "<svg viewBox=\"0 0 24 24\" width=\"18\" height=\"18\" fill=\"none\" ",
    "stroke=\"currentColor\" stroke-width=\"1.7\" stroke-linecap=\"round\" ",
    "stroke-linejoin=\"round\" aria-hidden=\"true\" focusable=\"false\">"
);

/// The icon for a section, by route path. Unknown paths get a neutral dot
/// rather than nothing, so a new route is never an invisible nav row.
pub fn section_icon(path: &str) -> String {
    let body = match path {
        // A speech bubble with a tail — the room where people talk.
        "/lobby" => "<path d=\"M4.5 5.5h15v10h-9l-4.2 3.4a.4.4 0 0 1-.65-.31V5.5z\"/>",
        // Stacked cards: threads piled on a board.
        "/boards" => concat!(
            "<rect x=\"3.5\" y=\"4.5\" width=\"17\" height=\"15\" rx=\"2\"/>",
            "<path d=\"M3.5 9.5h17M8 13h9M8 16h6\"/>"
        ),
        // An envelope — addressed to you, unlike the lobby.
        "/dms" => concat!(
            "<rect x=\"3\" y=\"5.5\" width=\"18\" height=\"13\" rx=\"2\"/>",
            "<path d=\"M3.6 7l7.4 5.2a2 2 0 0 0 2 0L20.4 7\"/>"
        ),
        // Two figures: the roster of this burrow.
        "/directory" => concat!(
            "<circle cx=\"9.5\" cy=\"8.5\" r=\"3\"/>",
            "<path d=\"M3.5 19.5a6 6 0 0 1 12 0\"/>",
            "<path d=\"M16 6.2a3 3 0 0 1 0 5.6M17.5 14.4a5.5 5.5 0 0 1 3 5.1\"/>"
        ),
        // A folder with a tab.
        "/files" => "<path d=\"M3.5 18.5v-12h5.2l2 2.2h9.8v9.8a1 1 0 0 1-1 1h-15a1 1 0 0 1-1-1z\"/>",
        // A transmitter: mast plus two broadcast arcs.
        "/radio" => concat!(
            "<circle cx=\"12\" cy=\"12\" r=\"2.2\"/>",
            "<path d=\"M8.2 8.2a5.4 5.4 0 0 0 0 7.6M15.8 15.8a5.4 5.4 0 0 0 0-7.6\"/>",
            "<path d=\"M5.4 5.4a9.4 9.4 0 0 0 0 13.2M18.6 18.6a9.4 9.4 0 0 0 0-13.2\"/>"
        ),
        // Stacked machines — other burrows to go and find.
        "/servers" => concat!(
            "<rect x=\"3.5\" y=\"4.5\" width=\"17\" height=\"6\" rx=\"1.6\"/>",
            "<rect x=\"3.5\" y=\"13.5\" width=\"17\" height=\"6\" rx=\"1.6\"/>",
            "<path d=\"M7 7.5h.01M7 16.5h.01\"/>"
        ),
        // A framed picture with a sun and a hill.
        "/art" => concat!(
            "<rect x=\"3.5\" y=\"4.5\" width=\"17\" height=\"15\" rx=\"2\"/>",
            "<circle cx=\"8.6\" cy=\"9.4\" r=\"1.5\"/>",
            "<path d=\"M4 17l4.6-4.4a1.6 1.6 0 0 1 2.2 0L20 19.4\"/>"
        ),
        // Sliders: the operator's console.
        "/admin" => concat!(
            "<path d=\"M5 7.5h9M17.5 7.5h1.5M5 16.5h1.5M10 16.5h9\"/>",
            "<circle cx=\"15.6\" cy=\"7.5\" r=\"2.1\"/>",
            "<circle cx=\"8.4\" cy=\"16.5\" r=\"2.1\"/>"
        ),
        // Everyone, everywhere you're connected.
        // Everyone, everywhere you're connected: plainly more than one person.
        "/people" => concat!(
            "<circle cx=\"9\" cy=\"9.4\" r=\"3.1\"/>",
            "<path d=\"M3.4 19a5.9 5.9 0 0 1 11.2 0\"/>",
            "<circle cx=\"17\" cy=\"8.2\" r=\"2.4\"/>",
            "<path d=\"M15.8 13.2a4.9 4.9 0 0 1 4.8 4.6\"/>"
        ),
        // Bytes coming down to you.
        "/transfers" => "<path d=\"M12 4v11m0 0l-4-4m4 4l4-4M4.5 19.5h15\"/>",
        // Your own key and face.
        // *You*, not "a person": a bust inside a ring — the badge/account
        // idiom, unmistakable beside /people's group of figures.
        "/you" => concat!(
            "<circle cx=\"12\" cy=\"12\" r=\"9\"/>",
            "<circle cx=\"12\" cy=\"10\" r=\"3.1\"/>",
            "<path d=\"M6.3 18.7a6.1 6.1 0 0 1 11.4 0\"/>"
        ),
        _ => "<circle cx=\"12\" cy=\"12\" r=\"3.2\"/>",
    };
    format!("{OPEN}{body}</svg>")
}

/// The theme-pack control: overlapping swatches, the universal "change how this
/// looks" glyph. Labelled by the button's `aria-label`, like the section icons.
pub fn pack_icon() -> String {
    format!(
        "{OPEN}{}</svg>",
        // Overlapping swatches. Two circles read as a chain link at this size,
        // which is the icon for something else entirely.
        concat!(
            "<rect x=\"3.4\" y=\"3.4\" width=\"11.2\" height=\"11.2\" rx=\"2.6\"/>",
            "<rect x=\"9.4\" y=\"9.4\" width=\"11.2\" height=\"11.2\" rx=\"2.6\"/>"
        )
    )
}

/// The burrow rail's own icon set.
///
/// The rail is the app's main nav, and it grew by accretion: a CSS-gradient
/// bullseye for Home, three section icons drawn for the sidebar's 18px, a
/// text "+" — four visual languages in one column, at four optical sizes,
/// with People and You both reading as "a person". One family now, drawn for
/// the rail's 20px box at matched optical weight:
///
/// * **home** — the rabbit hole itself, concentric rings: the brand mark, and
///   "into the burrow".
/// * **people** — a smiling face. Warm, instantly "the people I know", and
///   unmistakable next to **you**, the classic single bust.
/// * **transfers** — an arrow landing in a tray: bytes arriving somewhere,
///   not just pointing down.
/// * **add** — a plus.
///
/// Unknown names get the neutral dot, same as [`section_icon`].
pub fn rail_icon(which: &str) -> String {
    let body = match which {
        "home" => concat!(
            "<circle cx=\"12\" cy=\"12\" r=\"8.2\"/>",
            "<circle cx=\"12\" cy=\"12\" r=\"4.4\"/>",
            "<circle cx=\"12\" cy=\"12\" r=\"1\" fill=\"currentColor\" stroke=\"none\"/>"
        ),
        "people" => concat!(
            "<circle cx=\"12\" cy=\"12\" r=\"7.8\"/>",
            "<path d=\"M9.1 9.7v1M14.9 9.7v1\"/>",
            "<path d=\"M8.5 14.1a4.6 4.6 0 0 0 7 0\"/>"
        ),
        "transfers" => concat!(
            "<path d=\"M12 3.8v9.6m0 0l-4-4m4 4l4-4\"/>",
            "<path d=\"M4.5 15.7v2.6a1.5 1.5 0 0 0 1.5 1.5h12a1.5 1.5 0 0 0 1.5-1.5v-2.6\"/>"
        ),
        "you" => concat!(
            "<circle cx=\"12\" cy=\"12\" r=\"9\"/>",
            "<circle cx=\"12\" cy=\"10\" r=\"3.1\"/>",
            "<path d=\"M6.3 18.7a6.1 6.1 0 0 1 11.4 0\"/>"
        ),
        "add" => "<path d=\"M12 5.6v12.8M5.6 12h12.8\"/>",
        _ => "<circle cx=\"12\" cy=\"12\" r=\"3.2\"/>",
    };
    format!("{OPEN}{body}</svg>")
}

/// Settings: sliders — *your* app's console, distinct from Admin's operator
/// console for a burrow.
pub fn settings_icon() -> String {
    format!(
        "{OPEN}{}</svg>",
        concat!(
            "<path d=\"M4.5 7.5h9M17.5 7.5h2M4.5 16.5h2M10.5 16.5h9\"/>",
            "<circle cx=\"15.4\" cy=\"7.5\" r=\"2.1\"/>",
            "<circle cx=\"8.6\" cy=\"16.5\" r=\"2.1\"/>"
        )
    )
}

/// The chime toggle: a bell, slashed when muted. Drawn like every other icon
/// here — the colour emoji it replaces ignored the theme, sat on its own
/// baseline, and no native app puts emoji in its window chrome.
pub fn bell_icon(on: bool) -> String {
    let bell = "<path d=\"M12 4a5.2 5.2 0 0 1 5.2 5.2c0 3.2.9 4.7 1.8 5.8H5c.9-1.1 1.8-2.6 1.8-5.8A5.2 5.2 0 0 1 12 4z\"/>\
                <path d=\"M10 18.6a2.1 2.1 0 0 0 4 0\"/>";
    let body = if on {
        bell.to_string()
    } else {
        format!("{bell}<path d=\"M4.5 3.5l15 17\"/>")
    };
    format!("{OPEN}{body}</svg>")
}

/// A file-table row's icon: a tabbed folder or a dog-eared document. The
/// 📁/📄 emoji these replace are the fastest way to make a file browser read
/// as a hobby web page next to Finder.
pub fn file_icon(is_folder: bool) -> String {
    let body = if is_folder {
        "<path d=\"M3.5 18.5v-12h5.2l2 2.2h9.8v9.8a1 1 0 0 1-1 1h-15a1 1 0 0 1-1-1z\"/>"
    } else {
        "<path d=\"M6 3.5h8l4 4v13h-12z\"/><path d=\"M14 3.5v4h4\"/>"
    };
    format!("{OPEN}{body}</svg>")
}

/// The appearance control, drawn as what it currently is: a sun for light, a
/// moon for dark, and a half-filled disc for "follow the system" — the same
/// three shapes every OS uses, so the button says what it does without a word
/// of text beside it.
pub fn mode_icon(choice: crate::theme_css::ModeChoice) -> String {
    use crate::theme_css::ModeChoice;
    let body = match choice {
        ModeChoice::Light => concat!(
            "<circle cx=\"12\" cy=\"12\" r=\"4\"/>",
            "<path d=\"M12 2.6v2.2M12 19.2v2.2M2.6 12h2.2M19.2 12h2.2\"/>",
            "<path d=\"M5.4 5.4l1.6 1.6M17 17l1.6 1.6M18.6 5.4L17 7M7 17l-1.6 1.6\"/>"
        ),
        ModeChoice::Dark => {
            "<path d=\"M20 14.2A8.4 8.4 0 0 1 9.8 4a8.4 8.4 0 1 0 10.2 10.2z\"/>"
        }
        // Half light, half dark: the disc is outlined, and one side filled.
        ModeChoice::System => concat!(
            "<circle cx=\"12\" cy=\"12\" r=\"8.4\"/>",
            "<path d=\"M12 3.6a8.4 8.4 0 0 0 0 16.8z\" fill=\"currentColor\" stroke=\"none\"/>"
        ),
    };
    format!("{OPEN}{body}</svg>")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every section the nav can link to.
    const PATHS: [&str; 13] = [
        "/lobby",
        "/boards",
        "/dms",
        "/directory",
        "/files",
        "/radio",
        "/servers",
        "/art",
        "/admin",
        "/people",
        "/transfers",
        "/you",
        "/settings",
    ];

    #[test]
    fn every_section_has_a_self_contained_icon() {
        for p in PATHS {
            let svg = section_icon(p);
            assert!(svg.starts_with("<svg"), "{p}");
            assert!(svg.ends_with("</svg>"), "{p}");
            // No external anything: an icon that needs a fetch can paint late.
            assert!(!svg.contains("http"), "{p} reaches off-page");
            assert!(!svg.contains("<image"), "{p} embeds a raster");
            // Sized by the viewBox so it scales with the nav, not a fixed sprite.
            assert!(svg.contains("viewBox=\"0 0 24 24\""), "{p}");
        }
    }

    #[test]
    fn icons_inherit_colour_and_stay_out_of_the_accessibility_tree() {
        for p in PATHS {
            let svg = section_icon(p);
            // currentColor is what makes one set work in every theme pack.
            assert!(svg.contains("stroke=\"currentColor\""), "{p}");
            assert!(!svg.contains("#"), "{p} hardcodes a colour");
            // The link's text is the accessible name; the icon must not repeat it.
            assert!(svg.contains("aria-hidden=\"true\""), "{p}");
            assert!(svg.contains("focusable=\"false\""), "{p}");
        }
    }

    #[test]
    fn each_section_is_visually_distinct() {
        // A nav where two icons render identically is worse than no icons: it
        // tells you the rows differ when they don't.
        let mut seen: Vec<String> = Vec::new();
        for p in PATHS {
            let svg = section_icon(p);
            assert!(!seen.contains(&svg), "{p} duplicates another section's icon");
            seen.push(svg);
        }
    }

    #[test]
    fn the_rail_family_is_distinct_sized_alike_and_self_contained() {
        const RAIL: [&str; 5] = ["home", "people", "transfers", "you", "add"];
        let mut seen = Vec::new();
        for name in RAIL {
            let svg = rail_icon(name);
            assert!(svg.starts_with("<svg") && svg.ends_with("</svg>"), "{name}");
            assert!(svg.contains("stroke=\"currentColor\""), "{name}");
            assert!(svg.contains("aria-hidden=\"true\""), "{name}");
            assert!(svg.contains("viewBox=\"0 0 24 24\""), "{name} shares the grid");
            assert!(!svg.contains("http") && !svg.contains("<image"), "{name}");
            assert!(!seen.contains(&svg), "{name} duplicates another rail icon");
            seen.push(svg);
        }
        // People and You must not read as the same figure — that ambiguity is
        // what got the old set replaced.
        assert_ne!(rail_icon("people"), rail_icon("you"));
        // Unknown names degrade to the neutral dot, never a panic.
        assert!(rail_icon("wat").contains("circle"));
    }

    #[test]
    fn the_bell_and_file_icons_keep_the_module_contract() {
        // Same guarantees as the section set: self-contained, colour-inheriting,
        // silent to screen readers (the buttons carry the accessible names).
        for svg in [
            bell_icon(true),
            bell_icon(false),
            file_icon(true),
            file_icon(false),
        ] {
            assert!(svg.starts_with("<svg") && svg.ends_with("</svg>"));
            assert!(svg.contains("stroke=\"currentColor\""));
            assert!(svg.contains("aria-hidden=\"true\""));
            assert!(!svg.contains("http") && !svg.contains("<image"));
        }
        // The states must actually differ, or the toggle shows nothing.
        assert_ne!(bell_icon(true), bell_icon(false), "muted adds the slash");
        assert!(bell_icon(false).len() > bell_icon(true).len());
        assert_ne!(file_icon(true), file_icon(false), "folder and document differ");
    }

    #[test]
    fn an_unknown_route_still_draws_something() {
        // A new route added without an icon should look plain, not broken.
        let svg = section_icon("/wishing-well");
        assert!(svg.starts_with("<svg") && svg.contains("circle"));
    }
}
