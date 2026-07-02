//! Door definitions ([`DoorDef`]) and the [`DoorRegistry`] that holds them.
//!
//! A [`DoorDef`] is the static, config-shaped description of one installable
//! door game: what to run ([`DoorDef::command`]), where
//! ([`DoorDef::working_dir`]), which drop file it reads
//! ([`DoorDef::dropfile`]), how its I/O is wired ([`IoMode`]), which nodes it
//! may occupy ([`NodeRange`]) and an optional per-user daily time budget.
//!
//! Everything here is **pure data**. Nothing spawns a process — the burrow
//! slice that owns tokio turns a validated [`DoorDef`] into a real child
//! process later (see the crate docs for the seam). All types derive `serde`
//! `Serialize`/`Deserialize` with TOML-friendly field shapes, so a sysop's
//! door list can live in a `[[doors]]` array of tables:
//!
//! ```toml
//! [[doors]]
//! id = "lord"
//! title = "Legend of the Red Dragon"
//! command = ["dosemu", "-quiet", "LORD.BAT"]
//! working_dir = "/opt/doors/lord"
//! dropfile = "door.sys"
//! io_mode = "stdio"
//! nodes = { first = 1, last = 4 }
//! daily_limit_mins = 30
//! ```

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::dropfile::DropFile;
use crate::error::Error;

/// How the door's byte stream is wired to the caller.
///
/// This selects the bridging strategy (and, for [`Socket`](IoMode::Socket),
/// telnet-IAC escaping in the [`BridgeBuffer`](crate::BridgeBuffer)); the
/// actual pipe/socket plumbing lives in the driving slice, not here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IoMode {
    /// The door talks on its stdin/stdout (native or dosemu-style doors).
    #[default]
    Stdio,
    /// The door is handed a socket handle (DOOR32.SYS comm-type-2 style);
    /// the remote leg is a telnet stream, so `0xFF` must be IAC-escaped.
    Socket,
}

/// An inclusive range of node numbers a door may run on.
///
/// Node numbers are 1-based, as every classic drop file expects. A range with
/// `first == last` is a **single-node** door: only one caller can be inside
/// it at a time (the [`NodePool`](crate::NodePool) enforces this by simply
/// having no second free number to hand out).
///
/// ```
/// use rabbithole_legacy_doors::NodeRange;
///
/// let solo = NodeRange::single(3);
/// assert!(solo.is_single());
/// assert!(solo.contains(3) && !solo.contains(4));
/// assert_eq!(NodeRange::new(2, 5).count(), 4);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct NodeRange {
    /// First node number (inclusive, 1-based).
    pub first: u16,
    /// Last node number (inclusive).
    pub last: u16,
}

impl NodeRange {
    /// A range covering `first..=last`.
    #[must_use]
    pub const fn new(first: u16, last: u16) -> Self {
        NodeRange { first, last }
    }

    /// A single-node range: only `node` itself.
    #[must_use]
    pub const fn single(node: u16) -> Self {
        NodeRange {
            first: node,
            last: node,
        }
    }

    /// The unrestricted range: any node the pool can offer.
    #[must_use]
    pub const fn any() -> Self {
        NodeRange {
            first: 1,
            last: u16::MAX,
        }
    }

    /// Whether `node` falls inside this range.
    #[must_use]
    pub const fn contains(self, node: u16) -> bool {
        node >= self.first && node <= self.last
    }

    /// Number of node slots in the range (`0` for an invalid range).
    #[must_use]
    pub const fn count(self) -> u32 {
        if self.is_valid() {
            self.last as u32 - self.first as u32 + 1
        } else {
            0
        }
    }

    /// Whether this is a single-node range.
    #[must_use]
    pub const fn is_single(self) -> bool {
        self.is_valid() && self.first == self.last
    }

    /// A range is valid when it is non-empty and 1-based
    /// (`1 <= first <= last`).
    #[must_use]
    pub const fn is_valid(self) -> bool {
        self.first >= 1 && self.first <= self.last
    }
}

impl Default for NodeRange {
    /// Defaults to [`NodeRange::any`], so a config that omits `nodes` lets
    /// the door run on every node.
    fn default() -> Self {
        NodeRange::any()
    }
}

/// The static definition of one installable door game.
///
/// Fields marked `#[serde(default)]` may be omitted from config; see the
/// module docs for a TOML example. Definitions are plain data — call
/// [`DoorDef::validate`] (or add them through a [`DoorRegistry`], which
/// validates for you) before trusting one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DoorDef {
    /// Stable identifier used as the registry key (e.g. `"lord"`).
    /// Must be non-empty and contain no whitespace.
    pub id: String,
    /// Human-readable title shown on the door menu.
    pub title: String,
    /// Argv vector: `command[0]` is the program, the rest are its arguments.
    /// Must be non-empty with a non-empty program.
    pub command: Vec<String>,
    /// Working directory to run the door in. `None` means the driver runs it
    /// in the session's drop directory (where the drop file was written).
    #[serde(default)]
    pub working_dir: Option<PathBuf>,
    /// Which drop-file format the door reads.
    pub dropfile: DropFile,
    /// How the door's byte stream is wired to the caller.
    #[serde(default)]
    pub io_mode: IoMode,
    /// Which nodes the door may occupy; a single-node range serializes the
    /// door to one caller at a time. Defaults to [`NodeRange::any`].
    #[serde(default)]
    pub nodes: NodeRange,
    /// Per-user daily time budget in minutes; `None` means unlimited.
    /// Enforcement (tracking per-user usage) belongs to the driving slice.
    #[serde(default)]
    pub daily_limit_mins: Option<u32>,
}

impl DoorDef {
    /// Check this definition for internal consistency.
    ///
    /// Rejects: empty or whitespace-containing `id`, empty `title`, an empty
    /// argv (or empty program), an invalid [`NodeRange`], and a zero-minute
    /// daily limit (use `None` for "unlimited" instead).
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidDoor`] naming the offending field.
    pub fn validate(&self) -> Result<(), Error> {
        let fail = |reason: &'static str| {
            Err(Error::InvalidDoor {
                id: self.id.clone(),
                reason,
            })
        };
        if self.id.is_empty() {
            return fail("id is empty");
        }
        if self.id.chars().any(char::is_whitespace) {
            return fail("id contains whitespace");
        }
        if self.title.trim().is_empty() {
            return fail("title is empty");
        }
        if self.command.first().is_none_or(|p| p.trim().is_empty()) {
            return fail("command has no program");
        }
        if !self.nodes.is_valid() {
            return fail("node range is invalid (needs 1 <= first <= last)");
        }
        if self.daily_limit_mins == Some(0) {
            return fail("daily time limit of zero (use no limit instead)");
        }
        Ok(())
    }

    /// The program to execute (`command[0]`), if any.
    #[must_use]
    pub fn program(&self) -> Option<&str> {
        self.command.first().map(String::as_str)
    }

    /// The program's arguments (`command[1..]`).
    #[must_use]
    pub fn args(&self) -> &[String] {
        self.command.get(1..).unwrap_or(&[])
    }

    /// Whether this door is restricted to a single node (one caller at a
    /// time).
    #[must_use]
    pub fn is_single_node(&self) -> bool {
        self.nodes.is_single()
    }
}

/// An ordered collection of [`DoorDef`]s, keyed by their `id`.
///
/// Insertion order is preserved (it is the sysop's menu order). [`add`]
/// validates the definition and rejects duplicate ids. The registry
/// serializes transparently as a plain array of door tables; because
/// deserialization bypasses [`add`], call [`validate`](DoorRegistry::validate)
/// after loading config.
///
/// ```
/// use rabbithole_legacy_doors::{DoorDef, DoorRegistry, DropFile, IoMode, NodeRange};
///
/// let mut reg = DoorRegistry::new();
/// reg.add(DoorDef {
///     id: "tw2002".into(),
///     title: "Trade Wars 2002".into(),
///     command: vec!["tw2002".into()],
///     working_dir: None,
///     dropfile: DropFile::Door32Sys,
///     io_mode: IoMode::Socket,
///     nodes: NodeRange::any(),
///     daily_limit_mins: None,
/// })
/// .unwrap();
/// assert_eq!(reg.get("tw2002").unwrap().title, "Trade Wars 2002");
/// ```
///
/// [`add`]: DoorRegistry::add
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DoorRegistry {
    doors: Vec<DoorDef>,
}

impl DoorRegistry {
    /// An empty registry.
    #[must_use]
    pub fn new() -> Self {
        DoorRegistry::default()
    }

    /// Validate `def` and append it.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidDoor`] if the definition fails [`DoorDef::validate`],
    /// or [`Error::DuplicateDoor`] if a door with the same id is already
    /// registered. On error the registry is unchanged.
    pub fn add(&mut self, def: DoorDef) -> Result<(), Error> {
        def.validate()?;
        if self.get(&def.id).is_some() {
            return Err(Error::DuplicateDoor(def.id));
        }
        self.doors.push(def);
        Ok(())
    }

    /// Remove the door with this id, returning it if present.
    pub fn remove(&mut self, id: &str) -> Option<DoorDef> {
        let pos = self.doors.iter().position(|d| d.id == id)?;
        Some(self.doors.remove(pos))
    }

    /// Look up a door by id.
    #[must_use]
    pub fn get(&self, id: &str) -> Option<&DoorDef> {
        self.doors.iter().find(|d| d.id == id)
    }

    /// All doors, in insertion (menu) order.
    #[must_use]
    pub fn list(&self) -> &[DoorDef] {
        &self.doors
    }

    /// Number of registered doors.
    #[must_use]
    pub fn len(&self) -> usize {
        self.doors.len()
    }

    /// Whether the registry is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.doors.is_empty()
    }

    /// Re-run [`DoorDef::validate`] on every entry and check for duplicate
    /// ids. Use this after deserializing a registry from config, since serde
    /// bypasses [`add`](DoorRegistry::add).
    ///
    /// # Errors
    ///
    /// The first [`Error::InvalidDoor`] or [`Error::DuplicateDoor`] found.
    pub fn validate(&self) -> Result<(), Error> {
        for (i, def) in self.doors.iter().enumerate() {
            def.validate()?;
            if self.doors[..i].iter().any(|d| d.id == def.id) {
                return Err(Error::DuplicateDoor(def.id.clone()));
            }
        }
        Ok(())
    }
}
