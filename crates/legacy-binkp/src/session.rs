//! Sans-IO binkp session: happy-path handshake + batch FSM for both roles.
//!
//! binkp is symmetric after the handshake: once authenticated, each side may
//! send files to the other, so a session both *sends* and *receives* in one
//! batch. This state machine owns no sockets and does no framing — the caller
//! decodes wire blocks (via [`crate::frame`]/[`crate::command`]) into
//! [`Event`]s, feeds them to [`Session::advance`], and performs the returned
//! [`Action`]s (encode a frame, stream a file, write received bytes, …).
//!
//! ## Handshake (CRAM-MD5 secure session)
//!
//! ```text
//!   originating (caller)                 answering (server)
//!   ── connect ─────────────────────────────►
//!   M_NUL "SYS …" / "OPT …"    ◄──►   M_NUL "SYS …" / "OPT CRAM-MD5-<chal>"
//!   M_ADR <our addrs>          ◄──►   M_ADR <their addrs>
//!   M_PWD "CRAM-MD5-<digest>"  ──────►
//!                              ◄──────  M_OK "secure"     (password verified)
//!   ───────────── both enter the batch phase ─────────────
//! ```
//!
//! The answering side offers the challenge in an `M_NUL "OPT CRAM-MD5-…"`; the
//! originating side answers `M_PWD` with the [`crate::cram`] digest. With no
//! challenge the password travels in the clear (or `-` for an unsecured link).
//!
//! ## Batch phase (per direction, interleaved)
//!
//! ```text
//!   sender                                 receiver
//!   M_FILE "name size time 0"   ──────►    (ExpectFile → open for write)
//!   [data block] [data block] … ──────►    (WriteData …)
//!                               ◄──────     M_GOT "name size time"  (ack)
//!   … next file, or …
//!   M_EOB                       ──────►     (peer done sending)
//! ```
//!
//! The session completes when *both* sides have sent `M_EOB` and no transfer
//! is in flight in either direction.
//!
//! ## Deliberately deferred (future slices)
//!
//! - **Non-reliable mode / `M_GET`**: crash-recovery resume from a non-zero
//!   offset (`M_GET "name size time offset"`) is rejected as unexpected here;
//!   only whole-file offset-0 transfers are driven.
//! - **`MD` mode & dupe handling**: `OPT MD`/multi-batch dedupe, `NR` mode
//!   negotiation, and the `CRYPT` option are parsed-through but not acted on.
//! - **Crashmail / poll semantics**, `M_BSY` retry scheduling, and password
//!   comparison hardening (constant-time) live in the transport slice.
//! - **Pipelining**: one outstanding outbound file at a time; a second
//!   inbound `M_FILE` before the first completes is treated as unexpected.
//!
//! Out-of-order but well-formed events yield [`SessionError::Unexpected`]
//! rather than silent misbehaviour, and hostile input can never panic the
//! FSM (see the fuzz test).

use std::collections::VecDeque;

use thiserror::Error;

use crate::address::Address;
use crate::command::{Command, CommandId, FileId, FileInfo};
use crate::cram::cram_md5_response;

/// Which end of the connection this session represents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// The side that placed the call (sends `M_PWD`, awaits `M_OK`).
    Originating,
    /// The side that answered (offers the CRAM challenge, verifies `M_PWD`).
    Answering,
}

/// Static configuration for a session.
#[derive(Debug, Clone, Default)]
pub struct SessionConfig {
    /// Our own 5D addresses, advertised in `M_ADR`.
    pub addresses: Vec<Address>,
    /// Free-form `M_NUL` info lines to advertise (e.g. `SYS RabbitHole`).
    pub system_info: Vec<String>,
    /// Session password. Empty or `-` means an unsecured session.
    pub password: String,
    /// For the answering side: the CRAM-MD5 challenge to offer (raw bytes).
    pub challenge: Option<Vec<u8>>,
    /// Files to send to the peer during the batch phase.
    pub outgoing: Vec<FileInfo>,
}

/// Coarse session phase, for inspection/logging.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// Exchanging `M_NUL`/`M_ADR` (answering also awaits `M_PWD` here).
    Greeting,
    /// Originating side sent `M_PWD`; awaiting `M_OK`.
    AwaitOk,
    /// Authenticated; sending and receiving files.
    Transfer,
    /// Finished cleanly.
    Done,
    /// Aborted (`M_ERR`/`M_BSY`/auth failure).
    Failed,
}

impl Phase {
    fn name(self) -> &'static str {
        match self {
            Phase::Greeting => "Greeting",
            Phase::AwaitOk => "AwaitOk",
            Phase::Transfer => "Transfer",
            Phase::Done => "Done",
            Phase::Failed => "Failed",
        }
    }
}

/// A decoded input to the session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// A command frame arrived from the peer.
    Command(Command),
    /// A data block (file bytes for the current inbound file) arrived.
    Data(Vec<u8>),
}

impl Event {
    fn name(&self) -> &'static str {
        match self {
            Event::Data(_) => "data block",
            Event::Command(c) => command_name(c.id()),
        }
    }
}

fn command_name(id: CommandId) -> &'static str {
    match id {
        CommandId::Nul => "M_NUL",
        CommandId::Adr => "M_ADR",
        CommandId::Pwd => "M_PWD",
        CommandId::File => "M_FILE",
        CommandId::Ok => "M_OK",
        CommandId::Eob => "M_EOB",
        CommandId::Got => "M_GOT",
        CommandId::Err => "M_ERR",
        CommandId::Bsy => "M_BSY",
        CommandId::Get => "M_GET",
        CommandId::Skip => "M_SKIP",
    }
}

/// What the caller must do next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Encode and transmit this command frame.
    SendCommand(Command),
    /// Stream this file's bytes to the peer as data blocks, in order.
    StreamFile(FileInfo),
    /// The peer announced a file; open it for writing before data arrives.
    ExpectFile(FileInfo),
    /// Append these received bytes to the current inbound file.
    WriteData(Vec<u8>),
    /// The current inbound file is complete (an `M_GOT` ack was also emitted).
    FileComplete(FileId),
    /// The handshake authenticated successfully.
    Authenticated,
    /// The session finished cleanly.
    Finished,
    /// The session aborted; tear down. Carries the reason text.
    Aborted(String),
}

/// Errors from the session state machine.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum SessionError {
    /// An event arrived that this phase cannot handle.
    #[error("unexpected {event} in phase {phase}")]
    Unexpected {
        /// The current phase.
        phase: &'static str,
        /// The offending event.
        event: &'static str,
    },
    /// A data block arrived while no inbound file was open.
    #[error("data block arrived with no file in progress")]
    UnexpectedData,
}

/// A binkp session for one connection end.
#[derive(Debug)]
pub struct Session {
    role: Role,
    phase: Phase,
    addresses: Vec<Address>,
    system_info: Vec<String>,
    password: String,
    /// Answering side: the challenge we advertise.
    challenge: Option<Vec<u8>>,
    /// Originating side: the challenge parsed out of the peer's `M_NUL`.
    peer_challenge: Option<Vec<u8>>,
    peer_addresses: Vec<Address>,
    outgoing: VecDeque<FileInfo>,
    /// The file we are sending and awaiting an `M_GOT` for.
    sending: Option<FileId>,
    /// The inbound file and how many bytes we've received so far.
    receiving: Option<(FileInfo, u64)>,
    local_eob: bool,
    peer_eob: bool,
    started: bool,
}

impl Session {
    /// Create a session for `role` with the given configuration.
    pub fn new(role: Role, config: SessionConfig) -> Self {
        Session {
            role,
            phase: Phase::Greeting,
            addresses: config.addresses,
            system_info: config.system_info,
            password: config.password,
            challenge: config.challenge,
            peer_challenge: None,
            peer_addresses: Vec::new(),
            outgoing: config.outgoing.into_iter().collect(),
            sending: None,
            receiving: None,
            local_eob: false,
            peer_eob: false,
            started: false,
        }
    }

    /// Convenience constructor for the originating (calling) side.
    pub fn originating(config: SessionConfig) -> Self {
        Session::new(Role::Originating, config)
    }

    /// Convenience constructor for the answering (server) side.
    pub fn answering(config: SessionConfig) -> Self {
        Session::new(Role::Answering, config)
    }

    /// The current phase.
    pub fn phase(&self) -> Phase {
        self.phase
    }

    /// This session's role.
    pub fn role(&self) -> Role {
        self.role
    }

    /// The addresses the peer advertised in `M_ADR` (valid after greeting).
    pub fn peer_addresses(&self) -> &[Address] {
        &self.peer_addresses
    }

    /// The CRAM-MD5 challenge parsed from the peer's `M_NUL` (originating).
    pub fn peer_challenge(&self) -> Option<&[u8]> {
        self.peer_challenge.as_deref()
    }

    /// Emit the opening greeting frames (`M_NUL` info + `M_ADR`). Call once,
    /// before feeding any events.
    pub fn start(&mut self) -> Vec<Action> {
        if self.started {
            return Vec::new();
        }
        self.started = true;
        let mut actions = Vec::new();
        for line in &self.system_info {
            actions.push(Action::SendCommand(Command::Nul(line.clone())));
        }
        // The answering side advertises its CRAM-MD5 challenge.
        if self.role == Role::Answering {
            if let Some(challenge) = &self.challenge {
                actions.push(Action::SendCommand(Command::Nul(format!(
                    "OPT {}",
                    crate::cram::cram_md5_option(challenge)
                ))));
            }
        }
        actions.push(Action::SendCommand(Command::Adr(self.addresses.clone())));
        actions
    }

    /// Feed one event; returns the actions to perform in order.
    pub fn advance(&mut self, event: Event) -> Result<Vec<Action>, SessionError> {
        match self.phase {
            Phase::Greeting => self.advance_greeting(event),
            Phase::AwaitOk => self.advance_await_ok(event),
            Phase::Transfer => self.advance_transfer(event),
            Phase::Done | Phase::Failed => Err(self.unexpected(&event)),
        }
    }

    fn unexpected(&self, event: &Event) -> SessionError {
        SessionError::Unexpected {
            phase: self.phase.name(),
            event: event.name(),
        }
    }

    // -- greeting ----------------------------------------------------------

    fn advance_greeting(&mut self, event: Event) -> Result<Vec<Action>, SessionError> {
        let cmd = match event {
            Event::Command(cmd) => cmd,
            Event::Data(_) => return Err(SessionError::UnexpectedData),
        };
        match cmd {
            Command::Nul(line) => {
                if self.peer_challenge.is_none() {
                    if let Some(chal) = crate::cram::parse_challenge(&line) {
                        self.peer_challenge = Some(chal);
                    }
                }
                Ok(Vec::new())
            }
            Command::Adr(addrs) => {
                self.peer_addresses = addrs;
                match self.role {
                    Role::Originating => {
                        // We have the peer's challenge by now; answer with PWD.
                        let pwd = self.password_value();
                        self.phase = Phase::AwaitOk;
                        Ok(vec![Action::SendCommand(Command::Pwd(pwd))])
                    }
                    Role::Answering => Ok(Vec::new()),
                }
            }
            Command::Pwd(pw) if self.role == Role::Answering => {
                if self.verify_password(&pw) {
                    let mut actions = vec![
                        Action::SendCommand(Command::Ok("secure".to_string())),
                        Action::Authenticated,
                    ];
                    actions.extend(self.enter_transfer());
                    Ok(actions)
                } else {
                    self.phase = Phase::Failed;
                    Ok(vec![
                        Action::SendCommand(Command::Err("bad password".to_string())),
                        Action::Aborted("bad password".to_string()),
                    ])
                }
            }
            Command::Err(t) | Command::Bsy(t) => {
                self.phase = Phase::Failed;
                Ok(vec![Action::Aborted(t)])
            }
            other => Err(SessionError::Unexpected {
                phase: self.phase.name(),
                event: command_name(other.id()),
            }),
        }
    }

    // -- await OK (originating) --------------------------------------------

    fn advance_await_ok(&mut self, event: Event) -> Result<Vec<Action>, SessionError> {
        let cmd = match event {
            Event::Command(cmd) => cmd,
            Event::Data(_) => return Err(SessionError::UnexpectedData),
        };
        match cmd {
            Command::Ok(_) => {
                let mut actions = vec![Action::Authenticated];
                actions.extend(self.enter_transfer());
                Ok(actions)
            }
            // Peers may still emit info before the OK lands.
            Command::Nul(_) | Command::Adr(_) => Ok(Vec::new()),
            Command::Err(t) | Command::Bsy(t) => {
                self.phase = Phase::Failed;
                Ok(vec![Action::Aborted(t)])
            }
            other => Err(SessionError::Unexpected {
                phase: self.phase.name(),
                event: command_name(other.id()),
            }),
        }
    }

    // -- transfer ----------------------------------------------------------

    fn advance_transfer(&mut self, event: Event) -> Result<Vec<Action>, SessionError> {
        match event {
            Event::Data(bytes) => self.on_data(bytes),
            Event::Command(Command::File(info)) => self.on_file(info),
            Event::Command(Command::Got(_)) | Event::Command(Command::Skip(_)) => {
                // Ack (or decline) of our outstanding outbound file.
                if self.sending.take().is_some() {
                    Ok(self.start_next_send())
                } else {
                    Err(SessionError::Unexpected {
                        phase: self.phase.name(),
                        event: "M_GOT",
                    })
                }
            }
            Event::Command(Command::Eob) => {
                self.peer_eob = true;
                Ok(self.maybe_finish())
            }
            // Info frames may arrive at any time; ignore them.
            Event::Command(Command::Nul(_)) | Event::Command(Command::Adr(_)) => Ok(Vec::new()),
            Event::Command(Command::Err(t)) | Event::Command(Command::Bsy(t)) => {
                self.phase = Phase::Failed;
                Ok(vec![Action::Aborted(t)])
            }
            // M_GET (resume) and stray M_PWD/M_OK are deferred/out of place.
            Event::Command(other) => Err(SessionError::Unexpected {
                phase: self.phase.name(),
                event: command_name(other.id()),
            }),
        }
    }

    fn on_file(&mut self, info: FileInfo) -> Result<Vec<Action>, SessionError> {
        if self.receiving.is_some() {
            // One inbound file at a time (pipelining deferred).
            return Err(SessionError::Unexpected {
                phase: self.phase.name(),
                event: "M_FILE",
            });
        }
        let id = info.id();
        let mut actions = vec![Action::ExpectFile(info.clone())];
        if info.size == 0 {
            // Empty file: complete immediately, no data blocks expected.
            actions.push(Action::SendCommand(Command::Got(id.clone())));
            actions.push(Action::FileComplete(id));
            actions.extend(self.maybe_finish());
        } else {
            self.receiving = Some((info, 0));
        }
        Ok(actions)
    }

    fn on_data(&mut self, bytes: Vec<u8>) -> Result<Vec<Action>, SessionError> {
        let (info, received) = match self.receiving.as_mut() {
            Some(pair) => pair,
            None => return Err(SessionError::UnexpectedData),
        };
        *received += bytes.len() as u64;
        let complete = *received >= info.size;
        let mut actions = vec![Action::WriteData(bytes)];
        if complete {
            let id = info.id();
            self.receiving = None;
            actions.push(Action::SendCommand(Command::Got(id.clone())));
            actions.push(Action::FileComplete(id));
            actions.extend(self.maybe_finish());
        }
        Ok(actions)
    }

    // -- helpers -----------------------------------------------------------

    fn enter_transfer(&mut self) -> Vec<Action> {
        self.phase = Phase::Transfer;
        self.start_next_send()
    }

    /// Announce the next outbound file, or emit `M_EOB` if none remain.
    fn start_next_send(&mut self) -> Vec<Action> {
        if let Some(info) = self.outgoing.pop_front() {
            self.sending = Some(info.id());
            vec![
                Action::SendCommand(Command::File(info.clone())),
                Action::StreamFile(info),
            ]
        } else {
            if !self.local_eob {
                self.local_eob = true;
                let mut actions = vec![Action::SendCommand(Command::Eob)];
                actions.extend(self.maybe_finish());
                actions
            } else {
                self.maybe_finish()
            }
        }
    }

    fn maybe_finish(&mut self) -> Vec<Action> {
        if self.local_eob
            && self.peer_eob
            && self.sending.is_none()
            && self.receiving.is_none()
            && self.phase == Phase::Transfer
        {
            self.phase = Phase::Done;
            vec![Action::Finished]
        } else {
            Vec::new()
        }
    }

    /// The `M_PWD` value to send (originating side).
    fn password_value(&self) -> String {
        if self.password.is_empty() || self.password == "-" {
            return "-".to_string();
        }
        match &self.peer_challenge {
            Some(chal) => cram_md5_response(self.password.as_bytes(), chal),
            None => self.password.clone(),
        }
    }

    /// Verify a received `M_PWD` (answering side).
    fn verify_password(&self, pw: &str) -> bool {
        if self.password.is_empty() || self.password == "-" {
            return true; // unsecured session accepts anything
        }
        if let Some(chal) = &self.challenge {
            let expected = cram_md5_response(self.password.as_bytes(), chal);
            if pw == expected {
                return true;
            }
        }
        // Plaintext fallback (deferred hardening: constant-time compare).
        pw == self.password
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::FileInfo;

    fn addr(node: u16) -> Address {
        Address::new(2, 5020, node, 0).with_domain("fidonet")
    }

    fn sent_commands(actions: &[Action]) -> Vec<&Command> {
        actions
            .iter()
            .filter_map(|a| match a {
                Action::SendCommand(c) => Some(c),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn originating_start_emits_nul_and_adr() {
        let mut s = Session::originating(SessionConfig {
            addresses: vec![addr(1)],
            system_info: vec!["SYS RabbitHole".into()],
            password: "secret".into(),
            ..Default::default()
        });
        let actions = s.start();
        let cmds = sent_commands(&actions);
        assert_eq!(cmds[0], &Command::Nul("SYS RabbitHole".into()));
        assert_eq!(cmds[1], &Command::Adr(vec![addr(1)]));
        // start is idempotent.
        assert!(s.start().is_empty());
    }

    #[test]
    fn answering_start_advertises_challenge() {
        let mut s = Session::answering(SessionConfig {
            addresses: vec![addr(2)],
            password: "secret".into(),
            challenge: Some(vec![0xde, 0xad, 0xbe, 0xef]),
            ..Default::default()
        });
        let actions = s.start();
        let cmds = sent_commands(&actions);
        assert!(matches!(cmds[0], Command::Nul(l) if l.contains("CRAM-MD5-deadbeef")));
        assert_eq!(cmds[1], &Command::Adr(vec![addr(2)]));
    }

    #[test]
    fn originating_answers_cram_pwd_after_greeting() {
        let mut s = Session::originating(SessionConfig {
            addresses: vec![addr(1)],
            password: "secret".into(),
            ..Default::default()
        });
        s.start();
        // Peer NUL carries the challenge, then peer ADR.
        assert!(s
            .advance(Event::Command(Command::Nul("OPT CRAM-MD5-deadbeef".into())))
            .unwrap()
            .is_empty());
        assert_eq!(s.peer_challenge(), Some(&[0xde, 0xad, 0xbe, 0xef][..]));
        let actions = s
            .advance(Event::Command(Command::Adr(vec![addr(2)])))
            .unwrap();
        let expected = cram_md5_response(b"secret", &[0xde, 0xad, 0xbe, 0xef]);
        assert_eq!(sent_commands(&actions), vec![&Command::Pwd(expected)]);
        assert_eq!(s.phase(), Phase::AwaitOk);
        assert_eq!(s.peer_addresses(), &[addr(2)]);
    }

    #[test]
    fn originating_plaintext_when_no_challenge() {
        let mut s = Session::originating(SessionConfig {
            addresses: vec![addr(1)],
            password: "secret".into(),
            ..Default::default()
        });
        s.start();
        let actions = s
            .advance(Event::Command(Command::Adr(vec![addr(2)])))
            .unwrap();
        assert_eq!(
            sent_commands(&actions),
            vec![&Command::Pwd("secret".into())]
        );
    }

    #[test]
    fn unsecured_session_sends_dash() {
        let mut s = Session::originating(SessionConfig {
            addresses: vec![addr(1)],
            password: String::new(),
            ..Default::default()
        });
        s.start();
        let actions = s
            .advance(Event::Command(Command::Adr(vec![addr(2)])))
            .unwrap();
        assert_eq!(sent_commands(&actions), vec![&Command::Pwd("-".into())]);
    }

    #[test]
    fn answering_verifies_cram_and_sends_ok() {
        let challenge = vec![1, 2, 3, 4];
        let mut s = Session::answering(SessionConfig {
            addresses: vec![addr(2)],
            password: "secret".into(),
            challenge: Some(challenge.clone()),
            ..Default::default()
        });
        s.start();
        s.advance(Event::Command(Command::Adr(vec![addr(1)])))
            .unwrap();
        let response = cram_md5_response(b"secret", &challenge);
        let actions = s.advance(Event::Command(Command::Pwd(response))).unwrap();
        assert!(actions.contains(&Action::Authenticated));
        assert!(sent_commands(&actions).contains(&&Command::Ok("secure".into())));
        assert_eq!(s.phase(), Phase::Transfer);
    }

    #[test]
    fn answering_rejects_bad_password() {
        let mut s = Session::answering(SessionConfig {
            addresses: vec![addr(2)],
            password: "secret".into(),
            challenge: Some(vec![1, 2, 3, 4]),
            ..Default::default()
        });
        s.start();
        s.advance(Event::Command(Command::Adr(vec![addr(1)])))
            .unwrap();
        let actions = s
            .advance(Event::Command(Command::Pwd("CRAM-MD5-00".into())))
            .unwrap();
        assert!(matches!(actions.last(), Some(Action::Aborted(_))));
        assert_eq!(s.phase(), Phase::Failed);
    }

    #[test]
    fn transfer_receives_a_file_and_acks() {
        let mut s = Session::answering(SessionConfig {
            addresses: vec![addr(2)],
            password: String::new(),
            ..Default::default()
        });
        s.start();
        s.advance(Event::Command(Command::Adr(vec![addr(1)])))
            .unwrap();
        s.advance(Event::Command(Command::Pwd("-".into()))).unwrap();
        assert_eq!(s.phase(), Phase::Transfer);

        // Peer announces a 5-byte file.
        let info = FileInfo::new("hi.txt", 5, 100);
        let actions = s
            .advance(Event::Command(Command::File(info.clone())))
            .unwrap();
        assert_eq!(actions, vec![Action::ExpectFile(info.clone())]);

        let actions = s.advance(Event::Data(b"hel".to_vec())).unwrap();
        assert_eq!(actions, vec![Action::WriteData(b"hel".to_vec())]);

        let actions = s.advance(Event::Data(b"lo".to_vec())).unwrap();
        assert_eq!(actions[0], Action::WriteData(b"lo".to_vec()));
        assert!(sent_commands(&actions).contains(&&Command::Got(info.id())));
        assert!(actions.contains(&Action::FileComplete(info.id())));
    }

    #[test]
    fn empty_file_completes_immediately() {
        let mut s = Session::answering(SessionConfig {
            password: String::new(),
            ..Default::default()
        });
        s.start();
        s.advance(Event::Command(Command::Adr(vec![addr(1)])))
            .unwrap();
        s.advance(Event::Command(Command::Pwd("-".into()))).unwrap();
        let info = FileInfo::new("empty", 0, 1);
        let actions = s
            .advance(Event::Command(Command::File(info.clone())))
            .unwrap();
        assert_eq!(actions[0], Action::ExpectFile(info.clone()));
        assert!(sent_commands(&actions).contains(&&Command::Got(info.id())));
    }

    #[test]
    fn data_without_file_is_error() {
        let mut s = Session::answering(SessionConfig {
            password: String::new(),
            ..Default::default()
        });
        s.start();
        s.advance(Event::Command(Command::Adr(vec![addr(1)])))
            .unwrap();
        s.advance(Event::Command(Command::Pwd("-".into()))).unwrap();
        assert_eq!(
            s.advance(Event::Data(vec![1, 2, 3])),
            Err(SessionError::UnexpectedData)
        );
    }

    #[test]
    fn out_of_order_event_is_rejected() {
        let mut s = Session::originating(SessionConfig::default());
        s.start();
        let err = s.advance(Event::Command(Command::Eob)).unwrap_err();
        assert!(matches!(err, SessionError::Unexpected { .. }));
    }

    /// Drive both sides against each other through a whole session, exchanging
    /// *encoded wire blocks* end to end, transferring one file each way.
    #[test]
    fn full_two_sided_transfer_over_the_wire() {
        use crate::command::Command;
        use crate::frame::{decode_block, RawBlock};

        let challenge = vec![0xca, 0xfe, 0xba, 0xbe];
        let orig_file = FileInfo::new("out.pkt", 11, 100);
        let answ_file = FileInfo::new("in.pkt", 7, 200);

        let mut orig = Session::originating(SessionConfig {
            addresses: vec![addr(1)],
            system_info: vec!["SYS Caller".into()],
            password: "pw".into(),
            outgoing: vec![orig_file.clone()],
            ..Default::default()
        });
        let mut answ = Session::answering(SessionConfig {
            addresses: vec![addr(2)],
            system_info: vec!["SYS Server".into()],
            password: "pw".into(),
            challenge: Some(challenge.clone()),
            outgoing: vec![answ_file.clone()],
        });

        let orig_bytes = b"hello world".to_vec();
        let answ_bytes = b"welcome".to_vec();

        // Wire queues: blocks travelling toward each peer.
        let mut to_answ: VecDeque<Vec<u8>> = VecDeque::new();
        let mut to_orig: VecDeque<Vec<u8>> = VecDeque::new();

        let mut orig_recv: Vec<u8> = Vec::new();
        let mut answ_recv: Vec<u8> = Vec::new();
        let mut orig_done = false;
        let mut answ_done = false;

        // Encode a side's actions onto the wire toward the other side.
        fn pump(
            actions: Vec<Action>,
            out: &mut VecDeque<Vec<u8>>,
            file_bytes: &[u8],
            recv: &mut Vec<u8>,
            done: &mut bool,
        ) {
            for action in actions {
                match action {
                    Action::SendCommand(cmd) => out.push_back(cmd.to_block().encode().unwrap()),
                    Action::StreamFile(_) => {
                        // Stream the file bytes as a single data block.
                        out.push_back(RawBlock::Data(file_bytes.to_vec()).encode().unwrap());
                    }
                    Action::WriteData(bytes) => recv.extend_from_slice(&bytes),
                    Action::Finished => *done = true,
                    Action::ExpectFile(_) | Action::FileComplete(_) | Action::Authenticated => {}
                    Action::Aborted(reason) => panic!("aborted: {reason}"),
                }
            }
        }

        pump(
            orig.start(),
            &mut to_answ,
            &orig_bytes,
            &mut orig_recv,
            &mut orig_done,
        );
        pump(
            answ.start(),
            &mut to_orig,
            &answ_bytes,
            &mut answ_recv,
            &mut answ_done,
        );

        // Turn a wire block into an Event.
        fn to_event(block: RawBlock) -> Event {
            match block {
                RawBlock::Command { id, args } => {
                    Event::Command(Command::parse(id, &args).unwrap())
                }
                RawBlock::Data(d) => Event::Data(d),
            }
        }

        for _ in 0..200 {
            if let Some(wire) = to_answ.pop_front() {
                let (block, _) = decode_block(&wire).unwrap();
                let actions = answ.advance(to_event(block)).unwrap();
                pump(
                    actions,
                    &mut to_orig,
                    &answ_bytes,
                    &mut answ_recv,
                    &mut answ_done,
                );
            } else if let Some(wire) = to_orig.pop_front() {
                let (block, _) = decode_block(&wire).unwrap();
                let actions = orig.advance(to_event(block)).unwrap();
                pump(
                    actions,
                    &mut to_answ,
                    &orig_bytes,
                    &mut orig_recv,
                    &mut orig_done,
                );
            } else {
                break;
            }
        }

        assert!(orig_done, "originating side did not finish");
        assert!(answ_done, "answering side did not finish");
        assert_eq!(orig.phase(), Phase::Done);
        assert_eq!(answ.phase(), Phase::Done);
        // Each side received the other's file bytes.
        assert_eq!(answ_recv, orig_bytes); // server got the caller's file
        assert_eq!(orig_recv, answ_bytes); // caller got the server's file
    }
}
