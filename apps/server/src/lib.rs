//! Burrow as a library: everything `main.rs` does, callable from tests.

#![forbid(unsafe_code)]

pub mod admin_store;
pub mod announce;
pub mod backup;
mod chat_notice;
pub mod ctl;
pub mod doors;
pub mod fed_catalog;
pub mod fed_flood;
pub mod federation;
pub mod ftn;
pub mod handlers10;
pub mod handlers11;
pub mod handlers12;
pub mod handlers13;
pub mod handlers14;
pub mod handlers15;
pub mod handlers16;
pub mod handlers2;
pub mod handlers3;
pub mod handlers4;
pub mod handlers5;
pub mod handlers6;
pub mod handlers7;
pub mod handlers8;
pub mod handlers9;
pub mod hotline;
pub mod http;
pub mod identity_store;
pub mod legacy;
pub mod nntp;
pub mod nntp_feed;
pub mod portmap;
pub mod proved;
pub mod qwk;
pub mod radio;
pub mod s2s;
pub mod session;
pub mod stats;
pub mod surfaces;
pub mod syndication;
pub mod telnet;
pub mod upload_gate;
pub mod well_known;
pub mod zmodem;

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use anyhow::Result;
use rabbithole_blobs::BlobStore;
use rabbithole_net::quic::QuicListener;
use rabbithole_net::tls::CertFingerprint;
use rabbithole_net::ws::{validate_allowed_origins, WsListener};
use rabbithole_net::Listener;
use rabbithole_server_core::ratelimit::{self, Decision, LimitKey, Policy, Scope};
use rabbithole_server_core::{
    AuthService, BoardService, ChatService, ClassCache, DedupStore, EventBus, FileService,
    LiveConfig, ModerationService, PeerRegistry, PermissionEvaluator, PresenceRegistry, PushLog,
    RateLimiter, RegistrationMode, ServerConfig, ServerEvent, SwarmCatalog,
};
use rabbithole_store_server::SqlitePool;

/// Everything a session or ctl handler needs, shared across tasks.
pub struct Shared {
    pub config: LiveConfig,
    pub bus: EventBus,
    pub pool: SqlitePool,
    pub auth: AuthService,
    pub perms: PermissionEvaluator,
    pub presence: PresenceRegistry,
    pub chat: ChatService,
    pub boards: BoardService,
    pub files: FileService,
    pub pushlog: PushLog,
    pub classes: ClassCache,
    /// (sender_account, recipient_account) pairs already auto-responded
    /// this away period (cleared when the recipient comes back online).
    pub auto_responded: std::sync::Mutex<std::collections::HashSet<(i64, i64)>>,
    pub blobs: std::sync::Arc<BlobStore>,
    pub server_key: [u8; 32],
    /// Ed25519 signing seed for theme bundles and (later) federation.
    pub server_signing_seed: [u8; 32],
    pub fingerprint_hex: String,
    /// Where the QUIC client listener is bound: its port goes into pull
    /// grants for burrows that are not federation peers.
    pub quic_bound: SocketAddr,
    /// The shared dupe/seen gate — prevents reprocessing and rebroadcast
    /// loops once federation (W9) and syndication (W10) come online.
    pub dedup: DedupStore,
    /// Live bulk-transfer tickets (Wave 4.2).
    pub transfers: handlers9::TransferRegistry,
    /// Pulls between burrows: live federation links, pulls running, spent
    /// grants ([`s2s`]).
    pub s2s: s2s::S2sState,
    /// TTL'd who-has-what soft state for the Warren (Wave 5).
    pub swarm: SwarmCatalog,
    /// Radio station directory + live ICY mount fan-out (Wave 11.4).
    pub radio: radio::Stations,
    /// Connected Hotline clients for IM routing + user-list icons (Wave 7.3).
    pub hotline: hotline::Hub,
    /// Door-game host: registry, node pool, working root (Wave 6.x).
    pub doors: doors::DoorService,
    /// Known/approved/pending S2S federation peers + their state (Wave 9).
    pub peers: PeerRegistry,
    /// Local signed file-catalog + verified peer catalogs (Wave 9.x).
    pub catalogs: fed_catalog::CatalogState,
    /// Board-event flood-fill shared state: the pinned origin-key registry
    /// (Wave 9). Per-edge subscription/seen state lives in the session tasks.
    pub fed_flood: fed_flood::FloodState,
    /// Shared token-bucket rate limiter, per IP / account / endpoint class
    /// (Wave 13). Policies are resolved live from config on every check.
    pub ratelimit: RateLimiter,
    /// The moderation suite: report queue, quarantine set, hash-deny list
    /// (Wave 13). Quarantine/deny lookups are cheap in-memory mirrors.
    pub moderation: ModerationService,
    /// Interrupted telnet ZMODEM uploads parked for resume (Wave 6.x),
    /// keyed per (account, area, folder, name) — the HTXF partial-upload
    /// discipline.
    pub zpartials: zmodem::Partials,
    /// Live syndication/legacy-gateway activity counters (Wave 10),
    /// surfaced over the admin family and `ctl gateway-stats`.
    pub stats: stats::GatewayStats,
    /// The optional network surfaces, started and stopped while the burrow
    /// runs ([`surfaces::reconcile`]).
    pub surfaces: surfaces::Surfaces,
    next_session: AtomicU64,
}

impl Shared {
    pub fn next_session_id(&self) -> u64 {
        self.next_session.fetch_add(1, Ordering::Relaxed)
    }

    /// The server's origin id for `persona@origin` event authorship. Federated
    /// servers use the immutable restart-only `federation_origin`; legacy
    /// non-federated configs retain their display-name-derived local id.
    pub fn origin_name(&self) -> String {
        effective_origin(&self.config.read())
    }

    /// Parse the configured registration mode (bad values read as closed —
    /// fail safe).
    pub fn registration_mode(&self) -> RegistrationMode {
        RegistrationMode::parse(&self.config.read().registration_mode)
            .unwrap_or(RegistrationMode::Closed)
    }

    /// Consume one rate-limit token from `class` for `scope`; `true` =
    /// allowed. Always allows when limiting is disabled globally
    /// (`ratelimit_enabled=false`) or per class (rate knob 0). Refusals are
    /// audit-logged **sparsely** — the limiter flags at most one refusal per
    /// key per minute — so an abusive flood cannot flood the audit log too.
    pub fn rate_allow(&self, scope: Scope, class: &'static str) -> bool {
        !self.rate_decision(scope, class, true).is_limited()
    }

    /// Non-consuming probe: would a request on `class` be allowed right now?
    /// Used to gate an expensive attempt (a login) whose *failures* are what
    /// consume tokens — a success never spends from the budget.
    pub fn rate_probe(&self, scope: Scope, class: &'static str) -> bool {
        !self.rate_decision(scope, class, false).is_limited()
    }

    fn rate_decision(&self, scope: Scope, class: &'static str, consume: bool) -> Decision {
        let cfg = self.config.read();
        if !cfg.ratelimit_enabled {
            return Decision::Allowed;
        }
        let Some(policy) = Policy::for_class(&cfg, class) else {
            return Decision::Allowed; // rate knob 0: class disabled
        };
        drop(cfg);
        let key = LimitKey { scope, class };
        let now = ratelimit::now_ms();
        let decision = if consume {
            self.ratelimit.check_with(key, policy, now)
        } else {
            self.ratelimit.peek_with(key, policy, now)
        };
        if let Decision::Limited { audit: true, .. } = decision {
            let pool = self.pool.clone();
            let detail = format!("class={class} {scope}");
            tokio::spawn(async move {
                use rabbithole_store_server::repo::AuditRepo;
                let _ = AuditRepo(&pool)
                    .record("server", "rate-limited", &detail)
                    .await;
            });
        }
        decision
    }
}

/// A running burrow: bound addresses plus its shared state (tests reach in
/// through `shared`; `main` mostly just waits).
pub struct Burrow {
    pub shared: Arc<Shared>,
    pub quic_addr: SocketAddr,
    pub ws_addr: SocketAddr,
    /// Bound telnet address when `telnet_enabled` (else `None`).
    pub telnet_addr: Option<SocketAddr>,
    /// Bound finger address when `finger_enabled` (else `None`).
    pub finger_addr: Option<SocketAddr>,
    /// Bound embedded-HTTP address when `http_enabled` (else `None`).
    pub http_addr: Option<SocketAddr>,
    /// Bound NNTP address when `nntp_enabled` (else `None`).
    pub nntp_addr: Option<SocketAddr>,
    /// Bound NNTPS (implicit TLS, RFC 8143) address when `nntp_tls_enabled`
    /// (else `None`).
    pub nntp_tls_addr: Option<SocketAddr>,
    /// Bound NNTP peer-feed (transit) address when `nntp_feed_enabled`
    /// (else `None`).
    pub nntp_feed_addr: Option<SocketAddr>,
    /// Bound implicit-TLS peer-feed address when `nntp_feed_tls_enabled`
    /// (else `None`).
    pub nntp_feed_tls_addr: Option<SocketAddr>,
    /// Bound radio (ICY) delivery address when `radio_enabled` (else `None`).
    pub radio_addr: Option<SocketAddr>,
    /// Bound radio DJ source-ingest address when `radio_source_enabled`
    /// (else `None`).
    pub radio_source_addr: Option<SocketAddr>,
    /// Bound Hotline address when `hotline_enabled` (else `None`).
    pub hotline_addr: Option<SocketAddr>,
    /// Bound FTN binkp address when `ftn_enabled` (else `None`).
    pub ftn_addr: Option<SocketAddr>,
    /// Bound S2S federation address when `federation_enabled` (else `None`).
    pub federation_addr: Option<SocketAddr>,
    pub fingerprint: CertFingerprint,
    tasks: Vec<tokio::task::JoinHandle<()>>,
}

impl Burrow {
    /// Boot: open the store, load/create identity, bind listeners, start
    /// accepting. Returns once listening (not once shut down).
    pub async fn start(config: ServerConfig) -> Result<Burrow> {
        validate_ws_policy(&config)?;
        validate_federation_policy(&config)?;
        if !config.ws_addr.ip().is_loopback() {
            tracing::warn!(
                ws = %config.ws_addr,
                "INSECURE: remote plaintext WebSocket explicitly enabled; credentials and bearer tokens are exposed in transit"
            );
        }

        let data_dir = config.data_dir.clone();
        std::fs::create_dir_all(&data_dir)?;
        s2s::sweep_staging(&data_dir);

        let identity = identity_store::load_or_create(&data_dir, &["localhost".into()])?;
        let fingerprint = identity.tls.fingerprint();

        let pool = rabbithole_store_server::open(&data_dir.join("burrow.db")).await?;
        let bus = EventBus::default();
        let auth = AuthService::new(pool.clone(), config.session_ttl_secs);
        auth.seed_class_masks().await?;

        let classes = ClassCache::load(&pool).await?;
        let blobs = std::sync::Arc::new(
            BlobStore::open(data_dir.join("blobs")).map_err(|e| anyhow::anyhow!("blobs: {e}"))?,
        );

        let origin_name = effective_origin(&config);
        let boards = BoardService::new(pool.clone(), origin_name, identity.signing.seed());

        let quic = QuicListener::bind(config.quic_addr, &identity.tls)?;
        let ws = WsListener::bind_with_allowed_origins(config.ws_addr, &config.ws_allowed_origins)
            .await?;
        let quic_addr = quic.local_addr()?;
        let ws_addr = ws.local_addr()?;

        // TLS-over-TCP acceptor for the NNTPS/STARTTLS surfaces — the same
        // persistent identity (and pinned fingerprint) QUIC presents.
        let tls_acceptor = tokio_rustls::TlsAcceptor::from(identity.tls.server_config()?);

        // Captured before `config` moves into the live handle.
        let radio_library_areas = config.radio_library_areas.clone();
        let federation = config.federation_enabled.then_some(config.federation_addr);
        let federation_peers = config.federation_peers.clone();
        // Best-effort port mapping: only when enabled *and* a gateway IP that
        // actually parses is configured (empty or garbage = feature off).
        let portmap_gateway = config
            .portmap_enabled
            .then(|| {
                config
                    .portmap_gateway
                    .trim()
                    .parse::<std::net::IpAddr>()
                    .ok()
            })
            .flatten();
        let portmap_lifetime = config.portmap_lifetime_secs;
        // Door host: validates the `[[doors]]` list when doors are enabled.
        let door_host = doors::DoorService::from_config(&config, &data_dir)?;

        // Moderation suite: warm the quarantine/deny mirrors before any
        // session can read or upload.
        let moderation = ModerationService::new(pool.clone());
        moderation
            .load()
            .await
            .map_err(|e| anyhow::anyhow!("moderation: {e}"))?;

        let shared = Arc::new(Shared {
            chat: ChatService::new(bus.clone(), config.chat_max_len),
            files: FileService::new(pool.clone()),
            boards,
            presence: PresenceRegistry::new(bus.clone()),
            config: LiveConfig::new(config),
            bus,
            pool,
            auth,
            perms: PermissionEvaluator::new(),
            pushlog: PushLog::new(),
            classes,
            auto_responded: std::sync::Mutex::new(std::collections::HashSet::new()),
            blobs,
            server_key: identity.signing.public().0,
            server_signing_seed: identity.signing.seed(),
            fingerprint_hex: fingerprint.to_hex(),
            quic_bound: quic_addr,
            dedup: DedupStore::with_defaults(),
            transfers: handlers9::TransferRegistry::new(),
            s2s: s2s::S2sState::default(),
            swarm: SwarmCatalog::new(),
            radio: radio::Stations::new(),
            hotline: hotline::Hub::new(),
            doors: door_host,
            peers: PeerRegistry::new(),
            // Reload the last signed local catalog so the generation chain
            // survives restarts (peers must never see a stale "fresh" gen 1).
            catalogs: fed_catalog::CatalogState::load(&data_dir, &identity.signing.public().0),
            // Reload pinned origin keys so key-continuity survives a restart
            // (a reboot must not reopen the origin to a spoofer's re-pin).
            fed_flood: fed_flood::FloodState::load(&data_dir),
            ratelimit: RateLimiter::new(),
            moderation,
            zpartials: zmodem::Partials::new(),
            stats: stats::GatewayStats::new(),
            surfaces: surfaces::Surfaces::new(tls_acceptor.clone(), data_dir.clone()),
            next_session: AtomicU64::new(1),
        });

        // Seed the peer registry: admin-approved keys persisted on disk, plus
        // configured dial targets (implicitly approved on our side).
        for peer in federation::load_approved(&data_dir) {
            shared.peers.seed_approved(peer.key, "", Some(peer.origin));
        }
        for peer in &federation_peers {
            if let (Some(key), true) = (
                federation::hex_key(&peer.key),
                rabbithole_federation::is_valid_server_name(&peer.origin),
            ) {
                shared
                    .peers
                    .seed_approved(key, peer.name.clone(), Some(peer.origin.clone()));
            }
        }

        tracing::info!(
            quic = %quic_addr,
            ws = %ws_addr,
            fingerprint = shared.fingerprint_hex,
            "burrow is up"
        );

        let mut tasks = vec![
            tokio::spawn(accept_loop(Box::new(quic), shared.clone())),
            tokio::spawn(accept_loop(Box::new(ws), shared.clone())),
            tokio::spawn(replay_recorder(shared.clone())),
            tokio::spawn(maintenance(shared.clone())),
            tokio::spawn({
                let shared = shared.clone();
                async move {
                    if let Err(e) = ctl::serve(shared).await {
                        tracing::error!("ctl socket failed: {e}");
                    }
                }
            }),
        ];

        // Library playlist sources: pull each configured file area's audio into
        // a station's rotation. Off by default (empty map). Before the
        // surfaces start, because the radio surface gives every rotation that
        // has tracks a pump.
        install_radio_library(&shared, &radio_library_areas).await;
        if !shared.radio.program_slugs().is_empty() {
            tasks.push(radio::spawn_playlist_driver(shared.clone()));
        }
        if !radio_library_areas.is_empty() {
            tasks.push(tokio::spawn(radio_library_watcher(
                shared.clone(),
                radio_library_areas.clone(),
                std::time::Duration::from_secs(60),
            )));
        }
        // Every optional surface (telnet, finger, HTTP, NNTP and its TLS and
        // feed variants, radio and its source ingest, Hotline, FidoNet, the
        // feed poller): started here to match the config, and again whenever
        // the config changes. One that cannot start is reported, not fatal.
        surfaces::reconcile(&shared).await;
        let bound = |s| shared.surfaces.bound(s);
        let telnet_addr = bound(surfaces::Surface::Telnet);
        let finger_addr = bound(surfaces::Surface::Finger);
        let http_addr = bound(surfaces::Surface::Http);
        let nntp_addr = bound(surfaces::Surface::Nntp);
        let nntp_tls_addr = bound(surfaces::Surface::NntpTls);
        let nntp_feed_addr = bound(surfaces::Surface::NntpFeed);
        let nntp_feed_tls_addr = bound(surfaces::Surface::NntpFeedTls);
        let radio_addr = bound(surfaces::Surface::Radio);
        let radio_source_addr = bound(surfaces::Surface::RadioSource);
        let hotline_addr = bound(surfaces::Surface::Hotline);
        let ftn_addr = bound(surfaces::Surface::Ftn);
        // Looking Glass announce: tell the trackers this burrow exists so it
        // can be found by people who don't already know its address. The task
        // re-reads config every round, so it is spawned unconditionally and
        // stays inert while announcing is off or `advertise_host` is unset.
        {
            let cfg = shared.config.read();
            if cfg.announce_enabled {
                if cfg.advertise_host.trim().is_empty() {
                    tracing::info!(
                        "announce is on but advertise_host is unset — not listing an address \
                         we cannot state; set advertise_host to be discoverable"
                    );
                } else {
                    tracing::info!(
                        trackers = ?cfg.announce_trackers,
                        "announcing to Looking Glass"
                    );
                }
            }
        }
        tasks.push(announce::spawn_announce(shared.clone()));
        let mut federation_addr = None;
        if let Some(addr) = federation {
            let (bound, handle) =
                federation::spawn_federation(shared.clone(), addr, &identity.tls).await?;
            tracing::info!(federation = %bound, "S2S federation peering listening");
            federation_addr = Some(bound);
            tasks.push(handle);
        }
        // Best-effort router port mapping (opt-in). Spawned last, after every
        // listener is already accepting: it is fire-and-forget and can neither
        // fail nor delay boot.
        if let Some(gateway) = portmap_gateway {
            let ports = portmap::Ports {
                quic: quic_addr.port(),
                ws: ws_addr.port(),
            };
            tracing::info!(gateway = %gateway, "best-effort port mapping starting");
            tasks.push(portmap::spawn_portmap(
                shared.clone(),
                gateway,
                portmap_lifetime,
                ports,
            ));
        }

        Ok(Burrow {
            shared,
            quic_addr,
            ws_addr,
            telnet_addr,
            finger_addr,
            http_addr,
            nntp_addr,
            nntp_tls_addr,
            nntp_feed_addr,
            nntp_feed_tls_addr,
            radio_addr,
            radio_source_addr,
            hotline_addr,
            ftn_addr,
            federation_addr,
            fingerprint,
            tasks,
        })
    }

    /// Broadcast shutdown and stop the accept loops.
    pub async fn shutdown(self) {
        self.shared.bus.publish(ServerEvent::Shutdown);
        // Give sessions a beat to observe it before the process moves on.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        self.shared.surfaces.stop_all(&self.shared).await;
        for t in &self.tasks {
            t.abort();
        }
    }
}

fn validate_ws_policy(config: &ServerConfig) -> Result<()> {
    if !config.ws_addr.ip().is_loopback() && !config.ws_allow_insecure_remote {
        anyhow::bail!(
            "refusing plaintext WebSocket listener on {}; keep ws_addr loopback-only behind a WSS reverse proxy, or explicitly set ws_allow_insecure_remote = true to acknowledge credential exposure",
            config.ws_addr
        );
    }
    validate_allowed_origins(&config.ws_allowed_origins)?;
    let public_url = config.ws_public_url.trim();
    if !public_url.is_empty() {
        let normalized = rabbithole_core::api::normalize_secure_ws_endpoint(public_url)
            .map_err(|error| anyhow::anyhow!("invalid ws_public_url: {error}"))?;
        if normalized != public_url {
            anyhow::bail!("ws_public_url must be an explicit ws:// or wss:// URL");
        }
    }
    Ok(())
}

fn effective_origin(config: &ServerConfig) -> String {
    if config.federation_origin.is_empty() {
        config.name.to_lowercase().replace(' ', "-")
    } else {
        config.federation_origin.clone()
    }
}

fn validate_federation_policy(config: &ServerConfig) -> Result<()> {
    if !config.federation_enabled {
        return Ok(());
    }
    if !rabbithole_federation::is_valid_server_name(&config.federation_origin) {
        anyhow::bail!(
            "federation_origin must be set to an immutable lowercase federation server name before federation can be enabled"
        );
    }
    for (index, peer) in config.federation_peers.iter().enumerate() {
        if !rabbithole_federation::is_valid_server_name(&peer.origin) {
            anyhow::bail!(
                "federation_peers[{index}].origin must be set to the peer's immutable lowercase federation origin; migrate this configured peer before starting"
            );
        }
    }
    Ok(())
}

/// Periodic housekeeping: enforce the blob cache policy
/// (`swarm_cache_max_bytes`) by evicting oldest unreferenced blobs over the
/// cap. Referenced library content is never touched; `0` means unlimited
/// ("mirror"), so the sweep is a no-op. Whatever the cap, at startup and
/// hourly, clear what the blob store keeps beside its blobs that no longer
/// belongs (proofs of a removed blob, a write that never finished). Stops on
/// shutdown.
async fn maintenance(shared: Arc<Shared>) {
    let mut rx = shared.bus.subscribe();
    let mut tick = tokio::time::interval(std::time::Duration::from_secs(300));
    let mut ticks: u64 = 0;
    loop {
        tokio::select! {
            _ = tick.tick() => {
                if ticks % 12 == 0 {
                    let blobs = shared.blobs.clone();
                    if let Ok(Ok(n)) = tokio::task::spawn_blocking(move || blobs.sweep_sidecars()).await {
                        if n > 0 {
                            tracing::info!(removed = n, "blob store sidecars swept");
                        }
                    }
                }
                ticks += 1;
                let cap = shared.config.read().swarm_cache_max_bytes;
                if cap == 0 {
                    continue; // mirror: keep everything
                }
                let blobs = shared.blobs.clone();
                match tokio::task::spawn_blocking(move || blobs.evict_unreferenced_over(cap)).await {
                    Ok(Ok(removed)) if !removed.is_empty() => {
                        tracing::info!(evicted = removed.len(), "blob cache trimmed to cap");
                    }
                    Ok(Err(e)) => tracing::warn!("blob cache eviction failed: {e}"),
                    _ => {}
                }
            }
            ev = rx.recv() => {
                if matches!(ev, Ok(ServerEvent::Shutdown) | Err(tokio::sync::broadcast::error::RecvError::Closed)) {
                    break;
                }
            }
        }
    }
}

/// Records broadcast pushes into the replay logs of accounts that are
/// **known but currently offline**, so a token resume can deliver what was
/// missed. Online sessions stamp their own copies as they deliver; guests
/// (negative ids) can't resume and are skipped.
async fn replay_recorder(shared: Arc<Shared>) {
    let mut rx = shared.bus.subscribe();
    loop {
        use tokio::sync::broadcast::error::RecvError;
        match rx.recv().await {
            Ok(ServerEvent::Shutdown) => break,
            // DMs/read-receipts are durably queued in the DM store; the
            // replay ring must not double-deliver them.
            Ok(ServerEvent::Dm { .. }) | Ok(ServerEvent::DmRead { .. }) => continue,
            Ok(event) => {
                let online: std::collections::HashSet<i64> = shared
                    .presence
                    .snapshot()
                    .iter()
                    .map(|e| e.account_id)
                    .collect();
                for account_id in shared.pushlog.known_accounts() {
                    if account_id > 0 && !online.contains(&account_id) {
                        let Some(push) = session::push_for_event(
                            &event,
                            &shared,
                            rabbithole_server_core::Role::User,
                            account_id,
                            0, // no live session: room chat is filtered out
                        ) else {
                            continue;
                        };
                        let _ = shared.pushlog.stamp(account_id, push);
                    }
                }
            }
            Err(RecvError::Lagged(n)) => {
                tracing::warn!(missed = n, "replay recorder lagged behind the bus");
            }
            Err(RecvError::Closed) => break,
        }
    }
}

/// One station a library folder puts on the air: its mount, what to call
/// it, the folder it plays, its rotation, what it sends, what of the folder
/// it cannot send, and the cover art the folder holds.
struct LibraryProgram {
    slug: String,
    name: String,
    area: String,
    tracks: Vec<rabbithole_radio::Track>,
    sound: Option<radio::Sound>,
    unsendable: Vec<rabbithole_radio::TrackId>,
    covers: std::collections::HashMap<String, [u8; 32]>,
}

/// Put a library-backed radio program on the air per configured
/// `mount -> file-area` entry: recurse the area, map its audio files into a
/// playlist, and install it. A missing/empty area retains an empty station
/// that can receive its first tracks without restarting the burrow.
async fn install_radio_library(
    shared: &Arc<Shared>,
    areas: &std::collections::HashMap<String, String>,
) {
    let folders = read_radio_library(shared, areas).await;
    for plan in plan_radio_library(shared, areas, &folders).await {
        let count = plan.tracks.len();
        shared.radio.set_covers(&plan.slug, plan.covers);
        shared
            .radio
            .install_program(&plan.slug, &plan.name, &plan.area, plan.tracks, plan.sound);
        shared
            .radio
            .set_unsendable(&plan.slug, plan.unsendable.iter().copied());
        tracing::info!(mount = %plan.slug, area = %plan.area, tracks = count, "radio library program installed");
    }
}

/// What every configured library folder puts on the air, read afresh.
///
/// The same folders give the same stations at every reading: mounts are
/// settled in name order, a library cannot take a name another already has,
/// and a FLAC mount that is already sending keeps the form its listeners
/// were told about rather than being re-voted by what was added to it.
async fn plan_radio_library(
    shared: &Arc<Shared>,
    areas: &std::collections::HashMap<String, String>,
    folders: &[LibraryFolder],
) -> Vec<LibraryProgram> {
    let mut plans = Vec::new();
    // Names already on the air this pass, so a second library cannot take a
    // mount out from under the first one without saying so.
    let mut installed: std::collections::HashSet<String> = std::collections::HashSet::new();
    for folder in folders {
        let mount = &folder.mount;
        let area = &folder.area;
        let nodes = &folder.nodes;
        // A station sends one kind of sound, so a library holding more than
        // one kind gets a mount for each: the MP3 files where they have
        // always been, the Ogg files at `<mount>.ogg`, the FLAC files at
        // `<mount>.flac`. Nothing is left out for being the wrong kind, and
        // a listener picks which to tune in to. A library of one kind is
        // one mount, exactly as before.
        let split = radio::split_by_sound(nodes);
        let covers = radio::covers_from_nodes(nodes);
        // An existing mount keeps its codec even if the folder gains or
        // loses a format. Otherwise adding MP3 to an Ogg-only library would
        // replace the Ogg rotation under a pump that can only send Ogg.
        // At first installation, prefer an explicit suffix, then MP3, Ogg,
        // FLAC. Empty format rotations remain reserved for later uploads.
        // Which form of FLAC that mount will send is settled here too, by
        // what most of the library is, rather than by whichever track the
        // rotation happens to read first: one voice memo at the top of a
        // folder of albums would otherwise leave every album out.
        let (form, other_forms) = flac_form(shared, &split.flac, sending_form(shared, mount)).await;
        let mut kinds: Vec<(&str, radio::Sound, Vec<rabbithole_radio::Track>)> = vec![
            ("mp3", radio::Sound::Mpeg, split.mpeg),
            ("ogg", radio::Sound::Ogg(0), split.ogg),
            ("flac", radio::Sound::Flac(form), split.flac),
        ];
        // A mount the operator named for a kind — `jukebox.flac` — is that
        // kind's mount. It keeps the name they chose and the other kinds
        // hang off the base of it, so a listener who tunes to a name that
        // says FLAC is not answered MP3, with the FLAC away at a name
        // nobody would guess: `jukebox.flac.flac`.
        let (base, named) = library_mount_parts(mount);
        let primary = shared
            .radio
            .expected_sound(mount)
            .map(sound_extension)
            .or(named)
            .or_else(|| kinds.iter().find(|(_, _, t)| !t.is_empty()).map(|k| k.0));
        kinds.retain(|(ext, _, tracks)| {
            !tracks.is_empty()
                || Some(*ext) == primary
                || shared
                    .radio
                    .expected_sound(&format!("{base}.{ext}"))
                    .is_some()
        });
        if let Some(i) = kinds.iter().position(|k| Some(k.0) == primary) {
            let kind = kinds.remove(i);
            kinds.insert(0, kind);
        }
        // Each mount, what to call it, what goes on it, and what it sends
        // before the first track has been read.
        // What cannot be sent is kept in the rotation so the station can
        // say it left it out, and never offered to a listener to ask for.
        let unsendable: Vec<rabbithole_radio::TrackId> = split
            .other
            .iter()
            .map(|t| t.id)
            .chain(other_forms)
            .collect();
        let mut programs: Vec<(
            String,
            String,
            Vec<rabbithole_radio::Track>,
            Option<radio::Sound>,
        )> = Vec::new();
        for (i, (ext, sound, mut tracks)) in kinds.into_iter().enumerate() {
            if i == 0 {
                // Audio of a kind this burrow cannot send goes with the
                // first mount, which says what it could not play.
                tracks.extend(split.other.iter().cloned());
                programs.push((
                    mount.to_string(),
                    format!("{mount} (library)"),
                    tracks,
                    Some(sound),
                ));
                continue;
            }
            // A station of the operator's own by that name comes first, and
            // so does one another library already put on the air: neither is
            // quietly replaced by one derived from this.
            let beside = format!("{base}.{ext}");
            if areas.contains_key(&beside) || installed.contains(&beside) {
                tracing::warn!(
                    mount = %beside,
                    area = %area,
                    tracks = tracks.len(),
                    "radio: a station of that name is already on the air, so this \
                     library's tracks of that kind are not"
                );
                continue;
            }
            programs.push((
                beside,
                format!("{mount} (library, {})", ext.to_uppercase()),
                tracks,
                Some(sound),
            ));
        }
        if programs.is_empty() {
            // Nothing playable at all: the station still exists, so the
            // console can say the area holds no music it can send.
            programs.push((
                mount.to_string(),
                format!("{mount} (library)"),
                split.other.clone(),
                None,
            ));
        }
        for (slug, name, tracks, sound) in programs {
            installed.insert(slug.clone());
            plans.push(LibraryProgram {
                slug,
                name,
                area: area.clone(),
                tracks,
                sound,
                unsendable: unsendable.clone(),
                covers: covers.clone(),
            });
        }
    }
    plans
}

/// The FLAC form a library's station is already sending, if one is: a
/// mount that is up has told its listeners what it is, and a file added to
/// the folder does not get to change that under them. It only decides which
/// of the new files the station can send.
fn sending_form(shared: &Arc<Shared>, mount: &str) -> Option<radio::Form> {
    let (base, _) = library_mount_parts(mount);
    [mount.to_string(), format!("{base}.flac")]
        .iter()
        .filter(|slug| shared.radio.is_pumped(slug))
        .find_map(|slug| match shared.radio.expected_sound(slug) {
            Some(radio::Sound::Flac(form)) if form != radio::Form::default() => Some(form),
            _ => None,
        })
}

fn library_mount_parts(mount: &str) -> (&str, Option<&'static str>) {
    if let Some((base, suffix)) = mount.rsplit_once('.') {
        if !base.is_empty() {
            for ext in ["mp3", "ogg", "flac"] {
                if suffix.eq_ignore_ascii_case(ext) {
                    return (base, Some(ext));
                }
            }
        }
    }
    (mount, None)
}

fn sound_extension(sound: radio::Sound) -> &'static str {
    match sound {
        radio::Sound::Mpeg => "mp3",
        radio::Sound::Ogg(_) => "ogg",
        radio::Sound::Flac(_) => "flac",
    }
}

/// Bring every library station up to date with its folder: a song added
/// takes its turn, one taken out is not played again, and a folder that
/// now holds a kind of sound it did not gets a station for it. Nobody
/// listening is cut off. A station that has music for the first time gets
/// a pump when the radio is up, and the pump stops with the radio.
async fn refresh_radio_library(
    shared: &Arc<Shared>,
    areas: &std::collections::HashMap<String, String>,
    folders: &[LibraryFolder],
) {
    for plan in plan_radio_library(shared, areas, folders).await {
        let count = plan.tracks.len();
        shared.radio.set_covers(&plan.slug, plan.covers);
        let outcome = shared.radio.refresh_program(
            &plan.slug,
            &plan.name,
            &plan.area,
            plan.tracks,
            plan.sound,
            plan.unsendable,
        );
        match outcome {
            radio::Refreshed::Unchanged => {}
            radio::Refreshed::Changed { dropped, started } => {
                shared.bus.publish(ServerEvent::RadioRequestsChanged {
                    station: plan.slug.clone(),
                });
                tracing::info!(
                    mount = %plan.slug,
                    area = %plan.area,
                    tracks = count,
                    dropped_requests = dropped,
                    "radio library station follows its folder"
                );
                if started {
                    radio::publish_now_playing(
                        shared,
                        &plan.slug,
                        shared.radio.is_live(&plan.slug),
                    );
                }
            }
            radio::Refreshed::Installed => {
                tracing::info!(mount = %plan.slug, area = %plan.area, tracks = count, "radio library station put on the air");
                radio::publish_now_playing(shared, &plan.slug, shared.radio.is_live(&plan.slug));
            }
        }
        shared.surfaces.ensure_radio_pump(shared, &plan.slug).await;
    }
}

/// Only the file metadata that affects rotation, format or cover selection.
/// Download counts and ratings must not force audio headers to be re-read.
#[derive(PartialEq, Eq)]
struct LibraryFile {
    id: i64,
    parent_id: Option<i64>,
    kind: u8,
    name: String,
    mime: String,
    comment: String,
    blob: Option<[u8; 32]>,
}

/// A single manifest read feeds both change detection and the applied plan.
/// Reading it twice could apply transient contents B but record fingerprint A,
/// leaving B's removed tracks in the rotation after the folder returns to A.
struct LibraryFolder {
    mount: String,
    area: String,
    nodes: Vec<rabbithole_store_server::repo6::FileNodeRow>,
}

impl LibraryFolder {
    fn fingerprint(&self) -> (String, Vec<LibraryFile>) {
        let mut files: Vec<_> = self
            .nodes
            .iter()
            .map(|node| LibraryFile {
                id: node.id,
                parent_id: node.parent_id,
                kind: node.kind,
                name: node.name.clone(),
                mime: node.mime.clone(),
                comment: node.comment.clone(),
                blob: node.blob_id,
            })
            .collect();
        files.sort_by_key(|f| f.id);
        (self.mount.clone(), files)
    }
}

/// Cheap change detection: no file bytes are read. Include images and MIME
/// metadata, so replacing cover art or correcting a file's type is noticed.
async fn read_radio_library(
    shared: &Arc<Shared>,
    areas: &std::collections::HashMap<String, String>,
) -> Vec<LibraryFolder> {
    // Stable name order also decides ownership of colliding derived mounts.
    let mut ordered: Vec<(&String, &String)> = areas.iter().collect();
    ordered.sort();
    let mut out = Vec::new();
    for (mount, area) in ordered {
        let nodes = match shared.files.manifest(area, None).await {
            Ok(files) => files.into_iter().map(|(node, _)| node).collect(),
            Err(e) => {
                tracing::warn!(mount = %mount, area = %area, "radio library area unavailable: {e}");
                Vec::new()
            }
        };
        out.push(LibraryFolder {
            mount: mount.clone(),
            area: area.clone(),
            nodes,
        });
    }
    out
}

/// Keep every library station following its folder while the burrow runs.
///
/// A station's rotation used to be read once, at startup, so a song
/// uploaded to its folder could not be played or asked for until a
/// restart. A file landing in a library folder is announced on the bus and
/// looked at straight away; removals, renames and moves are not announced,
/// so the folders are also compared once a minute, cheaply. Audio headers
/// are re-read only after relevant file metadata changes.
async fn radio_library_watcher(
    shared: Arc<Shared>,
    areas: std::collections::HashMap<String, String>,
    interval: std::time::Duration,
) {
    let mut rx = shared.bus.subscribe();
    let mut tick = tokio::time::interval(interval);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // Always reconcile the first snapshot. A file may have arrived between
    // startup's installation scan and this task subscribing to the bus.
    let mut seen = None;
    let library = |area: &str| areas.values().any(|a| a.eq_ignore_ascii_case(area));
    loop {
        let look = tokio::select! {
            _ = tick.tick() => true,
            ev = rx.recv() => match ev {
                Ok(ServerEvent::FileAdded { area, .. }) => library(&area),
                // Missed some: look, since a file may have landed in them.
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => true,
                Ok(ServerEvent::Shutdown)
                | Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                Ok(_) => false,
            },
        };
        if !look {
            continue;
        }
        let folders = read_radio_library(&shared, &areas).await;
        let now: Vec<_> = folders.iter().map(LibraryFolder::fingerprint).collect();
        if seen.as_ref() != Some(&now) {
            refresh_radio_library(&shared, &areas, &folders).await;
            seen = Some(now);
        }
    }
}

/// What form of FLAC most of a rotation is: the rate, the channel count and
/// the depth a mount will say once, at the head of its stream.
///
/// A FLAC file's STREAMINFO is its first metadata block, so this reads the
/// front of each track rather than the track. A file that has audio where
/// its headers end votes for what it is; one that only *says* what it is —
/// a download that stopped after the metadata, a file whose picture is
/// bigger than the look — votes only when nothing better does, because
/// otherwise a shelf of stalled downloads could settle what a station sends
/// and leave every whole file out of it.
///
/// A library nobody can read, or one that is not FLAC after all, gives back
/// the form nobody has looked up, and the first track the pump plays settles
/// it instead.
/// And which of `tracks` said they were in another form: the mount cannot
/// send those, so they are not offered to listeners to ask for.
async fn flac_form(
    shared: &Arc<Shared>,
    tracks: &[rabbithole_radio::Track],
    sending: Option<radio::Form>,
) -> (radio::Form, Vec<rabbithole_radio::TrackId>) {
    if tracks.is_empty() {
        return (radio::Form::default(), Vec::new());
    }
    let blobs = shared.blobs.clone();
    let ids: Vec<(rabbithole_radio::TrackId, rabbithole_blobs::BlobId)> = tracks
        .iter()
        .map(|t| (t.id, rabbithole_blobs::BlobId(t.source.0)))
        .collect();
    let counted = tokio::task::spawn_blocking(move || {
        // Enough for the magic, the metadata chain and the first frame
        // header of an ordinary file. A file whose cover art is bigger
        // than this still says what it is; it just cannot show it here.
        const LOOK: usize = 64 << 10;
        // What each file said, in the order the area lists them: whether
        // it showed audio, and the form it claims.
        let mut said: Vec<(rabbithole_radio::TrackId, bool, (u32, u8, u8))> = Vec::new();
        for (track, id) in ids {
            let Ok(front) = blobs.read_range(&id, 0, LOOK) else {
                continue;
            };
            let looked = |bytes: &[u8]| {
                rabbithole_radio::flac::playable(bytes)
                    .map(|i| (true, (i.sample_rate, i.channels, i.bits_per_sample)))
                    .or_else(|| rabbithole_radio::flac::form_of(bytes).map(|f| (false, f)))
            };
            let seen = looked(&front).or_else(|| {
                // A tag in front of the music can be longer than the look,
                // and its own header says how long. The second look starts
                // where the tag ends.
                let skip = rabbithole_radio::mp3::id3v2_says(&front);
                if skip == 0 {
                    return None;
                }
                let past = blobs.read_range(&id, skip as u64, LOOK).ok()?;
                looked(&past)
            });
            if let Some((playable, form)) = seen {
                said.push((track, playable, form));
            }
        }
        said
    })
    .await
    .unwrap_or_default();
    // The files that showed audio decide it, if any of them did.
    let heard = counted.iter().any(|(_, playable, _)| *playable);
    // The most of any one form wins. A tie goes to whichever came first in
    // the area — the one the station would have played anyway — so the
    // answer is the same at every start.
    let mut tally: Vec<((u32, u8, u8), usize, usize)> = Vec::new();
    for (at, (_, playable, form)) in counted.iter().copied().enumerate() {
        if heard && !playable {
            continue;
        }
        match tally.iter_mut().find(|(seen, _, _)| *seen == form) {
            Some((_, count, _)) => *count += 1,
            None => tally.push((form, 1, at)),
        }
    }
    // A mount already sending has settled it for its listeners.
    let chosen = match sending {
        Some(f) => Some((f.rate, f.channels, f.bits)),
        None => tally
            .into_iter()
            .max_by_key(|(_, count, first)| (*count, std::cmp::Reverse(*first)))
            .map(|(form, _, _)| form),
    };
    let Some((rate, channels, bits)) = chosen else {
        return (radio::Form::default(), Vec::new());
    };
    // A file whose headers name another form cannot go out on this mount.
    // One that named no form, or could not be read, is the pump's to find
    // out about when its turn comes.
    let other = counted
        .iter()
        .filter(|(_, _, form)| *form != (rate, channels, bits))
        .map(|(track, _, _)| *track)
        .collect();
    (
        radio::Form {
            rate,
            channels,
            bits,
        },
        other,
    )
}

/// Resolve a possibly-relative path under `base` (absolute paths pass through).
pub(crate) fn resolve_dir(base: &std::path::Path, p: &std::path::Path) -> std::path::PathBuf {
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        base.join(p)
    }
}

async fn accept_loop(mut listener: Box<dyn Listener>, shared: Arc<Shared>) {
    loop {
        match listener.accept().await {
            Ok(conn) => {
                // Over the per-IP connection budget: drop it on the floor.
                let ip = conn.peer().remote_addr.ip();
                if !shared.rate_allow(Scope::Ip(ip), ratelimit::class::CONN) {
                    continue;
                }
                let session_id = shared.next_session_id();
                let shared = shared.clone();
                tokio::spawn(async move {
                    if let Err(e) = session::run_session(conn, session_id, shared).await {
                        tracing::debug!(session_id, "session error: {e}");
                    }
                });
            }
            Err(e) => {
                tracing::warn!("accept failed: {e}");
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rabbithole_server_core::Role;

    #[test]
    fn library_mount_suffixes_are_case_insensitive_and_utf8_safe() {
        assert_eq!(library_mount_parts("éxxx"), ("éxxx", None));
        assert_eq!(library_mount_parts("夜.FLaC"), ("夜", Some("flac")));
        assert_eq!(library_mount_parts("mix.ogg"), ("mix", Some("ogg")));
        assert_eq!(library_mount_parts(".mp3"), (".mp3", None));
        assert_eq!(library_mount_parts("mix.other"), ("mix.other", None));
    }

    async fn await_rotation(shared: &Arc<Shared>, slug: &str, count: usize) {
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            while shared.radio.track_count(slug) != count {
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap_or_else(|_| {
            panic!(
                "{slug} should have {count} tracks, observed {}",
                shared.radio.track_count(slug)
            )
        });
    }

    #[tokio::test]
    async fn library_watcher_preserves_codecs_clears_removed_formats_and_reloads_covers() {
        let dir = tempfile::tempdir().unwrap();
        // No configured watcher or radio surface: this test controls the
        // startup scan boundary and polls the real file store at a short interval.
        let burrow = Burrow::start(ServerConfig {
            quic_addr: "127.0.0.1:0".parse().unwrap(),
            ws_addr: "127.0.0.1:0".parse().unwrap(),
            data_dir: dir.path().into(),
            ..ServerConfig::default()
        })
        .await
        .unwrap();
        let shared = &burrow.shared;
        let owner = shared
            .auth
            .create_account("dj", "password", Role::Admin)
            .await
            .unwrap();
        shared
            .files
            .create_area("music", "Music", "")
            .await
            .unwrap();
        let add = |name: &'static str, mime: &'static str, blob: [u8; 32]| async move {
            shared
                .files
                .add_file("music", None, name, &blob, 1, mime, "", "", "dj", owner.id)
                .await
                .unwrap()
        };
        let ogg = add("one.opus", "audio/ogg", [1; 32]).await;
        let areas = std::collections::HashMap::from([("mix".into(), "music".into())]);
        install_radio_library(shared, &areas).await;
        assert_eq!(
            shared.radio.expected_sound("mix"),
            Some(radio::Sound::Ogg(0))
        );

        // These files arrive after installation but before the watcher starts.
        // They must not be absorbed into its first snapshot without a refresh.
        let mp3 = add("two.mp3", "audio/mpeg", [2; 32]).await;
        let requested = add("three.mp3", "audio/mpeg", [3; 32]).await;
        let watcher = tokio::spawn(radio_library_watcher(
            shared.clone(),
            areas.clone(),
            std::time::Duration::from_millis(20),
        ));
        await_rotation(shared, "mix.mp3", 2).await;
        assert_eq!(shared.radio.track_count("mix"), 1);
        assert_eq!(
            shared.radio.expected_sound("mix"),
            Some(radio::Sound::Ogg(0))
        );
        assert!(!shared.radio.program_slugs().contains(&"mix.ogg".into()));
        let waiting = if shared.radio.current_track("mix.mp3").unwrap().id.0 == mp3.id as u64 {
            requested.id
        } else {
            mp3.id
        };
        shared
            .radio
            .request("mix.mp3", waiting as u64, "listener", |_| false)
            .unwrap();

        // No audio changed: a newly supplied cover still refreshes the station.
        let cover = add("cover.png", "image/png", [4; 32]).await;
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            while shared.radio.cover_for("mix", "one.opus") != cover.blob_id {
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("cover-only changes should be noticed");

        // Delete a whole secondary format. The last song may finish, but the
        // old rotation and queued request must not survive in a stale program.
        shared.files.delete(mp3.id).await.unwrap();
        shared.files.delete(requested.id).await.unwrap();
        await_rotation(shared, "mix.mp3", 0).await;
        assert!(shared
            .radio
            .requests("mix.mp3", "listener", |_| false)
            .unwrap()
            .queue
            .is_empty());
        shared.radio.advance("mix.mp3", 0);
        assert!(shared.radio.current_track("mix.mp3").is_none());
        assert_eq!(
            shared.radio.expected_sound("mix.mp3"),
            Some(radio::Sound::Mpeg)
        );

        // Removing the primary format cannot move the secondary onto its URL.
        shared.files.delete(ogg.id).await.unwrap();
        await_rotation(shared, "mix", 0).await;
        shared.radio.advance("mix", 0);
        let again = add("again.mp3", "audio/mpeg", [5; 32]).await;
        await_rotation(shared, "mix.mp3", 1).await;
        assert_eq!(shared.radio.track_count("mix"), 0);
        assert_eq!(
            shared.radio.current_track("mix.mp3").unwrap().id.0,
            again.id as u64
        );
        let ogg_again = add("again.opus", "audio/ogg", [6; 32]).await;
        await_rotation(shared, "mix", 1).await;
        assert_eq!(
            shared.radio.current_track("mix").unwrap().id.0,
            ogg_again.id as u64
        );
        assert_eq!(
            shared.radio.expected_sound("mix"),
            Some(radio::Sound::Ogg(0))
        );
        assert!(!shared.radio.is_pumped("mix"), "disabled surface stays off");
        watcher.abort();
        let _ = watcher.await;

        // A -> B -> A while a refresh is in flight: a file uploaded after
        // the scan must not sneak into that scan's plan, then survive its
        // deletion because the recorded fingerprint still describes A.
        let in_flight = read_radio_library(shared, &areas).await;
        let transient = add("transient.mp3", "audio/mpeg", [7; 32]).await;
        refresh_radio_library(shared, &areas, &in_flight).await;
        assert_eq!(
            shared.radio.track_count("mix.mp3"),
            1,
            "apply exactly the captured folder state"
        );
        shared.files.delete(transient.id).await.unwrap();
        let after = read_radio_library(shared, &areas).await;
        assert!(
            in_flight
                .iter()
                .map(LibraryFolder::fingerprint)
                .collect::<Vec<_>>()
                == after
                    .iter()
                    .map(LibraryFolder::fingerprint)
                    .collect::<Vec<_>>()
        );
        assert!(shared
            .radio
            .request("mix.mp3", transient.id as u64, "listener", |_| false)
            .is_err());
        burrow.shutdown().await;
    }

    #[test]
    fn websocket_defaults_to_loopback_only() {
        let config = ServerConfig::default();
        assert!(config.ws_addr.ip().is_loopback());
        assert!(!config.ws_allow_insecure_remote);
        assert!(validate_ws_policy(&config).is_ok());
    }

    #[test]
    fn federation_requires_an_immutable_origin() {
        let mut config = ServerConfig {
            federation_enabled: true,
            ..ServerConfig::default()
        };
        assert!(validate_federation_policy(&config).is_err());
        config.federation_origin = "warren.example".into();
        assert!(validate_federation_policy(&config).is_ok());
        config.name = "A Different Display Name".into();
        assert_eq!(effective_origin(&config), "warren.example");
    }

    #[test]
    fn federation_rejects_unmigrated_configured_peer_origins() {
        let mut config = ServerConfig {
            federation_enabled: true,
            federation_origin: "warren.example".into(),
            federation_peers: vec![rabbithole_server_core::config::FederationPeer {
                name: "legacy-peer".into(),
                ..Default::default()
            }],
            ..ServerConfig::default()
        };
        let error = validate_federation_policy(&config).unwrap_err().to_string();
        assert!(error.contains("federation_peers[0].origin"));
        assert!(error.contains("migrate"));

        config.federation_peers[0].origin = "peer.example".into();
        assert!(validate_federation_policy(&config).is_ok());
    }

    #[test]
    fn remote_plaintext_websocket_requires_explicit_acknowledgement() {
        let mut config = ServerConfig {
            ws_addr: "0.0.0.0:4654".parse().unwrap(),
            ..ServerConfig::default()
        };
        assert!(validate_ws_policy(&config).is_err());

        config.ws_allow_insecure_remote = true;
        assert!(validate_ws_policy(&config).is_ok());
    }

    #[test]
    fn public_websocket_url_and_browser_origins_fail_closed() {
        let mut config = ServerConfig {
            ws_public_url: "ws://burrow.example:4654".into(),
            ..ServerConfig::default()
        };
        assert!(validate_ws_policy(&config).is_err());

        config.ws_public_url = "wss://burrow.example/rhp".into();
        config.ws_allowed_origins = vec!["http://burrow.example".into()];
        assert!(validate_ws_policy(&config).is_err());

        config.ws_allowed_origins = vec!["https://burrow.example".into()];
        assert!(validate_ws_policy(&config).is_ok());
    }
}
