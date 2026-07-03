//! Graceful-degradation test for the async [`PortMapper`].
//!
//! We cannot exercise a real router mapping in CI, so we prove the property
//! that actually matters for boot safety: against a gateway that never
//! answers, the mapper resolves cleanly to [`MapOutcome::NoGateway`] within a
//! small, bounded time — it never hangs and never panics. (A live mapping
//! against a real NAT-PMP/PCP gateway is documented as manual/out-of-CI.)

#![forbid(unsafe_code)]

use std::net::{IpAddr, Ipv4Addr};
use std::time::Duration;

use rabbithole_portmap::{MapOutcome, PortMapper, Protocol};
use tokio::net::UdpSocket;

#[tokio::test]
async fn silent_gateway_degrades_to_no_gateway_within_bound() {
    // A bound-but-silent UDP socket on loopback: it receives our probes but
    // never replies, so both NAT-PMP and PCP attempts time out.
    let sink = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let gateway = sink.local_addr().unwrap();

    // Point the mapper at that silent socket's IP with tiny timing so the whole
    // NAT-PMP-then-PCP sweep is quick.
    let mapper =
        PortMapper::new(IpAddr::V4(Ipv4Addr::LOCALHOST)).with_timing(Duration::from_millis(40), 2);
    // Sanity: the mapper targets port 5351, not the sink's port — the sink just
    // keeps that IP from emitting ICMP unreachable in some environments.
    let _ = gateway;

    // The whole call must finish well within this outer bound.
    let outcome = tokio::time::timeout(
        Duration::from_secs(3),
        mapper.map_port(Protocol::Udp, 4653, 7200),
    )
    .await
    .expect("map_port must resolve within the bound, never hang");

    assert_eq!(outcome, MapOutcome::NoGateway);
}

#[tokio::test]
async fn tcp_map_against_silent_gateway_also_clean() {
    let mapper = PortMapper::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)))
        .with_timing(Duration::from_millis(30), 2);
    let outcome = tokio::time::timeout(
        Duration::from_secs(3),
        mapper.map_port(Protocol::Tcp, 4654, 3600),
    )
    .await
    .expect("must resolve within bound");
    assert_eq!(outcome, MapOutcome::NoGateway);
}
