//! Outbound mail **scanner** and Binkley-Style Outbound (BSO) file naming.
//!
//! The scanner is the mirror of the [tosser](crate::tosser): it takes the
//! outbound messages a system wants to send, groups them by destination, wraps
//! each group into a `.PKT` [`Packet`], and computes the BSO filenames a mailer
//! will scan.
//!
//! ```text
//!   outbound messages ──▶ [ scanner ] ──▶ one Packet per destination
//!                                      └▶ BSO paths (.?ut / .?lo / .pkt)
//! ```
//!
//! # Binkley-Style Outbound
//!
//! BSO encodes a destination address into filenames under an outbound
//! directory. The base name is the net and node as **lowercase 4-digit hex**,
//! concatenated (`NNNNnnnn`):
//!
//! ```text
//!   2:280/464   net 280 = 0x0118, node 464 = 0x01d0  ->  "011801d0"
//! ```
//!
//! The extension encodes the *kind* of file and the *flavor* (priority):
//!
//! ```text
//!   kind \ flavor   Normal  Hold  Direct  Crash  Immediate
//!   packet (.?ut)   out     hut   dut     cut    iut     (netmail packet)
//!   flow   (.?lo)   flo     hlo   dlo     clo    ilo     (attach/reference list)
//! ```
//!
//! Zone and point placement (FTS-5001 / the de-facto BSO layout):
//!
//! ```text
//!   same zone, plain node   outbound/011801d0.clo
//!   other zone (hex, 3dig)  outbound.003/027901d0.clo
//!   a point                 outbound/011801d0.pnt/00000007.clo
//! ```
//!
//! The raw packet content itself is a `.pkt` file (referenced by a `.flo`, or
//! for netmail the `.?ut` *is* the packet); [`bso_packet_name`] gives the plain
//! `NNNNnnnn.pkt` form.
//!
//! All functions here are pure string/`Packet` transforms — no filesystem.

use std::collections::BTreeMap;

use crate::address::FtnAddress;
use crate::message::PackedMessage;
use crate::packet::{Packet, PacketHeader};

/// Outbound flavor (send priority) that selects the BSO extension letter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Flavor {
    /// Normal / routine (`out` / `flo`).
    #[default]
    Normal,
    /// Hold for pickup (`hut` / `hlo`).
    Hold,
    /// Direct, do not route (`dut` / `dlo`).
    Direct,
    /// Crash / high priority (`cut` / `clo`).
    Crash,
    /// Immediate (`iut` / `ilo`).
    Immediate,
}

/// Which BSO file the extension names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BsoKind {
    /// A netmail packet file (`.?ut`).
    Packet,
    /// A flow / attach-reference file (`.?lo`).
    Flow,
}

impl Flavor {
    /// The three-character BSO extension for this flavor and kind, e.g.
    /// `Crash` + `Flow` -> `"clo"`, `Normal` + `Packet` -> `"out"`.
    pub fn extension(self, kind: BsoKind) -> &'static str {
        match (kind, self) {
            (BsoKind::Packet, Flavor::Normal) => "out",
            (BsoKind::Packet, Flavor::Hold) => "hut",
            (BsoKind::Packet, Flavor::Direct) => "dut",
            (BsoKind::Packet, Flavor::Crash) => "cut",
            (BsoKind::Packet, Flavor::Immediate) => "iut",
            (BsoKind::Flow, Flavor::Normal) => "flo",
            (BsoKind::Flow, Flavor::Hold) => "hlo",
            (BsoKind::Flow, Flavor::Direct) => "dlo",
            (BsoKind::Flow, Flavor::Crash) => "clo",
            (BsoKind::Flow, Flavor::Immediate) => "ilo",
        }
    }
}

/// The `NNNNnnnn` BSO base name for a net/node pair (lowercase 8-hex).
pub fn bso_basename(net: u16, node: u16) -> String {
    format!("{net:04x}{node:04x}")
}

/// The plain packet content file name, `NNNNnnnn.pkt` (lowercase hex).
pub fn bso_packet_name(net: u16, node: u16) -> String {
    format!("{}.pkt", bso_basename(net, node))
}

/// A flavored BSO file name for a plain (non-point) node, e.g.
/// `011801d0.clo`. For point and cross-zone placement use
/// [`bso_relative_path`], which adds the enclosing directories.
pub fn bso_file_name(net: u16, node: u16, kind: BsoKind, flavor: Flavor) -> String {
    format!("{}.{}", bso_basename(net, node), flavor.extension(kind))
}

/// The full relative BSO path for `addr`, rooted at `outbound_base` (typically
/// `"outbound"`), given the system's `default_zone`.
///
/// - Same zone → `{base}/{NNNNnnnn}.{ext}`.
/// - Other zone → `{base}.{zzz}/{NNNNnnnn}.{ext}` (`zzz` = zone in 3-digit hex).
/// - Point → the file lives in a `{NNNNnnnn}.pnt/` directory and is named by the
///   point number in 8-digit hex, e.g. `.../011801d0.pnt/00000007.clo`.
///
/// Path separators are always `/` (BSO is defined in terms of DOS/Unix
/// forward-slash relative paths; the caller maps to a real filesystem).
pub fn bso_relative_path(
    outbound_base: &str,
    default_zone: u16,
    addr: &FtnAddress,
    kind: BsoKind,
    flavor: Flavor,
) -> String {
    let dir = if addr.zone == default_zone {
        outbound_base.to_string()
    } else {
        format!("{outbound_base}.{:03x}", addr.zone)
    };
    let ext = flavor.extension(kind);
    if addr.point == 0 {
        format!("{dir}/{}.{ext}", bso_basename(addr.net, addr.node))
    } else {
        format!(
            "{dir}/{}.pnt/{:08x}.{ext}",
            bso_basename(addr.net, addr.node),
            addr.point
        )
    }
}

/// Group packed messages by echo area tag.
///
/// The key is the message's `AREA:` tag (echomail); netmail — any message with
/// no `AREA:` line — is collected under the `None` key. Insertion order within
/// each group is preserved. A [`BTreeMap`] keeps the output deterministic.
pub fn group_by_area(messages: &[PackedMessage]) -> BTreeMap<Option<String>, Vec<PackedMessage>> {
    let mut groups: BTreeMap<Option<String>, Vec<PackedMessage>> = BTreeMap::new();
    for m in messages {
        let area = m.parse_body().area;
        groups.entry(area).or_default().push(m.clone());
    }
    groups
}

/// A scanned outbound bundle: a destination plus the packet built for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScannedBundle {
    /// The destination this packet is queued for.
    pub dest: FtnAddress,
    /// The assembled packet.
    pub packet: Packet,
}

impl ScannedBundle {
    /// Encode the packet to `.PKT` bytes.
    pub fn encode(&self) -> Vec<u8> {
        self.packet.encode()
    }

    /// The plain `NNNNnnnn.pkt` content name for this bundle's destination.
    pub fn packet_name(&self) -> String {
        bso_packet_name(self.dest.net, self.dest.node)
    }

    /// The flavored BSO path for this bundle's destination.
    pub fn bso_path(&self, outbound_base: &str, default_zone: u16, flavor: Flavor) -> String {
        bso_relative_path(
            outbound_base,
            default_zone,
            &self.dest,
            BsoKind::Packet,
            flavor,
        )
    }
}

/// Build a single [`Packet`] for `dest` from `messages`, stamping the header's
/// origin/destination node/net/zone fields from `template` and the two
/// addresses.
///
/// The scanner leaves message *content* untouched (SEEN-BY/PATH rewriting is the
/// caller's job, done before scanning); it only frames the packet. Use
/// [`scan`] to route many messages into per-destination bundles at once.
pub fn build_packet(
    template: &PacketHeader,
    orig: &FtnAddress,
    dest: &FtnAddress,
    messages: Vec<PackedMessage>,
) -> Packet {
    let mut header = template.clone();
    header.orig_zone = orig.zone;
    header.orig_net = orig.net;
    header.orig_node = orig.node;
    header.dest_zone = dest.zone;
    header.dest_net = dest.net;
    header.dest_node = dest.node;
    Packet { header, messages }
}

/// Route `(dest, message)` pairs into one [`ScannedBundle`] per destination.
///
/// Messages are grouped by destination address (order-preserving within each
/// group), and each group becomes a packet framed from `orig` + `template`.
/// Output bundles are ordered by destination address for determinism.
pub fn scan(
    template: &PacketHeader,
    orig: &FtnAddress,
    routed: impl IntoIterator<Item = (FtnAddress, PackedMessage)>,
) -> Vec<ScannedBundle> {
    // BTreeMap keyed by the address tuple keeps output deterministic without
    // requiring FtnAddress: Ord on the public type.
    type Group = (FtnAddress, Vec<PackedMessage>);
    let mut groups: BTreeMap<(u16, u16, u16, u16), Group> = BTreeMap::new();
    for (dest, msg) in routed {
        let key = (dest.zone, dest.net, dest.node, dest.point);
        groups
            .entry(key)
            .or_insert_with(|| (dest, Vec::new()))
            .1
            .push(msg);
    }
    groups
        .into_values()
        .map(|(dest, messages)| ScannedBundle {
            packet: build_packet(template, orig, &dest, messages),
            dest,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kludge::Message;

    fn template() -> PacketHeader {
        PacketHeader {
            baud: 9600,
            product_code_low: 0xFE,
            revision_low: 1,
            password: *b"secret\0\0",
            ..Default::default()
        }
    }

    fn msg(area: Option<&str>, subject: &str) -> PackedMessage {
        let mut m = PackedMessage {
            from: "Kevin".into(),
            to: "All".into(),
            subject: subject.into(),
            ..Default::default()
        };
        m.set_body(&Message {
            area: area.map(str::to_string),
            text: b"body".to_vec(),
            ..Default::default()
        });
        m
    }

    #[test]
    fn basename_is_lowercase_8_hex() {
        assert_eq!(bso_basename(280, 464), "011801d0");
        assert_eq!(bso_basename(0xFFFF, 0), "ffff0000");
        assert_eq!(bso_packet_name(280, 464), "011801d0.pkt");
    }

    #[test]
    fn flavor_extensions_cover_the_matrix() {
        assert_eq!(Flavor::Normal.extension(BsoKind::Packet), "out");
        assert_eq!(Flavor::Crash.extension(BsoKind::Packet), "cut");
        assert_eq!(Flavor::Hold.extension(BsoKind::Packet), "hut");
        assert_eq!(Flavor::Direct.extension(BsoKind::Packet), "dut");
        assert_eq!(Flavor::Immediate.extension(BsoKind::Packet), "iut");
        assert_eq!(Flavor::Normal.extension(BsoKind::Flow), "flo");
        assert_eq!(Flavor::Crash.extension(BsoKind::Flow), "clo");
        assert_eq!(Flavor::Hold.extension(BsoKind::Flow), "hlo");
        assert_eq!(Flavor::Direct.extension(BsoKind::Flow), "dlo");
        assert_eq!(Flavor::Immediate.extension(BsoKind::Flow), "ilo");
        assert_eq!(
            bso_file_name(280, 464, BsoKind::Flow, Flavor::Crash),
            "011801d0.clo"
        );
    }

    #[test]
    fn relative_path_same_zone() {
        let a = FtnAddress::new(2, 280, 464, 0);
        assert_eq!(
            bso_relative_path("outbound", 2, &a, BsoKind::Flow, Flavor::Crash),
            "outbound/011801d0.clo"
        );
    }

    #[test]
    fn relative_path_other_zone() {
        let a = FtnAddress::new(3, 633, 464, 0);
        assert_eq!(
            bso_relative_path("outbound", 2, &a, BsoKind::Packet, Flavor::Normal),
            "outbound.003/027901d0.out"
        );
    }

    #[test]
    fn relative_path_point_uses_pnt_dir() {
        let a = FtnAddress::new(2, 280, 464, 7);
        assert_eq!(
            bso_relative_path("outbound", 2, &a, BsoKind::Flow, Flavor::Hold),
            "outbound/011801d0.pnt/00000007.hlo"
        );
    }

    #[test]
    fn group_by_area_splits_echo_and_netmail() {
        let msgs = vec![
            msg(Some("AREA.A"), "a1"),
            msg(None, "n1"),
            msg(Some("AREA.A"), "a2"),
            msg(Some("AREA.B"), "b1"),
        ];
        let groups = group_by_area(&msgs);
        assert_eq!(groups[&None].len(), 1);
        assert_eq!(groups[&Some("AREA.A".to_string())].len(), 2);
        assert_eq!(groups[&Some("AREA.B".to_string())].len(), 1);
    }

    #[test]
    fn scan_groups_by_destination_and_frames_headers() {
        let orig = FtnAddress::new(2, 280, 464, 0);
        let up = FtnAddress::new(2, 280, 1, 0);
        let other = FtnAddress::new(2, 5000, 1, 0);
        let routed = vec![
            (up, msg(Some("AREA.A"), "a1")),
            (other, msg(Some("AREA.A"), "a2")),
            (up, msg(Some("AREA.B"), "b1")),
        ];
        let bundles = scan(&template(), &orig, routed);
        assert_eq!(bundles.len(), 2);
        // Ordered by (zone,net,node,point): net 280 before net 5000.
        assert_eq!(bundles[0].dest, up);
        assert_eq!(bundles[0].packet.messages.len(), 2);
        assert_eq!(bundles[0].packet.header.orig_net, 280);
        assert_eq!(bundles[0].packet.header.dest_net, 280);
        assert_eq!(bundles[0].packet.header.dest_node, 1);
        // The framed packet round-trips through the codec.
        let bytes = bundles[0].encode();
        assert_eq!(Packet::decode(&bytes).unwrap(), bundles[0].packet);
        assert_eq!(bundles[1].dest, other);
        assert_eq!(bundles[1].packet.messages.len(), 1);
        assert_eq!(bundles[0].packet_name(), "01180001.pkt");
        assert_eq!(
            bundles[1].bso_path("outbound", 2, Flavor::Normal),
            "outbound/13880001.out"
        );
    }

    #[test]
    fn scan_empty_input_is_empty() {
        let orig = FtnAddress::default();
        assert!(scan(&template(), &orig, std::iter::empty()).is_empty());
    }
}
