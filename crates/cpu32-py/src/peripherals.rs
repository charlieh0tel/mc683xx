//! The chip's pins as Python sees them: SCI frames, GPT pin names,
//! PWM channels and outputs, and the SIM's ports.

use mc683xx_mc68331::gpt::Input;
use mc683xx_mc68331::gpt::OutputChange;
use mc683xx_mc68331::gpt::pwm;
use mc683xx_mc68331::gpt::pwm::Channel;
use mc683xx_modules::qsm::sci;
use mc683xx_modules::sim::ports::PortName;
use pyo3::pyclass;

use crate::error::Error;

/// The most data bits a frame carries: nine, R8/T8 to R0/T0 (QSMRM
/// 5.2.4).
const FRAME_DATA_MASK: u16 = 0x1ff;

/// A frame on TXD or RXD.
#[pyclass(module = "mc683xx", frozen, eq, hash, from_py_object)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Frame {
    /// The data bits as on the line, LSB first, right-aligned: eight,
    /// or nine with M; the parity bit with PE.
    Data(u16),
    /// A frame of zeros, stop bit included (QSMRM 5.2.2, SBK).
    Break(),
}

impl TryFrom<Frame> for sci::Frame {
    type Error = Error;

    fn try_from(frame: Frame) -> Result<Self, Error> {
        match frame {
            Frame::Data(data) if data & !FRAME_DATA_MASK != 0 => Err(Error::Frame(data)),
            Frame::Data(data) => Ok(Self::Data(data)),
            Frame::Break() => Ok(Self::Break),
        }
    }
}

impl From<sci::Frame> for Frame {
    fn from(frame: sci::Frame) -> Self {
        match frame {
            sci::Frame::Data(data) => Self::Data(data),
            sci::Frame::Break => Self::Break(),
        }
    }
}

/// One of the GPT's two PWM outputs (Rust's `pwm::Channel`).
#[pyclass(
    name = "Channel",
    module = "mc683xx",
    eq,
    hash,
    eq_int,
    frozen,
    from_py_object
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum ChannelView {
    /// PWMA.
    A,
    /// PWMB.
    B,
}

impl From<ChannelView> for Channel {
    fn from(channel: ChannelView) -> Self {
        match channel {
            ChannelView::A => Self::A,
            ChannelView::B => Self::B,
        }
    }
}

impl From<Channel> for ChannelView {
    fn from(channel: Channel) -> Self {
        match channel {
            Channel::A => Self::A,
            Channel::B => Self::B,
        }
    }
}

/// One of the SIM's general-purpose ports (Rust's `ports::PortName`).
#[pyclass(
    name = "PortName",
    module = "mc683xx",
    eq,
    hash,
    eq_int,
    frozen,
    from_py_object
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum PortNameView {
    /// Port C: the chip-select pins CS3 to CS9 as discrete outputs.
    C,
    /// Port E.
    E,
    /// Port F.
    F,
}

impl From<PortNameView> for PortName {
    fn from(port: PortNameView) -> Self {
        match port {
            PortNameView::C => Self::C,
            PortNameView::E => Self::E,
            PortNameView::F => Self::F,
        }
    }
}

impl From<PortName> for PortNameView {
    fn from(port: PortName) -> Self {
        match port {
            PortName::C => Self::C,
            PortName::E => Self::E,
            PortName::F => Self::F,
        }
    }
}

/// A change in what the GPT drives (Rust's `gpt::OutputChange`), its
/// `PortLevels` as `driven` and `levels`.
#[pyclass(name = "OutputChange", module = "mc683xx", frozen, eq, hash)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum OutputChangeView {
    /// Port GP's pins and their levels, as two masks.
    PortGp {
        /// Its output pins.
        driven: u8,
        /// Their levels.
        levels: u8,
    },
    /// A PWM pin's level, while its edges are reported.
    Pwm {
        /// Which.
        channel: ChannelView,
        /// Its level.
        level: bool,
    },
}

impl From<OutputChange> for OutputChangeView {
    fn from(change: OutputChange) -> Self {
        match change {
            OutputChange::PortGp(outputs) => Self::PortGp {
                driven: outputs.driven,
                levels: outputs.levels,
            },
            OutputChange::Pwm { channel, level } => Self::Pwm {
                channel: channel.into(),
                level,
            },
            _ => panic!("a GPT output change the bindings do not know"),
        }
    }
}

/// GPT input pin `name`: `PGP0` to `PGP7`, `PAI` or `PCLK`.
pub(crate) fn input(name: &str) -> Result<Input, Error> {
    name.parse().map_err(|_| Error::Input(name.to_owned()))
}

/// What a PWM pin or OC1's carries.
#[pyclass(module = "mc683xx", frozen, eq, hash)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Output {
    /// A steady level.
    Level(bool),
    /// High for `high` of every `period` PWMCNT counts, from the
    /// period's start; `clocks` system clocks a count, or `None` when
    /// PCLK clocks PWMCNT.
    Wave {
        /// Counts in a period.
        period: u32,
        /// Counts high.
        high: u32,
        /// System clocks a count.
        clocks: Option<u64>,
    },
    /// A counter's clock: PWMCNT's on PWMA (PPROUT), TCNT's on OC1
    /// (CPROUT).
    Clock {
        /// System clocks a count.
        clocks: Option<u64>,
    },
}

impl From<pwm::Output> for Output {
    fn from(output: pwm::Output) -> Self {
        match output {
            pwm::Output::Level(level) => Self::Level(level),
            pwm::Output::Wave {
                period,
                high,
                clocks,
            } => Self::Wave {
                period,
                high,
                clocks,
            },
            pwm::Output::Clock { clocks } => Self::Clock { clocks },
            _ => panic!("a PWM output the bindings do not know"),
        }
    }
}

#[cfg(test)]
mod tests {
    use mc683xx_mc68331::gpt::Input;
    use mc683xx_mc68331::gpt::OutputChange;
    use mc683xx_mc68331::gpt::PortGpPin;
    use mc683xx_mc68331::gpt::pwm::Channel;
    use mc683xx_modules::pins::PortLevels;

    use super::ChannelView;
    use super::OutputChangeView;
    use super::input;
    use crate::error::Error;

    #[test]
    fn gpt_changes_keep_their_port_and_channel() {
        let port = OutputChange::PortGp(PortLevels {
            driven: 0x30,
            levels: 0x10,
        });
        assert_eq!(
            OutputChangeView::from(port),
            OutputChangeView::PortGp {
                driven: 0x30,
                levels: 0x10
            }
        );
        let pwm = OutputChange::Pwm {
            channel: Channel::B,
            level: true,
        };
        assert_eq!(
            OutputChangeView::from(pwm),
            OutputChangeView::Pwm {
                channel: ChannelView::B,
                level: true
            }
        );
    }

    #[test]
    fn input_pin_names() {
        let pgp7 = Input::PortGp(PortGpPin::new(7).unwrap());
        assert_eq!(input("PGP7").unwrap(), pgp7);
        assert_eq!(input("PAI").unwrap(), Input::Pai);
        assert_eq!(input("PCLK").unwrap(), Input::Pclk);
        for name in ["PGP8", "PGP07", "PGP", "PWMA"] {
            assert_eq!(
                input(name).unwrap_err().to_string(),
                Error::Input(name.to_owned()).to_string()
            );
        }
    }
}
