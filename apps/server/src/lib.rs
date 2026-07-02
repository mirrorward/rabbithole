//! Burrow as a library: everything `main.rs` does, callable from tests.

#![forbid(unsafe_code)]

pub mod admin_store;
pub mod ctl;
pub mod fed_catalog;
pub mod federation;
pub mod ftn;
pub mod handlers10;
pub mod handlers2;
pub mod handlers3;
pub mod handlers4;
pub mod handlers5;
pub mod handlers6;
pub mod handlers7;
pub mod handlers8;
pub mod handlers9;
pub mod hotline;
pub mod identity_store;
pub mod legacy;
pub mod nntp;
pub mod radio;
pub mod session;

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use anyhow::Result;
use rabbithole_blobs::BlobStore;
use rabbithole_net::quic::QuicListener;
use rabbithole_net::tls::CertFingerprint;
use rabbithole_net::ws::WsListener;
use rabbithole_net::Listener;
use rabbithole_server_core::{
    AuthService, BoardService, ChatService, ClassCache, DedupStore, EventBus, FileService,
    LiveConfig, PeerRegistry, PermissionEvaluator, PresenceRegistry, PushLog, RegistrationMode,
    ServerConfig, ServerEvent, SwarmCatalog,
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
    /// The shared dupe/seen gate — prevents reprocessing and rebroadcast
    /// loops once federation (W9) and syndication (W10) come online.
    pub dedup: DedupStore,
    /// Live bulk-transfer tickets (Wave 4.2).
    pub transfers: handlers9::TransferRegistry,
    /// TTL'd who-has-what soft state for the Warren (Wave 5).
    pub swarm: SwarmCatalog,
    /// Radio station directory + live ICY mount fan-out (Wave 11.4).
    pub radio: radio::Stations,
    /// Connected Hotline clients for IM routing + user-list icons (Wave 7.3).
    pub hotline: hotline::Hub,
    /// Known/approved/pending S2S federation peers + their state (Wave 9).
    pub peers: PeerRegistry,
    /// Local signed file-catalog + verified peer catalogs (Wave 9.x).
    pub catalogs: fed_catalog::CatalogState,
    next_session: AtomicU64,
}

impl Shared {
    pub fn next_session_id(&self) -> u64 {
        self.next_session.fetch_add(1, Ordering::Relaxed)
    }

    /// The server's origin id for `persona@origin` event authorship: the
    /// configured name, lowercased and space-free (federation hostnames
    /// arrive in W9).
    pub fn origin_name(&self) -> String {
        self.config.read().name.to_lowercase().replace(' ', "-")
    }

    /// Parse the configured registration mode (bad values read as closed —
    /// fail safe).
    pub fn registration_mode(&self) -> RegistrationMode {
        RegistrationMode::parse(&self.config.read().registration_mode)
            .unwrap_or(RegistrationMode::Closed)
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
    /// Bound NNTP address when `nntp_enabled` (else `None`).
    pub nntp_addr: Option<SocketAddr>,
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
        let data_dir = config.data_dir.clone();
        std::fs::create_dir_all(&data_dir)?;

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

        let origin_name = config.name.to_lowercase().replace(' ', "-");
        let boards = BoardService::new(pool.clone(), origin_name, identity.signing.seed());

        let quic = QuicListener::bind(config.quic_addr, &identity.tls)?;
        let ws = WsListener::bind(config.ws_addr).await?;
        let quic_addr = quic.local_addr()?;
        let ws_addr = ws.local_addr()?;

        // Legacy listener toggles (captured before `config` moves into the
        // live handle).
        let telnet = config.telnet_enabled.then_some(config.telnet_addr);
        let finger = config.finger_enabled.then_some(config.finger_addr);
        let nntp = config.nntp_enabled.then_some(config.nntp_addr);
        let radio = config.radio_enabled.then_some(config.radio_addr);
        let radio_source = config
            .radio_source_enabled
            .then_some(config.radio_source_addr);
        let radio_library_areas = config.radio_library_areas.clone();
        let hotline = config.hotline_enabled.then_some(config.hotline_addr);
        let ftn = config.ftn_enabled.then_some(config.ftn_addr);
        let federation = config.federation_enabled.then_some(config.federation_addr);
        let federation_peers = config.federation_peers.clone();
        // FTN spool dirs resolve under data_dir when relative.
        let ftn_inbound = resolve_dir(&data_dir, &config.ftn_inbound_dir);
        let ftn_outbound = resolve_dir(&data_dir, &config.ftn_outbound_dir);

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
            dedup: DedupStore::with_defaults(),
            transfers: handlers9::TransferRegistry::new(),
            swarm: SwarmCatalog::new(),
            radio: radio::Stations::new(),
            hotline: hotline::Hub::new(),
            peers: PeerRegistry::new(),
            // Reload the last signed local catalog so the generation chain
            // survives restarts (peers must never see a stale "fresh" gen 1).
            catalogs: fed_catalog::CatalogState::load(&data_dir, &identity.signing.public().0),
            next_session: AtomicU64::new(1),
        });

        // Seed the peer registry: admin-approved keys persisted on disk, plus
        // configured dial targets (implicitly approved on our side).
        for key in federation::load_approved(&data_dir) {
            shared.peers.seed_approved(key, "");
        }
        for peer in &federation_peers {
            if let Some(key) = federation::hex_key(&peer.key) {
                shared.peers.seed_approved(key, peer.name.clone());
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

        // Opt-in legacy surfaces (Wave 6).
        let mut telnet_addr = None;
        if let Some(addr) = telnet {
            let (bound, handle) = legacy::spawn_telnet(shared.clone(), addr).await?;
            tracing::info!(telnet = %bound, "telnet BBS listening");
            telnet_addr = Some(bound);
            tasks.push(handle);
        }
        let mut finger_addr = None;
        if let Some(addr) = finger {
            let (bound, handle) = legacy::spawn_finger(shared.clone(), addr).await?;
            tracing::info!(finger = %bound, "finger listening");
            finger_addr = Some(bound);
            tasks.push(handle);
        }
        let mut nntp_addr = None;
        if let Some(addr) = nntp {
            let (bound, handle) = nntp::spawn_nntp(shared.clone(), addr).await?;
            tracing::info!(nntp = %bound, "NNTP gateway listening");
            nntp_addr = Some(bound);
            tasks.push(handle);
        }
        let mut radio_addr = None;
        if let Some(addr) = radio {
            let (bound, handle) = radio::spawn_radio(shared.clone(), addr).await?;
            tracing::info!(radio = %bound, "radio (ICY) listening");
            radio_addr = Some(bound);
            tasks.push(handle);
        }
        // Library playlist sources: pull each configured file area's audio into
        // a station's rotation. Off by default (empty map).
        install_radio_library(&shared, &radio_library_areas).await;
        if !shared.radio.program_slugs().is_empty() {
            tasks.push(radio::spawn_playlist_driver(shared.clone()));
        }
        let mut radio_source_addr = None;
        if let Some(addr) = radio_source {
            let (bound, handle) = radio::spawn_radio_source(shared.clone(), addr).await?;
            tracing::info!(radio_source = %bound, "radio DJ source ingest listening");
            radio_source_addr = Some(bound);
            tasks.push(handle);
        }
        let mut hotline_addr = None;
        if let Some(addr) = hotline {
            let (bound, handle) = hotline::spawn_hotline(shared.clone(), addr).await?;
            tracing::info!(hotline = %bound, "Hotline listening");
            hotline_addr = Some(bound);
            tasks.push(handle);
        }
        let mut ftn_addr = None;
        if let Some(addr) = ftn {
            let (bound, handle) =
                ftn::spawn_ftn(shared.clone(), addr, ftn_inbound, ftn_outbound).await?;
            tracing::info!(ftn = %bound, "FidoNet (binkp) gateway listening");
            ftn_addr = Some(bound);
            tasks.push(handle);
        }
        let mut federation_addr = None;
        if let Some(addr) = federation {
            let (bound, handle) =
                federation::spawn_federation(shared.clone(), addr, &identity.tls).await?;
            tracing::info!(federation = %bound, "S2S federation peering listening");
            federation_addr = Some(bound);
            tasks.push(handle);
        }

        Ok(Burrow {
            shared,
            quic_addr,
            ws_addr,
            telnet_addr,
            finger_addr,
            nntp_addr,
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
        for t in &self.tasks {
            t.abort();
        }
    }
}

/// Periodic housekeeping: enforce the blob cache policy
/// (`swarm_cache_max_bytes`) by evicting oldest unreferenced blobs over the
/// cap. Referenced library content is never touched; `0` means unlimited
/// ("mirror"), so the sweep is a no-op. Stops on shutdown.
async fn maintenance(shared: Arc<Shared>) {
    let mut rx = shared.bus.subscribe();
    let mut tick = tokio::time::interval(std::time::Duration::from_secs(300));
    loop {
        tokio::select! {
            _ = tick.tick() => {
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

/// Build a library-backed radio program per configured `mount -> file-area`
/// entry: recurse the area, map its audio files into a playlist, and install
/// it. A missing/empty area logs and is skipped (the station just has no
/// automation until a DJ goes live).
async fn install_radio_library(
    shared: &Arc<Shared>,
    areas: &std::collections::HashMap<String, String>,
) {
    for (mount, area) in areas {
        let nodes = match shared.files.manifest(area, None).await {
            Ok(files) => files
                .into_iter()
                .map(|(node, _rel)| node)
                .collect::<Vec<_>>(),
            Err(e) => {
                tracing::warn!(mount = %mount, area = %area, "radio library area unavailable: {e}");
                Vec::new()
            }
        };
        let tracks = radio::tracks_from_nodes(&nodes);
        let count = tracks.len();
        shared
            .radio
            .install_program(mount, &format!("{mount} (library)"), area, tracks);
        tracing::info!(mount = %mount, area = %area, tracks = count, "radio library program installed");
    }
}

/// Resolve a possibly-relative path under `base` (absolute paths pass through).
fn resolve_dir(base: &std::path::Path, p: &std::path::Path) -> std::path::PathBuf {
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
