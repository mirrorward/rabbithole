//! [`TelnetStream`]: the protocol layer bound to a transport.
//!
//! Wraps any `AsyncRead + AsyncWrite` (a `TcpStream` in production, an
//! in-memory duplex in tests) and layers on: automatic option negotiation
//! (replies, TTYPE `SEND` follow-up, NAWS/TTYPE capture), a line-mode reader
//! with CR/LF and backspace handling plus server-side echo, and an
//! encoding-aware writer that translates `\n` → `\r\n` and doubles IAC.
//!
//! For file transfers (ZMODEM) and door bridges the stream also exposes a
//! **binary mode**: [`TelnetStream::read_binary`] / [`TelnetStream::write_binary`]
//! move raw payload bytes through the telnet layer 8-bit-cleanly — IAC
//! doubled outbound and undoubled inbound, negotiation still absorbed, and
//! *no* newline translation or character-encoding applied. Line mode and
//! binary mode share the stream safely: switching back to
//! [`TelnetStream::read_line`] after a transfer just resumes line editing.

use std::collections::VecDeque;
use std::io;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::encoding::{decode, encode_into, Encoding};
use crate::negotiate::{Negotiator, Notice};
use crate::proto::{escape_iac, opt, Event, Parser, IAC, SB, SE, TTYPE_IS, TTYPE_SEND};

/// Longest accepted encoded input line, in bytes; whole characters beyond
/// this limit are dropped.
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
    /// A consumed terminator must survive cancellation during echo I/O.
    /// Decoded before any encoding change can reinterpret this submitted line.
    completed_line: Option<String>,
    /// A valid but incomplete UTF-8 scalar, never exposed or echoed until
    /// complete. Its full width is reserved inside MAX_LINE before buffering.
    utf8_pending: Vec<u8>,
    /// Remaining continuation bytes of a scalar rejected at the line cap.
    /// They must not become replacement characters or spill into another line.
    utf8_discard: u8,
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
            completed_line: None,
            utf8_pending: Vec::new(),
            utf8_discard: 0,
        }
    }

    /// Set the output/input character encoding (default UTF-8). Complete
    /// pending text is re-encoded within the byte cap; an unfinished scalar
    /// is discarded. Setting the current encoding preserves all input.
    pub fn set_encoding(&mut self, enc: Encoding) {
        if enc == self.encoding {
            return;
        }
        let text = decode(self.encoding, &self.line_buf);
        self.line_buf.clear();
        encode_into(enc, &text, &mut self.line_buf);
        while self.line_buf.len() > MAX_LINE {
            pop_char(&mut self.line_buf, enc);
        }
        self.utf8_pending.clear();
        self.utf8_discard = 0;
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

    /// The decoded line being edited, for redrawing ordinary echoed input
    /// after asynchronous output. Do not redraw hidden password input.
    pub fn pending_line(&self) -> String {
        decode(self.encoding, &self.line_buf)
    }

    /// Forget entered input when its prompt is no longer valid, including a
    /// completed line waiting for echo I/O. Negotiation state and subsequent
    /// input remain available to the next prompt.
    pub fn discard_line(&mut self) {
        self.line_buf.clear();
        self.completed_line = None;
        self.utf8_pending.clear();
        self.utf8_discard = 0;
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
    /// captured silently. CP437 edits one encoded byte; UTF-8 edits one
    /// Unicode scalar (not a grapheme or terminal display cell). Incomplete
    /// UTF-8 is buffered across reads; malformed input becomes complete
    /// replacement characters when space permits. Returns `None` on EOF
    /// (any partial line is discarded).
    ///
    /// **Cancel-safe**: the partial line lives on the stream, so dropping
    /// this future (e.g. losing a `tokio::select!` race against a broadcast
    /// event) and calling `read_line` again resumes exactly where typing
    /// left off.
    pub async fn read_line(&mut self, echo: Echo) -> io::Result<Option<String>> {
        if let Some(complete) = self.completed_line.take() {
            return Ok(Some(complete));
        }
        loop {
            let Some(input) = self.next_input().await? else {
                self.discard_line();
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
                        if !self.utf8_pending.is_empty() || self.utf8_discard != 0 {
                            // No part of this scalar has been echoed, so
                            // cancel it without erasing the preceding one.
                            self.utf8_pending.clear();
                            self.utf8_discard = 0;
                        } else if pop_char(&mut self.line_buf, self.encoding) && echo == Echo::On {
                            echo_out.extend(b"\x08 \x08");
                        }
                    }
                    // Ignore other controls, but do not join UTF-8 bytes
                    // across one. Telnet negotiation never enters this arm.
                    b if b < 0x20 => self.finish_utf8(echo, &mut echo_out),
                    b => self.line_byte(b, echo, &mut echo_out),
                }
                if done {
                    self.finish_utf8(echo, &mut echo_out);
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
                let complete = std::mem::take(&mut self.line_buf);
                self.completed_line = Some(decode(self.encoding, &complete));
            }
            if !echo_out.is_empty() && self.echo_active() {
                let escaped = escape_iac(&echo_out);
                self.io.write_all(&escaped).await?;
                self.io.flush().await?;
            }
            if done {
                return Ok(self.completed_line.take());
            }
        }
    }

    /// Append only a complete encoded character that fits, echoing the same
    /// bytes. No rejected character is partially visible or editable.
    fn line_char(&mut self, bytes: &[u8], echo: Echo, out: &mut Vec<u8>) {
        if self.line_buf.len() + bytes.len() <= MAX_LINE {
            self.line_buf.extend_from_slice(bytes);
            if echo == Echo::On {
                out.extend_from_slice(bytes);
            }
        }
    }

    fn finish_utf8(&mut self, echo: Echo, out: &mut Vec<u8>) {
        if !self.utf8_pending.is_empty() {
            self.utf8_pending.clear();
            self.line_char("\u{fffd}".as_bytes(), echo, out);
        }
        self.utf8_discard = 0;
    }

    fn line_byte(&mut self, byte: u8, echo: Echo, out: &mut Vec<u8>) {
        if self.encoding == Encoding::Cp437 {
            self.line_char(&[byte], echo, out);
            return;
        }
        if self.utf8_discard != 0 {
            if byte & 0xc0 == 0x80 {
                self.utf8_discard -= 1;
                return;
            }
            self.utf8_discard = 0;
        }
        if !self.utf8_pending.is_empty() {
            self.utf8_pending.push(byte);
            match std::str::from_utf8(&self.utf8_pending) {
                Ok(_) => {
                    let complete = std::mem::take(&mut self.utf8_pending);
                    self.line_char(&complete, echo, out);
                    return;
                }
                Err(error) if error.error_len().is_none() => return,
                Err(_) => {
                    self.utf8_pending.clear();
                    self.line_char("\u{fffd}".as_bytes(), echo, out);
                    // The previous prefix was valid but incomplete. This
                    // byte broke it; process it again as a fresh character.
                }
            }
        }
        let width = match byte {
            0x20..=0x7e => {
                self.line_char(&[byte], echo, out);
                return;
            }
            0xc2..=0xdf => 2,
            0xe0..=0xef => 3,
            0xf0..=0xf4 => 4,
            _ => {
                self.line_char("\u{fffd}".as_bytes(), echo, out);
                return;
            }
        };
        if self.line_buf.len() + usize::from(width) <= MAX_LINE {
            self.utf8_pending.push(byte);
        } else {
            self.utf8_discard = width - 1;
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

    /// Write payload bytes 8-bit-safely: every `0xFF` is doubled to `IAC
    /// IAC` and nothing else is touched — no newline translation, no
    /// character encoding. Flushes. The outbound half of binary mode; a
    /// file-transfer driver (ZMODEM) sends its wire frames through this so
    /// the session encoding never corrupts them.
    pub async fn write_binary(&mut self, bytes: &[u8]) -> io::Result<()> {
        let escaped = escape_iac(bytes);
        self.io.write_all(&escaped).await?;
        self.io.flush().await
    }

    /// Next chunk of raw payload bytes, with `IAC IAC` already undoubled to
    /// `0xFF`. Negotiation traffic is still absorbed internally (replies
    /// written, NAWS/TTYPE captured silently). `None` means EOF. The
    /// inbound half of binary mode: unlike [`TelnetStream::read_line`] there
    /// is no line editing, no echo, and no encoding — bytes arrive exactly
    /// as the peer's telnet layer sent them.
    ///
    /// **Cancel-safe**: no partial state lives in the future; dropping it
    /// loses nothing.
    pub async fn read_binary(&mut self) -> io::Result<Option<Vec<u8>>> {
        loop {
            match self.next_input().await? {
                None => return Ok(None),
                Some(Input::Data(data)) if !data.is_empty() => return Ok(Some(data)),
                Some(_) => {} // window/ttype updates, or an empty chunk
            }
        }
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

/// Remove one complete encoded character; false if empty. UTF-8 buffers
/// contain only complete scalars, while every CP437 byte is independent.
fn pop_char(buf: &mut Vec<u8>, encoding: Encoding) -> bool {
    if buf.is_empty() {
        return false;
    }
    while let Some(b) = buf.pop() {
        if encoding == Encoding::Cp437 || b & 0xC0 != 0x80 {
            break; // stopped after removing a non-continuation byte
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::{DO, DONT, WILL, WONT};
    use std::future::{poll_fn, Future};
    use std::pin::{pin, Pin};
    use std::task::{Context, Poll};
    use tokio::io::duplex;
    use tokio::io::ReadBuf;

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
    async fn write_binary_doubles_iac_and_nothing_else() {
        let (mut client, server) = duplex(4096);
        let mut t = TelnetStream::new(server);
        // CP437 mode must not matter: binary mode bypasses encoding and
        // newline translation entirely.
        t.set_encoding(Encoding::Cp437);
        t.write_binary(&[0x00, b'\n', 0xFF, 0x18, 0xFF, 0xFF, 0x7F])
            .await
            .unwrap();
        assert_eq!(
            drain(&mut client).await,
            vec![0x00, b'\n', 0xFF, 0xFF, 0x18, 0xFF, 0xFF, 0xFF, 0xFF, 0x7F]
        );
    }

    #[tokio::test]
    async fn read_binary_undoubles_iac_and_absorbs_negotiation() {
        let (mut client, server) = duplex(4096);
        let mut t = TelnetStream::new(server);
        t.start().await.unwrap();
        drain(&mut client).await;

        // Payload with a doubled IAC, negotiation spliced into the middle,
        // and a NAWS report — the payload comes out contiguous per chunk,
        // the negotiation is answered/captured invisibly.
        client
            .write_all(&[
                1,
                2,
                IAC,
                IAC,
                3,
                IAC,
                WILL,
                opt::NAWS,
                IAC,
                SB,
                opt::NAWS,
                0,
                132,
                0,
                43,
                IAC,
                SE,
                4,
                5,
            ])
            .await
            .unwrap();
        assert_eq!(t.read_binary().await.unwrap(), Some(vec![1, 2, 0xFF, 3]));
        assert_eq!(t.read_binary().await.unwrap(), Some(vec![4, 5]));
        assert_eq!(t.window(), Some((132, 43)));

        // EOF surfaces as None.
        drop(client);
        assert_eq!(t.read_binary().await.unwrap(), None);
    }

    #[tokio::test]
    async fn binary_mode_and_line_mode_share_the_stream() {
        let (mut client, server) = duplex(4096);
        let mut t = TelnetStream::new(server);

        // A command line and the first transfer bytes arrive together; the
        // line reader stops at the terminator and binary mode picks up the
        // pushed-back remainder — then line mode resumes cleanly after.
        client
            .write_all(&[b'z', b'g', b'e', b't', b'\r', b'\n', 0xAA, IAC, IAC, 0xBB])
            .await
            .unwrap();
        assert_eq!(
            t.read_line(Echo::Hidden).await.unwrap().as_deref(),
            Some("zget")
        );
        assert_eq!(
            t.read_binary().await.unwrap(),
            Some(vec![b'\n', 0xAA, 0xFF, 0xBB])
        );
        client.write_all(b"ls\r\n").await.unwrap();
        assert_eq!(
            t.read_line(Echo::Hidden).await.unwrap().as_deref(),
            Some("ls")
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

    /// Poll through all available input, then cancel at the deterministic
    /// pending I/O boundary. No timeout or sleep stands in for cancellation.
    async fn cancel_pending<S: AsyncRead + AsyncWrite + Unpin>(
        t: &mut TelnetStream<S>,
        echo: Echo,
    ) {
        poll_fn(|cx| {
            let mut read = pin!(t.read_line(echo));
            assert!(read.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
    }

    #[tokio::test]
    async fn cp437_backspace_and_delete_remove_one_byte_including_iac() {
        let (mut client, server) = duplex(4096);
        let mut t = TelnetStream::new(server);
        t.set_encoding(Encoding::Cp437);
        t.start().await.unwrap();
        drain(&mut client).await;
        // é and shade occupy the UTF-8 continuation-byte range, but each
        // is a complete CP437 character. 0xff remains telnet-escaped.
        client
            .write_all(&[
                b'c', b'a', b'f', 0x82, 0x08, b'e', 0xb0, 0x7f, 0xcd, 0xb9, 0x7f, IAC, IAC, 0x08,
                b'\r', b'\n',
            ])
            .await
            .unwrap();
        assert_eq!(
            t.read_line(Echo::On).await.unwrap().as_deref(),
            Some("cafe═")
        );
        let mut echoed = b"caf\x82\x08 \x08e\xb0\x08 \x08\xcd\xb9\x08 \x08".to_vec();
        echoed.extend([IAC, IAC]);
        echoed.extend(b"\x08 \x08\r\n");
        assert_eq!(drain(&mut client).await, echoed);
    }

    #[tokio::test]
    async fn fragmented_utf8_survives_cancellation_and_interleaved_negotiation() {
        let (mut client, server) = duplex(4096);
        let mut t = TelnetStream::new(server);
        t.start().await.unwrap();
        drain(&mut client).await;
        let mut complete = String::from("A");
        client.write_all(complete.as_bytes()).await.unwrap();
        cancel_pending(&mut t, Echo::On).await;
        for scalar in ["é", "界", "😀"] {
            for (i, byte) in scalar.bytes().enumerate() {
                client.write_all(&[byte]).await.unwrap();
                if i == 0 {
                    // Negotiation is separate from editable UTF-8, even
                    // inside a scalar and across parser/read boundaries.
                    client
                        .write_all(&[IAC, SB, opt::NAWS, 0, 100, 0, 40, IAC, SE])
                        .await
                        .unwrap();
                    client
                        .write_all(&[IAC, SB, opt::TTYPE, TTYPE_IS, b'X', IAC, SE])
                        .await
                        .unwrap();
                }
                cancel_pending(&mut t, Echo::On).await;
                if i + 1 == scalar.len() {
                    complete.push_str(scalar);
                }
                assert_eq!(t.pending_line(), complete);
                assert!(t.line_buf.len() + t.utf8_pending.len() <= MAX_LINE);
            }
        }
        assert_eq!(t.window(), Some((100, 40)));
        assert_eq!(t.terminal(), Some("X"));
        client.write_all(b"\x08\x7fZ\r").await.unwrap();
        assert_eq!(t.read_line(Echo::On).await.unwrap().as_deref(), Some("AéZ"));
        assert_eq!(
            drain(&mut client).await,
            "Aé界😀\u{8} \u{8}\u{8} \u{8}Z\r\n".as_bytes()
        );
        client.write_all(b"\nnext\r\0").await.unwrap();
        assert_eq!(
            t.read_line(Echo::Hidden).await.unwrap().as_deref(),
            Some("next")
        );
        assert_eq!(drain(&mut client).await, b"\r\n");
    }

    #[tokio::test]
    async fn backspace_cancels_unfinished_utf8_without_erasing_echoed_text() {
        let (mut client, server) = duplex(4096);
        let mut t = TelnetStream::new(server);
        t.start().await.unwrap();
        drain(&mut client).await;
        client.write_all(b"A\xe2\x82").await.unwrap();
        cancel_pending(&mut t, Echo::On).await;
        assert_eq!(t.pending_line(), "A");
        assert_eq!(drain(&mut client).await, b"A");
        client.write_all(b"\x08B\xf0\x9f\x7fC\r\n").await.unwrap();
        assert_eq!(t.read_line(Echo::On).await.unwrap().as_deref(), Some("ABC"));
        assert_eq!(drain(&mut client).await, b"BC\r\n");
        // Hidden mode must suppress complete scalars, replacements and erase
        // sequences alike; only the final line ending is sent.
        client
            .write_all("é界\u{8}😀\u{7f}\r\n".as_bytes())
            .await
            .unwrap();
        assert_eq!(
            t.read_line(Echo::Hidden).await.unwrap().as_deref(),
            Some("é")
        );
        assert_eq!(drain(&mut client).await, b"\r\n");
    }

    #[tokio::test]
    async fn utf8_limit_admits_or_drops_whole_scalars_and_remains_editable() {
        for scalar in ["é", "界", "😀"] {
            for room in 0..scalar.len() {
                let (mut client, server) = duplex(4096);
                let mut t = TelnetStream::new(server);
                t.start().await.unwrap();
                drain(&mut client).await;
                let prefix = "x".repeat(MAX_LINE - room);
                client.write_all(prefix.as_bytes()).await.unwrap();
                cancel_pending(&mut t, Echo::On).await;
                for byte in scalar.bytes() {
                    client.write_all(&[byte]).await.unwrap();
                    cancel_pending(&mut t, Echo::On).await;
                    assert_eq!(t.pending_line(), prefix);
                    assert!(t.line_buf.len() + t.utf8_pending.len() <= MAX_LINE);
                }
                client.write_all(b"\x08Z\r\n").await.unwrap();
                let expected = format!("{}Z", &prefix[..prefix.len() - 1]);
                assert_eq!(t.read_line(Echo::On).await.unwrap(), Some(expected));
                assert_eq!(
                    drain(&mut client).await,
                    format!("{prefix}\u{8} \u{8}Z\r\n").as_bytes()
                );
            }
            let (mut client, server) = duplex(4096);
            let mut t = TelnetStream::new(server);
            let prefix = "x".repeat(MAX_LINE - scalar.len());
            client
                .write_all(format!("{prefix}{scalar}overflow\x7fZ\r\n").as_bytes())
                .await
                .unwrap();
            assert_eq!(
                t.read_line(Echo::Hidden).await.unwrap(),
                Some(format!("{prefix}Z"))
            );
        }
    }

    #[tokio::test]
    async fn malformed_utf8_reprocesses_the_following_character_or_control() {
        let (mut client, server) = duplex(4096);
        let mut t = TelnetStream::new(server);
        t.start().await.unwrap();
        drain(&mut client).await;
        client
            .write_all(b"\xe2A\xe2\x82B\xed\xa0\x80C\xf0\x9f\x01D\xe2\x82\r\n")
            .await
            .unwrap();
        let expected = "�A�B���C�D�";
        assert_eq!(
            t.read_line(Echo::On).await.unwrap().as_deref(),
            Some(expected)
        );
        assert_eq!(
            drain(&mut client).await,
            format!("{expected}\r\n").as_bytes()
        );

        // An incomplete scalar/replacement at the cap must not split the
        // replacement or eat the next ASCII input when it still fits.
        let prefix = "x".repeat(MAX_LINE - 2);
        client
            .write_all(format!("{prefix}\u{c0}Y\r\n").as_bytes())
            .await
            .unwrap();
        assert_eq!(
            t.read_line(Echo::Hidden).await.unwrap(),
            Some(format!("{prefix}À"))
        );
        drain(&mut client).await;
        let mut input = prefix.as_bytes().to_vec();
        input.extend(b"\xc2Y\r\n");
        client.write_all(&input).await.unwrap();
        assert_eq!(
            t.read_line(Echo::Hidden).await.unwrap(),
            Some(format!("{prefix}Y"))
        );
    }

    #[tokio::test]
    async fn encoding_changes_reencode_complete_text_and_preserve_same_mode_fragments() {
        let (mut client, server) = duplex(4096);
        let mut t = TelnetStream::new(server);
        client.write_all(b"caf\xc3").await.unwrap();
        cancel_pending(&mut t, Echo::Hidden).await;
        t.set_encoding(Encoding::Utf8);
        client.write_all(b"\xa9").await.unwrap();
        cancel_pending(&mut t, Echo::Hidden).await;
        assert_eq!(t.pending_line(), "café");
        t.set_encoding(Encoding::Cp437);
        assert_eq!(t.pending_line(), "café");
        assert_eq!(t.line_buf, b"caf\x82");
        client.write_all(b"\x08e\r\n").await.unwrap();
        assert_eq!(
            t.read_line(Echo::Hidden).await.unwrap().as_deref(),
            Some("cafe")
        );

        client.write_all(&vec![0xb0; MAX_LINE]).await.unwrap();
        cancel_pending(&mut t, Echo::Hidden).await;
        t.set_encoding(Encoding::Utf8);
        assert_eq!(t.pending_line(), "░".repeat(MAX_LINE / 3));
        assert!(t.line_buf.len() <= MAX_LINE);
        client.write_all(b"\xc3").await.unwrap();
        cancel_pending(&mut t, Echo::Hidden).await;
        t.set_encoding(Encoding::Cp437);
        assert!(t.utf8_pending.is_empty());
        assert_eq!(t.pending_line(), "░".repeat(MAX_LINE / 3));
    }

    #[tokio::test]
    async fn discard_and_eof_clear_all_partial_decoder_state() {
        let (mut client, server) = duplex(4096);
        let mut t = TelnetStream::new(server);
        client.write_all(b"old\xe2\x82").await.unwrap();
        cancel_pending(&mut t, Echo::Hidden).await;
        t.discard_line();
        assert_eq!(t.pending_line(), "");
        assert!(t.utf8_pending.is_empty());
        client.write_all(b"new\r\npartial\xf0\x9f").await.unwrap();
        assert_eq!(
            t.read_line(Echo::Hidden).await.unwrap().as_deref(),
            Some("new")
        );
        client.shutdown().await.unwrap();
        assert_eq!(t.read_line(Echo::Hidden).await.unwrap(), None);
        assert_eq!(t.pending_line(), "");
        assert!(t.utf8_pending.is_empty());
        assert_eq!(t.utf8_discard, 0);
    }

    /// Controlled output backpressure lets a test cancel precisely after a
    /// terminator was consumed, either before echo writes or during flush.
    struct GatedIo {
        input: VecDeque<u8>,
        block_write: bool,
        block_flush: bool,
    }

    impl AsyncRead for GatedIo {
        fn poll_read(
            mut self: Pin<&mut Self>,
            _: &mut Context<'_>,
            buf: &mut ReadBuf<'_>,
        ) -> Poll<io::Result<()>> {
            if self.input.is_empty() {
                return Poll::Pending;
            }
            while buf.remaining() != 0 {
                let Some(byte) = self.input.pop_front() else {
                    break;
                };
                buf.put_slice(&[byte]);
            }
            Poll::Ready(Ok(()))
        }
    }

    impl AsyncWrite for GatedIo {
        fn poll_write(
            self: Pin<&mut Self>,
            _: &mut Context<'_>,
            bytes: &[u8],
        ) -> Poll<io::Result<usize>> {
            if self.block_write {
                Poll::Pending
            } else {
                Poll::Ready(Ok(bytes.len()))
            }
        }
        fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
            if self.block_flush {
                Poll::Pending
            } else {
                Poll::Ready(Ok(()))
            }
        }
        fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }

    #[tokio::test]
    async fn cancellation_during_completed_line_echo_preserves_terminator_and_remainder() {
        for echo in [Echo::On, Echo::Hidden] {
            for block_write in [false, true] {
                let mut t = TelnetStream::new(GatedIo {
                    input: "café\r\nnext\r\n".bytes().collect(),
                    block_write: false,
                    block_flush: false,
                });
                t.start().await.unwrap();
                t.io.block_write = block_write;
                t.io.block_flush = !block_write;
                cancel_pending(&mut t, echo).await;
                assert_eq!(t.completed_line.as_deref(), Some("café"));
                assert_eq!(t.pending_line(), "");
                // The completed value keeps its original interpretation.
                t.set_encoding(Encoding::Cp437);
                t.io.block_write = false;
                t.io.block_flush = false;
                assert_eq!(t.read_line(echo).await.unwrap().as_deref(), Some("café"));
                assert_eq!(t.read_line(echo).await.unwrap().as_deref(), Some("next"));
                assert!(t.completed_line.is_none());
                cancel_pending(&mut t, echo).await;
            }
        }
    }
}
