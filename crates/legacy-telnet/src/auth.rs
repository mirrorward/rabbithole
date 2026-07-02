//! The pluggable authenticator seam for the telnet shell.
//!
//! This crate deliberately does not depend on `server-core`: burrow adapts
//! its `AuthService` to [`TelnetAuth`] when it wires the telnet listener in
//! (a later Wave 6 slice), and tests plug in stubs. `async_trait` keeps the
//! trait dyn-compatible with `Send` futures so shells can be spawned per
//! connection.

/// Checks a username/password pair for the telnet login prompt.
#[async_trait::async_trait]
pub trait TelnetAuth: Send + Sync {
    /// Attempt a login. `Some(screen_name)` on success — the name the shell
    /// greets the caller with — or `None` to reject (indistinguishably for
    /// unknown users and bad passwords; don't leak which).
    async fn login(&self, username: &str, password: &str) -> Option<String>;
}
