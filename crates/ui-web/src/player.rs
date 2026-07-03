//! Browser audio playback for the radio (`wasm32-unknown-unknown` only).
//!
//! [`RadioPlayer`] is a thin wrapper over a detached
//! [`HtmlAudioElement`](web_sys::HtmlAudioElement) pointed at an Icecast
//! delivery mount. All decisions — whether to play, which URL, what volume —
//! are made by the pure, host-tested logic in [`crate::radio`]
//! ([`stream_url`](crate::radio::stream_url), [`RadioPrefs`]); this module is
//! only the DOM edge: create the element lazily, set `src`/`volume`/`muted`,
//! and call `play()`/`pause()`.

use web_sys::HtmlAudioElement;

use crate::radio::{stream_url, RadioPrefs};

/// A lazily created, detached `<audio>` element playing the radio stream.
pub struct RadioPlayer {
    audio: Option<HtmlAudioElement>,
    /// The currently assigned stream URL, so volume/mute changes never
    /// reassign `src` (which would restart the live stream).
    src: Option<String>,
}

impl RadioPlayer {
    /// A fresh player with no element yet; the `<audio>` element is created
    /// on first use so a user who never enables the radio pays nothing.
    pub fn new() -> Self {
        Self {
            audio: None,
            src: None,
        }
    }

    /// The audio element, created on first use.
    fn ensure(&mut self) -> Option<&HtmlAudioElement> {
        if self.audio.is_none() {
            let audio = HtmlAudioElement::new().ok()?;
            // A live stream has no meaningful preload; wait for play().
            audio.set_preload("none");
            self.audio = Some(audio);
        }
        self.audio.as_ref()
    }

    /// Point the element at `url`. A no-op when the URL is unchanged, so
    /// volume adjustments never restart the stream.
    pub fn set_src(&mut self, url: &str) {
        if self.src.as_deref() == Some(url) {
            return;
        }
        self.src = Some(url.to_string());
        if let Some(audio) = self.ensure() {
            audio.set_src(url);
        }
    }

    /// Start (or resume) playback. The returned promise is intentionally
    /// dropped: autoplay rejections surface as a silent player, and every
    /// call here follows a user gesture anyway.
    pub fn play(&mut self) {
        if let Some(audio) = self.ensure() {
            let _ = audio.play();
        }
    }

    /// Pause playback. A no-op when no element exists yet.
    pub fn pause(&mut self) {
        if let Some(audio) = &self.audio {
            let _ = audio.pause();
        }
    }

    /// Set the playback volume (clamped to `0.0..=1.0`).
    pub fn set_volume(&mut self, volume: f64) {
        if let Some(audio) = self.ensure() {
            audio.set_volume(volume.clamp(0.0, 1.0));
        }
    }

    /// Mute or unmute playback (the volume is remembered underneath).
    pub fn set_muted(&mut self, muted: bool) {
        if let Some(audio) = self.ensure() {
            audio.set_muted(muted);
        }
    }

    /// Reconcile the element with the user's preferences: derive the stream
    /// URL (pure, host-tested [`stream_url`]), then play at the chosen
    /// volume/mute when enabled — or pause otherwise.
    pub fn sync(&mut self, prefs: &RadioPrefs) {
        let url = prefs
            .station
            .as_deref()
            .and_then(|station| stream_url(&prefs.base, station));
        match url {
            Some(url) if prefs.enabled => {
                self.set_src(&url);
                self.set_volume(f64::from(prefs.volume));
                self.set_muted(prefs.muted);
                self.play();
            }
            _ => self.pause(),
        }
    }
}

impl Default for RadioPlayer {
    fn default() -> Self {
        Self::new()
    }
}
