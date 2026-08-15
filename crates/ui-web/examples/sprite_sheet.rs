//! Write a contact sheet of every warren mark to an HTML file, so the sprites
//! can be *looked at*. The unit tests can prove a sprite is well-formed, 8×8 and
//! distinct from its neighbours; they cannot tell you whether the rabbit reads
//! as a rabbit at 20px, which is the only thing that matters. This is how that
//! gets checked.
//!
//! `cargo run -p rabbithole-ui-web --example sprite_sheet -- /tmp/marks.html`

use rabbithole_ui_web::avatar::{glyph_name, glyph_svg, GLYPH_COUNT, PALETTE};

fn main() {
    let out = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "sprites.html".into());
    let mut h = String::from(
        "<!doctype html><meta charset=utf-8><title>Warren marks</title>\
         <body style='background:#fff;font:12px system-ui;padding:24px'>\
         <div style='display:grid;grid-template-columns:repeat(8,1fr);gap:16px;max-width:900px'>",
    );
    for i in 0..GLYPH_COUNT {
        // Big enough to judge the drawing, and again at the size it's actually
        // used in a chat line — a sprite that only works large is no good.
        h.push_str(&format!(
            "<div style='text-align:center'>{}<br>{}<div style='opacity:.6'>{}</div></div>",
            glyph_svg(i, i % PALETTE.len(), 64),
            glyph_svg(i, i % PALETTE.len(), 20),
            glyph_name(i),
        ));
    }
    h.push_str("</div></body>");
    std::fs::write(&out, h).expect("write the contact sheet");
    println!("wrote {out}");
}
