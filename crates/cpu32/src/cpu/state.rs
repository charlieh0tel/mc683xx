//! What a snapshot of the core holds, and [`Cpu`]'s save and restore
//! ([`crate::state`]).

use serde::Deserialize;
use serde::Serialize;

use crate::cpu::Cpu;
use crate::cpu::Halt;
use crate::cpu::Idle;
use crate::cpu::bare::TakeOnce;
use crate::cpu::hooks::Resume;
use crate::cycles::Cycles;
use crate::effective_address::Altered;
use crate::exception::Exception;
use crate::exception::Fault;
use crate::exception::Movem;
use crate::exception::MovemResume;
use crate::interrupt::InterruptLevel;
use crate::memory::MemoryState;
use crate::registers::Registers;
use crate::registers::status;
use crate::state::Layer;
use crate::state::StateError;
use crate::state::VERSION;
use crate::state::decode_layer;
use crate::state::encode_layer;
use mc683xx_target::Address;
use mc683xx_target::Entry;

#[cfg(test)]
mod tests;

/// What a snapshot of the core holds: every field of [`Cpu`] but the
/// hooks and what a restore sets (a block starts at the restored PC),
/// and its memory's regions and allocated slabs of page tables and
/// permissions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct CpuState {
    /// The registers.
    registers: Registers,
    /// [`Cpu::instruction_pc`].
    instruction_pc: Address,
    /// [`Cpu::previous_pc`].
    previous_pc: Address,
    /// [`Cpu::last_tail`].
    last_tail: u8,
    /// [`Cpu::stall`].
    stall: Cycles,
    /// [`Cpu::extra`].
    extra: Cycles,
    /// [`Cpu::refund`].
    refund: Cycles,
    /// [`Cpu::stopped`].
    stopped: bool,
    /// [`Cpu::halted`].
    halt: Option<Halt>,
    /// [`Cpu::background`]'s entry, as ATEMP gives it.
    background: Option<u32>,
    /// [`Cpu::bdm_enabled`].
    bdm_enabled: bool,
    /// [`Cpu::breakpoint_pending`].
    breakpoint_pending: bool,
    /// The trace a fault stop left pending: its instruction's address.
    trace_pending: Option<Address>,
    /// The intercepted exception [`Cpu::take_natively_once`] lets
    /// through.
    native_once: Option<TakeOnce>,
    /// The exception an exception stop ended the last run at, which
    /// the next takes with no stop.
    exception_once: Option<TakeOnce>,
    /// [`Cpu::atemp`].
    atemp: u32,
    /// [`Cpu::pcc`].
    pcc: Address,
    /// [`Cpu::far`].
    far: Address,
    /// [`Cpu::fault_window`].
    fault_window: bool,
    /// [`Cpu::released`].
    released: Option<Fault>,
    /// [`Cpu::altered`].
    altered: Altered,
    /// [`Cpu::movem`].
    movem: Option<Movem>,
    /// [`Cpu::movem_resume`].
    movem_resume: Option<MovemResume>,
    /// [`Cpu::untraced`].
    untraced: bool,
    /// [`Cpu::sampled_level`].
    sampled_level: InterruptLevel,
    /// [`Cpu::sampled_mask`].
    sampled_mask: u8,
    /// [`Cpu::taken`].
    taken: Option<Exception>,
    /// [`Cpu::instructions`].
    instructions: u64,
    /// [`Cpu::resume`].
    resume: Option<(Address, Resume)>,
    /// [`Cpu::loopable`].
    loopable: Option<Address>,
    /// [`Cpu::loop_mode`].
    loop_mode: Option<Address>,
    /// The memory's address lines, as a mask.
    address_mask: u32,
    /// The memory's regions, page tables and permissions.
    memory: MemoryState,
}

impl CpuState {
    /// Whether it holds what a run could have saved: SR's unimplemented
    /// bits clear (CPU32RM 2.3.1.3), the altered registers address
    /// registers, the sampled mask a level.  Function codes and
    /// interrupt levels are checked as they decode.
    fn validate(&self) -> Result<(), StateError> {
        if self.registers.sr() & !status::IMPLEMENTED != 0 {
            return Err(StateError::invalid("status register"));
        }
        if !self.altered.is_valid() {
            return Err(StateError::invalid("altered address registers"));
        }
        if InterruptLevel::try_from(self.sampled_mask).is_err() {
            return Err(StateError::invalid("sampled interrupt mask"));
        }
        Ok(())
    }
}

impl Cpu {
    /// The core's whole state, as a snapshot: registers, the run
    /// loop's timing and exception state, and memory (its regions,
    /// ROM's included, page tables and permissions).  Not the hooks.
    ///
    /// # Errors
    ///
    /// None the core gives today; a `Result` as every other layer's
    /// `save` gives, so a state the core cannot save can be refused
    /// later without a break.
    pub fn save(&self) -> Result<Vec<u8>, StateError> {
        let memory = &self.memory;
        let state = CpuState {
            registers: self.registers.clone(),
            instruction_pc: self.instruction_pc,
            previous_pc: self.previous_pc,
            last_tail: self.last_tail_byte(),
            stall: self.stall,
            extra: self.extra,
            refund: self.refund,
            stopped: self.stopped,
            halt: self.halted(),
            background: self.background().map(Entry::atemp),
            bdm_enabled: self.bdm_enabled,
            breakpoint_pending: self.bare.breakpoint_pending,
            trace_pending: self.bare.trace_pending,
            native_once: self.bare.native_once,
            exception_once: self.bare.exception_once,
            atemp: self.atemp,
            pcc: self.pcc,
            far: self.far,
            fault_window: self.fault_window,
            released: self.released,
            altered: self.altered,
            movem: self.movem,
            movem_resume: self.movem_resume,
            untraced: self.untraced,
            sampled_level: self.sampled_level,
            sampled_mask: self.sampled_mask,
            taken: self.taken,
            instructions: self.instructions,
            resume: self.resume,
            loopable: self.loopable,
            loop_mode: self.loop_mode,
            address_mask: memory.address_mask,
            memory: memory.state(),
        };
        Ok(encode_layer(Layer::Cpu, VERSION, &state))
    }

    /// Restores a snapshot [`save`](Self::save) made of a CPU with the same
    /// address lines and regions of the same sizes; its hooks stay.  The
    /// instruction at the restored PC starts a block, as after a jump.  On
    /// an error the CPU is unchanged.
    ///
    /// # Errors
    ///
    /// [`StateError::NotSnapshot`] for bytes no layer saved,
    /// [`StateError::Layer`] for another layer's snapshot, [`decode`](crate::state::decode)'s;
    /// [`StateError::Mismatch`] for a snapshot of a CPU
    /// with other address lines or regions of other sizes, or page tables
    /// that do not fit them; [`StateError::Invalid`] for one no run could
    /// have saved.
    pub fn restore(&mut self, snapshot: &[u8]) -> Result<(), StateError> {
        let state: CpuState = decode_layer(Layer::Cpu, VERSION, snapshot)?;
        if state.address_mask != self.memory.address_mask {
            return Err(StateError::mismatch("address lines"));
        }
        state.validate()?;
        self.memory
            .restore(state.memory)
            .map_err(StateError::mismatch)?;
        self.registers = state.registers;
        self.instruction_pc = state.instruction_pc;
        self.previous_pc = state.previous_pc;
        self.last_tail = u32::from(state.last_tail);
        self.stall = state.stall;
        self.extra = state.extra;
        self.refund = state.refund;
        self.stopped = state.stopped;
        self.idle = state.halt.map(Idle::Halted).or_else(|| {
            state
                .background
                .map(|atemp| Idle::Background(Entry::from_atemp(atemp)))
        });
        self.bdm_enabled = state.bdm_enabled;
        self.bare.breakpoint_pending = state.breakpoint_pending;
        self.bare.trace_pending = state.trace_pending;
        self.bare.native_once = state.native_once;
        self.bare.exception_once = state.exception_once;
        self.atemp = state.atemp;
        self.pcc = state.pcc;
        self.far = state.far;
        self.fault_window = state.fault_window;
        self.released = state.released;
        self.altered = state.altered;
        self.movem = state.movem;
        self.movem_resume = state.movem_resume;
        self.untraced = state.untraced;
        self.sampled_level = state.sampled_level;
        self.sampled_mask = state.sampled_mask;
        self.taken = state.taken;
        self.instructions = state.instructions;
        self.block_start = true;
        self.left_pc = self.registers.pc;
        self.resume = state.resume;
        self.loopable = state.loopable;
        self.loop_mode = state.loop_mode;
        self.stop_request = None;
        Ok(())
    }
}
