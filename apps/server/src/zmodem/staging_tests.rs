use super::*;
use crate::Burrow;
use rabbithole_server_core::{Role, ServerConfig};
use std::future::{poll_fn, Future};
use std::task::Poll;

async fn prepared(
    shared: &Arc<Shared>,
    user: &AuthedUser,
    info: &FileInfo,
    bytes: &[u8],
) -> InFlight {
    let mut upload = match vet_offer(shared, user, "warez", Some("drop"), info).await {
        Ok(upload) => upload,
        Err(_) => panic!("offer should be authorized"),
    };
    assert!(upload.data.is_empty(), "new target has no inherited prefix");
    upload.lease.append(0, bytes).await.unwrap();
    upload.data.extend_from_slice(bytes);
    upload
}

#[tokio::test]
async fn folder_replacement_while_waiting_for_commit_cannot_redirect_upload() {
    let dir = tempfile::tempdir().unwrap();
    let b = Burrow::start(ServerConfig {
        data_dir: dir.path().to_owned(),
        quic_addr: "127.0.0.1:0".parse().unwrap(),
        ws_addr: "127.0.0.1:0".parse().unwrap(),
        ..ServerConfig::default()
    })
    .await
    .unwrap();
    b.shared
        .auth
        .create_account("alice", "pw-pw-pw", Role::User)
        .await
        .unwrap();
    let user = b
        .shared
        .auth
        .login_password("alice", "pw-pw-pw", None)
        .await
        .unwrap();
    b.shared
        .files
        .create_area("warez", "Warez", "")
        .await
        .unwrap();
    let original = b
        .shared
        .files
        .mkdir("warez", None, "drop", false)
        .await
        .unwrap();
    let bytes = b"complete, durable bytes";
    let info = FileInfo {
        length: Some(bytes.len() as u64),
        mtime: Some(7),
        ..FileInfo::new("race.bin")
    };
    let upload = prepared(&b.shared, &user, &info, bytes).await;
    let (message, replacement) = {
        let guard = crate::upload_gate::commit_lock(&b.shared).await;
        let finalization = finalize_upload(&b.shared, &user, "warez", Some("drop"), upload);
        tokio::pin!(finalization);
        // The commit lock is finalization's first await. Polling to Pending proves
        // that this already-vetted upload is waiting at that gate, without sleeps
        // or a process-global hook that could interfere with another test.
        poll_fn(|cx| {
            assert!(finalization.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
        b.shared.files.rename(original.id, "moved").await.unwrap();
        let replacement = b
            .shared
            .files
            .mkdir("warez", None, "drop", false)
            .await
            .unwrap();
        assert_ne!(original.id, replacement.id);
        drop(guard);
        (finalization.await, replacement)
    };
    assert!(
        message.contains("destination changed during upload"),
        "{message}"
    );
    for path in ["moved/race.bin", "drop/race.bin"] {
        assert!(b
            .shared
            .files
            .node_by_path("warez", path)
            .await
            .unwrap()
            .is_none());
    }
    let upload = prepared(&b.shared, &user, &info, bytes).await;
    assert_eq!(
        finalize_upload(&b.shared, &user, "warez", Some("drop"), upload).await,
        format!("Received race.bin ({} bytes).", bytes.len())
    );
    let node = b
        .shared
        .files
        .node_by_path("warez", "drop/race.bin")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(node.parent_id, Some(replacement.id));
    assert_eq!(node.blob_id, Some(*blake3::hash(bytes).as_bytes()));
    b.shutdown().await;
}
