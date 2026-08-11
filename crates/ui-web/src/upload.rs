//! Uploading files from the browser — drag-and-drop and a real file picker.
//!
//! The FILE family's `FileUpload` carries bytes **inline**, so an upload has to
//! fit inside the protocol's 1 MiB frame ([`rabbithole_proto::codec::MAX_FRAME_SIZE`]).
//! That limit is real, so the client enforces it up front and says so plainly
//! rather than letting the server reject a big file after the whole read.
//!
//! The checks and the MIME guess are pure and host-tested; only the
//! `File`→bytes read is wasm-gated.

/// Largest file we'll send inline. The 1 MiB frame also carries the area slug,
/// folder path, name, MIME and comment, so leave the envelope room.
pub const MAX_INLINE_UPLOAD: u64 = 900 * 1024;

/// Can this file be uploaded inline? `Err` carries a message meant for a human.
/// Pure — host-tested.
pub fn check_upload(name: &str, size: u64) -> Result<(), String> {
    if name.trim().is_empty() {
        return Err("That file has no name.".to_string());
    }
    if size == 0 {
        return Err(format!("\u{201c}{name}\u{201d} is empty."));
    }
    if size > MAX_INLINE_UPLOAD {
        return Err(format!(
            "\u{201c}{name}\u{201d} is {} \u{2014} uploads are limited to {} for now.",
            crate::files::human_size(size as i64),
            crate::files::human_size(MAX_INLINE_UPLOAD as i64),
        ));
    }
    Ok(())
}

/// A MIME type for a filename, from its extension. Falls back to
/// `application/octet-stream` — honest about not knowing rather than guessing
/// something that makes a browser mis-render it. Pure — host-tested.
pub fn guess_mime(name: &str) -> &'static str {
    let ext = name
        .rsplit_once('.')
        .map(|(_, e)| e.to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "txt" | "md" | "nfo" | "diz" => "text/plain",
        "ans" | "asc" => "text/x-ansi",
        "html" | "htm" => "text/html",
        "json" => "application/json",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "mp3" => "audio/mpeg",
        "ogg" | "oga" => "audio/ogg",
        "wav" => "audio/wav",
        "flac" => "audio/flac",
        "mp4" | "m4v" => "video/mp4",
        "zip" => "application/zip",
        "gz" | "tgz" => "application/gzip",
        "pdf" => "application/pdf",
        _ => "application/octet-stream",
    }
}

#[cfg(target_arch = "wasm32")]
mod browser {
    use leptos::SignalUpdate;
    use wasm_bindgen::JsCast;
    use wasm_bindgen_futures::{spawn_local, JsFuture};

    use crate::app::AppState;

    /// Read every file in `list` and upload the ones that fit, reporting each
    /// rejection as a toast so a refused file is never silent.
    pub fn upload_file_list(app: AppState, list: web_sys::FileList) {
        for i in 0..list.length() {
            let Some(file) = list.get(i) else { continue };
            upload_one(app, file);
        }
    }

    /// Read one `File` (a `Blob`) into bytes and hand it to the transport.
    pub fn upload_one(app: AppState, file: web_sys::File) {
        let name = file.name();
        let size = file.size() as u64;
        if let Err(msg) = super::check_upload(&name, size) {
            app.toasts
                .update(|q| { q.push(crate::toasts::ToastKind::Warn, msg); });
            return;
        }
        spawn_local(async move {
            // `File` inherits `Blob::array_buffer()`, so no FileReader dance.
            let Ok(buf) = JsFuture::from(file.array_buffer()).await else {
                app.toasts.update(|q| {
                    q.push(
                        crate::toasts::ToastKind::Warn,
                        format!("Couldn't read \u{201c}{name}\u{201d}."),
                    );
                });
                return;
            };
            let Ok(buf) = buf.dyn_into::<js_sys::ArrayBuffer>() else {
                return;
            };
            let bytes = js_sys::Uint8Array::new(&buf).to_vec();
            app.upload(&name, bytes);
        });
    }
}

#[cfg(target_arch = "wasm32")]
pub use browser::{upload_file_list, upload_one};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oversized_and_empty_files_are_refused_with_a_readable_reason() {
        assert!(check_upload("demo.zip", 1024).is_ok());
        assert!(check_upload("demo.zip", MAX_INLINE_UPLOAD).is_ok());

        let too_big = check_upload("huge.iso", MAX_INLINE_UPLOAD + 1).unwrap_err();
        assert!(too_big.contains("huge.iso"), "names the file: {too_big}");
        assert!(too_big.contains("limited to"), "explains the limit: {too_big}");

        assert!(check_upload("empty.txt", 0).unwrap_err().contains("empty"));
        assert!(check_upload("   ", 10).unwrap_err().contains("no name"));
    }

    #[test]
    fn the_inline_cap_fits_inside_a_protocol_frame() {
        // The upload rides inline in a single frame alongside its metadata, so
        // the cap must leave envelope room under the hard 1 MiB limit.
        assert!(MAX_INLINE_UPLOAD < rabbithole_proto::codec::MAX_FRAME_SIZE as u64);
    }

    #[test]
    fn mime_is_guessed_from_the_extension_or_admitted_unknown() {
        assert_eq!(guess_mime("readme.txt"), "text/plain");
        assert_eq!(guess_mime("LOADER.NFO"), "text/plain", "case-insensitive");
        assert_eq!(guess_mime("art.ans"), "text/x-ansi");
        assert_eq!(guess_mime("tune.mp3"), "audio/mpeg");
        assert_eq!(guess_mime("pack.zip"), "application/zip");
        // No extension, or one we don't know: say so rather than guess wrong.
        assert_eq!(guess_mime("COPYING"), "application/octet-stream");
        assert_eq!(guess_mime("thing.qqq"), "application/octet-stream");
    }
}
