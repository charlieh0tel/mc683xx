//! What a board's run traces: structured records, and their text.
//!
//! The text is one `CHIP: KIND key=value ...` line each ([`Line`]), the
//! chip's [`Chip::TYPE`] its prefix: a versioned text contract, the
//! same run giving the same bytes ([`docs/tracing.md`][tracing]).
//!
//! [tracing]: https://github.com/charlieh0tel/mc683xx/blob/main/docs/tracing.md

use std::fmt;

use mc683xx_cpu32::cpu::Stop;
use mc683xx_cpu32::cycles::Cycles;

#[cfg(doc)]
use crate::chip::Chip;
use crate::signal::Signal;
use crate::time::Time;

/// The text's version, which [`header`] gives.
pub const VERSION: u32 = 3;
/// Longs in a code record's stack.
pub const STACK_LONGS: usize = 8;

/// A trace's first line, for a run of `chip` (its [`Chip::TYPE`]): the
/// format, its version and the chip, `# mc683xx trace 3 chip=mc68331`.
#[must_use]
pub fn header(chip: &str) -> String {
    format!("# mc683xx trace {VERSION} chip={chip}")
}

/// A record's line: `chip` (its [`Chip::TYPE`]), `: `, and the record's
/// `Display`, `KIND key=value ...` ([`docs/tracing.md`][tracing]).
///
/// [tracing]: https://github.com/charlieh0tel/mc683xx/blob/main/docs/tracing.md
#[derive(Debug, Clone, Copy)]
pub struct Line<'a, R: ?Sized> {
    /// The chip's name.
    chip: &'a str,
    /// The record.
    record: &'a R,
}

impl<'a, R: fmt::Display + ?Sized> Line<'a, R> {
    /// `record`'s line in a trace of `chip`.
    #[must_use]
    pub const fn new(chip: &'a str, record: &'a R) -> Self {
        Self { chip, record }
    }
}

impl<R: fmt::Display + ?Sized> fmt::Display for Line<'_, R> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.chip, self.record)
    }
}

/// A value a net carried, on board time: the trace's `net` lines,
/// apart from the records, which are in cycle order.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct NetRecord {
    /// When.
    pub at: Time,
    /// The net's name.
    pub net: String,
    /// The value.
    pub signal: Signal,
}

/// `net time=... name=...` and the signal's fields, after a [`Line`]'s
/// prefix
/// ([`docs/tracing.md`][tracing]).
///
/// [tracing]: https://github.com/charlieh0tel/mc683xx/blob/main/docs/tracing.md
impl fmt::Display for NetRecord {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "net time={} name={} {}",
            self.at.0, self.net, self.signal
        )
    }
}

/// A bus cycle board peripherals answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct BusCycleRecord {
    /// When its instruction started.
    pub cycle: Cycles,
    /// That instruction's address.
    pub pc: u32,
    /// Whether it wrote.
    pub write: bool,
    /// The address on ADDR.
    pub address: u32,
    /// Bytes: 1 or 2.
    pub size: u32,
    /// The value read or written; [`None`] for a read that ended in BERR.
    pub value: Option<u32>,
}

/// An exception the CPU entered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct ExceptionRecord {
    /// When its frame was stacked.
    pub cycle: Cycles,
    /// Its vector number.
    pub vector: u8,
    /// The PC its frame stacked.
    pub pc: u32,
    /// The SR its frame stacked, if the core could read it.
    pub sr: Option<u16>,
    /// An interrupt's level.
    pub level: Option<u8>,
}

/// A chip pin on a net changed level.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct PinRecord {
    /// The system clock it takes effect at.
    pub cycle: Cycles,
    /// Its board time.
    pub time: Time,
    /// The pin, by its name after `chip.`.
    pub pin: &'static str,
    /// Its level.
    pub level: bool,
}

/// A QSPI transfer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct QspiRecord {
    /// When it started.
    pub cycle: Cycles,
    /// PCS\[3:0\]'s levels.
    pub pcs: u8,
    /// Bits shifted.
    pub bits: u8,
    /// Shifted out.
    pub out: u16,
    /// Shifted in.
    pub input: u16,
    /// CPOL: SCK idles high (QSMRM 4.3.1).
    pub cpol: bool,
    /// CPHA: data is changed on SCK's leading edge (QSMRM 4.3.1).
    pub cpha: bool,
}

/// A watched instruction about to run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct CodeRecord {
    /// Now.
    pub cycle: Cycles,
    /// Its address.
    pub pc: u32,
    /// SR.
    pub sr: u16,
    /// D0 to D7.
    pub data: [u32; 8],
    /// A0 to A7, the active stack pointer as A7.
    pub address: [u32; 8],
    /// The longs from A7 up, where the core can read them.
    pub stack: [Option<u32>; STACK_LONGS],
}

/// Where the CPU is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct PositionRecord {
    /// Now.
    pub cycle: Cycles,
    /// PC.
    pub pc: u32,
    /// SR.
    pub sr: u16,
    /// VBR.
    pub vbr: u32,
    /// A7.
    pub a7: u32,
    /// Instructions run since power-on.
    pub instructions: u64,
}

/// The run's end.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct StoppedRecord {
    /// Now.
    pub cycle: Cycles,
    /// Instructions run since power-on.
    pub instructions: u64,
    /// Why.
    pub reason: Stop,
}

/// One record of a trace.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Record {
    /// `bus-cycle`.
    BusCycle(BusCycleRecord),
    /// `exception`.
    Exception(ExceptionRecord),
    /// `pin`.
    Pin(PinRecord),
    /// `qspi`.
    Qspi(QspiRecord),
    /// `code`.
    Code(CodeRecord),
    /// `position`.
    Position(PositionRecord),
    /// `stopped`.
    Stopped(StoppedRecord),
}

impl Record {
    /// Its cycle.
    #[must_use]
    pub const fn cycle(&self) -> Cycles {
        match self {
            Self::BusCycle(record) => record.cycle,
            Self::Exception(record) => record.cycle,
            Self::Pin(record) => record.cycle,
            Self::Qspi(record) => record.cycle,
            Self::Code(record) => record.cycle,
            Self::Position(record) => record.cycle,
            Self::Stopped(record) => record.cycle,
        }
    }
}

/// Longs as hexadecimal, eight digits, comma-separated; `--------`
/// for one not read.
fn longs(values: impl Iterator<Item = Option<u32>>) -> String {
    values
        .map(|value| value.map_or_else(|| "--------".to_owned(), |value| format!("{value:08x}")))
        .collect::<Vec<_>>()
        .join(",")
}

/// A stop's reason as `stopped` names it.
const fn reason(stop: Stop) -> &'static str {
    match stop {
        Stop::Budget | Stop::Stopped => "budget",
        Stop::Halted(_) => "halted",
        Stop::Hook(_) => "hook",
        Stop::Requested => "requested",
        _ => "other",
    }
}

/// `KIND key=value ...`, after a [`Line`]'s prefix
/// ([`docs/tracing.md`][tracing]).
///
/// [tracing]: https://github.com/charlieh0tel/mc683xx/blob/main/docs/tracing.md
impl fmt::Display for Record {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BusCycle(record) => {
                write!(
                    formatter,
                    "bus-cycle cycle={} pc={:#010x} access={} address={:#010x} size={} value=",
                    record.cycle.0,
                    record.pc,
                    if record.write { "write" } else { "read" },
                    record.address,
                    record.size
                )?;
                match record.value {
                    Some(value) => {
                        write!(
                            formatter,
                            "{value:#0width$x}",
                            width = 2 + 2 * record.size as usize
                        )
                    }
                    None => formatter.write_str("berr"),
                }
            }
            Self::Exception(record) => {
                write!(
                    formatter,
                    "exception cycle={} vector={} pc={:#010x} sr=",
                    record.cycle.0, record.vector, record.pc
                )?;
                match record.sr {
                    Some(sr) => write!(formatter, "{sr:#06x}")?,
                    None => formatter.write_str("------")?,
                }
                if let Some(level) = record.level {
                    write!(formatter, " level={level}")?;
                }
                Ok(())
            }
            Self::Pin(record) => write!(
                formatter,
                "pin cycle={} time={} pin={} level={}",
                record.cycle.0,
                record.time.0,
                record.pin,
                u8::from(record.level)
            ),
            Self::Qspi(record) => write!(
                formatter,
                "qspi cycle={} pcs={:#x} bits={} out={:#06x} in={:#06x} cpol={} cpha={}",
                record.cycle.0,
                record.pcs,
                record.bits,
                record.out,
                record.input,
                u8::from(record.cpol),
                u8::from(record.cpha)
            ),
            Self::Code(record) => write!(
                formatter,
                "code cycle={} pc={:#010x} sr={:#06x} d={} a={} stack={}",
                record.cycle.0,
                record.pc,
                record.sr,
                longs(record.data.iter().copied().map(Some)),
                longs(record.address.iter().copied().map(Some)),
                longs(record.stack.iter().copied())
            ),
            Self::Position(record) => write!(
                formatter,
                "position cycle={} pc={:#010x} sr={:#06x} vbr={:#010x} a7={:#010x} instructions={}",
                record.cycle.0, record.pc, record.sr, record.vbr, record.a7, record.instructions
            ),
            Self::Stopped(record) => write!(
                formatter,
                "stopped cycle={} instructions={} reason={}",
                record.cycle.0,
                record.instructions,
                reason(record.reason)
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use mc683xx_cpu32::cpu::Stop;
    use mc683xx_cpu32::cycles::Cycles;

    use mc683xx_cpu32::frequency::Frequency;
    use mc683xx_modules::qsm::sci::Frame;

    use super::BusCycleRecord;
    use super::CodeRecord;
    use super::ExceptionRecord;
    use super::Line;
    use super::NetRecord;
    use super::PinRecord;
    use super::PositionRecord;
    use super::QspiRecord;
    use super::Record;
    use super::StoppedRecord;
    use super::header;
    use crate::signal::Serial;
    use crate::signal::Signal;
    use crate::time::Clock;
    use crate::time::Time;

    #[test]
    fn records_print_as_docs_tracing_shows() {
        let peripheral = Record::BusCycle(BusCycleRecord {
            cycle: Cycles(42),
            pc: 0x1_5f88,
            write: false,
            address: 0x20_0003,
            size: 1,
            value: Some(4),
        });
        assert_eq!(
            peripheral.to_string(),
            "bus-cycle cycle=42 pc=0x00015f88 access=read address=0x00200003 size=1 value=0x04"
        );
        let word = Record::BusCycle(BusCycleRecord {
            cycle: Cycles(42),
            pc: 0x1_5f88,
            write: true,
            address: 0x20_0003,
            size: 2,
            value: Some(0x542),
        });
        assert!(
            word.to_string()
                .ends_with("access=write address=0x00200003 size=2 value=0x0542")
        );
        let berr = Record::BusCycle(BusCycleRecord {
            value: None,
            write: false,
            ..BusCycleRecord {
                cycle: Cycles(1),
                pc: 0,
                write: false,
                address: 0,
                size: 2,
                value: None,
            }
        });
        assert!(berr.to_string().ends_with("value=berr"));
        let exception = Record::Exception(ExceptionRecord {
            cycle: Cycles(7),
            vector: 66,
            pc: 0x100,
            sr: Some(0x2000),
            level: Some(5),
        });
        assert_eq!(
            exception.to_string(),
            "exception cycle=7 vector=66 pc=0x00000100 sr=0x2000 level=5"
        );
        let pin = Record::Pin(PinRecord {
            cycle: Cycles(1677),
            time: Time(100_000_037),
            pin: "PGP0",
            level: true,
        });
        assert_eq!(
            pin.to_string(),
            "pin cycle=1677 time=100000037 pin=PGP0 level=1"
        );
        let qspi = Record::Qspi(QspiRecord {
            cycle: Cycles(90_210),
            pcs: 7,
            bits: 16,
            out: 0x8000,
            input: 0xffff,
            cpol: true,
            cpha: true,
        });
        assert_eq!(
            qspi.to_string(),
            "qspi cycle=90210 pcs=0x7 bits=16 out=0x8000 in=0xffff cpol=1 cpha=1"
        );
        let code = Record::Code(CodeRecord {
            cycle: Cycles(5),
            pc: 0x1234,
            sr: 0x2700,
            data: [0; 8],
            address: [0, 0, 0, 0, 0, 0, 0, 0x10_fffc],
            stack: [Some(1), None, None, None, None, None, None, None],
        });
        assert!(code.to_string().ends_with(
            "a=00000000,00000000,00000000,00000000,00000000,00000000,00000000,0010fffc \
             stack=00000001,--------,--------,--------,--------,--------,--------,--------"
        ));
        let position = Record::Position(PositionRecord {
            cycle: Cycles(1),
            pc: 2,
            sr: 0x2700,
            vbr: 0,
            a7: 0x10_fffc,
            instructions: 9,
        });
        assert_eq!(
            position.to_string(),
            "position cycle=1 pc=0x00000002 sr=0x2700 vbr=0x00000000 a7=0x0010fffc instructions=9"
        );
        let stopped = Record::Stopped(StoppedRecord {
            cycle: Cycles(3),
            instructions: 1,
            reason: Stop::Budget,
        });
        assert_eq!(
            stopped.to_string(),
            "stopped cycle=3 instructions=1 reason=budget"
        );
    }

    #[test]
    fn net_lines_carry_each_signal_s_fields() {
        let line = |signal| {
            NetRecord {
                at: Time(5),
                net: "pps".to_owned(),
                signal,
            }
            .to_string()
        };
        assert_eq!(line(Signal::Level(true)), "net time=5 name=pps level=1");
        assert_eq!(
            line(Signal::Clock(Clock::new(Frequency::from_hz(10), Time(0)))),
            "net time=5 name=pps clock_at=0 frequency=10000000000 phase=0"
        );
        assert_eq!(
            line(Signal::Serial(Serial::untimed(Frame::Data(0x41)))),
            "net time=5 name=pps data=0x41 baud=0 length=0"
        );
        assert_eq!(
            line(Signal::Serial(Serial::untimed(Frame::Break))),
            "net time=5 name=pps data=break baud=0 length=0"
        );
        assert_eq!(line(Signal::Analog(1.5)), "net time=5 name=pps analog=1.5");
        assert_eq!(
            line(Signal::Bus(0xabcd)),
            "net time=5 name=pps bus=0x0000abcd"
        );
    }

    #[test]
    fn a_line_names_its_chip() {
        let stopped = Record::Stopped(StoppedRecord {
            cycle: Cycles(3),
            instructions: 1,
            reason: Stop::Budget,
        });
        assert_eq!(
            Line::new("mc68331", &stopped).to_string(),
            "mc68331: stopped cycle=3 instructions=1 reason=budget"
        );
        assert_eq!(header("mc68331"), "# mc683xx trace 3 chip=mc68331");
    }
}
