//! Radio now-playing state for the TUI: a terminal-free reducer plus
//! render-to-lines helpers, unit-tested without a backend.
//!
//! ## Data source
//!
//! The server keeps per-station now-playing in its presence registry and
//! pushes it over the typed **RADIO** family (`Family(9)`): a
//! [`RadioNowPlaying`](rabbithole_proto::radio::RadioNowPlaying) frame per
//! change and a [`RadioOff`](rabbithole_proto::radio::RadioOff) on sign-off.
//! `main::handle_frame` decodes those — `RadioNowPlaying` converts into a
//! [`RadioStatus`] via [`From`] and feeds [`RadioState::apply_radio_status`],
//! `RadioOff` calls [`RadioState::clear_station`] — everything below the
//! reducer (status segment, panel) is already wired.
//!
//! Listening itself is a **handoff**: the radio view derives a stream URL
//! from these station slugs and hands it to an external player — see the
//! `handoff` module; the TUI never decodes audio.

/// One station's now-playing, mirroring the server-side `RadioStatus`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RadioStatus {
    /// Station mount slug (e.g. "live").
    pub station: String,
    /// Current track title (or station name before the first track).
    pub title: String,
    /// Current track artist (may be empty).
    pub artist: String,
    /// The source name: a live DJ, or the automation label.
    pub dj: String,
    /// Listeners currently tuned in.
    pub listeners: usize,
    /// Whether a live DJ is sourcing the mount (vs. playlist automation).
    pub live: bool,
}

/// Client-side view of every station on the air, sorted by station slug.
#[derive(Debug, Default)]
pub struct RadioState {
    stations: Vec<RadioStatus>,
}

impl RadioState {
    /// Upsert a station's now-playing (the reducer). Keeps stations sorted
    /// by slug so renders are stable.
    pub fn apply_radio_status(&mut self, status: RadioStatus) {
        match self
            .stations
            .iter_mut()
            .find(|s| s.station == status.station)
        {
            Some(slot) => *slot = status,
            None => {
                self.stations.push(status);
                self.stations.sort_by(|a, b| a.station.cmp(&b.station));
            }
        }
    }

    /// Drop a station (its mount went off the air).
    pub fn clear_station(&mut self, station: &str) {
        self.stations.retain(|s| s.station != station);
    }

    pub fn stations(&self) -> &[RadioStatus] {
        &self.stations
    }

    /// The station the status bar features: a live DJ wins over automation;
    /// ties go to the first slug alphabetically.
    pub fn on_air(&self) -> Option<&RadioStatus> {
        self.stations
            .iter()
            .find(|s| s.live)
            .or_else(|| self.stations.first())
    }
}

impl From<rabbithole_proto::radio::RadioNowPlaying> for RadioStatus {
    fn from(np: rabbithole_proto::radio::RadioNowPlaying) -> Self {
        Self {
            station: np.station,
            title: np.title,
            artist: np.artist,
            dj: np.dj,
            listeners: np.listeners as usize,
            live: np.live,
        }
    }
}

/// Truncate to `max` characters, ending in `…` when anything was cut.
fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let keep = max.saturating_sub(1);
    let mut out: String = s.chars().take(keep).collect();
    out.push('…');
    out
}

/// `Title — Artist`, or just the title when the artist is empty.
fn track_line(status: &RadioStatus) -> String {
    if status.artist.is_empty() {
        status.title.clone()
    } else {
        format!("{} — {}", status.title, status.artist)
    }
}

/// The status-bar segment: `♪ live: Title — Artist · DJ Robin · 3 listening`
/// for the featured station, truncated to `max_width`. `None` when no station
/// is on the air (the caller renders its off-air placeholder).
pub fn status_segment(state: &RadioState, max_width: usize) -> Option<String> {
    let s = state.on_air()?;
    let mut seg = format!("♪ {}: {}", s.station, track_line(s));
    if s.live && !s.dj.is_empty() {
        seg.push_str(&format!(" · DJ {}", s.dj));
    }
    seg.push_str(&format!(" · {} listening", s.listeners));
    Some(truncate(&seg, max_width))
}

/// One rendered panel row: the text plus whether it belongs to a live station
/// (so the caller can style it with the accent color).
#[derive(Debug, PartialEq, Eq)]
pub struct PanelLine {
    pub text: String,
    pub live: bool,
}

/// The radio panel body: two lines per station (header + track), each
/// truncated to `width`. A placeholder line when nothing is on the air.
pub fn panel_lines(state: &RadioState, width: usize) -> Vec<PanelLine> {
    if state.stations().is_empty() {
        return vec![PanelLine {
            text: "(off the air)".into(),
            live: false,
        }];
    }
    let mut out = Vec::new();
    for s in state.stations() {
        let header = if s.live {
            format!("● {} LIVE · DJ {} · {}", s.station, s.dj, s.listeners)
        } else {
            format!("○ {} · {} · {}", s.station, s.dj, s.listeners)
        };
        out.push(PanelLine {
            text: truncate(&header, width),
            live: s.live,
        });
        out.push(PanelLine {
            text: truncate(&format!("  {}", track_line(s)), width),
            live: false,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn auto(station: &str, title: &str, artist: &str, listeners: usize) -> RadioStatus {
        RadioStatus {
            station: station.into(),
            title: title.into(),
            artist: artist.into(),
            dj: "rotation".into(),
            listeners,
            live: false,
        }
    }

    fn live(station: &str, title: &str, artist: &str, dj: &str, listeners: usize) -> RadioStatus {
        RadioStatus {
            station: station.into(),
            title: title.into(),
            artist: artist.into(),
            dj: dj.into(),
            listeners,
            live: true,
        }
    }

    #[test]
    fn reducer_upserts_and_sorts() {
        let mut state = RadioState::default();
        state.apply_radio_status(auto("zeta", "A", "B", 1));
        state.apply_radio_status(live("alpha", "C", "D", "Robin", 2));
        let slugs: Vec<&str> = state
            .stations()
            .iter()
            .map(|s| s.station.as_str())
            .collect();
        assert_eq!(slugs, ["alpha", "zeta"]);

        // Same station replaces in place (track rotated, listeners moved).
        state.apply_radio_status(auto("zeta", "Next Track", "B", 5));
        assert_eq!(state.stations().len(), 2);
        assert_eq!(state.stations()[1].title, "Next Track");
        assert_eq!(state.stations()[1].listeners, 5);
    }

    #[test]
    fn reducer_clears_station() {
        let mut state = RadioState::default();
        state.apply_radio_status(auto("live", "A", "B", 1));
        state.clear_station("live");
        assert!(state.stations().is_empty());
        assert!(state.on_air().is_none());
    }

    #[test]
    fn on_air_prefers_live_dj() {
        let mut state = RadioState::default();
        state.apply_radio_status(auto("ambient", "Drift", "Eno", 9));
        state.apply_radio_status(live("night", "Request Hour", "", "Robin", 3));
        assert_eq!(state.on_air().unwrap().station, "night");
        // Without any live DJ, the first slug wins.
        state.clear_station("night");
        assert_eq!(state.on_air().unwrap().station, "ambient");
    }

    #[test]
    fn radio_now_playing_frame_converts_to_status() {
        let np = rabbithole_proto::radio::RadioNowPlaying::new(
            "live",
            "Down the Hole",
            "The Lagomorphs",
            "Robin",
            12,
            true,
        );
        let status: RadioStatus = np.into();
        assert_eq!(
            status,
            live("live", "Down the Hole", "The Lagomorphs", "Robin", 12)
        );
        // A title with pipes is no longer a wire hazard (the format is typed).
        let np =
            rabbithole_proto::radio::RadioNowPlaying::new("live", "A|B|C", "", "auto", 0, false);
        let status: RadioStatus = np.into();
        assert_eq!(status.title, "A|B|C");
        assert!(!status.live && status.artist.is_empty());
    }

    #[test]
    fn status_segment_states() {
        let mut state = RadioState::default();
        assert_eq!(status_segment(&state, 80), None);

        // Playlist automation: no DJ credit, artist joined with an em dash.
        state.apply_radio_status(auto("live", "Warren Dawn", "The Lagomorphs", 4));
        assert_eq!(
            status_segment(&state, 80).unwrap(),
            "♪ live: Warren Dawn — The Lagomorphs · 4 listening"
        );

        // Live DJ takes over the same mount: DJ credit appears.
        state.apply_radio_status(live("live", "Request Hour", "", "Robin", 7));
        assert_eq!(
            status_segment(&state, 80).unwrap(),
            "♪ live: Request Hour · DJ Robin · 7 listening"
        );
    }

    #[test]
    fn status_segment_truncates_long_titles() {
        let mut state = RadioState::default();
        state.apply_radio_status(auto(
            "live",
            "An Extremely Long Track Title That Cannot Possibly Fit",
            "Somebody",
            1,
        ));
        let seg = status_segment(&state, 24).unwrap();
        assert_eq!(seg.chars().count(), 24);
        assert!(seg.ends_with('…'), "got: {seg}");
        assert!(seg.starts_with("♪ live: An Extremely"));
    }

    #[test]
    fn panel_lines_empty_live_and_playlist() {
        let mut state = RadioState::default();
        assert_eq!(panel_lines(&state, 40)[0].text, "(off the air)");

        state.apply_radio_status(auto("ambient", "Drift", "Eno", 2));
        state.apply_radio_status(live("live", "Request Hour", "", "Robin", 7));
        let lines = panel_lines(&state, 40);
        let texts: Vec<&str> = lines.iter().map(|l| l.text.as_str()).collect();
        assert_eq!(
            texts,
            [
                "○ ambient · rotation · 2",
                "  Drift — Eno",
                "● live LIVE · DJ Robin · 7",
                "  Request Hour",
            ]
        );
        // Only the live station's header carries the live styling flag.
        assert_eq!(
            lines.iter().map(|l| l.live).collect::<Vec<_>>(),
            [false, false, true, false]
        );
    }

    #[test]
    fn panel_lines_truncate_to_width() {
        let mut state = RadioState::default();
        state.apply_radio_status(auto(
            "live",
            "A Ridiculously Overlong Title For A Narrow Sidebar",
            "Verbose Artist Collective",
            3,
        ));
        for line in panel_lines(&state, 20) {
            assert!(line.text.chars().count() <= 20, "too wide: {}", line.text);
        }
        assert!(panel_lines(&state, 20)[1].text.ends_with('…'));
    }
}
