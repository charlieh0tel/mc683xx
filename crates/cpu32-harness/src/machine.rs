//! The core as the harness drives it: registers in and out, one
//! instruction at a time, and how it ended.

use std::mem;

use cpu32_oracle::state::State;
use mc683xx_cpu32::bus::Bus;
use mc683xx_cpu32::cpu::Cpu;
use mc683xx_cpu32::cpu::Halt;
use mc683xx_cpu32::cpu::Stop;
use mc683xx_cpu32::cycles::Cycle;
use mc683xx_cpu32::cycles::Cycles;
use mc683xx_cpu32::exception::Exception;
use mc683xx_cpu32::exception::Vector;
use mc683xx_cpu32::interrupt::Acknowledge;
use mc683xx_cpu32::interrupt::Acknowledged;
use mc683xx_cpu32::interrupt::InterruptLevel;
use mc683xx_cpu32::interrupt::Interrupts;
use mc683xx_cpu32::memory::Memory;
use mc683xx_cpu32::registers::Registers;
use mc683xx_target::Address;
use mc683xx_target::Register;

/// The 68331's address lines, ADDR[23:0] (MC68331UM 4.4.1.1).
pub const ADDRESS_BITS: u32 = 24;
/// The fewest address lines `Memory` takes: a placeholder while a
/// core's memory moves to a new core.
const PLACEHOLDER_ADDRESS_BITS: u32 = 12;

/// No interrupt sources.
#[derive(Debug)]
struct NoInterrupts;

impl Interrupts for NoInterrupts {
    fn level(&self) -> InterruptLevel {
        InterruptLevel::NONE
    }

    fn acknowledge(&mut self, _level: InterruptLevel, _now: Cycle) -> Acknowledged {
        Acknowledge::Spurious.into()
    }
}

/// How one instruction ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ended {
    /// It completed.
    Completed,
    /// An illegal instruction or line A or F: the core does not
    /// implement the opcode, or not with these extension words.
    Unimplemented,
    /// Another exception, as raised, which the core took
    /// (`Cpu::exception_taken`): its registers are the handler's.
    Exception(Exception),
    /// The CPU halted.
    Halted(Halt),
}

/// Replaces `cpu` with a new core on its memory, so nothing of the last
/// instruction carries over: the core keeps state of its own between
/// instructions (a stop, the last instruction's timing tail).
pub fn renew(cpu: &mut Cpu) {
    let placeholder = Memory::new(PLACEHOLDER_ADDRESS_BITS).expect("12 address lines");
    *cpu = Cpu::new(mem::replace(&mut cpu.memory, placeholder));
}

/// A0-A6, as `State::address` holds them; A7 is USP or SSP.
const ADDRESS_REGISTERS: [Register; 7] = [
    Register::A0,
    Register::A1,
    Register::A2,
    Register::A3,
    Register::A4,
    Register::A5,
    Register::A6,
];

/// Sets the core's registers to `state`, SR first so A7 lands in the
/// right stack pointer.
pub fn load(registers: &mut Registers, state: &State) {
    registers.set_sr(state.sr);
    registers.data = state.data;
    for (&register, &value) in ADDRESS_REGISTERS.iter().zip(&state.address) {
        registers.set_register(register, value);
    }
    registers.usp = Address(state.usp);
    registers.ssp = Address(state.ssp);
    registers.pc = Address(state.pc);
}

/// The core's registers as a `State`.
#[must_use]
pub fn capture(registers: &Registers) -> State {
    State {
        data: registers.data,
        address: ADDRESS_REGISTERS.map(|register| registers.register(register)),
        usp: registers.usp.0,
        ssp: registers.ssp.0,
        sr: registers.sr(),
        pc: registers.pc.0,
    }
}

/// Runs one instruction: a budget of one cycle runs exactly one.
pub fn step(cpu: &mut Cpu, bus: &mut dyn Bus) -> Ended {
    let run = cpu.run_for(bus, &mut NoInterrupts, Cycle::ZERO, Cycles::from_clocks(1));
    // The exception first: taking it may halt the core, as on a stack
    // the case does not map.
    match (cpu.exception_taken(), run.stop) {
        (Some(Exception::Short { vector, .. }), _)
            if [Vector::ILLEGAL_INSTRUCTION, Vector::LINE_A, Vector::LINE_F].contains(&vector) =>
        {
            Ended::Unimplemented
        }
        (Some(exception), _) => Ended::Exception(exception),
        (None, Stop::Halted(halt)) => Ended::Halted(halt),
        (None, _) => Ended::Completed,
    }
}
