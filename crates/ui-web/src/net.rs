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
    let abort = web_sys::AbortController::new().ok()?;
    let timer_abort = abort.clone();
    let timeout = gloo_timers::callback::Timeout::new(5_000, move || timer_abort.abort());
    let result = async {
        let request = web_sys::RequestInit::new();
        request.set_signal(Some(&abort.signal()));
        let resp = JsFuture::from(window.fetch_with_str_and_init(url, &request))
            .await
            .ok()?;
        let resp: web_sys::Response = resp.dyn_into().ok()?;
        if !resp.ok() {
            return None;
        }
        let reader = resp
            .body()?
            .get_reader()
            .dyn_into::<web_sys::ReadableStreamDefaultReader>()
            .ok()?;
        let mut bytes = Vec::new();
        loop {
            let chunk = JsFuture::from(reader.read()).await.ok()?;
            if js_sys::Reflect::get(&chunk, &"done".into())
                .ok()?
                .as_bool()?
            {
                break;
            }
            let chunk = js_sys::Reflect::get(&chunk, &"value".into())
                .ok()?
                .dyn_into::<js_sys::Uint8Array>()
                .ok()?;
            // Enforce decoded bytes while reading, including chunked replies
            // without Content-Length. A proof listing is never an open stream.
            if bytes.len().saturating_add(chunk.length() as usize) > 1024 * 1024 {
                return None;
            }
            bytes.extend_from_slice(&chunk.to_vec());
        }
        String::from_utf8(bytes).ok()
    }
    .await;
    abort.abort();
    drop(timeout);
    result
}

/// Host stand-in: no DOM, no fetch.
#[cfg(not(target_arch = "wasm32"))]
pub async fn fetch_text(_url: &str) -> Option<String> {
    None
}
