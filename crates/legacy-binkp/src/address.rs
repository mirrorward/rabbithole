//! 5D FidoNet addressing as carried in binkp `M_ADR` frames.
//!
//! binkp advertises node addresses in the "5D" textual form:
//!
//! ```text
//!   zone : net / node . point @ domain
//!    2   : 5020 / 1042 .  0   @ fidonet
//!   └┬─┘  └─┬──┘ └─┬──┘ └┬┘   └──┬────┘
//!   region  hub   node  point   network domain (5th dimension)
//! ```
//!
//! The `.point` suffix and `@domain` suffix are both optional on input; an
//! absent point defaults to 0. `M_ADR` carries one or more of these separated
//! by single spaces. Parsing is total — malformed text yields
//! [`AddressError`] rather than a panic.

use std::fmt;
use std::str::FromStr;

use thiserror::Error;

/// A 5D FidoNet address (`zone:net/node.point@domain`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct Address {
    /// Zone (region). Zone 0 is reserved / unknown.
    pub zone: u16,
    /// Net (hub) number.
    pub net: u16,
    /// Node number within the net.
    pub node: u16,
    /// Point number; 0 means the node itself (no point).
    pub point: u16,
    /// Optional network domain (the 5th dimension), e.g. `fidonet`.
    pub domain: Option<String>,
}

impl Address {
    /// Construct an address from its four numeric components and no domain.
    pub fn new(zone: u16, net: u16, node: u16, point: u16) -> Self {
        Address {
            zone,
            net,
            node,
            point,
            domain: None,
        }
    }

    /// Attach a domain, consuming and returning `self` (builder style).
    pub fn with_domain(mut self, domain: impl Into<String>) -> Self {
        self.domain = Some(domain.into());
        self
    }
}

/// Errors from parsing a 5D address.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum AddressError {
    /// The `zone:` separator was missing.
    #[error("address {0:?} missing zone separator ':'")]
    MissingZone(String),
    /// The `/node` separator was missing.
    #[error("address {0:?} missing node separator '/'")]
    MissingNode(String),
    /// A numeric component failed to parse as a u16.
    #[error("address {input:?} has invalid {field}: {value:?}")]
    BadNumber {
        /// The full input, for context.
        input: String,
        /// Which component failed (zone/net/node/point).
        field: &'static str,
        /// The offending substring.
        value: String,
    },
    /// The address text was empty.
    #[error("empty address")]
    Empty,
}

impl fmt::Display for Address {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // binkp M_ADR uses the full 5D form; always emit the point.
        write!(f, "{}:{}/{}.{}", self.zone, self.net, self.node, self.point)?;
        if let Some(domain) = &self.domain {
            write!(f, "@{domain}")?;
        }
        Ok(())
    }
}

impl FromStr for Address {
    type Err = AddressError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.is_empty() {
            return Err(AddressError::Empty);
        }
        let (core, domain) = match s.split_once('@') {
            Some((core, dom)) => (core, Some(dom.to_string())),
            None => (s, None),
        };
        let parse = |field: &'static str, value: &str| -> Result<u16, AddressError> {
            value.parse::<u16>().map_err(|_| AddressError::BadNumber {
                input: s.to_string(),
                field,
                value: value.to_string(),
            })
        };

        let (zone_str, rest) = core
            .split_once(':')
            .ok_or_else(|| AddressError::MissingZone(s.to_string()))?;
        let (net_str, node_part) = rest
            .split_once('/')
            .ok_or_else(|| AddressError::MissingNode(s.to_string()))?;
        let (node_str, point_str) = match node_part.split_once('.') {
            Some((n, p)) => (n, Some(p)),
            None => (node_part, None),
        };

        Ok(Address {
            zone: parse("zone", zone_str)?,
            net: parse("net", net_str)?,
            node: parse("node", node_str)?,
            point: match point_str {
                Some(p) => parse("point", p)?,
                None => 0,
            },
            domain,
        })
    }
}

/// Parse a space-separated list of 5D addresses (an `M_ADR` argument).
pub fn parse_address_list(s: &str) -> Result<Vec<Address>, AddressError> {
    s.split_whitespace().map(Address::from_str).collect()
}

/// Render addresses as a single space-separated `M_ADR` argument string.
pub fn format_address_list(addrs: &[Address]) -> String {
    addrs
        .iter()
        .map(|a| a.to_string())
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_full_5d() {
        let a: Address = "2:5020/1042.7@fidonet".parse().unwrap();
        assert_eq!(a, Address::new(2, 5020, 1042, 7).with_domain("fidonet"));
    }

    #[test]
    fn parses_without_point_or_domain() {
        let a: Address = "1:234/56".parse().unwrap();
        assert_eq!(a, Address::new(1, 234, 56, 0));
        assert_eq!(a.point, 0);
        assert_eq!(a.domain, None);
    }

    #[test]
    fn display_round_trips_through_parse() {
        let a = Address::new(3, 712, 620, 1).with_domain("fsxnet");
        let text = a.to_string();
        assert_eq!(text, "3:712/620.1@fsxnet");
        assert_eq!(text.parse::<Address>().unwrap(), a);
    }

    #[test]
    fn address_list_round_trips() {
        let list = vec![
            Address::new(2, 5020, 1042, 0).with_domain("fidonet"),
            Address::new(1, 120, 500, 3),
        ];
        let text = format_address_list(&list);
        assert_eq!(text, "2:5020/1042.0@fidonet 1:120/500.3");
        assert_eq!(parse_address_list(&text).unwrap(), list);
    }

    #[test]
    fn rejects_malformed() {
        assert_eq!("".parse::<Address>(), Err(AddressError::Empty));
        assert!(matches!(
            "5020/1042".parse::<Address>(),
            Err(AddressError::MissingZone(_))
        ));
        assert!(matches!(
            "2:5020".parse::<Address>(),
            Err(AddressError::MissingNode(_))
        ));
        assert!(matches!(
            "2:net/1042".parse::<Address>(),
            Err(AddressError::BadNumber { .. })
        ));
        // Out-of-range for u16.
        assert!(matches!(
            "2:99999/1".parse::<Address>(),
            Err(AddressError::BadNumber { .. })
        ));
    }
}
