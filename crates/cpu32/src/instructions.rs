//! The instruction set: a handler for each of the 65536 opcodes, built
//! once.  Each family registers its opcodes from its own module, so
//! families are written apart; claiming an opcode twice is a bug the
//! tests catch.

/// `$body` with `$name` a const of the effective address kind
/// `$value` holds, one of `$kind`s (`effective_address::kind`): a
/// handler specialized by kind, picked when the table is built.  Any
/// other kind gives the illegal instruction handler.
macro_rules! specialize {
    ($value:expr, $name:ident in [$($kind:ident),+ $(,)?] => $body:expr) => {
        match $value {
            $(
                $crate::effective_address::kind::$kind => {
                    const $name: u8 = $crate::effective_address::kind::$kind;
                    $body
                }
            )+
            _ => $crate::instructions::illegal,
        }
    };
}

/// `$body` with `$name` a const of the `Size::index` of `$value`, an
/// `Option<Size>`: a handler specialized by size.  No size gives the
/// illegal instruction handler.
macro_rules! specialize_size {
    ($value:expr, $name:ident => $body:expr) => {
        match $value {
            Some($crate::size::Size::Byte) => {
                const $name: u8 = $crate::size::Size::Byte.index();
                $body
            }
            Some($crate::size::Size::Word) => {
                const $name: u8 = $crate::size::Size::Word.index();
                $body
            }
            Some($crate::size::Size::Long) => {
                const $name: u8 = $crate::size::Size::Long.index();
                $body
            }
            None => $crate::instructions::illegal,
        }
    };
}

/// Every kind a source operand can be: all but none.
macro_rules! specialize_source {
    ($value:expr, $name:ident => $body:expr) => {
        specialize!($value, $name in [
            DATA_REGISTER, ADDRESS_REGISTER, INDIRECT, POSTINCREMENT, PREDECREMENT,
            DISPLACEMENT, INDEX, ABSOLUTE_SHORT, ABSOLUTE_LONG, PC_DISPLACEMENT,
            PC_INDEX, IMMEDIATE,
        ] => $body)
    };
}

/// Every data alterable kind (CPU32RM Table 3-1): a destination.
macro_rules! specialize_data_alterable {
    ($value:expr, $name:ident => $body:expr) => {
        specialize!($value, $name in [
            DATA_REGISTER, INDIRECT, POSTINCREMENT, PREDECREMENT, DISPLACEMENT, INDEX,
            ABSOLUTE_SHORT, ABSOLUTE_LONG,
        ] => $body)
    };
}

/// Every alterable kind: the data alterable ones and An.
macro_rules! specialize_alterable {
    ($value:expr, $name:ident => $body:expr) => {
        specialize!($value, $name in [
            DATA_REGISTER, ADDRESS_REGISTER, INDIRECT, POSTINCREMENT, PREDECREMENT,
            DISPLACEMENT, INDEX, ABSOLUTE_SHORT, ABSOLUTE_LONG,
        ] => $body)
    };
}

/// Every memory alterable kind (CPU32RM Table 3-1).
macro_rules! specialize_memory_alterable {
    ($value:expr, $name:ident => $body:expr) => {
        specialize!($value, $name in [
            INDIRECT, POSTINCREMENT, PREDECREMENT, DISPLACEMENT, INDEX, ABSOLUTE_SHORT,
            ABSOLUTE_LONG,
        ] => $body)
    };
}

/// Every data kind (CPU32RM Table 3-1): all but An and none.
macro_rules! specialize_data {
    ($value:expr, $name:ident => $body:expr) => {
        specialize!($value, $name in [
            DATA_REGISTER, INDIRECT, POSTINCREMENT, PREDECREMENT, DISPLACEMENT, INDEX,
            ABSOLUTE_SHORT, ABSOLUTE_LONG, PC_DISPLACEMENT, PC_INDEX, IMMEDIATE,
        ] => $body)
    };
}

/// Every control kind (CPU32RM Table 3-1).
macro_rules! specialize_control {
    ($value:expr, $name:ident => $body:expr) => {
        specialize!($value, $name in [
            INDIRECT, DISPLACEMENT, INDEX, ABSOLUTE_SHORT, ABSOLUTE_LONG, PC_DISPLACEMENT,
            PC_INDEX,
        ] => $body)
    };
}

/// `$body` with `$name` a const of the two-bit field `$value`: a
/// handler specialized by an operation field, such as a shift's type.
macro_rules! specialize_two_bits {
    ($value:expr, $name:ident => $body:expr) => {
        match $value & 0b11 {
            0 => {
                const $name: u16 = 0;
                $body
            }
            1 => {
                const $name: u16 = 1;
                $body
            }
            2 => {
                const $name: u16 = 2;
                $body
            }
            _ => {
                const $name: u16 = 3;
                $body
            }
        }
    };
}

/// `$body` with `$name` a const of the condition `$value`: a handler
/// specialized by a one-bit field.
macro_rules! specialize_flag {
    ($value:expr, $name:ident => $body:expr) => {
        if $value {
            const $name: bool = true;
            $body
        } else {
            const $name: bool = false;
            $body
        }
    };
}

mod arithmetic;
mod bcd;
mod bit;
mod bounds;
mod branch;
mod control;
mod data_movement;
mod exception;
mod logic;
mod multiply_divide;
mod shift;
mod system;
mod table_lookup;

use std::sync::OnceLock;

use crate::cpu::Core;
use crate::exception::Exception;
use crate::exception::Vector;

/// What an instruction leaves the CPU to do next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Flow {
    /// The next instruction.
    Next,
    /// The instruction changed the flow of the program: a taken
    /// branch, a jump, a call, a return, or a write of SR.  Trace on
    /// change of flow (T0) traces it (CPU32RM 6.2.10).
    Changed,
    /// Stop until an interrupt: `stop`, `lpstop`.
    Stop,
}

/// One opcode's handler: it fetches its extension words, does the
/// instruction and says what follows, or raises an exception.
pub(crate) type Handler = fn(&mut Core<'_>, u16) -> Result<Flow, Exception>;

/// Opcodes: every 16-bit word.
pub(crate) const OPCODES: usize = 1 << u16::BITS;

/// The opcodes' handlers as the families register them.
pub(crate) struct Table {
    /// The handler of each opcode.
    handlers: Vec<Handler>,
    /// Whether a family has claimed each opcode.
    claimed: Vec<bool>,
}

impl Table {
    /// Opcodes $A000-$AFFF: line A (CPU32RM 6.2.8).
    const LINE_A: u16 = 0xa000;
    /// Opcodes $F000-$FFFF: line F, where the CPU32's own LPSTOP and
    /// TBL also sit.
    const LINE_F: u16 = 0xf000;
    /// The opcode bits that pick a line.
    const LINE_MASK: u16 = 0xf000;

    /// A table with no family registered: illegal instructions, and
    /// the two emulator lines.
    fn new() -> Self {
        let handlers = (0..OPCODES)
            .map(|opcode| match opcode as u16 & Self::LINE_MASK {
                Self::LINE_A => line_a as Handler,
                Self::LINE_F => line_f,
                _ => illegal,
            })
            .collect();
        Self {
            handlers,
            claimed: vec![false; OPCODES],
        }
    }

    /// Sets `handler` for every opcode with `opcode & mask == pattern`
    /// that `admits` admits.
    ///
    /// # Panics
    ///
    /// If another family already claimed one of them.
    pub(crate) fn set(
        &mut self,
        mask: u16,
        pattern: u16,
        admits: impl Fn(u16) -> bool,
        handler: Handler,
    ) {
        self.set_by(mask, pattern, admits, |_| handler);
    }

    /// Sets `handler(opcode)` for every opcode with `opcode & mask ==
    /// pattern` that `admits` admits: a family's handlers specialized
    /// by its opcodes' fields.
    ///
    /// # Panics
    ///
    /// If another family already claimed one of them.
    pub(crate) fn set_by(
        &mut self,
        mask: u16,
        pattern: u16,
        admits: impl Fn(u16) -> bool,
        handler: impl Fn(u16) -> Handler,
    ) {
        // Only the opcodes `mask` leaves free bits in: `free_bits` steps
        // through every subset of them, from zero back to zero.
        let free = !mask;
        let mut free_bits = 0_u16;
        loop {
            let opcode = pattern | free_bits;
            if opcode & mask == pattern && admits(opcode) {
                let index = usize::from(opcode);
                assert!(
                    !self.claimed[index],
                    "opcode {opcode:#06x} registered twice"
                );
                self.claimed[index] = true;
                self.handlers[index] = handler(opcode);
            }
            free_bits = free_bits.wrapping_sub(free) & free;
            if free_bits == 0 {
                break;
            }
        }
    }
}

/// Every opcode's handler, by opcode: an array, so an opcode indexes it
/// unchecked.
pub(crate) fn table() -> &'static [Handler; OPCODES] {
    static TABLE: OnceLock<Box<[Handler; OPCODES]>> = OnceLock::new();
    TABLE.get_or_init(|| {
        build()
            .handlers
            .into_boxed_slice()
            .try_into()
            .expect("a handler for every opcode")
    })
}

/// The table with every family registered.
fn build() -> Table {
    let mut table = Table::new();
    bcd::register(&mut table);
    bit::register(&mut table);
    bounds::register(&mut table);
    branch::register(&mut table);
    control::register(&mut table);
    arithmetic::register(&mut table);
    data_movement::register(&mut table);
    logic::register(&mut table);
    exception::register(&mut table);
    multiply_divide::register(&mut table);
    shift::register(&mut table);
    system::register(&mut table);
    table_lookup::register(&mut table);
    table
}

/// Any opcode no family claims: an illegal instruction (CPU32RM 6.2.8).
fn illegal(core: &mut Core<'_>, _opcode: u16) -> Result<Flow, Exception> {
    Err(core.illegal())
}

/// Line 1010 (CPU32RM 6.2.8): its own address stacked.
fn line_a(core: &mut Core<'_>, _opcode: u16) -> Result<Flow, Exception> {
    Err(Exception::Short {
        vector: Vector::LINE_A,
        pc: core.instruction_pc(),
    })
}

/// Line 1111 (CPU32RM 6.2.8): its own address stacked.
fn line_f(core: &mut Core<'_>, _opcode: u16) -> Result<Flow, Exception> {
    Err(Exception::Short {
        vector: Vector::LINE_F,
        pc: core.instruction_pc(),
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::build;
    use super::table;
    use crate::address::Address;
    use crate::bus::Bus;
    use crate::bus::BusAccess;
    use crate::bus::BusError;
    use crate::bus::PortSize;
    use crate::bus::Read;
    use crate::cpu::Cpu;
    use crate::cpu::Run;
    use crate::cycles::Cycles;
    use crate::interrupt::Acknowledge;
    use crate::interrupt::Acknowledged;
    use crate::interrupt::InterruptLevel;
    use crate::interrupt::Interrupts;
    use crate::memory::Backing;
    use crate::memory::Memory;
    use crate::memory::PAGE_BYTES;
    use crate::memory::Page;
    use crate::memory::Space;
    use crate::registers::status;
    use crate::size::Size;
    use crate::timing;

    /// Where [`Machine`] loads code.
    pub(crate) const CODE: u32 = 0x1000;
    /// Bytes of RAM, in both spaces, from zero.
    pub(crate) const RAM_BYTES: u32 = 16 * PAGE_BYTES;

    /// A bus with nothing on it.
    #[derive(Debug)]
    pub(crate) struct NoBus;

    impl Bus for NoBus {
        fn read(&mut self, _access: BusAccess) -> Result<Read, BusError> {
            Err(BusError)
        }

        fn write(&mut self, _access: BusAccess, _value: u32) -> Result<Cycles, BusError> {
            Err(BusError)
        }
    }

    /// No interrupt sources.
    #[derive(Debug)]
    pub(crate) struct NoInterrupts;

    impl Interrupts for NoInterrupts {
        fn level(&self) -> InterruptLevel {
            InterruptLevel::NONE
        }

        fn acknowledge(&mut self, _level: InterruptLevel, _now: Cycles) -> Acknowledged {
            Acknowledge::Spurious.into()
        }
    }

    /// A CPU in supervisor state on RAM from zero, for instruction
    /// tests.
    pub(crate) fn machine(code: &[u16]) -> Cpu {
        machine_with_clocks(code, 2)
    }

    /// [`machine`] on RAM whose bus cycles take `clocks`.
    pub(crate) fn machine_with_clocks(code: &[u16], clocks: u8) -> Cpu {
        let mut memory = Memory::new(24).unwrap();
        let region = memory.add_region(vec![0; RAM_BYTES as usize]);
        let backing = Some(Backing { region, offset: 0 });
        let page = Page {
            read: backing,
            write: backing,
            port: PortSize::Bits16,
            clocks,
        };
        for space in [Space::User, Space::Supervisor] {
            memory.map(space, Address(0), RAM_BYTES, page).unwrap();
        }
        let bytes = memory.region_mut(region);
        for (index, word) in code.iter().enumerate() {
            let at = CODE as usize + 2 * index;
            bytes[at..at + 2].copy_from_slice(&word.to_be_bytes());
        }
        let mut cpu = Cpu::new(memory);
        cpu.registers.set_sr(status::S | status::INTERRUPT_MASK);
        cpu.registers.pc = CODE;
        cpu
    }

    /// The long word at `at` in supervisor space.
    pub(crate) fn peek_long(cpu: &Cpu, at: u32) -> u32 {
        let word = |at| {
            cpu.memory
                .read(Space::Supervisor, Address(at), Size::Word)
                .expect("RAM")
                .0
        };
        word(at) << 16 | word(at + 2)
    }

    /// Writes the long word at `at` in supervisor space.
    pub(crate) fn poke_long(cpu: &mut Cpu, at: u32, value: u32) {
        for (offset, word) in [(0, value >> 16), (2, value & 0xffff)] {
            cpu.memory
                .write(Space::Supervisor, Address(at + offset), Size::Word, word)
                .expect("RAM");
        }
    }

    /// Runs `instructions` instructions, one at a time: a budget of
    /// one cycle runs exactly one.  The last run's outcome.
    pub(crate) fn step(cpu: &mut Cpu, instructions: usize) -> Run {
        let mut run = None;
        for _ in 0..instructions {
            run = Some(cpu.run_for(&mut NoBus, &mut NoInterrupts, Cycles::ZERO, Cycles(1)));
        }
        run.expect("at least one instruction")
    }

    #[test]
    fn the_table_builds_with_no_opcode_claimed_twice() {
        assert_eq!(table().len(), 1 << 16);
    }

    #[test]
    fn the_handlers_and_the_timing_decoder_agree_on_every_opcode() {
        let claimed = build().claimed;
        let mismatches: Vec<String> = (0..=u16::MAX)
            .filter(|&opcode| claimed[usize::from(opcode)] != timing::is_instruction(opcode))
            .map(|opcode| {
                let side = if claimed[usize::from(opcode)] {
                    "handler only"
                } else {
                    "timing only"
                };
                format!("{opcode:#06x} {side}")
            })
            .collect();
        assert!(
            mismatches.is_empty(),
            "{} mismatches: {:?}",
            mismatches.len(),
            mismatches
        );
    }
}
