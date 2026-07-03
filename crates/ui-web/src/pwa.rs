//! PWA install-readiness: the service-worker registration edge plus the
//! host-tested shape of the shell assets `trunk build` ships.
//!
//! The installable story is three checked-in pieces under `assets/`, copied
//! to the web root by `index.html`'s `data-trunk rel="copy-file"` links (the
//! embedded server then serves them at `/`; see `apps/server/src/http.rs` —
//! it generates `/manifest.webmanifest` only when the web root ships none,
//! so our checked-in manifest wins):
//!
//! - **`sw.js`** — the app-shell service worker: one version-stamped cache
//!   (`CACHE_VERSION`), runtime fetch-then-cache for trunk's content-hashed
//!   bundles (no precache list to drift), network-first navigations falling
//!   back to the cached shell document, a hard `/files/` bypass (downloads
//!   are never cached, so never stale), and old-version cleanup on activate.
//!   Plain JS, no external code; its load-bearing markers are asserted
//!   textually by the shape tests below — crude, but drift is visible.
//! - **`manifest.webmanifest`** — name/short_name "RabbitHole", standalone
//!   display, `start_url`/`scope` `/`, colours from the Clean-dark pack
//!   tokens ([`crate::packs::PackTokens`]), and two maskable icons.
//! - **`icon-192.png` / `icon-512.png`** — rendered by [`icon_rgba`] (a
//!   rabbit-hole disc on the accent field) and written once by the
//!   `#[ignore]`d `regenerate_icons` test; a normal test run decodes the
//!   checked-in bytes and compares them against the generator, so the PNGs
//!   cannot drift from the code that describes them.
//!
//! Registration itself ([`register_service_worker`], wasm only) is a
//! fire-and-forget edge in the boot path ([`crate::app::mount`]): it
//! feature-detects `navigator.serviceWorker` (absent on insecure contexts
//! and older browsers) and logs — never throws — on failure, so the app
//! boots identically with or without a worker.

/// URL the service worker is registered under. It must sit at the web-root
/// top level: a worker's default scope is its own directory, and ours has
/// to cover the whole app (`/`).
pub const SW_URL: &str = "/sw.js";

/// URL of the shipped manifest — matches both the `<link rel="manifest">`
/// in `index.html` and the embedded server's generated-fallback path.
pub const MANIFEST_URL: &str = "/manifest.webmanifest";

/// The icon field colour: Clean-dark `--rh-accent`. Cross-checked against
/// the pack tokens by `icon_and_manifest_colours_match_the_default_pack`.
const ICON_FIELD: [u8; 3] = [0x6c, 0x9c, 0xff];

/// The rabbit hole itself: Clean-dark `--rh-bg`.
const ICON_HOLE: [u8; 3] = [0x14, 0x16, 0x1b];

/// The hole's radius as a fraction of the icon edge. Maskable icons must
/// keep their motif inside the safe zone — a centred circle of 40% radius —
/// so 30% survives every platform mask (circle, squircle, rounded square).
const HOLE_RADIUS: f64 = 0.30;

/// Render the install icon: `size` × `size` fully opaque RGBA pixels — a
/// dark rabbit-hole disc centred on the accent field, with a one-pixel
/// antialiased rim. Pure and deterministic, so the checked-in PNGs are
/// reproducible from this function alone (see the module notes).
pub fn icon_rgba(size: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity(size as usize * size as usize * 4);
    let centre = (f64::from(size) - 1.0) / 2.0;
    let radius = f64::from(size) * HOLE_RADIUS;
    for y in 0..size {
        for x in 0..size {
            let dx = f64::from(x) - centre;
            let dy = f64::from(y) - centre;
            let dist = (dx * dx + dy * dy).sqrt();
            // 0 inside the hole, 1 on the field, blended across one pixel.
            let t = (dist - radius + 0.5).clamp(0.0, 1.0);
            for channel in 0..3 {
                let hole = f64::from(ICON_HOLE[channel]);
                let field = f64::from(ICON_FIELD[channel]);
                out.push((hole + (field - hole) * t).round() as u8);
            }
            out.push(0xff); // maskable icons must not rely on alpha
        }
    }
    out
}

/// Register `/sw.js` from the boot path. Fire-and-forget: unsupported and
/// insecure contexts are a silent no-op, and a failed registration is logged
/// and swallowed — the worker is an enhancement, never a dependency.
#[cfg(target_arch = "wasm32")]
pub fn register_service_worker() {
    use wasm_bindgen::JsValue;

    let Some(window) = web_sys::window() else {
        return;
    };
    let navigator = window.navigator();
    // Feature-detect instead of trusting the binding: on insecure contexts
    // (plain http that is not localhost) and older browsers,
    // `navigator.serviceWorker` is simply absent, and the app must boot
    // identically without it.
    let has_worker = js_sys::Reflect::has(navigator.as_ref(), &JsValue::from_str("serviceWorker"))
        .unwrap_or(false);
    if !has_worker {
        return;
    }
    let promise = navigator.service_worker().register(SW_URL);
    wasm_bindgen_futures::spawn_local(async move {
        // Quota pressure, private-mode storage, or a bad sw.js deploy: log
        // it and move on — never break the app over its own cache.
        if let Err(err) = wasm_bindgen_futures::JsFuture::from(promise).await {
            leptos::logging::warn!("service worker registration failed: {err:?}");
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::packs::PackTokens;

    /// The checked-in shell assets, read at compile time so the tests always
    /// see exactly what trunk will copy.
    const SW_JS: &str = include_str!("../assets/sw.js");
    const MANIFEST: &str = include_str!("../assets/manifest.webmanifest");
    const INDEX_HTML: &str = include_str!("../index.html");

    const ICON_SIZES: [u32; 2] = [192, 512];

    fn icon_path(size: u32) -> String {
        format!("{}/assets/icon-{size}.png", env!("CARGO_MANIFEST_DIR"))
    }

    fn hex(c: [u8; 3]) -> String {
        format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2])
    }

    // ---- manifest shape ---------------------------------------------------

    #[test]
    fn manifest_is_installable() {
        let m: serde_json::Value = serde_json::from_str(MANIFEST).expect("valid JSON");
        assert_eq!(m["name"], "RabbitHole");
        assert_eq!(m["short_name"], "RabbitHole");
        assert_eq!(m["display"], "standalone");
        assert_eq!(m["start_url"], "/");
        assert_eq!(m["scope"], "/");
        let icons = m["icons"].as_array().expect("icons array");
        assert!(icons.len() >= 2, "install prompts want at least two icons");
        let mut sizes: Vec<&str> = Vec::new();
        for icon in icons {
            let src = icon["src"].as_str().expect("icon src");
            assert!(src.starts_with('/'), "icon src should be root-relative");
            assert_eq!(icon["type"], "image/png");
            let purpose = icon["purpose"].as_str().expect("icon purpose");
            assert!(purpose.contains("maskable"), "icons must be maskable");
            sizes.push(icon["sizes"].as_str().expect("icon sizes"));
        }
        assert!(sizes.contains(&"192x192") && sizes.contains(&"512x512"));
    }

    #[test]
    fn icon_and_manifest_colours_match_the_default_pack() {
        // The manifest colours and the icon palette are the Clean-dark pack
        // tokens; if the pack ever changes, this pins the drift.
        let clean_dark = PackTokens::default().dark;
        let m: serde_json::Value = serde_json::from_str(MANIFEST).expect("valid JSON");
        assert_eq!(m["background_color"], clean_dark["--rh-bg"].as_str());
        assert_eq!(m["theme_color"], clean_dark["--rh-accent"].as_str());
        assert_eq!(hex(ICON_FIELD), clean_dark["--rh-accent"]);
        assert_eq!(hex(ICON_HOLE), clean_dark["--rh-bg"]);
    }

    // ---- sw.js shape (textual, deliberately crude) ------------------------

    #[test]
    fn sw_js_declares_a_version_stamped_cache() {
        let line = SW_JS
            .lines()
            .find(|l| l.starts_with("const CACHE_VERSION"))
            .expect("sw.js must declare CACHE_VERSION");
        let value = line
            .split('"')
            .nth(1)
            .expect("CACHE_VERSION must be a string literal");
        assert!(
            value.starts_with("rabbithole-shell-v"),
            "cache version should be recognisably ours and versioned: {value}"
        );
        assert!(
            value.len() > "rabbithole-shell-v".len(),
            "cache version needs an actual version suffix"
        );
    }

    #[test]
    fn sw_js_bypasses_files_and_cleans_up_old_caches() {
        // The /files/ bypass marker: downloads must never be cached.
        assert!(
            SW_JS.contains(r#"const FILES_PREFIX = "/files/";"#),
            "sw.js lost its /files/ bypass constant"
        );
        assert!(SW_JS.contains("FILES_PREFIX"), "bypass must be used");
        // Old cache versions are deleted on activate.
        assert!(SW_JS.contains("caches.delete"));
        assert!(SW_JS.contains("clients.claim"));
        assert!(SW_JS.contains("skipWaiting"));
        // The three lifecycle hooks exist.
        for event in ["install", "activate", "fetch"] {
            assert!(
                SW_JS.contains(&format!("addEventListener(\"{event}\"")),
                "sw.js must handle the {event} event"
            );
        }
        // Navigation fallback + same-origin discipline markers.
        assert!(SW_JS.contains(r#"request.mode === "navigate""#));
        assert!(SW_JS.contains("self.location.origin"));
    }

    // ---- index.html trunk wiring -------------------------------------------

    #[test]
    fn index_html_wires_the_pwa_through_trunk() {
        // The rust link builds this crate's bin; the copy-file links land the
        // shell assets at the dist root, where SW_URL/MANIFEST_URL expect them.
        assert!(INDEX_HTML.contains(r#"data-trunk rel="rust""#));
        for asset in [
            "assets/sw.js",
            "assets/manifest.webmanifest",
            "assets/icon-192.png",
            "assets/icon-512.png",
        ] {
            assert!(
                INDEX_HTML.contains(&format!(r#"data-trunk rel="copy-file" href="{asset}""#)),
                "index.html must trunk-copy {asset}"
            );
        }
        assert!(INDEX_HTML.contains(&format!(r#"<link rel="manifest" href="{MANIFEST_URL}""#)));
        assert!(INDEX_HTML.contains(r#"<meta name="theme-color""#));
    }

    #[test]
    fn urls_are_root_scoped() {
        // Both URLs live at the web-root top level: the worker so its scope
        // covers "/", the manifest so it shadows the server's generated one.
        for url in [SW_URL, MANIFEST_URL] {
            assert!(url.starts_with('/'));
            assert!(!url.trim_start_matches('/').contains('/'));
        }
        assert_eq!(SW_URL, "/sw.js");
        assert_eq!(MANIFEST_URL, "/manifest.webmanifest");
    }

    // ---- icons --------------------------------------------------------------

    #[test]
    fn icon_rgba_paints_a_hole_on_the_accent_field() {
        for size in ICON_SIZES {
            let px = icon_rgba(size);
            assert_eq!(px.len(), size as usize * size as usize * 4);
            // Every pixel fully opaque: maskable icons must not rely on alpha.
            assert!(px.chunks_exact(4).all(|p| p[3] == 0xff), "{size} opaque");
            // The corner is the untouched accent field...
            assert_eq!(px[..3], ICON_FIELD, "{size} corner");
            // ...and the centre is the hole.
            let centre = ((size / 2) * size + size / 2) as usize * 4;
            assert_eq!(px[centre..centre + 3], ICON_HOLE, "{size} centre");
        }
    }

    #[test]
    fn checked_in_icons_match_the_generator() {
        // Decode (not byte-compare) so a png-crate encoder change can't fail
        // this; only a real pixel drift between assets/ and icon_rgba can.
        for size in ICON_SIZES {
            let path = icon_path(size);
            let bytes = std::fs::read(&path).unwrap_or_else(|e| {
                panic!(
                    "{path}: {e}; regenerate with `cargo test -p rabbithole-ui-web \
                     regenerate_icons -- --ignored`"
                )
            });
            let decoder = png::Decoder::new(bytes.as_slice());
            let mut reader = decoder.read_info().expect("valid PNG");
            let mut buf = vec![0u8; reader.output_buffer_size()];
            let info = reader.next_frame(&mut buf).expect("decodable PNG");
            assert_eq!((info.width, info.height), (size, size));
            assert_eq!(info.color_type, png::ColorType::Rgba);
            assert_eq!(info.bit_depth, png::BitDepth::Eight);
            buf.truncate(info.buffer_size());
            assert_eq!(buf, icon_rgba(size), "icon-{size}.png drifted");
        }
    }

    /// Regenerate the checked-in icons from [`icon_rgba`]. `#[ignore]`d so a
    /// normal test run never writes into the tree; run explicitly after
    /// changing the generator — `checked_in_icons_match_the_generator` keeps
    /// the outputs honest in every normal run.
    #[test]
    #[ignore = "writes into assets/; run explicitly to regenerate the icons"]
    fn regenerate_icons() {
        for size in ICON_SIZES {
            let mut out = Vec::new();
            let mut encoder = png::Encoder::new(&mut out, size, size);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().expect("PNG header");
            writer.write_image_data(&icon_rgba(size)).expect("PNG data");
            writer.finish().expect("PNG finish");
            std::fs::write(icon_path(size), out).expect("write icon");
        }
    }
}
