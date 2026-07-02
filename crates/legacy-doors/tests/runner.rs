//! Integration tests for the door-session runner model: registry, node pool,
//! session FSM, I/O bridge, and dropfile preparation.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use rabbithole_legacy_doors::{
    prepare_dropfile, read_door32_sys, read_door_sys, BridgeBuffer, BridgeStats, DoorContext,
    DoorDef, DoorRegistry, DoorSession, DropFile, Error, IoMode, NodePool, NodeRange, SessionState,
    IAC,
};

/// A valid sample definition; tests mutate copies of this.
fn lord() -> DoorDef {
    DoorDef {
        id: "lord".to_string(),
        title: "Legend of the Red Dragon".to_string(),
        command: vec![
            "dosemu".to_string(),
            "-quiet".to_string(),
            "LORD.BAT".to_string(),
        ],
        working_dir: Some(PathBuf::from("/opt/doors/lord")),
        dropfile: DropFile::DoorSys,
        io_mode: IoMode::Stdio,
        nodes: NodeRange::new(1, 4),
        daily_limit_mins: Some(30),
    }
}

fn tw2002() -> DoorDef {
    DoorDef {
        id: "tw2002".to_string(),
        title: "Trade Wars 2002".to_string(),
        command: vec!["tw2002".to_string()],
        working_dir: None,
        dropfile: DropFile::Door32Sys,
        io_mode: IoMode::Socket,
        nodes: NodeRange::single(1),
        daily_limit_mins: None,
    }
}

// ---------------------------------------------------------------- door defs

#[test]
fn door_def_accessors() {
    let def = lord();
    assert_eq!(def.program(), Some("dosemu"));
    assert_eq!(def.args(), ["-quiet".to_string(), "LORD.BAT".to_string()]);
    assert!(!def.is_single_node());
    assert!(tw2002().is_single_node());
    assert_eq!(tw2002().args(), Vec::<String>::new());
}

#[test]
fn door_def_validation_rejects_bad_fields() {
    type Mutation = Box<dyn Fn(&mut DoorDef)>;
    let cases: Vec<(&str, Mutation)> = vec![
        ("empty id", Box::new(|d| d.id.clear())),
        ("whitespace id", Box::new(|d| d.id = "l o r d".into())),
        ("empty title", Box::new(|d| d.title = "  ".into())),
        ("empty argv", Box::new(|d| d.command.clear())),
        ("empty program", Box::new(|d| d.command[0] = " ".into())),
        (
            "inverted node range",
            Box::new(|d| d.nodes = NodeRange::new(4, 2)),
        ),
        (
            "zero-based node range",
            Box::new(|d| d.nodes = NodeRange::new(0, 3)),
        ),
        (
            "zero daily limit",
            Box::new(|d| d.daily_limit_mins = Some(0)),
        ),
    ];
    for (what, mutate) in cases {
        let mut def = lord();
        mutate(&mut def);
        let err = def.validate().expect_err(what);
        assert!(matches!(err, Error::InvalidDoor { .. }), "{what}: {err}");
    }
    assert_eq!(lord().validate(), Ok(()));
}

#[test]
fn node_range_semantics() {
    let r = NodeRange::new(2, 5);
    assert!(r.is_valid() && !r.is_single());
    assert_eq!(r.count(), 4);
    assert!(r.contains(2) && r.contains(5));
    assert!(!r.contains(1) && !r.contains(6));

    assert!(NodeRange::single(7).is_single());
    assert_eq!(NodeRange::single(7).count(), 1);

    assert!(!NodeRange::new(3, 1).is_valid());
    assert_eq!(NodeRange::new(3, 1).count(), 0);
    assert!(!NodeRange::new(3, 1).is_single());
    assert!(!NodeRange::new(0, 0).is_valid());

    let any = NodeRange::default();
    assert_eq!(any, NodeRange::any());
    assert!(any.contains(1) && any.contains(u16::MAX));
}

// ----------------------------------------------------------------- registry

#[test]
fn registry_add_get_list_remove() {
    let mut reg = DoorRegistry::new();
    assert!(reg.is_empty());
    reg.add(lord()).unwrap();
    reg.add(tw2002()).unwrap();
    assert_eq!(reg.len(), 2);
    assert!(!reg.is_empty());

    // Insertion (menu) order preserved.
    let ids: Vec<&str> = reg.list().iter().map(|d| d.id.as_str()).collect();
    assert_eq!(ids, ["lord", "tw2002"]);

    assert_eq!(reg.get("lord").unwrap().title, "Legend of the Red Dragon");
    assert!(reg.get("nosuch").is_none());

    let removed = reg.remove("lord").unwrap();
    assert_eq!(removed.id, "lord");
    assert!(reg.get("lord").is_none());
    assert!(reg.remove("lord").is_none());
    assert_eq!(reg.len(), 1);
}

#[test]
fn registry_rejects_duplicates_and_invalid() {
    let mut reg = DoorRegistry::new();
    reg.add(lord()).unwrap();
    assert_eq!(
        reg.add(lord()),
        Err(Error::DuplicateDoor("lord".to_string()))
    );

    let mut bad = tw2002();
    bad.command.clear();
    assert!(matches!(reg.add(bad), Err(Error::InvalidDoor { .. })));
    // Failed adds leave the registry unchanged.
    assert_eq!(reg.len(), 1);
    assert_eq!(reg.validate(), Ok(()));
}

#[test]
fn registry_toml_round_trip() {
    let mut reg = DoorRegistry::new();
    reg.add(lord()).unwrap();
    reg.add(tw2002()).unwrap();

    // The registry is transparent, so wrap it as a `doors` key for TOML.
    #[derive(serde::Serialize, serde::Deserialize)]
    struct Config {
        doors: DoorRegistry,
    }
    let toml_text = toml::to_string(&Config { doors: reg.clone() }).unwrap();
    assert!(toml_text.contains("[[doors]]"), "got: {toml_text}");
    assert!(toml_text.contains("dropfile = \"door.sys\""));
    assert!(toml_text.contains("io_mode = \"socket\""));

    let back: Config = toml::from_str(&toml_text).unwrap();
    assert_eq!(back.doors, reg);
    assert_eq!(back.doors.validate(), Ok(()));
}

#[test]
fn door_def_toml_defaults_apply() {
    let def: DoorDef = toml::from_str(
        r#"
            id = "pit"
            title = "The Pit"
            command = ["pit"]
            dropfile = "door32.sys"
        "#,
    )
    .unwrap();
    assert_eq!(def.io_mode, IoMode::Stdio);
    assert_eq!(def.nodes, NodeRange::any());
    assert_eq!(def.working_dir, None);
    assert_eq!(def.daily_limit_mins, None);
    assert_eq!(def.validate(), Ok(()));
}

#[test]
fn registry_validate_catches_deserialized_duplicates() {
    #[derive(serde::Deserialize)]
    struct Config {
        doors: DoorRegistry,
    }
    let cfg: Config = toml::from_str(
        r#"
            [[doors]]
            id = "lord"
            title = "Legend of the Red Dragon"
            command = ["lord"]
            dropfile = "door.sys"

            [[doors]]
            id = "lord"
            title = "LORD again"
            command = ["lord2"]
            dropfile = "door32.sys"
        "#,
    )
    .unwrap();
    assert_eq!(cfg.doors.len(), 2); // serde bypassed `add`
    assert_eq!(
        cfg.doors.validate(),
        Err(Error::DuplicateDoor("lord".to_string()))
    );
}

// ---------------------------------------------------------------- node pool

#[test]
fn pool_allocates_lowest_free_and_recycles() {
    let pool = Arc::new(NodePool::new(3));
    assert_eq!(pool.max_nodes(), 3);
    let a = pool.allocate().unwrap();
    let b = pool.allocate().unwrap();
    let c = pool.allocate().unwrap();
    assert_eq!((a.node(), b.node(), c.node()), (1, 2, 3));
    assert_eq!(pool.in_use(), 3);

    // Full: the next allocation fails.
    assert_eq!(
        pool.allocate().unwrap_err(),
        Error::NodesExhausted { first: 1, last: 3 }
    );

    // Release the middle node; the next allocation reuses it.
    drop(b);
    assert!(pool.is_free(2));
    assert_eq!(pool.allocate().unwrap().node(), 2);

    // Explicit release works too.
    a.release();
    assert_eq!(pool.allocate().unwrap().node(), 1);
    drop(c);
    assert_eq!(pool.in_use(), 0);
}

#[test]
fn pool_range_allocation_and_single_node_lock() {
    let pool = Arc::new(NodePool::new(8));

    // Range allocation starts at the range floor, not the pool floor.
    let hi = pool.allocate_in(NodeRange::new(5, 8)).unwrap();
    assert_eq!(hi.node(), 5);

    // Single-node door: first caller in, second locked out.
    let solo = NodeRange::single(3);
    let first = pool.allocate_in(solo).unwrap();
    assert_eq!(first.node(), 3);
    assert_eq!(
        pool.allocate_in(solo).unwrap_err(),
        Error::NodesExhausted { first: 3, last: 3 }
    );
    drop(first);
    assert_eq!(pool.allocate_in(solo).unwrap().node(), 3);

    // Whole-pool allocation skips numbers held by range leases.
    let one = pool.allocate().unwrap();
    assert_eq!(one.node(), 1);

    // Ranges are clamped to the pool; fully-outside ranges fail.
    assert_eq!(
        pool.allocate_in(NodeRange::new(9, 12)).unwrap_err(),
        Error::NodesExhausted { first: 9, last: 12 }
    );
    let clamped = pool.allocate_in(NodeRange::new(7, 200)).unwrap();
    assert_eq!(clamped.node(), 7);

    // A `NodeRange::any()` request works against a small pool.
    let any = pool.allocate_in(NodeRange::any()).unwrap();
    assert_eq!(any.node(), 2);
}

#[test]
fn pool_zero_capacity_never_allocates() {
    let pool = Arc::new(NodePool::new(0));
    assert!(pool.allocate().is_err());
    assert!(pool.allocate_in(NodeRange::single(1)).is_err());
    assert_eq!(pool.in_use(), 0);
}

#[test]
fn pool_contention_across_threads() {
    const NODES: u16 = 16;
    const THREADS: usize = 8;
    const ROUNDS: usize = 200;

    let pool = Arc::new(NodePool::new(NODES));
    let mut handles = Vec::new();
    for _ in 0..THREADS {
        let pool = Arc::clone(&pool);
        handles.push(std::thread::spawn(move || {
            for _ in 0..ROUNDS {
                let lease = pool.allocate().expect("pool cannot be exhausted");
                let node = lease.node();
                assert!((1..=NODES).contains(&node));
                // While held, the node must not be free.
                assert!(!pool.is_free(node));
                drop(lease);
            }
        }));
    }
    for h in handles {
        h.join().expect("no allocator thread may panic");
    }
    // Every lease was dropped, so the pool must be empty again.
    assert_eq!(pool.in_use(), 0);
    assert_eq!(pool.allocate().unwrap().node(), 1);
}

// -------------------------------------------------------------- session FSM

fn t(secs: u64) -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(secs)
}

#[test]
fn session_happy_path() {
    let mut s = DoorSession::new("lord", 2, "/tmp/doors/lord/node2", t(100));
    assert_eq!(s.state(), SessionState::Preparing);
    assert!(!s.is_terminal());
    assert_eq!(s.door_id(), "lord");
    assert_eq!(s.node(), 2);
    assert_eq!(s.drop_dir(), std::path::Path::new("/tmp/doors/lord/node2"));
    assert_eq!(s.created_at(), t(100));
    assert_eq!(s.started_at(), None);
    assert_eq!(s.elapsed(t(500)), None);
    assert_eq!(s.exit_code(), None);

    s.start(t(101)).unwrap();
    assert_eq!(s.state(), SessionState::Running);
    assert_eq!(s.started_at(), Some(t(101)));
    assert_eq!(s.elapsed(t(161)), Some(Duration::from_secs(60)));
    // A clock that went backwards yields zero, never a panic.
    assert_eq!(s.elapsed(t(50)), Some(Duration::ZERO));

    s.finish(0).unwrap();
    assert_eq!(s.state(), SessionState::Ended { exit_code: 0 });
    assert_eq!(s.exit_code(), Some(0));
    assert!(s.is_terminal());
}

#[test]
fn session_timeout_and_abort_paths() {
    // Running -> TimedOut.
    let mut s = DoorSession::new("lord", 1, "/tmp/d", t(0));
    s.start(t(1)).unwrap();
    s.timeout().unwrap();
    assert_eq!(s.state(), SessionState::TimedOut);
    assert!(s.is_terminal());
    assert_eq!(s.exit_code(), None);

    // Preparing -> Aborted (dropfile write failed before launch).
    let mut s = DoorSession::new("lord", 1, "/tmp/d", t(0));
    s.abort().unwrap();
    assert_eq!(s.state(), SessionState::Aborted);
    assert_eq!(s.started_at(), None);

    // Running -> Aborted (caller hung up mid-game).
    let mut s = DoorSession::new("lord", 1, "/tmp/d", t(0));
    s.start(t(1)).unwrap();
    s.abort().unwrap();
    assert_eq!(s.state(), SessionState::Aborted);

    // Nonzero exit codes are preserved.
    let mut s = DoorSession::new("lord", 1, "/tmp/d", t(0));
    s.start(t(1)).unwrap();
    s.finish(139).unwrap();
    assert_eq!(s.exit_code(), Some(139));
}

#[test]
fn session_rejects_illegal_transitions() {
    // From Preparing: finish and timeout are illegal (never started).
    let mut s = DoorSession::new("lord", 1, "/tmp/d", t(0));
    assert_eq!(
        s.finish(0),
        Err(Error::BadTransition {
            state: "preparing",
            event: "finish",
        })
    );
    assert_eq!(
        s.timeout(),
        Err(Error::BadTransition {
            state: "preparing",
            event: "timeout",
        })
    );

    // From Running: start again is illegal.
    s.start(t(1)).unwrap();
    assert_eq!(
        s.start(t(2)),
        Err(Error::BadTransition {
            state: "running",
            event: "start",
        })
    );
    // Failed events leave state untouched.
    assert_eq!(s.state(), SessionState::Running);
    assert_eq!(s.started_at(), Some(t(1)));

    // Terminal states reject every event.
    s.finish(1).unwrap();
    for (err, event) in [
        (s.clone().start(t(3)).unwrap_err(), "start"),
        (s.clone().finish(0).unwrap_err(), "finish"),
        (s.clone().timeout().unwrap_err(), "timeout"),
        (s.clone().abort().unwrap_err(), "abort"),
    ] {
        assert_eq!(
            err,
            Error::BadTransition {
                state: "ended",
                event,
            }
        );
    }

    // Error Display strings are informative.
    let msg = s.clone().abort().unwrap_err().to_string();
    assert_eq!(msg, "invalid door-session transition: abort while ended");
}

#[test]
fn session_state_names_and_terminality() {
    assert!(!SessionState::Preparing.is_terminal());
    assert!(!SessionState::Running.is_terminal());
    assert!(SessionState::Ended { exit_code: 0 }.is_terminal());
    assert!(SessionState::TimedOut.is_terminal());
    assert!(SessionState::Aborted.is_terminal());
    assert_eq!(SessionState::Preparing.name(), "preparing");
    assert_eq!(SessionState::Running.name(), "running");
    assert_eq!(SessionState::Ended { exit_code: 9 }.name(), "ended");
    assert_eq!(SessionState::TimedOut.name(), "timed-out");
    assert_eq!(SessionState::Aborted.name(), "aborted");
}

// --------------------------------------------------------- prepare_dropfile

#[test]
fn prepare_dropfile_dispatches_and_pins_node() {
    let ctx = DoorContext {
        node: 1, // will be overridden per call
        ..DoorContext::default()
    };

    let (name, body) = prepare_dropfile(&lord(), &ctx, 3);
    assert_eq!(name, "DOOR.SYS");
    let parsed = read_door_sys(&body).unwrap();
    assert_eq!(parsed.node, 3);

    let (name, body) = prepare_dropfile(&tw2002(), &ctx, 4);
    assert_eq!(name, "DOOR32.SYS");
    let parsed = read_door32_sys(&body).unwrap();
    assert_eq!(parsed.node, 4);

    let mut def = lord();
    def.dropfile = DropFile::DorInfo1;
    let (name, body) = prepare_dropfile(&def, &ctx, 2);
    assert_eq!(name, "DORINFO1.DEF");
    assert!(body.starts_with("RabbitHole\r\n"));

    // The caller's context is not mutated.
    assert_eq!(ctx.node, 1);
}

#[test]
fn prepare_dropfile_matches_direct_writer_with_node_set() {
    let mut ctx = DoorContext::default();
    let (_, via_prepare) = prepare_dropfile(&tw2002(), &ctx, 9);
    ctx.node = 9;
    assert_eq!(via_prepare, DropFile::Door32Sys.write(&ctx));
}

// ------------------------------------------------------------------- bridge

#[test]
fn stdio_bridge_is_cp437_safe_passthrough_both_ways() {
    let mut bridge = BridgeBuffer::new(IoMode::Stdio);
    assert!(!bridge.escapes_iac());
    let all: Vec<u8> = (0..=255u8).collect();

    let mut out = Vec::new();
    assert_eq!(bridge.door_to_remote(&all, &mut out), 256);
    assert_eq!(out, all); // every byte value intact, IAC included

    let mut back = Vec::new();
    assert_eq!(bridge.remote_to_door(&all, &mut back), 256);
    assert_eq!(back, all);
    assert!(!bridge.has_pending_iac());
    assert_eq!(bridge.finish_remote_to_door(&mut back), 0);

    assert_eq!(
        bridge.stats(),
        BridgeStats {
            door_to_remote: 256,
            remote_to_door: 256,
        }
    );
}

#[test]
fn socket_bridge_doubles_iac_on_the_way_out() {
    let mut bridge = BridgeBuffer::new(IoMode::Socket);
    assert!(bridge.escapes_iac());

    let mut out = Vec::new();
    let n = bridge.door_to_remote(&[0x01, IAC, 0x02, IAC, IAC, 0x03], &mut out);
    assert_eq!(out, [0x01, IAC, IAC, 0x02, IAC, IAC, IAC, IAC, 0x03]);
    assert_eq!(n, 9);
    // Stats count payload bytes, not wire bytes.
    assert_eq!(bridge.stats().door_to_remote, 6);

    // Non-IAC CP437 art passes through untouched.
    let mut out = Vec::new();
    bridge.door_to_remote(&[0xB0, 0xB1, 0xB2, 0xDB], &mut out);
    assert_eq!(out, [0xB0, 0xB1, 0xB2, 0xDB]);

    // Edge shapes: empty, lone IAC, all-IAC.
    let mut out = Vec::new();
    assert_eq!(bridge.door_to_remote(&[], &mut out), 0);
    assert_eq!(bridge.door_to_remote(&[IAC], &mut out), 2);
    assert_eq!(out, [IAC, IAC]);
    let mut out = Vec::new();
    bridge.door_to_remote(&[IAC; 4], &mut out);
    assert_eq!(out, [IAC; 8]);
}

#[test]
fn socket_bridge_collapses_doubled_iac_on_the_way_in() {
    let mut bridge = BridgeBuffer::new(IoMode::Socket);
    let mut out = Vec::new();
    let n = bridge.remote_to_door(&[0x01, IAC, IAC, 0x02, IAC, IAC, IAC, IAC], &mut out);
    assert_eq!(out, [0x01, IAC, 0x02, IAC, IAC]);
    assert_eq!(n, 5);
    assert_eq!(bridge.stats().remote_to_door, 5);
    assert!(!bridge.has_pending_iac());
}

#[test]
fn socket_bridge_round_trip_over_all_chunk_sizes() {
    // Payload exercising every interesting IAC position, including runs.
    let mut payload: Vec<u8> = (0..=255u8).collect();
    payload.extend_from_slice(&[IAC, IAC, IAC, 0x00, IAC]);

    let mut encoder = BridgeBuffer::new(IoMode::Socket);
    let mut wire = Vec::new();
    encoder.door_to_remote(&payload, &mut wire);

    // Decode the wire bytes chunked at every size, so the pending-IAC state
    // is exercised at every possible split point (including inside FF FF).
    for chunk_size in 1..=17 {
        let mut decoder = BridgeBuffer::new(IoMode::Socket);
        let mut back = Vec::new();
        for chunk in wire.chunks(chunk_size) {
            decoder.remote_to_door(chunk, &mut back);
        }
        decoder.finish_remote_to_door(&mut back);
        assert_eq!(back, payload, "chunk_size {chunk_size}");
        assert_eq!(decoder.stats().remote_to_door, payload.len() as u64);
    }
}

#[test]
fn socket_bridge_pending_iac_across_chunks() {
    let mut bridge = BridgeBuffer::new(IoMode::Socket);
    let mut out = Vec::new();

    // Chunk ends in a lone IAC: held, nothing emitted yet.
    assert_eq!(bridge.remote_to_door(&[0x41, IAC], &mut out), 1);
    assert_eq!(out, [0x41]);
    assert!(bridge.has_pending_iac());

    // Next chunk starts with the second IAC: one literal comes out.
    assert_eq!(bridge.remote_to_door(&[IAC, 0x42], &mut out), 2);
    assert_eq!(out, [0x41, IAC, 0x42]);
    assert!(!bridge.has_pending_iac());

    // A lone IAC before a non-IAC byte passes both through verbatim.
    let mut out = Vec::new();
    bridge.remote_to_door(&[IAC], &mut out);
    assert!(bridge.has_pending_iac());
    bridge.remote_to_door(&[0x43], &mut out);
    assert_eq!(out, [IAC, 0x43]);

    // A stream ending mid-escape flushes the dangling IAC as a literal.
    let mut out = Vec::new();
    bridge.remote_to_door(&[IAC], &mut out);
    assert_eq!(bridge.finish_remote_to_door(&mut out), 1);
    assert_eq!(out, [IAC]);
    assert!(!bridge.has_pending_iac());
    assert_eq!(bridge.finish_remote_to_door(&mut out), 0); // idempotent
}

#[test]
fn socket_bridge_large_buffer_chunking() {
    // 64 KiB of pure IAC: worst case for the escaper.
    let payload = vec![IAC; 64 * 1024];
    let mut bridge = BridgeBuffer::new(IoMode::Socket);
    let mut wire = Vec::new();
    let n = bridge.door_to_remote(&payload, &mut wire);
    assert_eq!(n, 128 * 1024);
    assert_eq!(wire.len(), 128 * 1024);
    assert!(wire.iter().all(|&b| b == IAC));
    assert_eq!(bridge.stats().door_to_remote, 64 * 1024);

    // Decode it back in awkwardly-sized chunks.
    let mut back = Vec::new();
    for chunk in wire.chunks(4093) {
        bridge.remote_to_door(chunk, &mut back);
    }
    bridge.finish_remote_to_door(&mut back);
    assert_eq!(back, payload);
}

#[test]
fn bridge_stats_rates_use_injected_time_only() {
    let stats = BridgeStats {
        door_to_remote: 4800,
        remote_to_door: 1200,
    };
    assert_eq!(stats.total(), 6000);
    let minute = Duration::from_secs(60);
    assert!((stats.door_to_remote_bps(minute) - 80.0).abs() < f64::EPSILON);
    assert!((stats.remote_to_door_bps(minute) - 20.0).abs() < f64::EPSILON);
    assert!((stats.total_bps(minute) - 100.0).abs() < f64::EPSILON);
    // Zero elapsed never divides by zero.
    assert_eq!(stats.total_bps(Duration::ZERO), 0.0);

    // Saturation instead of overflow.
    let maxed = BridgeStats {
        door_to_remote: u64::MAX,
        remote_to_door: 1,
    };
    assert_eq!(maxed.total(), u64::MAX);
}

#[test]
fn default_bridge_is_passthrough() {
    // `Default` matches `IoMode::default()` == Stdio: no escaping.
    let mut bridge = BridgeBuffer::default();
    assert!(!bridge.escapes_iac());
    let mut out = Vec::new();
    bridge.door_to_remote(&[IAC, IAC], &mut out);
    assert_eq!(out, [IAC, IAC]);
}

// ------------------------------------------------- end-to-end shaped flow

/// The full pure-core flow a burrow driver will run: registry lookup, node
/// lease, dropfile preparation, FSM, bridge, accounting.
#[test]
fn runner_model_end_to_end() {
    let mut reg = DoorRegistry::new();
    reg.add(tw2002()).unwrap();
    let def = reg.get("tw2002").unwrap();

    let pool = Arc::new(NodePool::new(4));
    let lease = pool.allocate_in(def.nodes).unwrap();
    assert_eq!(lease.node(), 1);
    // Single-node door: a second caller is refused while we hold the lease.
    assert!(pool.allocate_in(def.nodes).is_err());

    let ctx = DoorContext::default();
    let (filename, contents) = prepare_dropfile(def, &ctx, lease.node());
    assert_eq!(filename, "DOOR32.SYS");
    assert_eq!(read_door32_sys(&contents).unwrap().node, lease.node());

    let mut session = DoorSession::new(&def.id, lease.node(), "/tmp/doors/tw2002/node1", t(0));
    session.start(t(1)).unwrap();

    let mut bridge = BridgeBuffer::new(def.io_mode);
    let mut wire = Vec::new();
    bridge.door_to_remote(b"Trade Wars 2002\xFF", &mut wire);
    assert_eq!(&wire[wire.len() - 2..], [IAC, IAC]);

    session.finish(0).unwrap();
    assert!(session.is_terminal());
    assert_eq!(session.elapsed(t(61)), Some(Duration::from_secs(60)));

    drop(lease);
    assert_eq!(pool.allocate_in(def.nodes).unwrap().node(), 1);
}
