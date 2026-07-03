//! # Consumer-router port mapping (`rabbithole-portmap`)
//!
//! A self-hosted burrow usually sits behind a consumer NAT router, so its
//! QUIC (UDP) and WebSocket (TCP) listeners are unreachable from the public
//! internet until the operator sets up a static port-forward by hand. This
//! crate lets the burrow *best-effort* ask the router to open those ports for
//! it, using the three protocols consumer routers actually speak:
//!
//! - **NAT-PMP** (RFC 6886) — Apple's simple binary UDP protocol on gateway
//!   port 5351. See [`natpmp`].
//! - **PCP** (RFC 6887) — NAT-PMP's IETF successor, same UDP port 5351, a
//!   richer 24-byte-header binary format. See [`pcp`].
//! - **UPnP-IGD** — the SOAP-over-HTTP control protocol discovered over SSDP
//!   multicast, spoken by the majority of home routers. See [`upnp`].
//!
//! ## Layering: pure codecs vs. the live mapper
//!
//! Every wire format lives in a *pure, sans-IO* module ([`natpmp`], [`pcp`],
//! [`upnp`]): builders that produce `Vec<u8>`/`String` request bytes and
//! **total** decoders that turn arbitrary, hostile, or truncated bytes into a
//! typed value or a structured error — never a panic. Those modules touch no
//! socket and no clock, so they are exhaustively golden-file and
//! totality-tested.
//!
//! The [`mapper`] module layers a small `tokio` [`PortMapper`] on top: given a
//! gateway IP (supplied by the operator — this crate deliberately pulls in *no*
//! routing-table dependency to auto-discover it), it drives NAT-PMP then PCP
//! over UDP with bounded timeouts and a couple of retries, and resolves
//! cleanly to [`mapper::MapOutcome`] within a small time budget. A missing or
//! silent gateway degrades gracefully to [`mapper::MapOutcome::NoGateway`]; it
//! never hangs and never panics.
//!
//! ## What is *not* here (deferred)
//!
//! This crate is the port-mapping half of the NAT-traversal item. The other
//! halves stay deferred and are documented as follow-ups:
//!
//! - **Direct-connection hole punching** (STUN-style simultaneous-open) — not
//!   implemented.
//! - **Relay fallback** (a TURN-like bounce when neither mapping nor punching
//!   works) — not implemented.
//! - **Routing-table gateway auto-discovery** — the operator supplies the
//!   router LAN IP; we do not read the OS routing table.
//! - **Live UPnP discovery+control** — the SSDP `M-SEARCH` datagram, the SOAP
//!   action bodies, and a tolerant response parser are all provided as pure
//!   codecs, but the live control-URL discovery (fetching the device
//!   descriptor XML from the SSDP `LOCATION` header and POSTing the SOAP) is
//!   left to a follow-up; [`PortMapper`] speaks only NAT-PMP and PCP.

#![forbid(unsafe_code)]

pub mod mapper;
pub mod natpmp;
pub mod pcp;
pub mod upnp;

pub use mapper::{MapOutcome, Method, PortMapper};

/// The transport protocol a mapping applies to.
///
/// The wire encodings differ per protocol family, so each codec maps this to
/// its own on-wire value:
/// - NAT-PMP: a distinct *opcode* (1 = UDP, 2 = TCP).
/// - PCP: the IANA internet-protocol number ([`Protocol::iana`]).
/// - UPnP: the literal string `"UDP"` / `"TCP"` ([`Protocol::upnp_str`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Protocol {
    /// UDP — the QUIC transport rides on this.
    Udp,
    /// TCP — the WebSocket transport rides on this.
    Tcp,
}

impl Protocol {
    /// The IANA internet-protocol number: TCP = 6, UDP = 17. Used by PCP,
    /// whose MAP payload carries the protocol as this byte (RFC 6887 §11.1).
    pub const fn iana(self) -> u8 {
        match self {
            Protocol::Tcp => 6,
            Protocol::Udp => 17,
        }
    }

    /// The UPnP `NewProtocol` string.
    pub const fn upnp_str(self) -> &'static str {
        match self {
            Protocol::Tcp => "TCP",
            Protocol::Udp => "UDP",
        }
    }
}
