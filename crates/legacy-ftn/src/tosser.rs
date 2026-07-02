//! Inbound mail **tosser**: split a decoded packet into individual messages,
//! classify each as echomail or netmail, drop MSGID duplicates, and surface the
//! SEEN-BY / PATH loop-control lines in a structured form.
//!
//! A tosser is the inbound half of an FTN mail pipeline. It consumes packets
//! that a mailer has received and decides, per message, where each belongs:
//!
//! ```text
//!   .PKT bundle ──▶ [ tosser ] ──▶ echomail  (has an AREA: line; SEEN-BY/PATH)
//!                                └▶ netmail   (no AREA:; explicit dest node)
//!                                └▶ duplicates (MSGID already seen)
//! ```
//!
//! Classification follows FTS-0004: a message is **echomail** iff its body
//! begins with an `AREA:` line; everything else is **netmail**, routed to the
//! destination node named in the packed-message header (refined by an `INTL`
//! kludge when present, so 5D netmail addresses survive).
//!
//! **Dupe detection** keys on the `MSGID` kludge (FTS-0009): a rolling set of
//! seen ids is kept on the [`Tosser`], so a message whose MSGID was already
//! tossed — in this bundle or an earlier one — is diverted to
//! [`TossedBatch::duplicates`] instead of being filed again. Messages with no
//! MSGID are never treated as duplicates here (a body-hash fallback belongs to a
//! later slice).
//!
//! Everything is pure: [`Tosser::toss`] operates on an already-decoded
//! [`Packet`], and [`Tosser::toss_bytes`] layers packet decoding on top. No
//! filesystem, no clock, no network.

use std::collections::HashSet;

use crate::address::FtnAddress;
use crate::error::FtnError;
use crate::kludge::Message;
use crate::message::PackedMessage;
use crate::packet::{Packet, PacketHeader};

/// One tossed echomail message together with its parsed control lines.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EchoMail {
    /// Echo area tag from the `AREA:` line (e.g. `R20.GENERAL`).
    pub area: String,
    /// `MSGID` value, if the message carried one.
    pub msgid: Option<String>,
    /// The original packed record.
    pub message: PackedMessage,
    /// The body parsed into control lines + visible text.
    pub parsed: Message,
}

impl EchoMail {
    /// Expand the `SEEN-BY:` lines into concrete `(net, node)` pairs, resolving
    /// the 2D "same net" compression (see [`parse_2d_list`]).
    pub fn seen_by_nodes(&self) -> Vec<(u16, u16)> {
        expand_lists(&self.parsed.seen_by)
    }

    /// Expand the `PATH:` lines into concrete `(net, node)` pairs.
    pub fn path_nodes(&self) -> Vec<(u16, u16)> {
        expand_lists(&self.parsed.path)
    }
}

/// One tossed netmail message together with its resolved addresses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetMail {
    /// Resolved origin address.
    pub orig: FtnAddress,
    /// Resolved destination address.
    pub dest: FtnAddress,
    /// `MSGID` value, if the message carried one.
    pub msgid: Option<String>,
    /// The original packed record.
    pub message: PackedMessage,
    /// The body parsed into control lines + visible text.
    pub parsed: Message,
}

/// The result of tossing one packet: messages split by class, plus the ids of
/// any records rejected as duplicates.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TossedBatch {
    /// Echomail messages, in packet order.
    pub echomail: Vec<EchoMail>,
    /// Netmail messages, in packet order.
    pub netmail: Vec<NetMail>,
    /// MSGID values of records dropped as duplicates, in packet order.
    pub duplicates: Vec<String>,
}

/// Stateful inbound tosser holding the rolling MSGID dupe set.
///
/// Reuse a single `Tosser` across many packets so duplicates that arrive in
/// separate bundles are still caught.
#[derive(Debug, Clone, Default)]
pub struct Tosser {
    seen_msgids: HashSet<String>,
}

impl Tosser {
    /// A fresh tosser with an empty dupe set.
    pub fn new() -> Self {
        Tosser::default()
    }

    /// A tosser primed with previously-seen MSGIDs (e.g. loaded from a dupe
    /// database), so those ids are rejected on first sight.
    pub fn with_known_msgids<I, S>(ids: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Tosser {
            seen_msgids: ids.into_iter().map(Into::into).collect(),
        }
    }

    /// True if `msgid` has already been tossed through this instance.
    pub fn is_known(&self, msgid: &str) -> bool {
        self.seen_msgids.contains(msgid)
    }

    /// Number of distinct MSGIDs remembered so far.
    pub fn known_count(&self) -> usize {
        self.seen_msgids.len()
    }

    /// Toss one already-decoded packet.
    ///
    /// Each message is classified and MSGID-deduped; the dupe set is updated in
    /// place. Never panics.
    pub fn toss(&mut self, packet: &Packet) -> TossedBatch {
        let mut batch = TossedBatch::default();
        for message in &packet.messages {
            let parsed = message.parse_body();
            let msgid = parsed.msgid().map(str::to_string);

            // Dupe check: only meaningful when a MSGID is present. `insert`
            // returns false when the id was already in the set.
            if let Some(id) = &msgid {
                if !self.seen_msgids.insert(id.clone()) {
                    batch.duplicates.push(id.clone());
                    continue;
                }
            }

            match &parsed.area {
                Some(area) => batch.echomail.push(EchoMail {
                    area: area.clone(),
                    msgid,
                    message: message.clone(),
                    parsed,
                }),
                None => {
                    let (orig, dest) = resolve_netmail_addrs(&packet.header, message, &parsed);
                    batch.netmail.push(NetMail {
                        orig,
                        dest,
                        msgid,
                        message: message.clone(),
                        parsed,
                    });
                }
            }
        }
        batch
    }

    /// Decode a `.PKT` byte buffer and toss it. Returns the decode error on
    /// malformed input rather than panicking.
    pub fn toss_bytes(&mut self, buf: &[u8]) -> Result<TossedBatch, FtnError> {
        let packet = Packet::decode(buf)?;
        Ok(self.toss(&packet))
    }
}

/// Resolve the origin and destination addresses of a netmail record.
///
/// Zones default to the packet header's zones (the packed-message header only
/// carries net/node). An `INTL <dest> <orig>` kludge, when present and
/// parseable, overrides both — that is the canonical carrier of 5D netmail
/// zones per FTS-0001 / FSC-0004.
fn resolve_netmail_addrs(
    header: &PacketHeader,
    message: &PackedMessage,
    parsed: &Message,
) -> (FtnAddress, FtnAddress) {
    let mut orig = FtnAddress::new(header.orig_zone, message.orig_net, message.orig_node, 0);
    let mut dest = FtnAddress::new(header.dest_zone, message.dest_net, message.dest_node, 0);

    if let Some(intl) = parsed.intl() {
        let mut it = intl.split_whitespace();
        if let (Some(d), Some(o)) = (it.next(), it.next()) {
            if let Ok(a) = d.parse::<FtnAddress>() {
                dest = a;
            }
            if let Ok(a) = o.parse::<FtnAddress>() {
                orig = a;
            }
        }
    }

    // Point numbers ride in FMPT (origin) / TOPT (destination) kludges.
    if let Some(p) = parsed.fmpt().and_then(|v| v.trim().parse::<u16>().ok()) {
        orig.point = p;
    }
    if let Some(p) = parsed.topt().and_then(|v| v.trim().parse::<u16>().ok()) {
        dest.point = p;
    }

    (orig, dest)
}

/// Expand a set of raw SEEN-BY/PATH line bodies into `(net, node)` pairs.
fn expand_lists(lines: &[String]) -> Vec<(u16, u16)> {
    let mut out = Vec::new();
    for line in lines {
        parse_2d_list(line, &mut out);
    }
    out
}

/// Parse one 2D compressed node list (`net/node` with bare `node` inheriting the
/// previous net) into `(net, node)` pairs, appending to `out`.
///
/// FTS-0004 SEEN-BY / PATH lines look like `280/464 465 466 104/1`, which
/// expands to `280/464 280/465 280/466 104/1`: a token without a slash reuses
/// the most recent net. Unparseable tokens are skipped rather than erroring,
/// because these lines are advisory loop-control metadata, not payload.
pub fn parse_2d_list(line: &str, out: &mut Vec<(u16, u16)>) {
    let mut cur_net: Option<u16> = None;
    for tok in line.split_whitespace() {
        match tok.split_once('/') {
            Some((net_s, node_s)) => {
                if let (Ok(net), Ok(node)) = (net_s.parse::<u16>(), node_s.parse::<u16>()) {
                    cur_net = Some(net);
                    out.push((net, node));
                }
            }
            None => {
                if let (Some(net), Ok(node)) = (cur_net, tok.parse::<u16>()) {
                    out.push((net, node));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::packet::DosDateTime;

    fn header() -> PacketHeader {
        PacketHeader {
            orig_node: 464,
            dest_node: 1,
            date_time: DosDateTime::default(),
            baud: 0,
            orig_net: 280,
            dest_net: 104,
            product_code_low: 0,
            revision_low: 0,
            password: [0; 8],
            orig_zone: 2,
            dest_zone: 1,
            plus: None,
        }
    }

    fn echo_record(msgid: &str, area: &str) -> PackedMessage {
        let mut m = PackedMessage {
            orig_node: 464,
            orig_net: 280,
            dest_node: 0,
            dest_net: 0,
            to: "All".into(),
            from: "Kevin".into(),
            subject: "hi".into(),
            ..Default::default()
        };
        let model = Message {
            area: Some(area.into()),
            kludges: vec![format!("MSGID: 2:280/464 {msgid}")],
            text: b"Hello echo".to_vec(),
            seen_by: vec!["280/464 465 104/1".into()],
            path: vec!["280/464".into()],
            ..Default::default()
        };
        m.set_body(&model);
        m
    }

    fn netmail_record(msgid: &str) -> PackedMessage {
        let mut m = PackedMessage {
            orig_node: 464,
            orig_net: 280,
            dest_node: 1,
            dest_net: 104,
            to: "Sysop".into(),
            from: "Kevin".into(),
            subject: "private".into(),
            ..Default::default()
        };
        let model = Message {
            kludges: vec![format!("MSGID: 2:280/464 {msgid}")],
            text: b"Private note".to_vec(),
            ..Default::default()
        };
        m.set_body(&model);
        m
    }

    #[test]
    fn classifies_echo_and_netmail() {
        let pkt = Packet {
            header: header(),
            messages: vec![
                echo_record("aaaa0001", "R20.GENERAL"),
                netmail_record("bbbb0002"),
            ],
        };
        let batch = Tosser::new().toss(&pkt);
        assert_eq!(batch.echomail.len(), 1);
        assert_eq!(batch.netmail.len(), 1);
        assert!(batch.duplicates.is_empty());
        assert_eq!(batch.echomail[0].area, "R20.GENERAL");
        assert_eq!(
            batch.echomail[0].msgid.as_deref(),
            Some("2:280/464 aaaa0001")
        );
        assert_eq!(batch.netmail[0].dest.to_string(), "1:104/1");
        assert_eq!(batch.netmail[0].orig.to_string(), "2:280/464");
    }

    #[test]
    fn dedupes_by_msgid_within_and_across_packets() {
        let mut tosser = Tosser::new();
        let pkt = Packet {
            header: header(),
            messages: vec![
                echo_record("dup00001", "AREA.A"),
                echo_record("dup00001", "AREA.A"), // same MSGID again
            ],
        };
        let batch = tosser.toss(&pkt);
        assert_eq!(batch.echomail.len(), 1);
        assert_eq!(batch.duplicates, vec!["2:280/464 dup00001".to_string()]);

        // A second packet with the same id is still a dupe.
        let pkt2 = Packet {
            header: header(),
            messages: vec![echo_record("dup00001", "AREA.A")],
        };
        let batch2 = tosser.toss(&pkt2);
        assert!(batch2.echomail.is_empty());
        assert_eq!(batch2.duplicates.len(), 1);
        assert_eq!(tosser.known_count(), 1);
    }

    #[test]
    fn primed_dupe_set_rejects_on_sight() {
        let mut tosser = Tosser::with_known_msgids(["2:280/464 seen0001"]);
        assert!(tosser.is_known("2:280/464 seen0001"));
        let pkt = Packet {
            header: header(),
            messages: vec![echo_record("seen0001", "AREA.A")],
        };
        let batch = tosser.toss(&pkt);
        assert!(batch.echomail.is_empty());
        assert_eq!(batch.duplicates.len(), 1);
    }

    #[test]
    fn messages_without_msgid_are_never_dupes() {
        let mut m = PackedMessage {
            dest_node: 1,
            dest_net: 104,
            ..Default::default()
        };
        m.set_body(&Message {
            text: b"no id here".to_vec(),
            ..Default::default()
        });
        let pkt = Packet {
            header: header(),
            messages: vec![m.clone(), m],
        };
        let batch = Tosser::new().toss(&pkt);
        assert_eq!(batch.netmail.len(), 2);
        assert!(batch.duplicates.is_empty());
    }

    #[test]
    fn seen_by_expands_2d_compression() {
        let pkt = Packet {
            header: header(),
            messages: vec![echo_record("cccc0003", "AREA.A")],
        };
        let batch = Tosser::new().toss(&pkt);
        let seen = batch.echomail[0].seen_by_nodes();
        assert_eq!(seen, vec![(280, 464), (280, 465), (104, 1)]);
        assert_eq!(batch.echomail[0].path_nodes(), vec![(280, 464)]);
    }

    #[test]
    fn intl_kludge_overrides_netmail_zones() {
        let mut m = PackedMessage {
            orig_node: 464,
            orig_net: 280,
            dest_node: 1,
            dest_net: 1,
            ..Default::default()
        };
        m.set_body(&Message {
            kludges: vec![
                "INTL 3:633/280 2:280/464".into(),
                "FMPT 7".into(),
                "TOPT 5".into(),
            ],
            text: b"routed".to_vec(),
            ..Default::default()
        });
        let pkt = Packet {
            header: header(),
            messages: vec![m],
        };
        let batch = Tosser::new().toss(&pkt);
        let nm = &batch.netmail[0];
        assert_eq!(nm.dest.to_string(), "3:633/280.5");
        assert_eq!(nm.orig.to_string(), "2:280/464.7");
    }

    #[test]
    fn toss_bytes_decodes_then_tosses() {
        let pkt = Packet {
            header: header(),
            messages: vec![echo_record("dddd0004", "AREA.A")],
        };
        let bytes = pkt.encode();
        let batch = Tosser::new().toss_bytes(&bytes).unwrap();
        assert_eq!(batch.echomail.len(), 1);
    }

    #[test]
    fn toss_bytes_reports_error_on_junk() {
        let mut tosser = Tosser::new();
        assert!(tosser.toss_bytes(&[0xff; 4]).is_err());
        // A wide range of random buffers must never panic.
        for len in 0..64usize {
            let junk: Vec<u8> = (0..len).map(|i| (i as u8).wrapping_mul(37)).collect();
            let _ = Tosser::new().toss_bytes(&junk);
        }
    }

    #[test]
    fn parse_2d_list_skips_garbage_tokens() {
        let mut out = Vec::new();
        parse_2d_list("280/464 xx 465 999999/1 104/1", &mut out);
        // "xx" (no slash, not numeric) and "999999/1" (net overflow) are dropped.
        assert_eq!(out, vec![(280, 464), (280, 465), (104, 1)]);
    }
}
