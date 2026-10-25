//! Exception-related instructions (CPU32RM 4.4; 6.2.4, 6.2.5, 6.2.8,
//! 6.2.12): RTE, TRAP, TRAPV, TRAPcc, ILLEGAL and BKPT.

use crate::address::Address;
use crate::cpu::Core;
use crate::cycles::Cycles;
use crate::exception::CycleKind;
use crate::exception::Exception;
use crate::exception::Fault;
use crate::exception::MICROCODE_REVISION;
use crate::exception::MOVEM_TRANSFERS;
use crate::exception::Movem;
use crate::exception::MovemResume;
use crate::exception::Vector;
use crate::exception::format;
use crate::exception::frame;
use crate::exception::short_frame;
use crate::exception::ssw;
use crate::flags;
use crate::function_code::FunctionCode;
use crate::instructions::Flow;
use crate::instructions::Table;
use crate::instructions::table;
use crate::size::Size;
use crate::timing;

/// RTE (CPU32RM 4.4, RTE).
const RTE: u16 = 0x4e73;
/// TRAP: 0100 1110 0100 vvvv (CPU32RM 4.4, TRAP).
const TRAP_MASK: u16 = 0xfff0;
/// TRAP's fixed bits.
const TRAP: u16 = 0x4e40;
/// TRAP's vector field.
const TRAP_NUMBER: u16 = 0x000f;
/// TRAPV (CPU32RM 4.4, TRAPV).
const TRAPV: u16 = 0x4e76;
/// ILLEGAL, illegal on every M68000 Family member (CPU32RM 6.2.8).
const ILLEGAL: u16 = 0x4afc;
/// BKPT: 0100 1000 0100 1vvv (CPU32RM 4.4, BKPT).
const BKPT_MASK: u16 = 0xfff8;
/// BKPT's fixed bits.
const BKPT: u16 = 0x4848;
/// BKPT's breakpoint number.
const BKPT_NUMBER: u16 = 0b111;
/// Breakpoint acknowledges one BKPT runs back to back, each returning
/// another BKPT, before it ends at an instruction boundary: a bound
/// of ours, so a responder that always returns BKPT cannot hold the
/// run inside one instruction.
const BKPT_CHAIN: u32 = 16;
/// Where a breakpoint acknowledge cycle's address carries the
/// breakpoint number, A\[4:2\]; A1, T, is clear for a software
/// breakpoint, and A\[19:16\], the CPU space type, is 0000 (CPU32RM
/// 5.3.1.1; MC68331UM 4.5.4.1.1).
const BREAKPOINT_NUMBER_SHIFT: u32 = 2;
/// TRAPcc: 0101 cccc 1111 1ooo (CPU32RM 4.4, TRAPcc).
const TRAPCC_MASK: u16 = 0xf0f8;
/// TRAPcc's fixed bits.
const TRAPCC: u16 = 0x50f8;
/// Where TRAPcc's condition starts.
const CONDITION_SHIFT: u16 = 8;
/// TRAPcc's opmode field.
const OPMODE: u16 = 0b111;

/// TRAPcc's opmodes.
mod opmode {
    /// A word operand follows.
    pub(super) const WORD: u16 = 0b010;
    /// A long operand follows.
    pub(super) const LONG: u16 = 0b011;
    /// No operand.
    pub(super) const NONE: u16 = 0b100;
}

/// Registers the family's opcodes.
pub(super) fn register(table: &mut Table) {
    table.set(u16::MAX, RTE, |_| true, rte);
    table.set(TRAP_MASK, TRAP, |_| true, trap);
    table.set(u16::MAX, TRAPV, |_| true, trapv);
    table.set(u16::MAX, ILLEGAL, |_| true, illegal);
    table.set(BKPT_MASK, BKPT, |_| true, bkpt);
    table.set(
        TRAPCC_MASK,
        TRAPCC,
        |opcode| matches!(opcode & OPMODE, opmode::WORD | opmode::LONG | opmode::NONE),
        trapcc,
    );
}

/// The six-word TRAPcc and TRAPV exception: the next PC and the
/// instruction's own (CPU32RM 6.2.4, 6.4.2).
fn instruction_trap(core: &Core<'_>) -> Exception {
    Exception::Long {
        vector: Vector::TRAPCC,
        pc: Address(core.cpu.registers.pc),
        address: core.instruction_pc(),
    }
}

/// The format error: a four-word frame below the bad one, stacking
/// RTE's own address (CPU32RM 6.2.7, 6.2.12).
const fn format_error(core: &Core<'_>) -> Exception {
    Exception::Short {
        vector: Vector::FORMAT_ERROR,
        pc: core.instruction_pc(),
    }
}

/// TRAP #n: vector 32 + n, the next PC stacked (CPU32RM 6.2.4).
fn trap(core: &mut Core<'_>, opcode: u16) -> Result<Flow, Exception> {
    Err(Exception::Short {
        vector: Vector::trap((opcode & TRAP_NUMBER) as u8),
        pc: Address(core.cpu.registers.pc),
    })
}

/// TRAPV: a trap if V is set.
fn trapv(core: &mut Core<'_>, _opcode: u16) -> Result<Flow, Exception> {
    if core.cpu.registers.flag(crate::registers::status::V) {
        Err(instruction_trap(core))
    } else {
        Ok(Flow::Next)
    }
}

/// TRAPcc, TRAPcc.W #data, TRAPcc.L #data: a trap if the condition
/// holds, the operand words fetched and left to the handler.
fn trapcc(core: &mut Core<'_>, opcode: u16) -> Result<Flow, Exception> {
    match opcode & OPMODE {
        opmode::WORD => {
            core.fetch_word()?;
        }
        opmode::LONG => {
            core.fetch_long()?;
        }
        _ => {}
    }
    if flags::test(&core.cpu.registers, opcode >> CONDITION_SHIFT) {
        Err(instruction_trap(core))
    } else {
        Ok(Flow::Next)
    }
}

/// ILLEGAL: the illegal instruction exception.
fn illegal(core: &mut Core<'_>, _opcode: u16) -> Result<Flow, Exception> {
    Err(core.illegal())
}

/// BKPT #n: a breakpoint acknowledge read in CPU space.  The word it
/// returns executes in BKPT's place; a bus error ends it in the
/// illegal instruction exception (CPU32RM 6.2.5; MC68331UM 4.5.4.1.1).
/// A returned BKPT runs another acknowledge, up to [`BKPT_CHAIN`] of
/// them; then BKPT ends with PC at itself, an instruction boundary,
/// and runs again from memory.  Each acknowledge costs BKPT's row and
/// the word returned its own (CPU32RM 8.3.13).
fn bkpt(core: &mut Core<'_>, opcode: u16) -> Result<Flow, Exception> {
    let acknowledge_cost = Cycles(u64::from(timing::exception::BKPT.cycles));
    let mut number = opcode & BKPT_NUMBER;
    for acknowledge in 0..BKPT_CHAIN {
        if acknowledge > 0 {
            // The table charged the first.
            core.charge(acknowledge_cost);
        }
        let address = Address(u32::from(number) << BREAKPOINT_NUMBER_SHIFT);
        let Ok(word) = core.read(address, Size::Word, FunctionCode::CPU_SPACE) else {
            return Err(core.illegal());
        };
        let replacement = word as u16;
        if replacement & BKPT_MASK != BKPT {
            return replace(core, replacement, acknowledge_cost);
        }
        number = replacement & BKPT_NUMBER;
    }
    core.cpu.registers.pc = core.instruction_pc().0;
    Ok(Flow::Changed)
}

/// Executes `replacement`, the word a breakpoint acknowledge returned,
/// charged its own table entry beyond BKPT's.  If it raises an
/// exception, the run loop charges that as `replacement`'s, in place
/// of its entry, so the acknowledge is charged here.
fn replace(
    core: &mut Core<'_>,
    replacement: u16,
    acknowledge_cost: Cycles,
) -> Result<Flow, Exception> {
    let entry = Cycles(u64::from(timing::table()[usize::from(replacement)].cycles));
    match table()[usize::from(replacement)](core, replacement) {
        Ok(flow) => {
            core.charge_replacement(entry);
            Ok(flow)
        }
        Err(exception) => {
            core.charge(acknowledge_cost);
            core.cpu.replacement = Some(replacement);
            Err(exception)
        }
    }
}

/// A word of the frame at `sp`, at `offset`.
fn frame_word(core: &mut Core<'_>, sp: Address, offset: u32) -> Result<u16, Exception> {
    core.read(sp.offset(offset), Size::Word, FunctionCode::SUPERVISOR_DATA)
        .map(|word| word as u16)
}

/// RTE (CPU32RM 6.2.12): the frame at SSP by its format.  Four and six
/// words restore SR and PC; a format $C frame does what its SSW says;
/// any other format is a format error.  A bus error reading SR, PC or
/// the format/vector word is taken, and RTE restarted.
fn rte(core: &mut Core<'_>, _opcode: u16) -> Result<Flow, Exception> {
    if !core.cpu.registers.is_supervisor() {
        return Err(core.privilege_violation());
    }
    let sp = Address(core.cpu.registers.ssp);
    let sr = frame_word(core, sp, frame::SR)?;
    let pc = core.read(
        sp.offset(frame::PC),
        Size::Long,
        FunctionCode::SUPERVISOR_DATA,
    )?;
    let format_vector = frame_word(core, sp, frame::FORMAT_VECTOR)?;
    let frame_format = format_vector >> format::SHIFT;
    core.charge(timing::return_from_exception(frame_format as u8));
    match frame_format {
        format::FOUR_WORD => Ok(restore(core, sr, pc, frame::FOUR_WORD_BYTES)),
        format::SIX_WORD => Ok(restore(core, sr, pc, frame::SIX_WORD_BYTES)),
        format::BUS_ERROR => rte_bus_error(core, sp, sr, Address(pc)),
        _ => Err(format_error(core)),
    }
}

/// Deallocates `bytes` of frame and returns to `pc` with `sr`.
fn restore(core: &mut Core<'_>, sr: u16, pc: u32, bytes: u32) -> Flow {
    let registers = &mut core.cpu.registers;
    registers.ssp = registers.ssp.wrapping_add(bytes);
    registers.set_sr(sr);
    registers.pc = pc;
    Flow::Changed
}

/// What a format $C frame holds, by its SSW (CPU32RM 6.3, 6.4.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Layout {
    /// Types I to III (Figures 6-6, 6-7): twelve words.
    Instruction {
        /// Its SSW.
        ssw: u16,
        /// Its transfer count.
        count: u8,
    },
    /// Type IV (Figure 6-8): twelve words from a six-word frame, ten
    /// from a four-word one.
    Exception {
        /// Its SSW.
        ssw: u16,
        /// Whether the faulted frame was six words.
        six_word: bool,
    },
}

/// The size an SSW's SIZ names for a rerun; none for 11, reserved.
const fn rerun_size(status: u16) -> Option<Size> {
    match status >> ssw::SIZ_SHIFT & ssw::SIZ_MASK {
        ssw::SIZ_LONG => Some(Size::Long),
        ssw::SIZ_BYTE => Some(Size::Byte),
        ssw::SIZ_WORD => Some(Size::Word),
        _ => None,
    }
}

/// The layout of the format $C frame at `sp`, checked: the microcode
/// revision must be ours (CPU32RM 6.2.12).  The ten-word type IV frame
/// keeps its transfer count and SSW four bytes lower (6.4.3), where
/// the twelve-word frame keeps its current instruction PC.  It is
/// taken when all of it fits: our revision, TP set and MV clear, and a
/// faulted format/vector of the four-word frame.  Else the twelve-word
/// place is checked: types I to III with TP clear, or type IV with TP
/// set and a faulted six-word frame.  A frame that fits neither is a
/// format error, as is an SSW whose fields cannot be restored: a
/// rerun of reserved size.
fn layout(core: &mut Core<'_>, sp: Address) -> Result<Layout, Exception> {
    let ours = |word: u16| (word >> frame::REVISION_SHIFT) as u8 == MICROCODE_REVISION;
    let type_iv = |status: u16| status & (ssw::TP | ssw::MV) == ssw::TP;
    let faulted = |core: &mut Core<'_>| {
        frame_word(core, sp, frame::FAULTED_FORMAT_VECTOR).map(|word| word >> format::SHIFT)
    };
    let count = frame_word(core, sp, frame::SHORT_TRANSFER_COUNT)?;
    let status = frame_word(core, sp, frame::SHORT_SSW)?;
    if ours(count) && type_iv(status) && faulted(core)? == format::FOUR_WORD {
        return Ok(Layout::Exception {
            ssw: status,
            six_word: false,
        });
    }
    let count = frame_word(core, sp, frame::TRANSFER_COUNT)?;
    let status = frame_word(core, sp, frame::SSW)?;
    if !ours(count) {
        return Err(format_error(core));
    }
    if status & ssw::TP == 0 {
        if status & (ssw::MV | ssw::RR) == ssw::RR && rerun_size(status).is_none() {
            return Err(format_error(core));
        }
        return Ok(Layout::Instruction {
            ssw: status,
            count: count as u8,
        });
    }
    if type_iv(status) && faulted(core)? == format::SIX_WORD {
        Ok(Layout::Exception {
            ssw: status,
            six_word: true,
        })
    } else {
        Err(format_error(core))
    }
}

/// RTE of a format $C frame (CPU32RM 6.2.12, 6.3.2), not itself
/// traced (6.2.10).  A fault loading the rest of the frame, or
/// rewriting a faulted exception's frame, is a double bus fault
/// (6.2.12; MC68331UM 4.5.5.2).  A trace pending in TR is requeued.
fn rte_bus_error(
    core: &mut Core<'_>,
    sp: Address,
    sr: u16,
    return_pc: Address,
) -> Result<Flow, Exception> {
    let layout = layout(core, sp)?;
    core.cpu.untraced = true;
    match restore_bus_error(core, sp, sr, return_pc, layout) {
        Ok(None) => Ok(Flow::Next),
        Ok(Some(trace)) => Err(trace),
        Err(_) => {
            core.double_fault();
            Ok(Flow::Next)
        }
    }
}

/// Restores the format $C frame at `sp` as `layout` says; the trace
/// to requeue, if any.
fn restore_bus_error(
    core: &mut Core<'_>,
    sp: Address,
    sr: u16,
    return_pc: Address,
    layout: Layout,
) -> Result<Option<Exception>, Fault> {
    let load = |core: &mut Core<'_>, offset, size| {
        core.access_read(
            sp.offset(offset),
            size,
            FunctionCode::SUPERVISOR_DATA,
            CycleKind::Read,
        )
    };
    let requeue = |status: u16, pc: Address, address: Address| {
        (status & ssw::TR != 0).then_some(Exception::Long {
            vector: Vector::TRACE,
            pc,
            address,
        })
    };
    match layout {
        Layout::Instruction { ssw: status, count } => {
            let address = Address(load(core, frame::FAULT_ADDRESS, Size::Long)?);
            let data = load(core, frame::DATA_BUFFER, Size::Long)?;
            let current_pc = Address(load(core, frame::CURRENT_PC, Size::Long)?);
            let registers = &mut core.cpu.registers;
            registers.ssp = sp.offset(frame::BUS_ERROR_BYTES).0;
            registers.set_sr(sr);
            registers.pc = return_pc.0;
            if status & ssw::MV != 0 {
                // MOVEM repeats the faulted transfer itself (6.3.2.6).
                core.cpu.movem_resume = Some(MovemResume {
                    pc: return_pc,
                    progress: Movem {
                        attempted: MOVEM_TRANSFERS.saturating_sub(count),
                        next: address,
                    },
                });
            } else if status & ssw::RR != 0 {
                rerun(core, status, address, data);
            }
            Ok(requeue(status, return_pc, current_pc))
        }
        Layout::Exception {
            ssw: status,
            six_word,
        } => {
            let pre_exception_sr = load(core, frame::PRE_EXCEPTION_SR, Size::Word)? as u16;
            let faulted_format_vector =
                load(core, frame::FAULTED_FORMAT_VECTOR, Size::Word)? as u16;
            let (bytes, faulted) = if six_word {
                (
                    frame::BUS_ERROR_BYTES,
                    Some(Address(load(core, frame::CURRENT_PC, Size::Long)?)),
                )
            } else {
                (frame::SHORT_BUS_ERROR_BYTES, None)
            };
            // The faulted frame goes back where it was to go, above
            // this one, and its handler is fetched (6.3.2.7).
            let vector = format::vector(faulted_format_vector);
            let faulted_frame = short_frame(pre_exception_sr, return_pc, vector, faulted);
            let faulted_bytes = if six_word {
                frame::SIX_WORD_BYTES
            } else {
                frame::FOUR_WORD_BYTES
            };
            core.cpu.registers.set_sr(sr);
            let top = sp.offset(bytes + faulted_bytes).0;
            let bottom = core.stack(top, &faulted_frame)?;
            let handler = core.vector(vector)?;
            core.cpu.registers.ssp = bottom;
            core.cpu.registers.pc = handler.0;
            Ok(requeue(status, handler, faulted.unwrap_or(return_pc)))
        }
    }
}

/// Reruns the cycle an SSW with RR names, its RW, FUNC and SIZ, the
/// data from DBUF; a read's data is ignored (CPU32RM 6.3.2.2).  A
/// fault in it is a released write's, taken when RTE ends.
fn rerun(core: &mut Core<'_>, status: u16, address: Address, data: u32) {
    let Some(size) = rerun_size(status) else {
        return;
    };
    let function_code = FunctionCode::new(u32::from(status & ssw::FUNC));
    let result = if status & ssw::RW != 0 {
        core.access_read(address, size, function_code, CycleKind::Read)
            .map(|_| ())
    } else {
        core.access_write(address, size, function_code, data)
    };
    if let Err(fault) = result {
        core.cpu.released.get_or_insert(fault);
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::collections::BTreeSet;

    use super::BKPT;
    use super::BKPT_CHAIN;
    use super::ILLEGAL;
    use super::RTE;
    use super::TRAP;
    use super::TRAPV;
    use crate::address::Address;
    use crate::bus::Bus;
    use crate::bus::BusAccess;
    use crate::bus::BusError;
    use crate::bus::Read;
    use crate::cpu::Cpu;
    use crate::cpu::Halt;
    use crate::cpu::Run;
    use crate::cpu::Stop;
    use crate::cycles::Cycles;
    use crate::exception::MICROCODE_REVISION;
    use crate::exception::Vector;
    use crate::function_code::FunctionCode;
    use crate::instructions::tests::CODE;
    use crate::instructions::tests::NoInterrupts;
    use crate::instructions::tests::machine;
    use crate::instructions::tests::poke_long;
    use crate::memory::Space;
    use crate::registers::status;
    use crate::size::Size;
    use crate::timing;

    /// The supervisor stack's top in the tests.
    const STACK: u32 = 0x4000;
    /// Where vector n's handler is: [`HANDLERS`] plus 16 bytes each.
    const HANDLERS: u32 = 0x2000;
    /// Where the tests' RAM ends; the bus answers from here on.
    const RAM_END: u32 = 0x8000;
    /// NOP.
    const NOP: u16 = 0x4e71;
    /// MOVE.L (A0)+, D0.
    const MOVE_POSTINCREMENT_TO_D0: u16 = 0x2018;
    /// MOVE.L D0, (A0).
    const MOVE_D0_TO_INDIRECT: u16 = 0x2080;
    /// MOVE.L D0, -(A0).
    const MOVE_D0_TO_PREDECREMENT: u16 = 0x2100;
    /// MOVE.W (A0), D0.
    const MOVE_WORD_INDIRECT_TO_D0: u16 = 0x3010;
    /// MOVEQ #5, D0.
    const MOVEQ_5_TO_D0: u16 = 0x7005;
    /// The microcode revision in the transfer count register.
    const REVISION: u16 = (MICROCODE_REVISION as u16) << 8;

    /// A bus with memory where words are put, cycles that bus-error at
    /// chosen addresses, and a breakpoint acknowledge answer.
    #[derive(Debug, Default)]
    struct TestBus {
        /// Its memory, by word address; a cycle anywhere else
        /// bus-errors.
        words: BTreeMap<u32, u16>,
        /// Addresses whose cycles bus-error.
        faults: BTreeSet<u32>,
        /// The answer to a breakpoint acknowledge cycle, if any.
        breakpoint: Option<u16>,
        /// The CPU space cycles seen.
        cpu_space: Vec<BusAccess>,
    }

    impl TestBus {
        /// Zeroed memory from `start` for `bytes`.
        fn memory(start: u32, bytes: u32) -> Self {
            Self {
                words: (start..start + bytes)
                    .step_by(2)
                    .map(|at| (at, 0))
                    .collect(),
                ..Self::default()
            }
        }
    }

    impl Bus for TestBus {
        fn read(&mut self, access: BusAccess) -> Result<Read, BusError> {
            if access.function_code.is_cpu_space() {
                self.cpu_space.push(access);
                let word = self.breakpoint.ok_or(BusError)?;
                return Ok(Read {
                    value: u32::from(word),
                    stall: Cycles::ZERO,
                });
            }
            let at = access.address.0;
            if access.size != Size::Word || self.faults.contains(&at) {
                return Err(BusError);
            }
            let word = self.words.get(&at).ok_or(BusError)?;
            Ok(Read {
                value: u32::from(*word),
                stall: Cycles::ZERO,
            })
        }

        fn write(&mut self, access: BusAccess, value: u32) -> Result<Cycles, BusError> {
            let at = access.address.0;
            if access.size != Size::Word || self.faults.contains(&at) {
                return Err(BusError);
            }
            let word = self.words.get_mut(&at).ok_or(BusError)?;
            *word = value as u16;
            Ok(Cycles::ZERO)
        }
    }

    /// Vector `vector`'s handler.
    const fn handler(vector: Vector) -> u32 {
        HANDLERS + 16 * vector.0 as u32
    }

    /// A machine running `code` at [`CODE`], its stack at [`STACK`] and
    /// each vector pointing at its own handler.
    fn prepared(code: &[u16]) -> Cpu {
        let mut cpu = machine(code);
        cpu.registers.ssp = STACK;
        for vector in 0..=u8::MAX {
            poke_long(&mut cpu, Vector(vector).offset(), handler(Vector(vector)));
        }
        cpu
    }

    /// Writes `words` at `at` in supervisor space.
    fn poke(cpu: &mut Cpu, at: u32, words: &[u16]) {
        for (index, word) in words.iter().enumerate() {
            let address = Address(at + 2 * index as u32);
            cpu.memory
                .write(Space::Supervisor, address, Size::Word, u32::from(*word))
                .expect("RAM");
        }
    }

    /// The `count` words from `at` in supervisor space.
    fn peek(cpu: &Cpu, at: u32, count: u32) -> Vec<u16> {
        (0..count)
            .map(|index| {
                cpu.memory
                    .read(Space::Supervisor, Address(at + 2 * index), Size::Word)
                    .expect("RAM")
                    .0 as u16
            })
            .collect()
    }

    /// One instruction, with the exceptions it raises, on `bus`.
    fn step_on(cpu: &mut Cpu, bus: &mut TestBus) -> Run {
        cpu.run_for(bus, &mut NoInterrupts, Cycles::ZERO, Cycles(1))
    }

    /// One instruction on a bus with nothing on it.
    fn step(cpu: &mut Cpu) -> Run {
        step_on(cpu, &mut TestBus::default())
    }

    /// The high and low words of `long`.
    const fn words(long: u32) -> [u16; 2] {
        [(long >> 16) as u16, long as u16]
    }

    #[test]
    fn trap_stacks_a_four_word_frame_and_the_next_pc() {
        let mut cpu = prepared(&[TRAP | 3]);
        let sr = status::S | status::INTERRUPT_MASK | status::C;
        cpu.registers.set_sr(sr);
        step(&mut cpu);
        // Figure 6-3.
        let [high, low] = words(CODE + 2);
        assert_eq!(peek(&cpu, STACK - 8, 4), [sr, high, low, 0x008c]);
        assert_eq!(cpu.registers.ssp, STACK - 8);
        assert_eq!(cpu.registers.pc, handler(Vector::trap(3)));
        assert_eq!(cpu.registers.sr(), sr);
    }

    #[test]
    fn a_user_trap_enters_supervisor_state_untraced() {
        let mut cpu = prepared(&[TRAP]);
        cpu.registers.usp = 0x3000;
        cpu.registers.set_sr(status::T0 | status::Z);
        step(&mut cpu);
        assert_eq!(cpu.registers.sr(), status::S | status::Z);
        assert_eq!(peek(&cpu, STACK - 8, 1), [status::T0 | status::Z]);
        assert_eq!(cpu.registers.usp, 0x3000);
    }

    #[test]
    fn trapv_traps_only_on_overflow() {
        let mut cpu = prepared(&[TRAPV]);
        step(&mut cpu);
        assert_eq!(cpu.registers.pc, CODE + 2);
        let mut cpu = prepared(&[TRAPV]);
        let sr = status::S | status::V;
        cpu.registers.set_sr(sr);
        step(&mut cpu);
        // Figure 6-4: the next PC, then the instruction's own.
        let [next_high, next_low] = words(CODE + 2);
        let [high, low] = words(CODE);
        assert_eq!(
            peek(&cpu, STACK - 12, 6),
            [sr, next_high, next_low, 0x201c, high, low]
        );
        assert_eq!(cpu.registers.pc, handler(Vector::TRAPCC));
    }

    #[test]
    fn trapcc_fetches_its_operand_and_traps_on_the_condition() {
        // TRAPT.W #$1234: always.
        let mut cpu = prepared(&[0x50fa, 0x1234]);
        step(&mut cpu);
        let [next_high, next_low] = words(CODE + 4);
        let [high, low] = words(CODE);
        assert_eq!(
            peek(&cpu, STACK - 12, 6)[1..],
            [next_high, next_low, 0x201c, high, low]
        );
        // TRAPF.L #data: never.
        let mut cpu = prepared(&[0x51fb, 0, 0]);
        step(&mut cpu);
        assert_eq!(cpu.registers.pc, CODE + 6);
        // TRAPEQ with Z clear.
        let mut cpu = prepared(&[0x57fc]);
        step(&mut cpu);
        assert_eq!(cpu.registers.pc, CODE + 2);
    }

    #[test]
    fn illegal_and_the_emulator_lines_stack_their_own_address() {
        for (opcode, vector) in [
            (ILLEGAL, Vector::ILLEGAL_INSTRUCTION),
            (0xa123, Vector::LINE_A),
            (0xf123, Vector::LINE_F),
        ] {
            let mut cpu = prepared(&[opcode]);
            step(&mut cpu);
            let [high, low] = words(CODE);
            assert_eq!(
                peek(&cpu, STACK - 8, 4)[1..],
                [high, low, vector.offset() as u16]
            );
            assert_eq!(cpu.registers.pc, handler(vector));
        }
    }

    #[test]
    fn rte_in_user_state_is_a_privilege_violation() {
        let mut cpu = prepared(&[RTE]);
        cpu.registers.set_sr(0);
        step(&mut cpu);
        let [high, low] = words(CODE);
        assert_eq!(peek(&cpu, STACK - 8, 4), [0, high, low, 0x0020]);
        assert_eq!(cpu.registers.pc, handler(Vector::PRIVILEGE_VIOLATION));
    }

    #[test]
    fn rte_returns_through_four_and_six_word_frames() {
        for (format_vector, bytes) in [(0x0080, 8), (0x2018, 12)] {
            let mut cpu = prepared(&[RTE]);
            let sp = STACK - bytes;
            cpu.registers.ssp = sp;
            poke(&mut cpu, sp, &[status::N, 0x0000, 0x1234, format_vector]);
            assert_eq!(step(&mut cpu).stop, Stop::Budget);
            assert_eq!(cpu.registers.sr(), status::N);
            assert_eq!(cpu.registers.pc, 0x1234);
            assert_eq!(cpu.registers.ssp, STACK);
        }
    }

    #[test]
    fn trap_and_rte_round_trip() {
        let mut cpu = prepared(&[TRAP | 1, NOP]);
        poke(&mut cpu, handler(Vector::trap(1)), &[RTE]);
        let sr = status::S | status::X;
        cpu.registers.set_sr(sr);
        step(&mut cpu);
        step(&mut cpu);
        assert_eq!(cpu.registers.pc, CODE + 2);
        assert_eq!(cpu.registers.sr(), sr);
        assert_eq!(cpu.registers.ssp, STACK);
    }

    #[test]
    fn rte_of_an_unknown_format_is_a_format_error_below_it() {
        let mut cpu = prepared(&[RTE]);
        let sp = STACK - 8;
        cpu.registers.ssp = sp;
        let frame = [status::S, 0, 0x1234, 0x3080];
        poke(&mut cpu, sp, &frame);
        step(&mut cpu);
        let [high, low] = words(CODE);
        let sr = status::S | status::INTERRUPT_MASK;
        assert_eq!(peek(&cpu, sp - 8, 4), [sr, high, low, 0x0038]);
        assert_eq!(peek(&cpu, sp, 4), frame);
        assert_eq!(cpu.registers.pc, handler(Vector::FORMAT_ERROR));
    }

    #[test]
    fn a_read_fault_restores_postincrement_and_restarts() {
        let mut cpu = prepared(&[MOVE_POSTINCREMENT_TO_D0]);
        poke(&mut cpu, handler(Vector::BUS_ERROR), &[RTE]);
        let fault = 0x0001_0000;
        cpu.registers.set_address(0, fault);
        let sr = status::S | status::INTERRUPT_MASK;
        step(&mut cpu);
        assert_eq!(cpu.registers.address(0), fault);
        // Figure 6-6; SSW per 6.3.1.2: RW, LG, SIZ long, FUNC 5.
        let [code_high, code_low] = words(CODE);
        let [fault_high, fault_low] = words(fault);
        assert_eq!(
            peek(&cpu, STACK - 24, 12),
            [
                sr, code_high, code_low, 0xc008, fault_high, fault_low, 0, 0, code_high, code_low,
                REVISION, 0x0065
            ]
        );
        assert_eq!(cpu.registers.ssp, STACK - 24);
        assert_eq!(cpu.registers.pc, handler(Vector::BUS_ERROR));
        // The handler repairs the fault: the operand is now in RAM.
        cpu.registers.set_address(0, 0x3000);
        poke(&mut cpu, 0x3000, &[0xdead, 0xbeef]);
        step(&mut cpu);
        assert_eq!(cpu.registers.pc, CODE);
        assert_eq!(cpu.registers.ssp, STACK);
        step(&mut cpu);
        assert_eq!(cpu.registers.data[0], 0xdead_beef);
        assert_eq!(cpu.registers.address(0), 0x3004);
    }

    #[test]
    fn a_released_write_fault_completes_the_instruction() {
        let mut cpu = prepared(&[MOVE_D0_TO_PREDECREMENT]);
        let fault = 0x0001_0000;
        cpu.registers.set_address(0, fault + 4);
        cpu.registers.data[0] = 0x8000_0001;
        step(&mut cpu);
        // -(A0) stays done, and N was set before the frame was stacked.
        assert_eq!(cpu.registers.address(0), fault);
        let sr = status::S | status::INTERRUPT_MASK | status::N;
        // Figure 6-6; SSW per 6.3.1.1: RR, LG, SIZ long, FUNC 5.
        let [next_high, next_low] = words(CODE + 2);
        let [code_high, code_low] = words(CODE);
        let [fault_high, fault_low] = words(fault);
        assert_eq!(
            peek(&cpu, STACK - 24, 12),
            [
                sr, next_high, next_low, 0xc008, fault_high, fault_low, 0x8000, 0x0001, code_high,
                code_low, REVISION, 0x0225
            ]
        );
    }

    #[test]
    fn a_fault_on_a_long_writes_second_word_leaves_a_word() {
        let mut cpu = prepared(&[MOVE_D0_TO_INDIRECT]);
        cpu.registers.set_address(0, RAM_END - 2);
        cpu.registers.data[0] = 0x1234_5678;
        step(&mut cpu);
        assert_eq!(peek(&cpu, RAM_END - 2, 1), [0x1234]);
        let frame = peek(&cpu, STACK - 24, 12);
        assert_eq!(frame[4..6], words(RAM_END));
        // RR, LG, SIZ word, FUNC 5.
        assert_eq!(frame[11], 0x0235);
    }

    #[test]
    fn rte_reruns_a_released_write_unless_rr_is_cleared() {
        for rerun in [true, false] {
            let mut cpu = prepared(&[MOVE_D0_TO_INDIRECT, NOP]);
            poke(&mut cpu, handler(Vector::BUS_ERROR), &[RTE]);
            cpu.registers.set_address(0, 0x0001_0000);
            cpu.registers.data[0] = 0xcafe_f00d;
            step(&mut cpu);
            // The handler points the write at RAM, or completes it.
            let sp = STACK - 24;
            poke(&mut cpu, sp + 8, &words(0x3000));
            if !rerun {
                let ssw = peek(&cpu, sp + 0x16, 1)[0];
                poke(&mut cpu, sp + 0x16, &[ssw & !0x0200]);
            }
            step(&mut cpu);
            assert_eq!(cpu.registers.pc, CODE + 2);
            assert_eq!(cpu.registers.ssp, STACK);
            let expected = if rerun { [0xcafe, 0xf00d] } else { [0, 0] };
            assert_eq!(peek(&cpu, 0x3000, 2), expected);
        }
    }

    #[test]
    fn rte_of_another_revision_is_a_format_error() {
        let mut cpu = prepared(&[RTE]);
        let sp = STACK - 24;
        cpu.registers.ssp = sp;
        poke(&mut cpu, sp, &[status::S, 0, 0x1234, 0xc008]);
        poke(&mut cpu, sp + 0x14, &[REVISION + 0x0100, 0]);
        step(&mut cpu);
        assert_eq!(cpu.registers.pc, handler(Vector::FORMAT_ERROR));
        assert_eq!(cpu.registers.ssp, sp - 8);
    }

    #[test]
    fn a_fetch_fault_stacks_a_prefetch() {
        let mut cpu = prepared(&[]);
        let fault = 0x0001_0000;
        cpu.registers.pc = fault;
        step(&mut cpu);
        let frame = peek(&cpu, STACK - 24, 12);
        assert_eq!(frame[1..3], words(fault));
        assert_eq!(frame[4..6], words(fault));
        assert_eq!(frame[8..10], words(fault));
        // SSW: IN, RW, SIZ word, FUNC 6.
        assert_eq!(frame[11], 0x00d6);
    }

    #[test]
    fn an_odd_operand_is_an_address_error() {
        let mut cpu = prepared(&[MOVE_WORD_INDIRECT_TO_D0]);
        cpu.registers.set_address(0, 0x3001);
        step(&mut cpu);
        let frame = peek(&cpu, STACK - 24, 12);
        assert_eq!(frame[3], 0xc00c);
        assert_eq!(frame[4..6], words(0x3001));
        // RW, SIZ word, FUNC 5.
        assert_eq!(frame[11], 0x0055);
        assert_eq!(cpu.registers.pc, handler(Vector::ADDRESS_ERROR));
    }

    #[test]
    fn an_odd_pc_points_at_the_instruction_that_set_it() {
        let mut cpu = prepared(&[RTE]);
        let sp = STACK - 8;
        cpu.registers.ssp = sp;
        poke(&mut cpu, sp, &[status::S, 0, 0x1235, 0x0000]);
        step(&mut cpu);
        step(&mut cpu);
        let frame = peek(&cpu, STACK - 24, 12);
        // Return PC and fault address are the odd address, the current
        // instruction PC the RTE (CPU32RM 6.2.3).
        assert_eq!(frame[1..3], words(0x1235));
        assert_eq!(frame[3], 0xc00c);
        assert_eq!(frame[4..6], words(0x1235));
        assert_eq!(frame[8..10], words(CODE));
        assert_eq!(frame[11], 0x00d6);
    }

    #[test]
    fn a_fault_fetching_the_fault_handler_is_a_double_bus_fault() {
        let mut cpu = prepared(&[MOVE_POSTINCREMENT_TO_D0]);
        cpu.registers.set_address(0, 0x0001_0000);
        poke_long(&mut cpu, Vector::BUS_ERROR.offset(), 0x0002_0000);
        step(&mut cpu);
        assert_eq!(step(&mut cpu).stop, Stop::Halted(Halt::DoubleBusFault));
        assert_eq!(step(&mut cpu).stop, Stop::Halted(Halt::DoubleBusFault));
    }

    #[test]
    fn a_fault_stacking_a_fault_is_a_double_bus_fault() {
        let mut cpu = prepared(&[MOVE_POSTINCREMENT_TO_D0]);
        cpu.registers.set_address(0, 0x0001_0000);
        cpu.registers.ssp = RAM_END + 0x100;
        assert_eq!(step(&mut cpu).stop, Stop::Halted(Halt::DoubleBusFault));
    }

    #[test]
    fn a_handler_fault_after_its_first_fetch_is_no_double_fault() {
        let mut cpu = prepared(&[MOVE_POSTINCREMENT_TO_D0]);
        poke(
            &mut cpu,
            handler(Vector::BUS_ERROR),
            &[MOVE_POSTINCREMENT_TO_D0],
        );
        cpu.registers.set_address(0, 0x0001_0000);
        step(&mut cpu);
        assert_eq!(step(&mut cpu).stop, Stop::Budget);
        assert_eq!(cpu.registers.ssp, STACK - 48);
    }

    #[test]
    fn a_vector_fault_takes_a_ten_word_type_iv_frame() {
        let mut cpu = prepared(&[TRAP]);
        // TRAP #0's vector off the RAM, bus error's in it.
        let vbr = RAM_END - 0x40;
        cpu.registers.vbr = vbr;
        poke_long(
            &mut cpu,
            vbr + Vector::BUS_ERROR.offset(),
            handler(Vector::BUS_ERROR),
        );
        let sr = status::S | status::INTERRUPT_MASK;
        step(&mut cpu);
        let vector_address = vbr + Vector::TRAP.offset();
        // Figure 6-8 without the faulted instruction PC, below the
        // four-word frame's space.  SSW per 6.3.1.4: TP, RW, LG, FUNC 5.
        let [next_high, next_low] = words(CODE + 2);
        let [fault_high, fault_low] = words(vector_address);
        let base = STACK - 8 - 20;
        assert_eq!(
            peek(&cpu, base, 10),
            [
                sr, next_high, next_low, 0xc008, fault_high, fault_low, sr, 0x0080, REVISION,
                0x8065
            ]
        );
        assert_eq!(cpu.registers.ssp, base);
        assert_eq!(cpu.registers.pc, handler(Vector::BUS_ERROR));
        // The handler fixes the table; RTE rebuilds the TRAP frame.
        poke(&mut cpu, handler(Vector::BUS_ERROR), &[RTE]);
        cpu.registers.vbr = 0;
        step(&mut cpu);
        assert_eq!(cpu.registers.ssp, STACK - 8);
        assert_eq!(cpu.registers.pc, handler(Vector::TRAP));
        assert_eq!(peek(&cpu, STACK - 8, 4), [sr, next_high, next_low, 0x0080]);
    }

    #[test]
    fn a_user_sr_above_a_ten_word_frame_is_no_revision() {
        // The faulted four-word frame's SR, $01xx, is at +$14 of the
        // ten-word frame; no SR reads as the revision.
        let mut cpu = prepared(&[TRAP]);
        let vbr = RAM_END - 0x40;
        cpu.registers.vbr = vbr;
        poke_long(
            &mut cpu,
            vbr + Vector::BUS_ERROR.offset(),
            handler(Vector::BUS_ERROR),
        );
        poke(&mut cpu, handler(Vector::BUS_ERROR), &[RTE]);
        cpu.registers.usp = 0x3000;
        let sr = 0x0100 | status::Z;
        cpu.registers.set_sr(sr);
        step(&mut cpu);
        assert_eq!(peek(&cpu, STACK - 8, 1), [sr]);
        cpu.registers.vbr = 0;
        step(&mut cpu);
        assert_eq!(cpu.registers.pc, handler(Vector::TRAP));
        assert_eq!(cpu.registers.ssp, STACK - 8);
    }

    #[test]
    fn a_stale_revision_above_a_ten_word_frame_is_not_read() {
        // A four-word frame faults on its first word, so its space
        // above the ten-word frame keeps what an earlier twelve-word
        // frame left there: our revision and an SSW with TP clear.
        let mut cpu = prepared(&[TRAP]);
        let top = RAM_END + 4;
        cpu.registers.ssp = top;
        let faulted_base = top - 8;
        poke(&mut cpu, faulted_base, &[REVISION, 0]);
        let sr = status::S;
        cpu.registers.set_sr(sr);
        step(&mut cpu);
        let base = faulted_base - 20;
        assert_eq!(cpu.registers.ssp, base);
        assert_eq!(peek(&cpu, base + 0x10, 2), [REVISION, 0x8055]);
        poke(&mut cpu, handler(Vector::BUS_ERROR), &[RTE]);
        let mut bus = TestBus::memory(RAM_END, 0x10);
        step_on(&mut cpu, &mut bus);
        assert_eq!(cpu.registers.pc, handler(Vector::TRAP));
        assert_eq!(cpu.registers.ssp, faulted_base);
        assert_eq!(peek(&cpu, faulted_base, 2), [sr, 0]);
        assert_eq!(bus.words[&RAM_END], CODE as u16 + 2);
        assert_eq!(bus.words[&(RAM_END + 2)], 0x0080);
    }

    #[test]
    fn a_stacking_fault_of_a_six_word_frame_takes_twelve_words() {
        let mut cpu = prepared(&[TRAPV]);
        let sr = status::S | status::V;
        cpu.registers.set_sr(sr);
        let top = RAM_END + 4;
        cpu.registers.ssp = top;
        step(&mut cpu);
        let base = top - 12 - 24;
        let frame = peek(&cpu, base, 12);
        assert_eq!(
            frame[..4],
            [status::S | status::V, 0, CODE as u16 + 2, 0xc008]
        );
        // The first word stacked, the highest, faulted.
        assert_eq!(frame[4..6], words(RAM_END + 2));
        assert_eq!(frame[6..8], [sr, 0x201c]);
        assert_eq!(frame[8..10], words(CODE));
        // TP, RW, SIZ word, FUNC 5.
        assert_eq!(frame[10..], [REVISION, 0x8055]);
        assert_eq!(cpu.registers.ssp, base);
        // RTE rewrites the six-word frame once the stack is good.
        poke(&mut cpu, handler(Vector::BUS_ERROR), &[RTE]);
        let mut bus = TestBus::memory(RAM_END, 0x10);
        step_on(&mut cpu, &mut bus);
        assert_eq!(cpu.registers.ssp, top - 12);
        assert_eq!(cpu.registers.pc, handler(Vector::TRAPCC));
        assert_eq!(peek(&cpu, RAM_END - 8, 4), [sr, 0, CODE as u16 + 2, 0x201c]);
        assert_eq!(bus.words[&(RAM_END + 2)], CODE as u16);
    }

    #[test]
    fn a_fault_loading_a_bus_error_frame_halts() {
        let mut cpu = prepared(&[RTE]);
        let sp = RAM_END;
        cpu.registers.ssp = sp;
        let mut bus = TestBus::memory(RAM_END, 0x18);
        for (offset, word) in [(0, status::S), (6, 0xc008), (0x14, REVISION), (0x16, 0)] {
            bus.words.insert(sp + offset, word);
        }
        bus.faults.insert(sp + 0x0c);
        assert_eq!(
            step_on(&mut cpu, &mut bus).stop,
            Stop::Halted(Halt::DoubleBusFault)
        );
    }

    #[test]
    fn a_fault_reading_a_frame_is_a_bus_error_and_restarts_rte() {
        let mut cpu = prepared(&[RTE]);
        let sp = RAM_END + 0x10;
        cpu.registers.ssp = sp;
        let mut bus = TestBus::memory(RAM_END, 0x20);
        bus.faults.insert(sp + 2);
        step_on(&mut cpu, &mut bus);
        assert_eq!(cpu.registers.pc, handler(Vector::BUS_ERROR));
        // A twelve-word frame below it, half in RAM, half on the bus.
        let base = sp - 24;
        assert_eq!(peek(&cpu, base, 4)[1..3], words(CODE));
        let fault_address = [bus.words[&RAM_END], bus.words[&(RAM_END + 2)]];
        assert_eq!(fault_address, words(sp + 2));
    }

    #[test]
    fn bkpt_executes_the_word_its_acknowledge_returns() {
        let mut cpu = prepared(&[BKPT | 5]);
        let mut bus = TestBus {
            breakpoint: Some(MOVEQ_5_TO_D0),
            ..TestBus::default()
        };
        step_on(&mut cpu, &mut bus);
        assert_eq!(cpu.registers.data[0], 5);
        assert_eq!(cpu.registers.pc, CODE + 2);
        let access = bus.cpu_space[0];
        assert_eq!(access.address, Address(5 << 2));
        assert_eq!(access.function_code, FunctionCode::CPU_SPACE);
        assert_eq!(access.size, Size::Word);
    }

    #[test]
    fn bkpt_with_a_bus_error_is_illegal() {
        let mut cpu = prepared(&[BKPT]);
        let run = step(&mut cpu);
        assert_eq!(cpu.registers.pc, handler(Vector::ILLEGAL_INSTRUCTION));
        assert_eq!(peek(&cpu, STACK - 8, 4)[1..3], words(CODE));
        // CPU32RM 8.3.13, BKPT (Bus Error).
        assert_eq!(run.cycles, cycles(timing::exception::BKPT_BUS_ERROR));
    }

    /// `cost`'s clocks.
    fn cycles(cost: timing::Cost) -> Cycles {
        Cycles(u64::from(cost.cycles))
    }

    #[test]
    fn bkpt_charges_its_acknowledge_and_the_word_returned() {
        let mut cpu = prepared(&[BKPT]);
        let mut bus = TestBus {
            breakpoint: Some(MOVEQ_5_TO_D0),
            ..TestBus::default()
        };
        let run = step_on(&mut cpu, &mut bus);
        let moveq = timing::table()[usize::from(MOVEQ_5_TO_D0)];
        assert_eq!(run.cycles, cycles(timing::exception::BKPT) + cycles(moveq));
    }

    #[test]
    fn an_illegal_word_returned_is_charged_as_illegal_not_as_a_bus_error() {
        let mut cpu = prepared(&[BKPT]);
        let mut bus = TestBus {
            breakpoint: Some(ILLEGAL),
            ..TestBus::default()
        };
        let run = step_on(&mut cpu, &mut bus);
        assert_eq!(cpu.registers.pc, handler(Vector::ILLEGAL_INSTRUCTION));
        assert_eq!(
            run.cycles,
            cycles(timing::exception::BKPT) + cycles(timing::exception::ILLEGAL)
        );
    }

    #[test]
    fn bkpt_answered_by_bkpt_ends_at_a_boundary() {
        let mut cpu = prepared(&[BKPT | 7]);
        let mut bus = TestBus {
            breakpoint: Some(BKPT | 3),
            ..TestBus::default()
        };
        let run = step_on(&mut cpu, &mut bus);
        assert_eq!(cpu.registers.pc, CODE);
        assert_eq!(cpu.exception_taken(), None);
        let chain = BKPT_CHAIN as usize;
        assert_eq!(bus.cpu_space.len(), chain);
        // The first acknowledge is BKPT's own number, the rest the
        // number each BKPT returned carries.
        assert_eq!(bus.cpu_space[0].address, Address(7 << 2));
        assert_eq!(bus.cpu_space[1].address, Address(3 << 2));
        assert_eq!(
            run.cycles,
            Cycles(cycles(timing::exception::BKPT).0 * u64::from(BKPT_CHAIN))
        );
        // Then the word that ends the chain runs in BKPT's place.
        bus.breakpoint = Some(MOVEQ_5_TO_D0);
        step_on(&mut cpu, &mut bus);
        assert_eq!(cpu.registers.data[0], 5);
        assert_eq!(cpu.registers.pc, CODE + 2);
    }
}
