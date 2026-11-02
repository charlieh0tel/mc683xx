//! BDM's serial interface: the 17-bit exchange a development system
//! clocks with DSCLK, and the command set it carries (CPU32RM 7.2.7,
//! 7.2.8).  [`Mc68331::bdm_exchange`] is one transfer.
//!
//! Each command runs as soon as its last word is received and is done
//! before the next transfer, so a result is never late: "not ready"
//! goes out only while a command's operands come in, after an error
//! response, and first after entry (PLAN.md, Background debug mode).

use std::mem;

use mc683xx_modules::sim::ResetCause;
use mc683xx_target::address::Address;
use mc683xx_target::register::Register;
use mc683xx_target::register::SystemRegister;
use mc683xx_target::size::Size;
use serde::Deserialize;
use serde::Serialize;

use crate::chip::Mc68331;

/// The status/control bit, bit 16 of a transfer (CPU32RM 7.2.7).
pub const STATUS: u32 = 1 << 16;
/// "Not ready with response; come again" (CPU32RM Table 7-3).
pub const NOT_READY: u32 = STATUS;
/// "BERR terminated bus cycle; data invalid" (CPU32RM Table 7-3).
pub const BUS_ERROR: u32 = STATUS | 0x0001;
/// "Illegal command" (CPU32RM Table 7-3).
pub const ILLEGAL: u32 = STATUS | 0xFFFF;
/// "Command complete; status OK" (CPU32RM Table 7-3).
pub const COMPLETE: u32 = 0xFFFF;
/// What a transfer reads while the CPU is not in background mode: DSO
/// is then IPIPE (CPU32RM 7.2.6), which the chip does not model, so
/// the line reads high (provisional; PLAN.md, Background debug mode).
pub const NOT_IN_BACKGROUND: u32 = STATUS | 0xFFFF;
/// A transfer's 16 data bits.
const DATA: u32 = 0xFFFF;

/// The fixed bits of each command word (CPU32RM 7.2.8.4-7.2.8.15).
mod command {
    /// RDREG/RAREG; A/D and the register in bits 3-0.
    pub(super) const READ_REGISTER: u16 = 0x2180;
    /// WDREG/WAREG.
    pub(super) const WRITE_REGISTER: u16 = 0x2080;
    /// RSREG; the select code in bits 3-0.  7.2.8.6 prints bit 8, R/W,
    /// clear, as WSREG's; 7.2.8.1's R/W field sets it for a read.
    pub(super) const READ_SYSTEM_REGISTER: u16 = 0x2580;
    /// WSREG.
    pub(super) const WRITE_SYSTEM_REGISTER: u16 = 0x2480;
    /// READ; the size in bits 7-6.
    pub(super) const READ: u16 = 0x1900;
    /// WRITE.
    pub(super) const WRITE: u16 = 0x1800;
    /// DUMP.
    pub(super) const DUMP: u16 = 0x1D00;
    /// FILL.
    pub(super) const FILL: u16 = 0x1C00;
    /// GO.
    pub(super) const GO: u16 = 0x0C00;
    /// CALL.
    pub(super) const CALL: u16 = 0x0800;
    /// RST.
    pub(super) const RST: u16 = 0x0400;
    /// NOP.
    pub(super) const NOP: u16 = 0x0000;
    /// The register field of RDREG, WDREG, RSREG and WSREG, A/D with it.
    pub(super) const REGISTER_MASK: u16 = 0x000F;
    /// Where a sized command's size field starts (7.2.8.1).
    pub(super) const SIZE_SHIFT: u16 = 6;
    /// A sized command's size field.
    pub(super) const SIZE_MASK: u16 = 0b11 << SIZE_SHIFT;
}

/// What the serial interface does with the next word it receives.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
enum Next {
    /// Decode it as a command.
    #[default]
    Command,
    /// Take it as the command's next operand word; the words so far.
    Operand {
        /// The command word.
        command: u16,
        /// Its operand words received, most significant first.
        words: Vec<u16>,
    },
    /// Ignore it, a long result's high word going out; the low word
    /// goes out next.
    Low(u16),
    /// Ignore it, an error response going out (Figure 7-10, "data
    /// unused from this transfer"); "not ready" goes out next.
    Ignore,
}

/// The serial interface's state between transfers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Serial {
    /// The output shift register: the 17 bits the next transfer shifts
    /// out.
    response: u32,
    /// What the next word received is.
    next: Next,
    /// DUMP's and FILL's address: the one after the last READ, WRITE,
    /// DUMP or FILL access (7.2.8.10, 7.2.8.11).
    block: u32,
}

impl Default for Serial {
    /// Between commands, "not ready" first: what the first transfer
    /// after entry shifts out the manual leaves unstated ("???").
    fn default() -> Self {
        Self {
            response: NOT_READY,
            next: Next::Command,
            block: 0,
        }
    }
}

impl Serial {
    /// Drops a command in progress, as a debugger's own command does,
    /// leaving the interface between commands; DUMP's and FILL's
    /// address is kept.
    pub(crate) fn discard(&mut self) {
        *self = Self {
            block: self.block,
            ..Self::default()
        };
    }
}

/// How a command ended: what the serial interface shifts out next.
enum Outcome {
    /// A long result, high word first.
    Long(u32),
    /// A word result, or a byte's, zero-extended.
    Word(u16),
    /// "Command complete".
    Complete,
    /// A bus or address error.
    Fault,
    /// Not a command.
    Illegal,
    /// The CPU left background mode.
    Resumed,
}

/// A command (CPU32RM Table 7-4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Command {
    /// RDREG/RAREG.
    ReadRegister(Register),
    /// WDREG/WAREG.
    WriteRegister(Register),
    /// RSREG.
    ReadSystemRegister(SystemRegister),
    /// WSREG.
    WriteSystemRegister(SystemRegister),
    /// READ.
    Read(Size),
    /// WRITE.
    Write(Size),
    /// DUMP.
    Dump(Size),
    /// FILL.
    Fill(Size),
    /// GO.
    Go,
    /// CALL.
    Call,
    /// RST.
    Reset,
    /// NOP.
    Nop,
}

impl Command {
    /// The command `word` is, or `None` for an unused format, ILLEGAL
    /// (7.2.8.16): another operation, a reserved size, a select code
    /// 7.2.8.6 does not list, or a bit the format gives as 0 set.
    fn decode(word: u16) -> Option<Self> {
        let register = word & command::REGISTER_MASK;
        let size = Size::from_field((word & command::SIZE_MASK) >> command::SIZE_SHIFT);
        match (
            word & !command::REGISTER_MASK,
            word & !command::SIZE_MASK,
            size,
        ) {
            (command::READ_REGISTER, ..) => Register::from_code(register).map(Self::ReadRegister),
            (command::WRITE_REGISTER, ..) => Register::from_code(register).map(Self::WriteRegister),
            (command::READ_SYSTEM_REGISTER, ..) => {
                SystemRegister::from_code(register).map(Self::ReadSystemRegister)
            }
            (command::WRITE_SYSTEM_REGISTER, ..) => {
                SystemRegister::from_code(register).map(Self::WriteSystemRegister)
            }
            (_, command::READ, Some(size)) => Some(Self::Read(size)),
            (_, command::WRITE, Some(size)) => Some(Self::Write(size)),
            (_, command::DUMP, Some(size)) => Some(Self::Dump(size)),
            (_, command::FILL, Some(size)) => Some(Self::Fill(size)),
            _ => match word {
                command::GO => Some(Self::Go),
                command::CALL => Some(Self::Call),
                command::RST => Some(Self::Reset),
                command::NOP => Some(Self::Nop),
                _ => None,
            },
        }
    }

    /// The operand words it takes after its command word: an address
    /// is two, data one for a byte or a word and two for a long
    /// (7.2.8.1).
    const fn operand_words(self) -> usize {
        match self {
            Self::WriteRegister(_) | Self::WriteSystemRegister(_) | Self::Read(_) | Self::Call => 2,
            Self::Write(size) => 2 + data_words(size),
            Self::Fill(size) => data_words(size),
            Self::ReadRegister(_)
            | Self::ReadSystemRegister(_)
            | Self::Dump(_)
            | Self::Go
            | Self::Reset
            | Self::Nop => 0,
        }
    }
}

/// The words `size`'s data takes: a byte's is a word (7.2.8.9).
const fn data_words(size: Size) -> usize {
    match size {
        Size::Byte | Size::Word => 1,
        Size::Long => 2,
    }
}

/// `words`, most significant first, as one value.
fn value(words: &[u16]) -> u32 {
    words
        .iter()
        .fold(0, |value, &word| value << 16 | u32::from(word))
}

impl Mc68331 {
    /// One 17-bit transfer on BDM's serial interface (CPU32RM 7.2.7):
    /// `word` in, its bit 16, the control bit, ignored as the CPU does
    /// (Table 7-3); the 17 bits shifted out returned, bit 16 the status
    /// bit.  The command a word completes runs before this returns
    /// (7.2.8): its result, or "command complete", goes out with the
    /// next transfer, a long's in two (7.2.8.2).
    ///
    /// While the CPU is not in background mode the interface is off
    /// (7.2.6): the word is not received, and its DSCLK pulses are
    /// BKPT's, a breakpoint (7.2.7.2) acknowledged after it, as
    /// [`Cpu32Target::halt`](mc683xx_target::Cpu32Target::halt) does
    /// (provisional; PLAN.md, Background debug mode).  The transfer
    /// reads [`NOT_IN_BACKGROUND`].
    pub fn bdm_exchange(&mut self, word: u32) -> u32 {
        if self.cpu.background().is_none() {
            self.breakpoint();
            return NOT_IN_BACKGROUND;
        }
        let data = (word & DATA) as u16;
        // the shift register reloads with "not ready" (7.2.7.1)
        let out = mem::replace(&mut self.serial.response, NOT_READY);
        let (command, words) = match mem::take(&mut self.serial.next) {
            Next::Command => (data, Vec::new()),
            Next::Operand { command, mut words } => {
                words.push(data);
                (command, words)
            }
            Next::Low(low) => {
                self.serial.response = u32::from(low);
                return out;
            }
            Next::Ignore => return out,
        };
        let outcome = match Command::decode(command) {
            None => Outcome::Illegal,
            Some(decoded) if words.len() < decoded.operand_words() => {
                self.serial.next = Next::Operand { command, words };
                return out;
            }
            Some(decoded) => self.serial_command(decoded, &words),
        };
        self.serial_finish(outcome);
        out
    }

    /// Runs `command` with its operand `words`.
    fn serial_command(&mut self, command: Command, words: &[u16]) -> Outcome {
        match command {
            Command::ReadRegister(register) => Outcome::Long(self.register(register)),
            Command::WriteRegister(register) => {
                self.set_register(register, value(words));
                Outcome::Complete
            }
            Command::ReadSystemRegister(register) => Outcome::Long(self.system_register(register)),
            Command::WriteSystemRegister(register) => {
                self.set_system_register(register, value(words));
                Outcome::Complete
            }
            Command::Read(size) => self.serial_read(value(&words[..2]), size),
            Command::Dump(size) => self.serial_read(self.serial.block, size),
            Command::Write(size) => self.serial_write(value(&words[..2]), size, value(&words[2..])),
            Command::Fill(size) => self.serial_write(self.serial.block, size, value(words)),
            Command::Go => {
                self.resume();
                Outcome::Resumed
            }
            Command::Call => match self.call(Address(value(words))) {
                Ok(()) => Outcome::Resumed,
                Err(_) => Outcome::Fault,
            },
            Command::Reset => {
                self.reset_peripherals();
                Outcome::Complete
            }
            Command::Nop => Outcome::Complete,
        }
    }

    /// READ or DUMP at `address`, through SFC (7.2.8.8, 7.2.8.10).
    fn serial_read(&mut self, address: u32, size: Size) -> Outcome {
        self.serial.block = address.wrapping_add(size.bytes());
        let space = self.cpu.registers.sfc;
        match (self.bus_read(Address(address), size, space), size) {
            (Err(_), _) => Outcome::Fault,
            (Ok(value), Size::Long) => Outcome::Long(value),
            (Ok(value), _) => Outcome::Word(value as u16),
        }
    }

    /// WRITE or FILL of `value` at `address`, through DFC (7.2.8.9,
    /// 7.2.8.11); a byte's is the low byte of its word.
    fn serial_write(&mut self, address: u32, size: Size, value: u32) -> Outcome {
        self.serial.block = address.wrapping_add(size.bytes());
        let value = match size {
            Size::Byte => value & 0xFF,
            Size::Word | Size::Long => value,
        };
        let space = self.cpu.registers.dfc;
        match self.bus_write(Address(address), size, space, value) {
            Ok(()) => Outcome::Complete,
            Err(_) => Outcome::Fault,
        }
    }

    /// RST: RESET driven, as the `reset` instruction drives it, the CPU
    /// not reset (7.2.8.14), at once: no time passes in background mode.
    /// The modules come out of their reset frozen, FREEZE still asserted.
    fn reset_peripherals(&mut self) {
        self.board.reset_instruction(self.now);
        if mem::take(&mut self.board.system_reload) {
            self.board.reload(&mut self.cpu.memory, ResetCause::System);
        }
        if self.board.decode_changed {
            self.board.rebuild(&mut self.cpu.memory);
        }
        self.board.frozen = false;
        if self.board.set_freeze(true, self.now) {
            self.board.rebuild(&mut self.cpu.memory);
        }
    }

    /// Loads what goes out next for `outcome`.
    fn serial_finish(&mut self, outcome: Outcome) {
        let serial = &mut self.serial;
        match outcome {
            Outcome::Long(value) => {
                serial.response = value >> 16;
                serial.next = Next::Low(value as u16);
            }
            Outcome::Word(value) => serial.response = u32::from(value),
            Outcome::Complete => serial.response = COMPLETE,
            Outcome::Fault => {
                serial.response = BUS_ERROR;
                serial.next = Next::Ignore;
            }
            Outcome::Illegal => {
                serial.response = ILLEGAL;
                serial.next = Next::Ignore;
            }
            Outcome::Resumed => {}
        }
    }
}
