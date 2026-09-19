//! Helping other people's downloads.
//!
//! A swarm is fast because the people who already have a file serve it. The
//! command-line client has always been able to (`rabbit swarm share`); the
//! app only ever took. This is the app's half: once the person opts in, a
//! file they download from a burrow is offered to other people **on that same
//! burrow**, from this machine, for as long as the app is open.
//!
//! What stays true, because the existing design already made it so:
//!
//! - **The burrow decides who may fetch.** A fetcher must present a
//!   capability the burrow signed for that exact file and that exact person
//!   ([`rabbithole_swarm::cap::CapToken`]); this peer verifies it against the
//!   burrow's key and serves nothing without it. Opting in shares files with
//!   people the burrow would have given them to anyway.
//! - **Only what was downloaded from that burrow, for that burrow.** A seeder
//!   is per burrow: its own store, its own peer endpoint bound to that
//!   burrow's key. A file from one burrow is never offered to another. The
//!   one reach past it is the burrow's own: when it sends a file to another
//!   burrow and its operator shares its swarm (`s2s_swarm_sources`), it may
//!   name this peer to the receiving burrow with a capability it signed for
//!   that burrow ([`rabbithole_swarm::cap::S2sCapToken`]), and that burrow
//!   fetches from here and sees this machine's address.
//! - **Nothing is read but the seeded files**, by content hash, and a fetcher
//!   verifies every block against that hash, so a file changed on disk since
//!   is simply refused by the fetcher.
//! - **It is soft state.** Adverts expire unless re-announced and die with
//!   the session. Closing the app stops all of it.

use std::path::Path;
use std::sync::Arc;

use rabbithole_core::{Client, ClientError};
use rabbithole_proto::swarm::AdvertEntry;
use rabbithole_swarm::peer::{PeerServer, SeedStore};

/// How long an advert is asked to live. The burrow may grant less; the
/// re-announce period follows what it granted.
pub const ADVERT_TTL_SECS: u32 = 600;

/// Why a file could not be shared. Never a reason to fail the download that
/// produced it: sharing is a courtesy on top.
#[derive(Debug)]
pub enum SeedError {
    /// The burrow refused (usually: this account may not advertise).
    Client(ClientError),
    /// The peer endpoint could not start, or the file could not be indexed.
    Peer(rabbithole_swarm::peer::PeerError),
}

impl std::fmt::Display for SeedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SeedError::Client(e) => write!(f, "the burrow would not list it: {e}"),
            SeedError::Peer(e) => write!(f, "this machine could not serve it: {e}"),
        }
    }
}
impl std::error::Error for SeedError {}

impl From<ClientError> for SeedError {
    fn from(e: ClientError) -> Self {
        SeedError::Client(e)
    }
}
impl From<rabbithole_swarm::peer::PeerError> for SeedError {
    fn from(e: rabbithole_swarm::peer::PeerError) -> Self {
        SeedError::Peer(e)
    }
}

/// One burrow's seeding: the files on offer there and the endpoint serving them.
#[derive(Default)]
pub struct Seeder {
    seeds: Arc<SeedStore>,
    server: Option<PeerServer>,
    entries: Vec<AdvertEntry>,
    /// What the burrow last granted, for pacing the re-announce.
    granted_ttl: u32,
}

impl Seeder {
    /// How many files are on offer.
    pub fn files(&self) -> usize {
        self.entries.len()
    }

    /// Seconds to wait before announcing again: two thirds of what the burrow
    /// granted, so an advert never lapses between announcements.
    pub fn reannounce_after(&self) -> u64 {
        reannounce_after(self.granted_ttl)
    }

    /// Offer `path` (whose content hash is `root`) to this burrow's swarm.
    ///
    /// Starts the peer endpoint on first use, bound to this burrow's key so
    /// only capabilities *it* signed are honoured, registers the contact card,
    /// and advertises. Idempotent for a root already on offer.
    pub async fn share(
        &mut self,
        client: &mut Client,
        root: [u8; 32],
        size: u64,
        name: &str,
        path: &Path,
    ) -> Result<(), SeedError> {
        self.seeds.add(root, path)?;
        if !self.entries.iter().any(|e| e.root == root) {
            self.entries
                .push(AdvertEntry::new(root, size, name, mime_of(name)));
        }
        self.announce(client).await
    }

    /// (Re)register the contact card and (re)advertise everything on offer.
    /// Called after a share, on the re-announce timer, and after a reconnect
    /// (adverts die with the session).
    pub async fn announce(&mut self, client: &mut Client) -> Result<(), SeedError> {
        if self.entries.is_empty() {
            return Ok(());
        }
        if self.server.is_none() {
            let server = PeerServer::start(
                "0.0.0.0:0".parse().expect("valid addr"),
                client.server.server_key,
                self.seeds.clone(),
            )
            .await?;
            self.server = Some(server);
        }
        let server = self.server.as_ref().expect("just started");
        client
            .swarm_contact(server.addr.port(), server.fingerprint.0)
            .await?;
        let ack = client
            .swarm_advertise(self.entries.clone(), ADVERT_TTL_SECS)
            .await?;
        self.granted_ttl = ack.ttl_secs;
        Ok(())
    }

    /// Stop offering anything here: withdraw the adverts and close the
    /// endpoint. Best effort on the withdraw; the endpoint closes regardless,
    /// and an advert nobody can dial expires on its own.
    pub async fn stop(&mut self, client: Option<&mut Client>) {
        if let Some(client) = client {
            let roots = self.entries.iter().map(|e| e.root).collect();
            let _ = client.swarm_withdraw(roots).await;
        }
        for e in self.entries.drain(..) {
            self.seeds.remove(&e.root);
        }
        self.server = None; // dropping it aborts the listener
    }
}

/// Two thirds of the granted lifetime, never less than ten seconds (a burrow
/// that grants a tiny TTL must not turn this into a busy loop) and, when
/// nothing has been granted yet, a sane default.
pub fn reannounce_after(granted_ttl_secs: u32) -> u64 {
    let ttl = if granted_ttl_secs == 0 {
        ADVERT_TTL_SECS
    } else {
        granted_ttl_secs
    };
    (u64::from(ttl) * 2 / 3).max(10)
}

/// A MIME guess from the extension, for the advert. Cosmetic: the content
/// hash is what identifies a file.
pub fn mime_of(name: &str) -> &'static str {
    match name
        .rsplit_once('.')
        .map(|(_, ext)| ext.to_ascii_lowercase())
        .as_deref()
    {
        Some("txt" | "md" | "nfo" | "diz") => "text/plain",
        Some("ans" | "asc") => "text/x-ansi",
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("mp3") => "audio/mpeg",
        Some("ogg" | "oga") => "audio/ogg",
        Some("flac") => "audio/flac",
        Some("zip") => "application/zip",
        Some("lha" | "lzh") => "application/x-lzh",
        Some("pdf") => "application/pdf",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_advert_is_renewed_before_it_lapses_and_never_in_a_busy_loop() {
        assert_eq!(reannounce_after(600), 400);
        assert_eq!(reannounce_after(90), 60);
        assert_eq!(reannounce_after(3), 10, "a tiny grant is not a busy loop");
        assert_eq!(reannounce_after(0), 400, "nothing granted yet: the default");
        assert!(reannounce_after(600) < 600);
    }

    #[test]
    fn the_mime_guess_is_by_extension_and_case_blind() {
        assert_eq!(mime_of("Down the Hole.MP3"), "audio/mpeg");
        assert_eq!(mime_of("lister.lha"), "application/x-lzh");
        assert_eq!(mime_of("README"), "application/octet-stream");
        assert_eq!(mime_of("weird.xyz"), "application/octet-stream");
    }
}
