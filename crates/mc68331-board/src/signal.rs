//! What nets carry ([PLAN.md, Boards][plan-boards]): their types, the
//! directions of the ports on them, and the values driven.
//!
//! [plan-boards]: https://github.com/charlieh0tel/mc683xx/blob/main/PLAN.md#boards

use std::fmt;
use std::str::FromStr;

use mc683xx_modules::qsm::sci::Frame;
use serde::Deserialize;
use serde::Serialize;

use crate::time::Clock;
use crate::time::PICOSECONDS_PER_SECOND;
use crate::time::Time;
use mc683xx_cpu32::frequency::Frequency;
use mc683xx_cpu32::frequency::NANOHERTZ_PER_HERTZ;

/// A net's type, and a port's.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum Kind {
    /// A level whose state matters (INIT, DONE, a latch bit, a key):
    /// [`Signal::Level`].
    Logic,
    /// A level whose changes' times matter (1 PPS, an interpolator's
    /// start): [`Signal::Level`], each change a timed edge.  A chip pin
    /// takes either; a peripheral port takes its own kind only.
    Edge,
    /// A clock, its frequency and exact phase: [`Signal::Clock`].
    Clock,
    /// UART frames: [`Signal::Serial`].
    Serial,
    /// A value (a control voltage, a temperature): [`Signal::Analog`].
    Analog,
    /// Parallel lines as one value (an ADC's byte into an FPGA), so a
    /// word changes in one drive, not one per line: [`Signal::Bus`].
    Bus,
}

impl Kind {
    /// Every kind, in declaration order.
    const ALL: [Self; 6] = [
        Self::Logic,
        Self::Edge,
        Self::Clock,
        Self::Serial,
        Self::Analog,
        Self::Bus,
    ];

    /// Its name in a board file.
    const fn name(self) -> &'static str {
        match self {
            Self::Logic => "logic",
            Self::Edge => "edge",
            Self::Clock => "clock",
            Self::Serial => "serial",
            Self::Analog => "analog",
            Self::Bus => "bus",
        }
    }

    /// Whether a value of `signal` belongs on a net of this kind.
    #[must_use]
    pub const fn carries(self, signal: &Signal) -> bool {
        matches!(
            (self, signal),
            (Self::Logic | Self::Edge, Signal::Level(_))
                | (Self::Clock, Signal::Clock(_))
                | (Self::Serial, Signal::Serial(_))
                | (Self::Analog, Signal::Analog(_))
                | (Self::Bus, Signal::Bus(_))
        )
    }
}

/// A UART frame on a serial net, as the SCI's (QSMRM 5): its data bits
/// as on the line, or a break.
///
/// Its time on the net is its start bit's
/// leading edge, and it carries its bit rate and length, so a receiver
/// times its start or its end (`end`) as its own logic wants.  Built
/// with `new`, `timed` or `untimed`, so a field added later gets a
/// default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Serial {
    /// The frame.
    pub frame: Frame,
    /// The sender's bit rate; zero if it gave none (an untimed frame).
    pub baud: Frequency,
    /// Picoseconds from the start bit's leading edge to the end of the
    /// stop bits; zero for an untimed frame.
    pub length: u64,
}

impl Serial {
    /// `frame`, `bits` bits on the line, start and stop bits included
    /// (10 for eight data bits and one stop bit), at `baud`: its length
    /// rounded down to a picosecond.
    #[must_use]
    pub const fn new(frame: Frame, baud: Frequency, bits: u32) -> Self {
        let scale = PICOSECONDS_PER_SECOND as u128 * NANOHERTZ_PER_HERTZ as u128;
        let length = if baud.0 == 0 {
            0
        } else {
            bits as u128 * scale / baud.0 as u128
        };
        Self::timed(frame, baud, length as u64)
    }

    /// `frame` at `baud`, `length` picoseconds from start bit to the end
    /// of its stop bits (one and a half stop bits, say).
    #[must_use]
    pub const fn timed(frame: Frame, baud: Frequency, length: u64) -> Self {
        Self {
            frame,
            baud,
            length,
        }
    }

    /// `frame` with no timing: a host's character, which a receiver
    /// takes whole at its time.
    #[must_use]
    pub const fn untimed(frame: Frame) -> Self {
        Self::timed(frame, Frequency(0), 0)
    }

    /// When the frame's stop bits end, for a frame whose start bit
    /// begins at `start`.
    #[must_use]
    pub fn end(&self, start: Time) -> Time {
        start + self.length
    }
}

/// Its name in a board file: `logic`, `edge`, `clock`, `serial`,
/// `analog`, `bus`.
impl fmt::Display for Kind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.name())
    }
}

/// The kind a board file names so.
impl FromStr for Kind {
    type Err = ParseKindError;

    fn from_str(name: &str) -> Result<Self, ParseKindError> {
        Self::ALL
            .into_iter()
            .find(|kind| kind.name() == name)
            .ok_or_else(|| ParseKindError(name.to_owned()))
    }
}

/// Text that names no net type.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("no net type {0:?}")]
pub struct ParseKindError(pub String);

/// Which way a port faces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Direction {
    /// It is driven: a net's sink.
    In,
    /// It drives: a net's driver.
    Out,
    /// It drives and is driven: an open-drain pin (an FPGA's DONE) on
    /// a wired net, which reads the net's resolved level.
    InOut,
}

impl Direction {
    /// Every direction.
    const ALL: [Self; 3] = [Self::In, Self::Out, Self::InOut];

    /// Its name, as a board file's port is declared.
    const fn name(self) -> &'static str {
        match self {
            Self::In => "in",
            Self::Out => "out",
            Self::InOut => "in-out",
        }
    }

    /// Whether it drives.
    #[must_use]
    pub const fn drives(self) -> bool {
        matches!(self, Self::Out | Self::InOut)
    }

    /// Whether it is driven.
    #[must_use]
    pub const fn sinks(self) -> bool {
        matches!(self, Self::In | Self::InOut)
    }
}

/// Its name: `in`, `out`, `in-out`.
impl fmt::Display for Direction {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.name())
    }
}

/// The direction named so.
impl FromStr for Direction {
    type Err = ParseDirectionError;

    fn from_str(name: &str) -> Result<Self, ParseDirectionError> {
        Self::ALL
            .into_iter()
            .find(|direction| direction.name() == name)
            .ok_or_else(|| ParseDirectionError(name.to_owned()))
    }
}

/// Text that names no direction.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("no direction {0:?}")]
pub struct ParseDirectionError(pub String);

/// The value as the trace writes it, `key=value` fields
/// ([`docs/tracing.md`][tracing]): `level=1`; `clock_at=PS frequency=NHZ
/// phase=UNITS`; `data=0x41` or `data=break`, `baud=NHZ length=PS`;
/// `analog=1.5`; `bus=0x0000abcd`.
///
/// [tracing]: https://github.com/charlieh0tel/mc683xx/blob/main/docs/tracing.md
impl fmt::Display for Signal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Level(level) => write!(formatter, "level={}", u8::from(*level)),
            Self::Clock(clock) => write!(
                formatter,
                "clock_at={} frequency={} phase={}",
                clock.at.0, clock.frequency.0, clock.phase.0
            ),
            Self::Serial(serial) => {
                match serial.frame {
                    Frame::Data(data) => write!(formatter, "data={data:#04x}")?,
                    Frame::Break => formatter.write_str("data=break")?,
                }
                write!(
                    formatter,
                    " baud={} length={}",
                    serial.baud.0, serial.length
                )
            }
            Self::Analog(value) => write!(formatter, "analog={value}"),
            Self::Bus(word) => write!(formatter, "bus={word:#010x}"),
        }
    }
}

/// A value on a net from a time on.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum Signal {
    /// A logic level, electrical: `true` high.
    Level(bool),
    /// A clock from its `at` on.
    Clock(Clock),
    /// A UART frame, timed from its start bit ([`Serial`]).
    Serial(Serial),
    /// A value, in the units the two ends agree on.  Data only: never
    /// used to schedule.
    Analog(f64),
    /// Parallel lines' levels, the first line in bit 0.
    Bus(u32),
}
