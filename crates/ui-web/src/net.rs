//! Tiny `fetch` wrapper for the one thing this client GETs over HTTP.
//!
//! Everything else in RabbitHole rides the RHP socket; the Looking Glass
//! directory is the exception, because a directory you can only reach *after*
//! joining a burrow is no use for finding one. Kept deliberately small — no
//! HTTP client crate in the wasm bundle for a single GET.

/// Fetch a URL as text. `None` on any failure — a directory that can't be
/// reached is a fallback, not an error to propagate.
#[cfg(target_arch = "wasm32")]
pub async fn fetch_text(url: &str) -> Option<String> {
    use wasm_bindgen::JsCast;
    use wasm_bindgen_futures::JsFuture;

    let window = web_sys::window()?;
    let resp = JsFuture::from(window.fetch_with_str(url)).await.ok()?;
    let resp: web_sys::Response = resp.dyn_into().ok()?;
    if !resp.ok() {
        return None;
    }
    let text = JsFuture::from(resp.text().ok()?).await.ok()?;
    text.as_string()
}

/// Host stand-in: no DOM, no fetch.
#[cfg(not(target_arch = "wasm32"))]
pub async fn fetch_text(_url: &str) -> Option<String> {
    None
}
