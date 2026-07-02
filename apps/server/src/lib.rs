//! Burrow as a library: everything `main.rs` does, callable from tests.

#![forbid(unsafe_code)]

pub mod admin_store;
pub mod ctl;
pub mod handlers2;
pub mod handlers3;
pub mod identity_store;
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
    AuthService, ChatService, ClassCache, EventBus, LiveConfig, PermissionEvaluator,
    PresenceRegistry, PushLog, RegistrationMode, ServerConfig, ServerEvent,
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
    pub pushlog: PushLog,
    pub classes: ClassCache,
    /// (sender_account, recipient_account) pairs already auto-responded
    /// this away period (cleared when the recipient comes back online).
    pub auto_responded: std::sync::Mutex<std::collections::HashSet<(i64, i64)>>,
    pub blobs: std::sync::Arc<BlobStore>,
    pub server_key: [u8; 32],
    pub fingerprint_hex: String,
    next_session: AtomicU64,
}

impl Shared {
    pub fn next_session_id(&self) -> u64 {
        self.next_session.fetch_add(1, Ordering::Relaxed)
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

        let quic = QuicListener::bind(config.quic_addr, &identity.tls)?;
        let ws = WsListener::bind(config.ws_addr).await?;
        let quic_addr = quic.local_addr()?;
        let ws_addr = ws.local_addr()?;

        let shared = Arc::new(Shared {
            chat: ChatService::new(bus.clone(), config.chat_max_len),
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
            fingerprint_hex: fingerprint.to_hex(),
            next_session: AtomicU64::new(1),
        });

        tracing::info!(
            quic = %quic_addr,
            ws = %ws_addr,
            fingerprint = shared.fingerprint_hex,
            "burrow is up"
        );

        let tasks = vec![
            tokio::spawn(accept_loop(Box::new(quic), shared.clone())),
            tokio::spawn(accept_loop(Box::new(ws), shared.clone())),
            tokio::spawn(replay_recorder(shared.clone())),
            tokio::spawn({
                let shared = shared.clone();
                async move {
                    if let Err(e) = ctl::serve(shared).await {
                        tracing::error!("ctl socket failed: {e}");
                    }
                }
            }),
        ];

        Ok(Burrow {
            shared,
            quic_addr,
            ws_addr,
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
