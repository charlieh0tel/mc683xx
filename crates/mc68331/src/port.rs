//! The chip's general-purpose ports, by one key across the modules
//! that hold them: the SIM's ports C, E and F, the GPT's port GP and
//! the QSM's port QS.

use std::fmt;
use std::str::FromStr;

use mc683xx_cpu32::cycles::Cycle;

use mc683xx_modules::pins::PortLevels;
use mc683xx_modules::sim::ports::PortName;

use crate::gpt::PwmChange;

/// One of the MC68331's general-purpose ports, as the manual names
/// them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Port {
    /// Port C: the chip-select pins CS3 to CS9 assigned as discrete
    /// outputs, PC0 to PC6 (MC68331UM 4.8.1.4); outputs only.
    C,
    /// Port E: the SIM's bus control pins, PE0 to PE7 (MC68331UM 4.9).
    E,
    /// Port F: the SIM's interrupt request pins and MODCLK, PF0 to PF7
    /// (MC68331UM 4.9).
    F,
    /// Port GP: the GPT's capture and compare pins, PGP0 to PGP7
    /// (MC68331UM 7.6).
    Gp,
    /// Port QS: the QSM's pins, PQS0 to PQS7 (MC68331UM 6.2.2).
    Qs,
}

impl Port {
    /// All five, in the order the chip reports their changes at one
    /// time.
    pub const ALL: [Self; 5] = [Self::C, Self::E, Self::F, Self::Gp, Self::Qs];

    /// Its name, as the manual's pin names carry it: `C`, `E`, `F`,
    /// `GP` or `QS`.
    const fn name(self) -> &'static str {
        match self {
            Self::C => "C",
            Self::E => "E",
            Self::F => "F",
            Self::Gp => "GP",
            Self::Qs => "QS",
        }
    }

    /// Its index in [`Port::ALL`].
    pub(crate) const fn index(self) -> usize {
        self as usize
    }

    /// The port the SIM's key names.
    pub(crate) const fn from_sim(port: PortName) -> Self {
        match port {
            PortName::C => Self::C,
            PortName::E => Self::E,
            PortName::F => Self::F,
        }
    }
}

/// Its name, `C`, `E`, `F`, `GP` or `QS`.
impl fmt::Display for Port {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.name())
    }
}

/// The port the text names, as [`Port`]'s `Display` does, in any case.
impl FromStr for Port {
    type Err = ParsePortError;

    fn from_str(name: &str) -> Result<Self, ParsePortError> {
        Self::ALL
            .into_iter()
            .find(|candidate| candidate.name().eq_ignore_ascii_case(name))
            .ok_or_else(|| ParsePortError(name.to_owned()))
    }
}

/// Text that names no port.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("no port {0:?}")]
pub struct ParsePortError(String);

/// A port with no inputs for the board to drive: port C, outputs
/// only (MC68331UM 4.8.1.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("port {0} has no inputs")]
pub struct NoInputs(pub Port);

/// A change in what a port drives: from its time on, the pins it drives
/// and their levels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct PortChange {
    /// When.
    pub now: Cycle,
    /// The port.
    pub port: Port,
    /// What it drives.
    pub outputs: PortLevels,
}

/// A change in what the chip drives on a port or a PWM pin, as
/// [`Mc68331::take_output_changes`](crate::chip::Mc68331::take_output_changes)
/// gives them in the order they happened.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum OutputChange {
    /// A port's.
    Port(PortChange),
    /// A PWM pin's, while the host asks for its edges.
    Pwm(PwmChange),
}

impl OutputChange {
    /// When it happened.
    #[must_use]
    pub const fn now(&self) -> Cycle {
        match self {
            Self::Port(change) => change.now,
            Self::Pwm(change) => change.now,
        }
    }

    /// The port whose place it takes among the changes at one time: a
    /// PWM pin's port GP's, as the GPT drives both.
    pub(crate) const fn place(&self) -> Port {
        match self {
            Self::Port(change) => change.port,
            Self::Pwm(_) => Port::Gp,
        }
    }
}

/// What changed, without its time: `port E drives 0x0f at 0x05`,
/// `PWMA high`.
impl fmt::Display for OutputChange {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Port(change) => write!(
                formatter,
                "port {} drives {:#04x} at {:#04x}",
                change.port,
                change.outputs.driven(),
                change.outputs.levels()
            ),
            Self::Pwm(change) => {
                let level = if change.level { "high" } else { "low" };
                write!(formatter, "PWM{} {level}", change.channel)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use mc683xx_cpu32::cycles::Cycle;
    use mc683xx_modules::pins::PortLevels;

    use super::OutputChange;
    use super::ParsePortError;
    use super::Port;
    use super::PortChange;
    use crate::gpt::PwmChange;
    use crate::gpt::pwm::Channel;

    #[test]
    fn ports_print_as_their_names_and_parse_back_in_any_case() {
        for (index, port) in Port::ALL.into_iter().enumerate() {
            assert_eq!(port.to_string().parse(), Ok(port));
            assert_eq!(port.index(), index);
        }
        assert_eq!("qs".parse(), Ok(Port::Qs));
        assert_eq!(Port::Gp.to_string(), "GP");
        assert_eq!("?".parse::<Port>(), Err(ParsePortError("?".to_owned())));
    }

    #[test]
    fn an_output_change_prints_what_changed() {
        let port = OutputChange::Port(PortChange {
            now: Cycle::ZERO,
            port: Port::E,
            outputs: PortLevels::new(0x0f, 0x05),
        });
        assert_eq!(port.to_string(), "port E drives 0x0f at 0x05");
        let pwm = OutputChange::Pwm(PwmChange {
            now: Cycle::ZERO,
            channel: Channel::A,
            level: true,
        });
        assert_eq!(pwm.to_string(), "PWMA high");
    }
}
