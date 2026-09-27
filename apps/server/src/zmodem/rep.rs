//! A disposable, single-file REP transfer. Never enters library upload staging.
use super::*;
use rabbithole_legacy_qwk::rep_archive::MAX_ARCHIVE_BYTES;

const REP_TRANSFER_TIMEOUT: Duration = Duration::from_secs(5 * 60);

/// Receive one complete REP batch into bounded memory. A refused/cancelled/
/// malformed batch drops all bytes, including a completed first file if a
/// sender offers a second. Only the caller's importer may publish replies.
pub(crate) async fn receive_rep<S>(
    t: &mut TelnetStream<S>,
    shared: &Arc<Shared>,
    authed: &AuthedUser,
) -> io::Result<Option<Vec<u8>>>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    t.write_str(
        "\nReady for REP via ZMODEM. Send one .REP archive (maximum 8 MiB).\n\
         Five Ctrl-X cancel; 60 seconds idle, 5 minutes total.\n",
    )
    .await?;
    let outcome = receive_with_budget(&mut Wire::new(t), REP_TRANSFER_TIMEOUT).await;
    match outcome {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) => {
            finish_failed(t, shared, authed, "qwk-rep-receive", "", error, None).await?;
            Ok(None)
        }
    }
}

async fn receive_with_budget<S: AsyncRead + AsyncWrite + Unpin>(
    wire: &mut Wire<'_, S>,
    budget: Duration,
) -> Result<Vec<u8>, Zx> {
    tokio::time::timeout(budget, receive(wire))
        .await
        .unwrap_or(Err(Zx::Timeout))
}

async fn receive<S: AsyncRead + AsyncWrite + Unpin>(wire: &mut Wire<'_, S>) -> Result<Vec<u8>, Zx> {
    let mut rx = Receiver::new();
    let mut wide = false;
    let mut offered = false;
    let mut closed = false;
    let mut declared = None;
    let mut bytes = Vec::new();
    loop {
        let event = match rx.state() {
            RecvState::AwaitingFileInfo | RecvState::ReceivingData { .. } => {
                let sub = wire.next_subpacket(wide).await?;
                RecvEvent::Data {
                    payload: sub.payload,
                    end: sub.end,
                }
            }
            _ => {
                let decoded = match wire.next_header().await {
                    Ok(header) => header,
                    Err(Zx::BadHeader) => {
                        for action in rx.corrupt_header()? {
                            if let RecvAction::SendHeader { header, format } = action {
                                wire.send(&header.encode(format)).await?;
                            }
                        }
                        continue;
                    }
                    Err(error) => return Err(error),
                };
                if matches!(
                    decoded.header.frame_type,
                    FrameType::Zabort | FrameType::Zferr | FrameType::Zcan
                ) {
                    return Err(Zx::Cancelled);
                }
                if matches!(
                    decoded.header.frame_type,
                    FrameType::Zfile | FrameType::Zdata
                ) {
                    wide = decoded.format == HeaderFormat::Bin32;
                }
                RecvEvent::Header(decoded.header)
            }
        };
        if rx.state() == RecvState::AwaitingFileInfo {
            if offered {
                return Err(Zx::Refused(
                    "send exactly one REP archive per command".into(),
                ));
            }
            let RecvEvent::Data { payload, .. } = &event else {
                unreachable!()
            };
            let info = FileInfo::decode(payload)
                .map_err(|_| Zx::Protocol("invalid file information".into()))?;
            vet(&info)?;
            declared = info.length;
            offered = true;
        }
        for action in rx.advance(event)? {
            match action {
                RecvAction::SendHeader { header, format } => {
                    wire.send(&header.encode(format)).await?
                }
                RecvAction::OpenFile(_) => {}
                RecvAction::WriteData { offset, data } => {
                    if offset as usize != bytes.len() {
                        return Err(Zx::Protocol("non-contiguous REP data".into()));
                    }
                    let size = bytes.len() + data.len();
                    if size > MAX_ARCHIVE_BYTES || declared.is_some_and(|n| size as u64 > n) {
                        return Err(Zx::Refused(
                            "REP exceeds 8 MiB or its declared length".into(),
                        ));
                    }
                    bytes.extend_from_slice(&data);
                }
                RecvAction::CloseFile => {
                    if declared.is_some_and(|n| n != bytes.len() as u64) {
                        return Err(Zx::Refused("REP does not match its declared length".into()));
                    }
                    closed = true;
                }
                RecvAction::SendOverAndOut => wire.send(b"OO").await?,
                RecvAction::Finished => {
                    wire.finish_receive(&mut rx).await?;
                    if !closed || bytes.is_empty() {
                        return Err(Zx::Refused("no complete REP archive received".into()));
                    }
                    return Ok(bytes);
                }
            }
        }
    }
}

fn vet(info: &FileInfo) -> Result<(), Zx> {
    // Never use this untrusted name for a path or output. Permit conventional
    // sender paths because the archive itself is never extracted to disk.
    let name = info.name.rsplit(['/', '\\']).next().unwrap_or("");
    if name.len() <= 4
        || name.len() > 128
        || name.chars().any(char::is_control)
        || !name.to_ascii_lowercase().ends_with(".rep")
    {
        return Err(Zx::Refused("select a .REP archive".into()));
    }
    if info
        .length
        .is_some_and(|n| n == 0 || n > MAX_ARCHIVE_BYTES as u64)
    {
        return Err(Zx::Refused(
            "REP must contain between 1 byte and 8 MiB".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
#[path = "rep_tests.rs"]
mod tests;
