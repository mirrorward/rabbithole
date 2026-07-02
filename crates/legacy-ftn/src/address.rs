//! FidoNet Technology Network (FTN) node addressing.
//!
//! An FTN address locates a node within the store-and-forward network:
//!
//! ```text
//!   zone : net / node . point
//!    2   :  280 / 464  .  0
//!   └┬─┘   └─┬─┘ └─┬─┘  └┬┘
//!   region  hub  system  fidonet "point" (0 == the node itself)
//! ```
//!
//! The textual form is `zone:net/node.point`; the `.point` suffix is omitted
//! when the point is 0 (a plain node). Every component is a 16-bit unsigned
//! value, matching the on-the-wire packet fields.

use std::fmt;
use std::str::FromStr;

use crate::error::{AddressErrorKind, FtnError};

/// A 4-dimensional FidoNet address (`zone:net/node.point`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct FtnAddress {
    /// Zone (region). Zone 0 is reserved / unknown.
    pub zone: u16,
    /// Net (hub) number.
    pub net: u16,
    /// Node number within the net.
    pub node: u16,
    /// Point number; 0 means the node itself (no point).
    pub point: u16,
}

impl FtnAddress {
    /// Construct an address from its four components.
    pub fn new(zone: u16, net: u16, node: u16, point: u16) -> Self {
        FtnAddress {
            zone,
            net,
            node,
            point,
        }
    }

    /// True when this address names a point (`point != 0`).
    pub fn is_point(&self) -> bool {
        self.point != 0
    }
}

impl fmt::Display for FtnAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}/{}", self.zone, self.net, self.node)?;
        if self.point != 0 {
            write!(f, ".{}", self.point)?;
        }
        Ok(())
    }
}

impl FromStr for FtnAddress {
    type Err = FtnError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        // Strip an optional `@domain` suffix (5D form); it is not modelled.
        let core = s.split('@').next().unwrap_or(s);
        let err = |reason| FtnError::Address {
            input: s.to_string(),
            reason,
        };

        let (zone_str, rest) = core
            .split_once(':')
            .ok_or_else(|| err(AddressErrorKind::MissingZone))?;
        let (net_str, node_part) = rest
            .split_once('/')
            .ok_or_else(|| err(AddressErrorKind::MissingNode))?;
        let (node_str, point_str) = match node_part.split_once('.') {
            Some((n, p)) => (n, Some(p)),
            None => (node_part, None),
        };

        let parse = |v: &str| {
            v.parse::<u16>()
                .map_err(|_| err(AddressErrorKind::BadNumber))
        };
        let zone = parse(zone_str)?;
        let net = parse(net_str)?;
        let node = parse(node_str)?;
        let point = match point_str {
            Some(p) => parse(p)?,
            None => 0,
        };

        Ok(FtnAddress::new(zone, net, node, point))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_omits_zero_point() {
        assert_eq!(FtnAddress::new(2, 280, 464, 0).to_string(), "2:280/464");
        assert_eq!(FtnAddress::new(1, 104, 1024, 5).to_string(), "1:104/1024.5");
    }

    #[test]
    fn parse_roundtrip() {
        for s in ["2:280/464", "1:104/1024.5", "3:633/280.1", "1:1/0"] {
            let a: FtnAddress = s.parse().unwrap();
            assert_eq!(a.to_string(), s);
        }
    }

    #[test]
    fn parse_ignores_domain() {
        let a: FtnAddress = "2:280/464.0@fidonet".parse().unwrap();
        assert_eq!(a, FtnAddress::new(2, 280, 464, 0));
    }

    #[test]
    fn parse_errors_do_not_panic() {
        for bad in ["", "garbage", "2/280", "2:280", "2:x/1", ":/", "2:280/"] {
            assert!(
                bad.parse::<FtnAddress>().is_err(),
                "expected err for {bad:?}"
            );
        }
    }
}
