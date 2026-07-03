//! High-level outbound QWK packet builder for CLI / web export.
//!
//! The per-member encoders already live in this crate ([`MessagesDat`],
//! [`ControlDat`], [`ndx`], [`DoorId`]); this module is the thin convenience that
//! wires them together into the full set of files a `.QWK` download is made of,
//! without duplicating any of the byte-level logic. It is pure and sans-I/O: it
//! returns the member bytes in memory, leaving the ZIP-bundling seam (the actual
//! `.QWK` archive) to the delivery layer — exactly the boundary the crate root
//! documents.
//!
//! [`build_packet`] takes the outbound [`QwkMessage`]s plus a [`ControlDat`]
//! carrying the BBS identity and conference metadata, and produces a
//! [`QwkPacket`]:
//!
//! - `MESSAGES.DAT` — every message, via [`MessagesDat::encode`].
//! - `CONTROL.DAT` — the manifest, with `total_messages` recomputed to match.
//! - `NNN.NDX` — one per conference that carries messages, each entry pointing at
//!   the 1-based `MESSAGES.DAT` block of a message header (via [`ndx`]/[`mbf`]).
//! - `DOOR.ID` — a QWKE-advertising door id (caller-supplied or a default).
//!
//! [`mbf`]: crate::mbf

use std::collections::BTreeMap;

use crate::control::ControlDat;
use crate::messages::{block_len, MessagesDat};
use crate::model::QwkMessage;
use crate::ndx::{self, NdxRecord};
use crate::qwke::DoorId;

/// One named index member of a packet (e.g. `005.NDX`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NdxFile {
    /// On-disk filename, conference number zero-padded to three digits.
    pub filename: String,
    /// Encoded `.NDX` bytes (5-byte records).
    pub bytes: Vec<u8>,
}

/// The assembled members of an outbound QWK packet, as in-memory byte buffers.
///
/// These are the files a delivery layer would place into the `.QWK` ZIP; this
/// crate deliberately stops at the bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QwkPacket {
    /// `MESSAGES.DAT` bytes.
    pub messages_dat: Vec<u8>,
    /// `CONTROL.DAT` bytes.
    pub control_dat: Vec<u8>,
    /// `DOOR.ID` bytes.
    pub door_id: Vec<u8>,
    /// Per-conference `.NDX` members, sorted by conference number.
    pub indexes: Vec<NdxFile>,
}

impl QwkPacket {
    /// All members as `(canonical filename, bytes)` pairs, in a stable order
    /// (`MESSAGES.DAT`, `CONTROL.DAT`, `DOOR.ID`, then each `.NDX`).
    ///
    /// Convenient for a ZIP-bundling layer that just needs to iterate the files.
    pub fn members(&self) -> Vec<(&str, &[u8])> {
        let mut out: Vec<(&str, &[u8])> = vec![
            ("MESSAGES.DAT", &self.messages_dat),
            ("CONTROL.DAT", &self.control_dat),
            ("DOOR.ID", &self.door_id),
        ];
        for idx in &self.indexes {
            out.push((idx.filename.as_str(), &idx.bytes));
        }
        out
    }

    /// Bundle the members into the delivered `.QWK` file: a STORE-method ZIP
    /// (see [`crate::zip`]). Deterministic — the same packet yields identical
    /// bytes.
    pub fn to_zip(&self) -> Vec<u8> {
        crate::zip::zip_store(&self.members())
    }
}

/// Build the full set of outbound QWK packet members.
///
/// `control` supplies the BBS identity and the conference list; its
/// `total_messages` field is overwritten to match `messages.len()` so the
/// manifest is always self-consistent. `door` is the QWKE `DOOR.ID` to advertise;
/// when `None`, a default QWKE door id naming the BBS is generated.
///
/// This is pure and total — it never performs I/O and never panics.
pub fn build_packet(
    mut control: ControlDat,
    messages: Vec<QwkMessage>,
    door: Option<DoorId>,
) -> QwkPacket {
    // Build per-conference indexes while we still have the message list to walk.
    // Block 1 is the producer header; the first message header is block 2.
    let mut by_conf: BTreeMap<u16, Vec<NdxRecord>> = BTreeMap::new();
    let mut block: u32 = 2;
    for msg in &messages {
        by_conf
            .entry(msg.conference)
            .or_default()
            .push(NdxRecord::new(block, msg.conference as u8));
        block += block_len(msg) as u32;
    }
    let indexes = by_conf
        .into_iter()
        .map(|(conf, records)| NdxFile {
            filename: format!("{conf:03}.NDX"),
            bytes: ndx::encode(&records),
        })
        .collect();

    control.total_messages = messages.len() as u32;
    let control_dat = control.to_bytes();

    let door_id = door
        .unwrap_or_else(|| DoorId::qwke("RabbitHole", env!("CARGO_PKG_VERSION"), &control.bbs_name))
        .to_bytes();

    let messages_dat = MessagesDat::new(messages).encode();

    QwkPacket {
        messages_dat,
        control_dat,
        door_id,
        indexes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mbf;
    use crate::messages::BLOCK;

    fn control() -> ControlDat {
        ControlDat {
            bbs_name: "RabbitHole BBS".into(),
            city_state: "Portland, OR".into(),
            phone: "503-555-0100".into(),
            sysop: "KEVIN".into(),
            serial: "12345".into(),
            bbs_id: "RABBIT".into(),
            date: "07-02-2026,13:45:00".into(),
            username: "KEVIN".into(),
            total_messages: 0,
            conferences: vec![(0, "Main".into()), (1, "Chat".into()), (5, "Rust".into())],
            files: vec!["WELCOME".into()],
        }
    }

    fn messages() -> Vec<QwkMessage> {
        vec![
            QwkMessage::new(0, 1, "ALL", "KEVIN", "First", "hello world"),
            // A long body forcing a second body block, to exercise the pointer math.
            QwkMessage::new(5, 2, "SYSOP", "KEVIN", "Second", "y".repeat(BLOCK + 5)),
            QwkMessage::new(0, 3, "ALL", "KEVIN", "Third", "back in conf 0"),
        ]
    }

    #[test]
    fn control_total_messages_is_recomputed() {
        let packet = build_packet(control(), messages(), None);
        let back = ControlDat::parse(&packet.control_dat).unwrap();
        assert_eq!(back.total_messages, 3);
        assert_eq!(back.conferences.len(), 3);
    }

    #[test]
    fn messages_dat_round_trips_through_builder() {
        let msgs = messages();
        let packet = build_packet(control(), msgs.clone(), None);
        let back = MessagesDat::decode(&packet.messages_dat).unwrap();
        assert_eq!(back.messages, msgs);
    }

    #[test]
    fn one_index_per_conference_with_messages() {
        let packet = build_packet(control(), messages(), None);
        // Conferences 0 and 5 carry messages; conference 1 does not.
        let names: Vec<&str> = packet.indexes.iter().map(|i| i.filename.as_str()).collect();
        assert_eq!(names, vec!["000.NDX", "005.NDX"]);
    }

    #[test]
    fn index_pointers_land_on_message_headers() {
        let msgs = messages();
        let packet = build_packet(control(), msgs.clone(), None);

        // Recompute the expected 1-based block of each message header.
        let mut expected: Vec<(u16, u32)> = Vec::new();
        let mut block = 2u32;
        for m in &msgs {
            expected.push((m.conference, block));
            block += block_len(m) as u32;
        }

        // Conference 0 index: messages #1 (block 2) and #3.
        let conf0 = packet
            .indexes
            .iter()
            .find(|i| i.filename == "000.NDX")
            .unwrap();
        let recs = ndx::decode(&conf0.bytes).unwrap();
        let want0: Vec<u32> = expected
            .iter()
            .filter(|(c, _)| *c == 0)
            .map(|(_, b)| *b)
            .collect();
        let got0: Vec<u32> = recs.iter().map(|r| r.number).collect();
        assert_eq!(got0, want0);

        // Each pointer must address a real 128-byte header block in MESSAGES.DAT.
        for r in &recs {
            let off = (r.number as usize - 1) * BLOCK;
            assert!(off + BLOCK <= packet.messages_dat.len());
        }

        // Sanity check the MBF encoding of the first pointer.
        assert_eq!(&conf0.bytes[0..4], &mbf::encode(2));
    }

    #[test]
    fn default_door_id_advertises_qwke() {
        let packet = build_packet(control(), messages(), None);
        let door = DoorId::parse_bytes(&packet.door_id);
        assert!(door.advertises_qwke());
    }

    #[test]
    fn caller_supplied_door_id_is_used() {
        let mut door = DoorId::new();
        door.set("DOOR", "CustomDoor");
        let packet = build_packet(control(), messages(), Some(door));
        let back = DoorId::parse_bytes(&packet.door_id);
        assert_eq!(back.get("DOOR"), Some("CustomDoor"));
    }

    #[test]
    fn empty_message_list_produces_no_indexes() {
        let packet = build_packet(control(), Vec::new(), None);
        assert!(packet.indexes.is_empty());
        let back = ControlDat::parse(&packet.control_dat).unwrap();
        assert_eq!(back.total_messages, 0);
        // MESSAGES.DAT is just the producer block.
        assert_eq!(packet.messages_dat.len(), BLOCK);
    }

    #[test]
    fn members_lists_all_files_in_stable_order() {
        let packet = build_packet(control(), messages(), None);
        let names: Vec<&str> = packet.members().iter().map(|(n, _)| *n).collect();
        assert_eq!(
            names,
            vec![
                "MESSAGES.DAT",
                "CONTROL.DAT",
                "DOOR.ID",
                "000.NDX",
                "005.NDX"
            ]
        );
    }
}
