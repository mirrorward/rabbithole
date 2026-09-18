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
//! On arrival the client asks for the whole picture with
//! [`RadioStationsRequest`](rabbithole_proto::radio::RadioStationsRequest):
//! every station, its cover and recent tracks, and **where its audio is
//! served** ([`Tuning`]). The pushes keep that picture current.
//!
//! ## Listening
//!
//! Playback is plain HTTP audio straight off the burrow's own stream
//! listener: `<base>/<station>`. The base is the server's to say, never the
//! person's to type: the operator's `radio_public_base` when there is one,
//! else the host this client dialled plus the listener's port.
//! [`RadioState::stream_url`] holds the pure derivation + validation (scheme
//! allowlist `http`/`https`).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// How many tracks a station's recently-played list holds.
pub const RECENT_TRACKS: usize = 10;

/// A track a station played before the current one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Played {
    pub title: String,
    /// May be empty.
    pub artist: String,
    /// When it started, Unix milliseconds (0 when unknown).
    pub started_unix_ms: u64,
}

/// Where a burrow's audio is served, as the burrow itself says.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Tuning {
    /// The burrow this came from (its WebSocket endpoint), which supplies the
    /// host when the operator named no public base.
    pub endpoint: String,
    /// The operator's public base URL, or empty.
    pub stream_base: String,
    /// The stream listener's port; 0 when the burrow's radio is off.
    pub port: u16,
}

/// One station: its now-playing, and (once a listing has arrived) its name,
/// cover, history and whether there is audio to be had.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
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
    /// The station's display name; empty until a listing names it.
    pub name: String,
    pub description: String,
    /// Whether audio can be had right now. `None` until a listing says: a
    /// push carries now-playing but not this.
    pub streaming: Option<bool>,
    /// Cover art for the current track, as a blob id.
    pub cover: Option<[u8; 32]>,
    /// What played before, newest first, at most [`RECENT_TRACKS`].
    pub recent: Vec<Played>,
}

impl StationStatus {
    /// What to call the station: its name when it has one, else its slug.
    pub fn display_name(&self) -> &str {
        if self.name.trim().is_empty() {
            &self.station
        } else {
            &self.name
        }
    }
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
    /// Where the audio is, once the burrow has said.
    tuning: Option<Tuning>,
}

impl RadioState {
    /// Fold one [`RadioUpdate`] into the state (the reducer): `Playing`
    /// upserts the station, `Off` drops it. Unknown `Off` slugs are a no-op.
    ///
    /// A push carries now-playing and nothing else, so what a listing taught
    /// us about the station (name, description, whether it streams, history)
    /// is kept. When the *track* changed, the old one joins the history here,
    /// without waiting for the next listing, and the cover is forgotten: it
    /// belonged to the old track. Returns whether the track changed, which is
    /// the caller's cue to ask for a fresh listing (the new cover).
    pub fn apply_update(&mut self, update: RadioUpdate) -> bool {
        match update {
            RadioUpdate::Playing(mut status) => {
                let mut changed = true;
                if let Some(old) = self.stations.remove(&status.station) {
                    changed = old.title != status.title || old.artist != status.artist;
                    status.name = old.name;
                    status.description = old.description;
                    status.streaming = old.streaming;
                    status.recent = old.recent;
                    if changed {
                        if !old.title.trim().is_empty() {
                            status.recent.insert(
                                0,
                                Played {
                                    title: old.title,
                                    artist: old.artist,
                                    started_unix_ms: 0,
                                },
                            );
                            status.recent.truncate(RECENT_TRACKS);
                        }
                    } else {
                        status.cover = old.cover;
                    }
                }
                self.stations.insert(status.station.clone(), status);
                changed
            }
            RadioUpdate::Off(station) => {
                self.stations.remove(&station);
                false
            }
        }
    }

    /// A listing arrived: the burrow's whole picture replaces ours.
    pub fn apply_listing(&mut self, tuning: Tuning, stations: Vec<StationStatus>) {
        self.tuning = Some(tuning);
        self.stations = stations
            .into_iter()
            .map(|s| (s.station.clone(), s))
            .collect();
    }

    /// Where the audio is, once the burrow has said.
    pub fn tuning(&self) -> Option<&Tuning> {
        self.tuning.as_ref()
    }

    /// Whether the burrow told us its radio is off (a listing with port 0 and
    /// no public base). `false` while nothing has been heard yet.
    pub fn radio_is_off(&self) -> bool {
        self.tuning
            .as_ref()
            .is_some_and(|t| t.port == 0 && !base_is_valid(&t.stream_base))
    }

    /// The URL to tune `station` in at, or `None` when there is nothing to
    /// tune in to: no listing yet, the burrow's radio off, the station not
    /// streaming, or an address that isn't plain `http(s)`.
    pub fn stream_url(&self, station: &str) -> Option<String> {
        let tuning = self.tuning.as_ref()?;
        if self.get(station)?.streaming == Some(false) {
            return None;
        }
        let base = if base_is_valid(&tuning.stream_base) {
            tuning.stream_base.trim().trim_end_matches('/').to_string()
        } else {
            if tuning.port == 0 {
                return None;
            }
            format!("http://{}:{}", hostname_of(&tuning.endpoint)?, tuning.port)
        };
        join_mount(&base, station)
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

/// The stand-in sleeve for a track with no cover art: the burrow-hole mark,
/// which is a record if you squint, in a hue drawn from the track itself so
/// every track has its own face and the same track always has the same one.
/// An inline `style`, because the colours belong to the track, not the theme.
pub fn sleeve_style(title: &str, artist: &str) -> String {
    // FNV-1a over the track's identity.
    let mut hash: u32 = 0x811c_9dc5;
    for b in title.bytes().chain([0]).chain(artist.bytes()) {
        hash ^= b as u32;
        hash = hash.wrapping_mul(0x0100_0193);
    }
    let hue = hash % 360;
    format!(
        "background:radial-gradient(circle at 50% 50%,\
hsl({hue} 25% 10%) 0 6%,hsl({hue} 70% 62%) 6% 19%,hsl({hue} 30% 14%) 19% 21%,\
hsl({hue} 45% 28%) 21% 38%,hsl({hue} 30% 14%) 38% 40%,hsl({hue} 50% 40%) 40% 60%,\
hsl({hue} 30% 14%) 60% 62%,hsl({hue} 55% 22%) 62% 100%)"
    )
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
}

impl Default for RadioPrefs {
    fn default() -> Self {
        Self {
            enabled: false,
            volume: DEFAULT_VOLUME,
            muted: false,
            station: None,
        }
    }
}

impl RadioPrefs {
    /// Normalise the preferences into their valid domain: volume clamped to
    /// `0.0..=1.0` (NaN falls back to the default) and an empty station slug
    /// becomes `None`. (Profiles saved when the person had to type a stream
    /// address still carry a `base`; it is ignored on load.)
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

/// Whether `base` is a usable stream base: an `http://` or `https://` URL
/// (scheme allowlist) with a non-empty host part.
pub fn base_is_valid(base: &str) -> bool {
    let base = base.trim().trim_end_matches('/');
    base.strip_prefix("http://")
        .or_else(|| base.strip_prefix("https://"))
        .is_some_and(|rest| !rest.is_empty())
}

/// `<base>/<station>`, or `None` for an empty or spaced slug.
fn join_mount(base: &str, station: &str) -> Option<String> {
    let station = station.trim().trim_matches('/');
    if station.is_empty() || station.contains(char::is_whitespace) {
        return None;
    }
    Some(format!("{}/{station}", base.trim_end_matches('/')))
}

/// The host of a burrow endpoint, without scheme, port or path:
/// `wss://warren.example:4654/` is `warren.example`, and a bracketed IPv6
/// literal keeps its brackets (a URL needs them).
pub fn hostname_of(endpoint: &str) -> Option<String> {
    let rest = crate::connect::host(endpoint);
    let rest = rest.split('/').next().unwrap_or_default();
    let host = if let Some(end) = rest.find(']') {
        &rest[..=end]
    } else {
        rest.rsplit_once(':').map_or(rest, |(h, _)| h)
    };
    (!host.is_empty() && !host.contains(char::is_whitespace)).then(|| host.to_string())
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
            ..Default::default()
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
            ..Default::default()
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
        };
        assert_eq!(prefs_from_str(&prefs_to_str(&prefs)), Some(prefs));
    }

    #[test]
    fn prefs_reject_garbage_and_sanitise_on_load() {
        assert_eq!(prefs_from_str("nonsense"), None);
        assert_eq!(prefs_from_str(""), None);
        assert_eq!(prefs_from_str("{\"enabled\":true}"), None); // missing fields

        // Out-of-range volume clamps; an empty station normalises to None;
        // and a profile from when the person typed the stream address still
        // loads, its `base` ignored.
        let raw = "{\"enabled\":true,\"volume\":7.5,\"muted\":false,\
                   \"station\":\"  \",\"base\":\" http://h:8000 \"}";
        let prefs = prefs_from_str(raw).unwrap();
        assert_eq!(prefs.volume, 1.0);
        assert_eq!(prefs.station, None);
    }

    #[test]
    fn volume_clamps_into_unit_range() {
        assert_eq!(clamp_volume(-0.5), 0.0);
        assert_eq!(clamp_volume(0.5), 0.5);
        assert_eq!(clamp_volume(1.5), 1.0);
        assert_eq!(clamp_volume(f32::NAN), DEFAULT_VOLUME);
    }

    fn tuned(endpoint: &str, base: &str, port: u16, stations: Vec<StationStatus>) -> RadioState {
        let mut state = RadioState::default();
        state.apply_listing(
            Tuning {
                endpoint: endpoint.into(),
                stream_base: base.into(),
                port,
            },
            stations,
        );
        state
    }

    fn streaming(mut s: StationStatus) -> StationStatus {
        s.streaming = Some(true);
        s
    }

    #[test]
    fn a_track_with_no_cover_always_gets_the_same_sleeve() {
        let a = sleeve_style("Down the Hole", "The Lagomorphs");
        assert_eq!(a, sleeve_style("Down the Hole", "The Lagomorphs"));
        assert_ne!(a, sleeve_style("Carrot Cake", "The Lagomorphs"));
        // Title and artist are separate fields, not one run of letters.
        assert_ne!(sleeve_style("ab", "c"), sleeve_style("a", "bc"));
        assert!(a.starts_with("background:radial-gradient("));
        assert!(
            !a.contains('\n') && !a.contains("  "),
            "one tidy declaration"
        );
    }

    #[test]
    fn the_burrow_says_where_to_tune_in_and_nobody_types_it() {
        let live = || vec![streaming(live("live", "A", "B", "Robin", 1))];
        // No public base: the host this client dialled, the listener's port.
        let state = tuned("wss://warren.example:4654/", "", 8000, live());
        assert_eq!(
            state.stream_url("live"),
            Some("http://warren.example:8000/live".into())
        );
        let state = tuned("ws://[::1]:4654", "", 8123, live());
        assert_eq!(
            state.stream_url("live"),
            Some("http://[::1]:8123/live".into())
        );
        let state = tuned("localhost:4654", "", 8000, live());
        assert_eq!(
            state.stream_url("live"),
            Some("http://localhost:8000/live".into())
        );
        // The operator's public base wins, trailing slash and all.
        let state = tuned(
            "wss://warren.example",
            " https://radio.example.org/ ",
            8000,
            live(),
        );
        assert_eq!(
            state.stream_url("live"),
            Some("https://radio.example.org/live".into())
        );
        // A base that is not http(s) is not trusted; the derivation stands in.
        let state = tuned("ws://h:4654", "ftp://elsewhere", 8000, live());
        assert_eq!(state.stream_url("live"), Some("http://h:8000/live".into()));
    }

    #[test]
    fn nothing_to_tune_in_to_is_none_not_a_url_that_404s() {
        // Before any listing: not known yet.
        let mut state = RadioState::default();
        state.apply_update(RadioUpdate::Playing(live("live", "A", "B", "Robin", 1)));
        assert_eq!(state.stream_url("live"), None);
        assert!(!state.radio_is_off(), "unheard is not off");
        // The burrow's radio is off.
        let state = tuned(
            "ws://h:4654",
            "",
            0,
            vec![streaming(auto("live", "A", "B", 1))],
        );
        assert_eq!(state.stream_url("live"), None);
        assert!(state.radio_is_off());
        // A rotation with no encoder behind it has now-playing and no audio.
        let mut silent = auto("ambient", "Drift", "", 3);
        silent.streaming = Some(false);
        let state = tuned("ws://h:4654", "", 8000, vec![silent]);
        assert_eq!(state.stream_url("ambient"), None);
        // An unknown station, and a slug that is not one.
        assert_eq!(state.stream_url("nope"), None);
        assert_eq!(join_mount("http://h:8000", "a b"), None);
        assert_eq!(join_mount("http://h:8000", "  "), None);
        assert_eq!(
            join_mount("http://h:8000/", "/live/"),
            Some("http://h:8000/live".into())
        );
        assert!(!base_is_valid("ws://host:9000"));
        assert!(!base_is_valid("http://"));
        assert!(base_is_valid("https://host"));
    }

    #[test]
    fn a_push_keeps_what_the_listing_taught_and_moves_history_itself() {
        let mut listed = streaming(live("live", "One", "A", "Robin", 2));
        listed.name = "Warren FM".into();
        listed.cover = Some([7; 32]);
        listed.recent = vec![Played {
            title: "Zero".into(),
            artist: "A".into(),
            started_unix_ms: 5,
        }];
        let mut state = tuned("ws://h:4654", "", 8000, vec![listed]);

        // Same track, listeners moved: nothing about the track changes.
        assert!(!state.apply_update(RadioUpdate::Playing(live("live", "One", "A", "Robin", 9))));
        let s = state.get("live").unwrap();
        assert_eq!((s.listeners, s.display_name()), (9, "Warren FM"));
        assert_eq!(s.cover, Some([7; 32]));
        assert_eq!(s.recent.len(), 1);

        // A new track: the old one joins the history now, its cover goes, and
        // the caller is told to ask for a fresh listing.
        assert!(state.apply_update(RadioUpdate::Playing(live("live", "Two", "B", "Robin", 9))));
        let s = state.get("live").unwrap();
        assert_eq!(s.recent[0].title, "One");
        assert_eq!(s.recent[1].title, "Zero");
        assert_eq!(s.cover, None);
        assert_eq!(s.streaming, Some(true), "still streaming");
        assert_eq!(s.display_name(), "Warren FM");

        // History has an end.
        for n in 0..20 {
            state.apply_update(RadioUpdate::Playing(live(
                "live",
                &format!("T{n}"),
                "",
                "Robin",
                1,
            )));
        }
        assert_eq!(state.get("live").unwrap().recent.len(), RECENT_TRACKS);
        // A station nobody has named is called by its slug.
        assert_eq!(auto("ambient", "x", "", 0).display_name(), "ambient");
    }
}
