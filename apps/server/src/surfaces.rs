//! The burrow's optional network surfaces, started and stopped while it runs.
//!
//! Every gateway used to be bound once, at boot, from a snapshot of the
//! config. Turning one on in the admin console therefore did nothing until a
//! restart, and the console could not say so honestly: it showed the *setting*
//! ("enabled") where the operator needed the *fact* (is anything listening?).
//!
//! [`reconcile`] closes that gap. It compares what the live config asks for
//! with what is running, and starts, stops or restarts each surface to match.
//! It runs once at boot and again after every config change, before the
//! change is acknowledged, so by the time the console hears "applied" the
//! listener is up or the reason it is not is on record ([`Surfaces::report`]).
//!
//! Stopping a surface stops it *accepting*. Sessions already connected are
//! their own tasks and finish on their own; nobody is cut off mid-transfer
//! because an operator flipped a switch.
//!
//! A surface that cannot start is reported, not fatal: a burrow whose NNTP
//! port is taken is still a burrow, and one that refused to boot could not be
//! fixed from its own console. (The two transports clients arrive on, QUIC and
//! the web socket, are not surfaces. They still fail the boot.)

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use rabbithole_server_core::ServerConfig;
use tokio::task::JoinHandle;

use crate::Shared;

/// A surface the supervisor owns. The order here is the order they start in
/// and the order they are reported in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Surface {
    Telnet,
    Finger,
    Http,
    Nntp,
    NntpTls,
    NntpFeed,
    NntpFeedTls,
    Radio,
    RadioSource,
    Hotline,
    Ftn,
    Syndication,
}

impl Surface {
    pub const ALL: [Surface; 12] = [
        Surface::Telnet,
        Surface::Finger,
        Surface::Http,
        Surface::Nntp,
        Surface::NntpTls,
        Surface::NntpFeed,
        Surface::NntpFeedTls,
        Surface::Radio,
        Surface::RadioSource,
        Surface::Hotline,
        Surface::Ftn,
        Surface::Syndication,
    ];

    /// The config key that switches this surface on: how a console ties a
    /// report to the switch it belongs beside.
    pub fn key(self) -> &'static str {
        match self {
            Surface::Telnet => "telnet_enabled",
            Surface::Finger => "finger_enabled",
            Surface::Http => "http_enabled",
            Surface::Nntp => "nntp_enabled",
            Surface::NntpTls => "nntp_tls_enabled",
            Surface::NntpFeed => "nntp_feed_enabled",
            Surface::NntpFeedTls => "nntp_feed_tls_enabled",
            Surface::Radio => "radio_enabled",
            Surface::RadioSource => "radio_source_enabled",
            Surface::Hotline => "hotline_enabled",
            Surface::Ftn => "ftn_enabled",
            Surface::Syndication => "syndication_enabled",
        }
    }
}

/// What is actually going on with a surface, as opposed to what is configured.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SurfaceState {
    /// Not asked for, or stopped.
    Off,
    /// Bound and accepting on this address.
    Listening(SocketAddr),
    /// Running, with no address of its own (the feed poller).
    Running,
    /// Asked for and not running; the reason, in the system's words.
    Failed(String),
    /// Switched on, with nothing to do (feeds enabled and none mapped).
    Idle(&'static str),
}

/// Everything about a surface that, if it changed, means the running one is no
/// longer the one the config describes. Compared whole: any difference is a
/// restart.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Spec {
    Listener(SocketAddr),
    Http {
        addr: SocketAddr,
        web_root: Option<PathBuf>,
    },
    Ftn {
        addr: SocketAddr,
        inbound: PathBuf,
        outbound: PathBuf,
        // Parsed once into the gateway when it starts.
        node: String,
        uplink: String,
        uplink_host: String,
        password: String,
        areas: Vec<(String, String)>,
    },
    Syndication {
        poll_secs: i64,
        feeds: Vec<(String, String)>,
    },
}

struct Running {
    spec: Spec,
    handles: Vec<JoinHandle<()>>,
}

/// The supervisor's state. One per burrow, on [`Shared`].
pub struct Surfaces {
    /// Held across a whole reconcile, so two config changes landing together
    /// cannot both try to bind the same port.
    running: tokio::sync::Mutex<HashMap<Surface, Running>>,
    /// Read from anywhere, never across an await.
    states: parking_lot::Mutex<HashMap<Surface, SurfaceState>>,
    tls: tokio_rustls::TlsAcceptor,
    data_dir: PathBuf,
}

fn sorted(map: &HashMap<String, String>) -> Vec<(String, String)> {
    let mut v: Vec<(String, String)> = map.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    v.sort();
    v
}

impl Surfaces {
    pub fn new(tls: tokio_rustls::TlsAcceptor, data_dir: PathBuf) -> Self {
        Self {
            running: tokio::sync::Mutex::new(HashMap::new()),
            states: parking_lot::Mutex::new(HashMap::new()),
            tls,
            data_dir,
        }
    }

    /// What `surface` is doing right now.
    pub fn state(&self, surface: Surface) -> SurfaceState {
        self.states
            .lock()
            .get(&surface)
            .cloned()
            .unwrap_or(SurfaceState::Off)
    }

    /// The address `surface` is accepting on, when it is.
    pub fn bound(&self, surface: Surface) -> Option<SocketAddr> {
        match self.state(surface) {
            SurfaceState::Listening(addr) => Some(addr),
            _ => None,
        }
    }

    /// Start a newly populated station under the same lock as surface
    /// stop/restart. Checking and spawning before taking this lock can leave
    /// two pumps after a restart, or create an orphan mount after shutdown.
    pub async fn ensure_radio_pump(&self, shared: &Arc<Shared>, slug: &str) {
        if let Some(run) = self.running.lock().await.get_mut(&Surface::Radio) {
            if shared.radio.track_count(slug) > 0 && !shared.radio.is_pumped(slug) {
                run.handles.push(crate::radio::spawn_program_pump(
                    shared.clone(),
                    slug.into(),
                ));
            }
        }
    }

    /// Every surface and what it is doing, in [`Surface::ALL`] order.
    pub fn report(&self) -> Vec<(Surface, SurfaceState)> {
        Surface::ALL.iter().map(|s| (*s, self.state(*s))).collect()
    }

    fn resolve(&self, dir: &std::path::Path) -> PathBuf {
        crate::resolve_dir(&self.data_dir, dir)
    }

    /// What the config asks of `surface`: `None` for off.
    fn wanted(&self, surface: Surface, c: &ServerConfig) -> Option<Spec> {
        let listener = |on: bool, addr: SocketAddr| on.then_some(Spec::Listener(addr));
        match surface {
            Surface::Telnet => listener(c.telnet_enabled, c.telnet_addr),
            Surface::Finger => listener(c.finger_enabled, c.finger_addr),
            Surface::Http => c.http_enabled.then(|| Spec::Http {
                addr: c.http_addr,
                // Empty = no static serving (the /files handoff still works).
                web_root: (!c.http_web_root.as_os_str().is_empty())
                    .then(|| self.resolve(&c.http_web_root)),
            }),
            Surface::Nntp => listener(c.nntp_enabled, c.nntp_addr),
            Surface::NntpTls => listener(c.nntp_tls_enabled, c.nntp_tls_addr),
            Surface::NntpFeed => listener(c.nntp_feed_enabled, c.nntp_feed_addr),
            Surface::NntpFeedTls => listener(c.nntp_feed_tls_enabled, c.nntp_feed_tls_addr),
            Surface::Radio => listener(c.radio_enabled, c.radio_addr),
            Surface::RadioSource => listener(c.radio_source_enabled, c.radio_source_addr),
            Surface::Hotline => listener(c.hotline_enabled, c.hotline_addr),
            Surface::Ftn => c.ftn_enabled.then(|| Spec::Ftn {
                addr: c.ftn_addr,
                inbound: self.resolve(&c.ftn_inbound_dir),
                outbound: self.resolve(&c.ftn_outbound_dir),
                node: c.ftn_node.clone(),
                uplink: c.ftn_uplink.clone(),
                uplink_host: c.ftn_uplink_host.clone(),
                password: c.ftn_password.clone(),
                areas: sorted(&c.ftn_areas),
            }),
            // Worth running only when enabled *and* mapped.
            Surface::Syndication => (c.syndication_enabled && !c.syndication_feeds.is_empty())
                .then(|| Spec::Syndication {
                    poll_secs: c.syndication_poll_secs,
                    feeds: sorted(&c.syndication_feeds),
                }),
        }
    }

    /// Start `surface` as `spec` describes. The bound address (when it has
    /// one) and every task that is part of it.
    async fn start(
        &self,
        shared: &Arc<Shared>,
        surface: Surface,
        spec: &Spec,
    ) -> anyhow::Result<(SurfaceState, Vec<JoinHandle<()>>)> {
        let s = shared.clone();
        let one = |(addr, handle): (SocketAddr, JoinHandle<()>)| {
            (SurfaceState::Listening(addr), vec![handle])
        };
        Ok(match (surface, spec) {
            (Surface::Telnet, Spec::Listener(a)) => one(crate::legacy::spawn_telnet(s, *a).await?),
            (Surface::Finger, Spec::Listener(a)) => one(crate::legacy::spawn_finger(s, *a).await?),
            (Surface::Http, Spec::Http { addr, web_root }) => {
                one(crate::http::spawn_http(s, *addr, web_root.clone()).await?)
            }
            (Surface::Nntp, Spec::Listener(a)) => {
                one(crate::nntp::spawn_nntp(s, *a, self.tls.clone()).await?)
            }
            (Surface::NntpTls, Spec::Listener(a)) => {
                one(crate::nntp::spawn_nntps(s, *a, self.tls.clone()).await?)
            }
            (Surface::NntpFeed, Spec::Listener(a)) => {
                one(crate::nntp_feed::spawn_nntp_feed(s, *a, self.tls.clone()).await?)
            }
            (Surface::NntpFeedTls, Spec::Listener(a)) => {
                one(crate::nntp_feed::spawn_nntp_feed_tls(s, *a, self.tls.clone()).await?)
            }
            (Surface::Radio, Spec::Listener(a)) => {
                let (bound, listener) = crate::radio::spawn_radio(s, *a).await?;
                // What clients are told to tune in to: the port that actually
                // bound, which is not the configured one when that was 0.
                shared.radio.set_listen_port(bound.port());
                let mut handles = vec![listener];
                // With somewhere to tune in, a library station is more than a
                // list of titles: each gets a pump that plays its rotation.
                for slug in shared.radio.program_slugs() {
                    if shared.radio.track_count(&slug) > 0 {
                        tracing::info!(mount = %slug, "radio library station streaming");
                        handles.push(crate::radio::spawn_program_pump(shared.clone(), slug));
                    }
                }
                (SurfaceState::Listening(bound), handles)
            }
            (Surface::RadioSource, Spec::Listener(a)) => {
                one(crate::radio::spawn_radio_source(s, *a).await?)
            }
            (Surface::Hotline, Spec::Listener(a)) => {
                one(crate::hotline::spawn_hotline(s, *a).await?)
            }
            (
                Surface::Ftn,
                Spec::Ftn {
                    addr,
                    inbound,
                    outbound,
                    ..
                },
            ) => one(crate::ftn::spawn_ftn(s, *addr, inbound.clone(), outbound.clone()).await?),
            (Surface::Syndication, Spec::Syndication { .. }) => (
                SurfaceState::Running,
                vec![crate::syndication::spawn_syndication(
                    s,
                    self.data_dir.join("syndication"),
                )],
            ),
            (surface, spec) => anyhow::bail!("{surface:?} cannot be started as {spec:?}"),
        })
    }

    /// Undo whatever starting `surface` did besides spawning tasks.
    fn stopped(&self, shared: &Arc<Shared>, surface: Surface) {
        if surface == Surface::Radio {
            shared.radio.set_listen_port(0);
            shared.radio.retire_program_mounts();
        }
    }

    /// Stop every surface (shutdown).
    pub async fn stop_all(&self, shared: &Arc<Shared>) {
        let mut running = self.running.lock().await;
        for (surface, run) in running.drain() {
            for h in run.handles {
                h.abort();
            }
            self.stopped(shared, surface);
        }
        self.states.lock().clear();
    }
}

/// Make what is running match what the live config asks for. Returns the
/// surfaces whose state changed, for the log and for tests.
pub async fn reconcile(shared: &Arc<Shared>) -> Vec<Surface> {
    let surfaces = &shared.surfaces;
    let config = shared.config.read();
    let mut running = surfaces.running.lock().await;
    let mut changed = Vec::new();

    for surface in Surface::ALL {
        let wanted = surfaces.wanted(surface, &config);
        let current = running.get(&surface).map(|r| &r.spec);
        // A surface that failed to start is retried whenever anything changes:
        // the port it wanted may have come free.
        let failed = matches!(surfaces.state(surface), SurfaceState::Failed(_));
        if current == wanted.as_ref() && !failed {
            continue;
        }

        if let Some(run) = running.remove(&surface) {
            for h in &run.handles {
                h.abort();
            }
            // Wait for the tasks to be gone: the listener is dropped with its
            // task, and the same port may be wanted again a line from now.
            for h in run.handles {
                let _ = h.await;
            }
            surfaces.stopped(shared, surface);
            tracing::info!(?surface, "surface stopped");
        }

        let state = match &wanted {
            None => {
                if surface == Surface::Syndication && config.syndication_enabled {
                    SurfaceState::Idle("no feeds are mapped in burrow.toml")
                } else {
                    SurfaceState::Off
                }
            }
            Some(spec) => match surfaces.start(shared, surface, spec).await {
                Ok((state, handles)) => {
                    tracing::info!(?surface, ?state, "surface started");
                    running.insert(
                        surface,
                        Running {
                            spec: spec.clone(),
                            handles,
                        },
                    );
                    state
                }
                Err(e) => {
                    tracing::error!(?surface, "surface could not start: {e:#}");
                    SurfaceState::Failed(format!("{e:#}"))
                }
            },
        };
        if surfaces.state(surface) != state {
            changed.push(surface);
        }
        surfaces.states.lock().insert(surface, state);
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn library_pumps_are_started_once_and_stop_with_the_radio() {
        let dir = tempfile::tempdir().unwrap();
        let burrow = crate::Burrow::start(ServerConfig {
            quic_addr: "127.0.0.1:0".parse().unwrap(),
            ws_addr: "127.0.0.1:0".parse().unwrap(),
            radio_addr: "127.0.0.1:0".parse().unwrap(),
            data_dir: dir.path().into(),
            ..ServerConfig::default()
        })
        .await
        .unwrap();
        let shared = &burrow.shared;
        shared.radio.install_program(
            "later",
            "Later",
            "music",
            vec![rabbithole_radio::Track::new(
                rabbithole_radio::TrackId(1),
                "one.mp3",
                "",
                1_000,
                rabbithole_radio::BlobId::ZERO,
            )],
            Some(crate::radio::Sound::Mpeg),
        );
        for _ in 0..4 {
            shared.config.set_key("radio_enabled", "true").unwrap();
            // The refresh and the operator can both start this station. Their
            // order must not leave duplicate pumps or mount entries.
            tokio::join!(
                reconcile(shared),
                shared.surfaces.ensure_radio_pump(shared, "later"),
                shared.surfaces.ensure_radio_pump(shared, "later"),
            );
            assert_eq!(
                shared.surfaces.running.lock().await[&Surface::Radio]
                    .handles
                    .len(),
                2,
                "one listener and one pump"
            );
            assert!(shared.radio.is_pumped("later"));
            assert!(shared.radio.is_streaming("later"));

            shared.config.set_key("radio_enabled", "false").unwrap();
            tokio::join!(
                shared.surfaces.ensure_radio_pump(shared, "later"),
                reconcile(shared),
                shared.surfaces.ensure_radio_pump(shared, "later"),
            );
            assert!(!shared.radio.is_pumped("later"));
            assert!(
                !shared.radio.is_streaming("later"),
                "no orphan mount after stop"
            );
            assert!(!shared
                .surfaces
                .running
                .lock()
                .await
                .contains_key(&Surface::Radio));
        }
        burrow.shutdown().await;
    }
}
