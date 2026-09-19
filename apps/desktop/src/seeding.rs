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

use crate::swarm::ShareAs;

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
    /// Files being shared while they download (see [`Seeder::begin`]).
    partial: Vec<AdvertEntry>,
    /// What the burrow last granted, for pacing the re-announce.
    granted_ttl: u32,
}

impl Seeder {
    /// How many files are on offer, whole or still downloading (each once).
    pub fn files(&self) -> usize {
        let mut roots: Vec<[u8; 32]> = self
            .entries
            .iter()
            .chain(&self.partial)
            .map(|e| e.root)
            .collect();
        roots.sort_unstable();
        roots.dedup();
        roots.len()
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
        // No longer downloading, whatever happens next.
        let was_partial = self.partial.iter().any(|e| e.root == root);
        self.partial.retain(|e| e.root != root);
        // A download that shared as it went is already seeded whole from
        // this very file, from the proofs it kept: no second pass over it.
        if !self.seeds.holds_whole_at(&root, path) {
            if let Err(e) = self.seeds.add(root, path) {
                // Not seedable after all: take back what was offered in part.
                if was_partial && !self.seeds.holds_whole(&root) {
                    self.seeds.remove_partial(&root);
                    let _ = client.swarm_withdraw(vec![root]).await;
                }
                return Err(e.into());
            }
        }
        if !self.entries.iter().any(|e| e.root == root) {
            self.entries
                .push(AdvertEntry::new(root, size, name, mime_of(name)));
        }
        self.announce(client).await
    }

    /// Get ready to share a file while it downloads: the endpoint starts (if
    /// it has not) and its contact card is registered, and the returned
    /// [`ShareAs`] is handed to the download. The download advertises the
    /// file as held in part once its first verified unit lands, and shares
    /// each unit with its proof from then on. Nothing is advertised yet.
    pub async fn begin(
        &mut self,
        client: &mut Client,
        root: [u8; 32],
        size: u64,
        name: &str,
    ) -> Result<ShareAs, SeedError> {
        let own = self.endpoint(client).await?;
        let entry = AdvertEntry::new(root, size, name, mime_of(name));
        if !self.partial.iter().any(|e| e.root == root) {
            self.partial.push(entry.clone());
        }
        Ok(ShareAs {
            seeds: self.seeds.clone(),
            own,
            entry,
            ttl_secs: ADVERT_TTL_SECS,
        })
    }

    /// A download begun with [`Seeder::begin`] did not finish: stop offering
    /// what it had, unless the file is seeded whole from an earlier download.
    pub async fn abandon(&mut self, client: &mut Client, root: [u8; 32]) {
        self.partial.retain(|e| e.root != root);
        if self.seeds.holds_whole(&root) {
            return;
        }
        self.seeds.remove_partial(&root);
        let _ = client.swarm_withdraw(vec![root]).await;
    }

    /// This machine's peer endpoint (started on first use) with its contact
    /// card registered. Returns the certificate fingerprint it answers with,
    /// which this machine's own downloads leave out of their sources.
    async fn endpoint(&mut self, client: &mut Client) -> Result<[u8; 32], SeedError> {
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
        Ok(server.fingerprint.0)
    }

    /// The certificate fingerprint this machine's peer endpoint answers
    /// with, once it has started.
    pub fn fingerprint(&self) -> Option<[u8; 32]> {
        self.server.as_ref().map(|s| s.fingerprint.0)
    }

    /// (Re)register the contact card and (re)advertise everything on offer.
    /// Called after a share, on the re-announce timer, and after a reconnect
    /// (adverts die with the session). Files still downloading are
    /// advertised by their download.
    pub async fn announce(&mut self, client: &mut Client) -> Result<(), SeedError> {
        if self.entries.is_empty() {
            return Ok(());
        }
        self.endpoint(client).await?;
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
            let roots = self
                .entries
                .iter()
                .chain(&self.partial)
                .map(|e| e.root)
                .collect();
            let _ = client.swarm_withdraw(roots).await;
        }
        for e in self.entries.drain(..) {
            self.seeds.remove(&e.root);
        }
        for e in self.partial.drain(..) {
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
