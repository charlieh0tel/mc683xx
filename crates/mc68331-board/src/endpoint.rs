//! Net endpoints by name: `peripheral.port`, `chip.PIN` and
//! `external.name` ([`docs/board-files.md`, Nets][board-files-net]).
//!
//! A chip pin's name is the chip's to check, when the board loads
//! ([`ChipPin`]).
//!
//! [board-files-net]: https://github.com/charlieh0tel/mc683xx/blob/main/docs/board-files.md#net
//! [`ChipPin`]: crate::chip::ChipPin

use std::fmt;
use std::str::FromStr;

use serde::Deserialize;
use serde::Serialize;

/// An endpoint as a board file names it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[non_exhaustive]
pub enum Endpoint {
    /// A peripheral's port: `peripheral.port`.
    Peripheral {
        /// The peripheral.
        peripheral: String,
        /// The port.
        port: String,
    },
    /// A chip pin, by the name after `chip.`: `chip.PIN`.
    Chip(String),
    /// A run-time endpoint the runner or host binds: `external.name`.
    External(String),
}

/// The prefix of a chip pin.
const CHIP: &str = "chip";
/// The prefix of an external endpoint.
const EXTERNAL: &str = "external";

/// The endpoint named so: `peripheral.port`, `chip.PIN`, `external.name`.
impl FromStr for Endpoint {
    type Err = ParseEndpointError;

    fn from_str(name: &str) -> Result<Self, ParseEndpointError> {
        Self::parse(name).ok_or_else(|| ParseEndpointError(name.to_owned()))
    }
}

/// Its name, as a board file writes it.
impl fmt::Display for Endpoint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Peripheral { peripheral, port } => write!(formatter, "{peripheral}.{port}"),
            Self::Chip(pin) => write!(formatter, "{CHIP}.{pin}"),
            Self::External(name) => write!(formatter, "{EXTERNAL}.{name}"),
        }
    }
}

/// Text that names no endpoint.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("no endpoint {0:?}")]
pub struct ParseEndpointError(pub String);

impl Endpoint {
    /// The endpoint `name`, if it is one.
    pub(crate) fn parse(name: &str) -> Option<Self> {
        let (head, tail) = name.split_once('.')?;
        if head.is_empty() || tail.is_empty() {
            return None;
        }
        Some(match head {
            CHIP => Self::Chip(tail.to_owned()),
            EXTERNAL => Self::External(tail.to_owned()),
            _ => Self::Peripheral {
                peripheral: head.to_owned(),
                port: tail.to_owned(),
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::Endpoint;

    #[test]
    fn endpoints_show_as_they_parse() {
        for name in [
            "fpga.pps",
            "chip.PGP0",
            "chip.IRQ7",
            "chip.RESET",
            "external.host",
        ] {
            let endpoint: Endpoint = name.parse().unwrap();
            assert_eq!(endpoint.to_string(), name);
        }
        assert_eq!(
            "nodot".parse::<Endpoint>(),
            Err(super::ParseEndpointError("nodot".to_owned()))
        );
        assert_eq!("logic".parse(), Ok(crate::signal::Kind::Logic));
        assert_eq!(crate::signal::Kind::Analog.to_string(), "analog");
        assert!("wire".parse::<crate::signal::Kind>().is_err());
        assert_eq!("in-out".parse(), Ok(crate::signal::Direction::InOut));
        assert_eq!(crate::signal::Direction::Out.to_string(), "out");
        assert_eq!(crate::time::Time(1_000).to_string(), "1000 ps");
    }

    #[test]
    fn endpoints_by_name() {
        assert_eq!(
            Endpoint::parse("fpga.pps"),
            Some(Endpoint::Peripheral {
                peripheral: "fpga".to_owned(),
                port: "pps".to_owned()
            })
        );
        assert_eq!(
            Endpoint::parse("chip.PGP0"),
            Some(Endpoint::Chip("PGP0".to_owned()))
        );
        assert_eq!(
            Endpoint::parse("external.host"),
            Some(Endpoint::External("host".to_owned()))
        );
        for bad in ["nodot", "a.", ".a"] {
            assert_eq!(Endpoint::parse(bad), None, "{bad}");
        }
    }
}
