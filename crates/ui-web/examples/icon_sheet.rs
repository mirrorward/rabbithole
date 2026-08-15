//! Contact sheet for the nav icon sets — the rail family and the sidebar
//! sections, at the size each is actually used. The unit tests prove the SVGs
//! are well-formed and distinct; only eyes can tell whether they sit at the
//! same optical weight and read as what they name. Same idea as
//! `sprite_sheet.rs` for the avatars.
//!
//! `cargo run -p rabbithole-ui-web --example icon_sheet -- /tmp/icons.html`

use rabbithole_ui_web::icons::{bell_icon, file_icon, rail_icon, section_icon};

fn main() {
    let out = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "icons.html".into());
    let mut h = String::from(
        "<!doctype html><meta charset=utf-8><title>Nav icons</title>\
         <body style='background:#f6f7f9;font:12px system-ui;padding:24px;color:#333'>\
         <h3>Rail (20px, on 40px tiles)</h3><div style='display:flex;gap:10px'>",
    );
    for name in ["home", "people", "transfers", "you", "add"] {
        h.push_str(&format!(
            "<div style='text-align:center'><div style='width:40px;height:40px;display:grid;\
             place-items:center;background:#fff;border-radius:12px;border:1px solid #ddd'>\
             <div style='width:20px;height:20px'>{}</div></div>{}</div>",
            rail_icon(name).replace("width=\"18\" height=\"18\"", "width=\"20\" height=\"20\""),
            name,
        ));
    }
    h.push_str("</div><h3>Sidebar (18px)</h3><div style='display:flex;gap:14px'>");
    for path in [
        "/lobby",
        "/boards",
        "/dms",
        "/directory",
        "/files",
        "/radio",
        "/art",
        "/admin",
    ] {
        h.push_str(&format!(
            "<div style='text-align:center'>{}<br>{}</div>",
            section_icon(path),
            path.trim_start_matches('/'),
        ));
    }
    h.push_str("</div><h3>Controls</h3><div style='display:flex;gap:14px'>");
    for (svg, name) in [
        (bell_icon(true), "bell on"),
        (bell_icon(false), "bell off"),
        (file_icon(true), "folder"),
        (file_icon(false), "file"),
    ] {
        h.push_str(&format!(
            "<div style='text-align:center'>{svg}<br>{name}</div>"
        ));
    }
    h.push_str("</div></body>");
    std::fs::write(&out, h).expect("write the contact sheet");
    println!("wrote {out}");
}
