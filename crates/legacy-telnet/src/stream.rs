//! [`TelnetStream`]: the protocol layer bound to a transport.
//!
//! Wraps any `AsyncRead + AsyncWrite` (a `TcpStream` in production, an
//! in-memory duplex in tests) and layers on: automatic option negotiation
//! (replies, TTYPE `SEND` follow-up, NAWS/TTYPE capture), a line-mode reader
//! with CR/LF and backspace handling plus server-side echo, and an
//! encoding-aware writer that translates `\n` → `\r\n` and doubles IAC.

use std::collections::VecDeque;
use std::io;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::encoding::{decode, encode_into, Encoding};
use crate::negotiate::{Negotiator, Notice};
use crate::proto::{escape_iac, opt, Event, Parser, IAC, SB, SE, TTYPE_IS, TTYPE_SEND};

/// Longest accepted input line, in bytes; further input is dropped.
const MAX_LINE: usize = 1024;

/// A high-level inbound item, after negotiation has been absorbed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Input {
    /// Plain data bytes (IAC already undoubled).
    Data(Vec<u8>),
    /// The peer reported its window size via NAWS.
    WindowSize {
        /// Columns (0 means "unspecified" per RFC 1073).
        cols: u16,
        /// Rows (0 means "unspecified" per RFC 1073).
        rows: u16,
    },
    /// The peer reported its terminal type via TTYPE `IS`.
    TerminalType(String),
}

/// Echo behavior for [`TelnetStream::read_line`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Echo {
    /// Echo typed characters back (when we hold the ECHO option).
    On,
    /// Echo nothing — passwords.
    Hidden,
}

/// A telnet session over `S`, owning the parser and negotiation state.
#[derive(Debug)]
pub struct TelnetStream<S> {
    io: S,
    parser: Parser,
    neg: Negotiator,
    events: VecDeque<Event>,
    encoding: Encoding,
    window: Option<(u16, u16)>,
    terminal: Option<String>,
    ttype_requested: bool,
    /// A CR just ended a line; swallow one following LF/NUL (telnet NVT
    /// sends CR LF or CR NUL), even across read boundaries.
    swallow_lf: bool,
    /// Partial line accumulated by [`TelnetStream::read_line`]. Living on
    /// the stream (not the future) makes `read_line` **cancel-safe**: a
    /// caller may race it in `tokio::select!` (e.g. a chat screen splicing
    /// bus events between keystrokes) and re-call it without losing what
    /// the user already typed.
    line_buf: Vec<u8>,
}

impl<S: AsyncRead + AsyncWrite + Unpin> TelnetStream<S> {
    /// Wrap a transport. Nothing is sent until [`TelnetStream::start`].
    pub fn new(io: S) -> TelnetStream<S> {
        TelnetStream {
            io,
            parser: Parser::new(),
            neg: Negotiator::new(),
            events: VecDeque::new(),
            encoding: Encoding::default(),
            window: None,
            terminal: None,
            ttype_requested: false,
            swallow_lf: false,
            line_buf: Vec::new(),
        }
    }

    /// Set the output/input character encoding (default UTF-8).
    pub fn set_encoding(&mut self, enc: Encoding) {
        self.encoding = enc;
    }

    /// The current character encoding.
    pub fn encoding(&self) -> Encoding {
        self.encoding
    }

    /// Last window size the peer reported via NAWS, `(cols, rows)`.
    pub fn window(&self) -> Option<(u16, u16)> {
        self.window
    }

    /// Terminal type the peer reported via TTYPE, if any.
    pub fn terminal(&self) -> Option<&str> {
        self.terminal.as_deref()
    }

    /// Open negotiation: offer ECHO + SGA, request SGA + NAWS + TTYPE.
    pub async fn start(&mut self) -> io::Result<()> {
        let mut out = Vec::new();
        self.neg.offer_all(&mut out);
        self.io.write_all(&out).await?;
        self.io.flush().await
    }

    /// Next high-level input. Negotiation traffic is handled internally
    /// (replies written, NAWS/TTYPE captured — and also surfaced as
    /// [`Input`] items so callers *can* react). `None` means EOF.
    pub async fn next_input(&mut self) -> io::Result<Option<Input>> {
        loop {
            while let Some(ev) = self.events.pop_front() {
                if let Some(input) = self.absorb(ev).await? {
                    return Ok(Some(input));
                }
            }
            if !self.fill().await? {
                return Ok(None);
            }
        }
    }

    /// Process one parsed event; returns an [`Input`] if it surfaces one.
    async fn absorb(&mut self, ev: Event) -> io::Result<Option<Input>> {
        let mut out = Vec::new();
        let notice = match ev {
            Event::Data(d) => return Ok(Some(Input::Data(d))),
            Event::Will(o) => self.neg.on_will(o, &mut out),
            Event::Wont(o) => self.neg.on_wont(o, &mut out),
            Event::Do(o) => self.neg.on_do(o, &mut out),
            Event::Dont(o) => self.neg.on_dont(o, &mut out),
            Event::Command(_) => None, // NOP/GA/AYT/…: ignore
            Event::Subnegotiation(o, payload) => {
                return Ok(self.absorb_subneg(o, &payload));
            }
        };
        // Once the peer agrees to TTYPE, ask it to send the terminal type.
        if notice == Some(Notice::RemoteEnabled(opt::TTYPE)) && !self.ttype_requested {
            self.ttype_requested = true;
            out.extend([IAC, SB, opt::TTYPE, TTYPE_SEND, IAC, SE]);
        }
        if !out.is_empty() {
            self.io.write_all(&out).await?;
            self.io.flush().await?;
        }
        Ok(None)
    }

    fn absorb_subneg(&mut self, option: u8, payload: &[u8]) -> Option<Input> {
        match option {
            opt::NAWS if payload.len() >= 4 => {
                let cols = u16::from_be_bytes([payload[0], payload[1]]);
                let rows = u16::from_be_bytes([payload[2], payload[3]]);
                self.window = Some((cols, rows));
                Some(Input::WindowSize { cols, rows })
            }
            opt::TTYPE if payload.first() == Some(&TTYPE_IS) => {
                let name = String::from_utf8_lossy(&payload[1..]).trim().to_string();
                self.terminal = Some(name.clone());
                Some(Input::TerminalType(name))
            }
            _ => None, // malformed or unknown: drop
        }
    }

    /// Read one line in NVT line mode. Handles CR LF / CR NUL / bare LF
    /// terminators, backspace (BS/DEL) editing, and — when we hold the ECHO
    /// option — echoes input back. NAWS/TTYPE updates arriving mid-line are
    /// captured silently. Returns `None` on EOF (any partial line is
    /// discarded).
    ///
    /// **Cancel-safe**: the partial line lives on the stream, so dropping
    /// this future (e.g. losing a `tokio::select!` race against a broadcast
    /// event) and calling `read_line` again resumes exactly where typing
    /// left off.
    pub async fn read_line(&mut self, echo: Echo) -> io::Result<Option<String>> {
        loop {
            let Some(input) = self.next_input().await? else {
                self.line_buf.clear();
                return Ok(None);
            };
            let Input::Data(data) = input else {
                continue; // window/ttype updates are captured on self
            };
            let mut echo_out: Vec<u8> = Vec::new();
            let mut done = false;
            let mut rest_at = data.len();
            for (i, &b) in data.iter().enumerate() {
                if self.swallow_lf {
                    self.swallow_lf = false;
                    if b == b'\n' || b == 0 {
                        continue;
                    }
                }
                match b {
                    b'\r' => {
                        self.swallow_lf = true;
                        done = true;
                    }
                    b'\n' => done = true,
                    0x08 | 0x7F => {
                        if pop_char(&mut self.line_buf) && echo == Echo::On {
                            echo_out.extend(b"\x08 \x08");
                        }
                    }
                    b if b < 0x20 => {} // other control bytes: ignore
                    b => {
                        if self.line_buf.len() < MAX_LINE {
                            self.line_buf.push(b);
                            if echo == Echo::On {
                                echo_out.push(b);
                            }
                        }
                    }
                }
                if done {
                    rest_at = i + 1;
                    break;
                }
            }
            // Push any bytes past the terminator back for the next read.
            if rest_at < data.len() {
                self.events
                    .push_front(Event::Data(data[rest_at..].to_vec()));
            }
            if done {
                echo_out.extend(b"\r\n");
            }
            if !echo_out.is_empty() && self.echo_active() {
                let escaped = escape_iac(&echo_out);
                self.io.write_all(&escaped).await?;
                self.io.flush().await?;
            }
            if done {
                let complete = std::mem::take(&mut self.line_buf);
                return Ok(Some(decode(self.encoding, &complete)));
            }
        }
    }

    /// Write text: `\n` becomes `\r\n`, characters are encoded per the
    /// session encoding, and `0xFF` bytes are IAC-doubled. Flushes.
    pub async fn write_str(&mut self, s: &str) -> io::Result<()> {
        let mut translated = String::with_capacity(s.len() + 8);
        let mut prev = '\0';
        for c in s.chars() {
            if c == '\n' && prev != '\r' {
                translated.push('\r');
            }
            translated.push(c);
            prev = c;
        }
        let mut encoded = Vec::with_capacity(translated.len());
        encode_into(self.encoding, &translated, &mut encoded);
        let escaped = escape_iac(&encoded);
        self.io.write_all(&escaped).await?;
        self.io.flush().await
    }

    /// Write raw bytes **verbatim** (no newline translation, no encoding, no
    /// IAC escaping) and flush. This is the seam a door-game bridge pumps
    /// 8-bit CP437 output through: the caller is responsible for telnet
    /// safety (doubling `0xFF`, e.g. via a `BridgeBuffer`), because the
    /// bytes may already contain deliberate escapes that a second pass here
    /// would corrupt.
    pub async fn write_raw(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.io.write_all(bytes).await?;
        self.io.flush().await
    }

    /// Are we echoing? True once we hold ECHO, or while our WILL ECHO offer
    /// is outstanding (classic BBS behavior; stops if the peer refuses).
    fn echo_active(&self) -> bool {
        self.neg.local_active(opt::ECHO)
    }

    /// Read more bytes from the transport into the event queue.
    /// Returns `false` on EOF.
    async fn fill(&mut self) -> io::Result<bool> {
        let mut buf = [0u8; 4096];
        let n = self.io.read(&mut buf).await?;
        if n == 0 {
            return Ok(false);
        }
        let mut events = Vec::new();
        self.parser.feed(&buf[..n], &mut events);
        self.events.extend(events);
        Ok(true)
    }
}

/// Remove the last (UTF-8-aware) character from `buf`; false if empty.
fn pop_char(buf: &mut Vec<u8>) -> bool {
    if buf.is_empty() {
        return false;
    }
    while let Some(b) = buf.pop() {
        if b & 0xC0 != 0x80 {
            break; // stopped after removing a non-continuation byte
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::{DO, DONT, WILL, WONT};
    use tokio::io::duplex;

    const LINEMODE: u8 = 34;

    /// Read whatever the server has written to the client side.
    async fn drain(client: &mut (impl AsyncRead + Unpin)) -> Vec<u8> {
        let mut buf = [0u8; 4096];
        let n = client.read(&mut buf).await.unwrap();
        buf[..n].to_vec()
    }

    #[tokio::test]
    async fn start_sends_offers_and_captures_naws_ttype() {
        let (mut client, server) = duplex(4096);
        let mut t = TelnetStream::new(server);
        t.start().await.unwrap();

        let offers = drain(&mut client).await;
        assert_eq!(
            offers,
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

        // Client accepts NAWS + TTYPE and reports an 80x24 window.
        client
            .write_all(&[
                IAC,
                WILL,
                opt::NAWS,
                IAC,
                WILL,
                opt::TTYPE,
                IAC,
                SB,
                opt::NAWS,
                0,
                80,
                0,
                24,
                IAC,
                SE,
            ])
            .await
            .unwrap();

        assert_eq!(
            t.next_input().await.unwrap(),
            Some(Input::WindowSize { cols: 80, rows: 24 })
        );
        assert_eq!(t.window(), Some((80, 24)));

        // Server must have asked for the terminal type after WILL TTYPE.
        let sent = drain(&mut client).await;
        assert_eq!(sent, vec![IAC, SB, opt::TTYPE, TTYPE_SEND, IAC, SE]);

        let mut reply = vec![IAC, SB, opt::TTYPE, TTYPE_IS];
        reply.extend(b"ANSI");
        reply.extend([IAC, SE]);
        client.write_all(&reply).await.unwrap();
        assert_eq!(
            t.next_input().await.unwrap(),
            Some(Input::TerminalType("ANSI".into()))
        );
        assert_eq!(t.terminal(), Some("ANSI"));
    }

    #[tokio::test]
    async fn refuses_unknown_options() {
        let (mut client, server) = duplex(4096);
        let mut t = TelnetStream::new(server);
        client
            .write_all(&[IAC, WILL, LINEMODE, IAC, DO, LINEMODE, b'x'])
            .await
            .unwrap();
        assert_eq!(t.next_input().await.unwrap(), Some(Input::Data(vec![b'x'])));
        let sent = drain(&mut client).await;
        assert_eq!(sent, vec![IAC, DONT, LINEMODE, IAC, WONT, LINEMODE]);
    }

    #[tokio::test]
    async fn read_line_edits_echoes_and_splits() {
        let (mut client, server) = duplex(4096);
        let mut t = TelnetStream::new(server);
        t.start().await.unwrap();
        drain(&mut client).await;

        // Backspace editing + two lines in one packet + CR NUL terminator.
        client
            .write_all(b"abcx\x08\r\nsecond\r\0third\r\n")
            .await
            .unwrap();
        assert_eq!(t.read_line(Echo::On).await.unwrap().as_deref(), Some("abc"));
        let echoed = drain(&mut client).await;
        assert_eq!(echoed, b"abcx\x08 \x08\r\n");

        assert_eq!(
            t.read_line(Echo::Hidden).await.unwrap().as_deref(),
            Some("second")
        );
        // Hidden mode echoes only the line ending.
        assert_eq!(drain(&mut client).await, b"\r\n");

        assert_eq!(
            t.read_line(Echo::On).await.unwrap().as_deref(),
            Some("third")
        );

        // EOF: drop the client, partial input is discarded.
        client.write_all(b"partial").await.unwrap();
        drop(client);
        assert_eq!(t.read_line(Echo::Hidden).await.unwrap(), None);
    }

    #[tokio::test]
    async fn read_line_undoubles_iac_and_survives_split_crlf() {
        let (mut client, server) = duplex(4096);
        let mut t = TelnetStream::new(server);
        t.set_encoding(Encoding::Cp437);

        client
            .write_all(&[b'A', IAC, IAC, b'B', b'\r'])
            .await
            .unwrap();
        // CR arrives at a packet edge; LF follows in the next packet and
        // must be swallowed rather than produce an empty second line.
        let line = t.read_line(Echo::Hidden).await.unwrap();
        // 0xFF decodes through the real CP437 table (a no-break space).
        assert_eq!(line.as_deref(), Some("A\u{a0}B"));
        client.write_all(b"\nnext\r\n").await.unwrap();
        assert_eq!(
            t.read_line(Echo::Hidden).await.unwrap().as_deref(),
            Some("next")
        );
    }

    #[tokio::test]
    async fn no_echo_after_peer_refuses_echo() {
        let (mut client, server) = duplex(4096);
        let mut t = TelnetStream::new(server);
        t.start().await.unwrap();
        drain(&mut client).await;

        client.write_all(&[IAC, DONT, opt::ECHO]).await.unwrap();
        client.write_all(b"hi\r\n").await.unwrap();
        assert_eq!(t.read_line(Echo::On).await.unwrap().as_deref(), Some("hi"));

        // Nothing echoed: next bytes on the wire are from this write only.
        t.write_str("done").await.unwrap();
        assert_eq!(drain(&mut client).await, b"done");
    }

    #[tokio::test]
    async fn write_str_translates_newlines_and_encodes() {
        let (mut client, server) = duplex(4096);
        let mut t = TelnetStream::new(server);

        t.write_str("a\nb\r\nc ♥\n").await.unwrap();
        assert_eq!(drain(&mut client).await, "a\r\nb\r\nc ♥\r\n".as_bytes());

        t.set_encoding(Encoding::Cp437);
        t.write_str("café\n").await.unwrap();
        // The real CP437 table: 'é' is 0x82 on the wire.
        assert_eq!(
            drain(&mut client).await,
            [b'c', b'a', b'f', 0x82, b'\r', b'\n']
        );
    }

    #[tokio::test]
    async fn utf8_backspace_removes_whole_character() {
        let (mut client, server) = duplex(4096);
        let mut t = TelnetStream::new(server);
        // "é" is two bytes in UTF-8; one backspace must remove both.
        let mut input = b"caf".to_vec();
        input.extend("é".as_bytes());
        input.extend(b"\x7fe\r\n");
        client.write_all(&input).await.unwrap();
        assert_eq!(
            t.read_line(Echo::Hidden).await.unwrap().as_deref(),
            Some("cafe")
        );
    }
}
