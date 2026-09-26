use super::*;

fn header(kind: FrameType) -> Header {
    Header::new(kind)
}
fn send_header(kind: FrameType) -> SendEvent {
    SendEvent::Header(header(kind))
}
fn recv_header(kind: FrameType) -> RecvEvent {
    RecvEvent::Header(header(kind))
}
fn offer() -> RecvEvent {
    RecvEvent::Data {
        payload: FileInfo::new("next.bin").encode().unwrap(),
        end: FrameEnd::Zcrcw,
    }
}

#[test]
fn sender_replays_the_pending_exchange_without_losing_positions_or_crc_width() {
    let mut tx = Sender::new(FileInfo::new("test.bin"));
    let init = tx.start().unwrap();
    assert_eq!(tx.advance(send_header(FrameType::Znak)).unwrap(), init);
    let file = tx
        .advance(SendEvent::Header(Header::with_flags(
            FrameType::Zrinit,
            0,
            0,
            0,
            CANFC32,
        )))
        .unwrap();
    assert!(matches!(
        &file[..],
        [
            SendAction::SendHeader {
                format: HeaderFormat::Bin32,
                ..
            },
            SendAction::SendFileInfo(_)
        ]
    ));
    assert_eq!(tx.advance(send_header(FrameType::Znak)).unwrap(), file);
    tx.advance(SendEvent::Header(Header::with_pos(FrameType::Zrpos, 123)))
        .unwrap();
    let eof = tx
        .advance(SendEvent::DataExhausted { offset: 456 })
        .unwrap();
    tx.corrupt_header().unwrap();
    assert_eq!(
        tx.advance(send_header(FrameType::Znak)).unwrap(),
        eof,
        "a NAK never replaces the pending real exchange"
    );
    assert!(matches!(eof[0], SendAction::SendHeader { header, .. } if header.pos() == 456));
    let fin = tx.advance(send_header(FrameType::Zrinit)).unwrap();
    assert_eq!(tx.advance(send_header(FrameType::Znak)).unwrap(), fin);
    tx.advance(send_header(FrameType::Zfin)).unwrap();
    assert_eq!(tx.state(), SendState::Done);
    assert!(!tx.skipped());
}

#[test]
fn skipped_download_finishes_cleanly_and_duplicate_declines_are_bounded() {
    let mut tx = Sender::new(FileInfo::new("test.bin"));
    assert!(tx.advance(send_header(FrameType::Znak)).is_err());
    tx.start().unwrap();
    tx.advance(send_header(FrameType::Zrinit)).unwrap();
    let finish = tx.advance(send_header(FrameType::Zskip)).unwrap();
    assert!(tx.skipped());
    assert_eq!(tx.state(), SendState::AwaitingFinAck);
    for _ in 0..MAX_RECOVERY_ATTEMPTS {
        assert_eq!(tx.advance(send_header(FrameType::Zskip)).unwrap(), finish);
    }
    assert_eq!(
        tx.advance(send_header(FrameType::Zskip)),
        Err(SessionError::RetriesExhausted)
    );
    assert!(matches!(
        &tx.advance(send_header(FrameType::Zfin)).unwrap()[..],
        [SendAction::SendOverAndOut, SendAction::Finished]
    ));
}

#[test]
fn receiver_decline_preserves_batch_and_drops_the_declined_resume_offset() {
    let mut rx = Receiver::new();
    assert!(rx.decline_file().is_err());
    rx.advance(recv_header(FrameType::Zrqinit)).unwrap();
    rx.advance(recv_header(FrameType::Zfile)).unwrap();
    rx.set_resume_offset(123);
    let skip = rx.decline_file().unwrap();
    assert_eq!(rx.state(), RecvState::AwaitingFile);
    assert_eq!(rx.advance(recv_header(FrameType::Znak)).unwrap(), skip);
    rx.advance(recv_header(FrameType::Zfile)).unwrap();
    let accepted = rx.advance(offer()).unwrap();
    assert!(
        matches!(&accepted[..], [RecvAction::OpenFile(_), RecvAction::SendHeader { header, .. }] if header.pos() == 0)
    );
    assert_eq!(rx.state(), RecvState::AwaitingData { offset: 0 });
    let position = rx.advance(recv_header(FrameType::Znak)).unwrap();
    assert!(
        matches!(position[0], RecvAction::SendHeader { header, .. } if header.frame_type == FrameType::Zrpos && header.pos() == 0)
    );
}

#[test]
fn mixed_malformed_headers_and_naks_share_one_session_retry_budget() {
    let mut tx = Sender::new(FileInfo::new("test.bin"));
    tx.start().unwrap();
    let mut rx = Receiver::new();
    rx.advance(recv_header(FrameType::Zrqinit)).unwrap();
    for attempt in 0..MAX_RECOVERY_ATTEMPTS {
        if attempt % 2 == 0 {
            tx.corrupt_header().unwrap();
            rx.corrupt_header().unwrap();
        } else {
            tx.advance(send_header(FrameType::Znak)).unwrap();
            rx.advance(recv_header(FrameType::Znak)).unwrap();
        }
    }
    assert_eq!(tx.corrupt_header(), Err(SessionError::RetriesExhausted));
    assert_eq!(rx.corrupt_header(), Err(SessionError::RetriesExhausted));
}

#[test]
fn receiver_final_reply_can_be_retried_without_reclosing_a_file() {
    let mut rx = Receiver::new();
    rx.advance(recv_header(FrameType::Zrqinit)).unwrap();
    rx.advance(recv_header(FrameType::Zfin)).unwrap();
    for event in [recv_header(FrameType::Znak), recv_header(FrameType::Zfin)] {
        let actions = rx.advance(event).unwrap();
        assert!(
            matches!(&actions[..], [RecvAction::SendHeader { header, .. }] if header.frame_type == FrameType::Zfin)
        );
        assert_eq!(rx.state(), RecvState::Done);
    }
}
