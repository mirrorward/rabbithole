//! Putting downloaded bytes on disk.
//!
//! One door for every download whose bytes the client already holds: an
//! inline transfer over the socket, or a seeded demo file. (A swarm download
//! is different: the desktop shell fetches and writes it itself.)
//!
//! In a browser tab the only way to save is to hand the browser a Blob and
//! click a detached `<a download>`; it picks the folder. In the desktop shell
//! that click goes nowhere (a webview has no download manager), so the bytes
//! go to the shell, which writes them into the downloads folder and says
//! where. That second path is why demo downloads "failed" in the app.

/// The person's download preferences, as the desktop shell reports them. The
/// shell owns them, and a folder is only ever chosen in a native panel: the
/// webview shows them and asks for changes, it never names a path.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DownloadPrefs {
    /// Where downloads go without asking. `None`: a save panel each time.
    pub folder: Option<String>,
    /// Whether each burrow's files get a folder of their own.
    pub per_burrow: bool,
    /// The system downloads folder, where a save panel opens.
    pub system_folder: String,
}

impl DownloadPrefs {
    /// What the Settings screen says downloads do, in a sentence.
    pub fn summary(&self) -> String {
        match (&self.folder, self.per_burrow) {
            (None, false) => "Each download asks where to save.".to_string(),
            (None, true) => {
                "Each download asks where to save, starting in a folder named for its burrow."
                    .to_string()
            }
            (Some(folder), false) => format!("Downloads go to {folder} without asking."),
            (Some(folder), true) => format!(
                "Downloads go to {folder}, in a folder named for their burrow, without asking."
            ),
        }
    }
}

/// Save `bytes` as `name`. Reports the outcome through the app's toasts when
/// there is something to say: where the shell put it, or why it could not.
#[cfg(target_arch = "wasm32")]
pub fn save_bytes(name: &str, mime: &str, bytes: &[u8]) {
    if crate::native::native_available() {
        let shown = name.to_string();
        let burrow = crate::app::current()
            .map(|app| app.focused_burrow_label())
            .unwrap_or_default();
        crate::native::save_file(name, bytes, &burrow, move |outcome| {
            let Some(app) = crate::app::current() else {
                return;
            };
            match outcome {
                // The person closed the save panel. They know; say nothing.
                Ok(None) => {}
                Ok(Some(path)) => {
                    app.notify(
                        crate::toasts::ToastKind::Success,
                        format!("Saved {shown} to {path}"),
                    );
                }
                Err(why) => {
                    app.notify(
                        crate::toasts::ToastKind::Warn,
                        format!("Couldn\u{2019}t save {shown}: {why}"),
                    );
                }
            }
        });
        return;
    }
    browser_save(name, mime, bytes);
}

/// Host builds have no disk door; the reducers are what get tested there.
#[cfg(not(target_arch = "wasm32"))]
pub fn save_bytes(_name: &str, _mime: &str, _bytes: &[u8]) {}

/// Wrap the bytes in a `Blob`, mint an object URL, and click a detached
/// `<a download>`. No DOM element is left behind; the URL is revoked at once.
#[cfg(target_arch = "wasm32")]
fn browser_save(name: &str, mime: &str, bytes: &[u8]) {
    use wasm_bindgen::JsCast;
    let array = js_sys::Array::new();
    array.push(&js_sys::Uint8Array::from(bytes));
    let opts = web_sys::BlobPropertyBag::new();
    opts.set_type(if mime.is_empty() {
        "application/octet-stream"
    } else {
        mime
    });
    let Ok(blob) = web_sys::Blob::new_with_u8_array_sequence_and_options(&array, &opts) else {
        return;
    };
    let Ok(url) = web_sys::Url::create_object_url_with_blob(&blob) else {
        return;
    };
    if let Some(doc) = web_sys::window().and_then(|w| w.document()) {
        if let Ok(el) = doc.create_element("a") {
            if let Ok(a) = el.dyn_into::<web_sys::HtmlAnchorElement>() {
                a.set_href(&url);
                a.set_download(name);
                a.click();
            }
        }
    }
    let _ = web_sys::Url::revoke_object_url(&url);
}

#[cfg(test)]
mod tests {
    use super::DownloadPrefs;

    #[test]
    fn the_summary_says_what_a_download_will_do() {
        let mut p = DownloadPrefs::default();
        assert_eq!(p.summary(), "Each download asks where to save.");
        p.per_burrow = true;
        assert!(p.summary().contains("named for its burrow"));
        p.folder = Some("/Volumes/Big/Warren".into());
        assert_eq!(
            p.summary(),
            "Downloads go to /Volumes/Big/Warren, in a folder named for their burrow, without asking."
        );
        p.per_burrow = false;
        assert_eq!(
            p.summary(),
            "Downloads go to /Volumes/Big/Warren without asking."
        );
    }
}
