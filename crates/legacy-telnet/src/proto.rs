//! Sans-IO telnet byte codec (RFC 854/855).
//!
//! [`Parser`] is a push parser: feed it raw socket bytes, get back ordered
//! [`Event`]s — plain data (with `IAC IAC` undoubled), option negotiation
//! commands, `SB…SE` subnegotiations, and bare commands (NOP, GA, AYT, …).
//! It holds partial state across feeds, so commands split at read boundaries
//! parse correctly. [`escape_iac`] is the mirror for the write side.

/// Interpret As Command — every telnet command starts with this byte.
pub const IAC: u8 = 255;
/// Demand the peer stop performing an option.
pub const DONT: u8 = 254;
/// Ask the peer to perform an option.
pub const DO: u8 = 253;
/// Refuse to perform an option ourselves.
pub const WONT: u8 = 252;
/// Offer to perform an option ourselves.
pub const WILL: u8 = 251;
/// Subnegotiation begin.
pub const SB: u8 = 250;
/// Subnegotiation end.
pub const SE: u8 = 240;
/// No-operation.
pub const NOP: u8 = 241;
/// Go ahead (half-duplex relic; ignored).
pub const GA: u8 = 249;

/// Option codes this crate knows about.
pub mod opt {
    /// Echo (RFC 857) — we offer it: the server echoes what you type.
    pub const ECHO: u8 = 1;
    /// Suppress Go Ahead (RFC 858) — full-duplex, both directions.
    pub const SGA: u8 = 3;
    /// Terminal Type (RFC 1091) — we ask the peer for it.
    pub const TTYPE: u8 = 24;
    /// Negotiate About Window Size (RFC 1073) — we ask the peer for it.
    pub const NAWS: u8 = 31;
}

/// TTYPE subnegotiation verb: the peer is telling us its terminal type.
pub const TTYPE_IS: u8 = 0;
/// TTYPE subnegotiation verb: we are asking the peer for its terminal type.
pub const TTYPE_SEND: u8 = 1;

/// Subnegotiation payloads longer than this are truncated (defense against
/// a peer that opens `SB` and never sends `SE`).
const MAX_SUBNEG: usize = 1024;

/// One decoded unit of the inbound telnet stream, in wire order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// Plain data bytes; `IAC IAC` has already been undoubled to one `0xFF`.
    Data(Vec<u8>),
    /// `IAC WILL <option>` — the peer offers to perform an option.
    Will(u8),
    /// `IAC WONT <option>` — the peer refuses or stops an option.
    Wont(u8),
    /// `IAC DO <option>` — the peer asks us to perform an option.
    Do(u8),
    /// `IAC DONT <option>` — the peer forbids us an option.
    Dont(u8),
    /// `IAC SB <option> …payload… IAC SE`, payload with `IAC IAC` undoubled.
    Subnegotiation(u8, Vec<u8>),
    /// Any other `IAC <command>` (NOP, GA, AYT, …).
    Command(u8),
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum State {
    /// Plain data flow.
    #[default]
    Data,
    /// Seen `IAC`, awaiting the command byte.
    Iac,
    /// Seen `IAC <WILL|WONT|DO|DONT>`, awaiting the option byte.
    Opt(u8),
    /// Seen `IAC SB`, awaiting the option byte.
    SubOpt,
    /// Inside a subnegotiation payload for the given option.
    Sub(u8),
    /// Inside a subnegotiation, seen `IAC` — next byte decides.
    SubIac(u8),
}

/// Incremental telnet parser; safe to feed arbitrary chunk boundaries.
#[derive(Debug, Default)]
pub struct Parser {
    state: State,
    subbuf: Vec<u8>,
}

impl Parser {
    /// A parser in the initial (plain data) state.
    pub fn new() -> Parser {
        Parser::default()
    }

    /// Consume `input`, appending decoded [`Event`]s to `events` in order.
    pub fn feed(&mut self, input: &[u8], events: &mut Vec<Event>) {
        let mut data: Vec<u8> = Vec::new();
        for &b in input {
            match self.state {
                State::Data => {
                    if b == IAC {
                        self.state = State::Iac;
                    } else {
                        data.push(b);
                    }
                }
                State::Iac => self.after_iac(b, &mut data, events),
                State::Opt(cmd) => {
                    flush_data(&mut data, events);
                    events.push(match cmd {
                        WILL => Event::Will(b),
                        WONT => Event::Wont(b),
                        DO => Event::Do(b),
                        _ => Event::Dont(b),
                    });
                    self.state = State::Data;
                }
                State::SubOpt => self.state = State::Sub(b),
                State::Sub(opt) => {
                    if b == IAC {
                        self.state = State::SubIac(opt);
                    } else {
                        self.push_sub(b);
                    }
                }
                State::SubIac(opt) => match b {
                    IAC => {
                        self.push_sub(IAC);
                        self.state = State::Sub(opt);
                    }
                    SE => {
                        flush_data(&mut data, events);
                        events.push(Event::Subnegotiation(opt, std::mem::take(&mut self.subbuf)));
                        self.state = State::Data;
                    }
                    // Protocol error: the peer forgot `IAC SE`. Salvage what
                    // we have and re-interpret this byte as a fresh command.
                    other => {
                        flush_data(&mut data, events);
                        events.push(Event::Subnegotiation(opt, std::mem::take(&mut self.subbuf)));
                        self.after_iac(other, &mut data, events);
                    }
                },
            }
        }
        flush_data(&mut data, events);
    }

    /// Handle the byte following a bare `IAC` in the data stream.
    fn after_iac(&mut self, b: u8, data: &mut Vec<u8>, events: &mut Vec<Event>) {
        match b {
            IAC => {
                data.push(IAC);
                self.state = State::Data;
            }
            WILL | WONT | DO | DONT => self.state = State::Opt(b),
            SB => {
                self.subbuf.clear();
                self.state = State::SubOpt;
            }
            other => {
                flush_data(data, events);
                events.push(Event::Command(other));
                self.state = State::Data;
            }
        }
    }

    fn push_sub(&mut self, b: u8) {
        if self.subbuf.len() < MAX_SUBNEG {
            self.subbuf.push(b);
        }
    }
}

fn flush_data(data: &mut Vec<u8>, events: &mut Vec<Event>) {
    if !data.is_empty() {
        events.push(Event::Data(std::mem::take(data)));
    }
}

/// Double every `0xFF` so it survives the wire as a data byte (RFC 854).
pub fn escape_iac(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len());
    for &b in bytes {
        out.push(b);
        if b == IAC {
            out.push(IAC);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(bytes: &[u8]) -> Vec<Event> {
        let mut p = Parser::new();
        let mut ev = Vec::new();
        p.feed(bytes, &mut ev);
        ev
    }

    #[test]
    fn plain_data_passes_through() {
        assert_eq!(parse(b"hello"), vec![Event::Data(b"hello".to_vec())]);
    }

    #[test]
    fn iac_iac_undoubles_to_data() {
        assert_eq!(
            parse(&[b'A', IAC, IAC, b'B']),
            vec![Event::Data(vec![b'A', 0xFF, b'B'])]
        );
    }

    #[test]
    fn negotiation_commands_parse_in_order() {
        let ev = parse(&[
            IAC,
            WILL,
            opt::NAWS,
            b'x',
            IAC,
            DO,
            opt::ECHO,
            IAC,
            WONT,
            opt::TTYPE,
            IAC,
            DONT,
            9,
        ]);
        assert_eq!(
            ev,
            vec![
                Event::Will(opt::NAWS),
                Event::Data(vec![b'x']),
                Event::Do(opt::ECHO),
                Event::Wont(opt::TTYPE),
                Event::Dont(9),
            ]
        );
    }

    #[test]
    fn subnegotiation_with_doubled_iac_payload() {
        let ev = parse(&[IAC, SB, opt::NAWS, 0, IAC, IAC, 0, 24, IAC, SE]);
        assert_eq!(
            ev,
            vec![Event::Subnegotiation(opt::NAWS, vec![0, 0xFF, 0, 24])]
        );
    }

    #[test]
    fn commands_split_across_feeds() {
        let mut p = Parser::new();
        let mut ev = Vec::new();
        p.feed(&[b'a', IAC], &mut ev);
        assert_eq!(ev, vec![Event::Data(vec![b'a'])]);
        p.feed(&[DO], &mut ev);
        p.feed(&[opt::SGA, IAC, SB, opt::TTYPE], &mut ev);
        p.feed(&[TTYPE_IS, b'a', b'n', b's', b'i', IAC], &mut ev);
        p.feed(&[SE, b'z'], &mut ev);
        assert_eq!(
            ev,
            vec![
                Event::Data(vec![b'a']),
                Event::Do(opt::SGA),
                Event::Subnegotiation(opt::TTYPE, b"\x00ansi".to_vec()),
                Event::Data(vec![b'z']),
            ]
        );
    }

    #[test]
    fn bare_commands_and_unterminated_subneg() {
        assert_eq!(
            parse(&[IAC, NOP, IAC, GA]),
            vec![Event::Command(NOP), Event::Command(GA)]
        );
        // Missing IAC SE: salvage the payload, then parse the stray command.
        let ev = parse(&[IAC, SB, opt::TTYPE, TTYPE_IS, b'v', IAC, DO, opt::ECHO]);
        assert_eq!(
            ev,
            vec![
                Event::Subnegotiation(opt::TTYPE, vec![TTYPE_IS, b'v']),
                Event::Do(opt::ECHO),
            ]
        );
    }

    #[test]
    fn oversized_subnegotiation_is_truncated() {
        let mut bytes = vec![IAC, SB, opt::TTYPE];
        bytes.extend(std::iter::repeat_n(b'x', 5000));
        bytes.extend([IAC, SE]);
        let ev = parse(&bytes);
        match &ev[0] {
            Event::Subnegotiation(o, payload) => {
                assert_eq!(*o, opt::TTYPE);
                assert_eq!(payload.len(), 1024);
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }

    #[test]
    fn escape_iac_doubles_ff() {
        assert_eq!(escape_iac(&[1, 0xFF, 2]), vec![1, 0xFF, 0xFF, 2]);
        assert_eq!(escape_iac(b"plain"), b"plain".to_vec());
    }
}
