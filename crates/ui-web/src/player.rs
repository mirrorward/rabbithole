//! Browser radio audio, with observed playback state and explicit recovery.
//! Preferences express intent; a play promise or media error tells us what
//! actually happened. The host-tested reducer rejects obsolete answers.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use wasm_bindgen::{closure::Closure, JsCast, JsValue};
use wasm_bindgen_futures::{spawn_local, JsFuture};
use web_sys::{Event, HtmlAudioElement};

use crate::playback::{Playback, PlaybackAction, PlaybackStatus};
use crate::radio::RadioPrefs;

/// The envelope never saves a volume to restore: every frame uses current intent.
/// A weak animation task cannot retain the player after its owner is disposed.
#[derive(Default)]
struct Volume {
    audio: RefCell<Option<HtmlAudioElement>>,
    prefs: RefCell<RadioPrefs>,
    ducking: RefCell<crate::ducking::Ducking>,
    animating: Cell<bool>,
}

impl Volume {
    fn now() -> Option<f64> {
        // Wall-clock corrections must never prolong a quiet interval.
        Some(web_sys::window()?.performance()?.now())
    }

    fn apply(&self) -> bool {
        let mut envelope = self.ducking.borrow_mut();
        let gain = if let Some(now) = Self::now() {
            envelope.gain(now)
        } else {
            // No monotonic clock: retain ordinary volume rather than ducking.
            *envelope = crate::ducking::Ducking::default();
            1.0
        };
        if let Some(audio) = self.audio.borrow().as_ref() {
            let prefs = self.prefs.borrow();
            audio.set_volume(f64::from(crate::radio::clamp_volume(prefs.volume)) * gain);
            audio.set_muted(prefs.muted);
        }
        envelope.active()
    }

    fn sync(&self, prefs: &RadioPrefs) {
        *self.prefs.borrow_mut() = prefs.clone();
        if !prefs.ducking {
            if let Some(now) = Self::now() {
                self.ducking.borrow_mut().release(now);
            }
        }
        self.apply();
    }

    fn chime(self: &Rc<Self>, duration_ms: u32) {
        let Some(now) = Self::now() else { return };
        self.ducking.borrow_mut().chime(now, duration_ms);
        self.apply();
        if self.animating.replace(true) {
            return;
        }
        let weak = Rc::downgrade(self);
        spawn_local(async move {
            loop {
                gloo_timers::future::TimeoutFuture::new(16).await;
                let Some(volume) = weak.upgrade() else { break };
                if !volume.apply() {
                    volume.animating.set(false);
                    break;
                }
            }
        });
    }
}

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
    volume: Rc<Volume>,
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
            volume: Rc::new(Volume::default()),
        }
    }

    fn release(&mut self) {
        self.volume.audio.borrow_mut().take();
        *self.volume.ducking.borrow_mut() = crate::ducking::Ducking::default();
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
        *self.volume.audio.borrow_mut() = Some(audio.clone());
        self.volume.sync(prefs);
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
        self.volume.sync(prefs);
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

    /// Called only once a non-silent chime has actually started successfully.
    pub fn duck_for_chime(&self, duration_ms: u32) {
        let prefs = self.volume.prefs.borrow();
        if prefs.ducking
            && prefs.enabled
            && !prefs.muted
            && prefs.volume > 0.0
            && self.shared.model.borrow().status() == PlaybackStatus::Playing
        {
            drop(prefs);
            self.volume.chime(duration_ms);
        }
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
