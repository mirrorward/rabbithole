//! Backend-agnostic text-UI surface for RabbitHole's retro clients.
//!
//! RabbitHole shows one BBS look through two very different doors: a modern
//! UTF-8 terminal and a 30-year-old CP437/ANSI telnet client (SyncTERM,
//! NetRunner, qodem). This crate is the seam between them. A [`Screen`] is a
//! plain `width × height` grid of character [`Cell`]s with a chainable
//! drawing API — move, color, print, boxes, rules — that never touches a
//! socket. [`Screen::flush`] then serializes the whole grid to wire bytes
//! for a chosen [`ScreenMode`]: Unicode glyphs + ANSI SGR for a modern
//! terminal, or CP437 bytes + ANSI SGR for a legacy one.
//!
//! Encoding and box-drawing lean entirely on [`rabbithole_art`]: glyphs are
//! stored as Unicode and mapped through its CP437 tables on a CP437 flush,
//! so a flushed screen round-trips back through the art ANSI parser
//! byte-for-byte. This slice is a direct cell buffer rather than a ratatui
//! backend — simpler, dependency-light, and enough to build the telnet BBS
//! and the local TUI on the same primitives.
//!
//! ```
//! use rabbithole_screen::{BoxStyle, Color, Menu, Screen, ScreenMode};
//!
//! let mut screen = Screen::new(24, 8, ScreenMode::Cp437Ansi);
//! screen.fg(Color::BrightCyan).draw_box(0, 0, 24, 8, BoxStyle::Double);
//! Menu::new("MAIN MENU", &["Chat", "Boards", "Files"])
//!     .select(1)
//!     .render(&mut screen, 2, 1);
//! let wire: Vec<u8> = screen.flush(ScreenMode::Cp437Ansi);
//! assert!(wire.starts_with(b"\x1b[0m\x1b[2J\x1b[H"));
//! ```

#![forbid(unsafe_code)]

mod color;
mod menu;
mod screen;

pub use color::Color;
pub use menu::Menu;
pub use screen::{BoxStyle, Screen, ScreenMode};

// Re-exported from `rabbithole-art` so callers can inspect flushed cells
// (and the attribute flags a menu highlight sets) without a second dep.
pub use rabbithole_art::ansi::{Attrs, Cell};
