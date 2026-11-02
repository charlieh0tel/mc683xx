//! Init scripts: the writes and checks that ready a board for a load,
//! one command a line, run on a halted target.
//!
//! ```
//! use mc683xx_debug::script::Script;
//!
//! let script: Script = r"
//! ## Chip selects for RAM at 0x200000 (SIMRM 7), then check them.
//! write_memory.w 0xfffa44 0x3fff       # CSPAR0
//! write_memory.w 0xfffa4c 0x2003       # CSBAR0: 64 Kbytes
//! read_memory.w 0xfffa44 expect 0x3fff
//! write_register a7 0x20fffc
//! write_system_register vbr 0x200000
//! poll_memory.b 0x300001 mask 0x80 expect 0x80 timeout_ms 100 fc 5
//! "
//! .parse()?;
//! assert_eq!(script.lines()[0].number, 3);
//! # Ok::<(), mc683xx_debug::script::ParseScriptError>(())
//! ```
//!
//! - `write_memory.SIZE ADDRESS VALUE [fc N]`
//! - `write_register REGISTER VALUE`: `d0`-`d7`, `a0`-`a7`, in any case.
//! - `write_system_register REGISTER VALUE`: `rpc`, `pcc`, `atemp`,
//!   `far`, `vbr`, `sr`, `usp`, `ssp`, `sfc`, `dfc`, in any case.
//! - `read_memory.SIZE ADDRESS expect VALUE [mask M] [fc N]`: fails
//!   unless the value read, masked with M (every bit by default), is
//!   VALUE.
//! - `poll_memory.SIZE ADDRESS mask M expect VALUE timeout_ms N [fc N]`:
//!   reads until the value read, masked with M, is VALUE, and fails if
//!   it is not by N milliseconds of host time after the first read,
//!   which is always made.
//!
//! SIZE is `b`, `w` or `l`.  The clauses after the address (`expect`,
//! `mask`, `timeout_ms`, `fc`) come in any order, each once.  Numbers
//! are hex with `0x` or decimal; a value, a mask and an expected value
//! fit the size; a word or long address is even (CPU32RM 6.2.3); `fc`,
//! the function code, is 0 to 7 and supervisor data (5) by default.
//! `#` starts a comment; blank lines are skipped.
//!
//! `poll_memory` is the only use of host time in the workspace outside
//! a runner's pacing: a peripheral on silicon settles in real time,
//! and the emulated chip, halted, does not move at all, so a poll on
//! it either holds at once or times out.

use std::fmt;
use std::str::FromStr;
use std::str::SplitWhitespace;
use std::time::Duration;
use std::time::Instant;

use mc683xx_target::Cpu32Target;
use mc683xx_target::address::Address;
use mc683xx_target::function_code::FunctionCode;
use mc683xx_target::register::ParseRegisterError;
use mc683xx_target::register::ParseSystemRegisterError;
use mc683xx_target::register::Register;
use mc683xx_target::register::SystemRegister;
use mc683xx_target::size::Size;

/// What starts a comment.
const COMMENT: char = '#';
/// What separates a memory command's name from its size.
const SIZE_SEPARATOR: char = '.';
/// Hex numbers' prefix.
const HEX_PREFIX: &str = "0x";
/// The highest function code, FC\[2:0\] all set (CPU32RM 5.3).
const MAX_FUNCTION_CODE: u32 = 7;

/// `write_memory`.
const WRITE_MEMORY: &str = "write_memory";
/// `write_register`.
const WRITE_REGISTER: &str = "write_register";
/// `write_system_register`.
const WRITE_SYSTEM_REGISTER: &str = "write_system_register";
/// `read_memory`.
const READ_MEMORY: &str = "read_memory";
/// `poll_memory`.
const POLL_MEMORY: &str = "poll_memory";
/// The `expect` clause.
const EXPECT: &str = "expect";
/// The `mask` clause.
const MASK: &str = "mask";
/// The `timeout_ms` clause.
const TIMEOUT_MS: &str = "timeout_ms";
/// The `fc` clause.
const FC: &str = "fc";

/// One command of a script.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Command {
    /// Writes `value` to memory.
    WriteMemory {
        /// Where.
        address: Address,
        /// In one access of this size.
        size: Size,
        /// What, in the size's low bits.
        value: u32,
        /// In this space.
        function_code: FunctionCode,
    },
    /// Writes a data or address register.
    WriteRegister {
        /// Which.
        register: Register,
        /// What.
        value: u32,
    },
    /// Writes a system register.
    WriteSystemRegister {
        /// Which.
        register: SystemRegister,
        /// What.
        value: u32,
    },
    /// Reads memory once and checks it.
    ReadMemory {
        /// Where.
        address: Address,
        /// In one access of this size.
        size: Size,
        /// The value read, masked with `mask`, must be this.
        expect: u32,
        /// The bits checked.
        mask: u32,
        /// In this space.
        function_code: FunctionCode,
    },
    /// Reads memory until it checks, or the time is up.
    PollMemory {
        /// Where.
        address: Address,
        /// In accesses of this size.
        size: Size,
        /// The value read, masked with `mask`, must be this.
        expect: u32,
        /// The bits checked.
        mask: u32,
        /// Host time from the first read to give up after.
        timeout: Duration,
        /// In this space.
        function_code: FunctionCode,
    },
}

/// Why a line is not a command.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ParseCommandError {
    /// The line is blank or only a comment.
    #[error("no command")]
    Empty,
    /// The first word names no command.
    #[error("{name:?} is not a command")]
    Unknown {
        /// The word.
        name: String,
    },
    /// A memory command without `.b`, `.w` or `.l`, or with another.
    #[error("{name:?} needs a size: .b, .w or .l")]
    Size {
        /// The command's first word.
        name: String,
    },
    /// An operand or a clause's value is missing.
    #[error("missing {what}")]
    Missing {
        /// What.
        what: &'static str,
    },
    /// A word where none, or no such clause, is expected.
    #[error("unexpected {word:?}")]
    Unexpected {
        /// The word.
        word: String,
    },
    /// A clause given twice.
    #[error("`{clause}` given twice")]
    Repeated {
        /// The clause's keyword.
        clause: &'static str,
    },
    /// A word that is not a number.
    #[error("{word:?} is not a number: hex with 0x, or decimal")]
    Number {
        /// The word.
        word: String,
    },
    /// A number too wide for its access.
    #[error("{value:#x} does not fit in {} bits", size.bytes() * 8)]
    TooWide {
        /// The number.
        value: u32,
        /// The access size.
        size: Size,
    },
    /// A word or long access at an odd address, an address error
    /// (CPU32RM 6.2.3).
    #[error("a {}-byte access at odd address {address}", size.bytes())]
    Odd {
        /// The address.
        address: Address,
        /// The access size.
        size: Size,
    },
    /// A function code past FC\[2:0\].
    #[error("function code {value} is not 0 to 7")]
    FunctionCode {
        /// The number.
        value: u32,
    },
    /// A word that names no data or address register.
    #[error("{name:?}: {error}")]
    Register {
        /// The word.
        name: String,
        /// Why.
        error: ParseRegisterError,
    },
    /// A word that names no system register.
    #[error("{name:?}: {error}")]
    SystemRegister {
        /// The word.
        name: String,
        /// Why.
        error: ParseSystemRegisterError,
    },
    /// An expected value with bits outside the mask, which no read
    /// can match.
    #[error("expect {expect:#x} has bits outside mask {mask:#x}")]
    OutsideMask {
        /// The expected value.
        expect: u32,
        /// The mask.
        mask: u32,
    },
}

/// The words of a line, taken in order.
type Words<'a> = SplitWhitespace<'a>;

/// `word` as a number: hex after `0x`, otherwise decimal, digits only.
fn number(word: &str) -> Result<u32, ParseCommandError> {
    let error = || ParseCommandError::Number {
        word: word.to_owned(),
    };
    let (digits, radix) = match word.strip_prefix(HEX_PREFIX) {
        Some(digits) => (digits, 16),
        None => (word, 10),
    };
    // from_str_radix takes a sign, which an address or value has not.
    if digits.is_empty() || !digits.chars().all(|digit| digit.is_digit(radix)) {
        return Err(error());
    }
    u32::from_str_radix(digits, radix).map_err(|_| error())
}

/// The next word, or [`ParseCommandError::Missing`] naming `what`.
fn next<'a>(words: &mut Words<'a>, what: &'static str) -> Result<&'a str, ParseCommandError> {
    words.next().ok_or(ParseCommandError::Missing { what })
}

/// The next word as a number.
fn next_number(words: &mut Words<'_>, what: &'static str) -> Result<u32, ParseCommandError> {
    number(next(words, what)?)
}

/// `value`, unless it is too wide for `size`.
const fn fitting(value: u32, size: Size) -> Result<u32, ParseCommandError> {
    if value & !size.mask() == 0 {
        Ok(value)
    } else {
        Err(ParseCommandError::TooWide { value, size })
    }
}

/// `address`, unless a `size` access there is an address error.
fn aligned(address: u32, size: Size) -> Result<Address, ParseCommandError> {
    let address = Address(address);
    if size != Size::Byte && address.is_odd() {
        Err(ParseCommandError::Odd { address, size })
    } else {
        Ok(address)
    }
}

/// The rest of the words as clauses: a keyword of `keywords` and a
/// number each, in any order, each once.
fn clauses<const N: usize>(
    words: &mut Words<'_>,
    keywords: [&'static str; N],
) -> Result<[Option<u32>; N], ParseCommandError> {
    let mut values = [None; N];
    while let Some(word) = words.next() {
        let index = keywords
            .iter()
            .position(|keyword| *keyword == word)
            .ok_or_else(|| ParseCommandError::Unexpected {
                word: word.to_owned(),
            })?;
        if values[index].is_some() {
            return Err(ParseCommandError::Repeated {
                clause: keywords[index],
            });
        }
        values[index] = Some(next_number(words, keywords[index])?);
    }
    Ok(values)
}

/// A required clause's value.
fn required(value: Option<u32>, clause: &'static str) -> Result<u32, ParseCommandError> {
    value.ok_or(ParseCommandError::Missing { what: clause })
}

/// The function code in an `fc` clause, supervisor data without one.
fn function_code(value: Option<u32>) -> Result<FunctionCode, ParseCommandError> {
    match value {
        None => Ok(FunctionCode::SUPERVISOR_DATA),
        Some(value) if value <= MAX_FUNCTION_CODE => Ok(FunctionCode::new(value)),
        Some(value) => Err(ParseCommandError::FunctionCode { value }),
    }
}

/// The mask, the default every bit of `size`, and the expected value,
/// each fitting it and the value within the mask.
fn checked(expect: u32, mask: Option<u32>, size: Size) -> Result<(u32, u32), ParseCommandError> {
    let mask = fitting(mask.unwrap_or(size.mask()), size)?;
    let expect = fitting(expect, size)?;
    if expect & !mask != 0 {
        return Err(ParseCommandError::OutsideMask { expect, mask });
    }
    Ok((expect, mask))
}

/// The size a memory command's suffix names.
fn size(suffix: &str) -> Option<Size> {
    match suffix {
        "b" => Some(Size::Byte),
        "w" => Some(Size::Word),
        "l" => Some(Size::Long),
        _ => None,
    }
}

/// A size's suffix.
const fn suffix(size: Size) -> &'static str {
    match size {
        Size::Byte => "b",
        Size::Word => "w",
        Size::Long => "l",
    }
}

/// A line's text with its comment cut.
fn uncommented(line: &str) -> &str {
    line.split_once(COMMENT).map_or(line, |(text, _)| text)
}

/// A command as a script writes it: one line, with no comment.
impl FromStr for Command {
    type Err = ParseCommandError;

    fn from_str(line: &str) -> Result<Self, ParseCommandError> {
        let mut words = uncommented(line).split_whitespace();
        let first = words.next().ok_or(ParseCommandError::Empty)?;
        let (name, suffix) = match first.split_once(SIZE_SEPARATOR) {
            Some((name, suffix)) => (name, Some(suffix)),
            None => (first, None),
        };
        let unknown = || ParseCommandError::Unknown {
            name: first.to_owned(),
        };
        // A memory command's size, which it must have.
        let sized = || {
            suffix
                .and_then(size)
                .ok_or_else(|| ParseCommandError::Size {
                    name: first.to_owned(),
                })
        };
        // A register command, which takes no size.
        let no_size = || suffix.map_or(Ok(()), |_| Err(unknown()));
        let command = match name {
            WRITE_MEMORY => {
                let size = sized()?;
                let address = aligned(next_number(&mut words, "address")?, size)?;
                let value = fitting(next_number(&mut words, "value")?, size)?;
                let [fc] = clauses(&mut words, [FC])?;
                Self::WriteMemory {
                    address,
                    size,
                    value,
                    function_code: function_code(fc)?,
                }
            }
            READ_MEMORY => {
                let size = sized()?;
                let address = aligned(next_number(&mut words, "address")?, size)?;
                let [expect, mask, fc] = clauses(&mut words, [EXPECT, MASK, FC])?;
                let (expect, mask) = checked(required(expect, EXPECT)?, mask, size)?;
                Self::ReadMemory {
                    address,
                    size,
                    expect,
                    mask,
                    function_code: function_code(fc)?,
                }
            }
            POLL_MEMORY => {
                let size = sized()?;
                let address = aligned(next_number(&mut words, "address")?, size)?;
                let [mask, expect, timeout, fc] =
                    clauses(&mut words, [MASK, EXPECT, TIMEOUT_MS, FC])?;
                let (expect, mask) =
                    checked(required(expect, EXPECT)?, Some(required(mask, MASK)?), size)?;
                Self::PollMemory {
                    address,
                    size,
                    expect,
                    mask,
                    timeout: Duration::from_millis(required(timeout, TIMEOUT_MS)?.into()),
                    function_code: function_code(fc)?,
                }
            }
            WRITE_REGISTER => {
                no_size()?;
                let name = next(&mut words, "register")?;
                let register = name.parse().map_err(|error| ParseCommandError::Register {
                    name: name.to_owned(),
                    error,
                })?;
                Self::WriteRegister {
                    register,
                    value: next_number(&mut words, "value")?,
                }
            }
            WRITE_SYSTEM_REGISTER => {
                no_size()?;
                let name = next(&mut words, "register")?;
                let register = name
                    .parse()
                    .map_err(|error| ParseCommandError::SystemRegister {
                        name: name.to_owned(),
                        error,
                    })?;
                Self::WriteSystemRegister {
                    register,
                    value: next_number(&mut words, "value")?,
                }
            }
            _ => return Err(unknown()),
        };
        match words.next() {
            Some(word) => Err(ParseCommandError::Unexpected {
                word: word.to_owned(),
            }),
            None => Ok(command),
        }
    }
}

/// ` fc N`, or nothing for supervisor data.
struct FcClause(FunctionCode);

impl fmt::Display for FcClause {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.0 == FunctionCode::SUPERVISOR_DATA {
            Ok(())
        } else {
            write!(formatter, " {FC} {}", self.0)
        }
    }
}

/// The command as a script writes it, which parses back to it; `mask`
/// left out where it is every bit, `fc` where it is supervisor data.
impl fmt::Display for Command {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::WriteMemory {
                address,
                size,
                value,
                function_code,
            } => write!(
                formatter,
                "{WRITE_MEMORY}.{} {address} {value:#x}{}",
                suffix(size),
                FcClause(function_code)
            ),
            Self::WriteRegister { register, value } => {
                write!(formatter, "{WRITE_REGISTER} {register} {value:#x}")
            }
            Self::WriteSystemRegister { register, value } => {
                write!(formatter, "{WRITE_SYSTEM_REGISTER} {register} {value:#x}")
            }
            Self::ReadMemory {
                address,
                size,
                expect,
                mask,
                function_code,
            } => {
                write!(
                    formatter,
                    "{READ_MEMORY}.{} {address} {EXPECT} {expect:#x}",
                    suffix(size)
                )?;
                if mask != size.mask() {
                    write!(formatter, " {MASK} {mask:#x}")?;
                }
                write!(formatter, "{}", FcClause(function_code))
            }
            Self::PollMemory {
                address,
                size,
                expect,
                mask,
                timeout,
                function_code,
            } => write!(
                formatter,
                "{POLL_MEMORY}.{} {address} {MASK} {mask:#x} {EXPECT} {expect:#x} {TIMEOUT_MS} {}{}",
                suffix(size),
                timeout.as_millis(),
                FcClause(function_code)
            ),
        }
    }
}

/// Why a command failed on the target.  `E` is the target's error.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum CommandError<E> {
    /// The target failed the access.
    #[error(transparent)]
    Target(E),
    /// A read did not check.
    #[error("read {read:#x}")]
    Mismatch {
        /// The value read.
        read: u32,
    },
    /// A poll did not check in time.
    #[error("timed out, last read {read:#x}")]
    Timeout {
        /// The last value read.
        read: u32,
    },
}

/// `value`'s four bytes, big-endian, and where a `size` access's bytes,
/// its low ones, start in them.
fn big_endian(value: u32, size: Size) -> ([u8; 4], usize) {
    let bytes = value.to_be_bytes();
    let start = bytes.len() - size.bytes() as usize;
    (bytes, start)
}

/// Reads one `size` access at `address`.
fn read<T: Cpu32Target>(
    target: &mut T,
    address: Address,
    size: Size,
    function_code: FunctionCode,
) -> Result<u32, CommandError<T::Error>> {
    let (mut buffer, start) = big_endian(0, size);
    target
        .read_memory(address, function_code, size, &mut buffer[start..])
        .map_err(CommandError::Target)?;
    Ok(u32::from_be_bytes(buffer))
}

impl Command {
    /// Runs the command on `target`, which is halted.  A poll takes
    /// host time (the module's documentation).
    ///
    /// # Errors
    /// The target failed an access, a read did not check, or a poll
    /// did not check in time.
    pub fn run<T: Cpu32Target>(&self, target: &mut T) -> Result<(), CommandError<T::Error>> {
        match *self {
            Self::WriteMemory {
                address,
                size,
                value,
                function_code,
            } => {
                let (buffer, start) = big_endian(value, size);
                target
                    .write_memory(address, function_code, size, &buffer[start..])
                    .map_err(CommandError::Target)
            }
            Self::WriteRegister { register, value } => target
                .write_register(register, value)
                .map_err(CommandError::Target),
            Self::WriteSystemRegister { register, value } => target
                .write_system_register(register, value)
                .map_err(CommandError::Target),
            Self::ReadMemory {
                address,
                size,
                expect,
                mask,
                function_code,
            } => {
                let read = read(target, address, size, function_code)?;
                if read & mask == expect {
                    Ok(())
                } else {
                    Err(CommandError::Mismatch { read })
                }
            }
            Self::PollMemory {
                address,
                size,
                expect,
                mask,
                timeout,
                function_code,
            } => {
                let start = Instant::now();
                loop {
                    let read = read(target, address, size, function_code)?;
                    if read & mask == expect {
                        return Ok(());
                    }
                    if start.elapsed() >= timeout {
                        return Err(CommandError::Timeout { read });
                    }
                }
            }
        }
    }
}

/// A command and the line it is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct Line {
    /// The line, from 1.
    pub number: usize,
    /// Its command.
    pub command: Command,
}

/// A script: its commands, in order, with their lines.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Script {
    /// The commands, in order.
    lines: Vec<Line>,
}

/// A line of a script that is not a command.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("line {line}: {error}")]
pub struct ParseScriptError {
    /// The line, from 1.
    pub line: usize,
    /// Why.
    pub error: ParseCommandError,
}

/// A command of a script that failed, and the line it is on.
#[derive(Debug, thiserror::Error)]
#[error("line {line}: `{command}`: {error}")]
pub struct ScriptError<E> {
    /// The line, from 1.
    pub line: usize,
    /// The command.
    pub command: Command,
    /// Why.
    pub error: CommandError<E>,
}

/// A script's text: a command a line, `#` comments and blank lines.
impl FromStr for Script {
    type Err = ParseScriptError;

    fn from_str(text: &str) -> Result<Self, ParseScriptError> {
        let lines = text
            .lines()
            .enumerate()
            .filter(|(_, line)| !uncommented(line).trim().is_empty())
            .map(|(index, line)| {
                let number = index + 1;
                line.parse()
                    .map(|command| Line { number, command })
                    .map_err(|error| ParseScriptError {
                        line: number,
                        error,
                    })
            })
            .collect::<Result<_, _>>()?;
        Ok(Self { lines })
    }
}

impl Script {
    /// The commands, in order, with their lines.
    #[must_use]
    pub fn lines(&self) -> &[Line] {
        &self.lines
    }

    /// Runs the commands on `target`, which is halted, in order, up to
    /// the first that fails.
    ///
    /// # Errors
    /// The first command that failed, with its line.
    pub fn run<T: Cpu32Target>(&self, target: &mut T) -> Result<(), ScriptError<T::Error>> {
        self.lines.iter().try_for_each(|line| {
            line.command.run(target).map_err(|error| ScriptError {
                line: line.number,
                command: line.command,
                error,
            })
        })
    }
}

#[cfg(test)]
mod tests;
