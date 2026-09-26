//! Inbound mail **tosser**: split a decoded packet into individual messages,
//! classify each as echomail or netmail, drop duplicates, and surface the
//! SEEN-BY / PATH loop-control lines in a structured form.
//!
//! A tosser is the inbound half of an FTN mail pipeline. It consumes packets
//! that a mailer has received and decides, per message, where each belongs:
//!
//! ```text
//!   .PKT bundle ──▶ [ tosser ] ──▶ echomail  (has an AREA: line; SEEN-BY/PATH)
//!                                └▶ netmail   (no AREA:; explicit dest node)
//!                                └▶ duplicates (message identity already seen)
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
//! [`TossedBatch::duplicates`] instead of being filed again. Without MSGID,
//! a versioned BLAKE3 fingerprint covers the original date, names, subject,
//! packed/resolved addresses and body bytes. Transit control lines (SEEN-BY, PATH, Via,
//! TID) and the final CR delimiter are ignored; no visible text is normalized.
//! Message attributes/cost, packet dates, passwords and transport metadata
//! are not message identity.
//! These sets live for the tosser's lifetime; they are not a durable dupe log.
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
    /// MSGID values, or `fallback:blake3:<hex>` fingerprints for missing IDs,
    /// of records dropped as duplicates, in packet order.
    pub duplicates: Vec<String>,
}

/// Stateful inbound tosser holding MSGID and original-message dupe sets.
///
/// Reuse a single `Tosser` across many packets so duplicates that arrive in
/// separate bundles are still caught.
#[derive(Debug, Clone, Default)]
pub struct Tosser {
    seen_msgids: HashSet<String>,
    // A distinct namespace: an arbitrary literal MSGID cannot poison fallback
    // identity, even if it spells the fingerprint's diagnostic label.
    seen_fallbacks: HashSet<[u8; 32]>,
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
            seen_fallbacks: HashSet::new(),
        }
    }

    /// True if `msgid` has already been tossed through this instance.
    pub fn is_known(&self, msgid: &str) -> bool {
        self.seen_msgids.contains(msgid)
    }

    /// Number of distinct message identities remembered so far.
    pub fn known_count(&self) -> usize {
        self.seen_msgids.len() + self.seen_fallbacks.len()
    }

    /// Toss one already-decoded packet.
    ///
    /// Each message is classified and deduped; the dupe sets are updated in
    /// place. Never panics.
    pub fn toss(&mut self, packet: &Packet) -> TossedBatch {
        let mut batch = TossedBatch::default();
        for message in &packet.messages {
            let parsed = message.parse_body();
            let msgid = parsed.msgid().map(str::to_string);

            // A present MSGID remains authoritative, regardless of content.
            // `insert` returns false when the identity was already seen.
            if let Some(id) = &msgid {
                if !self.seen_msgids.insert(id.clone()) {
                    batch.duplicates.push(id.clone());
                    continue;
                }
            } else {
                let fingerprint = fallback_identity(&packet.header, message, &parsed);
                if !self.seen_fallbacks.insert(*fingerprint.as_bytes()) {
                    batch
                        .duplicates
                        .push(format!("fallback:blake3:{}", fingerprint.to_hex()));
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

/// Hash original message data, never an import timestamp or the packet's
/// changing envelope. Length framing keeps adjacent strings/lines unambiguous.
fn fallback_identity(
    header: &PacketHeader,
    message: &PackedMessage,
    parsed: &Message,
) -> blake3::Hash {
    let mut hash = blake3::Hasher::new();
    hash.update(b"rabbithole-ftn-missing-msgid-v1\0");
    let (orig, dest) = resolve_netmail_addrs(header, message, parsed);
    for field in [
        orig.zone,
        orig.net,
        orig.node,
        orig.point,
        dest.zone,
        dest.net,
        dest.node,
        dest.point,
        message.orig_net,
        message.orig_node,
        message.dest_net,
        message.dest_node,
    ] {
        hash.update(&field.to_le_bytes());
    }
    let mut field = |bytes: &[u8]| {
        hash.update(&(bytes.len() as u64).to_le_bytes());
        hash.update(bytes);
    };
    for text in [
        &message.date_time,
        &message.to,
        &message.from,
        &message.subject,
    ] {
        field(text.as_bytes());
    }
    // Appending transport lines normally adds a CR after the final content
    // line. Treat that terminal delimiter consistently, retaining empty lines
    // inside the body and all non-transit bytes (including raw CP437).
    let body = message.body.strip_suffix(b"\r").unwrap_or(&message.body);
    for line in body
        .split(|&byte| byte == b'\r')
        .filter(|_| !body.is_empty())
    {
        if !is_transit_line(line) {
            field(line);
        }
    }
    hash.finalize()
}

fn is_transit_line(line: &[u8]) -> bool {
    let line = line.strip_prefix(b"\n").unwrap_or(line);
    if line
        .get(..8)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(b"SEEN-BY:"))
    {
        return true;
    }
    let Some(kludge) = line.strip_prefix(b"\x01") else {
        return false;
    };
    let tag = kludge
        .split(|byte| *byte == b':' || byte.is_ascii_whitespace())
        .next()
        .unwrap_or_default();
    [b"PATH".as_slice(), b"Via".as_slice(), b"TID".as_slice()]
        .iter()
        .any(|known| tag.eq_ignore_ascii_case(known))
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
    fn missing_msgid_reimports_dedupe_within_and_across_repackaged_packets() {
        let mut m = PackedMessage {
            dest_node: 1,
            dest_net: 104,
            date_time: "02 Jul 26  13:30:45".into(),
            ..Default::default()
        };
        m.set_body(&Message {
            text: b"no id here".to_vec(),
            ..Default::default()
        });
        let mut pkt = Packet {
            header: header(),
            messages: vec![m.clone(), m],
        };
        let mut tosser = Tosser::new();
        let batch = tosser.toss(&pkt);
        assert_eq!(batch.netmail.len(), 1);
        assert_eq!(batch.netmail[0].msgid, None);
        assert_eq!(batch.duplicates.len(), 1);
        assert!(batch.duplicates[0].starts_with("fallback:blake3:"));
        // Identity has no random state, import clock, or packet envelope data.
        assert_eq!(Tosser::new().toss(&pkt).duplicates, batch.duplicates);
        pkt.messages.truncate(1);
        pkt.header.date_time.year = 2027;
        pkt.header.orig_node = 777;
        pkt.header.dest_node = 888;
        pkt.header.product_code_low = 42;
        pkt.header.password = *b"repacked";
        pkt.messages[0].attribute = 0x010c; // received/sent/transit state
        pkt.messages[0].cost = 123;
        let again = tosser.toss_bytes(&pkt.encode()).unwrap();
        assert!(again.netmail.is_empty());
        assert_eq!(again.duplicates, batch.duplicates);
        assert_eq!(tosser.known_count(), 1);
    }

    #[test]
    fn missing_id_fingerprint_ignores_only_transit_lines() {
        let mut original = netmail_record("unused");
        original.body = b"\x01INTL 1:104/1 2:280/464\r\x01PID: Writer\rhi\x82\r\rthere".to_vec();
        let mut forwarded = original.clone();
        forwarded.body.extend_from_slice(
            b"\rSEEN-BY: 104/1 2\r\x01PATH: 104/1\r\x01Via 2:280/9 @date\r\x01TID: Transport\r",
        );
        let mut content_changed = forwarded.clone();
        let index = content_changed
            .body
            .iter()
            .position(|&b| b == 0x82)
            .unwrap();
        content_changed.body[index] = 0x83;
        let mut authoring_changed = original.clone();
        authoring_changed
            .body
            .extend_from_slice(b"\r\x01REPLY: different-parent\r");
        let packet = Packet {
            header: header(),
            messages: vec![original, forwarded, content_changed, authoring_changed],
        };
        let batch = Tosser::new().toss(&packet);
        assert_eq!(batch.netmail.len(), 3);
        assert_eq!(batch.duplicates.len(), 1);
    }

    #[test]
    fn missing_id_keeps_original_fields_and_routing_distinct() {
        let original = PackedMessage {
            orig_net: 280,
            orig_node: 464,
            dest_net: 104,
            dest_node: 1,
            to: "Alice".into(),
            from: "Kevin".into(),
            subject: "A note".into(),
            date_time: "02 Jul 26  13:30:45".into(),
            body: b"same content".to_vec(),
            ..Default::default()
        };
        let mut variants = vec![original.clone()];
        for changed in [
            PackedMessage {
                to: "Bob".into(),
                ..original.clone()
            },
            PackedMessage {
                from: "Another author".into(),
                ..original.clone()
            },
            PackedMessage {
                subject: "Other subject".into(),
                ..original.clone()
            },
            PackedMessage {
                date_time: "03 Jul 26  13:30:45".into(),
                ..original.clone()
            },
            PackedMessage {
                dest_node: 2,
                ..original.clone()
            },
            PackedMessage {
                body: b"\x01INTL 3:104/1 2:280/464\rsame content".to_vec(),
                ..original.clone()
            },
            PackedMessage {
                body: b"\x01TOPT 5\rsame content".to_vec(),
                ..original.clone()
            },
            PackedMessage {
                body: b"AREA:AREA.A\rsame content".to_vec(),
                ..original.clone()
            },
            PackedMessage {
                body: b"AREA:AREA.B\rsame content".to_vec(),
                ..original.clone()
            },
        ] {
            variants.push(changed);
        }
        let packet = Packet {
            header: header(),
            messages: variants,
        };
        let mut tosser = Tosser::new();
        let batch = tosser.toss(&packet);
        assert_eq!(
            batch.netmail.len() + batch.echomail.len(),
            packet.messages.len()
        );
        assert!(batch.duplicates.is_empty());
        assert_eq!(tosser.toss(&packet).duplicates.len(), packet.messages.len());

        let mut zone = packet.clone();
        zone.messages = vec![original];
        zone.header.dest_zone += 1;
        assert_eq!(tosser.toss(&zone).netmail.len(), 1);
    }

    #[test]
    fn fallback_namespace_and_field_boundaries_do_not_collide() {
        let mut message = PackedMessage {
            from: "ab".into(),
            subject: "c".into(),
            ..Default::default()
        };
        let fingerprint = fallback_identity(&header(), &message, &message.parse_body());
        let label = format!("fallback:blake3:{}", fingerprint.to_hex());
        let mut tosser = Tosser::with_known_msgids([label.clone()]);
        let packet = Packet {
            header: header(),
            messages: vec![message.clone()],
        };
        assert_eq!(tosser.toss(&packet).netmail.len(), 1);
        assert_eq!(tosser.toss(&packet).duplicates, vec![label]);
        message.from = "a".into();
        message.subject = "bc".into();
        assert_eq!(
            tosser
                .toss(&Packet {
                    header: header(),
                    messages: vec![message]
                })
                .netmail
                .len(),
            1
        );
    }

    #[test]
    fn explicit_msgid_stays_authoritative_over_changed_content_and_destination() {
        let mut original = echo_record("authoritative", "AREA.A");
        let mut changed = echo_record("authoritative", "AREA.B");
        changed.dest_node += 1;
        changed.body.extend_from_slice(b"changed text\r");
        let mut tosser = Tosser::new();
        let batch = tosser.toss(&Packet {
            header: header(),
            messages: vec![original.clone(), changed],
        });
        assert_eq!(batch.echomail.len(), 1);
        assert_eq!(batch.duplicates, vec!["2:280/464 authoritative"]);
        // A present but empty MSGID keeps the pre-existing MSGID behavior too.
        original.set_body(&Message {
            kludges: vec!["MSGID:".into()],
            ..Default::default()
        });
        let batch = tosser.toss(&Packet {
            header: header(),
            messages: vec![original.clone(), original],
        });
        assert_eq!(batch.netmail.len(), 1);
        assert_eq!(batch.duplicates, vec![""]);
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
