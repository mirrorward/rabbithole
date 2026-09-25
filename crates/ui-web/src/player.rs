//! Browser radio audio, with observed playback state and explicit recovery.
//! Preferences express intent; a play promise or media error tells us what
//! actually happened. The host-tested reducer rejects obsolete answers.

use std::cell::RefCell;
use std::rc::Rc;

use wasm_bindgen::{closure::Closure, JsCast, JsValue};
use wasm_bindgen_futures::{spawn_local, JsFuture};
use web_sys::{Event, HtmlAudioElement};

use crate::playback::{Playback, PlaybackAction, PlaybackStatus};
use crate::radio::RadioPrefs;

struct Shared {
    model: RefCell<Playback>,
    changed: Box<dyn Fn(PlaybackStatus)>,
}

impl Shared {
    fn notify(&self) {
        let status = self.model.borrow().status();
        (self.changed)(status);
    }
}

/// One lazily created audio element. A new attempt owns a fresh element so
/// an old source's queued media events cannot be mistaken for the new one.
/// Volume, mute and metadata updates keep the current element and stream.
pub struct RadioPlayer {
    audio: Option<HtmlAudioElement>,
    listeners: Vec<(&'static str, Closure<dyn FnMut(Event)>)>,
    shared: Rc<Shared>,
}

impl RadioPlayer {
    pub fn new() -> Self {
        Self::with_status(|_| {})
    }

    pub fn with_status(changed: impl Fn(PlaybackStatus) + 'static) -> Self {
        Self {
            audio: None,
            listeners: vec![],
            shared: Rc::new(Shared {
                model: RefCell::new(Playback::default()),
                changed: Box::new(changed),
            }),
        }
    }

    fn release(&mut self) {
        if let Some(audio) = self.audio.take() {
            for (event, callback) in self.listeners.drain(..) {
                let _ = audio
                    .remove_event_listener_with_callback(event, callback.as_ref().unchecked_ref());
            }
            let _ = audio.pause();
            let _ = audio.remove_attribute("src");
            // Stop downloading and reject any pending play promise. Its weak
            // callback is already invalidated by the reducer's generation.
            audio.load();
        }
    }

    fn start(&mut self, generation: u64, prefs: &RadioPrefs) {
        self.release();
        let source = self.shared.model.borrow().source().map(str::to_owned);
        let Some(source) = source else { return };
        let Ok(audio) = HtmlAudioElement::new() else {
            self.shared.model.borrow_mut().failed(generation);
            self.shared.notify();
            return;
        };
        audio.set_preload("none");
        audio.set_volume(f64::from(crate::radio::clamp_volume(prefs.volume)));
        audio.set_muted(prefs.muted);
        for event in ["error", "ended"] {
            let weak = Rc::downgrade(&self.shared);
            let callback = Closure::wrap(Box::new(move |_: Event| {
                if let Some(shared) = weak.upgrade() {
                    let changed = shared.model.borrow_mut().failed(generation);
                    if changed {
                        shared.notify();
                    }
                }
            }) as Box<dyn FnMut(Event)>);
            let _ =
                audio.add_event_listener_with_callback(event, callback.as_ref().unchecked_ref());
            self.listeners.push((event, callback));
        }
        audio.set_src(&source);
        // Call play() in the original click stack. Only observing its result
        // is deferred, preserving the browser's user activation for retries.
        let result = audio.play();
        self.audio = Some(audio);
        let weak = Rc::downgrade(&self.shared);
        spawn_local(async move {
            let result = match result {
                Ok(promise) => JsFuture::from(promise).await,
                Err(error) => Err(error),
            };
            let outcome = match result {
                Ok(_) => PlaybackStatus::Playing,
                Err(error)
                    if js_sys::Reflect::get(&error, &JsValue::from_str("name"))
                        .ok()
                        .and_then(|value| value.as_string())
                        .as_deref()
                        == Some("NotAllowedError") =>
                {
                    PlaybackStatus::Blocked
                }
                Err(_) => PlaybackStatus::Failed,
            };
            if let Some(shared) = weak.upgrade() {
                let changed = shared.model.borrow_mut().settled(generation, outcome);
                if changed {
                    shared.notify();
                }
            }
        });
    }

    fn apply(&mut self, action: PlaybackAction, prefs: &RadioPrefs) {
        if action != PlaybackAction::None {
            self.shared.notify();
        }
        match action {
            PlaybackAction::Start(generation) => self.start(generation, prefs),
            PlaybackAction::Stop => self.release(),
            PlaybackAction::None => {}
        }
        if let Some(audio) = &self.audio {
            audio.set_volume(f64::from(crate::radio::clamp_volume(prefs.volume)));
            audio.set_muted(prefs.muted);
        }
    }

    pub fn sync(&mut self, prefs: &RadioPrefs, url: Option<String>) {
        let action = self.shared.model.borrow_mut().reconcile(prefs.enabled, url);
        self.apply(action, prefs);
    }

    /// A direct user gesture, separate from passive preference/listing sync.
    pub fn retry(&mut self, prefs: &RadioPrefs) {
        let action = self.shared.model.borrow_mut().retry();
        self.apply(action, prefs);
    }
}

impl Default for RadioPlayer {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for RadioPlayer {
    fn drop(&mut self) {
        self.shared.model.borrow_mut().reconcile(false, None);
        self.release();
    }
}
