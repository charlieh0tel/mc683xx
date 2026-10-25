//! The SIM's general-purpose I/O (SIMRM 9; MC68331UM 4.9): ports E and
//! F, and port C's discrete outputs on chip-select pins.

use std::fmt;
use std::str::FromStr;

use crate::pins::PortLevels;

/// One of ports E and F: its data latch, data direction and pin
/// assignment registers, and the levels the board drives on its pins.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Port {
    /// The data latch, `PORTE` or `PORTF`.
    pub latch: u8,
    /// The data direction register, `DDRE` or `DDRF`: a set bit makes the
    /// pin an output.
    pub direction: u8,
    /// The pin assignment register, `PEPAR` or `PFPAR`: a set bit gives
    /// the pin its bus or interrupt function.
    pub assignment: u8,
    /// The levels the board drives on its input pins.
    pub pins: u8,
}

impl Port {
    /// The port out of reset with `assignment` its pin assignment;
    /// the latch is undefined at power-up and unaffected by reset
    /// (SIMRM Table 8-4), so it starts at zero; the board's pins start
    /// high.
    pub(crate) const fn new(assignment: u8) -> Self {
        Self {
            latch: 0,
            direction: 0,
            assignment,
            pins: u8::MAX,
        }
    }

    /// The port after a reset: the latch kept, the pins inputs.
    pub(crate) const fn reset(self, assignment: u8) -> Self {
        Self {
            latch: self.latch,
            ..Self::new(assignment)
        }
        .with_pins(self.pins)
    }

    /// The port with the board driving `pins`.
    const fn with_pins(self, pins: u8) -> Self {
        Self { pins, ..self }
    }

    /// The data register as read: the latch on outputs, the pin on
    /// inputs (MC68331UM 4.9.3).
    #[must_use]
    pub const fn data(&self) -> u8 {
        self.latch & self.direction | self.pins & !self.direction
    }

    /// The levels the port drives on its output pins.
    #[must_use]
    pub const fn outputs(&self) -> PortLevels {
        let driven = self.direction & !self.assignment;
        PortLevels {
            driven,
            levels: self.latch & driven,
        }
    }
}

/// One of the SIM's general-purpose ports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum PortName {
    /// Port C: the chip-select pins CS3 to CS9 assigned as discrete
    /// outputs, PC0 to PC6 (MC68331UM 4.8.1.4).
    C,
    /// Port E (MC68331UM 4.9).
    E,
    /// Port F (MC68331UM 4.9).
    F,
}

impl PortName {
    /// All three, in the order the SIM reports their changes at one
    /// time.
    pub const ALL: [Self; 3] = [Self::C, Self::E, Self::F];

    /// Its name, `C`, `E` or `F`.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::C => "C",
            Self::E => "E",
            Self::F => "F",
        }
    }
}

impl fmt::Display for PortName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.name())
    }
}

/// The SIM port [`PortName::name`] names, in any case.
impl FromStr for PortName {
    type Err = ParsePortNameError;

    fn from_str(name: &str) -> Result<Self, ParsePortNameError> {
        Self::ALL
            .into_iter()
            .find(|candidate| candidate.name().eq_ignore_ascii_case(name))
            .ok_or_else(|| ParsePortNameError(name.to_owned()))
    }
}

/// Text that names no SIM port.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("no SIM port {0:?}")]
pub struct ParsePortNameError(pub String);

/// A change in what a port drives: from its time on, the pins it
/// drives and their levels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Change {
    /// The port.
    pub port: PortName,
    /// What it drives.
    pub outputs: PortLevels,
}

/// PORTC's implemented bits, PC6 to PC0 (MC68331UM D.3.22).
pub(crate) const PORTC_IMPLEMENTED: u8 = 0x7f;
/// PORTC out of reset (MC68331UM D.3.22).
pub(crate) const PORTC_RESET: u8 = 0x7f;

#[cfg(test)]
mod tests {
    use crate::pins::PortLevels;

    use super::ParsePortNameError;
    use super::Port;
    use super::PortName;

    #[test]
    fn inputs_read_the_pins_and_outputs_the_latch() {
        let mut port = Port::new(0);
        port.latch = 0x0f;
        port.direction = 0x03;
        port.pins = 0xa0;
        assert_eq!(port.data(), 0xa3);
        assert_eq!(
            port.outputs(),
            PortLevels {
                driven: 0x03,
                levels: 0x03
            }
        );
        let port = port.reset(0xff);
        assert_eq!(port.latch, 0x0f);
        assert_eq!(port.direction, 0);
        assert_eq!(port.pins, 0xa0);
    }

    #[test]
    fn port_names_print_as_their_letters_and_parse_back_in_any_case() {
        for port in PortName::ALL {
            assert_eq!(port.to_string().parse(), Ok(port));
        }
        assert_eq!("f".parse(), Ok(PortName::F));
        assert_eq!(
            "?".parse::<PortName>(),
            Err(ParsePortNameError("?".to_owned()))
        );
    }
}
