//! Unicorn's known m68k faults, as smartclock's harness works around
//! them (github.com/charlieh0tel/smartclock,
//! `tools/firmware/unicorn_check.py`, its `harness.py` and
//! `docs/emulator.md`, "The condition codes"), as regression tests of
//! what the CPU32 does instead (AGENTS.md: they are tests here, never
//! exclusions).  The faults:
//!
//! 1. A read of SR spoils the lazy condition codes: it returns, and
//!    leaves the CPU to go on with, the last flag-setting instruction's
//!    operands read as flags (Unicorn #2380).  Read after a run stops,
//!    and in a trap's handler.
//! 2. Part way through a translated block, a code hook sees stale
//!    codes: a write of PC there loses them, a read of SR returns stale
//!    ones, a write of SR is read by the rest of the block as the
//!    operands of the instruction that set the codes, and a write of SR
//!    on an instruction that then faults is undone.  This core has no
//!    blocks: a code hook runs between two instructions, with the
//!    state the first left, and one that stops the run lets the host
//!    write registers before the second runs.  Each case runs through
//!    a code hook at the instruction and again between two runs.
//! 3. `rtr` is taken for an illegal instruction.
//! 4. `move (sp)+,sr` that clears S pops the user stack, not the
//!    supervisor stack it read.
//! 5. `stop` runs on rather than waiting for an interrupt.
//! 6. No exception is taken by the CPU: each, `rte` too, is handed to
//!    the host.
//! 7. VBR cannot be read, so the host must follow each `movec` to it.
//!
//! Each case of 1 and 2 runs smartclock's compares, 2 with 2 (Z) and 1
//! with 2 (N, C), then `scs d1; seq d3`, which record the codes the
//! CPU goes on with.  Fault 1 has twins too, which set the codes with
//! MOVEQ and read them back.  Further cases check that hooks see the
//! state the manual gives at an instruction boundary.

use std::cell::RefCell;
use std::ops::ControlFlow;
use std::ops::RangeInclusive;
use std::rc::Rc;

use mc683xx_cpu32::address::Address;
use mc683xx_cpu32::bus::Bus;
use mc683xx_cpu32::bus::BusAccess;
use mc683xx_cpu32::bus::BusError;
use mc683xx_cpu32::bus::PortSize;
use mc683xx_cpu32::bus::Read;
use mc683xx_cpu32::cpu::Cpu;
use mc683xx_cpu32::cpu::Run;
use mc683xx_cpu32::cpu::Stop;
use mc683xx_cpu32::cpu::hooks::Instruction;
use mc683xx_cpu32::cycles::Cycles;
use mc683xx_cpu32::exception::Vector;
use mc683xx_cpu32::interrupt::Acknowledge;
use mc683xx_cpu32::interrupt::Acknowledged;
use mc683xx_cpu32::interrupt::InterruptLevel;
use mc683xx_cpu32::interrupt::Interrupts;
use mc683xx_cpu32::memory::Backing;
use mc683xx_cpu32::memory::Memory;
use mc683xx_cpu32::memory::Page;
use mc683xx_cpu32::memory::RegionId;
use mc683xx_cpu32::memory::Space;
use mc683xx_cpu32::registers::Registers;
use mc683xx_cpu32::registers::status;

/// The address lines of the 68331, ADDR[23:0] (MC68331UM 4.4.1.1).
const ADDRESS_BITS: u32 = 24;
/// RAM from zero, in both spaces: the vector table, code and stacks.
const RAM_BYTES: u32 = 0x10000;
/// Clocks of a RAM cycle: the fastest (CPU32RM 8.1.5).
const RAM_CLOCKS: u8 = 2;
/// Where the code starts, as in `unicorn_check.py`.
const START: u32 = 0x1000;
/// The supervisor stack's top, as in `unicorn_check.py`.
const STACK_TOP: u32 = 0x8000;
/// The user stack's top, for the cases that leave supervisor state.
const USER_STACK_TOP: u32 = 0x6000;
/// Where the cases' exception handlers are.
const HANDLERS: u32 = 0x3000;
/// Bytes between two handlers.
const HANDLER_SPACING: u32 = 0x10;
/// An address no case maps, which a read from is a bus error.
const UNMAPPED: u32 = 0x0010_0000;
/// Supervisor state, interrupts masked: how each case starts.
const SUPERVISOR: u16 = status::S | status::INTERRUPT_MASK;
/// The most instructions a case runs before it is taken to be stuck.
const STEP_LIMIT: usize = 100;
/// A run's budget for a case a hook stops: more than any case needs.
const BUDGET: Cycles = Cycles(1000);

/// MOVE #0, CCR (CPU32RM 4.4, MOVE to CCR).
const CLEAR_CODES: [u16; 2] = [0x44fc, 0x0000];
/// MOVEQ #1, D0 and MOVEQ #2, D0 (CPU32RM 4.4, MOVEQ).
const MOVEQ_1: u16 = 0x7001;
const MOVEQ_2: u16 = 0x7002;
/// MOVEQ #0, D0: Z.  MOVEQ #-1, D0: N.
const MOVEQ_0: u16 = 0x7000;
const MOVEQ_MINUS_1: u16 = 0x70ff;
/// CMPI.L #2, D0 (CPU32RM 4.4, CMPI).
const COMPARE_WITH_2: [u16; 3] = [0x0c80, 0x0000, 0x0002];
/// SCS D1; SEQ D3 (CPU32RM 4.4, Scc): each $FF if its condition
/// holds, else $00.
const RECORD: [u16; 2] = [0x55c1, 0x57c3];
/// What RECORD leaves for a condition that holds.
const SET: u8 = 0xff;
/// TRAP #0 (CPU32RM 4.4, TRAP).
const TRAP_0: u16 = 0x4e40;
/// MOVE.L (UNMAPPED).L, D1 (CPU32RM 4.4, MOVE).
const LOAD_UNMAPPED: [u16; 3] = [0x2239, (UNMAPPED >> 16) as u16, UNMAPPED as u16];
/// NOP (CPU32RM 4.4, NOP).
const NOP: u16 = 0x4e71;
/// RTE (CPU32RM 4.4, RTE).
const RTE: u16 = 0x4e73;
/// RTR (CPU32RM 4.4, RTR).
const RTR: u16 = 0x4e77;
/// MOVE (SP)+, SR (CPU32RM 4.4, MOVE to SR).
const POP_STATUS: u16 = 0x46df;
/// STOP #<data> (CPU32RM 4.4, STOP).
const STOP: u16 = 0x4e72;
/// MOVE #<data>, SR and ORI #<data>, SR (CPU32RM 4.4, MOVE to SR, ORI
/// to SR).
const MOVE_TO_SR: u16 = 0x46fc;
const ORI_TO_SR: u16 = 0x007c;
/// BRA.W to the next instruction: displacement 2 from the extension
/// word (CPU32RM 4.4, BRA).
const BRANCH_TO_NEXT: [u16; 2] = [0x6000, 0x0002];
/// MOVEC D0, VBR (CPU32RM 4.4, MOVEC): Rn D0, control register $801.
const MOVE_D0_TO_VBR: [u16; 2] = [0x4e7b, 0x0801];

/// The four- and six-word frames' fields (CPU32RM 6.4.1, 6.4.2), and
/// the BERR frame's first three (6.4.3): offsets from SP.
const FRAME_SR: u32 = 0;
const FRAME_PC: u32 = 2;
const FRAME_FORMAT_VECTOR: u32 = 6;
/// A format $0 frame's bytes (CPU32RM 6.4.1).
const FOUR_WORD_BYTES: u32 = 8;

/// A bus with nothing on it.
#[derive(Debug)]
struct NoBus;

impl Bus for NoBus {
    fn read(&mut self, _access: BusAccess) -> Result<Read, BusError> {
        Err(BusError)
    }

    fn write(&mut self, _access: BusAccess, _value: u32) -> Result<Cycles, BusError> {
        Err(BusError)
    }
}

/// An interrupt source: one level, held until acknowledged, then the
/// level's autovector.
#[derive(Debug)]
struct Request(InterruptLevel);

impl Interrupts for Request {
    fn level(&self) -> InterruptLevel {
        self.0
    }

    fn acknowledge(&mut self, _level: InterruptLevel, _now: Cycles) -> Acknowledged {
        self.0 = InterruptLevel::NONE;
        Acknowledge::Autovector.into()
    }
}

/// One of smartclock's compares: its code, the condition codes it
/// leaves, and what RECORD then leaves in D1 and D3.
#[derive(Debug, Clone, Copy)]
struct Compare {
    /// What it is, for a failure's message.
    name: &'static str,
    /// MOVE #0, CCR; MOVEQ; CMPI.L #2, D0.
    code: [u16; 6],
    /// The codes it leaves.
    codes: u16,
    /// SCS D1's and SEQ D3's low bytes after it.
    recorded: (u8, u8),
}

/// 2 with 2: Z.  1 with 2: N and C (CPU32RM 4.4, CMPI).
const COMPARES: [Compare; 2] = [
    Compare {
        name: "2 cmp 2",
        code: compare(MOVEQ_2),
        codes: status::Z,
        recorded: (0, SET),
    },
    Compare {
        name: "1 cmp 2",
        code: compare(MOVEQ_1),
        codes: status::N | status::C,
        recorded: (SET, 0),
    },
];

/// MOVE #0, CCR; `moveq`; CMPI.L #2, D0.
const fn compare(moveq: u16) -> [u16; 6] {
    [
        CLEAR_CODES[0],
        CLEAR_CODES[1],
        moveq,
        COMPARE_WITH_2[0],
        COMPARE_WITH_2[1],
        COMPARE_WITH_2[2],
    ]
}

/// Instructions in a compare: MOVE to CCR, MOVEQ, CMPI.
const COMPARE_INSTRUCTIONS: usize = 3;

/// The MOVEQ twins of the compares: the codes each leaves (CPU32RM
/// 4.4, MOVEQ: N and Z from the result, V and C cleared).
const MOVEQS: [(u16, u16); 2] = [(MOVEQ_0, status::Z), (MOVEQ_MINUS_1, status::N)];

/// A CPU in supervisor state, interrupts masked, at `START` on RAM
/// holding `code`, each vector at its own handler, every handler RTE.
struct Machine {
    /// The core.
    cpu: Cpu,
    /// Its RAM.
    ram: RegionId,
}

impl Machine {
    fn new(code: &[u16]) -> Self {
        let mut memory = Memory::new(ADDRESS_BITS).expect("24 address lines");
        let ram = memory.add_region(vec![0; RAM_BYTES as usize]);
        let backing = Some(Backing {
            region: ram,
            offset: 0,
        });
        let page = Page {
            read: backing,
            write: backing,
            port: PortSize::Bits16,
            clocks: RAM_CLOCKS,
        };
        for space in [Space::User, Space::Supervisor] {
            memory
                .map(space, Address(0), RAM_BYTES, page)
                .expect("RAM inside 24 bits");
        }
        let mut machine = Self {
            cpu: Cpu::new(memory),
            ram,
        };
        for vector in 0..=u8::MAX {
            let handler = handler(Vector(vector));
            machine.poke_long(Vector(vector).offset(), handler);
            machine.poke_words(handler, &[RTE]);
        }
        machine.poke_words(START, code);
        machine.cpu.registers.set_sr(SUPERVISOR);
        machine.cpu.registers.ssp = STACK_TOP;
        machine.cpu.registers.usp = USER_STACK_TOP;
        machine.cpu.registers.pc = START;
        machine
    }

    fn poke_words(&mut self, at: u32, words: &[u16]) {
        let ram = self.cpu.memory.region_mut(self.ram);
        for (index, word) in words.iter().enumerate() {
            let at = at as usize + 2 * index;
            ram[at..at + 2].copy_from_slice(&word.to_be_bytes());
        }
    }

    fn poke_long(&mut self, at: u32, value: u32) {
        self.poke_words(at, &[(value >> 16) as u16, value as u16]);
    }

    fn peek_word(&self, at: u32) -> u16 {
        let ram = self.cpu.memory.region(self.ram);
        u16::from_be_bytes([ram[at as usize], ram[at as usize + 1]])
    }

    fn peek_long(&self, at: u32) -> u32 {
        u32::from(self.peek_word(at)) << 16 | u32::from(self.peek_word(at + 2))
    }

    /// Runs one instruction: a budget of one cycle runs exactly one.
    fn step(&mut self) -> Run {
        self.cpu.run_for(
            &mut NoBus,
            &mut Request(InterruptLevel::NONE),
            Cycles::ZERO,
            Cycles(1),
        )
    }

    /// Runs `count` instructions.
    fn steps(&mut self, count: usize) {
        for _ in 0..count {
            assert_eq!(
                self.step().stop,
                Stop::Budget,
                "at {:#x}",
                self.cpu.registers.pc
            );
        }
    }

    /// Runs until PC is `end`.
    fn run_to(&mut self, end: u32) {
        for _ in 0..STEP_LIMIT {
            if self.cpu.registers.pc == end {
                return;
            }
            self.steps(1);
        }
        panic!("never reached {end:#x}: at {:#x}", self.cpu.registers.pc);
    }

    /// Runs until the instruction at `at` is next, `pause`'s way: the
    /// registers a code hook saw there, if one did.
    fn pause(&mut self, at: u32, pause: Pause) -> Option<Registers> {
        match pause {
            Pause::BetweenRuns => {
                self.run_to(at);
                None
            }
            Pause::CodeHook => {
                let seen = Rc::new(RefCell::new(None));
                let into = Rc::clone(&seen);
                let id = self
                    .cpu
                    .add_code_hook(Address(at)..=Address(at), move |instruction| {
                        *into.borrow_mut() = Some(instruction.registers.clone());
                        ControlFlow::Break(())
                    });
                let run = self.cpu.run_for(
                    &mut NoBus,
                    &mut Request(InterruptLevel::NONE),
                    Cycles::ZERO,
                    BUDGET,
                );
                assert_eq!(run.stop, Stop::Hook(id));
                assert_eq!(self.cpu.registers.pc, at);
                seen.take()
            }
        }
    }

    /// Runs `interrupts` until a block hook at `at` stops it: what the
    /// hook saw.
    fn block_at(&mut self, at: u32, interrupts: &mut dyn Interrupts) -> Instruction {
        let seen = Rc::new(RefCell::new(None));
        let into = Rc::clone(&seen);
        let id = self
            .cpu
            .add_block_hook(Address(at)..=Address(at), move |instruction| {
                *into.borrow_mut() = Some(instruction.clone());
                ControlFlow::Break(())
            });
        let run = self
            .cpu
            .run_for(&mut NoBus, interrupts, Cycles::ZERO, BUDGET);
        assert_eq!(run.stop, Stop::Hook(id));
        assert_eq!(self.cpu.registers.pc, at);
        seen.take().expect("the hook ran")
    }

    /// Adds a code hook on every address that logs each instruction's
    /// address and registers, and goes on.
    fn log_code(&mut self) -> Rc<RefCell<Vec<Instruction>>> {
        let seen = Rc::new(RefCell::new(Vec::new()));
        let into = Rc::clone(&seen);
        self.cpu.add_code_hook(everywhere(), move |instruction| {
            into.borrow_mut().push(instruction.clone());
            ControlFlow::Continue(())
        });
        seen
    }

    /// SCS D1's and SEQ D3's low bytes.
    fn recorded(&self) -> (u8, u8) {
        (
            self.cpu.registers.data[1] as u8,
            self.cpu.registers.data[3] as u8,
        )
    }

    /// The SR a frame at SSP stacked.
    fn stacked_sr(&self) -> u16 {
        self.peek_word(self.cpu.registers.ssp + FRAME_SR)
    }
}

/// How a case stops before an instruction for the host to read or
/// write registers.
#[derive(Debug, Clone, Copy)]
enum Pause {
    /// A run stops, and the next starts, there.
    BetweenRuns,
    /// A code hook there stops the run, as smartclock's harness stops
    /// Unicorn's.
    CodeHook,
}

/// Both pauses.
const PAUSES: [Pause; 2] = [Pause::BetweenRuns, Pause::CodeHook];

/// Every address.
fn everywhere() -> RangeInclusive<Address> {
    Address(0)..=Address(u32::MAX)
}

/// Where `vector`'s handler is.
const fn handler(vector: Vector) -> u32 {
    HANDLERS + vector.0 as u32 * HANDLER_SPACING
}

/// `parts` of code, end to end, and where each starts.
fn program(parts: &[&[u16]]) -> (Vec<u16>, Vec<u32>) {
    let mut code = Vec::new();
    let mut starts = Vec::new();
    for part in parts {
        starts.push(START + 2 * code.len() as u32);
        code.extend_from_slice(part);
    }
    (code, starts)
}

/// Fault 1, read after a stop: SR read once a run stops between the
/// compare and RECORD gives the compare's codes, and the CPU goes on
/// with them (CPU32RM 2.3.1.3: the CCR is the codes the last
/// instruction left; reading it is not an instruction).
#[test]
fn fault_1_status_read_after_a_stop_keeps_the_codes() {
    for compare in COMPARES {
        let (code, _) = program(&[&compare.code, &RECORD]);
        let mut machine = Machine::new(&code);
        machine.steps(COMPARE_INSTRUCTIONS);
        assert_eq!(
            machine.cpu.registers.sr(),
            SUPERVISOR | compare.codes,
            "{}",
            compare.name
        );
        machine.steps(RECORD.len());
        assert_eq!(machine.recorded(), compare.recorded, "{}", compare.name);
    }
}

/// Fault 1's twin on MOVEQ, which the core has.
#[test]
fn fault_1_status_read_after_a_stop_keeps_moveq_codes() {
    for (moveq, codes) in MOVEQS {
        let mut machine = Machine::new(&[moveq, NOP]);
        machine.steps(1);
        assert_eq!(machine.cpu.registers.sr(), SUPERVISOR | codes);
        machine.steps(1);
        assert_eq!(machine.cpu.registers.sr(), SUPERVISOR | codes);
    }
}

/// Fault 1, read in a trap: TRAP #0 between the compare and RECORD
/// stacks SR with the compare's codes (CPU32RM 6.2.4, 6.4.1), its
/// handler's RTE restores them (6.2.12), and RECORD records them.
#[test]
fn fault_1_status_read_in_a_trap_keeps_the_codes() {
    for compare in COMPARES {
        let (code, starts) = program(&[&compare.code, &[TRAP_0], &RECORD, &[NOP]]);
        let mut machine = Machine::new(&code);
        machine.run_to(handler(Vector::trap(0)));
        assert_eq!(
            machine.stacked_sr(),
            SUPERVISOR | compare.codes,
            "{}",
            compare.name
        );
        machine.run_to(starts[3]);
        assert_eq!(machine.recorded(), compare.recorded, "{}", compare.name);
    }
}

/// Fault 1's twin on MOVEQ: the trap stacks MOVEQ's codes, and RTE
/// gives them back.
#[test]
fn fault_1_status_read_in_a_trap_keeps_moveq_codes() {
    for (moveq, codes) in MOVEQS {
        let (code, starts) = program(&[&[moveq], &[TRAP_0], &[NOP]]);
        let mut machine = Machine::new(&code);
        machine.run_to(handler(Vector::trap(0)));
        assert_eq!(machine.stacked_sr(), SUPERVISOR | codes);
        assert_eq!(machine.cpu.registers.ccr(), codes as u8);
        machine.run_to(starts[2]);
        assert_eq!(machine.cpu.registers.sr(), SUPERVISOR | codes);
    }
}

/// Fault 2, a PC write: PC written at a code hook on RECORD, part way
/// through the compare's block, to RECORD itself, loses nothing; nor
/// does the same write between two runs.
#[test]
fn fault_2_pc_write_in_a_code_hook_keeps_the_codes() {
    for pause in PAUSES {
        for compare in COMPARES {
            let (code, starts) = program(&[&compare.code, &RECORD]);
            let mut machine = Machine::new(&code);
            machine.pause(starts[1], pause);
            machine.cpu.registers.pc = starts[1];
            machine.steps(RECORD.len());
            assert_eq!(
                machine.recorded(),
                compare.recorded,
                "{} {pause:?}",
                compare.name
            );
        }
    }
}

/// Fault 2, an SR read part way: a code hook on RECORD, part way
/// through the compare's block, reads SR with the compare's codes
/// (CPU32RM 2.3.1.3), as does the host once the hook or a run stops
/// there; RECORD then goes on with them.
#[test]
fn fault_2_status_read_in_a_code_hook_is_current() {
    for pause in PAUSES {
        for compare in COMPARES {
            let (code, starts) = program(&[&compare.code, &RECORD]);
            let mut machine = Machine::new(&code);
            let seen = machine.pause(starts[1], pause);
            let expected = SUPERVISOR | compare.codes;
            if let Some(seen) = seen {
                assert_eq!(seen.sr(), expected, "{} in the hook", compare.name);
            }
            assert_eq!(
                machine.cpu.registers.sr(),
                expected,
                "{} {pause:?}",
                compare.name
            );
            machine.steps(RECORD.len());
            assert_eq!(
                machine.recorded(),
                compare.recorded,
                "{} {pause:?}",
                compare.name
            );
        }
    }
}

/// Fault 2, an SR write part way: SR written at a code hook on RECORD,
/// its codes clear, is what RECORD goes on with (CPU32RM 4.4, Scc: the
/// condition is tested as it executes).
#[test]
fn fault_2_status_write_in_a_code_hook_is_what_follows() {
    for pause in PAUSES {
        for compare in COMPARES {
            let (code, starts) = program(&[&compare.code, &RECORD]);
            let mut machine = Machine::new(&code);
            machine.pause(starts[1], pause);
            machine.cpu.registers.set_sr(SUPERVISOR);
            machine.steps(RECORD.len());
            assert_eq!(machine.recorded(), (0, 0), "{} {pause:?}", compare.name);
        }
    }
}

/// Fault 2, an SR write before a fault: SR written at a code hook on a
/// load that then takes a bus error is kept.  The BERR frame stacks it
/// (CPU32RM 6.2.2, 6.4.3), its codes are the CPU's in the handler, and
/// a run from past the load, as `unicorn_check.py` goes on, records
/// them.
#[test]
fn fault_2_status_write_in_a_code_hook_before_a_fault_is_kept() {
    for pause in PAUSES {
        for compare in COMPARES {
            let (code, starts) = program(&[&compare.code, &LOAD_UNMAPPED, &RECORD]);
            let mut machine = Machine::new(&code);
            machine.pause(starts[1], pause);
            machine.cpu.registers.set_sr(SUPERVISOR);
            machine.run_to(handler(Vector::BUS_ERROR));
            let name = compare.name;
            assert_eq!(machine.stacked_sr(), SUPERVISOR, "{name} {pause:?}");
            assert_eq!(machine.cpu.registers.ccr(), 0, "{name} {pause:?}");
            machine.cpu.registers.pc = starts[2];
            machine.steps(RECORD.len());
            assert_eq!(machine.recorded(), (0, 0), "{name} {pause:?}");
        }
    }
}

/// Fault 2 at a block's start: a block hook on RECORD, a branch's
/// target, reads the compare's codes, which BRA leaves (CPU32RM 4.4,
/// BRA: condition codes not affected).
#[test]
fn a_block_hook_reads_the_codes_the_block_before_left() {
    for compare in COMPARES {
        let (code, starts) = program(&[&compare.code, &BRANCH_TO_NEXT, &RECORD]);
        let mut machine = Machine::new(&code);
        let seen = machine.block_at(starts[2], &mut Request(InterruptLevel::NONE));
        assert_eq!(
            seen.registers.sr(),
            SUPERVISOR | compare.codes,
            "{}",
            compare.name
        );
        machine.steps(RECORD.len());
        assert_eq!(machine.recorded(), compare.recorded, "{}", compare.name);
    }
}

/// A code hook on each instruction sees the codes the one before it
/// left, its own not yet set (CPU32RM 2.3.1.3; 4.4, MOVE to CCR,
/// MOVEQ, CMPI, Scc).
#[test]
fn a_code_hook_sees_the_codes_at_each_instruction() {
    for compare in COMPARES {
        let moveq = compare.code[CLEAR_CODES.len()];
        let (code, starts) = program(&[
            &CLEAR_CODES,
            &[moveq],
            &COMPARE_WITH_2,
            &RECORD[..1],
            &RECORD[1..],
        ]);
        let mut machine = Machine::new(&code);
        machine.cpu.registers.set_sr(SUPERVISOR | status::CCR);
        let seen = machine.log_code();
        machine.steps(starts.len());
        // MOVEQ #1 and #2 set no code; Scc changes none.
        let codes = [status::CCR, 0, 0, compare.codes, compare.codes];
        let expected: Vec<_> = starts
            .iter()
            .zip(codes)
            .map(|(&at, codes)| (Address(at), SUPERVISOR | codes))
            .collect();
        let seen: Vec<_> = seen
            .borrow()
            .iter()
            .map(|instruction| (instruction.pc, instruction.registers.sr()))
            .collect();
        assert_eq!(seen, expected, "{}", compare.name);
    }
}

/// A code hook sees SR's system byte as the instruction before it left
/// it: the mask and S that MOVE to SR and ORI to SR wrote (CPU32RM
/// 4.4, MOVE to SR, ORI to SR), and A7 and the stack's top following S
/// (2.3.1.2).
#[test]
fn a_code_hook_sees_the_system_byte_at_each_instruction() {
    /// Supervisor state, mask 0.
    const UNMASKED: u16 = status::S;
    /// ORI's mask: level 5.
    const LEVEL_5: u16 = 5 << status::INTERRUPT_SHIFT;
    /// What each stack's top holds.
    const SUPERVISOR_TOP: u32 = 0x5555_aaaa;
    const USER_TOP: u32 = 0x1234_5678;
    let (code, starts) = program(&[
        &[MOVE_TO_SR, UNMASKED],
        &[ORI_TO_SR, LEVEL_5],
        &[MOVE_TO_SR, 0],
        &[NOP],
    ]);
    let mut machine = Machine::new(&code);
    machine.poke_long(STACK_TOP, SUPERVISOR_TOP);
    machine.poke_long(USER_STACK_TOP, USER_TOP);
    let seen = machine.log_code();
    machine.steps(starts.len());
    let supervisor = (STACK_TOP, Some(SUPERVISOR_TOP));
    let states = [
        (SUPERVISOR, supervisor),
        (UNMASKED, supervisor),
        (UNMASKED | LEVEL_5, supervisor),
        (0, (USER_STACK_TOP, Some(USER_TOP))),
    ];
    let expected: Vec<_> = starts
        .iter()
        .zip(states)
        .map(|(&at, (sr, (a7, top)))| (Address(at), sr, a7, top))
        .collect();
    let seen: Vec<_> = seen
        .borrow()
        .iter()
        .map(|instruction| {
            let registers = &instruction.registers;
            (
                instruction.pc,
                registers.sr(),
                registers.address(7),
                instruction.stack_top,
            )
        })
        .collect();
    assert_eq!(seen, expected);
}

/// A block hook on a trap's handler sees SR as exception processing
/// left it: S set, T1 and T0 clear, the mask and codes kept (CPU32RM
/// 6.1.3); A7 the frame's, with the frame's SR and PC's high word at
/// its top (6.4.1).
#[test]
fn a_block_hook_sees_the_status_a_trap_enters_with() {
    /// User state, mask 0.
    const USER: u16 = 0;
    let (code, starts) = program(&[&[MOVEQ_0], &[TRAP_0], &[NOP]]);
    let mut machine = Machine::new(&code);
    machine.cpu.registers.set_sr(USER);
    let seen = machine.block_at(handler(Vector::trap(0)), &mut Request(InterruptLevel::NONE));
    let frame = STACK_TOP - FOUR_WORD_BYTES;
    let frame_top = u32::from(USER | status::Z) << 16 | starts[2] >> 16;
    assert_eq!(
        (
            seen.registers.sr(),
            seen.registers.address(7),
            seen.stack_top
        ),
        (status::S | status::Z, frame, Some(frame_top))
    );
}

/// A block hook on an interrupt's handler sees the mask raised to the
/// level taken (CPU32RM 6.1.3, 6.2.11); the frame keeps the mask
/// before.
#[test]
fn a_block_hook_sees_the_mask_an_interrupt_enters_with() {
    let level = InterruptLevel::new(3).expect("a level");
    let mut machine = Machine::new(&[NOP]);
    machine.cpu.registers.set_sr(status::S);
    let seen = machine.block_at(handler(Vector::autovector(level)), &mut Request(level));
    assert_eq!(
        seen.registers.sr(),
        status::S | u16::from(level.get()) << status::INTERRUPT_SHIFT
    );
    assert_eq!(machine.stacked_sr(), status::S);
}

/// Fault 3: RTR pops the CCR from the word at SP, then PC; the
/// supervisor byte is unaffected (CPU32RM 4.4, RTR).
#[test]
fn fault_3_rtr_restores_the_codes_and_returns() {
    /// Where RTR returns to.
    const RETURN: u32 = 0x2000;
    /// The stacked word: the CCR its low byte; the high byte ignored.
    const STACKED: u16 = 0xa715;
    let mut machine = Machine::new(&[RTR]);
    let sp = STACK_TOP - 6;
    machine.cpu.registers.ssp = sp;
    machine.poke_words(sp, &[STACKED]);
    machine.poke_long(sp + 2, RETURN);
    machine.steps(1);
    assert_eq!(
        machine.cpu.registers.sr(),
        SUPERVISOR | (STACKED & status::CCR)
    );
    assert_eq!(machine.cpu.registers.pc, RETURN);
    assert_eq!(machine.cpu.registers.ssp, STACK_TOP);
}

/// Fault 4: MOVE (SP)+, SR reads its source, on the supervisor stack,
/// before it writes SR (CPU32RM 4.4, MOVE to SR: Source -> SR), so a
/// popped SR that clears S increments SSP and leaves USP alone.
#[test]
fn fault_4_status_pop_into_user_state_pops_the_supervisor_stack() {
    /// User state, Z.
    const POPPED: u16 = status::Z;
    let mut machine = Machine::new(&[POP_STATUS, NOP]);
    let sp = STACK_TOP - 2;
    machine.cpu.registers.ssp = sp;
    machine.poke_words(sp, &[POPPED]);
    machine.steps(1);
    assert_eq!(machine.cpu.registers.sr(), POPPED);
    assert_eq!(machine.cpu.registers.ssp, STACK_TOP);
    assert_eq!(machine.cpu.registers.usp, USER_STACK_TOP);
    assert_eq!(machine.cpu.registers.address(7), USER_STACK_TOP);
    assert_eq!(machine.cpu.registers.pc, START + 2);
}

/// Fault 5: STOP loads SR, advances PC past itself and stops until an
/// interrupt above the new mask (CPU32RM 4.4, STOP; 6.2.11), which
/// stacks the next instruction's address.
#[test]
fn fault_5_stop_waits_for_an_interrupt() {
    /// Supervisor state, mask 0.
    const LOADED: u16 = status::S;
    /// A whole run's budget: more than one instruction.
    const BUDGET: Cycles = Cycles(1000);
    let level = InterruptLevel::new(1).expect("a level");
    let mut machine = Machine::new(&[STOP, LOADED, NOP]);
    let run = machine.cpu.run_for(
        &mut NoBus,
        &mut Request(InterruptLevel::NONE),
        Cycles::ZERO,
        BUDGET,
    );
    assert_eq!(run.stop, Stop::Stopped);
    assert_eq!(machine.cpu.registers.sr(), LOADED);
    assert_eq!(machine.cpu.registers.pc, START + 4);
    machine
        .cpu
        .run_for(&mut NoBus, &mut Request(level), Cycles::ZERO, Cycles(1));
    assert_eq!(machine.cpu.registers.pc, handler(Vector::autovector(level)));
    let ssp = machine.cpu.registers.ssp;
    assert_eq!(machine.peek_long(ssp + FRAME_PC), START + 4);
}

/// Fault 6: the CPU takes TRAP itself, through a format $0 frame of SR,
/// the next instruction's address and the vector offset (CPU32RM
/// 6.2.4, 6.4.1), and RTE pops it (6.2.12).
#[test]
fn fault_6_trap_and_rte_are_taken_by_the_cpu() {
    /// TRAP #5.
    const TRAP_5: u16 = TRAP_0 | 5;
    let (code, starts) = program(&[&[MOVEQ_0], &[TRAP_5], &[NOP]]);
    let mut machine = Machine::new(&code);
    machine.steps(2);
    let vector = Vector::trap(5);
    let ssp = machine.cpu.registers.ssp;
    assert_eq!(machine.cpu.registers.pc, handler(vector));
    assert_eq!(ssp, STACK_TOP - FOUR_WORD_BYTES);
    assert_eq!(machine.peek_word(ssp + FRAME_SR), SUPERVISOR | status::Z);
    assert_eq!(machine.peek_long(ssp + FRAME_PC), starts[2]);
    assert_eq!(
        u32::from(machine.peek_word(ssp + FRAME_FORMAT_VECTOR)),
        vector.offset()
    );
    machine.steps(1);
    assert_eq!(machine.cpu.registers.pc, starts[2]);
    assert_eq!(machine.cpu.registers.ssp, STACK_TOP);
    assert_eq!(machine.cpu.registers.sr(), SUPERVISOR | status::Z);
}

/// Fault 7: MOVEC to VBR moves the vector table (CPU32RM 4.4, MOVEC;
/// 6.1.1), and VBR is in the programming model to read.
#[test]
fn fault_7_movec_to_vbr_moves_the_vector_table() {
    /// The new vector table.
    const TABLE: u32 = 0x4000;
    /// Where it sends TRAP #0.
    const MOVED_HANDLER: u32 = 0x5000;
    let mut machine = Machine::new(&[MOVE_D0_TO_VBR[0], MOVE_D0_TO_VBR[1], TRAP_0]);
    machine.cpu.registers.data[0] = TABLE;
    machine.poke_long(TABLE + Vector::trap(0).offset(), MOVED_HANDLER);
    machine.steps(1);
    assert_eq!(machine.cpu.registers.vbr, TABLE);
    machine.steps(1);
    assert_eq!(machine.cpu.registers.pc, MOVED_HANDLER);
}
