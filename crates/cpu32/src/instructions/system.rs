//! System control instructions (CPU32RM 4.3.9): the status register
//! moves and immediates, MOVE USP, MOVEC, MOVES, RESET, STOP, LPSTOP
//! and BGND.  Every privileged one (CPU32RM 6.2.9) raises a privilege
//! violation in user state, before it executes.
//!
//! LPSTOP shares its first word, $F800, with TBLS and TBLU of D0
//! (CPU32RM 4.4, LPSTOP, TBLS); only the second word tells them apart.
//! The table lookup family claims $F800 and hands a second word of
//! [`LPSTOP_EXTENSION`] to [`lpstop`].

use crate::bus::LPSTOP_BROADCAST;
use crate::cpu::Core;
use crate::effective_address::Category;
use crate::effective_address::Operand;
use crate::effective_address::THREE_BITS;
use crate::exception::Exception;
use crate::function_code::FunctionCode;
use crate::instructions::Flow;
use crate::instructions::Table;
use crate::instructions::data_movement::low_register;
use crate::registers::status;
use crate::size::Size;
use crate::timing;

/// RESET (CPU32RM 4.4, RESET).
const RESET: u16 = 0x4e70;
/// STOP #data (CPU32RM 4.4, STOP).
const STOP: u16 = 0x4e72;
/// MOVEC Rc, Rn: dr = 0 (CPU32RM 4.4, MOVEC).
const MOVEC_TO_GENERAL: u16 = 0x4e7a;
/// MOVEC Rn, Rc: dr = 1.
const MOVEC_TO_CONTROL: u16 = 0x4e7b;
/// MOVE USP: 0100 1110 0110 drrr (CPU32RM 4.4, MOVE USP).
const MOVE_USP_MASK: u16 = 0xfff0;
/// MOVE USP's fixed bits.
const MOVE_USP: u16 = 0x4e60;
/// MOVE USP's dr: USP to An, else An to USP.
const USP_TO_ADDRESS: u16 = 1 << 3;
/// ORI to CCR (CPU32RM 4.4, ORI to CCR).
const ORI_TO_CCR: u16 = 0x003c;
/// ORI to SR (CPU32RM 4.4, ORI to SR).
const ORI_TO_SR: u16 = 0x007c;
/// ANDI to CCR (CPU32RM 4.4, ANDI to CCR).
const ANDI_TO_CCR: u16 = 0x023c;
/// ANDI to SR (CPU32RM 4.4, ANDI to SR).
const ANDI_TO_SR: u16 = 0x027c;
/// EORI to CCR (CPU32RM 4.4, EORI to CCR).
const EORI_TO_CCR: u16 = 0x0a3c;
/// EORI to SR (CPU32RM 4.4, EORI to SR).
const EORI_TO_SR: u16 = 0x0a7c;
/// Bit 6 of an immediate-to-status opcode: SR, else CCR.
const TO_SR: u16 = 1 << 6;
/// The status register moves: 0100 0ss0 11, then an effective address
/// (CPU32RM 4.4, MOVE from SR, MOVE from CCR, MOVE to CCR, MOVE to SR).
const MOVE_STATUS_MASK: u16 = 0xffc0;
/// MOVE from SR.
const MOVE_FROM_SR: u16 = 0x40c0;
/// MOVE from CCR.
const MOVE_FROM_CCR: u16 = 0x42c0;
/// MOVE to CCR.
const MOVE_TO_CCR: u16 = 0x44c0;
/// MOVE to SR.
const MOVE_TO_SR: u16 = 0x46c0;
/// MOVES: 0000 1110 ss, then a memory alterable address (CPU32RM 4.4,
/// MOVES).
const MOVES_MASK: u16 = 0xff00;
/// MOVES's fixed bits.
const MOVES: u16 = 0x0e00;
/// Where MOVES's size field, bits 7:6, starts.
const MOVES_SIZE_SHIFT: u16 = 6;
/// BGND (CPU32RM 4.4, BGND).
const BGND: u16 = 0x4afa;
/// LPSTOP's first word (CPU32RM 4.4, LPSTOP).
pub(super) const LPSTOP: u16 = 0xf800;
/// LPSTOP's second word.
pub(super) const LPSTOP_EXTENSION: u16 = 0x01c0;
/// The bits of a six-bit effective address field.
const FIELD_MASK: u16 = 0x3f;

/// MOVEC's and MOVES's second word (CPU32RM 4.4, MOVEC, MOVES).
mod extension {
    /// A/D: the general register is an address register.
    pub(super) const ADDRESS_REGISTER: u16 = 1 << 15;
    /// Where the general register's number starts.
    pub(super) const REGISTER_SHIFT: u16 = 12;
    /// MOVES dr: register to memory.
    pub(super) const TO_MEMORY: u16 = 1 << 11;
    /// MOVEC's control register field.
    pub(super) const CONTROL_REGISTER: u16 = 0x0fff;
}

/// MOVEC's control register codes; any other is an illegal
/// instruction (CPU32RM 4.4, MOVEC).
mod control {
    /// Source function code.
    pub(super) const SFC: u16 = 0x000;
    /// Destination function code.
    pub(super) const DFC: u16 = 0x001;
    /// User stack pointer.
    pub(super) const USP: u16 = 0x800;
    /// Vector base register.
    pub(super) const VBR: u16 = 0x801;
}

/// Registers the family's opcodes.
pub(super) fn register(table: &mut Table) {
    let data = |opcode| Category::DATA.admits(opcode & FIELD_MASK);
    let data_alterable = |opcode| {
        Category::DATA
            .and(Category::ALTERABLE)
            .admits(opcode & FIELD_MASK)
    };
    table.set(u16::MAX, RESET, |_| true, reset);
    table.set(u16::MAX, STOP, |_| true, stop);
    table.set(u16::MAX, MOVEC_TO_GENERAL, |_| true, movec);
    table.set(u16::MAX, MOVEC_TO_CONTROL, |_| true, movec);
    table.set(MOVE_USP_MASK, MOVE_USP, |_| true, move_usp);
    for opcode in [ORI_TO_CCR, ANDI_TO_CCR, EORI_TO_CCR] {
        table.set(u16::MAX, opcode, |_| true, immediate_to_ccr);
    }
    for opcode in [ORI_TO_SR, ANDI_TO_SR, EORI_TO_SR] {
        table.set(u16::MAX, opcode, |_| true, immediate_to_sr);
    }
    table.set(MOVE_STATUS_MASK, MOVE_FROM_SR, data_alterable, move_from_sr);
    table.set(
        MOVE_STATUS_MASK,
        MOVE_FROM_CCR,
        data_alterable,
        move_from_ccr,
    );
    table.set(MOVE_STATUS_MASK, MOVE_TO_CCR, data, move_to_ccr);
    table.set(MOVE_STATUS_MASK, MOVE_TO_SR, data, move_to_sr);
    table.set(
        MOVES_MASK,
        MOVES,
        |opcode| {
            Size::from_field(opcode >> MOVES_SIZE_SHIFT).is_some()
                && Category::MEMORY
                    .and(Category::ALTERABLE)
                    .admits(opcode & FIELD_MASK)
        },
        moves,
    );
    table.set(u16::MAX, BGND, |_| true, bgnd);
}

/// A privilege violation unless in supervisor state.
fn supervisor(core: &Core<'_>) -> Result<(), Exception> {
    if core.cpu.registers.is_supervisor() {
        Ok(())
    } else {
        Err(core.privilege_violation())
    }
}

/// RESET: asserts RESET for the chip's peripherals and modules; the CPU's
/// state but PC unaffected (CPU32RM 4.4, RESET; SIMRM 8.2.6).
fn reset(core: &mut Core<'_>, _opcode: u16) -> Result<Flow, Exception> {
    supervisor(core)?;
    core.reset_instruction();
    Ok(Flow::Next)
}

/// STOP #data: the word to SR, then stop until a trace, interrupt or
/// reset (CPU32RM 4.4, STOP).  The manual loads the word whatever its
/// S bit; so does the core.
fn stop(core: &mut Core<'_>, _opcode: u16) -> Result<Flow, Exception> {
    supervisor(core)?;
    let sr = core.fetch_word()?;
    core.cpu.registers.set_sr(sr);
    Ok(Flow::Stop)
}

/// LPSTOP #data, its second word fetched: the word to SR, the LPSTOP
/// broadcast of the new interrupt mask, then stop (CPU32RM 4.4,
/// LPSTOP; MC68331UM 4.5.4.2).  A word with S clear is a privilege
/// violation, as in user state.
pub(super) fn lpstop(core: &mut Core<'_>) -> Result<Flow, Exception> {
    supervisor(core)?;
    let sr = core.fetch_word()?;
    if sr & status::S == 0 {
        return Err(core.privilege_violation());
    }
    core.cpu.registers.set_sr(sr);
    let mask = u32::from(core.cpu.registers.interrupt_mask());
    core.write(LPSTOP_BROADCAST, Size::Word, FunctionCode::CPU_SPACE, mask)?;
    let last_tail = core.last_tail();
    core.adjust(timing::lpstop(last_tail));
    Ok(Flow::Stop)
}

/// BGND: background mode when enabled, else an illegal instruction
/// (CPU32RM 4.4, BGND; 7.2.1).  The core has no background mode, so
/// it is never enabled.
fn bgnd(core: &mut Core<'_>, _opcode: u16) -> Result<Flow, Exception> {
    Err(core.illegal())
}

/// The operation of an immediate-to-status opcode, by bits 11:9: OR,
/// AND or EOR (CPU32RM 4.4, ORI, ANDI, EORI to CCR and to SR).
fn status_operation(opcode: u16, status: u16, immediate: u16) -> u16 {
    match opcode & !TO_SR {
        ORI_TO_CCR => status | immediate,
        ANDI_TO_CCR => status & immediate,
        _ => status ^ immediate,
    }
}

/// ORI, ANDI, EORI #data, CCR: the low byte of the word with the CCR.
fn immediate_to_ccr(core: &mut Core<'_>, opcode: u16) -> Result<Flow, Exception> {
    let immediate = core.fetch_word()? & status::CCR;
    let ccr = u16::from(core.cpu.registers.ccr());
    core.cpu
        .registers
        .set_ccr(status_operation(opcode, ccr, immediate) as u8);
    Ok(Flow::Next)
}

/// ORI, ANDI, EORI #data, SR: the word with SR.
fn immediate_to_sr(core: &mut Core<'_>, opcode: u16) -> Result<Flow, Exception> {
    supervisor(core)?;
    let immediate = core.fetch_word()?;
    let sr = core.cpu.registers.sr();
    core.cpu
        .registers
        .set_sr(status_operation(opcode, sr, immediate));
    Ok(Flow::Changed)
}

/// MOVE SR, <ea>: SR, a word, privileged on the CPU32 (CPU32RM 4.4,
/// MOVE from SR; 6.2.9).
fn move_from_sr(core: &mut Core<'_>, opcode: u16) -> Result<Flow, Exception> {
    supervisor(core)?;
    let destination = core.calculated_address(opcode & FIELD_MASK, Size::Word)?;
    let sr = core.cpu.registers.sr();
    core.write_operand(destination, Size::Word, u32::from(sr))?;
    Ok(Flow::Next)
}

/// MOVE CCR, <ea>: the CCR zero extended to a word (CPU32RM 4.4, MOVE
/// from CCR).
fn move_from_ccr(core: &mut Core<'_>, opcode: u16) -> Result<Flow, Exception> {
    let destination = core.calculated_address(opcode & FIELD_MASK, Size::Word)?;
    let ccr = core.cpu.registers.ccr();
    core.write_operand(destination, Size::Word, u32::from(ccr))?;
    Ok(Flow::Next)
}

/// MOVE <ea>, CCR: the low byte of a word source (CPU32RM 4.4, MOVE to
/// CCR).
fn move_to_ccr(core: &mut Core<'_>, opcode: u16) -> Result<Flow, Exception> {
    let source = core.effective_address(opcode & FIELD_MASK, Size::Word)?;
    let value = core.read_operand(source, Size::Word)?;
    core.cpu.registers.set_ccr(value as u8);
    Ok(Flow::Next)
}

/// MOVE <ea>, SR: a word source (CPU32RM 4.4, MOVE to SR).
fn move_to_sr(core: &mut Core<'_>, opcode: u16) -> Result<Flow, Exception> {
    supervisor(core)?;
    let source = core.effective_address(opcode & FIELD_MASK, Size::Word)?;
    let value = core.read_operand(source, Size::Word)?;
    core.cpu.registers.set_sr(value as u16);
    Ok(Flow::Changed)
}

/// MOVE USP, An and MOVE An, USP (CPU32RM 4.4, MOVE USP).
fn move_usp(core: &mut Core<'_>, opcode: u16) -> Result<Flow, Exception> {
    supervisor(core)?;
    let register = low_register(opcode);
    let registers = &mut core.cpu.registers;
    if opcode & USP_TO_ADDRESS != 0 {
        registers.set_address(register, registers.usp);
    } else {
        registers.usp = registers.address(register);
    }
    Ok(Flow::Next)
}

/// The number of the general register a MOVEC or MOVES second word
/// names, and whether it is an address register.
const fn general(extension: u16) -> (usize, bool) {
    (
        (extension >> extension::REGISTER_SHIFT & THREE_BITS) as usize,
        extension & extension::ADDRESS_REGISTER != 0,
    )
}

/// The general register a MOVEC or MOVES second word names.
fn general_register(core: &Core<'_>, extension: u16) -> u32 {
    match general(extension) {
        (number, true) => core.cpu.registers.address(number),
        (number, false) => core.cpu.registers.data[number],
    }
}

/// MOVEC Rc, Rn and MOVEC Rn, Rc: a long, a control register's
/// unimplemented bits read as zero (CPU32RM 4.4, MOVEC).
fn movec(core: &mut Core<'_>, opcode: u16) -> Result<Flow, Exception> {
    supervisor(core)?;
    let extension = core.fetch_word()?;
    let code = extension & extension::CONTROL_REGISTER;
    if ![control::SFC, control::DFC, control::USP, control::VBR].contains(&code) {
        return Err(core.illegal());
    }
    if opcode == MOVEC_TO_CONTROL {
        let value = general_register(core, extension);
        let registers = &mut core.cpu.registers;
        match code {
            control::SFC => registers.sfc = FunctionCode::new(value),
            control::DFC => registers.dfc = FunctionCode::new(value),
            control::USP => registers.usp = value,
            _ => registers.vbr = value,
        }
        return Ok(Flow::Next);
    }
    let registers = &mut core.cpu.registers;
    let value = match code {
        control::SFC => u32::from(registers.sfc.bits()),
        control::DFC => u32::from(registers.dfc.bits()),
        control::USP => registers.usp,
        _ => registers.vbr,
    };
    match general(extension) {
        (number, true) => registers.set_address(number, value),
        (number, false) => registers.data[number] = value,
    }
    Ok(Flow::Next)
}

/// MOVES Rn, <ea> and MOVES <ea>, Rn: an operand in the space DFC or
/// SFC names, which may be any function code, CPU space and the
/// reserved ones included (CPU32RM 4.4, MOVES; 5.3.1).  An address
/// register takes the operand sign extended.  Rn is read after the
/// address is computed, so MOVES An, (An)+ stores An incremented, as
/// the CPU32 does (CPU32RM 4.4, MOVES, note).
fn moves(core: &mut Core<'_>, opcode: u16) -> Result<Flow, Exception> {
    supervisor(core)?;
    let Some(size) = Size::from_field(opcode >> MOVES_SIZE_SHIFT) else {
        return Err(core.illegal());
    };
    let extension = core.fetch_word()?;
    core.charge(timing::moves(extension));
    let Operand::Data(address) = core.calculated_address(opcode & FIELD_MASK, size)? else {
        return Err(core.illegal());
    };
    if extension & extension::TO_MEMORY != 0 {
        let value = general_register(core, extension) & size.mask();
        let space = core.cpu.registers.dfc;
        core.write(address, size, space, value)?;
        return Ok(Flow::Next);
    }
    let space = core.cpu.registers.sfc;
    let value = core.read(address, size, space)?;
    let registers = &mut core.cpu.registers;
    match general(extension) {
        (number, true) => registers.set_address(number, size.sign_extend(value)),
        (number, false) => {
            let register = &mut registers.data[number];
            *register = *register & !size.mask() | value;
        }
    }
    Ok(Flow::Next)
}

#[cfg(test)]
mod tests {
    use super::LPSTOP;
    use super::LPSTOP_EXTENSION;
    use crate::address::Address;
    use crate::bus::Bus;
    use crate::bus::BusAccess;
    use crate::bus::BusError;
    use crate::bus::LPSTOP_BROADCAST;
    use crate::bus::Read;
    use crate::cpu::Cpu;
    use crate::cpu::Stop;
    use crate::cycles::Cycles;
    use crate::exception::Exception;
    use crate::exception::Vector;
    use crate::function_code::FunctionCode;
    use crate::instructions::tests::CODE;
    use crate::instructions::tests::NoInterrupts;
    use crate::instructions::tests::machine;
    use crate::instructions::tests::peek_long;
    use crate::instructions::tests::poke_long;
    use crate::instructions::tests::step;
    use crate::registers::status;
    use crate::size::Size;

    /// A stack in the test RAM.
    const STACK: u32 = 0x4000;
    /// An operand's address in the test RAM.
    const DATA: u32 = 0x2000;
    /// What [`Recorder`] reads.
    const READ_VALUE: u32 = 0x0080;

    /// A bus that records its cycles and RESET, and reads [`READ_VALUE`].
    #[derive(Debug, Default)]
    struct Recorder {
        /// The cycles, with the value of each write.
        cycles: Vec<(BusAccess, Option<u32>)>,
        /// When RESET was asserted.
        resets: Vec<Cycles>,
    }

    impl Bus for Recorder {
        fn read(&mut self, access: BusAccess) -> Result<Read, BusError> {
            self.cycles.push((access, None));
            Ok(Read {
                value: READ_VALUE,
                stall: Cycles::ZERO,
            })
        }

        fn write(&mut self, access: BusAccess, value: u32) -> Result<Cycles, BusError> {
            self.cycles.push((access, Some(value)));
            Ok(Cycles::ZERO)
        }

        fn reset_instruction(&mut self, now: Cycles) {
            self.resets.push(now);
        }
    }

    /// A supervisor machine with a stack.
    fn supervisor(code: &[u16]) -> Cpu {
        let mut cpu = machine(code);
        cpu.registers.ssp = STACK;
        cpu
    }

    /// One instruction on `bus`.
    fn step_on(cpu: &mut Cpu, bus: &mut Recorder) -> Stop {
        cpu.run_for(bus, &mut NoInterrupts, Cycles::ZERO, Cycles(1))
            .stop
    }

    /// The vector of the exception the last instruction took.
    fn taken(cpu: &Cpu) -> Option<Vector> {
        cpu.exception_taken().map(Exception::vector)
    }

    /// The exception an instruction at [`CODE`] raises with its own
    /// address stacked.
    const fn own(vector: Vector) -> Option<Exception> {
        Some(Exception::Short {
            vector,
            pc: Address(CODE),
        })
    }

    #[test]
    fn privileged_instructions_violate_in_user_state() {
        let privileged: &[&[u16]] = &[
            &[0x4e70],                           // RESET
            &[0x4e72, 0x2700],                   // STOP #$2700
            &[LPSTOP, LPSTOP_EXTENSION, 0x2700], // LPSTOP #$2700
            &[0x4e7a, 0x0801],                   // MOVEC VBR, D0
            &[0x4e7b, 0x0801],                   // MOVEC D0, VBR
            &[0x4e68],                           // MOVE USP, A0
            &[0x4e60],                           // MOVE A0, USP
            &[0x007c, 0x2700],                   // ORI #$2700, SR
            &[0x027c, 0x2700],                   // ANDI #$2700, SR
            &[0x0a7c, 0x2700],                   // EORI #$2700, SR
            &[0x40c0],                           // MOVE SR, D0
            &[0x46c0],                           // MOVE D0, SR
            &[0x0e50, 0x0800],                   // MOVES.W D0, (A0)
        ];
        for code in privileged {
            let mut cpu = supervisor(code);
            cpu.registers.set_sr(0);
            cpu.registers.data[0] = 0x1234_5678;
            cpu.registers.set_address(0, DATA);
            let before = cpu.registers.clone();
            step(&mut cpu, 1);
            assert_eq!(
                cpu.exception_taken(),
                own(Vector::PRIVILEGE_VIOLATION),
                "{code:x?}"
            );
            // The frame's PC is the instruction's own (CPU32RM 6.2.9).
            assert_eq!(peek_long(&cpu, STACK - 6), CODE, "{code:x?}");
            assert_eq!(cpu.registers.data, before.data);
            assert_eq!(cpu.registers.usp, before.usp);
        }
    }

    #[test]
    fn stop_loads_sr_and_stops() {
        let mut cpu = supervisor(&[0x4e72, 0x2315]);
        assert_eq!(step(&mut cpu, 1).stop, Stop::Budget);
        assert_eq!(cpu.registers.sr(), 0x2315);
        assert_eq!(cpu.registers.pc, CODE + 4);
        assert_eq!(step(&mut cpu, 1).stop, Stop::Stopped);
    }

    #[test]
    fn stop_traced_with_t1_does_not_stop() {
        // CPU32RM 6.2.10: STOP traced resumes at the trace handler.
        let mut cpu = supervisor(&[0x4e72, 0x2700, 0x4e71]);
        poke_long(&mut cpu, Vector::TRACE.offset(), CODE + 4);
        cpu.registers
            .set_sr(status::T1 | status::S | status::INTERRUPT_MASK);
        step(&mut cpu, 1);
        assert_eq!(taken(&cpu), Some(Vector::TRACE));
        assert_eq!(cpu.registers.pc, CODE + 4);
        assert_eq!(step(&mut cpu, 1).stop, Stop::Budget);
        assert_eq!(cpu.registers.pc, CODE + 6);
    }

    #[test]
    fn lpstop_broadcasts_the_mask_and_stops() {
        let mut cpu = supervisor(&[LPSTOP, LPSTOP_EXTENSION, 0x2504]);
        let mut bus = Recorder::default();
        step_on(&mut cpu, &mut bus);
        assert_eq!(cpu.registers.sr(), 0x2504);
        assert_eq!(cpu.registers.pc, CODE + 6);
        let [(access, value)] = bus.cycles[..] else {
            panic!("one cycle: {:?}", bus.cycles);
        };
        assert_eq!(access.function_code, FunctionCode::CPU_SPACE);
        assert_eq!(access.address, LPSTOP_BROADCAST);
        assert_eq!(access.size, Size::Word);
        assert_eq!(value, Some(5));
        assert_eq!(step_on(&mut cpu, &mut bus), Stop::Stopped);
    }

    #[test]
    fn lpstop_costs_its_own_row_not_tbls() {
        // CPU32RM 8.3.13, LPSTOP: head 3, 25 clocks.  After NOP (tail
        // 0) all 25; after MOVE.L D0, (A0) (tail 4), 25 less 3.
        let mut cpu = supervisor(&[LPSTOP, LPSTOP_EXTENSION, 0x2700]);
        let mut bus = Recorder::default();
        let run = cpu.run_for(&mut bus, &mut NoInterrupts, Cycles::ZERO, Cycles(1));
        assert_eq!(run.cycles, Cycles(25));
        let mut cpu = supervisor(&[0x2080, LPSTOP, LPSTOP_EXTENSION, 0x2700]);
        cpu.registers.set_address(0, DATA);
        step_on(&mut cpu, &mut bus);
        let run = cpu.run_for(&mut bus, &mut NoInterrupts, Cycles::ZERO, Cycles(1));
        assert_eq!(run.cycles, Cycles(25 - 3));
    }

    #[test]
    fn lpstop_with_s_clear_is_a_privilege_violation() {
        let mut cpu = supervisor(&[LPSTOP, LPSTOP_EXTENSION, 0x0700]);
        let mut bus = Recorder::default();
        step_on(&mut cpu, &mut bus);
        assert_eq!(cpu.exception_taken(), own(Vector::PRIVILEGE_VIOLATION));
        assert!(bus.cycles.is_empty());
    }

    #[test]
    fn f800_with_another_second_word_is_line_f() {
        let mut cpu = supervisor(&[LPSTOP, 0x01c1]);
        step(&mut cpu, 1);
        assert_eq!(cpu.exception_taken(), own(Vector::LINE_F));
    }

    #[test]
    fn reset_asserts_reset_and_goes_on() {
        let mut cpu = supervisor(&[0x4e70]);
        let before = cpu.registers.clone();
        let mut bus = Recorder::default();
        step_on(&mut cpu, &mut bus);
        assert_eq!(bus.resets, [Cycles::ZERO]);
        let mut expected = before;
        expected.pc = CODE + 2;
        assert_eq!(cpu.registers, expected);
    }

    #[test]
    fn movec_moves_each_control_register() {
        let mut cpu = supervisor(&[
            0x4e7b, 0x1000, // MOVEC D1, SFC
            0x4e7b, 0x1001, // MOVEC D1, DFC
            0x4e7b, 0xb800, // MOVEC A3, USP
            0x4e7b, 0x1801, // MOVEC D1, VBR
            0x4e7a, 0x2000, // MOVEC SFC, D2
            0x4e7a, 0x3001, // MOVEC DFC, D3
            0x4e7a, 0xc800, // MOVEC USP, A4
            0x4e7a, 0xf801, // MOVEC VBR, A7
        ]);
        cpu.registers.data[1] = 0xffff_fffd;
        cpu.registers.set_address(3, 0x0012_3456);
        step(&mut cpu, 8);
        let registers = &cpu.registers;
        assert_eq!(registers.sfc, FunctionCode::SUPERVISOR_DATA);
        assert_eq!(registers.dfc, FunctionCode::SUPERVISOR_DATA);
        assert_eq!(registers.usp, 0x0012_3456);
        assert_eq!(registers.vbr, 0xffff_fffd);
        // Unimplemented bits read as zero.
        assert_eq!(registers.data[2], 5);
        assert_eq!(registers.data[3], 5);
        assert_eq!(registers.address(4), 0x0012_3456);
        // A7 in supervisor state is SSP.
        assert_eq!(registers.ssp, 0xffff_fffd);
    }

    #[test]
    fn movec_of_another_control_register_is_illegal() {
        // CACR, CAAR, MSP, ISP: the 68020's, not the CPU32's.
        for code in [0x002, 0x802, 0x803, 0x804] {
            let mut cpu = supervisor(&[0x4e7a, code]);
            step(&mut cpu, 1);
            assert_eq!(
                cpu.exception_taken(),
                own(Vector::ILLEGAL_INSTRUCTION),
                "{code:#x}"
            );
        }
    }

    #[test]
    fn moves_writes_to_each_function_code() {
        for code in 0..=7 {
            // MOVES.W D0, (A0)
            let mut cpu = supervisor(&[0x0e50, 0x0800]);
            let function_code = FunctionCode::new(code);
            cpu.registers.dfc = function_code;
            cpu.registers.data[0] = 0xdead_beef;
            cpu.registers.set_address(0, DATA);
            let mut bus = Recorder::default();
            step_on(&mut cpu, &mut bus);
            assert_eq!(taken(&cpu), None);
            if function_code.is_memory_space() {
                assert!(bus.cycles.is_empty());
                assert_eq!(peek_long(&cpu, DATA) >> 16, 0xbeef, "FC {code}");
            } else {
                // CPU space and the reserved codes go to the bus.
                let [(access, value)] = bus.cycles[..] else {
                    panic!("FC {code}: one cycle: {:?}", bus.cycles);
                };
                assert_eq!(access.function_code, function_code);
                assert_eq!(access.address, Address(DATA));
                assert_eq!(value, Some(0xbeef));
            }
        }
    }

    #[test]
    fn moves_reads_through_sfc_sign_extending_into_an_address_register() {
        // MOVES.B (A0), A1; MOVES.B (A0), D1
        let mut cpu = supervisor(&[0x0e10, 0x9000, 0x0e10, 0x1000]);
        cpu.registers.sfc = FunctionCode::CPU_SPACE;
        cpu.registers.set_address(0, DATA + 1);
        cpu.registers.data[1] = 0x1234_5678;
        let mut bus = Recorder::default();
        step_on(&mut cpu, &mut bus);
        step_on(&mut cpu, &mut bus);
        assert_eq!(cpu.registers.address(1), 0xffff_ff80);
        assert_eq!(cpu.registers.data[1], 0x1234_5680);
        assert_eq!(bus.cycles.len(), 2);
        assert!(
            bus.cycles
                .iter()
                .all(|(access, _)| access.function_code == FunctionCode::CPU_SPACE)
        );
    }

    #[test]
    fn moves_an_postincrement_stores_the_incremented_an() {
        // MOVES.L A0, (A0)+ (CPU32RM 4.4, MOVES, note)
        let mut cpu = supervisor(&[0x0e98, 0x8800]);
        cpu.registers.dfc = FunctionCode::SUPERVISOR_DATA;
        cpu.registers.set_address(0, DATA);
        step(&mut cpu, 1);
        assert_eq!(peek_long(&cpu, DATA), DATA + 4);
        assert_eq!(cpu.registers.address(0), DATA + 4);
    }

    #[test]
    fn moves_to_memory_costs_more() {
        let cycles = |code: &[u16]| {
            let mut cpu = supervisor(code);
            cpu.registers.sfc = FunctionCode::SUPERVISOR_DATA;
            cpu.registers.dfc = FunctionCode::SUPERVISOR_DATA;
            cpu.registers.set_address(0, DATA);
            step(&mut cpu, 1).cycles
        };
        assert!(cycles(&[0x0e50, 0x0800]) > cycles(&[0x0e50, 0x0000]));
    }

    #[test]
    fn status_register_moves() {
        let mut cpu = supervisor(&[
            0x46fc, 0x2715, // MOVE #$2715, SR
            0x40c0, // MOVE SR, D0
            0x42c1, // MOVE CCR, D1
            0x44fc, 0xffea, // MOVE #$FFEA, CCR
        ]);
        cpu.registers.data[0] = 0xffff_ffff;
        cpu.registers.data[1] = 0xffff_ffff;
        step(&mut cpu, 4);
        assert_eq!(cpu.registers.data[0], 0xffff_2715);
        assert_eq!(cpu.registers.data[1], 0xffff_0015);
        assert_eq!(cpu.registers.sr(), 0x270a);
    }

    #[test]
    fn ccr_moves_and_immediates_are_not_privileged() {
        let mut cpu = supervisor(&[
            0x003c, 0x0011, // ORI #$11, CCR
            0x023c, 0x0013, // ANDI #$13, CCR
            0x0a3c, 0x00ff, // EORI #$FF, CCR
            0x42c0, // MOVE CCR, D0
            0x44fc, 0x0003, // MOVE #3, CCR
        ]);
        cpu.registers.set_sr(0x0004);
        cpu.registers.usp = STACK;
        step(&mut cpu, 3);
        assert_eq!(taken(&cpu), None);
        assert_eq!(cpu.registers.sr(), 0x000e);
        step(&mut cpu, 1);
        assert_eq!(cpu.registers.data[0] & 0xffff, 0x000e);
        step(&mut cpu, 1);
        assert_eq!(taken(&cpu), None);
        assert_eq!(cpu.registers.sr(), 0x0003);
    }

    #[test]
    fn immediates_to_sr() {
        let mut cpu = supervisor(&[
            0x007c, 0x0010, // ORI #$10, SR
            0x0a7c, 0x0015, // EORI #$15, SR
            0x027c, 0xdfff, // ANDI #$DFFF, SR
        ]);
        cpu.registers.set_sr(status::S | status::INTERRUPT_MASK);
        cpu.registers.usp = 0x1111;
        step(&mut cpu, 1);
        assert_eq!(cpu.registers.sr(), 0x2710);
        step(&mut cpu, 1);
        assert_eq!(cpu.registers.sr(), 0x2705);
        step(&mut cpu, 1);
        // S cleared: A7 is USP.
        assert_eq!(cpu.registers.sr(), 0x0705);
        assert_eq!(cpu.registers.address(7), 0x1111);
    }

    #[test]
    fn sr_writes_are_changes_of_flow_for_t0() {
        // ORI #0, SR is traced on change of flow; ORI #0, CCR is not.
        for (code, traced) in [(0x007c, true), (0x003c, false)] {
            let mut cpu = supervisor(&[code, 0x0000]);
            poke_long(&mut cpu, Vector::TRACE.offset(), CODE + 0x100);
            cpu.registers
                .set_sr(status::T0 | status::S | status::INTERRUPT_MASK);
            step(&mut cpu, 1);
            assert_eq!(taken(&cpu) == Some(Vector::TRACE), traced, "{code:#06x}");
        }
    }

    #[test]
    fn move_usp_both_ways() {
        // MOVE A2, USP; MOVE USP, A5
        let mut cpu = supervisor(&[0x4e62, 0x4e6d]);
        cpu.registers.set_address(2, 0x0abc_def0);
        step(&mut cpu, 2);
        assert_eq!(cpu.registers.usp, 0x0abc_def0);
        assert_eq!(cpu.registers.address(5), 0x0abc_def0);
        assert_eq!(cpu.registers.ssp, STACK);
    }

    #[test]
    fn bgnd_is_illegal_without_background_mode() {
        let mut cpu = supervisor(&[0x4afa]);
        step(&mut cpu, 1);
        assert_eq!(cpu.exception_taken(), own(Vector::ILLEGAL_INSTRUCTION));
    }
}
