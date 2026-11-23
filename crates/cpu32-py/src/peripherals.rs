//! The chip's pins as Python sees them: SCI frames, GPT pin names,
//! PWM channels and outputs, and the ports.

use mc683xx_cpu32::cycles::Cycles;
use mc683xx_mc68331::gpt::Input;
use mc683xx_mc68331::gpt::PortGpPin;
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

/// A GPT input pin (Rust's `gpt::Input`), a member per pin: Rust's
/// `PortGp(pin)` is `PGP0` to `PGP7` here, as the pins are named.
#[pyclass(
    name = "Input",
    module = "mc683xx",
    eq,
    hash,
    eq_int,
    frozen,
    from_py_object
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[expect(
    clippy::upper_case_acronyms,
    reason = "the members are the pins' names, as Python shows them"
)]
pub(crate) enum InputView {
    /// PGP0, IC1.
    PGP0,
    /// PGP1, IC2.
    PGP1,
    /// PGP2, IC3.
    PGP2,
    /// PGP3, OC1.
    PGP3,
    /// PGP4, OC2.
    PGP4,
    /// PGP5, OC3.
    PGP5,
    /// PGP6, OC4.
    PGP6,
    /// PGP7, IC4/OC5.
    PGP7,
    /// PAI, the pulse accumulator's input.
    PAI,
    /// PCLK, the external clock.
    PCLK,
}

impl From<InputView> for Input {
    fn from(input: InputView) -> Self {
        let port_gp = |bit| Self::PortGp(PortGpPin::try_from(bit).expect("a port GP pin"));
        match input {
            InputView::PGP0 => port_gp(0),
            InputView::PGP1 => port_gp(1),
            InputView::PGP2 => port_gp(2),
            InputView::PGP3 => port_gp(3),
            InputView::PGP4 => port_gp(4),
            InputView::PGP5 => port_gp(5),
            InputView::PGP6 => port_gp(6),
            InputView::PGP7 => port_gp(7),
            InputView::PAI => Self::Pai,
            InputView::PCLK => Self::Pclk,
        }
    }
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
    /// High for `high_counts` of every `period_counts` PWMCNT counts,
    /// from the period's start; `clocks_per_count` system clocks a
    /// count, or `None` when PCLK clocks PWMCNT.
    Wave {
        /// Counts in a period.
        period_counts: u32,
        /// Counts high.
        high_counts: u32,
        /// System clocks a count.
        clocks_per_count: Option<u64>,
    },
    /// A counter's clock: PWMCNT's on PWMA (PPROUT), TCNT's on OC1
    /// (CPROUT).
    Clock {
        /// System clocks a count.
        clocks: Option<u64>,
    },
}

impl TryFrom<pwm::Output> for Output {
    type Error = Error;

    /// The output, or [`Error::Unmapped`] for one the bindings do not
    /// know.
    fn try_from(output: pwm::Output) -> Result<Self, Error> {
        Ok(match output {
            pwm::Output::Level(level) => Self::Level { level },
            pwm::Output::Wave {
                period_counts,
                high_counts,
                clocks_per_count,
            } => Self::Wave {
                period_counts,
                high_counts,
                clocks_per_count: clocks_per_count.map(Cycles::clocks),
            },
            pwm::Output::Clock { clocks } => Self::Clock {
                clocks: clocks.map(Cycles::clocks),
            },
            _ => return Err(Error::unmapped("PWM output")),
        })
    }
}

#[cfg(test)]
mod tests {
    use mc683xx_mc68331::gpt::Input;
    use mc683xx_mc68331::gpt::PortGpPin;
    use mc683xx_mc68331::port::Port;

    use super::InputView;
    use super::PortView;

    #[test]
    fn ports_convert_both_ways() {
        for port in Port::ALL {
            assert_eq!(Port::from(PortView::from(port)), port);
        }
    }

    #[test]
    fn inputs_are_named_as_their_pins() {
        for (view, name) in [
            (InputView::PGP0, "PGP0"),
            (InputView::PGP7, "PGP7"),
            (InputView::PAI, "PAI"),
            (InputView::PCLK, "PCLK"),
        ] {
            assert_eq!(Input::from(view).to_string(), name);
        }
        assert_eq!(
            Input::from(InputView::PGP7),
            Input::PortGp(PortGpPin::try_from(7).unwrap())
        );
    }
}
