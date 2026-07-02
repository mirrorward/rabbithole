//! Loop-safe telnet option negotiation (RFC 855, simplified Q-method).
//!
//! [`Negotiator`] tracks one state per (side, option) pair. *Local* options
//! are ones we perform (we send WILL/WONT, the peer sends DO/DONT): ECHO and
//! SGA. *Remote* options are ones we want the peer to perform (we send
//! DO/DONT, the peer sends WILL/WONT): SGA, NAWS, and TTYPE. Every other
//! option is refused with DONT/WONT.
//!
//! Loop safety follows RFC 1143's core rules: never reply to a negative with
//! a negative, never re-request a state we are already in or have already
//! asked for. This slice only ever *enables* options, so the state space is
//! `No → WantYes → Yes → No` — there is no locally-initiated disable yet.

use crate::proto::{opt, DO, DONT, IAC, WILL, WONT};

/// Per-option negotiation state.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum OptState {
    /// Off (initial, refused, or disabled by the peer).
    #[default]
    No,
    /// We sent our request (WILL for local, DO for remote); awaiting reply.
    WantYes,
    /// Both sides agreed; the option is in effect.
    Yes,
}

/// State-change notification produced while absorbing a peer command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Notice {
    /// The peer now performs `option` (its WILL was accepted or confirmed).
    RemoteEnabled(u8),
    /// The peer stopped performing `option` (WONT after it was on).
    RemoteDisabled(u8),
    /// We now perform `option` (our WILL was accepted, or a DO we agreed to).
    LocalEnabled(u8),
    /// We stopped performing `option` (DONT after it was on).
    LocalDisabled(u8),
}

/// The negotiation state machine. Feed it peer commands via `on_*`; each
/// call appends any required reply bytes to `out` and reports state changes.
#[derive(Debug)]
pub struct Negotiator {
    /// Options we are willing to perform (WILL side).
    local: [(u8, OptState); 2],
    /// Options we want the peer to perform (DO side).
    remote: [(u8, OptState); 3],
}

impl Default for Negotiator {
    fn default() -> Negotiator {
        Negotiator::new()
    }
}

impl Negotiator {
    /// A fresh machine: ECHO + SGA offered locally; SGA, NAWS, TTYPE wanted
    /// from the peer. Nothing sent yet — call [`Negotiator::offer_all`].
    pub fn new() -> Negotiator {
        Negotiator {
            local: [(opt::ECHO, OptState::No), (opt::SGA, OptState::No)],
            remote: [
                (opt::SGA, OptState::No),
                (opt::NAWS, OptState::No),
                (opt::TTYPE, OptState::No),
            ],
        }
    }

    /// Open negotiation: WILL every local option, DO every remote option.
    /// Idempotent — options already requested or enabled are skipped.
    pub fn offer_all(&mut self, out: &mut Vec<u8>) {
        for (o, state) in self.local.iter_mut() {
            if *state == OptState::No {
                out.extend([IAC, WILL, *o]);
                *state = OptState::WantYes;
            }
        }
        for (o, state) in self.remote.iter_mut() {
            if *state == OptState::No {
                out.extend([IAC, DO, *o]);
                *state = OptState::WantYes;
            }
        }
    }

    /// Peer sent `IAC WILL option`.
    pub fn on_will(&mut self, option: u8, out: &mut Vec<u8>) -> Option<Notice> {
        match find(&mut self.remote, option) {
            Some(state) => match *state {
                OptState::No => {
                    *state = OptState::Yes;
                    out.extend([IAC, DO, option]);
                    Some(Notice::RemoteEnabled(option))
                }
                OptState::WantYes => {
                    *state = OptState::Yes;
                    Some(Notice::RemoteEnabled(option))
                }
                OptState::Yes => None,
            },
            None => {
                out.extend([IAC, DONT, option]);
                None
            }
        }
    }

    /// Peer sent `IAC WONT option`.
    pub fn on_wont(&mut self, option: u8, out: &mut Vec<u8>) -> Option<Notice> {
        match find(&mut self.remote, option) {
            Some(state) => match *state {
                OptState::No => None, // never answer a negative with a negative
                OptState::WantYes => {
                    *state = OptState::No; // our DO was refused
                    None
                }
                OptState::Yes => {
                    *state = OptState::No;
                    out.extend([IAC, DONT, option]);
                    Some(Notice::RemoteDisabled(option))
                }
            },
            None => None,
        }
    }

    /// Peer sent `IAC DO option`.
    pub fn on_do(&mut self, option: u8, out: &mut Vec<u8>) -> Option<Notice> {
        match find(&mut self.local, option) {
            Some(state) => match *state {
                OptState::No => {
                    *state = OptState::Yes;
                    out.extend([IAC, WILL, option]);
                    Some(Notice::LocalEnabled(option))
                }
                OptState::WantYes => {
                    *state = OptState::Yes;
                    Some(Notice::LocalEnabled(option))
                }
                OptState::Yes => None,
            },
            None => {
                out.extend([IAC, WONT, option]);
                None
            }
        }
    }

    /// Peer sent `IAC DONT option`.
    pub fn on_dont(&mut self, option: u8, out: &mut Vec<u8>) -> Option<Notice> {
        match find(&mut self.local, option) {
            Some(state) => match *state {
                OptState::No => None,
                OptState::WantYes => {
                    *state = OptState::No; // our WILL was refused
                    None
                }
                OptState::Yes => {
                    *state = OptState::No;
                    out.extend([IAC, WONT, option]);
                    Some(Notice::LocalDisabled(option))
                }
            },
            None => None,
        }
    }

    /// Is the peer confirmed to be performing `option`?
    pub fn remote_enabled(&self, option: u8) -> bool {
        self.remote
            .iter()
            .any(|&(o, s)| o == option && s == OptState::Yes)
    }

    /// Are we confirmed to be performing `option`?
    pub fn local_enabled(&self, option: u8) -> bool {
        self.local
            .iter()
            .any(|&(o, s)| o == option && s == OptState::Yes)
    }

    /// Are we performing `option`, or have we offered it and not been
    /// refused? (Used to decide whether to echo before the peer replies.)
    pub fn local_active(&self, option: u8) -> bool {
        self.local
            .iter()
            .any(|&(o, s)| o == option && s != OptState::No)
    }
}

fn find(list: &mut [(u8, OptState)], option: u8) -> Option<&mut OptState> {
    list.iter_mut().find(|(o, _)| *o == option).map(|(_, s)| s)
}

#[cfg(test)]
mod tests {
    use super::*;

    const LINEMODE: u8 = 34;

    #[test]
    fn offers_all_supported_options_once() {
        let mut n = Negotiator::new();
        let mut out = Vec::new();
        n.offer_all(&mut out);
        assert_eq!(
            out,
            vec![
                IAC,
                WILL,
                opt::ECHO,
                IAC,
                WILL,
                opt::SGA,
                IAC,
                DO,
                opt::SGA,
                IAC,
                DO,
                opt::NAWS,
                IAC,
                DO,
                opt::TTYPE,
            ]
        );
        out.clear();
        n.offer_all(&mut out); // idempotent while pending
        assert!(out.is_empty());
    }

    #[test]
    fn accepting_our_offers_sends_no_extra_bytes() {
        let mut n = Negotiator::new();
        let mut out = Vec::new();
        n.offer_all(&mut out);
        out.clear();
        assert_eq!(
            n.on_do(opt::ECHO, &mut out),
            Some(Notice::LocalEnabled(opt::ECHO))
        );
        assert_eq!(
            n.on_will(opt::NAWS, &mut out),
            Some(Notice::RemoteEnabled(opt::NAWS))
        );
        assert!(out.is_empty(), "no reply to an accept of our own request");
        assert!(n.local_enabled(opt::ECHO));
        assert!(n.remote_enabled(opt::NAWS));
    }

    #[test]
    fn unsolicited_supported_requests_are_acked() {
        let mut n = Negotiator::new();
        let mut out = Vec::new();
        assert_eq!(
            n.on_will(opt::TTYPE, &mut out),
            Some(Notice::RemoteEnabled(opt::TTYPE))
        );
        assert_eq!(out, vec![IAC, DO, opt::TTYPE]);
        out.clear();
        assert_eq!(
            n.on_do(opt::SGA, &mut out),
            Some(Notice::LocalEnabled(opt::SGA))
        );
        assert_eq!(out, vec![IAC, WILL, opt::SGA]);
    }

    #[test]
    fn unsupported_options_are_refused() {
        let mut n = Negotiator::new();
        let mut out = Vec::new();
        assert_eq!(n.on_will(LINEMODE, &mut out), None);
        assert_eq!(out, vec![IAC, DONT, LINEMODE]);
        out.clear();
        assert_eq!(n.on_do(LINEMODE, &mut out), None);
        assert_eq!(out, vec![IAC, WONT, LINEMODE]);
        out.clear();
        // Negative for an unknown option: stay silent (no loops).
        assert_eq!(n.on_wont(LINEMODE, &mut out), None);
        assert_eq!(n.on_dont(LINEMODE, &mut out), None);
        assert!(out.is_empty());
    }

    #[test]
    fn repeated_requests_do_not_loop() {
        let mut n = Negotiator::new();
        let mut out = Vec::new();
        n.on_will(opt::NAWS, &mut out);
        assert_eq!(out, vec![IAC, DO, opt::NAWS]);
        out.clear();
        assert_eq!(n.on_will(opt::NAWS, &mut out), None);
        assert!(out.is_empty(), "second WILL must not re-ack");
    }

    #[test]
    fn refusal_of_our_request_is_absorbed_silently() {
        let mut n = Negotiator::new();
        let mut out = Vec::new();
        n.offer_all(&mut out);
        out.clear();
        assert_eq!(n.on_wont(opt::NAWS, &mut out), None);
        assert_eq!(n.on_dont(opt::ECHO, &mut out), None);
        assert!(out.is_empty());
        assert!(!n.remote_enabled(opt::NAWS));
        assert!(!n.local_active(opt::ECHO));
    }

    #[test]
    fn disable_after_enable_is_acked_once() {
        let mut n = Negotiator::new();
        let mut out = Vec::new();
        n.offer_all(&mut out);
        n.on_will(opt::NAWS, &mut out);
        n.on_do(opt::ECHO, &mut out);
        out.clear();

        assert_eq!(
            n.on_wont(opt::NAWS, &mut out),
            Some(Notice::RemoteDisabled(opt::NAWS))
        );
        assert_eq!(out, vec![IAC, DONT, opt::NAWS]);
        out.clear();
        assert_eq!(n.on_wont(opt::NAWS, &mut out), None);
        assert!(out.is_empty());

        assert_eq!(
            n.on_dont(opt::ECHO, &mut out),
            Some(Notice::LocalDisabled(opt::ECHO))
        );
        assert_eq!(out, vec![IAC, WONT, opt::ECHO]);
        assert!(!n.local_enabled(opt::ECHO));
    }
}
