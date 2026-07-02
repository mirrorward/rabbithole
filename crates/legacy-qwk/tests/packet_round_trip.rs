//! End-to-end round trips across the codec members, including a QWKE-extended
//! message whose long fields are carried as body kludges through the
//! `MESSAGES.DAT` `0xE3` edge.

use rabbithole_legacy_qwk::{
    messages::BLOCK, ndx, ControlDat, DoorId, MessagesDat, NdxRecord, QwkMessage, QwkeKludges,
};

#[test]
fn qwke_long_fields_survive_the_messages_dat_edge() {
    let long_to = "A recipient whose name is far longer than twenty five characters";
    let long_subject = "Subject text that would be brutally truncated by the 25-byte header field";

    let kludges = QwkeKludges {
        to: Some(long_to.to_string()),
        from: None,
        subject: Some(long_subject.to_string()),
    };
    let real_body = "Body paragraph one.\nBody paragraph two.\n";
    let body_with_kludges = rabbithole_legacy_qwk::qwke::prepend_kludges(&kludges, real_body);

    // The short header fields hold truncated values; the body carries the full
    // QWKE kludges.
    let msg = QwkMessage {
        to: long_to.chars().take(25).collect(),
        subject: long_subject.chars().take(25).collect(),
        body: body_with_kludges,
        ..QwkMessage::new(3, 1, "", "KEVIN", "", "")
    };

    let bytes = MessagesDat::new(vec![msg.clone()]).encode();
    assert_eq!(bytes.len() % BLOCK, 0);

    let decoded = MessagesDat::decode(&bytes).unwrap();
    assert_eq!(decoded.messages.len(), 1);
    let got = &decoded.messages[0];
    assert_eq!(got, &msg);

    // And the QWKE kludges parse back out of the decoded body.
    let (parsed, rest) = rabbithole_legacy_qwk::qwke::parse_kludges(&got.body);
    assert_eq!(parsed.to.as_deref(), Some(long_to));
    assert_eq!(parsed.subject.as_deref(), Some(long_subject));
    assert_eq!(rest, real_body);
}

#[test]
fn whole_packet_members_round_trip_together() {
    let messages = vec![
        QwkMessage::new(0, 1, "ALL", "SYSOP", "Welcome", "Welcome to the board!\n"),
        QwkMessage::new(1, 2, "KEVIN", "ALICE", "Hi", "Hello Kevin\nHow are you?"),
    ];

    let dat = MessagesDat::new(messages);
    let dat_bytes = dat.encode();
    assert_eq!(MessagesDat::decode(&dat_bytes).unwrap(), dat);

    let control = ControlDat {
        bbs_name: "RabbitHole".into(),
        city_state: "Portland, OR".into(),
        phone: "n/a".into(),
        sysop: "SYSOP".into(),
        serial: "1".into(),
        bbs_id: "RABBIT".into(),
        date: "07-02-2026,00:00:00".into(),
        username: "KEVIN".into(),
        total_messages: 2,
        conferences: vec![(0, "Main".into()), (1, "Chat".into())],
        files: vec!["NEWS".into()],
    };
    assert_eq!(ControlDat::parse(&control.to_bytes()).unwrap(), control);

    // The .NDX points at the 1-based block position of each message header.
    let ndx_records = vec![NdxRecord::new(2, 0), NdxRecord::new(4, 1)];
    assert_eq!(
        ndx::decode(&ndx::encode(&ndx_records)).unwrap(),
        ndx_records
    );

    let door = DoorId::qwke("RabbitHole", "1.0", "RabbitHole v0.4");
    assert!(door.advertises_qwke());
    assert_eq!(DoorId::parse_bytes(&door.to_bytes()), door);
}
