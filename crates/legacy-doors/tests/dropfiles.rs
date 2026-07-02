//! Integration tests for the door drop-file codecs: golden strings, round
//! trips, and malformed-input safety.

use std::time::SystemTime;

use rabbithole_legacy_doors::{
    detect, read_door32_sys, read_door_sys, write, write_door32_sys, write_door_sys,
    write_dorinfo1, DoorContext, DoorUser, DropFile, Emulation,
};

/// A fully-populated sample context anchored at the Unix epoch so date/time
/// derived fields are deterministic (`01/01/70`, `00:00`).
fn sample() -> DoorContext {
    DoorContext {
        node: 3,
        com_port: 0,
        baud: 38400,
        rows: 25,
        cols: 80,
        bbs_name: "RabbitHole BBS".to_string(),
        sysop_name: "Alice Sysop".to_string(),
        bbs_id: "RABBIT".to_string(),
        user: DoorUser {
            real_name: "John Q Public".to_string(),
            alias: "Neo".to_string(),
            location: "Springfield, IL".to_string(),
            security_level: 100,
            time_left_mins: 546,
            emulation: Emulation::Ansi,
            is_ansi: true,
        },
        session_start: SystemTime::UNIX_EPOCH,
    }
}

fn crlf(lines: &[&str]) -> String {
    let mut s = lines.join("\r\n");
    s.push_str("\r\n");
    s
}

#[test]
fn door_sys_golden() {
    let expected = crlf(&[
        "COM0:",           // 1
        "38400",           // 2
        "8",               // 3
        "3",               // 4
        "38400",           // 5
        "Y",               // 6
        "N",               // 7
        "N",               // 8
        "N",               // 9
        "John Q Public",   // 10
        "Springfield, IL", // 11
        "",                // 12
        "",                // 13
        "",                // 14
        "100",             // 15
        "0",               // 16
        "01/01/70",        // 17
        "32760",           // 18
        "546",             // 19
        "GR",              // 20
        "25",              // 21
        "Y",               // 22
        "",                // 23
        "0",               // 24
        "",                // 25
        "0",               // 26
        "Z",               // 27
        "0",               // 28
        "0",               // 29
        "0",               // 30
        "0",               // 31
        "",                // 32
        "",                // 33
        "",                // 34
        "Alice Sysop",     // 35
        "Neo",             // 36
        "00:00",           // 37
        "Y",               // 38
        "Y",               // 39
        "N",               // 40
        "7",               // 41
        "546",             // 42
        "",                // 43
        "00:00",           // 44
        "00:00",           // 45
        "0",               // 46
        "0",               // 47
        "0",               // 48
        "0",               // 49
        "",                // 50
        "0",               // 51
        "0",               // 52
    ]);
    assert_eq!(write_door_sys(&sample()), expected);
    // 52 lines + trailing terminator => 52 CRLF pairs.
    assert_eq!(write_door_sys(&sample()).matches("\r\n").count(), 52);
}

#[test]
fn dorinfo1_golden() {
    let expected = crlf(&[
        "RabbitHole BBS",   // 1
        "Alice",            // 2
        "Sysop",            // 3
        "COM0",             // 4
        "38400 BAUD,N,8,1", // 5
        "0",                // 6
        "John",             // 7
        "Q Public",         // 8
        "Springfield, IL",  // 9
        "1",                // 10
        "100",              // 11
        "546",              // 12
    ]);
    assert_eq!(write_dorinfo1(&sample()), expected);
}

#[test]
fn door32_golden() {
    let expected = crlf(&[
        "0",             // 1 comm type (local)
        "0",             // 2 handle
        "38400",         // 3 baud
        "RABBIT",        // 4 bbsid
        "0",             // 5 record pos
        "John Q Public", // 6 real name
        "Neo",           // 7 alias
        "100",           // 8 security
        "546",           // 9 time left
        "1",             // 10 emulation (ANSI)
        "3",             // 11 node
    ]);
    assert_eq!(write_door32_sys(&sample()), expected);
}

#[test]
fn door_sys_serial_port_and_no_ansi() {
    let mut ctx = sample();
    ctx.com_port = 2;
    ctx.user.is_ansi = false;
    let out = write_door_sys(&ctx);
    let lines: Vec<&str> = out.split("\r\n").collect();
    assert_eq!(lines[0], "COM2:");
    assert_eq!(lines[19], "NG"); // graphics mode
    assert_eq!(lines[38], "N"); // ANSI flag
}

#[test]
fn door_sys_round_trip() {
    let ctx = sample();
    let parsed = read_door_sys(&write_door_sys(&ctx)).unwrap();
    assert_eq!(parsed.com_port, ctx.com_port);
    assert_eq!(parsed.baud, ctx.baud);
    assert_eq!(parsed.node, ctx.node);
    assert_eq!(parsed.rows, ctx.rows);
    assert_eq!(parsed.sysop_name, ctx.sysop_name);
    assert_eq!(parsed.user.real_name, ctx.user.real_name);
    assert_eq!(parsed.user.location, ctx.user.location);
    assert_eq!(parsed.user.alias, ctx.user.alias);
    assert_eq!(parsed.user.security_level, ctx.user.security_level);
    assert_eq!(parsed.user.time_left_mins, ctx.user.time_left_mins);
    assert_eq!(parsed.user.is_ansi, ctx.user.is_ansi);
    assert_eq!(parsed.user.emulation, ctx.user.emulation);
}

#[test]
fn door32_round_trip() {
    for emu in [Emulation::Ascii, Emulation::Ansi, Emulation::Avatar] {
        let mut ctx = sample();
        ctx.com_port = 4;
        ctx.user.emulation = emu;
        ctx.user.is_ansi = emu != Emulation::Ascii;
        let parsed = read_door32_sys(&write_door32_sys(&ctx)).unwrap();
        assert_eq!(parsed.com_port, ctx.com_port);
        assert_eq!(parsed.baud, ctx.baud);
        assert_eq!(parsed.bbs_id, ctx.bbs_id);
        assert_eq!(parsed.node, ctx.node);
        assert_eq!(parsed.user.real_name, ctx.user.real_name);
        assert_eq!(parsed.user.alias, ctx.user.alias);
        assert_eq!(parsed.user.security_level, ctx.user.security_level);
        assert_eq!(parsed.user.time_left_mins, ctx.user.time_left_mins);
        assert_eq!(parsed.user.emulation, ctx.user.emulation);
        assert_eq!(parsed.user.is_ansi, ctx.user.is_ansi);
    }
}

#[test]
fn dispatch_matches_direct_writers() {
    let ctx = sample();
    assert_eq!(write(DropFile::DoorSys, &ctx), write_door_sys(&ctx));
    assert_eq!(write(DropFile::DorInfo1, &ctx), write_dorinfo1(&ctx));
    assert_eq!(write(DropFile::Door32Sys, &ctx), write_door32_sys(&ctx));
    assert_eq!(DropFile::DoorSys.write(&ctx), write_door_sys(&ctx));
    assert_eq!(DropFile::DoorSys.filename(), "DOOR.SYS");
    assert_eq!(DropFile::DorInfo1.filename(), "DORINFO1.DEF");
    assert_eq!(DropFile::Door32Sys.filename(), "DOOR32.SYS");
}

#[test]
fn detect_recognizes_each_format() {
    let ctx = sample();
    assert_eq!(
        detect(write_door_sys(&ctx).as_bytes()),
        Some(DropFile::DoorSys)
    );
    assert_eq!(
        detect(write_dorinfo1(&ctx).as_bytes()),
        Some(DropFile::DorInfo1)
    );
    assert_eq!(
        detect(write_door32_sys(&ctx).as_bytes()),
        Some(DropFile::Door32Sys)
    );
}

#[test]
fn detect_rejects_unknown() {
    assert_eq!(detect(b""), None);
    assert_eq!(detect(b"hello world\r\nnothing here\r\n"), None);
}

#[test]
fn readers_reject_empty() {
    assert!(read_door_sys("").is_err());
    assert!(read_door_sys("\r\n\r\n").is_err());
    assert!(read_door32_sys("").is_err());
}

#[test]
fn readers_survive_truncated_and_garbage() {
    // Truncated: only a few lines present.
    let ctx = read_door_sys("COM3:\r\n2400\r\n").unwrap();
    assert_eq!(ctx.com_port, 3);
    assert_eq!(ctx.baud, 2400);

    // Only the comm-type line present; every other field stays at its default.
    let ctx = read_door32_sys("0\r\n").unwrap();
    assert_eq!(ctx, DoorContext::default());

    // Garbage in numeric fields: no panic, values fall back to defaults.
    let ctx = read_door32_sys("x\r\ny\r\nnotbaud\r\nBID\r\n").unwrap();
    assert_eq!(ctx.bbs_id, "BID");
    assert_eq!(ctx.baud, DoorContext::default().baud);
}

#[test]
fn bare_lf_line_endings_are_tolerated_on_read() {
    let unix = write_door32_sys(&sample()).replace("\r\n", "\n");
    let parsed = read_door32_sys(&unix).unwrap();
    assert_eq!(parsed.node, 3);
    assert_eq!(parsed.bbs_id, "RABBIT");
}

#[test]
fn default_context_is_deterministic() {
    // Two defaults are identical (session_start is the epoch, not `now`).
    assert_eq!(DoorContext::default(), DoorContext::default());
    let out = write_door_sys(&DoorContext::default());
    assert!(out.starts_with("COM0:\r\n"));
}
