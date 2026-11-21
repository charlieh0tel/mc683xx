//! The chip's pins as Python sees them: SCI frames, GPT pin names,
//! PWM channels and outputs, and the ports.

use mc683xx_mc68331::gpt::Input;
use mc683xx_mc68331::gpt::pwm;
use mc683xx_mc68331::gpt::pwm::Channel;
use mc683xx_mc68331::port::Port;
use mc683xx_modules::pins::PortState;
use mc683xx_modules::qsm::sci;
use pyo3::pyclass;
use pyo3::pymethods;

use crate::error::Error;

/// The most data bits a frame carries: nine, R8/T8 to R0/T0 (QSMRM
/// 5.2.4).
const FRAME_DATA_MASK: u16 = 0x1ff;

/// A frame on TXD or RXD.  A variant's value is a field named for it,
/// not Rust's `_0`.
#[pyclass(module = "mc683xx", frozen, eq, hash, from_py_object)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Frame {
    /// The data bits as on the line, LSB first, right-aligned: eight,
    /// or nine with M; the parity bit with PE.
    Data {
        /// The data bits.
        data: u16,
    },
    /// A frame of zeros, stop bit included (QSMRM 5.2.2, SBK).
    Break(),
}

impl TryFrom<Frame> for sci::Frame {
    type Error = Error;

    fn try_from(frame: Frame) -> Result<Self, Error> {
        match frame {
            Frame::Data { data } if data & !FRAME_DATA_MASK != 0 => Err(Error::Frame(data)),
            Frame::Data { data } => Ok(Self::Data(data)),
            Frame::Break() => Ok(Self::Break),
        }
    }
}

#[pymethods]
impl Frame {
    fn __repr__(&self) -> String {
        match self {
            Self::Data { data } => format!("Frame.Data({data:#x})"),
            Self::Break() => "Frame.Break()".to_owned(),
        }
    }
}

impl From<sci::Frame> for Frame {
    fn from(frame: sci::Frame) -> Self {
        match frame {
            sci::Frame::Data(data) => Self::Data { data },
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

/// One of the chip's general-purpose ports (Rust's `port::Port`).
#[pyclass(
    name = "Port",
    module = "mc683xx",
    eq,
    hash,
    eq_int,
    frozen,
    from_py_object
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum PortView {
    /// Port C: the chip-select pins CS3 to CS9 as discrete outputs.
    C,
    /// Port E.
    E,
    /// Port F.
    F,
    /// Port GP.
    #[pyo3(name = "GP")]
    Gp,
    /// Port QS.
    #[pyo3(name = "QS")]
    Qs,
}

impl From<PortView> for Port {
    fn from(port: PortView) -> Self {
        match port {
            PortView::C => Self::C,
            PortView::E => Self::E,
            PortView::F => Self::F,
            PortView::Gp => Self::Gp,
            PortView::Qs => Self::Qs,
        }
    }
}

impl From<Port> for PortView {
    fn from(port: Port) -> Self {
        match port {
            Port::C => Self::C,
            Port::E => Self::E,
            Port::F => Self::F,
            Port::Gp => Self::Gp,
            Port::Qs => Self::Qs,
        }
    }
}

/// A port's registers and the levels the board drives on its pins
/// (Rust's `pins::PortState`).
#[pyclass(name = "PortState", module = "mc683xx", frozen, eq, hash, get_all)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct PortStateView {
    /// The data latch.
    latch: u8,
    /// The data direction: a set bit makes the pin an output.
    direction: u8,
    /// The pin assignment: a set bit gives the pin its module function.
    assignment: u8,
    /// The levels the board drives on the pins.
    pins: u8,
}

#[pymethods]
impl PortStateView {
    #[new]
    const fn new(latch: u8, direction: u8, assignment: u8, pins: u8) -> Self {
        Self {
            latch,
            direction,
            assignment,
            pins,
        }
    }

    fn __repr__(&self) -> String {
        format!(
            "PortState(latch={:#04x}, direction={:#04x}, assignment={:#04x}, pins={:#04x})",
            self.latch, self.direction, self.assignment, self.pins
        )
    }
}

impl From<PortState> for PortStateView {
    fn from(state: PortState) -> Self {
        Self {
            latch: state.latch,
            direction: state.direction,
            assignment: state.assignment,
            pins: state.pins,
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
    Level {
        /// The level.
        level: bool,
    },
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
            pwm::Output::Level(level) => Self::Level { level },
            pwm::Output::Wave {
                period,
                high,
                clocks,
            } => Self::Wave {
                period,
                high,
                clocks: clocks.map(u64::from),
            },
            pwm::Output::Clock { clocks } => Self::Clock {
                clocks: clocks.map(u64::from),
            },
            _ => panic!("a PWM output the bindings do not know"),
        }
    }
}

#[cfg(test)]
mod tests {
    use mc683xx_mc68331::gpt::Input;
    use mc683xx_mc68331::gpt::PortGpPin;
    use mc683xx_mc68331::port::Port;

    use super::PortView;
    use super::input;
    use crate::error::Error;

    #[test]
    fn ports_convert_both_ways() {
        for port in Port::ALL {
            assert_eq!(Port::from(PortView::from(port)), port);
        }
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
