//! Sending files from one burrow to another: the words for every way it can
//! go, and the queue row's view of a pull. DOM-free and host-tested; the
//! dialog is [`crate::send_view`].
//!
//! The person is on both burrows. The one the files are on (the **source**)
//! signs a permission naming the other (the **destination**), the app hands
//! it over, and the destination fetches the files itself over its federation
//! session with the source. So a refusal can come from either burrow, and
//! each sentence says which.

use rabbithole_proto::filelib::{pull_reason, pull_state, RemotePullStatus};
use rabbithole_proto::ErrorCode;

use crate::files::human_size;

fn quoted(name: &str) -> String {
    format!("\u{201c}{name}\u{201d}")
}

/// "3 files, 1.2 MB" / "1 file, 12 B".
pub fn amount(files: u32, bytes: u64) -> String {
    format!(
        "{files} file{}, {}",
        if files == 1 { "" } else { "s" },
        human_size(bytes.min(i64::MAX as u64) as i64)
    )
}

/// Why the source would not sign. `None`: the connection went first.
pub fn grant_refusal(code: Option<ErrorCode>, what: &str, source: &str, dest: &str) -> String {
    let what = quoted(what);
    match code {
        None => format!("The connection to {source} dropped. Try again."),
        Some(ErrorCode::Unsupported) => format!(
            "{source} does not let people send files to other burrows. Its operator can \
             turn that on under Federation & feeds."
        ),
        Some(ErrorCode::Unavailable) => format!(
            "{source} and {dest} are not peers, so files cannot go between them. Their \
             operators approve each other under Peers."
        ),
        Some(ErrorCode::Forbidden) => format!("You may not send {what} from {source}."),
        Some(ErrorCode::NotFound) => format!("{what} is empty, or no longer on {source}."),
        Some(ErrorCode::BadRequest) => {
            format!("{what} holds more than one send can carry: at most 1000 files.")
        }
        Some(other) => format!("{source} could not send {what} ({other:?})."),
    }
}

/// Why the destination would not take it. `None`: the connection went first.
pub fn pull_refusal(
    code: Option<ErrorCode>,
    what: &str,
    is_folder: bool,
    bytes: u64,
    source: &str,
    dest: &str,
) -> String {
    let what = quoted(what);
    match code {
        None => format!("The connection to {dest} dropped. Try again."),
        Some(ErrorCode::Unsupported) => format!(
            "{dest} does not take files sent from other burrows. Its operator can turn that \
             on under Federation & feeds."
        ),
        Some(ErrorCode::Unavailable) => {
            format!("{dest} is not connected to {source} right now, so it cannot fetch {what}.")
        }
        // A folder is recreated there, which takes the right to make folders,
        // and never inside a drop box.
        Some(ErrorCode::Forbidden) if is_folder => format!(
            "You may not make folders there on {dest}, or {dest} refuses something in \
             {what}. A folder cannot go into a drop box."
        ),
        Some(ErrorCode::Forbidden) => {
            format!("You may not put files in that folder on {dest}, or {dest} refuses {what}.")
        }
        Some(ErrorCode::NotFound) => format!("That folder on {dest} is not there any more."),
        Some(ErrorCode::TooLarge) => format!(
            "{what} ({}) is more than {dest} takes in one file, one send, or your space there.",
            human_size(bytes.min(i64::MAX as u64) as i64)
        ),
        Some(ErrorCode::SessionExpired) => {
            "The permission to send ran out before it was used. Try again.".to_string()
        }
        Some(ErrorCode::AlreadyExists) => "That send was already used. Try again.".to_string(),
        Some(ErrorCode::RateLimited) => format!(
            "You already have as many sends coming in to {dest} as it allows. Wait for one \
             to finish."
        ),
        Some(ErrorCode::BadRequest) => format!("{dest} could not read the permission. Try again."),
        Some(other) => format!("{dest} could not take {what} ({other:?})."),
    }
}

/// Why a pull that started stopped, for its queue row. The row sits under
/// the destination, so "here" is the destination.
pub fn stopped(reason: u8, source: &str) -> String {
    match reason {
        pull_reason::SOURCE_REFUSED => format!("{source} stopped sending."),
        pull_reason::SOURCE_UNREACHABLE => format!("The connection to {source} dropped."),
        pull_reason::TOO_LARGE => "A file was bigger than this burrow takes.".to_string(),
        pull_reason::OVER_QUOTA => "It would have gone over your space here.".to_string(),
        pull_reason::DENIED_CONTENT => "This burrow refuses something in it.".to_string(),
        pull_reason::VERIFY_FAILED => "A file arrived damaged.".to_string(),
        pull_reason::CANCELLED => crate::upload::CANCELLED.to_string(),
        pull_reason::STOPPED => "This burrow stopped taking sends.".to_string(),
        _ => "This burrow could not file it.".to_string(),
    }
}

/// The sentence for a toast when a pull ends.
pub fn ended(status: &RemotePullStatus, what: &str, dest: &str) -> String {
    let what = quoted(what);
    match status.state {
        pull_state::DONE if status.files_done == 0 => {
            format!(
                "Nothing of {what} reached {dest}: {} no longer had it.",
                status.source
            )
        }
        pull_state::DONE if status.missing > 0 => format!(
            "{what} reached {dest}, all but {} file{} {} no longer had.",
            status.missing,
            if status.missing == 1 { "" } else { "s" },
            status.source
        ),
        pull_state::DONE => format!("{what} reached {dest}."),
        _ if status.reason == pull_reason::CANCELLED => {
            format!("Stopped sending {what} to {dest}.")
        }
        _ => format!(
            "Sending {what} to {dest} stopped: {}",
            stopped(status.reason, &status.source)
        ),
    }
}

/// A folder picked on the destination: its area and the folders within.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Place {
    pub area: Option<String>,
    pub path: Vec<String>,
}

impl Place {
    /// The folder path as the burrow takes it, `None` at the area's root.
    pub fn folder(&self) -> Option<String> {
        (!self.path.is_empty()).then(|| self.path.join("/"))
    }

    /// Where it is, for the dialog's line: "Music / tapes".
    pub fn label(&self, area_title: &str) -> String {
        std::iter::once(area_title.to_string())
            .chain(self.path.iter().cloned())
            .collect::<Vec<_>>()
            .join(" / ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(state: u8, done: u32, missing: u32, reason: u8) -> RemotePullStatus {
        RemotePullStatus::new(
            1,
            state,
            done,
            3,
            10,
            30,
            reason,
            missing,
            "Scratch Warren",
            "inbox",
            "tapes",
        )
    }

    #[test]
    fn each_refusal_says_which_burrow_and_what_to_do() {
        let g = |c| grant_refusal(Some(c), "tapes", "Scratch", "Kevin\u{2019}s Burrow");
        assert!(g(ErrorCode::Unsupported).contains("Scratch does not let people send"));
        assert!(g(ErrorCode::Unavailable).contains("approve each other under Peers"));
        assert_eq!(
            g(ErrorCode::Forbidden),
            "You may not send \u{201c}tapes\u{201d} from Scratch."
        );
        assert!(grant_refusal(None, "tapes", "Scratch", "K").contains("connection to Scratch"));

        let p = |c| pull_refusal(Some(c), "tapes", false, 3 * 1024 * 1024, "Scratch", "Kevin");
        assert!(p(ErrorCode::Unsupported).starts_with("Kevin does not take files"));
        assert!(p(ErrorCode::Unavailable).contains("not connected to Scratch"));
        assert!(p(ErrorCode::TooLarge).contains("(3.0 MB) is more than Kevin takes"));
        assert!(p(ErrorCode::RateLimited).contains("as many sends coming in to Kevin"));
        assert!(p(ErrorCode::SessionExpired).contains("ran out"));
        assert!(pull_refusal(None, "tapes", false, 1, "S", "Kevin").contains("connection to Kevin"));
        assert!(
            pull_refusal(Some(ErrorCode::Forbidden), "tapes", true, 1, "S", "Kevin")
                .contains("make folders there on Kevin")
        );
        assert_eq!(
            stopped(pull_reason::STOPPED, "S"),
            "This burrow stopped taking sends."
        );
    }

    #[test]
    fn a_pull_ends_in_a_sentence_that_counts_what_was_missing() {
        assert_eq!(
            ended(&status(pull_state::DONE, 3, 0, 0), "tapes", "Kevin"),
            "\u{201c}tapes\u{201d} reached Kevin."
        );
        assert_eq!(
            ended(&status(pull_state::DONE, 2, 1, 0), "tapes", "Kevin"),
            "\u{201c}tapes\u{201d} reached Kevin, all but 1 file Scratch Warren no longer had."
        );
        assert!(
            ended(&status(pull_state::DONE, 0, 3, 0), "tapes", "Kevin").starts_with("Nothing of")
        );
        assert_eq!(
            ended(
                &status(pull_state::FAILED, 1, 0, pull_reason::CANCELLED),
                "tapes",
                "Kevin"
            ),
            "Stopped sending \u{201c}tapes\u{201d} to Kevin."
        );
        assert_eq!(
            ended(
                &status(pull_state::FAILED, 1, 0, pull_reason::OVER_QUOTA),
                "tapes",
                "Kevin"
            ),
            "Sending \u{201c}tapes\u{201d} to Kevin stopped: It would have gone over your space here."
        );
        assert_eq!(
            stopped(pull_reason::SOURCE_REFUSED, "S"),
            "S stopped sending."
        );
        assert_eq!(stopped(250, "S"), "This burrow could not file it.");
        assert_eq!(amount(1, 12), "1 file, 12 B");
        assert_eq!(amount(3, 1024 * 1024), "3 files, 1.0 MB");
    }

    #[test]
    fn a_place_is_an_area_and_the_folders_within() {
        let mut place = Place::default();
        assert_eq!(place.folder(), None);
        place.area = Some("music".into());
        place.path = vec!["tapes".into(), "b-sides".into()];
        assert_eq!(place.folder().as_deref(), Some("tapes/b-sides"));
        assert_eq!(place.label("Music"), "Music / tapes / b-sides");
    }
}
