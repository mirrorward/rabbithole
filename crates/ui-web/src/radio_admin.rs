//! The words the Radio pane says about a station.
//!
//! The burrow answers with facts (`RadioStatus`): a slug, a content type, a
//! rotation size, a list of tracks it could not play. An operator wants
//! sentences — what is on, what it is sending, and what to do about the
//! tracks it left out. All pure, so the wording is tested rather than
//! looked at.

use rabbithole_proto::radio::{LeftOut, RadioStationStatus};

/// What a station is sending, for a person: not a MIME type.
pub fn sound_label(content_type: &str) -> &'static str {
    match content_type {
        "audio/mpeg" => "MP3",
        "audio/ogg" => "Ogg",
        "" => "nothing yet",
        _ => "something else",
    }
}

/// What a station is doing, in a sentence.
pub fn station_line(s: &RadioStationStatus) -> String {
    if s.live {
        return format!(
            "A DJ is live{}.",
            if s.title.is_empty() {
                String::new()
            } else {
                format!(" with \u{201c}{}\u{201d}", s.title)
            }
        );
    }
    let playing = if s.title.is_empty() {
        "Nothing playing".to_string()
    } else if s.artist.is_empty() {
        format!("Playing \u{201c}{}\u{201d}", s.title)
    } else {
        format!("Playing \u{201c}{}\u{201d} by {}", s.title, s.artist)
    };
    let tracks = match s.tracks {
        0 => "an empty rotation".to_string(),
        1 => "1 track".to_string(),
        n => format!("{n} tracks"),
    };
    format!(
        "{playing}, from {tracks}, as {}.",
        sound_label(&s.content_type)
    )
}

/// Who is hearing it.
pub fn listeners_line(listeners: u32) -> String {
    match listeners {
        0 => "Nobody listening".to_string(),
        1 => "1 listening".to_string(),
        n => format!("{n} listening"),
    }
}

/// What to make of the tracks a station left out, in a sentence. `None`
/// when it left nothing out — the pane says nothing rather than saying
/// "0 tracks left out".
pub fn left_out_line(left_out: &[LeftOut]) -> Option<String> {
    let n = left_out.len();
    if n == 0 {
        return None;
    }
    let head = if n == 1 {
        "1 track could not be played".to_string()
    } else {
        format!("{n} tracks could not be played")
    };
    Some(format!(
        "{head}. A station sends one kind of sound, so anything else in its \
         area is passed over."
    ))
}

/// Where a station's music comes from, for the pane's subtitle.
pub fn area_line(area: &str) -> String {
    if area.is_empty() {
        "No file area: this station is a live mount only.".to_string()
    } else {
        format!("Its music is the {area} area.")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn station() -> RadioStationStatus {
        RadioStationStatus::new("jukebox", "The Jukebox")
            .of_area("music", "audio/mpeg")
            .on_air("Down the Hole", "The Lagomorphs", 3, false)
            .with_rotation(42, Vec::new())
    }

    #[test]
    fn a_station_says_what_it_is_doing_in_a_sentence() {
        let s = station();
        assert_eq!(
            station_line(&s),
            "Playing \u{201c}Down the Hole\u{201d} by The Lagomorphs, from 42 tracks, as MP3."
        );
        assert_eq!(area_line(&s.area), "Its music is the music area.");
        assert_eq!(listeners_line(s.listeners), "3 listening");

        // A station that has not started says so rather than saying MP3.
        let quiet = RadioStationStatus::new("quiet", "Quiet").with_rotation(0, Vec::new());
        assert_eq!(
            station_line(&quiet),
            "Nothing playing, from an empty rotation, as nothing yet."
        );
        assert_eq!(
            area_line(&quiet.area),
            "No file area: this station is a live mount only."
        );
        assert_eq!(listeners_line(0), "Nobody listening");

        // An Ogg station says Ogg, and one track is singular.
        let ogg = RadioStationStatus::new("oggcast", "Oggcast")
            .of_area("music", "audio/ogg")
            .on_air("Burrow Song", "", 1, false)
            .with_rotation(1, Vec::new());
        assert_eq!(
            station_line(&ogg),
            "Playing \u{201c}Burrow Song\u{201d}, from 1 track, as Ogg."
        );
        assert_eq!(listeners_line(1), "1 listening");
    }

    #[test]
    fn a_live_dj_is_said_to_be_live_and_not_counted_as_a_rotation() {
        let mut s = station();
        s.live = true;
        assert_eq!(
            station_line(&s),
            "A DJ is live with \u{201c}Down the Hole\u{201d}."
        );
        s.title = String::new();
        assert_eq!(station_line(&s), "A DJ is live.");
    }

    #[test]
    fn what_was_left_out_is_only_mentioned_when_there_is_some() {
        assert_eq!(left_out_line(&[]), None);
        let one = vec![LeftOut::new("notes.txt", "not audio", 1)];
        assert!(left_out_line(&one)
            .unwrap()
            .starts_with("1 track could not"));
        let two = vec![
            LeftOut::new("a.txt", "not audio", 1),
            LeftOut::new("b.flac", "not what this station is sending", 2),
        ];
        assert!(left_out_line(&two)
            .unwrap()
            .starts_with("2 tracks could not"));
    }
}
