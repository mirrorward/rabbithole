//! The swarm leg of a send between burrows: the destination asks the source
//! for the swarm peers holding each larger file and a capability naming the
//! destination, fetches the pieces from those peers, and falls back to the
//! source for whatever they cannot give. Each end's operator allows it
//! separately; a person's capability and a burrow's are never taken for
//! each other.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use burrow::Burrow;
use rabbithole_core::Client;
use rabbithole_proto::filelib::{
    pull_state, PullGrantAsk, PullGrantIssued, RemotePull, RemotePullAccepted, RemotePullStatus,
};
use rabbithole_proto::swarm::AdvertEntry;
use rabbithole_server_core::{Role, ServerConfig};
use rabbithole_store_server::repo::AuditRepo;
use rabbithole_swarm::{PeerServer, SeedStore};

const PW: &str = "pw-pw-pw-pw";

fn config(dir: &Path, name: &str) -> ServerConfig {
    ServerConfig {
        name: name.into(),
        quic_addr: "127.0.0.1:0".parse().unwrap(),
        ws_addr: "127.0.0.1:0".parse().unwrap(),
        data_dir: dir.to_path_buf(),
        s2s_grants_enabled: true,
        s2s_pull_enabled: true,
        s2s_grants_to_any: true,
        s2s_pull_from_any: true,
        // Everything here is on one machine.
        s2s_private_addresses: true,
        s2s_swarm: true,
        s2s_swarm_sources: true,
        ..ServerConfig::default()
    }
}

async fn start(dir: &Path, name: &str, people: &[&str]) -> Burrow {
    let burrow = Burrow::start(config(dir, name)).await.unwrap();
    for person in people {
        burrow
            .shared
            .auth
            .create_account(person, PW, Role::User)
            .await
            .unwrap();
    }
    burrow
}

async fn login(burrow: &Burrow, user: &str) -> Client {
    let url = format!("ws://127.0.0.1:{}", burrow.ws_addr.port());
    let mut c = Client::connect(&url, None, None, "e2e", "0").await.unwrap();
    c.auth_password(user, PW).await.unwrap();
    c.expect_welcome().await.unwrap();
    c
}

async fn until_done(c: &mut Client, pull_id: u64) -> RemotePullStatus {
    tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            let frame = c.next_push().await.unwrap().expect("session open");
            if let Some(Ok(status)) = frame.decode::<RemotePullStatus>() {
                if status.pull_id == pull_id && status.state != pull_state::RUNNING {
                    return status;
                }
            }
        }
    })
    .await
    .expect("the pull ends")
}

/// The `swarm=` count on the destination's audit line for pull `pull_id`.
async fn from_swarm(dest: &Burrow, pull_id: u64) -> u32 {
    let prefix = format!("#{pull_id} ");
    for _ in 0..50 {
        let rows = AuditRepo(&dest.shared.pool).recent(50).await.unwrap();
        if let Some(row) = rows
            .iter()
            .find(|r| r.action == "pull-done" && r.detail.starts_with(&prefix))
        {
            let n = row
                .detail
                .split_whitespace()
                .find_map(|kv| kv.strip_prefix("swarm="))
                .expect("the audit line counts swarm files");
            return n.parse().unwrap();
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("no audit line for pull {pull_id}");
}

/// A person at the source seeding `path` (root `root`) from a peer-wire
/// endpoint of their own, advertised to the source with a contact card.
async fn seeder(
    source: &Burrow,
    name: &str,
    root: [u8; 32],
    path: &Path,
    size: u64,
) -> (Client, PeerServer) {
    let mut s = login(source, name).await;
    let seeds = Arc::new(SeedStore::new());
    seeds.add(root, path).unwrap();
    let peer = PeerServer::start("127.0.0.1:0".parse().unwrap(), s.server.server_key, seeds)
        .await
        .unwrap();
    s.swarm_advertise(
        vec![AdvertEntry::new(
            root,
            size,
            "tape.bin",
            "application/octet-stream",
        )],
        0,
    )
    .await
    .unwrap();
    s.swarm_contact(peer.addr.port(), peer.fingerprint.0)
        .await
        .unwrap();
    (s, peer)
}

#[tokio::test]
async fn a_sent_file_comes_from_the_sources_swarm_and_falls_back_to_the_source() {
    let work = tempfile::tempdir().unwrap();
    let source = start(
        &work.path().join("source"),
        "Swarm Source",
        &["alice", "carol", "dave"],
    )
    .await;
    let dest = start(&work.path().join("dest"), "Swarm Dest", &["alice"]).await;
    let dest_key = dest.shared.server_key;

    // Four units: worth the swarm.
    let body: Vec<u8> = (0..3 * 1024 * 1024 + 99).map(|i| (i % 253) as u8).collect();
    let root = *blake3::hash(&body).as_bytes();
    let on_disk = work.path().join("tape.bin");
    std::fs::write(&on_disk, &body).unwrap();
    let files = &source.shared.files;
    files.create_area("music", "Music", "").await.unwrap();
    let blob = source.shared.blobs.put(&body).unwrap();
    assert_eq!(blob.0, root);
    let tape = files
        .add_file(
            "music",
            None,
            "tape.bin",
            &blob.0,
            body.len() as i64,
            "application/octet-stream",
            "",
            "",
            "x@y",
            1,
        )
        .await
        .unwrap()
        .id;
    dest.shared
        .files
        .create_area("inbox", "Inbox", "")
        .await
        .unwrap();

    let size = body.len() as u64;
    let (carol, carol_peer) = seeder(&source, "carol", root, &on_disk, size).await;
    let (dave, dave_peer) = seeder(&source, "dave", root, &on_disk, size).await;

    let mut alice_s = login(&source, "alice").await;
    let mut alice_d = login(&dest, "alice").await;
    let mut send = async |name: &str| {
        let issued: PullGrantIssued = alice_s
            .request(&PullGrantAsk::new(dest_key, vec![tape], "127.0.0.1"))
            .await
            .unwrap();
        let accepted: RemotePullAccepted = alice_d
            .request(&RemotePull::new(issued.grant, "inbox", None))
            .await
            .unwrap();
        let done = until_done(&mut alice_d, accepted.pull_id).await;
        assert_eq!(
            (done.state, done.files_done),
            (pull_state::DONE, 1),
            "{done:?}"
        );
        let node = dest
            .shared
            .files
            .node_by_path("inbox", name)
            .await
            .unwrap()
            .expect("filed");
        assert_eq!(node.blob_id, Some(root));
        accepted.pull_id
    };

    let ranges_served = || {
        source
            .shared
            .s2s
            .ranges_served
            .load(std::sync::atomic::Ordering::Relaxed)
    };

    // Both ends allow it: the pieces come from Carol and Dave, with the
    // source itself as one more source. Held to a trickle here, so the
    // seeders surely carry some of it.
    source
        .shared
        .config
        .set_key("transfer_rate_bytes_per_sec", "262144")
        .unwrap();
    let pull = send("tape.bin").await;
    source
        .shared
        .config
        .set_key("transfer_rate_bytes_per_sec", "0")
        .unwrap();
    assert_eq!(from_swarm(&dest, pull).await, 1, "fetched from the swarm");

    // The source does not share its swarm: with nobody to mix with, the
    // source sends it all as one plain stream, checked whole.
    source
        .shared
        .config
        .set_key("s2s_swarm_sources", "false")
        .unwrap();
    let before = ranges_served();
    let pull = send("tape (2).bin").await;
    assert_eq!(from_swarm(&dest, pull).await, 0);
    assert_eq!(ranges_served(), before, "no proved ranges without seeders");
    source
        .shared
        .config
        .set_key("s2s_swarm_sources", "true")
        .unwrap();

    // The destination does not want it: the same.
    dest.shared.config.set_key("s2s_swarm", "false").unwrap();
    let pull = send("tape (3).bin").await;
    assert_eq!(from_swarm(&dest, pull).await, 0);
    dest.shared.config.set_key("s2s_swarm", "true").unwrap();

    // The seeders are gone but still listed (their sessions are up): the
    // swarm gives nothing, and the source sends the file instead.
    carol_peer.stop();
    dave_peer.stop();
    let before = ranges_served();
    let pull = send("tape (4).bin").await;
    assert_eq!(from_swarm(&dest, pull).await, 0);
    // It came as proved ranges from the source (every unit of it), which
    // keeps the file's proofs beside it for that.
    assert!(
        ranges_served() >= before + 4,
        "the burrow's own ranges carried it: {} to {}",
        before,
        ranges_served()
    );
    let blob = rabbithole_blobs::BlobId(root);
    assert!(source.shared.blobs.outboard_path(&blob).exists());

    // Nothing of a swarm attempt is left in the staging folder.
    let staging = dest.shared.config.read().data_dir.join("transfers");
    let left: Vec<_> = std::fs::read_dir(&staging)
        .map(|d| d.flatten().map(|e| e.file_name()).collect())
        .unwrap_or_default();
    assert!(left.is_empty(), "left behind: {left:?}");

    drop((carol, dave));
    source.shutdown().await;
    dest.shutdown().await;
}
