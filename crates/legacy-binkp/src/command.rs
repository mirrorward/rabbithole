//! binkp command frames: the `M_*` message set and their typed arguments.
//!
//! A command block (top bit of the header set) is a 1-byte command id followed
//! by ASCII argument bytes:
//!
//! ```text
//!   ┌────┬──────────────────────────────┐
//!   │ id │ args (ASCII, command-specific)│
//!   └────┴──────────────────────────────┘
//! ```
//!
//! The arguments are command-specific text. This module maps each id to a
//! typed [`Command`] and back:
//!
//! ```text
//!   id name     args
//!   0  M_NUL    free-form info line ("SYS …", "OPT CRAM-MD5-…", …)
//!   1  M_ADR    space-separated 5D addresses
//!   2  M_PWD    password, or "CRAM-MD5-<hex>"
//!   3  M_FILE   name size unixtime offset
//!   4  M_OK     free-form text (usually "secure"/"non-secure")
//!   5  M_EOB    (no args)
//!   6  M_GOT    name size unixtime
//!   7  M_ERR    free-form error text
//!   8  M_BSY    free-form busy text
//!   9  M_GET    name size unixtime
//!   10 M_SKIP   name size unixtime
//! ```
//!
//! Parsing is total: an unknown id, non-UTF-8 args, or a malformed field
//! yields [`CommandError`] instead of panicking.

use std::fmt;

use thiserror::Error;

use crate::address::{format_address_list, parse_address_list, Address, AddressError};
use crate::frame::RawBlock;

/// The binkp command ids (`M_*`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CommandId {
    /// `M_NUL` (0): informational, ignored for protocol flow.
    Nul = 0,
    /// `M_ADR` (1): the sender's addresses.
    Adr = 1,
    /// `M_PWD` (2): session password (plaintext or CRAM-MD5).
    Pwd = 2,
    /// `M_FILE` (3): announce a file about to be sent.
    File = 3,
    /// `M_OK` (4): password accepted.
    Ok = 4,
    /// `M_EOB` (5): end of batch (no more files this direction).
    Eob = 5,
    /// `M_GOT` (6): file received successfully.
    Got = 6,
    /// `M_ERR` (7): fatal error, session aborting.
    Err = 7,
    /// `M_BSY` (8): busy, try later.
    Bsy = 8,
    /// `M_GET` (9): request (re)send of a file from an offset.
    Get = 9,
    /// `M_SKIP` (10): skip this file (do not send now).
    Skip = 10,
}

impl CommandId {
    /// Map a raw id byte to a [`CommandId`], if known.
    pub fn from_u8(v: u8) -> Option<Self> {
        Some(match v {
            0 => CommandId::Nul,
            1 => CommandId::Adr,
            2 => CommandId::Pwd,
            3 => CommandId::File,
            4 => CommandId::Ok,
            5 => CommandId::Eob,
            6 => CommandId::Got,
            7 => CommandId::Err,
            8 => CommandId::Bsy,
            9 => CommandId::Get,
            10 => CommandId::Skip,
            _ => return None,
        })
    }

    /// The raw id byte.
    pub fn to_u8(self) -> u8 {
        self as u8
    }
}

/// A file named in an `M_FILE` frame: `name size unixtime offset`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileInfo {
    /// The file name (no embedded spaces in binkp/1.0).
    pub name: String,
    /// The file size in bytes.
    pub size: u64,
    /// The file's Unix modification time (seconds since epoch).
    pub unixtime: u64,
    /// The offset the transfer should start from (0 for a fresh send).
    pub offset: u64,
}

impl FileInfo {
    /// Construct a whole-file (offset 0) announcement.
    pub fn new(name: impl Into<String>, size: u64, unixtime: u64) -> Self {
        FileInfo {
            name: name.into(),
            size,
            unixtime,
            offset: 0,
        }
    }

    /// The [`FileId`] identifying this file (name/size/time), dropping offset.
    pub fn id(&self) -> FileId {
        FileId {
            name: self.name.clone(),
            size: self.size,
            unixtime: self.unixtime,
        }
    }
}

impl fmt::Display for FileInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} {} {} {}",
            self.name, self.size, self.unixtime, self.offset
        )
    }
}

/// A file identity used by `M_GOT`/`M_GET`/`M_SKIP`: `name size unixtime`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileId {
    /// The file name.
    pub name: String,
    /// The file size in bytes.
    pub size: u64,
    /// The file's Unix modification time (seconds since epoch).
    pub unixtime: u64,
}

impl FileId {
    /// Construct a file identity.
    pub fn new(name: impl Into<String>, size: u64, unixtime: u64) -> Self {
        FileId {
            name: name.into(),
            size,
            unixtime,
        }
    }
}

impl fmt::Display for FileId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {} {}", self.name, self.size, self.unixtime)
    }
}

/// A typed binkp command frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// `M_NUL`: a free-form informational line.
    Nul(String),
    /// `M_ADR`: the sender's 5D addresses.
    Adr(Vec<Address>),
    /// `M_PWD`: the session password (plaintext, `-`, or `CRAM-MD5-<hex>`).
    Pwd(String),
    /// `M_FILE`: announce an outgoing file.
    File(FileInfo),
    /// `M_OK`: password accepted (with optional descriptive text).
    Ok(String),
    /// `M_EOB`: end of batch.
    Eob,
    /// `M_GOT`: file received.
    Got(FileId),
    /// `M_ERR`: fatal error text.
    Err(String),
    /// `M_BSY`: busy text.
    Bsy(String),
    /// `M_GET`: request (re)send of a file.
    Get(FileId),
    /// `M_SKIP`: skip a file.
    Skip(FileId),
}

/// Errors from parsing/serializing command frames.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum CommandError {
    /// The frame's command id byte is not a known `M_*` command.
    #[error("unknown command id {0}")]
    UnknownId(u8),
    /// Command arguments were not valid UTF-8/ASCII text.
    #[error("command {0:?} arguments are not valid UTF-8")]
    BadUtf8(CommandId),
    /// A file argument list had the wrong number of whitespace fields.
    #[error("command {id:?} expected {expected} fields, found {found}")]
    WrongFieldCount {
        /// The command being parsed.
        id: CommandId,
        /// Number of fields expected.
        expected: usize,
        /// Number of fields found.
        found: usize,
    },
    /// A numeric field (size/time/offset) failed to parse.
    #[error("command {id:?} has invalid {field}: {value:?}")]
    BadNumber {
        /// The command being parsed.
        id: CommandId,
        /// Which field failed.
        field: &'static str,
        /// The offending text.
        value: String,
    },
    /// An address in an `M_ADR` frame failed to parse.
    #[error("command M_ADR has a bad address: {0}")]
    BadAddress(#[from] AddressError),
    /// The block passed to [`Command::from_block`] was a data block.
    #[error("expected a command block, found a data block")]
    NotACommand,
}

/// Parse `name size unixtime` (used by `M_GOT`/`M_GET`/`M_SKIP`).
fn parse_file_id(id: CommandId, args: &str) -> Result<FileId, CommandError> {
    let fields: Vec<&str> = args.split_whitespace().collect();
    if fields.len() != 3 {
        return Err(CommandError::WrongFieldCount {
            id,
            expected: 3,
            found: fields.len(),
        });
    }
    let num = |field: &'static str, value: &str| -> Result<u64, CommandError> {
        value.parse::<u64>().map_err(|_| CommandError::BadNumber {
            id,
            field,
            value: value.to_string(),
        })
    };
    Ok(FileId {
        name: fields[0].to_string(),
        size: num("size", fields[1])?,
        unixtime: num("unixtime", fields[2])?,
    })
}

/// Parse `name size unixtime offset` (used by `M_FILE`).
fn parse_file_info(args: &str) -> Result<FileInfo, CommandError> {
    let id = CommandId::File;
    let fields: Vec<&str> = args.split_whitespace().collect();
    if fields.len() != 4 {
        return Err(CommandError::WrongFieldCount {
            id,
            expected: 4,
            found: fields.len(),
        });
    }
    let num = |field: &'static str, value: &str| -> Result<u64, CommandError> {
        value.parse::<u64>().map_err(|_| CommandError::BadNumber {
            id,
            field,
            value: value.to_string(),
        })
    };
    Ok(FileInfo {
        name: fields[0].to_string(),
        size: num("size", fields[1])?,
        unixtime: num("unixtime", fields[2])?,
        offset: num("offset", fields[3])?,
    })
}

impl Command {
    /// This command's id.
    pub fn id(&self) -> CommandId {
        match self {
            Command::Nul(_) => CommandId::Nul,
            Command::Adr(_) => CommandId::Adr,
            Command::Pwd(_) => CommandId::Pwd,
            Command::File(_) => CommandId::File,
            Command::Ok(_) => CommandId::Ok,
            Command::Eob => CommandId::Eob,
            Command::Got(_) => CommandId::Got,
            Command::Err(_) => CommandId::Err,
            Command::Bsy(_) => CommandId::Bsy,
            Command::Get(_) => CommandId::Get,
            Command::Skip(_) => CommandId::Skip,
        }
    }

    /// Render this command's argument bytes (ASCII text, no id, no header).
    pub fn args(&self) -> Vec<u8> {
        let text = match self {
            Command::Nul(s)
            | Command::Pwd(s)
            | Command::Ok(s)
            | Command::Err(s)
            | Command::Bsy(s) => s.clone(),
            Command::Adr(addrs) => format_address_list(addrs),
            Command::File(info) => info.to_string(),
            Command::Eob => String::new(),
            Command::Got(f) | Command::Get(f) | Command::Skip(f) => f.to_string(),
        };
        text.into_bytes()
    }

    /// Serialize this command as a [`RawBlock::Command`].
    pub fn to_block(&self) -> RawBlock {
        RawBlock::Command {
            id: self.id().to_u8(),
            args: self.args(),
        }
    }

    /// Parse a typed command from a raw id byte and its argument bytes.
    pub fn parse(id: u8, args: &[u8]) -> Result<Command, CommandError> {
        let cid = CommandId::from_u8(id).ok_or(CommandError::UnknownId(id))?;
        let text = std::str::from_utf8(args).map_err(|_| CommandError::BadUtf8(cid))?;
        Ok(match cid {
            CommandId::Nul => Command::Nul(text.to_string()),
            CommandId::Adr => Command::Adr(parse_address_list(text)?),
            CommandId::Pwd => Command::Pwd(text.to_string()),
            CommandId::File => Command::File(parse_file_info(text)?),
            CommandId::Ok => Command::Ok(text.to_string()),
            CommandId::Eob => Command::Eob,
            CommandId::Got => Command::Got(parse_file_id(cid, text)?),
            CommandId::Err => Command::Err(text.to_string()),
            CommandId::Bsy => Command::Bsy(text.to_string()),
            CommandId::Get => Command::Get(parse_file_id(cid, text)?),
            CommandId::Skip => Command::Skip(parse_file_id(cid, text)?),
        })
    }

    /// Parse a typed command from a [`RawBlock`] (must be a command block).
    pub fn from_block(block: &RawBlock) -> Result<Command, CommandError> {
        match block {
            RawBlock::Command { id, args } => Command::parse(*id, args),
            RawBlock::Data(_) => Err(CommandError::NotACommand),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(cmd: Command) {
        let block = cmd.to_block();
        let back = Command::from_block(&block).unwrap();
        assert_eq!(cmd, back);
    }

    #[test]
    fn all_commands_round_trip() {
        round_trip(Command::Nul("SYS RabbitHole".into()));
        round_trip(Command::Adr(vec![
            Address::new(2, 5020, 1042, 0).with_domain("fidonet"),
            Address::new(1, 120, 5, 3),
        ]));
        round_trip(Command::Pwd("CRAM-MD5-deadbeef".into()));
        round_trip(Command::File(FileInfo {
            name: "netmail.pkt".into(),
            size: 1234,
            unixtime: 1_700_000_000,
            offset: 512,
        }));
        round_trip(Command::Ok("secure".into()));
        round_trip(Command::Eob);
        round_trip(Command::Got(FileId::new(
            "netmail.pkt",
            1234,
            1_700_000_000,
        )));
        round_trip(Command::Err("bad password".into()));
        round_trip(Command::Bsy("all lines busy".into()));
        round_trip(Command::Get(FileId::new("a.zip", 9, 1)));
        round_trip(Command::Skip(FileId::new("b.zip", 9, 1)));
    }

    #[test]
    fn file_parses_four_fields() {
        let cmd = Command::parse(3, b"file.zip 4096 1700000000 0").unwrap();
        assert_eq!(
            cmd,
            Command::File(FileInfo::new("file.zip", 4096, 1_700_000_000))
        );
    }

    #[test]
    fn wrong_field_count_is_error() {
        assert!(matches!(
            Command::parse(3, b"file.zip 4096 0"),
            Err(CommandError::WrongFieldCount {
                id: CommandId::File,
                expected: 4,
                found: 3
            })
        ));
        assert!(matches!(
            Command::parse(6, b"file.zip 4096"),
            Err(CommandError::WrongFieldCount {
                id: CommandId::Got,
                expected: 3,
                found: 2
            })
        ));
    }

    #[test]
    fn bad_number_is_error() {
        assert!(matches!(
            Command::parse(3, b"file.zip big 0 0"),
            Err(CommandError::BadNumber {
                id: CommandId::File,
                field: "size",
                ..
            })
        ));
    }

    #[test]
    fn unknown_id_is_error() {
        assert_eq!(Command::parse(200, b""), Err(CommandError::UnknownId(200)));
    }

    #[test]
    fn non_utf8_args_are_error() {
        assert_eq!(
            Command::parse(0, &[0xff, 0xfe]),
            Err(CommandError::BadUtf8(CommandId::Nul))
        );
    }

    #[test]
    fn eob_has_no_args() {
        let block = Command::Eob.to_block();
        assert_eq!(
            block,
            RawBlock::Command {
                id: 5,
                args: vec![]
            }
        );
    }

    #[test]
    fn data_block_is_not_a_command() {
        let block = RawBlock::Data(vec![1, 2, 3]);
        assert_eq!(Command::from_block(&block), Err(CommandError::NotACommand));
    }
}
