//! St. Louis-format **nodelist** parsing, **NODEDIFF** application, and the
//! nodelist **CRC-16** header checksum (FTS-0005).
//!
//! The nodelist is FidoNet's "DNS": a plaintext, comma-delimited file published
//! weekly and distributed as a compressed diff. Each data line is:
//!
//! ```text
//!   Keyword,Number,Name,Location,SysopName,Phone,Speed,Flags...
//!   ───┬───┬────── ─────────────────────────────────── ──┬───
//!   keyword │      seven mandatory fields                 └ zero or more flags
//!           └ Zone / Region / Host / Hub / Pvt / Down / Hold / <blank> (a node)
//! ```
//!
//! Structure is **positional**: a `Zone` line opens a zone, `Host`/`Region`
//! lines open nets within it, and subsequent blank-keyword (`Node`) lines are
//! nodes in the current net. [`parse_line`] decodes one line and [`parse`]
//! decodes a whole file (skipping `;` comments and blank lines);
//! [`resolve_addresses`] walks the positional hierarchy to assign each entry its
//! `zone:net/node` [`FtnAddress`].
//!
//! # Header CRC
//!
//! The first line is a comment carrying a 16-bit CRC of the *rest* of the file:
//!
//! ```text
//!   ;A FidoNet Nodelist for Friday, ... -- Day number 152 : 46893
//!                                                            └─ decimal CRC-16
//! ```
//!
//! The CRC is **CRC-16/ARC** — polynomial `0xA001` (the reflection of
//! `x^16 + x^15 + x^2 + 1`), initial value `0x0000`, reflected in and out, no
//! final XOR — computed over every byte *after* the first line's terminator.
//! [`crc16`] implements it (check value: `crc16(b"123456789") == 0xBB3D`) and
//! [`verify_nodelist`] checks a file's declared header value against it.
//!
//! # NODEDIFF
//!
//! A diff turns last week's nodelist into this week's with three commands —
//! `A<n>` (add the next `n` diff lines), `C<n>` (copy `n` base lines), `D<n>`
//! (delete `n` base lines). [`apply_nodediff`] applies one, preserving line
//! terminators byte-for-byte so the result's CRC stays verifiable.
//!
//! Everything is pure and total: malformed input yields an [`FtnError`], never a
//! panic.

use crate::address::FtnAddress;
use crate::error::{FtnError, NodediffErrorKind, NodelistErrorKind};

/// The kind of a nodelist entry, from its leading keyword.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeKeyword {
    /// `Zone` — opens a zone (its number is the zone number).
    Zone,
    /// `Region` — opens a region net.
    Region,
    /// `Host` — opens a net (network host).
    Host,
    /// `Hub` — a routing hub within the current net.
    Hub,
    /// `Pvt` — a private (unlisted-number) node.
    Pvt,
    /// `Down` — a node currently down.
    Down,
    /// `Hold` — a node holding mail.
    Hold,
    /// A plain node (blank keyword field).
    Node,
}

impl NodeKeyword {
    /// Parse a keyword field (the text before the first comma). An empty field
    /// is a plain [`Node`](NodeKeyword::Node). Unknown keywords yield `None`.
    pub fn parse(field: &str) -> Option<NodeKeyword> {
        Some(match field.trim() {
            "" => NodeKeyword::Node,
            k if k.eq_ignore_ascii_case("Zone") => NodeKeyword::Zone,
            k if k.eq_ignore_ascii_case("Region") => NodeKeyword::Region,
            k if k.eq_ignore_ascii_case("Host") => NodeKeyword::Host,
            k if k.eq_ignore_ascii_case("Hub") => NodeKeyword::Hub,
            k if k.eq_ignore_ascii_case("Pvt") => NodeKeyword::Pvt,
            k if k.eq_ignore_ascii_case("Down") => NodeKeyword::Down,
            k if k.eq_ignore_ascii_case("Hold") => NodeKeyword::Hold,
            _ => return None,
        })
    }
}

/// A single parsed nodelist data line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodelistEntry {
    /// The leading keyword classifying this line.
    pub keyword: NodeKeyword,
    /// The line's number (zone/region/net/hub/node number, per keyword).
    pub number: u16,
    /// System/network name.
    pub name: String,
    /// Location (city, region).
    pub location: String,
    /// Sysop name.
    pub sysop: String,
    /// Phone number (`-Unpublished-` for private nodes).
    pub phone: String,
    /// Maximum baud rate string (e.g. `9600`, `300`, `33600`).
    pub speed: String,
    /// Trailing flags (e.g. `CM`, `XA`, `IBN`, `INA:host`), in order.
    pub flags: Vec<String>,
}

/// Parse one nodelist line.
///
/// Returns `Ok(None)` for comment lines (leading `;`) and blank lines, and
/// `Ok(Some(entry))` for data lines. Returns [`FtnError::Nodelist`] when a data
/// line has fewer than the seven mandatory fields, an unrecognized keyword, or a
/// non-numeric `Number`.
pub fn parse_line(line: &str) -> Result<Option<NodelistEntry>, FtnError> {
    let line = line.trim_end_matches(['\r', '\n']);
    if line.trim().is_empty() || line.trim_start().starts_with(';') {
        return Ok(None);
    }

    let fields: Vec<&str> = line.split(',').collect();
    if fields.len() < 7 {
        return Err(FtnError::Nodelist {
            reason: NodelistErrorKind::TooFewFields {
                found: fields.len(),
            },
        });
    }

    let keyword = NodeKeyword::parse(fields[0]).ok_or(FtnError::Nodelist {
        reason: NodelistErrorKind::UnknownKeyword,
    })?;
    let number = fields[1]
        .trim()
        .parse::<u16>()
        .map_err(|_| FtnError::Nodelist {
            reason: NodelistErrorKind::BadNumber,
        })?;

    Ok(Some(NodelistEntry {
        keyword,
        number,
        name: fields[2].to_string(),
        location: fields[3].to_string(),
        sysop: fields[4].to_string(),
        phone: fields[5].to_string(),
        speed: fields[6].to_string(),
        flags: fields[7..]
            .iter()
            .filter(|f| !f.is_empty())
            .map(|f| f.to_string())
            .collect(),
    }))
}

/// Parse a whole nodelist file into entries, skipping comments and blank lines.
///
/// The first malformed data line stops parsing with an error. To resolve each
/// entry's full address, pass the result to [`resolve_addresses`].
pub fn parse(text: &str) -> Result<Vec<NodelistEntry>, FtnError> {
    let mut out = Vec::new();
    for line in text.split('\n') {
        if let Some(entry) = parse_line(line)? {
            out.push(entry);
        }
    }
    Ok(out)
}

/// Walk the positional hierarchy and assign each entry its [`FtnAddress`].
///
/// `Zone N` sets the current zone (and its net) to `N`; `Region`/`Host N` set
/// the current net to `N`; `Hub`/`Pvt`/`Down`/`Hold`/`Node N` set the node
/// number to `N` within the current net. The returned vector is parallel to
/// `entries`.
pub fn resolve_addresses(entries: &[NodelistEntry]) -> Vec<FtnAddress> {
    let mut zone = 0u16;
    let mut net = 0u16;
    let mut out = Vec::with_capacity(entries.len());
    for e in entries {
        let addr = match e.keyword {
            NodeKeyword::Zone => {
                zone = e.number;
                net = e.number;
                FtnAddress::new(zone, net, 0, 0)
            }
            NodeKeyword::Region | NodeKeyword::Host => {
                net = e.number;
                FtnAddress::new(zone, net, 0, 0)
            }
            NodeKeyword::Hub
            | NodeKeyword::Pvt
            | NodeKeyword::Down
            | NodeKeyword::Hold
            | NodeKeyword::Node => FtnAddress::new(zone, net, e.number, 0),
        };
        out.push(addr);
    }
    out
}

/// CRC-16/ARC over `data` (poly `0xA001`, init `0x0000`, reflected in/out).
///
/// This is the checksum used on the nodelist header line. Check value:
/// `crc16(b"123456789") == 0xBB3D`.
pub fn crc16(data: &[u8]) -> u16 {
    let mut crc: u16 = 0;
    for &byte in data {
        crc ^= byte as u16;
        for _ in 0..8 {
            if crc & 1 != 0 {
                crc = (crc >> 1) ^ 0xA001;
            } else {
                crc >>= 1;
            }
        }
    }
    crc
}

/// Extract the declared CRC from a nodelist header line (`;A … : NNNNN`).
///
/// Returns the decimal value after the final colon, or `None` if there is none.
pub fn header_crc(first_line: &str) -> Option<u16> {
    let tail = first_line.rsplit_once(':')?.1.trim();
    tail.parse::<u16>().ok()
}

/// Split `bytes` at the first line's terminator, returning `(first_line_bytes,
/// rest_bytes)`. `rest` is everything the CRC is computed over.
fn split_first_line(bytes: &[u8]) -> (&[u8], &[u8]) {
    match bytes.iter().position(|&b| b == b'\n') {
        Some(i) => (&bytes[..i], &bytes[i + 1..]),
        None => (bytes, &[]),
    }
}

/// Compute the CRC-16 a well-formed nodelist header should carry: [`crc16`] of
/// every byte after the first line's terminator.
pub fn nodelist_body_crc(bytes: &[u8]) -> u16 {
    let (_first, rest) = split_first_line(bytes);
    crc16(rest)
}

/// Verify a nodelist's header CRC against its body.
///
/// Returns [`FtnError::Nodelist`] with
/// [`MissingHeaderCrc`](NodelistErrorKind::MissingHeaderCrc) when the first line
/// carries no parseable CRC, and [`FtnError::Crc`] when the declared value does
/// not match the computed one. On success returns the (matching) CRC.
pub fn verify_nodelist(bytes: &[u8]) -> Result<u16, FtnError> {
    let (first, rest) = split_first_line(bytes);
    let first_line = String::from_utf8_lossy(first);
    let declared = header_crc(&first_line).ok_or(FtnError::Nodelist {
        reason: NodelistErrorKind::MissingHeaderCrc,
    })?;
    let computed = crc16(rest);
    if declared == computed {
        Ok(computed)
    } else {
        Err(FtnError::Crc { declared, computed })
    }
}

/// A single NODEDIFF command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DiffOp {
    Add(usize),
    Copy(usize),
    Delete(usize),
}

fn parse_op(line: &str, diff_line: usize) -> Result<Option<DiffOp>, FtnError> {
    let t = line.trim_end_matches(['\r', '\n']).trim();
    if t.is_empty() {
        return Ok(None);
    }
    let (c, rest) = t.split_at(1);
    let n = rest
        .trim()
        .parse::<usize>()
        .map_err(|_| FtnError::Nodediff {
            reason: NodediffErrorKind::BadCommand { line: diff_line },
        })?;
    Ok(Some(match c {
        "A" | "a" => DiffOp::Add(n),
        "C" | "c" => DiffOp::Copy(n),
        "D" | "d" => DiffOp::Delete(n),
        _ => {
            return Err(FtnError::Nodediff {
                reason: NodediffErrorKind::BadCommand { line: diff_line },
            })
        }
    }))
}

/// Apply a NODEDIFF to a base nodelist, returning the new nodelist.
///
/// The diff is a stream of `A<n>` / `C<n>` / `D<n>` commands (case-insensitive):
///
/// - `C<n>` copies the next `n` lines of the base unchanged;
/// - `D<n>` deletes (skips) the next `n` base lines;
/// - `A<n>` inserts the next `n` lines that follow it *in the diff*.
///
/// **Header handling.** Real NODEDIFF files begin with the new nodelist's header
/// line (which starts with `;`, not a command letter). When the first non-blank
/// diff line is not a valid command, it is emitted as the result's first line
/// and the base's own first line (its old header) is dropped; the remaining
/// commands then apply to the rest of the base. When the diff instead begins
/// with a command, it is applied over the entire base with no special-casing.
///
/// Line terminators are preserved byte-for-byte (copied lines keep the base's,
/// added lines keep the diff's), so [`verify_nodelist`] works on the result.
/// Any command that runs past the end of the base, or an `A` short of its
/// promised data lines, yields [`FtnError::Nodediff`].
pub fn apply_nodediff(base: &str, diff: &str) -> Result<String, FtnError> {
    let base_lines: Vec<&str> = base.split_inclusive('\n').collect();
    let diff_lines: Vec<&str> = diff.split_inclusive('\n').collect();

    let mut out = String::with_capacity(base.len());
    let mut base_idx = 0usize;
    let mut di = 0usize;
    let mut diff_line_no = 0usize;

    // Header mode: if the first non-blank diff line is not a valid command, it
    // is the new header line — emit it and drop the base's old header line.
    while di < diff_lines.len() {
        let candidate = diff_lines[di];
        let trimmed = candidate.trim_end_matches(['\r', '\n']).trim();
        if trimmed.is_empty() {
            di += 1;
            diff_line_no += 1;
            continue;
        }
        let (c, rest) = trimmed.split_at(1);
        let is_command =
            matches!(c, "A" | "a" | "C" | "c" | "D" | "d") && rest.trim().parse::<usize>().is_ok();
        if !is_command {
            out.push_str(candidate);
            if base_idx < base_lines.len() {
                base_idx += 1; // drop old header
            }
            di += 1;
            diff_line_no += 1;
        }
        break;
    }

    while di < diff_lines.len() {
        let cmd_line = diff_lines[di];
        di += 1;
        diff_line_no += 1;
        let op = match parse_op(cmd_line, diff_line_no)? {
            Some(op) => op,
            None => continue, // blank line between commands
        };
        match op {
            DiffOp::Copy(n) => {
                let end = base_idx
                    .checked_add(n)
                    .filter(|&e| e <= base_lines.len())
                    .ok_or(FtnError::Nodediff {
                        reason: NodediffErrorKind::Underflow { line: diff_line_no },
                    })?;
                for line in &base_lines[base_idx..end] {
                    out.push_str(line);
                }
                base_idx = end;
            }
            DiffOp::Delete(n) => {
                let end = base_idx
                    .checked_add(n)
                    .filter(|&e| e <= base_lines.len())
                    .ok_or(FtnError::Nodediff {
                        reason: NodediffErrorKind::Underflow { line: diff_line_no },
                    })?;
                base_idx = end;
            }
            DiffOp::Add(n) => {
                for _ in 0..n {
                    if di >= diff_lines.len() {
                        return Err(FtnError::Nodediff {
                            reason: NodediffErrorKind::MissingAddLines { line: diff_line_no },
                        });
                    }
                    out.push_str(diff_lines[di]);
                    di += 1;
                    diff_line_no += 1;
                }
            }
        }
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
Zone,2,ZoneGate,Europe,Sysop_Name,-Unpublished-,300,CM\r
Host,280,SomeNet,Netherlands,Host_Sysop,31-20-1234567,9600,CM,XA\r
Hub,464,TheHub,Amsterdam,Hub_Sysop,-Unpublished-,9600\r
,464,The_Warren,Amsterdam,Kevin,-Unpublished-,33600,CM,IBN,INA:bbs.example\r
Pvt,999,Secret,Nowhere,Someone,-Unpublished-,9600\r
";

    #[test]
    fn parse_keyword_variants() {
        assert_eq!(NodeKeyword::parse(""), Some(NodeKeyword::Node));
        assert_eq!(NodeKeyword::parse("Zone"), Some(NodeKeyword::Zone));
        assert_eq!(NodeKeyword::parse("host"), Some(NodeKeyword::Host));
        assert_eq!(NodeKeyword::parse("Pvt"), Some(NodeKeyword::Pvt));
        assert_eq!(NodeKeyword::parse("Bogus"), None);
    }

    #[test]
    fn parse_line_fields_and_flags() {
        let e = parse_line(
            ",464,The_Warren,Amsterdam,Kevin,-Unpublished-,33600,CM,IBN,INA:bbs.example",
        )
        .unwrap()
        .unwrap();
        assert_eq!(e.keyword, NodeKeyword::Node);
        assert_eq!(e.number, 464);
        assert_eq!(e.name, "The_Warren");
        assert_eq!(e.sysop, "Kevin");
        assert_eq!(e.speed, "33600");
        assert_eq!(e.flags, vec!["CM", "IBN", "INA:bbs.example"]);
    }

    #[test]
    fn parse_line_comments_and_blanks_are_none() {
        assert_eq!(parse_line(";A comment").unwrap(), None);
        assert_eq!(parse_line("").unwrap(), None);
        assert_eq!(parse_line("   \r\n").unwrap(), None);
    }

    #[test]
    fn parse_line_errors() {
        assert!(matches!(
            parse_line("Zone,2,too,few,fields"),
            Err(FtnError::Nodelist {
                reason: NodelistErrorKind::TooFewFields { found: 5 }
            })
        ));
        assert!(matches!(
            parse_line("Bogus,1,a,b,c,d,e"),
            Err(FtnError::Nodelist {
                reason: NodelistErrorKind::UnknownKeyword
            })
        ));
        assert!(matches!(
            parse_line("Zone,notnum,a,b,c,d,e"),
            Err(FtnError::Nodelist {
                reason: NodelistErrorKind::BadNumber
            })
        ));
    }

    #[test]
    fn parse_and_resolve_addresses() {
        let entries = parse(SAMPLE).unwrap();
        assert_eq!(entries.len(), 5);
        let addrs = resolve_addresses(&entries);
        assert_eq!(addrs[0].to_string(), "2:2/0"); // Zone
        assert_eq!(addrs[1].to_string(), "2:280/0"); // Host opens net 280
        assert_eq!(addrs[2].to_string(), "2:280/464"); // Hub
        assert_eq!(addrs[3].to_string(), "2:280/464"); // node 464
        assert_eq!(addrs[4].to_string(), "2:280/999"); // Pvt node
    }

    #[test]
    fn crc16_check_vector() {
        assert_eq!(crc16(b"123456789"), 0xBB3D);
        assert_eq!(crc16(b""), 0x0000);
    }

    #[test]
    fn header_crc_extraction() {
        let line = ";A FidoNet Nodelist for Friday -- Day number 152 : 46893";
        assert_eq!(header_crc(line), Some(46893));
        assert_eq!(header_crc(";A no crc here"), None);
    }

    #[test]
    fn verify_nodelist_roundtrip() {
        let body = "line one\r\nline two\r\n";
        let crc = crc16(body.as_bytes());
        let file = format!(";A Nodelist -- Day number 001 : {crc}\r\n{body}");
        assert_eq!(verify_nodelist(file.as_bytes()).unwrap(), crc);

        // Tamper with the body -> mismatch.
        let bad = format!(";A Nodelist -- Day number 001 : {crc}\r\nline one\r\nCHANGED\r\n");
        assert!(matches!(
            verify_nodelist(bad.as_bytes()),
            Err(FtnError::Crc { declared, .. }) if declared == crc
        ));

        // No CRC on the header line.
        assert!(matches!(
            verify_nodelist(b";A no crc\r\nbody\r\n"),
            Err(FtnError::Nodelist {
                reason: NodelistErrorKind::MissingHeaderCrc
            })
        ));
    }

    #[test]
    fn apply_nodediff_command_stream() {
        // Base has 4 lines; keep 1, delete 2, add 1, keep 1.
        let base = "L1\r\nL2\r\nL3\r\nL4\r\n";
        let diff = "C1\r\nD2\r\nA1\r\nNEW\r\nC1\r\n";
        let out = apply_nodediff(base, diff).unwrap();
        assert_eq!(out, "L1\r\nNEW\r\nL4\r\n");
    }

    #[test]
    fn apply_nodediff_header_mode_replaces_first_line() {
        let base = ";A OLD header : 0\r\nL1\r\nL2\r\n";
        // First diff line is the new header (not a command): it replaces the old
        // header, then C2 copies the two data lines.
        let diff = ";A NEW header : 12345\r\nC2\r\n";
        let out = apply_nodediff(base, diff).unwrap();
        assert_eq!(out, ";A NEW header : 12345\r\nL1\r\nL2\r\n");
    }

    #[test]
    fn apply_nodediff_then_verify_crc() {
        let base = ";A OLD : 0\r\nAlpha\r\nBravo\r\nCharlie\r\n";
        let body = "Alpha\r\nDelta\r\nCharlie\r\n";
        let crc = crc16(body.as_bytes());
        let diff = format!(";A NEW : {crc}\r\nC1\r\nD1\r\nA1\r\nDelta\r\nC1\r\n");
        let out = apply_nodediff(base, &diff).unwrap();
        assert_eq!(out, format!(";A NEW : {crc}\r\n{body}"));
        assert_eq!(verify_nodelist(out.as_bytes()).unwrap(), crc);
    }

    #[test]
    fn apply_nodediff_errors() {
        // Copy past end.
        assert!(matches!(
            apply_nodediff("L1\r\n", "C5\r\n"),
            Err(FtnError::Nodediff {
                reason: NodediffErrorKind::Underflow { .. }
            })
        ));
        // Add promising more lines than present.
        assert!(matches!(
            apply_nodediff("L1\r\n", "C1\r\nA3\r\nonly\r\n"),
            Err(FtnError::Nodediff {
                reason: NodediffErrorKind::MissingAddLines { .. }
            })
        ));
        // Bad command letter.
        assert!(matches!(
            apply_nodediff("L1\r\n", "C1\r\nX2\r\n"),
            Err(FtnError::Nodediff {
                reason: NodediffErrorKind::BadCommand { .. }
            })
        ));
    }

    #[test]
    fn parse_never_panics_on_junk() {
        for junk in [
            "",
            ";;;",
            ",,,,,,",
            "\r\n\r\n",
            "Zone",
            "\u{ffff}bogus,line",
        ] {
            let _ = parse(junk);
        }
        let _ = verify_nodelist(&[0xff; 32]);
        let _ = apply_nodediff("", "");
        let _ = apply_nodediff("junk", "junk");
    }
}
