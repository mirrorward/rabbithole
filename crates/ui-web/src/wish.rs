//! The words the Wishing Well says.
//!
//! A burrow answers with numbers: a kind, a status, a vote count, who
//! claimed it. A person wants to read what somebody asked for, whether
//! anybody has taken it on, and what came of it. All pure, so the wording
//! is tested rather than looked at.

use rabbithole_proto::wish::WishView;

/// What somebody is asking for. The wire says 0..3; these are the words.
pub const KINDS: [(u8, &str); 4] = [
    (0, "File"),
    (1, "Board"),
    (2, "Feature"),
    (3, "Something else"),
];

/// Where a wish has got to.
pub mod status {
    /// Nobody has taken it on.
    pub const OPEN: u8 = 0;
    /// Somebody is doing it.
    pub const CLAIMED: u8 = 1;
    /// Done, and what came of it is on the wish.
    pub const FULFILLED: u8 = 2;
    /// Turned down, or withdrawn by whoever asked.
    pub const DECLINED: u8 = 3;
}

/// What kind of thing this is, for a person.
pub fn kind_label(kind: u8) -> &'static str {
    KINDS
        .iter()
        .find(|(k, _)| *k == kind)
        .map(|(_, label)| *label)
        .unwrap_or("Something else")
}

/// Where it has got to, for a person.
pub fn status_label(status: u8) -> &'static str {
    match status {
        status::OPEN => "Open",
        status::CLAIMED => "Being done",
        status::FULFILLED => "Granted",
        status::DECLINED => "Turned down",
        _ => "Unknown",
    }
}

/// What has become of a wish, in a sentence: who asked, who took it on,
/// and what came of it.
pub fn wish_line(wish: &WishView) -> String {
    let who = if wish.requester.is_empty() {
        "Somebody".to_string()
    } else {
        wish.requester.clone()
    };
    match (wish.status, wish.claimed_by.as_deref()) {
        (status::FULFILLED, Some(by)) => format!("{who} asked; {by} granted it."),
        (status::FULFILLED, None) => format!("{who} asked, and it was granted."),
        (status::CLAIMED, Some(by)) => format!("{who} asked; {by} is doing it."),
        (status::CLAIMED, None) => format!("{who} asked, and somebody is doing it."),
        (status::DECLINED, _) => format!("{who} asked; it will not be done."),
        _ => format!("{who} asked."),
    }
}

/// How many have wished for it, said out loud.
///
/// Whoever asked does not vote for their own wish, so nought means nobody
/// has joined it yet rather than that nobody wants it.
pub fn votes_line(votes: u64) -> String {
    match votes {
        0 => "Nobody else has wished for it yet".to_string(),
        1 => "1 other wish for it".to_string(),
        n => format!("{n} others wish for it"),
    }
}

/// The order a well is read in: what nobody has taken on first, most
/// wished-for at the top, and what is finished or turned down at the
/// bottom. Ties break by id so the order never wobbles.
pub fn in_reading_order(mut wishes: Vec<WishView>) -> Vec<WishView> {
    wishes.sort_by_key(|w| {
        let settled = matches!(w.status, status::FULFILLED | status::DECLINED);
        (settled, std::cmp::Reverse(w.votes), std::cmp::Reverse(w.id))
    });
    wishes
}

/// Whether this session may take a wish on, finish it or turn it down: a
/// person who is signed in and not a guest, since the burrow refuses a
/// guest either way and a button that cannot work should not be offered.
pub fn may_work_on(is_guest: bool, signed_in: bool) -> bool {
    signed_in && !is_guest
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wish(id: i64, status: u8, votes: u64) -> WishView {
        WishView::new(id, 0, "A thing", "", "alice", status, None, None, votes, 0)
    }

    #[test]
    fn a_wish_says_who_asked_and_what_came_of_it() {
        let mut w = wish(1, status::OPEN, 3);
        assert_eq!(wish_line(&w), "alice asked.");
        w.status = status::CLAIMED;
        w.claimed_by = Some("bob".into());
        assert_eq!(wish_line(&w), "alice asked; bob is doing it.");
        w.status = status::FULFILLED;
        assert_eq!(wish_line(&w), "alice asked; bob granted it.");
        w.status = status::DECLINED;
        assert_eq!(wish_line(&w), "alice asked; it will not be done.");
        w.requester = String::new();
        assert_eq!(wish_line(&w), "Somebody asked; it will not be done.");
    }

    #[test]
    fn the_numbers_are_said_in_words() {
        assert_eq!(kind_label(0), "File");
        assert_eq!(kind_label(1), "Board");
        assert_eq!(kind_label(2), "Feature");
        assert_eq!(kind_label(9), "Something else");
        assert_eq!(status_label(status::OPEN), "Open");
        assert_eq!(status_label(status::CLAIMED), "Being done");
        assert_eq!(status_label(status::FULFILLED), "Granted");
        assert_eq!(status_label(status::DECLINED), "Turned down");
        assert_eq!(votes_line(0), "Nobody else has wished for it yet");
        assert_eq!(votes_line(1), "1 other wish for it");
        assert_eq!(votes_line(12), "12 others wish for it");
    }

    #[test]
    fn a_well_is_read_with_the_asking_at_the_top() {
        let order = in_reading_order(vec![
            wish(1, status::FULFILLED, 99),
            wish(2, status::OPEN, 2),
            wish(3, status::CLAIMED, 5),
            wish(4, status::OPEN, 5),
            wish(5, status::DECLINED, 50),
        ]);
        let ids: Vec<i64> = order.iter().map(|w| w.id).collect();
        // Not settled first, most wished-for at the top; a tie goes to the
        // newer wish, and what is over is at the bottom in the same order.
        assert_eq!(ids, vec![4, 3, 2, 1, 5]);
        // And it is stable: reading it again does not move anything.
        let again = in_reading_order(order.clone());
        assert_eq!(
            again.iter().map(|w| w.id).collect::<Vec<_>>(),
            ids,
            "the order must not wobble"
        );
    }

    #[test]
    fn a_guest_is_not_offered_what_a_burrow_would_refuse() {
        assert!(may_work_on(false, true));
        assert!(!may_work_on(true, true), "a guest may not");
        assert!(!may_work_on(false, false), "nor anybody signed out");
    }
}
