//! ANSI/ASCII art pipeline: CP437, escape-sequence parsing, SAUCE, rendering.
//!
//! ANSI art is a first-class medium in RabbitHole (PLAN.md §9.8): welcome
//! screens, board headers, gallery file areas, and user profiles all speak
//! it. The scene's file format is deceptively hostile to modern software —
//! CP437 bytes, terminal escape sequences as a drawing language, metadata
//! smuggled after an EOF byte, and a 30-year-old convention (iCE colors)
//! that redefines what "blink" means. This crate owns all of that so the
//! rest of the workspace deals only in parsed canvases and clean strings.
//!
//! The pipeline is: bytes → [`ansi::AnsiParser`] → [`ansi::Canvas`] →
//! renderer. [`sauce::SauceRecord`] is read first when present, since it
//! supplies the canvas width and the iCE-color flag the parser needs.
//! Everything is `std`-only and total: arbitrary input may render as
//! garbage, but it can never panic or exhaust memory.
//!
//! ```
//! use rabbithole_art::{ansi, render, sauce::SauceRecord};
//!
//! let file = b"\x1b[1;36mRabbit\x1b[0;36mHole\x1b[0m";
//! let body = SauceRecord::strip(file);
//! let ice = SauceRecord::from_bytes(file).is_some_and(|s| s.ice_colors());
//! let canvas = ansi::parse_with(body, ansi::DEFAULT_WIDTH, ice);
//! assert_eq!(render::render_plain(&canvas), "RabbitHole\n");
//! ```

#![forbid(unsafe_code)]

pub mod ansi;
pub mod cp437;
pub mod render;
pub mod sauce;

pub use ansi::{AnsiParser, Attrs, Canvas, Cell};
pub use cp437::{cp437_to_string, cp437_to_unicode, unicode_to_cp437, unicode_to_cp437_lossy};
pub use render::{render_ansi, render_plain};
pub use sauce::SauceRecord;

#[cfg(test)]
mod tests {
    use super::*;

    /// End-to-end: a SAUCE'd iCE-color ANSI renders with bright backgrounds.
    #[test]
    fn full_pipeline_sauce_to_render() {
        let record = SauceRecord {
            title: "demo".into(),
            datatype: sauce::data_type::CHARACTER,
            filetype: sauce::character_file_type::ANSI,
            tinfo1: 40,
            tflags: sauce::TFLAGS_ICE_COLORS,
            ..Default::default()
        };
        let mut file = b"\x1b[5;44;33mhi".to_vec();
        record.append_to(&mut file);

        let parsed = SauceRecord::from_bytes(&file).expect("sauce present");
        let width = parsed.width_hint().unwrap_or(ansi::DEFAULT_WIDTH);
        assert_eq!(width, 40);
        let canvas = ansi::parse_with(SauceRecord::strip(&file), width, parsed.ice_colors());

        assert_eq!(canvas.width(), 40);
        let cell = canvas.cell(0, 0).unwrap();
        assert_eq!(cell.bg, 12); // iCE: blink became bright blue background
        assert!(!cell.attrs.contains(Attrs::BLINK));
        assert_eq!(render_plain(&canvas), "hi\n");
        assert_eq!(render_ansi(&canvas), "\x1b[0;33;104mhi\x1b[0m\n");
    }

    /// The SAUCE trailer itself must never leak into the rendered art,
    /// even when callers skip `strip` (the 0x1A EOF byte protects them).
    #[test]
    fn sauce_trailer_never_renders() {
        let mut file = b"art".to_vec();
        SauceRecord {
            title: "hidden".into(),
            ..Default::default()
        }
        .append_to(&mut file);
        let canvas = ansi::parse(&file);
        let text = render_plain(&canvas);
        assert_eq!(text, "art\n");
        assert!(!text.contains("hidden"));
    }
}
