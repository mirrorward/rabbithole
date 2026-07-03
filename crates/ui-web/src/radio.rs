//! Radio now-playing state, player preferences, and the pure stream-URL logic
//! for the web SPA — all DOM-free and host-tested. The wasm-only `<audio>`
//! element wrapper lives in [`crate::player`].
//!
//! ## Wire
//!
//! The server keeps per-station now-playing in its presence registry and
//! pushes it over the typed **RADIO** family (`Family(9)`): a
//! [`RadioNowPlaying`](rabbithole_proto::radio::RadioNowPlaying) frame per
//! change and a [`RadioOff`](rabbithole_proto::radio::RadioOff) on sign-off.
//! [`frame_to_notice_route`](crate::wire::frame_to_notice_route) decodes those
//! into a [`RadioUpdate`] the reducer ([`RadioState::apply_update`]) folds in —
//! everything below it (status segment, Radio view, player) is already wired.
//!
//! ## Listening
//!
//! Playback is plain HTTP audio straight off the Icecast **delivery** mount:
//! the stream URL is `<radio_base>/<station>`, where `radio_base` is the
//! user-supplied delivery address (e.g. `http://host:8000`) persisted with
//! the player preferences. [`stream_url`] holds the pure join + validation
//! (scheme allowlist `http`/`https`).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// One station's now-playing, decoded from the notice bridge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StationStatus {
    /// Station mount slug (e.g. "live").
    pub station: String,
    /// Current track title (or station name before the first track).
    pub title: String,
    /// Current track artist (may be empty).
    pub artist: String,
    /// The source name: a live DJ, or the automation label.
    pub dj: String,
    /// Listeners currently tuned in.
    pub listeners: u32,
    /// Whether a live DJ is sourcing the mount (vs. playlist automation).
    pub live: bool,
}

/// One decoded radio-bridge notice: a now-playing change or a sign-off.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RadioUpdate {
    /// A station's now-playing changed.
    Playing(StationStatus),
    /// A station went off the air.
    Off(String),
}

/// Client-side view of every station on the air, keyed by station slug (so
/// iteration order — and therefore rendering — is stable).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RadioState {
    stations: BTreeMap<String, StationStatus>,
}

impl RadioState {
    /// Fold one [`RadioUpdate`] into the state (the reducer): `Playing`
    /// upserts the station, `Off` drops it. Unknown `Off` slugs are a no-op.
    pub fn apply_update(&mut self, update: RadioUpdate) {
        match update {
            RadioUpdate::Playing(status) => {
                self.stations.insert(status.station.clone(), status);
            }
            RadioUpdate::Off(station) => {
                self.stations.remove(&station);
            }
        }
    }

    /// Every station on the air, ordered by slug.
    pub fn stations(&self) -> impl Iterator<Item = &StationStatus> {
        self.stations.values()
    }

    /// A station's status by slug.
    pub fn get(&self, station: &str) -> Option<&StationStatus> {
        self.stations.get(station)
    }

    /// Whether nothing is on the air.
    pub fn is_empty(&self) -> bool {
        self.stations.is_empty()
    }

    /// The station the status bar features: a live DJ wins over automation;
    /// ties go to the first slug alphabetically.
    pub fn on_air(&self) -> Option<&StationStatus> {
        self.stations
            .values()
            .find(|s| s.live)
            .or_else(|| self.stations.values().next())
    }
}

/// `Title — Artist`, or just the title when the artist is empty.
pub fn track_line(status: &StationStatus) -> String {
    if status.artist.is_empty() {
        status.title.clone()
    } else {
        format!("{} — {}", status.title, status.artist)
    }
}

/// The compact status-bar segment for the featured station:
/// `♪ live: Title — Artist · DJ Robin · 3 listening`. `None` when nothing is
/// on the air (the caller hides the segment).
pub fn status_segment(state: &RadioState) -> Option<String> {
    let s = state.on_air()?;
    let mut seg = format!("♪ {}: {}", s.station, track_line(s));
    if s.live && !s.dj.is_empty() {
        seg.push_str(&format!(" · DJ {}", s.dj));
    }
    seg.push_str(&format!(" · {} listening", s.listeners));
    Some(seg)
}

// ---------------------------------------------------------------------------
// Player preferences: per-user enable + volume + station + delivery address,
// persisted to localStorage behind the same wasm-gated storage seam the theme
// choice uses. All resolve/validation logic is pure and host-tested.
// ---------------------------------------------------------------------------

/// The volume a fresh profile starts with.
pub const DEFAULT_VOLUME: f32 = 0.8;

/// Per-user radio player preferences.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RadioPrefs {
    /// Whether the player is tuned in (audio playing) at all.
    pub enabled: bool,
    /// Playback volume in `0.0..=1.0`.
    pub volume: f32,
    /// Whether playback is muted (volume is remembered underneath).
    pub muted: bool,
    /// The selected station slug, if any.
    pub station: Option<String>,
    /// The Icecast **delivery** base address streams are joined onto, e.g.
    /// `http://host:8000`. Empty until the user sets it.
    pub base: String,
}

impl Default for RadioPrefs {
    fn default() -> Self {
        Self {
            enabled: false,
            volume: DEFAULT_VOLUME,
            muted: false,
            station: None,
            base: String::new(),
        }
    }
}

impl RadioPrefs {
    /// Normalise the preferences into their valid domain: volume clamped to
    /// `0.0..=1.0` (NaN falls back to the default), an empty station slug
    /// becomes `None`, and the base address is trimmed.
    pub fn sanitized(mut self) -> Self {
        self.volume = clamp_volume(self.volume);
        if let Some(s) = &self.station {
            let s = s.trim();
            self.station = if s.is_empty() {
                None
            } else {
                Some(s.to_string())
            };
        }
        self.base = self.base.trim().to_string();
        self
    }
}

/// Clamp a volume into `0.0..=1.0`; NaN falls back to [`DEFAULT_VOLUME`].
pub fn clamp_volume(volume: f32) -> f32 {
    if volume.is_nan() {
        DEFAULT_VOLUME
    } else {
        volume.clamp(0.0, 1.0)
    }
}

/// Serialise preferences for persistence (JSON).
pub fn prefs_to_str(prefs: &RadioPrefs) -> String {
    serde_json::to_string(prefs).unwrap_or_default()
}

/// Parse persisted preferences; garbage yields `None`, and anything that
/// parses is [sanitised](RadioPrefs::sanitized) into its valid domain.
pub fn prefs_from_str(raw: &str) -> Option<RadioPrefs> {
    let prefs: RadioPrefs = serde_json::from_str(raw).ok()?;
    Some(prefs.sanitized())
}

// ---------------------------------------------------------------------------
// Stream-URL derivation: pure join + validation for the audio player.
// ---------------------------------------------------------------------------

/// Whether `base` is a usable Icecast delivery address: an `http://` or
/// `https://` URL (scheme allowlist) with a non-empty host part.
pub fn base_is_valid(base: &str) -> bool {
    let base = base.trim().trim_end_matches('/');
    base.strip_prefix("http://")
        .or_else(|| base.strip_prefix("https://"))
        .is_some_and(|rest| !rest.is_empty())
}

/// Join the delivery `base` and a station slug into the stream URL the
/// player tunes to: `<base>/<station>`. Returns `None` when the base is
/// invalid (see [`base_is_valid`]) or the station slug is empty/whitespace.
pub fn stream_url(base: &str, station: &str) -> Option<String> {
    if !base_is_valid(base) {
        return None;
    }
    let base = base.trim().trim_end_matches('/');
    let station = station.trim().trim_matches('/');
    if station.is_empty() || station.contains(char::is_whitespace) {
        return None;
    }
    Some(format!("{base}/{station}"))
}

/// Browser-side preference persistence (`wasm32` only): the untestable DOM
/// edge over the pure `prefs_to_str`/`prefs_from_str` core above.
#[cfg(target_arch = "wasm32")]
pub mod storage {
    use super::{prefs_from_str, prefs_to_str, RadioPrefs};

    /// `localStorage` key the radio preferences are stored under.
    const KEY: &str = "rh-radio";

    /// The persisted preferences, if any.
    pub fn load_prefs() -> Option<RadioPrefs> {
        let storage = web_sys::window()?.local_storage().ok()??;
        let raw = storage.get_item(KEY).ok()??;
        prefs_from_str(&raw)
    }

    /// Persist the preferences (best-effort; storage may be unavailable).
    pub fn save_prefs(prefs: &RadioPrefs) {
        if let Some(Ok(Some(storage))) = web_sys::window().map(|w| w.local_storage()) {
            let _ = storage.set_item(KEY, &prefs_to_str(prefs));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn auto(station: &str, title: &str, artist: &str, listeners: u32) -> StationStatus {
        StationStatus {
            station: station.into(),
            title: title.into(),
            artist: artist.into(),
            dj: "rotation".into(),
            listeners,
            live: false,
        }
    }

    fn live(station: &str, title: &str, artist: &str, dj: &str, listeners: u32) -> StationStatus {
        StationStatus {
            station: station.into(),
            title: title.into(),
            artist: artist.into(),
            dj: dj.into(),
            listeners,
            live: true,
        }
    }

    #[test]
    fn reducer_upserts_keyed_by_slug() {
        let mut state = RadioState::default();
        state.apply_update(RadioUpdate::Playing(auto("zeta", "A", "B", 1)));
        state.apply_update(RadioUpdate::Playing(live("alpha", "C", "D", "Robin", 2)));
        let slugs: Vec<&str> = state.stations().map(|s| s.station.as_str()).collect();
        assert_eq!(slugs, ["alpha", "zeta"]);

        // Same station replaces in place (track rotated, listeners moved).
        state.apply_update(RadioUpdate::Playing(auto("zeta", "Next Track", "B", 5)));
        assert_eq!(state.stations().count(), 2);
        let zeta = state.get("zeta").unwrap();
        assert_eq!(zeta.title, "Next Track");
        assert_eq!(zeta.listeners, 5);
    }

    #[test]
    fn reducer_off_clears_and_unknown_off_is_noop() {
        let mut state = RadioState::default();
        state.apply_update(RadioUpdate::Playing(auto("live", "A", "B", 1)));
        state.apply_update(RadioUpdate::Off("nobody".into()));
        assert_eq!(state.stations().count(), 1);
        state.apply_update(RadioUpdate::Off("live".into()));
        assert!(state.is_empty());
        assert!(state.on_air().is_none());
    }

    #[test]
    fn on_air_prefers_a_live_dj() {
        let mut state = RadioState::default();
        state.apply_update(RadioUpdate::Playing(auto("ambient", "Drift", "Eno", 9)));
        state.apply_update(RadioUpdate::Playing(live(
            "night",
            "Request Hour",
            "",
            "Robin",
            3,
        )));
        assert_eq!(state.on_air().unwrap().station, "night");
        // Without any live DJ, the first slug wins.
        state.apply_update(RadioUpdate::Off("night".into()));
        assert_eq!(state.on_air().unwrap().station, "ambient");
    }

    #[test]
    fn status_segment_states() {
        let mut state = RadioState::default();
        assert_eq!(status_segment(&state), None);

        // Playlist automation: no DJ credit, artist joined with an em dash.
        state.apply_update(RadioUpdate::Playing(auto(
            "live",
            "Warren Dawn",
            "The Lagomorphs",
            4,
        )));
        assert_eq!(
            status_segment(&state).unwrap(),
            "♪ live: Warren Dawn — The Lagomorphs · 4 listening"
        );

        // A live DJ takes over the same mount: the DJ credit appears.
        state.apply_update(RadioUpdate::Playing(live(
            "live",
            "Request Hour",
            "",
            "Robin",
            7,
        )));
        assert_eq!(
            status_segment(&state).unwrap(),
            "♪ live: Request Hour · DJ Robin · 7 listening"
        );
    }

    #[test]
    fn prefs_roundtrip_through_persistence() {
        let prefs = RadioPrefs {
            enabled: true,
            volume: 0.35,
            muted: true,
            station: Some("live".into()),
            base: "http://warren.example:8000".into(),
        };
        assert_eq!(prefs_from_str(&prefs_to_str(&prefs)), Some(prefs));
    }

    #[test]
    fn prefs_reject_garbage_and_sanitise_on_load() {
        assert_eq!(prefs_from_str("nonsense"), None);
        assert_eq!(prefs_from_str(""), None);
        assert_eq!(prefs_from_str("{\"enabled\":true}"), None); // missing fields

        // Out-of-range volume clamps; an empty station normalises to None;
        // the base is trimmed.
        let raw = "{\"enabled\":true,\"volume\":7.5,\"muted\":false,\
                   \"station\":\"  \",\"base\":\" http://h:8000 \"}";
        let prefs = prefs_from_str(raw).unwrap();
        assert_eq!(prefs.volume, 1.0);
        assert_eq!(prefs.station, None);
        assert_eq!(prefs.base, "http://h:8000");
    }

    #[test]
    fn volume_clamps_into_unit_range() {
        assert_eq!(clamp_volume(-0.5), 0.0);
        assert_eq!(clamp_volume(0.5), 0.5);
        assert_eq!(clamp_volume(1.5), 1.0);
        assert_eq!(clamp_volume(f32::NAN), DEFAULT_VOLUME);
    }

    #[test]
    fn stream_url_joins_base_and_mount() {
        assert_eq!(
            stream_url("http://host:8000", "live"),
            Some("http://host:8000/live".into())
        );
        // Trailing slashes and padding collapse.
        assert_eq!(
            stream_url(" http://host:8000/ ", "/live/"),
            Some("http://host:8000/live".into())
        );
        assert_eq!(
            stream_url("https://radio.example", "ambient"),
            Some("https://radio.example/ambient".into())
        );
    }

    #[test]
    fn stream_url_enforces_the_scheme_allowlist() {
        assert_eq!(stream_url("ftp://host:8000", "live"), None);
        assert_eq!(stream_url("host:8000", "live"), None);
        assert_eq!(stream_url("http://", "live"), None);
        assert_eq!(stream_url("", "live"), None);
        assert!(!base_is_valid("ws://host:9000"));
        assert!(base_is_valid("http://host:8000"));
        assert!(base_is_valid("https://host"));
    }

    #[test]
    fn stream_url_rejects_bad_stations() {
        assert_eq!(stream_url("http://host:8000", ""), None);
        assert_eq!(stream_url("http://host:8000", "  "), None);
        assert_eq!(stream_url("http://host:8000", "a b"), None);
    }
}
