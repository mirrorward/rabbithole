//! End-to-end packet round-trip and total-decoding (never-panic) tests.

use rabbithole_legacy_ftn::{DosDateTime, Message, PackedMessage, Packet, PacketHeader, Type2Plus};

fn build_packet() -> Packet {
    let header = PacketHeader {
        orig_node: 464,
        dest_node: 1,
        date_time: DosDateTime {
            year: 2026,
            month: 6,
            day: 2,
            hour: 13,
            minute: 30,
            second: 45,
        },
        baud: 9600,
        orig_net: 0xFFFF,
        dest_net: 104,
        product_code_low: 0xFE,
        revision_low: 1,
        password: *b"pass\0\0\0\0",
        orig_zone: 2,
        dest_zone: 1,
        plus: Some(Type2Plus {
            aux_net: 280,
            capability: 0x0001,
            product_code_high: 0xFD,
            revision_high: 2,
            orig_zone: 2,
            dest_zone: 1,
            orig_point: 7,
            dest_point: 0,
            product_data: 0,
        }),
    };

    // An echomail message assembled through the Message model, then packed.
    let model = Message {
        area: Some("R20.GENERAL".into()),
        kludges: vec![
            "MSGID: 2:280/464.7 4d5e6f70".into(),
            "PID: RabbitHole/0.4".into(),
        ],
        text: b"First echomail post.\r\rEnjoy the box art: \xc9\xcd\xbb".to_vec(),
        tearline: Some("RabbitHole/0.4".into()),
        origin: Some("The Warren (2:280/464.7)".into()),
        seen_by: vec!["280/464 464/1".into()],
        path: vec!["280/464".into()],
    };

    let mut msg = PackedMessage {
        orig_node: 464,
        dest_node: 1,
        orig_net: 280,
        dest_net: 104,
        attribute: 0,
        cost: 0,
        date_time: "02 Jul 26  13:30:45".into(),
        to: "All".into(),
        from: "Kevin".into(),
        subject: "Hello, echo".into(),
        body: Vec::new(),
    };
    msg.set_body(&model);

    Packet {
        header,
        messages: vec![msg],
    }
}

#[test]
fn full_packet_roundtrips() {
    let pkt = build_packet();
    let bytes = pkt.encode();
    let decoded = Packet::decode(&bytes).expect("decode");
    assert_eq!(decoded, pkt);

    // The type-2+ point address is reconstructed from aux_net + orig_point.
    assert_eq!(decoded.header.orig_address().to_string(), "2:280/464.7");
    assert_eq!(decoded.header.dest_address().to_string(), "1:104/1");

    // Body kludges survive and the CP437 box-art bytes are preserved raw.
    let model = decoded.messages[0].parse_body();
    assert_eq!(model.msgid(), Some("2:280/464.7 4d5e6f70"));
    assert_eq!(model.area.as_deref(), Some("R20.GENERAL"));
    assert!(model.text.ends_with(&[0xc9, 0xcd, 0xbb]));
    assert_eq!(
        model.text_str(),
        "First echomail post.\r\rEnjoy the box art: ╔═╗"
    );
    assert!(decoded.messages[0].body_text().contains("╔═╗"));
}

#[test]
fn decoding_random_bytes_never_panics() {
    // A cheap LCG standing in for a fuzzer: exercise many pseudo-random
    // buffers of assorted lengths through every decode entry point.
    let mut state: u64 = 0x1234_5678_9abc_def0;
    let mut next = || {
        state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (state >> 33) as u32
    };

    for _ in 0..2000 {
        let len = (next() % 200) as usize;
        let buf: Vec<u8> = (0..len).map(|_| (next() & 0xff) as u8).collect();
        let _ = Packet::decode(&buf);
        let _ = PacketHeader::decode(&buf);
        let _ = rabbithole_legacy_ftn::decode_messages(&buf);
        let _ = Message::parse(&buf);
    }
}

#[test]
fn truncated_valid_packet_never_panics() {
    let bytes = build_packet().encode();
    for n in 0..=bytes.len() {
        let _ = Packet::decode(&bytes[..n]);
    }
}
