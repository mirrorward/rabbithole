//! The pluggable data source behind the finger surface.
//!
//! The finger server never touches RabbitHole's stores directly; it asks a
//! [`FingerDirectory`] for who's online and for individual profiles. The
//! burrow adapts its persona/presence layers behind this trait (honoring
//! per-persona opt-outs before entries ever reach this crate), and tests
//! substitute stubs. All fields are owned `String`s — this is a host-side
//! interface, not a wire format.

use async_trait::async_trait;

/// One line in the who's-online listing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WhoEntry {
    /// The persona's public screen name.
    pub screen_name: String,
    /// Seconds since the persona's last activity.
    pub idle_secs: u64,
    /// Free-form location from the profile, if shared.
    pub location: Option<String>,
}

/// A member profile as rendered for a `user` query.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Profile {
    /// The persona's public screen name.
    pub screen_name: String,
    /// Real name, if the member chose to share one.
    pub real_name: Option<String>,
    /// Free-form location.
    pub location: Option<String>,
    /// Free-form interests.
    pub interests: Option<String>,
    /// Signature quote.
    pub quote: Option<String>,
    /// Pronouns.
    pub pronouns: Option<String>,
    /// The `.plan` text, rendered verbatim (post-sanitization) under the
    /// `Plan:` heading. `None` renders as `No Plan.`
    pub plan: Option<String>,
}

/// Async directory the finger server consults for every query.
///
/// Implementations decide what "online" means, how lookups match (exact,
/// case-insensitive, ...), and which personas are visible at all.
#[async_trait]
pub trait FingerDirectory: Send + Sync {
    /// Everyone currently online, in the order they should be listed.
    async fn who(&self) -> Vec<WhoEntry>;

    /// Look up a single user by the name given in the query, or `None` if
    /// there is no such (visible) user.
    async fn lookup(&self, user: &str) -> Option<Profile>;
}
