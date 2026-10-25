//! Exceptions (CPU32RM 6): what instructions and bus cycles raise, the
//! vectors they go through, and their processing: the four-, six- and
//! twelve-word stack frames (6.4) and the special status word (6.3).

use crate::address::Address;
use crate::cpu::Core;
use crate::cpu::Halt;
use crate::cycles::Cycles;
use crate::function_code::FunctionCode;
use crate::interrupt::InterruptLevel;
use crate::registers::status;
use crate::size::Size;
use crate::timing;
use std::fmt;

/// An exception vector number (CPU32RM 6.1.1, Table 6-1).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub struct Vector(pub u8);

/// The vector number in decimal, as the manuals number them (CPU32RM
/// Table 6-1).
impl fmt::Display for Vector {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.0)
    }
}

impl From<u8> for Vector {
    fn from(number: u8) -> Self {
        Self(number)
    }
}

impl From<Vector> for u8 {
    fn from(vector: Vector) -> Self {
        vector.0
    }
}

impl Vector {
    /// Reset: the initial supervisor stack pointer.
    pub const RESET_STACK_POINTER: Self = Self(0);
    /// Reset: the initial program counter.
    pub const RESET_PROGRAM_COUNTER: Self = Self(1);
    /// Bus error.
    pub const BUS_ERROR: Self = Self(2);
    /// Address error.
    pub const ADDRESS_ERROR: Self = Self(3);
    /// Illegal instruction.
    pub const ILLEGAL_INSTRUCTION: Self = Self(4);
    /// Zero divide.
    pub const ZERO_DIVIDE: Self = Self(5);
    /// CHK and CHK2.
    pub const CHK: Self = Self(6);
    /// TRAPcc and TRAPV.
    pub const TRAPCC: Self = Self(7);
    /// Privilege violation.
    pub const PRIVILEGE_VIOLATION: Self = Self(8);
    /// Trace.
    pub const TRACE: Self = Self(9);
    /// Line 1010 emulator.
    pub const LINE_A: Self = Self(10);
    /// Line 1111 emulator.
    pub const LINE_F: Self = Self(11);
    /// Hardware breakpoint.
    pub const HARDWARE_BREAKPOINT: Self = Self(12);
    /// Format error.
    pub const FORMAT_ERROR: Self = Self(14);
    /// Uninitialized interrupt.
    pub const UNINITIALIZED_INTERRUPT: Self = Self(15);
    /// Spurious interrupt; the autovectors for levels 1 to 7 follow it.
    pub const SPURIOUS_INTERRUPT: Self = Self(24);
    /// TRAP #0; #1 to #15 follow it.
    pub const TRAP: Self = Self(32);

    /// The vector's offset from VBR.
    #[must_use]
    pub const fn offset(self) -> u32 {
        self.0 as u32 * 4
    }

    /// The autovector of interrupt level `level`, 25 to 31.
    #[must_use]
    pub const fn autovector(level: InterruptLevel) -> Self {
        Self(Self::SPURIOUS_INTERRUPT.0 + level.get())
    }

    /// TRAP #`number`'s vector, 32 to 47.
    #[must_use]
    pub const fn trap(number: u8) -> Self {
        Self(Self::TRAP.0 + number)
    }
}

/// The kind of bus cycle a fault ended: what the SSW's IN and RW bits
/// record of it (CPU32RM 6.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum CycleKind {
    /// An instruction fetch: IN set, RW set.
    Fetch,
    /// An operand read: IN clear, RW set.
    Read,
    /// An operand write: IN clear, RW clear.
    Write,
}

/// A bus or address error's cycle: what a format $C frame records of
/// it (CPU32RM 6.3, 6.4.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Fault {
    /// [`Vector::BUS_ERROR`] or [`Vector::ADDRESS_ERROR`].
    pub vector: Vector,
    /// The address of the cycle that faulted: for the second word of a
    /// long operand, the operand's address plus two.
    pub address: Address,
    /// The faulted cycle's function code: the SSW's FUNC.
    pub function_code: FunctionCode,
    /// What remained of the operand when the cycle faulted: the SSW's
    /// SIZ, a word for the second half of a long (CPU32RM 6.3).
    pub size: Size,
    /// Whether the operand was a long word: the SSW's LG.
    pub long: bool,
    /// The kind of cycle.
    pub cycle: CycleKind,
    /// The data a write was to write, right-aligned (the frame's
    /// DBUF); zero for a read.
    pub data: u32,
    /// Whether the cycle was the read or write of a read-modify-write
    /// operation (TAS): the SSW's RM.
    pub read_modify_write: bool,
}

/// An exception an instruction or a bus cycle raises.  The variant is
/// the stack frame it takes (CPU32RM 6.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Exception {
    /// A format $0 four-word frame, stacking `pc` (CPU32RM 6.4.1).
    Short {
        /// Its vector.
        vector: Vector,
        /// The program counter to stack: the instruction's own, or the
        /// next, as the exception says.
        pc: Address,
    },
    /// A format $2 six-word frame, stacking `pc` and the address of
    /// the instruction that raised it (CPU32RM 6.4.2).
    Long {
        /// Its vector.
        vector: Vector,
        /// The program counter to stack: the next instruction's.
        pc: Address,
        /// The instruction's own address.
        address: Address,
    },
    /// A bus or address error: a format $C frame (CPU32RM 6.4.3).
    Fault(Fault),
}

impl Exception {
    /// Its vector.
    #[must_use]
    pub const fn vector(self) -> Vector {
        match self {
            Self::Short { vector, .. } | Self::Long { vector, .. } => vector,
            Self::Fault(fault) => fault.vector,
        }
    }

    /// Whether it is in Table 6-2's group 3, processed before the
    /// instruction executes: illegal, line A and F, privilege
    /// violation.  The instruction is not traced (CPU32RM 6.2.10) and
    /// alters no register (6.2.8).
    #[must_use]
    pub(crate) const fn before_execution(self) -> bool {
        matches!(
            self.vector(),
            Vector::ILLEGAL_INSTRUCTION
                | Vector::LINE_A
                | Vector::LINE_F
                | Vector::PRIVILEGE_VIOLATION
        )
    }

    /// The same exception, a fault marked as the read or write of a
    /// read-modify-write operation, for TAS (the SSW's RM, CPU32RM
    /// 6.3.1.2).
    #[must_use]
    pub(crate) const fn in_read_modify_write(self) -> Self {
        match self {
            Self::Fault(fault) => Self::Fault(Fault {
                read_modify_write: true,
                ..fault
            }),
            other => other,
        }
    }
}

/// The microcode revision number this core stacks in a format $C frame.
///
/// It goes in the frame's internal transfer count register, and `rte`
/// requires it there (CPU32RM 6.2.12, 6.4.3, Figure 6-5).  The manuals
/// give no value; this one is ours.  Its bit 3 is SR bit 11 in the
/// word above a ten-word type IV frame, the faulted frame's SR, which
/// is always zero on the CPU32 (CPU32RM Figure 2-3), so no SR reads as
/// this revision.
pub const MICROCODE_REVISION: u8 = 0xc8;

/// MOVEM's transfer count: a format $C frame stacks it less the
/// transfers attempted (CPU32RM 6.3.1.3).
pub(crate) const MOVEM_TRANSFERS: u8 = 16;

/// The format/vector word (CPU32RM 6.1.4, Figure 6-1; RTE's
/// format/offset word, CPU32RM 4.4, RTE).
pub(crate) mod format {
    use super::Vector;

    /// Format $0, the four-word frame (Figure 6-3).
    pub(crate) const FOUR_WORD: u16 = 0x0;
    /// Format $2, the six-word frame (Figure 6-4).
    pub(crate) const SIX_WORD: u16 = 0x2;
    /// Format $C, the twelve-word bus error frame (Figures 6-6 to 6-8).
    pub(crate) const BUS_ERROR: u16 = 0xc;
    /// Where the format field starts.
    pub(crate) const SHIFT: u16 = 12;
    /// The vector offset, bits 9:0.
    pub(crate) const VECTOR_OFFSET: u16 = 0x03ff;

    /// The word for `format` and `vector`.
    pub(crate) const fn word(format: u16, vector: Vector) -> u16 {
        format << SHIFT | vector.offset() as u16
    }

    /// The vector a format/vector word names.
    pub(crate) const fn vector(word: u16) -> Vector {
        Vector(((word & VECTOR_OFFSET) >> 2) as u8)
    }
}

/// Byte offsets in the four- and six-word frames (Figures 6-3, 6-4)
/// and the format $C frame (Figures 6-6 to 6-8).
pub(crate) mod frame {
    /// The status register.
    pub(crate) const SR: u32 = 0x00;
    /// The return program counter.
    pub(crate) const PC: u32 = 0x02;
    /// The format/vector word.
    pub(crate) const FORMAT_VECTOR: u32 = 0x06;
    /// Bytes in the four-word frame.
    pub(crate) const FOUR_WORD_BYTES: u32 = 8;
    /// Bytes in the six-word frame.
    pub(crate) const SIX_WORD_BYTES: u32 = 12;
    /// Format $C: the faulted address.
    pub(crate) const FAULT_ADDRESS: u32 = 0x08;
    /// Format $C, types I to III: the data buffer.
    pub(crate) const DATA_BUFFER: u32 = 0x0c;
    /// Format $C, type IV: the pre-exception status register.
    pub(crate) const PRE_EXCEPTION_SR: u32 = 0x0c;
    /// Format $C, type IV: the faulted exception's format/vector word.
    pub(crate) const FAULTED_FORMAT_VECTOR: u32 = 0x0e;
    /// Format $C: the current instruction program counter; type IV of
    /// a six-word frame: its faulted instruction program counter.
    pub(crate) const CURRENT_PC: u32 = 0x10;
    /// Format $C: the internal transfer count register (Figure 6-5).
    pub(crate) const TRANSFER_COUNT: u32 = 0x14;
    /// Format $C: the special status word.
    pub(crate) const SSW: u32 = 0x16;
    /// Bytes in the format $C frame.
    pub(crate) const BUS_ERROR_BYTES: u32 = 0x18;
    /// Format $C, type IV of a four-word frame: the internal transfer
    /// count register (CPU32RM 6.4.3).
    pub(crate) const SHORT_TRANSFER_COUNT: u32 = 0x10;
    /// Format $C, type IV of a four-word frame: the SSW.
    pub(crate) const SHORT_SSW: u32 = 0x12;
    /// Bytes in a type IV format $C frame of a four-word frame.
    pub(crate) const SHORT_BUS_ERROR_BYTES: u32 = 0x14;
    /// Where the microcode revision starts in the internal transfer
    /// count register.
    pub(crate) const REVISION_SHIFT: u16 = 8;
}

/// Special status word bits (CPU32RM 6.3).
pub(crate) mod ssw {
    /// TP: the fault was in exception processing (type IV).
    pub(crate) const TP: u16 = 1 << 15;
    /// MV: MOVEM was in progress (type III).
    pub(crate) const MV: u16 = 1 << 14;
    /// TR: a trace was pending.
    pub(crate) const TR: u16 = 1 << 12;
    /// RR: rerun the write after `rte`.
    pub(crate) const RR: u16 = 1 << 9;
    /// RM: a read-modify-write cycle.
    pub(crate) const RM: u16 = 1 << 8;
    /// IN: an instruction prefetch.
    pub(crate) const IN: u16 = 1 << 7;
    /// RW: a read (a prefetch or an operand read).
    pub(crate) const RW: u16 = 1 << 6;
    /// LG: the operand was a long word.
    pub(crate) const LG: u16 = 1 << 5;
    /// Where SIZ starts.
    pub(crate) const SIZ_SHIFT: u16 = 3;
    /// SIZ's two bits.
    pub(crate) const SIZ_MASK: u16 = 0b11;
    /// SIZ: a long word remains.
    pub(crate) const SIZ_LONG: u16 = 0b00;
    /// SIZ: a byte remains.
    pub(crate) const SIZ_BYTE: u16 = 0b01;
    /// SIZ: a word remains.
    pub(crate) const SIZ_WORD: u16 = 0b10;
    /// FUNC: the faulted cycle's function code.
    pub(crate) const FUNC: u16 = 0b111;
}

/// What became of an exception's processing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Processed {
    /// Its frame stacked and its handler's address in PC.
    Taken,
    /// A bus or address error while stacking or fetching the vector:
    /// a type IV fault taken in its place, or a double bus fault.
    Faulted,
}

/// MOVEM's progress through its operand transfers, which a type III
/// fault stacks and `rte` hands back (CPU32RM 6.3.1.3, 6.3.2.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct Movem {
    /// Transfers attempted, the one in progress included.
    pub(crate) attempted: u8,
    /// The next operand's address: the frame's fault address.
    pub(crate) next: Address,
}

/// MOVEM's progress as `rte` of a type III frame restored it, pending
/// until the MOVEM at the frame's return PC continues from it (CPU32RM
/// 6.3.2.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct MovemResume {
    /// The return PC: the faulted MOVEM's address.
    pub(crate) pc: Address,
    /// Its progress.
    pub(crate) progress: Movem,
}

/// How a format $C frame's instruction fault is recovered (CPU32RM
/// 6.3.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Recovery {
    /// Type I, a released write: the instruction completed.
    Released,
    /// Type II: the instruction is restarted.
    Restart,
    /// Type III: MOVEM is continued.
    Movem(Movem),
}

/// A stack frame as the words it stacks, lowest address first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Frame {
    /// Its words; the first `length` count.
    words: [u16; 12],
    /// Words in it.
    length: usize,
}

impl Frame {
    /// No words.
    const fn new() -> Self {
        Self {
            words: [0; 12],
            length: 0,
        }
    }

    /// The frame with `word` after its words.
    const fn word(self, word: u16) -> Self {
        let mut words = self.words;
        words[self.length] = word;
        Self {
            words,
            length: self.length + 1,
        }
    }

    /// The frame with `long` after its words, high word first.
    const fn long(self, long: u32) -> Self {
        self.word((long >> 16) as u16).word(long as u16)
    }

    /// Its words.
    pub(crate) fn words(&self) -> &[u16] {
        &self.words[..self.length]
    }

    /// Its size in bytes.
    const fn bytes(&self) -> u32 {
        2 * self.length as u32
    }
}

/// The four- or six-word frame (Figures 6-3, 6-4) for `sr`, `pc` and
/// `vector`, with the faulted instruction's address if six.
pub(crate) const fn short_frame(
    sr: u16,
    pc: Address,
    vector: Vector,
    faulted: Option<Address>,
) -> Frame {
    match faulted {
        None => Frame::new()
            .word(sr)
            .long(pc.0)
            .word(format::word(format::FOUR_WORD, vector)),
        Some(address) => Frame::new()
            .word(sr)
            .long(pc.0)
            .word(format::word(format::SIX_WORD, vector))
            .long(address.0),
    }
}

/// The SSW's LG, SIZ and FUNC for `fault` (CPU32RM 6.3).
const fn operand_bits(fault: &Fault) -> u16 {
    let size = match fault.size {
        Size::Long => ssw::SIZ_LONG,
        Size::Byte => ssw::SIZ_BYTE,
        Size::Word => ssw::SIZ_WORD,
    };
    let long = if fault.long { ssw::LG } else { 0 };
    long | size << ssw::SIZ_SHIFT | fault.function_code.bits() as u16 & ssw::FUNC
}

/// SSW TR, if `trace_pending`.
const fn trace_bit(trace_pending: bool) -> u16 {
    if trace_pending { ssw::TR } else { 0 }
}

/// The internal transfer count register (Figure 6-5) for `count`.
const fn transfer_count(count: u8) -> u16 {
    (MICROCODE_REVISION as u16) << frame::REVISION_SHIFT | count as u16
}

impl Core<'_> {
    /// Exception processing for a four- or six-word frame (CPU32RM
    /// 6.1.3): the status register copied, S set and T1 and T0 cleared
    /// (with an interrupt, the mask set to its `level`), the frame
    /// stacked, PC from the vector.  A fault on the way is a type IV
    /// fault, which stacks `trace_pending` in its SSW.
    pub(crate) fn process(
        &mut self,
        vector: Vector,
        pc: Address,
        faulted: Option<Address>,
        level: Option<InterruptLevel>,
        trace_pending: bool,
    ) -> Processed {
        let sr = self.cpu.registers.sr();
        let state = (sr | status::S) & !(status::T1 | status::T0);
        let state = match level {
            Some(level) => {
                state & !status::INTERRUPT_MASK | u16::from(level.get()) << status::INTERRUPT_SHIFT
            }
            None => state,
        };
        self.cpu.registers.set_sr(state);
        let frame = short_frame(sr, pc, vector, faulted);
        let top = self.cpu.registers.ssp;
        let taken = self
            .stack(top, &frame)
            .and_then(|bottom| Ok((bottom, self.vector(vector)?)));
        match taken {
            Ok((bottom, handler)) => {
                self.entered(vector, &frame, bottom, handler, level);
                Processed::Taken
            }
            Err(fault) if self.cpu.bare.stop_on_fault => {
                let end = self.cpu.fault_end(fault);
                self.cpu.end_with(end);
                Processed::Faulted
            }
            Err(fault) => {
                self.exception_fault(fault, top, &frame, trace_pending);
                Processed::Faulted
            }
        }
    }

    /// Exception processing for a bus or address error in an
    /// instruction (CPU32RM 6.2.2, 6.2.3, 6.3.1), types I to III: the
    /// format $C frame of Figures 6-6 and 6-7.  One while a bus or
    /// address error is still being taken is a double bus fault.
    pub(crate) fn take_fault(
        &mut self,
        fault: Fault,
        recovery: Recovery,
        return_pc: Address,
        current_pc: Address,
        trace_pending: bool,
    ) {
        if self.cpu.fault_window {
            return self.double_fault();
        }
        let sr = self.cpu.registers.sr();
        self.cpu
            .registers
            .set_sr((sr | status::S) & !(status::T1 | status::T0));
        let read = match fault.cycle {
            CycleKind::Fetch => ssw::IN | ssw::RW,
            CycleKind::Read => ssw::RW,
            CycleKind::Write => 0,
        };
        let read_modify_write = if fault.read_modify_write { ssw::RM } else { 0 };
        // The SSW patterns of 6.3.1.1 to 6.3.1.3.
        let (bits, address, count) = match recovery {
            Recovery::Released => (ssw::RR | trace_bit(trace_pending), fault.address, 0),
            Recovery::Restart => (read_modify_write | read, fault.address, 0),
            Recovery::Movem(movem) => {
                let rerun = if fault.cycle == CycleKind::Write {
                    ssw::RR
                } else {
                    0
                };
                (
                    ssw::MV | rerun | read,
                    movem.next,
                    MOVEM_TRANSFERS.saturating_sub(movem.attempted),
                )
            }
        };
        let frame = Frame::new()
            .word(sr)
            .long(return_pc.0)
            .word(format::word(format::BUS_ERROR, fault.vector))
            .long(address.0)
            .long(fault.data)
            .long(current_pc.0)
            .word(transfer_count(count))
            .word(bits | operand_bits(&fault));
        let top = self.cpu.registers.ssp;
        self.fault_frame(top, &frame, fault.vector);
    }

    /// A type IV fault (CPU32RM 6.3.1.4): `fault` while `faulted` was
    /// stacked to end at `top` or its vector fetched.  The format $C
    /// frame of Figure 6-8 carries the faulted frame's status register,
    /// return PC, format/vector word and, from a six-word frame, its
    /// faulted instruction PC; it is ten words for a four-word frame.
    /// It goes below the space the faulted frame takes, where `rte`
    /// rewrites it.
    fn exception_fault(&mut self, fault: Fault, top: u32, faulted: &Frame, trace_pending: bool) {
        if self.cpu.fault_window {
            return self.double_fault();
        }
        self.charge(Cycles(u64::from(
            timing::exception::BUS_ERROR_IN_EXCEPTION.cycles,
        )));
        let words = faulted.words();
        let frame = Frame::new()
            .word(self.cpu.registers.sr())
            .word(words[1])
            .word(words[2])
            .word(format::word(format::BUS_ERROR, fault.vector))
            .long(fault.address.0)
            .word(words[0])
            .word(words[3]);
        let frame = if let &[high, low] = &words[4..] {
            frame.word(high).word(low)
        } else {
            frame
        };
        // The pattern of 6.3.1.4, TP:MV 10 as its text and 6.4.3's say.
        let status = ssw::TP | trace_bit(trace_pending) | ssw::RW | operand_bits(&fault);
        let frame = frame.word(transfer_count(0)).word(status);
        self.fault_frame(top.wrapping_sub(faulted.bytes()), &frame, fault.vector);
    }

    /// Stacks a format $C `frame` to end at `top` and vectors through
    /// `vector`; a fault in either is a double bus fault (CPU32RM
    /// 6.2.2).  Until the handler's first instruction is fetched, a
    /// bus or address error is one too (MC68331UM 4.5.5.2).
    fn fault_frame(&mut self, top: u32, frame: &Frame, vector: Vector) {
        let taken = self
            .stack(top, frame)
            .and_then(|bottom| Ok((bottom, self.vector(vector)?)));
        match taken {
            Ok((bottom, handler)) => {
                self.entered(vector, frame, bottom, handler, None);
                self.cpu.fault_window = true;
            }
            Err(_) => self.double_fault(),
        }
    }

    /// Writes `frame` to end at `top` in supervisor data space, a word
    /// a cycle from the highest address down, as TRAP pushes (CPU32RM
    /// 4.4, TRAP).  The frame's lowest address, the new SSP.
    pub(crate) fn stack(&mut self, top: u32, frame: &Frame) -> Result<u32, Fault> {
        let bottom = top.wrapping_sub(frame.bytes());
        self.set_stacking(true);
        let stacked = frame
            .words()
            .iter()
            .enumerate()
            .rev()
            .try_for_each(|(index, word)| {
                self.access_write(
                    Address(bottom).offset(2 * index as u32),
                    Size::Word,
                    FunctionCode::SUPERVISOR_DATA,
                    u32::from(*word),
                )
            });
        self.set_stacking(false);
        stacked.map(|()| bottom)
    }

    /// The handler address in `vector`, read from VBR plus its offset
    /// in supervisor data space (CPU32RM 6.1.1).
    pub(crate) fn vector(&mut self, vector: Vector) -> Result<Address, Fault> {
        let at = Address(self.cpu.registers.vbr).offset(vector.offset());
        self.access_read(
            at,
            Size::Long,
            FunctionCode::SUPERVISOR_DATA,
            CycleKind::Read,
        )
        .map(Address)
    }

    /// Halts the CPU in a double bus fault (CPU32RM 6.2.2).
    pub(crate) const fn double_fault(&mut self) {
        self.cpu.halt = Some(Halt::DoubleBusFault);
    }
}

#[cfg(test)]
mod tests {
    use super::Exception;
    use super::Vector;
    use super::format;
    use crate::address::Address;
    use crate::interrupt::InterruptLevel;

    #[test]
    fn vectors_by_number() {
        assert_eq!(Vector::autovector(InterruptLevel::NONMASKABLE), Vector(31));
        assert_eq!(Vector::trap(15), Vector(47));
        assert_eq!(
            format::vector(format::word(format::SIX_WORD, Vector(47))),
            Vector(47)
        );
    }

    #[test]
    fn group_3_is_before_execution() {
        let short = |vector| Exception::Short {
            vector,
            pc: Address(0),
        };
        assert!(short(Vector::LINE_F).before_execution());
        assert!(!short(Vector::TRAP).before_execution());
    }

    #[test]
    fn only_faults_take_rm() {
        let short = Exception::Short {
            vector: Vector::TRAP,
            pc: Address(0),
        };
        assert_eq!(short.in_read_modify_write(), short);
    }
}
