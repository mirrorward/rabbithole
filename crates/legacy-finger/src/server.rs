//! The finger TCP server: one query line in, one capped response out.
//!
//! Each accepted connection gets a single RFC 1288 query line (read with a
//! length cap and a deadline — finger clients that dawdle get hung up on),
//! the query is answered from the [`FingerDirectory`], and the connection is
//! closed. Responses always pass through [`to_wire`], so CRLF endings,
//! control-character stripping, and the size cap hold no matter which path
//! produced the text.

use std::io;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};

use crate::directory::FingerDirectory;
use crate::query::{parse_query, Query};
use crate::render::{format_forward_refused, format_profile, format_unknown, format_who, to_wire};

/// Maximum accepted query-line length in bytes (excluding CRLF). Anything
/// longer is answered with a polite error and the connection is dropped.
pub const MAX_QUERY_BYTES: usize = 512;

/// How long a client gets to deliver its query line.
const QUERY_DEADLINE: Duration = Duration::from_secs(30);

/// An RFC 1288 finger server over a pluggable [`FingerDirectory`].
pub struct FingerServer {
    directory: Arc<dyn FingerDirectory>,
}

impl FingerServer {
    /// Build a server answering from the given directory.
    pub fn new(directory: Arc<dyn FingerDirectory>) -> Self {
        Self { directory }
    }

    /// Accept connections on `listener` forever, answering one query per
    /// connection. Returns only if `accept` itself fails.
    pub async fn serve(self, listener: TcpListener) -> io::Result<()> {
        loop {
            let (stream, peer) = listener.accept().await?;
            let directory = Arc::clone(&self.directory);
            tokio::spawn(async move {
                if let Err(err) = handle_connection(stream, directory.as_ref()).await {
                    tracing::debug!(%peer, %err, "finger connection error");
                }
            });
        }
    }
}

/// Answer one parsed query line (CRLF already stripped) from the directory.
/// Returns display text with `\n` endings; callers pass it through
/// [`to_wire`] before it touches a socket. Exposed for tests and for hosts
/// that front finger with something other than raw TCP.
pub async fn handle_query(directory: &dyn FingerDirectory, line: &str) -> String {
    match parse_query(line) {
        Query::Who => format_who(&directory.who().await),
        Query::User(name) => match directory.lookup(&name).await {
            Some(profile) => format_profile(&profile),
            None => format_unknown(&name),
        },
        Query::Forward => format_forward_refused(),
    }
}

async fn handle_connection(
    mut stream: TcpStream,
    directory: &dyn FingerDirectory,
) -> io::Result<()> {
    let query = tokio::time::timeout(QUERY_DEADLINE, read_query_line(&mut stream)).await;
    let text = match query {
        Ok(Ok(Some(line))) => handle_query(directory, &line).await,
        Ok(Ok(None)) => "finger: query too long.\n".to_string(),
        Ok(Err(err)) => return Err(err),
        // Deadline passed without a full query line: just hang up.
        Err(_elapsed) => return Ok(()),
    };
    stream.write_all(to_wire(&text).as_bytes()).await?;
    stream.shutdown().await
}

/// Read one query line, capped at [`MAX_QUERY_BYTES`] (plus line ending).
/// Returns `Ok(None)` if the client exceeded the cap without sending a
/// newline. Bytes are decoded lossily — a garbage query simply won't match
/// any user.
async fn read_query_line(stream: &mut TcpStream) -> io::Result<Option<String>> {
    let mut limited = BufReader::new(stream.take((MAX_QUERY_BYTES + 2) as u64));
    let mut buf = Vec::with_capacity(128);
    limited.read_until(b'\n', &mut buf).await?;
    if !buf.ends_with(b"\n") && buf.len() > MAX_QUERY_BYTES {
        return Ok(None);
    }
    while buf.last() == Some(&b'\n') || buf.last() == Some(&b'\r') {
        buf.pop();
    }
    Ok(Some(String::from_utf8_lossy(&buf).into_owned()))
}
