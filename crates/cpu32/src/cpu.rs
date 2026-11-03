//! The CPU, the context an instruction runs in, and the run loop.
//!
//! An instruction's bus cycles are served from the core's memory or
//! sent to the [`Bus`]; the run loop takes exceptions, traces and
//! interrupts between instructions (CPU32RM 6).

use std::mem;

use crate::bus::Bus;
use crate::bus::BusAccess;
use crate::bus::BusError;
use crate::cycles::Cycles;
use crate::effective_address::Altered;
use crate::exception;
use crate::exception::Breakpoint;
use crate::exception::CycleKind;
use crate::exception::Exception;
use crate::exception::Fault;
use crate::exception::HARDWARE_BREAKPOINT_NUMBER;
use crate::exception::Movem;
use crate::exception::MovemResume;
use crate::exception::Processed;
use crate::exception::Recovery;
use crate::exception::Vector;
use crate::instructions;
use crate::instructions::Flow;
use crate::instructions::Handler;
use crate::instructions::OPCODES;
use crate::interrupt::Acknowledge;
use crate::interrupt::InterruptLevel;
use crate::interrupt::Interrupts;
use crate::memory::Memory;
use crate::memory::Permissions;
use crate::memory::Space;
use crate::memory::watch;
use crate::registers::Registers;
use crate::registers::status;
use crate::timing;
use mc683xx_target::address::Address;
use mc683xx_target::entry::Entry;
use mc683xx_target::function_code::FunctionCode;
use mc683xx_target::size::Size;

pub mod bare;
pub mod control;
pub mod hooks;
pub mod state;

use bare::Bare;
use bare::Limits;

use hooks::HookId;
use hooks::Hooks;
use hooks::Resume;

/// Why a run ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Stop {
    /// It spent its budget.
    Budget,
    /// The CPU is stopped by `stop` or `lpstop` and waits for an
    /// interrupt; the rest of the budget passed waiting.
    Stopped,
    /// The CPU halted; only reset restarts it.
    Halted(Halt),
    /// A hook stopped it: the first to, if several did at once.
    Hook(HookId),
    /// The bus asked it to stop ([`Bus::stop_requested`]), once the
    /// instruction whose cycle asked ended.
    Requested,
    /// The CPU is in background mode, for the entry ATEMP reports
    /// (CPU32RM 7.2.3); only a debugger's GO, CALL or a reset resumes
    /// it.
    Background(Entry),
    /// A fault stopped it rather than being taken
    /// ([`Cpu::set_stop_on_fault`]), the registers as the instruction,
    /// or the exception processing after one, began:
    /// [`Cpu::fault_stop`] says which fault.
    Fault,
}

/// Why the CPU halted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[non_exhaustive]
pub enum Halt {
    /// A bus or address error while fetching the reset vectors, taking
    /// another bus or address error, or loading a bus error frame in
    /// `rte` (CPU32RM 6.2.1 to 6.2.3; MC68331UM 4.5.5.2).
    DoubleBusFault,
}

/// PCC after a double bus fault out of reset (CPU32RM 7.2.5.3; 7.2.3
/// gives $FFFFFFFF instead: PLAN.md, Background debug mode).
const PCC_RESET_DOUBLE_FAULT: u32 = 0x0000_0001;

/// Why the CPU executes nothing, until what ends it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Idle {
    /// Halted, until reset.
    Halted(Halt),
    /// In background mode (CPU32RM 7.2).
    Background(Entry),
}

impl Idle {
    /// The stop a run ends in at once.
    const fn stop(self) -> Stop {
        match self {
            Self::Halted(halt) => Stop::Halted(halt),
            Self::Background(entry) => Stop::Background(entry),
        }
    }

    /// Why it halted, if halted.
    const fn halt(self) -> Option<Halt> {
        match self {
            Self::Halted(halt) => Some(halt),
            Self::Background(_) => None,
        }
    }
}

/// A run's outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct Run {
    /// The cycles it took.
    pub cycles: Cycles,
    /// Why it ended.
    pub stop: Stop,
}

impl Run {
    /// A run that took `cycles` and ended for `stop`: a chip's own run,
    /// or a host's stand-in for one.
    #[must_use]
    pub const fn new(cycles: Cycles, stop: Stop) -> Self {
        Self { cycles, stop }
    }
}

/// The CPU32: its registers, its memory, and what timing and exception
/// processing carry from one instruction to the next.
#[derive(Debug)]
pub struct Cpu {
    /// The programming model.
    pub registers: Registers,
    /// The memory the core serves itself.
    pub memory: Memory,
    /// The address of the instruction executing.
    instruction_pc: Address,
    /// The address of the instruction before it: the current
    /// instruction PC an address error on an odd PC stacks (CPU32RM
    /// 6.2.3).
    previous_pc: Address,
    /// The tail of the last instruction, for overlap (CPU32RM 8.1.6).
    last_tail: u8,
    /// Clocks the instruction's bus cycles took beyond the tables'.
    stall: Cycles,
    /// Clocks an instruction adds to its table entry for what the
    /// table cannot know from the opcode: a branch taken, a shift
    /// count, a MOVEM register list.
    extra: Cycles,
    /// Clocks an instruction takes from its table entry: a full format
    /// extension word cheaper than the brief, LPSTOP on TBL's opcode.
    refund: Cycles,
    /// Whether `stop` or `lpstop` stopped it.
    stopped: bool,
    /// Why it executes nothing, while it does not.
    pub(crate) idle: Option<Idle>,
    /// Whether background mode is enabled: BKPT low at RESET's rising
    /// edge latches it (CPU32RM 7.2.1).
    pub(crate) bdm_enabled: bool,
    /// ATEMP: the entry source on entering background mode, then BDM's
    /// temporary (CPU32RM 7.2.3).
    pub(crate) atemp: u32,
    /// PCC: the first word of the last instruction executed before
    /// background mode (CPU32RM 7.2.5.3).
    pub(crate) pcc: Address,
    /// FAR: the address of the last bus or address error's cycle
    /// (CPU32RM 7.2.5.1).  Unlike the chip's, not moved by cycles that
    /// end normally (PLAN.md, Background debug mode).
    pub(crate) far: Address,
    /// Set from taking a bus or address error, or reset, until the
    /// handler's first opcode is fetched: a bus or address error then
    /// is a double bus fault (CPU32RM 6.2.2; MC68331UM 4.5.5.2).
    pub(crate) fault_window: bool,
    /// A released write's fault, taken when the instruction ends
    /// (CPU32RM 6.3.1.1).
    pub(crate) released: Option<Fault>,
    /// Address registers the instruction's (An)+ and -(An) changed.
    pub(crate) altered: Altered,
    /// MOVEM's progress, while its operands transfer: a fault then is
    /// type III (CPU32RM 6.3.1.3).
    pub(crate) movem: Option<Movem>,
    /// The progress `rte` of a type III frame restored, kept across
    /// interrupts and exceptions until the MOVEM at its PC continues
    /// from it (CPU32RM 6.3.2.6).
    pub(crate) movem_resume: Option<MovemResume>,
    /// The word a breakpoint acknowledge returned, when it raised an
    /// exception in BKPT's place: the exception is charged as its
    /// (CPU32RM 8.3.13).  Set and taken within one instruction.
    pub(crate) replacement: Option<u16>,
    /// Whether the instruction is exempt from tracing: `rte` of a
    /// format $C frame (CPU32RM 6.2.10).
    pub(crate) untraced: bool,
    /// The request level when interrupts were last sampled, for level
    /// 7's transitions (CPU32RM 6.2.11).
    sampled_level: InterruptLevel,
    /// The interrupt mask when interrupts were last sampled.
    sampled_mask: u8,
    /// The first exception the last instruction or interrupt took.
    taken: Option<Exception>,
    /// Instructions executed, or attempted and ended by an exception.
    instructions: u64,
    /// Whether the next instruction starts a block: the first after a
    /// change of flow, an exception, reset, restore, or the host's
    /// jump.
    block_start: bool,
    /// Where the last run, reset or restore left PC.  A run that finds
    /// PC elsewhere starts a block there: the host jumped.
    left_pc: Address,
    /// The host's hooks.
    hooks: Box<Hooks>,
    /// Why the run is to stop once this instruction ends: a hook or
    /// the bus asked.
    stop_request: Option<Stop>,
    /// Where a code or block hook stopped the run, to resume past it.
    resume: Option<(Address, Resume)>,
    /// The address of the last instruction if it can be looped: one
    /// word, completed with no exception and no change of flow
    /// (CPU32RM 1.1.2).
    loopable: Option<Address>,
    /// The address of the DBcc whose loop runs in loop mode, from the
    /// DBcc that enters it until an instruction leaves the loop or an
    /// exception is taken (CPU32RM 1.1.2).
    loop_mode: Option<Address>,
    /// Whether `began` is kept: while a hook or a setting reads it.
    capture: bool,
    /// The registers as the instruction began, or the trace, hardware
    /// breakpoint or interrupt processing after one, while `capture`:
    /// what a fault stop restores (PLAN.md, Faults).
    began: Registers,
    /// The SR bits that trace: T1 and T0, or none with the untraced
    /// deviation.
    trace_mask: u16,
    /// Whether exception processing is stacking a frame.
    stacking: bool,
    /// Whether a long operand is one access: offered to the bus whole
    /// first ([`Bus::read_long`], `write_long`), and shown to the memory
    /// hooks once, as size 4, however many cycles it took.
    long_bus: bool,
    /// Whether a long operand's cycles are running under
    /// [`long_bus`](Self::long_bus), so the memory hooks wait for the whole
    /// operand.
    long_operand: bool,
    /// The bare host's settings.
    pub(crate) bare: Box<Bare>,
}

/// The interrupt mask that admits no maskable level.
const MASK_ALL: u8 = InterruptLevel::NONMASKABLE.get();

impl Cpu {
    /// A CPU on `memory`, its registers zero, not yet reset.
    ///
    /// # Examples
    ///
    /// A CPU on a page of RAM in both spaces, nothing on its bus, runs
    /// MOVEQ #42,D0 from where the host puts PC:
    ///
    /// ```
    /// use mc683xx_target::address::Address;
    /// use mc683xx_cpu32::bus::Bus;
    /// use mc683xx_cpu32::bus::BusAccess;
    /// use mc683xx_cpu32::bus::BusError;
    /// use mc683xx_cpu32::bus::Read;
    /// use mc683xx_cpu32::cpu::Cpu;
    /// use mc683xx_cpu32::cycles::Cycles;
    /// use mc683xx_cpu32::interrupt::Acknowledge;
    /// use mc683xx_cpu32::interrupt::Acknowledged;
    /// use mc683xx_cpu32::interrupt::InterruptLevel;
    /// use mc683xx_cpu32::interrupt::Interrupts;
    /// use mc683xx_cpu32::memory::Backing;
    /// use mc683xx_cpu32::memory::Memory;
    /// use mc683xx_cpu32::memory::PAGE_BYTES;
    /// use mc683xx_cpu32::memory::Page;
    /// use mc683xx_cpu32::memory::Space;
    ///
    /// /// A bus with nothing on it.
    /// struct NoBus;
    ///
    /// impl Bus for NoBus {
    ///     fn read(&mut self, _access: BusAccess) -> Result<Read, BusError> {
    ///         Err(BusError)
    ///     }
    ///
    ///     fn write(&mut self, _access: BusAccess, _value: u32) -> Result<Cycles, BusError> {
    ///         Err(BusError)
    ///     }
    /// }
    ///
    /// /// No interrupt sources.
    /// struct NoInterrupts;
    ///
    /// impl Interrupts for NoInterrupts {
    ///     fn level(&self) -> InterruptLevel {
    ///         InterruptLevel::NONE
    ///     }
    ///
    ///     fn acknowledge(&mut self, _level: InterruptLevel, _now: Cycles) -> Acknowledged {
    ///         Acknowledge::Spurious.into()
    ///     }
    /// }
    ///
    /// let mut memory = Memory::new(24)?;
    /// let region = memory.add_region(vec![0; PAGE_BYTES as usize]);
    /// let backing = Some(Backing { region, offset: 0 });
    /// let ram = Page {
    ///     read: backing,
    ///     write: backing,
    ///     ..Page::BUS
    /// };
    /// for space in [Space::User, Space::Supervisor] {
    ///     memory.map(space, Address(0), PAGE_BYTES, ram)?;
    /// }
    /// memory.region_mut(region)[0x400..0x402].copy_from_slice(&0x702a_u16.to_be_bytes());
    ///
    /// let mut cpu = Cpu::new(memory);
    /// cpu.registers.pc = 0x400;
    /// // A budget of one clock runs one instruction.
    /// cpu.run_for(&mut NoBus, &mut NoInterrupts, Cycles::ZERO, Cycles(1));
    /// assert_eq!(cpu.registers.data[0], 42);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    #[must_use]
    pub fn new(memory: Memory) -> Self {
        Self {
            registers: Registers::default(),
            memory,
            instruction_pc: Address::default(),
            previous_pc: Address::default(),
            last_tail: 0,
            stall: Cycles::ZERO,
            extra: Cycles::ZERO,
            refund: Cycles::ZERO,
            stopped: false,
            idle: None,
            bdm_enabled: false,
            atemp: 0,
            pcc: Address(0),
            far: Address(0),
            fault_window: false,
            released: None,
            altered: Altered::default(),
            movem: None,
            movem_resume: None,
            replacement: None,
            untraced: false,
            sampled_level: InterruptLevel::NONE,
            sampled_mask: MASK_ALL,
            taken: None,
            instructions: 0,
            block_start: true,
            left_pc: Address::default(),
            hooks: Box::default(),
            stop_request: None,
            resume: None,
            loopable: None,
            loop_mode: None,
            capture: false,
            began: Registers::default(),
            trace_mask: status::T1 | status::T0,
            stacking: false,
            long_bus: false,
            long_operand: false,
            bare: Box::default(),
        }
    }

    /// Instructions run since the CPU was made: executed, or attempted
    /// and ended by an exception.
    #[must_use]
    pub const fn instructions(&self) -> u64 {
        self.instructions
    }

    /// The reset exception (CPU32RM 6.2.1): tracing off, supervisor
    /// state, interrupt mask 7, VBR zero, then SSP and PC from the two
    /// long words at zero in supervisor program space.  A bus or
    /// address error before the first instruction is fetched is a
    /// double bus fault.  A pending hardware breakpoint is dropped, as
    /// reset clears all other exceptions (6.1.5).
    ///
    /// # Errors
    ///
    /// [`Halt::DoubleBusFault`] if fetching the vectors ends in a bus or
    /// address error, with background mode disabled; enabled, the CPU
    /// enters it instead ([`Cpu::background`]).
    pub fn reset(&mut self, bus: &mut dyn Bus, now: Cycles) -> Result<(), Halt> {
        self.registers
            .set_sr(status::S | status::INTERRUPT_MASK | (self.registers.sr() & status::CCR));
        self.registers.vbr = 0;
        self.stopped = false;
        self.last_tail = 0;
        self.idle = None;
        self.bare.breakpoint_pending = false;
        self.bare.trace_pending = None;
        self.fault_window = true;
        self.released = None;
        self.movem_resume = None;
        self.sampled_level = InterruptLevel::NONE;
        self.sampled_mask = MASK_ALL;
        self.block_start = true;
        self.resume = None;
        self.leave_loop_mode();
        let mut core = Core {
            cpu: self,
            bus,
            now,
        };
        let vector = |core: &mut Core<'_>, vector: Vector| {
            core.read(
                Address(vector.offset()),
                Size::Long,
                FunctionCode::SUPERVISOR_PROGRAM,
            )
        };
        // SSP is loaded before PC is fetched, and the first fault ends
        // the sequence (CPU32RM 6.2.1, Figure 6-2)
        let loaded = vector(&mut core, Vector::RESET_STACK_POINTER)
            .map(|ssp| core.cpu.registers.ssp = ssp)
            .and_then(|()| vector(&mut core, Vector::RESET_PROGRAM_COUNTER));
        match loaded {
            Ok(pc) => {
                self.registers.pc = pc;
                self.left_pc = Address(pc);
                Ok(())
            }
            Err(exception) => {
                let fault = match exception {
                    Exception::Fault(fault) => Some(fault),
                    _ => None,
                };
                self.double_fault(fault.as_ref(), Address(PCC_RESET_DOUBLE_FAULT));
                self.halted().map_or(Ok(()), Err)
            }
        }
    }

    /// The first exception the last instruction, or interrupt acceptance,
    /// of the last [`run_for`](Self::run_for) took, as raised: its own, a bus or
    /// address error, a trace or an interrupt.  [`None`] if it took none.
    /// Test harnesses use it to tell an exception from a completed
    /// instruction.
    #[must_use]
    pub const fn exception_taken(&self) -> Option<Exception> {
        self.taken
    }

    /// Runs whole instructions from `now` until their cycles reach
    /// `budget`, or the CPU stops or halts.  Between instructions it
    /// takes the exceptions they raise, then trace, then an interrupt,
    /// in Table 6-2's order (CPU32RM 6.1.5).  A hook can stop it early,
    /// before an instruction or after one, and the bus after one
    /// ([`Bus::stop_requested`]).  If the host moved PC since the last
    /// run, the instruction there starts a block.
    pub fn run_for(
        &mut self,
        bus: &mut dyn Bus,
        interrupts: &mut dyn Interrupts,
        now: Cycles,
        budget: Cycles,
    ) -> Run {
        if self.registers.pc != self.left_pc.0 {
            self.block_start = true;
        }
        self.bare.end = None;
        let run = self.run_from_boundary(bus, interrupts, now, budget, None);
        self.left_pc = Address(self.registers.pc);
        run
    }

    /// [`run_instructions`](Self::run_instructions), a pending trace
    /// and breakpoint taken first: outside its loop, so a run with none
    /// pending runs as it would without them.
    #[inline]
    fn run_from_boundary(
        &mut self,
        bus: &mut dyn Bus,
        interrupts: &mut dyn Interrupts,
        now: Cycles,
        budget: Cycles,
        limits: Option<Limits>,
    ) -> Run {
        if (self.bare.breakpoint_pending || self.bare.trace_pending.is_some())
            && budget > Cycles::ZERO
        {
            return self.run_after_pending(bus, interrupts, now, budget, limits);
        }
        self.run_instructions(bus, interrupts, now, budget, limits)
    }

    /// [`run_from_boundary`](Self::run_from_boundary) with a trace or a
    /// breakpoint pending: the trace, then the breakpoint, in Table
    /// 6-2's order (CPU32RM 6.1.5), then the instructions in what is
    /// left of `budget`, unless a fault or a hook ended the run in
    /// their exception processing.
    #[cold]
    #[inline(never)]
    fn run_after_pending(
        &mut self,
        bus: &mut dyn Bus,
        interrupts: &mut dyn Interrupts,
        now: Cycles,
        budget: Cycles,
        limits: Option<Limits>,
    ) -> Run {
        self.stop_request = None;
        let mut core = Core {
            cpu: self,
            bus,
            now,
        };
        let traced = core.pending_trace();
        let taken = if core.cpu.bare.breakpoint_pending && core.cpu.stop_request.is_none() {
            core.now = now + traced;
            traced + core.pending_breakpoint()
        } else {
            traced
        };
        if let Some(stop) = self.stop_request.take() {
            return Run {
                cycles: taken,
                stop,
            };
        }
        let mut run =
            self.run_instructions(bus, interrupts, now + taken, taken.until(budget), limits);
        run.cycles += taken;
        run
    }

    /// [`run_for`](Self::run_for)'s loop, and [`run_limited`](Self::run_limited)'s,
    /// which ends at its `limits` too: [`run_for`](Self::run_for)'s limits are
    /// [`None`].  One [`Core`] serves the whole run, its `now` moved to
    /// each instruction's start, and the tables are loaded once.
    fn run_instructions(
        &mut self,
        bus: &mut dyn Bus,
        interrupts: &mut dyn Interrupts,
        now: Cycles,
        budget: Cycles,
        limits: Option<Limits>,
    ) -> Run {
        let first = self.instructions;
        self.stop_request = None;
        if !self.memory.is_special() {
            self.resume = None;
        }
        let tables = Tables::load();
        let requested =
            |cpu: &mut Self, cycles| cpu.stop_request.take().map(|stop| Run { cycles, stop });
        let mut core = Core {
            cpu: self,
            bus,
            now,
        };
        let mut elapsed = Cycles::ZERO;
        while elapsed < budget {
            let cpu = &mut *core.cpu;
            if let Some(idle) = cpu.idle {
                return Run {
                    cycles: elapsed,
                    stop: idle.stop(),
                };
            }
            if let Some(limits) = &limits {
                if let Some(end) = cpu.limit_reached(limits, first) {
                    cpu.bare.end = Some(end);
                    return Run {
                        cycles: elapsed,
                        stop: Stop::Requested,
                    };
                }
                // the steps taken: interrupts pending, then the end
                if limits.steps.is_some_and(|count| {
                    cpu.instructions - first + cpu.bare.exceptions_stepped >= count
                }) {
                    if let Some(cycles) = core.step_boundary(interrupts, now + elapsed) {
                        elapsed += cycles;
                        if let Some(run) = requested(core.cpu, elapsed) {
                            return run;
                        }
                        continue;
                    }
                    core.cpu.bare.end = Some(bare::End::Steps);
                    return Run {
                        cycles: elapsed,
                        stop: Stop::Requested,
                    };
                }
            }
            if let Some(cycles) = core.boundary(interrupts, now + elapsed) {
                elapsed += cycles;
                if let Some(run) = requested(core.cpu, elapsed) {
                    return run;
                }
                continue;
            }
            let cpu = &mut *core.cpu;
            if cpu.stopped {
                // With no budget, a limited run does not wait.
                let unbudgeted = limits.is_some_and(|limits| limits.cycles.is_none());
                return Run {
                    cycles: if unbudgeted { elapsed } else { budget },
                    stop: Stop::Stopped,
                };
            }
            if cpu.memory.is_special()
                && let Some(id) = cpu.before_instruction()
            {
                return Run {
                    cycles: elapsed,
                    stop: Stop::Hook(id),
                };
            }
            elapsed += core.step(tables);
            let cpu = &mut *core.cpu;
            cpu.instructions += 1;
            if cpu.stop_request.is_some() {
                if cpu.bare.end.is_some() && cpu.bare.trace_pending.is_none() {
                    // A fault or intercept stop in the instruction:
                    // nothing ran.
                    cpu.instructions -= 1;
                }
                if let Some(run) = requested(cpu, elapsed) {
                    return run;
                }
            }
        }
        Run {
            cycles: elapsed,
            stop: core.cpu.idle.map_or(Stop::Budget, Idle::stop),
        }
    }

    /// The end `limits` make before the next instruction, if one, the
    /// run having started at instruction count `first`.
    fn limit_reached(&self, limits: &Limits, first: u64) -> Option<bare::End> {
        if limits.until == Some(Address(self.registers.pc)) {
            return Some(bare::End::Until);
        }
        limits
            .instructions
            .filter(|&count| self.instructions - first >= count)
            .map(|_| bare::End::Instructions)
    }

    /// The instruction at `instruction_pc` as the last, if it can be
    /// looped, having completed with `flow` and no exception: one word,
    /// so PC is past it, and no change of flow (CPU32RM 1.1.2).  The
    /// changes of flow are those trace on change of flow traces
    /// (6.2.10), writes of SR among them.
    fn loopable_after(&self, flow: Flow, instruction_pc: Address) -> Option<Address> {
        (flow == Flow::Next && self.registers.pc == instruction_pc.offset(2).0)
            .then_some(instruction_pc)
    }

    /// Stays in loop mode for the instruction at `instruction_pc`, after
    /// one at `previous_pc`, if it is the loop's next: the looped
    /// instruction after its DBcc, or the DBcc after its looped
    /// instruction completed as it can be looped.  Leaves it otherwise.
    /// The DBcc's tail is zero after its taken and loop mode rows alike
    /// (CPU32RM 8.3.11), not the condition-true row's the run loop kept.
    fn continue_loop_mode(&mut self, instruction_pc: Address, previous_pc: Address) {
        let Some(dbcc) = self.loop_mode else {
            return;
        };
        let looped = Address(dbcc.0.wrapping_sub(LOOPED_WORD));
        if previous_pc == dbcc {
            self.last_tail = 0;
        }
        let continues = if instruction_pc == dbcc {
            self.loopable == Some(looped)
        } else {
            instruction_pc == looped && previous_pc == dbcc
        };
        if !continues {
            self.loop_mode = None;
        }
    }

    /// Keeps the registers as an instruction, or the exception
    /// processing after one, begins in `began`, while `capture`.
    #[inline(always)]
    fn capture_began(&mut self) {
        if self.capture {
            self.began.clone_from(&self.registers);
        }
    }

    /// Leaves loop mode, as an exception does (CPU32RM 1.1.2), and
    /// forgets the last instruction, which an exception came after.
    fn leave_loop_mode(&mut self) {
        self.loop_mode = None;
        self.loopable = None;
    }

    /// Enters background mode for `entry`, the instruction at `pcc` the
    /// last executed: ATEMP takes the entry source (CPU32RM 7.2.3,
    /// 7.2.5.3).
    pub(crate) fn enter_background(&mut self, entry: Entry, pcc: Address) {
        self.idle = Some(Idle::Background(entry));
        self.atemp = entry.atemp();
        self.pcc = pcc;
        self.stopped = false;
        self.block_start = true;
        self.leave_loop_mode();
    }

    /// A double bus fault, the second fault `fault` if known: background
    /// mode if it is enabled, with its SSW in ATEMP, else halted
    /// (CPU32RM Table 7-1, Table 7-2).
    pub(crate) fn double_fault(&mut self, fault: Option<&Fault>, pcc: Address) {
        if let Some(fault) = fault {
            self.far = fault.address;
        }
        if self.bdm_enabled {
            let ssw = fault.map_or(0, exception::double_fault_ssw);
            self.enter_background(Entry::DoubleBusFault { ssw }, pcc);
        } else {
            self.idle = Some(Idle::Halted(Halt::DoubleBusFault));
        }
    }

    /// Charges `cost` after the last instruction, less their overlap
    /// (CPU32RM 8.1.6).
    fn spend(&mut self, cost: timing::Cost) -> Cycles {
        let overlap = self.last_tail.min(cost.head);
        self.last_tail = cost.tail;
        Cycles(u64::from(cost.cycles.saturating_sub(u16::from(overlap))))
    }
}

/// The bytes of the one-word instruction loop mode loops (CPU32RM
/// 1.1.2, Figure 1-1).
const LOOPED_WORD: u32 = 2;

/// The fault of a misaligned access's byte cycle at `address`.
const fn misaligned_fault(
    address: Address,
    function_code: FunctionCode,
    cycle: CycleKind,
    size: Size,
    data: u32,
) -> Fault {
    Fault {
        vector: Vector::BUS_ERROR,
        address,
        function_code,
        size: Size::Byte,
        long: matches!(size, Size::Long),
        cycle,
        data,
        read_modify_write: false,
    }
}

/// RTE's opcode, whose bus errors cost their own (CPU32RM 8.3.14).
const RTE: u16 = 0x4e73;
/// BKPT's opcodes, $4848 to $484F, whose acknowledge ends in a bus
/// error as an illegal instruction exception.
const BKPT_MASK: u16 = 0xfff8;
/// BKPT's fixed bits.
const BKPT: u16 = 0x4848;
/// The $F8xx opcodes, TBL among them, whose second word can be illegal
/// (CPU32RM 8.3.13, F-line (Second word illegal)).
const LINE_F_SECOND_WORD_MASK: u16 = 0xff00;
/// Their fixed bits.
const LINE_F_SECOND_WORD: u16 = 0xf800;

/// What an exception raised by an instruction costs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExceptionCost {
    /// Its row, in place of the opcode's table cost.
    InPlace(timing::Cost),
    /// The opcode's table cost and these clocks beyond it.
    Beyond(Cycles),
}

/// The cost of the exception of `vector` the instruction `opcode`
/// raised (CPU32RM 8.3.12 to 8.3.14).  Traps cost their opcode's
/// table cost, beyond it what [`timing::trap_taken`] says.
fn exception_cost(vector: Vector, opcode: Option<u16>) -> ExceptionCost {
    let opcode = opcode.unwrap_or_default();
    match vector {
        Vector::ILLEGAL_INSTRUCTION if opcode & BKPT_MASK == BKPT => {
            ExceptionCost::InPlace(timing::exception::BKPT_BUS_ERROR)
        }
        Vector::ILLEGAL_INSTRUCTION => ExceptionCost::InPlace(timing::exception::ILLEGAL),
        Vector::LINE_A => ExceptionCost::InPlace(timing::exception::LINE_A),
        Vector::LINE_F if opcode & LINE_F_SECOND_WORD_MASK == LINE_F_SECOND_WORD => {
            ExceptionCost::InPlace(timing::exception::line_f_second_word(opcode))
        }
        Vector::LINE_F => ExceptionCost::InPlace(timing::exception::LINE_F),
        Vector::PRIVILEGE_VIOLATION => {
            ExceptionCost::InPlace(timing::exception::PRIVILEGE_VIOLATION)
        }
        Vector::FORMAT_ERROR => ExceptionCost::InPlace(timing::exception::FORMAT_ERROR),
        Vector::ZERO_DIVIDE => {
            ExceptionCost::Beyond(Cycles(u64::from(timing::exception::ZERO_DIVIDE.cycles)))
        }
        Vector::TRACE => ExceptionCost::Beyond(Cycles(u64::from(timing::exception::TRACE.cycles))),
        _ => ExceptionCost::Beyond(timing::trap_taken(opcode)),
    }
}

/// The tables an instruction runs and is charged from, loaded once a
/// run, not once an instruction.
#[derive(Clone, Copy)]
struct Tables {
    /// Each opcode's handler.
    handlers: &'static [Handler; OPCODES],
    /// Each opcode's cost.
    costs: &'static [timing::Cost; OPCODES],
}

impl Tables {
    /// The tables, built if this is the first run.
    fn load() -> Self {
        Self {
            handlers: instructions::table(),
            costs: timing::table(),
        }
    }

    /// The cost of `opcode`.
    fn cost(self, opcode: u16) -> timing::Cost {
        self.costs[usize::from(opcode)]
    }
}

/// An instruction's context: the CPU and the bus its cycles reach.
pub(crate) struct Core<'a> {
    /// The CPU.
    pub(crate) cpu: &'a mut Cpu,
    /// The bus.
    bus: &'a mut dyn Bus,
    /// When the instruction started.
    now: Cycles,
}

impl Core<'_> {
    /// The address of the instruction executing.
    pub(crate) const fn instruction_pc(&self) -> Address {
        self.cpu.instruction_pc
    }

    /// Adds clocks the timing table cannot know from the opcode.
    pub(crate) fn charge(&mut self, cycles: Cycles) {
        self.cpu.extra += cycles;
    }

    /// Takes clocks from the instruction's table entry the opcode
    /// cannot tell it does without.
    pub(crate) fn refund(&mut self, cycles: Cycles) {
        self.cpu.refund += cycles;
    }

    /// Charges `entry`, the table entry of a word executed in BKPT's
    /// place, less the clocks it refunded, which come from that entry
    /// and not BKPT's.
    pub(crate) fn charge_replacement(&mut self, entry: Cycles) {
        let refund = mem::take(&mut self.cpu.refund);
        self.charge(refund.until(entry));
    }

    /// Charges or refunds `adjustment`.
    pub(crate) fn adjust(&mut self, adjustment: timing::Adjustment) {
        match adjustment {
            timing::Adjustment::More(cycles) => self.charge(cycles),
            timing::Adjustment::Fewer(cycles) => self.refund(cycles),
        }
    }

    /// Whether the DBcc executing is in loop mode (CPU32RM 1.1.2): then
    /// it fetches nothing, so the wait states of its words are not
    /// charged.  Call it once its words are fetched.
    pub(crate) fn looping(&mut self) -> bool {
        let looping = self.cpu.loop_mode == Some(self.instruction_pc());
        if looping {
            self.cpu.stall = Cycles::ZERO;
        }
        looping
    }

    /// Enters loop mode, or stays in it, for the DBcc executing, taken
    /// with a displacement of -4: if the instruction before it was the
    /// word it branches to and can be looped (CPU32RM 1.1.2).
    pub(crate) fn enter_loop_mode(&mut self) {
        let dbcc = self.instruction_pc();
        if self.cpu.loopable == Some(Address(dbcc.0.wrapping_sub(LOOPED_WORD))) {
            self.cpu.loop_mode = Some(dbcc);
        }
    }

    /// The tail of the instruction before this one (CPU32RM 8.1.6).
    pub(crate) const fn last_tail(&self) -> u8 {
        self.cpu.last_tail
    }

    /// Asserts RESET for the `reset` instruction (CPU32RM 4.4, RESET):
    /// the chip resets its external peripherals and other modules, and may
    /// ask the run to stop after it, as after a cycle.
    pub(crate) fn reset_instruction(&mut self) {
        self.bus.reset_instruction(self.now);
        self.bus_stop_requested();
    }

    /// The illegal instruction exception for the instruction executing
    /// (CPU32RM 6.2.8): its own address stacked.
    pub(crate) const fn illegal(&self) -> Exception {
        Exception::Short {
            vector: Vector::ILLEGAL_INSTRUCTION,
            pc: self.instruction_pc(),
        }
    }

    /// The privilege violation for the instruction executing (CPU32RM
    /// 6.2.9): its own address stacked.
    pub(crate) const fn privilege_violation(&self) -> Exception {
        Exception::Short {
            vector: Vector::PRIVILEGE_VIOLATION,
            pc: self.instruction_pc(),
        }
    }

    /// Records MOVEM's progress before each operand transfer: a fault
    /// in it is then type III, stacking `attempted` and `next` (CPU32RM
    /// 6.3.1.3).
    pub(crate) const fn movem_progress(&mut self, attempted: u8, next: Address) {
        self.cpu.movem = Some(Movem { attempted, next });
    }

    /// The progress MOVEM continues from, once, when `rte` of a type
    /// III frame returned to it (CPU32RM 6.3.2.6): only the MOVEM at
    /// the frame's return PC takes it.  Call it once MOVEM's own words
    /// are fetched, so a fault fetching them, type II, leaves it for
    /// the restart.
    pub(crate) fn movem_continuation(&mut self) -> Option<Movem> {
        let pc = self.instruction_pc();
        self.cpu
            .movem_resume
            .take_if(|resume| resume.pc == pc)
            .map(|resume| resume.progress)
    }

    /// The boundary before an instruction, at `now`: the registers
    /// captured if asked, the stall, the extra and refunded clocks and
    /// the exception taken cleared, then an interrupt taken if one is
    /// due: its cycles, if one was.  Always inlined, as `step` is.
    #[inline(always)]
    fn boundary(&mut self, interrupts: &mut dyn Interrupts, now: Cycles) -> Option<Cycles> {
        let cpu = &mut *self.cpu;
        cpu.capture_began();
        cpu.stall = Cycles::ZERO;
        cpu.extra = Cycles::ZERO;
        cpu.refund = Cycles::ZERO;
        cpu.taken = None;
        self.now = now;
        let processed = self.interrupt(interrupts)?;
        let cpu = &mut *self.cpu;
        cpu.leave_loop_mode();
        if processed == Processed::Stopped {
            // charged when it is taken on resume
            return Some(Cycles::ZERO);
        }
        Some(cpu.spend(timing::exception::INTERRUPT) + cpu.stall + cpu.extra)
    }

    /// [`boundary`](Self::boundary) where a step ends: out of line, so
    /// the run loop's frame and code are as without it.  With no
    /// interrupt taken, what the last instruction left (its registers
    /// as it began, its exception, its clocks) is as it was, so the run
    /// ends as if this look had not been made.
    #[cold]
    #[inline(never)]
    fn step_boundary(&mut self, interrupts: &mut dyn Interrupts, now: Cycles) -> Option<Cycles> {
        let cpu = &*self.cpu;
        let began = cpu.capture.then(|| cpu.began.clone());
        let left = (cpu.stall, cpu.extra, cpu.refund, cpu.taken);
        let taken = self.boundary(interrupts, now);
        if taken.is_none() {
            let cpu = &mut *self.cpu;
            (cpu.stall, cpu.extra, cpu.refund, cpu.taken) = left;
            if let Some(began) = began {
                cpu.began = began;
            }
        }
        taken
    }

    /// One instruction and the exceptions it raises: its cycles.  The
    /// run loop has cleared the stall, the extra and refunded clocks
    /// and the exception taken.  Always inlined: the run loop's code
    /// changes without it ([docs/performance.md, Forced inlining]).
    ///
    /// [docs/performance.md, Forced inlining]: https://github.com/charlieh0tel/mc683xx/blob/main/docs/performance.md#made-since-the-baseline
    #[inline(always)]
    fn step(&mut self, tables: Tables) -> Cycles {
        let cpu = &mut *self.cpu;
        let instruction_pc = Address(cpu.registers.pc);
        let previous_pc = cpu.previous_pc;
        cpu.instruction_pc = instruction_pc;
        cpu.previous_pc = instruction_pc;
        cpu.altered = Altered::default();
        cpu.movem = None;
        cpu.untraced = false;
        if cpu.loop_mode.is_some() {
            cpu.continue_loop_mode(instruction_pc, previous_pc);
        }
        // Tracing follows T1 and T0 as the instruction begins (6.2.10).
        let trace = cpu.registers.sr() & cpu.trace_mask;
        let opcode = match self.fetch_fast() {
            Some(opcode) => Ok(opcode),
            None => self.fetch_word_slow(),
        };
        if self.cpu.loop_mode.is_some() {
            // Loop mode fetches nothing (CPU32RM 1.1.2).
            self.cpu.stall = Cycles::ZERO;
        }
        let opcode = match opcode {
            Ok(opcode) => opcode,
            Err(exception) => {
                return self.step_exceptional(Err(exception), None, trace, previous_pc, tables);
            }
        };
        self.cpu.fault_window = false;
        let step = tables.handlers[usize::from(opcode)](self, opcode);
        // The common case first: the instruction completed, untraced,
        // with no released write's fault.
        if let Ok(flow) = step
            && trace == 0
            && self.cpu.released.is_none()
        {
            let cpu = &mut *self.cpu;
            cpu.block_start |= flow == Flow::Changed;
            cpu.stopped = flow == Flow::Stop;
            cpu.loopable = cpu.loopable_after(flow, instruction_pc);
            let cycles = cpu.spend(tables.cost(opcode));
            return cpu.refund.until(cycles) + cpu.stall + cpu.extra;
        }
        self.step_exceptional(step, Some(opcode), trace, previous_pc, tables)
    }

    /// The rest of [`step`](Self::step) when the instruction `opcode`
    /// ([`None`] if its fetch faulted) did not simply complete: it raised
    /// [`step`](Self::step)'s exception, it is traced (`trace`, SR's trace
    /// bits as it began) or a released write faulted.  Its cycles.
    #[inline(never)]
    fn step_exceptional(
        &mut self,
        step: Result<Flow, Exception>,
        opcode: Option<u16>,
        trace: u16,
        previous_pc: Address,
        tables: Tables,
    ) -> Cycles {
        // A BKPT's replacement that raised an exception is charged as
        // the instruction.
        let opcode = self.cpu.replacement.take().or(opcode);
        let instruction_pc = self.instruction_pc();
        let released = self.cpu.released;
        if let Some(end) = self.cpu.bare_end(step, released, instruction_pc) {
            // Nothing ran, as if an exception were taken.
            self.cpu.leave_loop_mode();
            self.cpu.end_with(end);
            return Cycles::ZERO;
        }
        if matches!(step, Ok(Flow::Changed)) {
            self.cpu.block_start = true;
        }
        let untraced = self.cpu.untraced;
        let traced = |change_of_flow: bool| {
            !untraced && (trace & status::T1 != 0 || trace & status::T0 != 0 && change_of_flow)
        };
        let instruction_cost =
            |cpu: &mut Cpu| opcode.map_or(Cycles::ZERO, |opcode| cpu.spend(tables.cost(opcode)));
        let next_pc = Address(self.cpu.registers.pc);
        let released = self.cpu.released.take();
        self.cpu.taken = match (step, released) {
            (_, Some(fault)) => Some(Exception::Fault(fault)),
            (Err(exception), None) => Some(exception),
            (Ok(_), None) => None,
        };
        let cycles = match (step, released) {
            (Ok(flow), released) => {
                let cycles = instruction_cost(self.cpu);
                let trace_pending = traced(flow == Flow::Changed);
                if let Some(fault) = released {
                    // The instruction completed; its trace waits in TR.
                    self.take_fault(
                        fault,
                        Recovery::Released,
                        next_pc,
                        instruction_pc,
                        trace_pending,
                    );
                    cycles + self.cpu.spend(timing::exception::BUS_ERROR)
                } else if trace_pending {
                    // STOP and LPSTOP traced do not stop (6.2.10).
                    if self.trace() == Processed::Stopped {
                        // charged when it is taken on resume
                        cycles
                    } else {
                        cycles + self.cpu.spend(timing::exception::TRACE)
                    }
                } else {
                    self.cpu.stopped = flow == Flow::Stop;
                    cycles
                }
            }
            (Err(exception), Some(fault)) => {
                // A released write faulted first: CPU32 instructions
                // raise nothing after their last write but `rte`'s
                // requeued trace.
                let trace_pending = exception.vector() == Vector::TRACE || traced(true);
                self.take_fault(
                    fault,
                    Recovery::Released,
                    next_pc,
                    instruction_pc,
                    trace_pending,
                );
                instruction_cost(self.cpu) + self.cpu.spend(timing::exception::BUS_ERROR)
            }
            (Err(Exception::Fault(fault)), None) => {
                self.instruction_fault(fault, previous_pc);
                let cost = if opcode == Some(RTE) {
                    timing::exception::RTE_BUS_ERROR_FOUR_WORD
                } else {
                    timing::exception::BUS_ERROR
                };
                self.cpu.spend(cost)
            }
            (Err(exception @ (Exception::Short { .. } | Exception::Long { .. })), None) => {
                let requeued = exception.vector() == Vector::TRACE;
                let (processed, trace) = self.instruction_exception(exception, traced(true));
                if processed == Processed::Stopped && !requeued {
                    // Nothing ran, as with a fault in the instruction.
                    self.cpu.leave_loop_mode();
                    return Cycles::ZERO;
                }
                let cycles = match exception_cost(exception.vector(), opcode) {
                    // `rte`'s trace, charged when it is taken on resume
                    _ if processed == Processed::Stopped => instruction_cost(self.cpu),
                    ExceptionCost::InPlace(cost) => self.cpu.spend(cost),
                    ExceptionCost::Beyond(cycles) => instruction_cost(self.cpu) + cycles,
                };
                if trace.is_some_and(|trace| trace != Processed::Stopped) {
                    cycles + self.cpu.spend(timing::exception::TRACE)
                } else {
                    cycles
                }
            }
        };
        match (self.cpu.taken, step) {
            (None, Ok(flow)) => self.cpu.loopable = self.cpu.loopable_after(flow, instruction_pc),
            _ => self.cpu.leave_loop_mode(),
        }
        // A refund is never more than the table entry it is taken
        // from; saturating keeps an exception's cost in place of that
        // entry from going below zero.  The stall is bus time, kept.
        self.cpu.refund.until(cycles) + self.cpu.stall + self.cpu.extra
    }

    /// Trace exception processing (CPU32RM 6.2.10) after the instruction
    /// executing completed: a six-word frame with the next PC and the
    /// traced instruction's address.
    fn trace(&mut self) -> Processed {
        let address = self.instruction_pc();
        self.trace_at(address)
    }

    /// [`trace`](Self::trace) of the instruction at `address`, taken
    /// after it completed (6.2.10): the registers captured first, so
    /// that a fault stop stacking its frame keeps the instruction and
    /// leaves the trace pending for the next run.
    fn trace_at(&mut self, address: Address) -> Processed {
        self.cpu.capture_began();
        let pc = Address(self.cpu.registers.pc);
        self.cpu.taken.get_or_insert(Exception::Long {
            vector: Vector::TRACE,
            pc,
            address,
        });
        let processed = self.process_after_instruction(Vector::TRACE, pc, Some(address), None);
        if processed == Processed::Stopped {
            self.cpu.bare.trace_pending = Some(address);
        }
        processed
    }

    /// Exception processing at an instruction boundary, after an
    /// instruction completed: a trace, a hardware breakpoint or an
    /// interrupt (CPU32RM 6.1.5, Table 6-2), with no trace pending
    /// beyond it.  A fault stop in it leaves the hooks at PC as they
    /// were, the instruction there not begun, and charges none of its
    /// bus cycles.
    fn process_after_instruction(
        &mut self,
        vector: Vector,
        pc: Address,
        faulted: Option<Address>,
        level: Option<InterruptLevel>,
    ) -> Processed {
        let (resume, stall) = (self.cpu.resume, self.cpu.stall);
        let processed = self.process(vector, pc, faulted, level, false);
        if processed == Processed::Stopped {
            // its cycles charged when it is taken on resume
            (self.cpu.resume, self.cpu.stall) = (resume, stall);
        }
        processed
    }

    /// A bus or address error that aborted the instruction: type III
    /// while MOVEM transfers its operands, else type II, its (An)+ and
    /// -(An) undone and the instruction restarted (CPU32RM 6.3.1).
    fn instruction_fault(&mut self, fault: Fault, previous_pc: Address) {
        let instruction_pc = self.instruction_pc();
        if let Some(movem) = self.cpu.movem {
            return self.take_fault(
                fault,
                Recovery::Movem(movem),
                instruction_pc,
                instruction_pc,
                false,
            );
        }
        self.restore_altered();
        // An odd PC faults where a change of flow left it; that
        // instruction is the current one (6.2.3).
        let odd_pc = fault.vector == Vector::ADDRESS_ERROR
            && fault.cycle == CycleKind::Fetch
            && fault.address == instruction_pc;
        let current_pc = if odd_pc { previous_pc } else { instruction_pc };
        self.take_fault(fault, Recovery::Restart, instruction_pc, current_pc, false);
    }

    /// An exception the instruction raised with a four- or six-word
    /// frame: group 3 before it executes, its registers untouched and
    /// untraced; group 2 as part of it, then the trace it is due if
    /// `trace_pending` (CPU32RM 6.1.5, Table 6-2).  What became of it,
    /// and of the trace, if one followed.
    fn instruction_exception(
        &mut self,
        exception: Exception,
        trace_pending: bool,
    ) -> (Processed, Option<Processed>) {
        let (vector, pc, faulted) = match exception {
            Exception::Short { vector, pc } => (vector, pc, None),
            Exception::Long {
                vector,
                pc,
                address,
            } => (vector, pc, Some(address)),
            Exception::Fault(fault) => {
                self.instruction_fault(fault, self.cpu.previous_pc);
                return (Processed::Faulted, None);
            }
        };
        if exception.before_execution() {
            self.restore_altered();
            return (self.process(vector, pc, faulted, None, false), None);
        }
        if let Some(address) = faulted.filter(|_| vector == Vector::TRACE) {
            // `rte`'s requeued trace (6.3.2.2), the RTE complete: a
            // trace of its own, which a fault stop leaves pending
            return (self.trace_at(address), None);
        }
        // Group 2 changes the flow to its handler.
        let processed = self.process(vector, pc, faulted, None, trace_pending);
        let trace = (processed == Processed::Taken && trace_pending).then(|| self.trace());
        (processed, trace)
    }

    /// Samples the interrupt request and takes it if it is due: a level
    /// above the mask, or level 7 on its transition to 7 and when the
    /// mask drops from 7 while it stays at 7 (CPU32RM 6.2.11).
    /// Acknowledged, it gives a vector, the autovector of its level, or
    /// the spurious vector; the mask is set to its level.  What became
    /// of the one due, if one was.
    #[inline]
    fn interrupt(&mut self, interrupts: &mut dyn Interrupts) -> Option<Processed> {
        let level = interrupts.level();
        let mask = self.cpu.registers.interrupt_mask();
        let due = if level == InterruptLevel::NONMASKABLE {
            self.cpu.sampled_level != level || self.cpu.sampled_mask == MASK_ALL && mask < MASK_ALL
        } else {
            level.get() > mask
        };
        self.cpu.sampled_level = level;
        self.cpu.sampled_mask = mask;
        due.then(|| self.take_interrupt(interrupts, level))
    }

    /// Acknowledges the interrupt request at `level` and takes it: what
    /// became of it.
    #[cold]
    #[inline(never)]
    fn take_interrupt(
        &mut self,
        interrupts: &mut dyn Interrupts,
        level: InterruptLevel,
    ) -> Processed {
        let acknowledged = interrupts.acknowledge(level, self.now);
        self.bus_stop_requested();
        self.cpu.stall += acknowledged.stall;
        let vector = match acknowledged.answer {
            Acknowledge::Vector(number) => Vector(number),
            Acknowledge::Autovector => Vector::autovector(level),
            Acknowledge::Spurious => Vector::SPURIOUS_INTERRUPT,
        };
        let pc = Address(self.cpu.registers.pc);
        self.cpu.taken = Some(Exception::Short { vector, pc });
        let processed = self.process_after_instruction(vector, pc, None, Some(level));
        self.cpu.sampled_mask = self.cpu.registers.interrupt_mask();
        // taken, it is a step and ends STOP and LPSTOP (CPU32RM 4.4);
        // a fault stop, the registers as before it, took none and
        // leaves the CPU stopped as it was
        if processed != Processed::Stopped {
            self.cpu.bare.exceptions_stepped += 1;
            self.cpu.stopped = false;
        }
        // a breakpoint STOP left pending: acknowledged at the end of
        // the exception processing that ends STOP (CPU32RM 6.2.6), a
        // boundary of its own; a fault stop ended none
        if self.cpu.bare.breakpoint_pending
            && !self.cpu.bdm_enabled
            && self.cpu.idle.is_none()
            && processed != Processed::Stopped
        {
            self.cpu.capture_began();
            let cycles = self.hardware_breakpoint();
            self.charge(cycles);
        }
        processed
    }

    /// A trace a fault stop left pending (PLAN.md, Faults), taken as a
    /// run starts, at the boundary after its instruction, unless the
    /// CPU is halted or in background mode, which leaves it pending.
    /// Its clocks.
    #[cold]
    #[inline(never)]
    fn pending_trace(&mut self) -> Cycles {
        let cpu = &mut *self.cpu;
        if cpu.idle.is_some() {
            return Cycles::ZERO;
        }
        let Some(address) = cpu.bare.trace_pending.take() else {
            return Cycles::ZERO;
        };
        cpu.stall = Cycles::ZERO;
        cpu.extra = Cycles::ZERO;
        cpu.taken = None;
        cpu.leave_loop_mode();
        if self.trace_at(address) == Processed::Stopped {
            return Cycles::ZERO;
        }
        self.cpu.bare.exceptions_stepped += 1;
        let cycles = self.cpu.spend(timing::exception::TRACE);
        cycles + self.cpu.stall + self.cpu.extra
    }

    /// A breakpoint pending as a run starts, at an instruction boundary
    /// after the last instruction's trace and before the interrupt
    /// sampled there, as Table 6-2 orders them (CPU32RM 6.1.5, 6.2.10):
    /// with background mode enabled the CPU enters it, as
    /// [`Cpu::breakpoint`]; disabled, the hardware breakpoint is
    /// acknowledged, unless the CPU is halted or stopped, which leaves
    /// it pending (PLAN.md, Background debug mode).  Its clocks.
    #[cold]
    #[inline(never)]
    fn pending_breakpoint(&mut self) -> Cycles {
        let cpu = &mut *self.cpu;
        if cpu.bdm_enabled {
            cpu.bare.breakpoint_pending = false;
            cpu.breakpoint();
            return Cycles::ZERO;
        }
        if cpu.idle.is_some() || cpu.stopped {
            return Cycles::ZERO;
        }
        cpu.capture_began();
        cpu.stall = Cycles::ZERO;
        cpu.extra = Cycles::ZERO;
        cpu.taken = None;
        let cycles = self.hardware_breakpoint();
        cycles + self.cpu.stall + self.cpu.extra
    }

    /// A pending hardware breakpoint acknowledged, background mode
    /// disabled (CPU32RM 6.2.6, 7.2.2.1; MC68331UM 4.5.4.1.2): the
    /// acknowledge cycle, then, if DSACK ended it, nothing more, the
    /// word on the bus ignored; if BERR did, exception processing
    /// through vector 12 with a six-word frame, the next PC and the
    /// address of the instruction executing when the breakpoint was
    /// sensed, the last executed (6.4.2).  A fault stop in that
    /// processing leaves it pending, as nothing was taken, and charges
    /// nothing.  Its clocks (8.3.13, Breakpoint), less the overlap; the
    /// cycles' wait states go to the stall.
    #[cold]
    #[inline(never)]
    fn hardware_breakpoint(&mut self) -> Cycles {
        let stall = self.cpu.stall;
        self.cpu.bare.breakpoint_pending = false;
        if self
            .breakpoint_acknowledge(HARDWARE_BREAKPOINT_NUMBER, Breakpoint::Hardware)
            .is_some()
        {
            return self.cpu.spend(timing::exception::BREAKPOINT);
        }
        let vector = Vector::HARDWARE_BREAKPOINT;
        let pc = Address(self.cpu.registers.pc);
        let address = self.instruction_pc();
        self.cpu.taken.get_or_insert(Exception::Long {
            vector,
            pc,
            address,
        });
        self.cpu.leave_loop_mode();
        let processed = self.process_after_instruction(vector, pc, Some(address), None);
        if processed == Processed::Stopped {
            // its acknowledge cycle made again on resume
            self.cpu.bare.breakpoint_pending = true;
            self.cpu.stall = stall;
            return Cycles::ZERO;
        }
        self.cpu.spend(timing::exception::BREAKPOINT_BUS_ERROR)
    }

    /// Reads an operand of `size` at `address` in `function_code`'s
    /// space.
    pub(crate) fn read(
        &mut self,
        address: Address,
        size: Size,
        function_code: FunctionCode,
    ) -> Result<u32, Exception> {
        self.access_read(address, size, function_code, CycleKind::Read)
            .map_err(Exception::Fault)
    }

    /// Writes an operand of `size` at `address` in `function_code`'s
    /// space as a released write, an instruction's last (CPU32RM
    /// 6.3.1.1): a fault in it does not stop the instruction but is
    /// taken as type I when it ends, so it never returns one.
    pub(crate) fn write(
        &mut self,
        address: Address,
        size: Size,
        function_code: FunctionCode,
        value: u32,
    ) -> Result<(), Exception> {
        if let Err(fault) = self.access_write(address, size, function_code, value) {
            self.cpu.released.get_or_insert(fault);
        }
        Ok(())
    }

    /// Writes an operand that is not an instruction's last: TAS, MOVEP
    /// but the last write, MOVEM (CPU32RM 6.3.1.2).  A fault in it
    /// aborts the instruction, as type II or, in MOVEM, type III.
    pub(crate) fn write_unreleased(
        &mut self,
        address: Address,
        size: Size,
        function_code: FunctionCode,
        value: u32,
    ) -> Result<(), Exception> {
        self.access_write(address, size, function_code, value)
            .map_err(Exception::Fault)
    }

    /// Reads `size` at `address` in `function_code`'s space as a cycle
    /// of kind `cycle`: a long as two word cycles, high word first.
    pub(crate) fn access_read(
        &mut self,
        address: Address,
        size: Size,
        function_code: FunctionCode,
        cycle: CycleKind,
    ) -> Result<u32, Fault> {
        if self.whole_long(size) {
            self.cpu.long_operand = true;
            let read = self.access_read(address, size, function_code, cycle);
            self.cpu.long_operand = false;
            if let Ok(value) = read {
                let address = self.cpu.memory.mask(address);
                self.watch_long(watch::READ, address, function_code, value);
            }
            return read;
        }
        let fault = |vector, address, remaining| Fault {
            vector,
            address,
            function_code,
            size: remaining,
            long: size == Size::Long,
            cycle,
            data: 0,
            read_modify_write: false,
        };
        if size != Size::Byte && address.is_odd() {
            if self.cpu.bare.deviations.misaligned && cycle != CycleKind::Fetch {
                return self.misaligned_read(address, size, function_code, cycle);
            }
            return Err(fault(Vector::ADDRESS_ERROR, address, size));
        }
        if size == Size::Long
            && self.cpu.long_bus
            && let Some(read) = self.long_read(address, function_code, cycle)
        {
            return read.ok_or_else(|| fault(Vector::BUS_ERROR, address, Size::Long));
        }
        if size == Size::Long {
            let high = self
                .cycle_read(address, Size::Word, function_code, cycle)
                .ok_or_else(|| fault(Vector::BUS_ERROR, address, Size::Long))?;
            let low_address = address.offset(2);
            let low = self
                .cycle_read(low_address, Size::Word, function_code, cycle)
                .ok_or_else(|| fault(Vector::BUS_ERROR, low_address, Size::Word))?;
            Ok(high << 16 | low)
        } else {
            self.cycle_read(address, size, function_code, cycle)
                .ok_or_else(|| fault(Vector::BUS_ERROR, address, size))
        }
    }

    /// Writes `value`, `size` of it, at `address` in `function_code`'s
    /// space: a long as two word cycles, high word first.
    pub(crate) fn access_write(
        &mut self,
        address: Address,
        size: Size,
        function_code: FunctionCode,
        value: u32,
    ) -> Result<(), Fault> {
        if self.whole_long(size) {
            self.cpu.long_operand = true;
            let written = self.access_write(address, size, function_code, value);
            self.cpu.long_operand = false;
            if written.is_ok() {
                let address = self.cpu.memory.mask(address);
                self.watch_long(watch::WRITE, address, function_code, value);
            }
            return written;
        }
        let data = value & size.mask();
        let fault = |vector, address, remaining| Fault {
            vector,
            address,
            function_code,
            size: remaining,
            long: size == Size::Long,
            cycle: CycleKind::Write,
            data,
            read_modify_write: false,
        };
        if size != Size::Byte && address.is_odd() {
            if self.cpu.bare.deviations.misaligned {
                return self.misaligned_write(address, size, function_code, data);
            }
            return Err(fault(Vector::ADDRESS_ERROR, address, size));
        }
        if size == Size::Long
            && self.cpu.long_bus
            && let Some(written) = self.long_write(address, function_code, data)
        {
            return written.ok_or_else(|| fault(Vector::BUS_ERROR, address, Size::Long));
        }
        if size == Size::Long {
            self.cycle_write(address, Size::Word, function_code, data >> 16)
                .ok_or_else(|| fault(Vector::BUS_ERROR, address, Size::Long))?;
            let low_address = address.offset(2);
            self.cycle_write(low_address, Size::Word, function_code, data & 0xffff)
                .ok_or_else(|| fault(Vector::BUS_ERROR, low_address, Size::Word))
        } else {
            self.cycle_write(address, size, function_code, data)
                .ok_or_else(|| fault(Vector::BUS_ERROR, address, size))
        }
    }

    /// Whether an access of `size` is a long operand the memory hooks
    /// see once (`long_bus`), not yet under way, while some page has a
    /// hook or permissions.  A long that faults is not seen; its cycles
    /// before the fault are not either.
    fn whole_long(&self, size: Size) -> bool {
        size == Size::Long
            && self.cpu.long_bus
            && !self.cpu.long_operand
            && self.cpu.memory.is_special()
    }

    /// Reads data space.
    pub(crate) fn read_data(&mut self, address: Address, size: Size) -> Result<u32, Exception> {
        let space = self.cpu.registers.data_space();
        self.read(address, size, space)
    }

    /// Writes data space.
    pub(crate) fn write_data(
        &mut self,
        address: Address,
        size: Size,
        value: u32,
    ) -> Result<(), Exception> {
        let space = self.cpu.registers.data_space();
        self.write(address, size, space, value)
    }

    /// Reads program space.
    pub(crate) fn read_program(&mut self, address: Address, size: Size) -> Result<u32, Exception> {
        let space = self.cpu.registers.program_space();
        self.read(address, size, space)
    }

    /// The word at PC, PC past it: through the fetch page if it serves
    /// it, inlined, else `fetch_word_slow`.
    #[inline]
    pub(crate) fn fetch_word(&mut self) -> Result<u16, Exception> {
        match self.fetch_fast() {
            Some(word) => Ok(word),
            None => self.fetch_word_slow(),
        }
    }

    /// The word at PC, PC past it, if the fetch page serves it.  Always
    /// inlined, so the run loop's opcode fetch is too.
    #[inline(always)]
    fn fetch_fast(&mut self) -> Option<u16> {
        let pc = Address(self.cpu.registers.pc);
        if pc.is_odd() {
            return None;
        }
        let space = Space::of(self.cpu.registers.program_space());
        let (word, stall) = self.cpu.memory.fetch(space, pc)?;
        self.cpu.stall += stall;
        self.cpu.registers.pc = pc.offset(2).0;
        Some(word)
    }

    /// A fetch the fetch page did not serve: an odd PC, a hooked or
    /// protected page, or the bus's.
    #[inline(never)]
    fn fetch_word_slow(&mut self) -> Result<u16, Exception> {
        let pc = Address(self.cpu.registers.pc);
        let space = self.cpu.registers.program_space();
        let word = self
            .access_read(pc, Size::Word, space, CycleKind::Fetch)
            .map_err(Exception::Fault)?;
        self.cpu.registers.pc = pc.offset(2).0;
        Ok(word as u16)
    }

    /// The long word at PC, PC past it.
    pub(crate) fn fetch_long(&mut self) -> Result<u32, Exception> {
        let high = self.fetch_word()?;
        let low = self.fetch_word()?;
        Ok(u32::from(high) << 16 | u32::from(low))
    }

    /// One byte or word read cycle of kind `cycle`, from memory or the
    /// bus; [`None`] on a bus error, its page's permissions' included.
    fn cycle_read(
        &mut self,
        address: Address,
        size: Size,
        function_code: FunctionCode,
        cycle: CycleKind,
    ) -> Option<u32> {
        let address = self.cpu.memory.mask(address);
        if function_code.is_memory_space() {
            let space = Space::of(function_code);
            if let Some((value, stall)) = self.cpu.memory.read(space, address, size) {
                self.cpu.stall += stall;
                return Some(value);
            }
        }
        self.slow_read(address, size, function_code, cycle)
    }

    /// A read cycle the fast path did not serve: on a hooked or
    /// protected page (which serves nothing on the fast path), or the
    /// bus's.  Out of line, to keep the fast path small.
    #[inline(never)]
    fn slow_read(
        &mut self,
        address: Address,
        size: Size,
        function_code: FunctionCode,
        cycle: CycleKind,
    ) -> Option<u32> {
        if function_code.is_memory_space() && self.cpu.memory.is_special() {
            return self.special_read(address, size, function_code, cycle);
        }
        self.bus_read(address, size, function_code, cycle)
    }

    /// A read cycle in a memory space at `address`, on the address lines,
    /// hooked page or not: from memory if the page serves it, else the
    /// bus.
    fn paged_read(
        &mut self,
        address: Address,
        size: Size,
        function_code: FunctionCode,
        cycle: CycleKind,
    ) -> Option<u32> {
        let space = Space::of(function_code);
        if let Some((value, stall)) = self.cpu.memory.read_backed(space, address, size) {
            self.cpu.stall += stall;
            return Some(value);
        }
        self.bus_read(address, size, function_code, cycle)
    }

    /// A read cycle on the bus; a bus error goes to the invalid access
    /// hooks.
    fn bus_read(
        &mut self,
        address: Address,
        size: Size,
        function_code: FunctionCode,
        cycle: CycleKind,
    ) -> Option<u32> {
        let access = self.access(address, size, function_code);
        let read = self.bus.read_with_memory(access, &mut self.cpu.memory);
        self.bus_stop_requested();
        if let Ok(read) = read {
            self.cpu.stall += read.stall;
            return Some(read.value);
        }
        self.invalid(self.invalid_access(address, size, function_code, cycle, None));
        None
    }

    /// One byte or word write cycle, to memory or the bus; [`None`] on a
    /// bus error, its page's permissions' included.
    fn cycle_write(
        &mut self,
        address: Address,
        size: Size,
        function_code: FunctionCode,
        value: u32,
    ) -> Option<()> {
        let address = self.cpu.memory.mask(address);
        if function_code.is_memory_space() {
            let space = Space::of(function_code);
            if let Some(stall) = self.cpu.memory.write(space, address, size, value) {
                self.cpu.stall += stall;
                return Some(());
            }
        }
        self.slow_write(address, size, function_code, value)
    }

    /// A write cycle the fast path did not serve, as `slow_read`.
    #[inline(never)]
    fn slow_write(
        &mut self,
        address: Address,
        size: Size,
        function_code: FunctionCode,
        value: u32,
    ) -> Option<()> {
        if function_code.is_memory_space() && self.cpu.memory.is_special() {
            return self.special_write(address, size, function_code, value);
        }
        self.bus_write(address, size, function_code, value)
    }

    /// A write cycle in a memory space at `address`, on the address
    /// lines, hooked page or not: to memory if the page serves it, else
    /// the bus.
    fn paged_write(
        &mut self,
        address: Address,
        size: Size,
        function_code: FunctionCode,
        value: u32,
    ) -> Option<()> {
        let space = Space::of(function_code);
        if let Some(stall) = self.cpu.memory.write_backed(space, address, size, value) {
            self.cpu.stall += stall;
            return Some(());
        }
        self.bus_write(address, size, function_code, value)
    }

    /// A write cycle on the bus; a bus error goes to the invalid access
    /// hooks.
    fn bus_write(
        &mut self,
        address: Address,
        size: Size,
        function_code: FunctionCode,
        value: u32,
    ) -> Option<()> {
        let access = self.access(address, size, function_code);
        let written = self
            .bus
            .write_with_memory(access, value, &mut self.cpu.memory);
        self.bus_stop_requested();
        if let Ok(stall) = written {
            self.cpu.stall += stall;
            return Some(());
        }
        let access =
            self.invalid_access(address, size, function_code, CycleKind::Write, Some(value));
        self.invalid(access);
        None
    }

    /// A word or long at an odd address as byte cycles, high byte
    /// first: the misaligned deviation, not the CPU32's behavior.
    #[cold]
    fn misaligned_read(
        &mut self,
        address: Address,
        size: Size,
        function_code: FunctionCode,
        cycle: CycleKind,
    ) -> Result<u32, Fault> {
        (0..size.bytes()).try_fold(0, |value, index| {
            let at = address.offset(index);
            let byte = self.cycle_read(at, Size::Byte, function_code, cycle);
            byte.map(|byte| value << 8 | byte)
                .ok_or_else(|| misaligned_fault(at, function_code, cycle, size, 0))
        })
    }

    /// A word or long write at an odd address as byte cycles, as
    /// `misaligned_read`.
    #[cold]
    fn misaligned_write(
        &mut self,
        address: Address,
        size: Size,
        function_code: FunctionCode,
        data: u32,
    ) -> Result<(), Fault> {
        (0..size.bytes()).try_for_each(|index| {
            let at = address.offset(index);
            let byte = data >> (8 * (size.bytes() - 1 - index)) & 0xff;
            self.cycle_write(at, Size::Byte, function_code, byte)
                .ok_or_else(|| misaligned_fault(at, function_code, CycleKind::Write, size, data))
        })
    }

    /// A long read the bus takes in one access ([`Bus::read_long`]), at
    /// an address whose first word memory does not serve: [`None`] if it
    /// takes two word cycles; `Some(None)` for a bus error.
    #[inline(never)]
    fn long_read(
        &mut self,
        address: Address,
        function_code: FunctionCode,
        cycle: CycleKind,
    ) -> Option<Option<u32>> {
        let address = self.cpu.memory.mask(address);
        if function_code.is_memory_space() {
            let space = Space::of(function_code);
            let protected = self.cpu.memory.is_special()
                && self.cpu.memory.permissions(space, address) != Permissions::ALL;
            if protected || self.cpu.memory.peek(space, address, Size::Word).is_some() {
                return None;
            }
        }
        let access = self.access(address, Size::Long, function_code);
        let read = self.bus.read_long(access)?;
        self.bus_stop_requested();
        match read {
            Ok(read) => {
                self.cpu.stall += read.stall;
                Some(Some(read.value))
            }
            Err(BusError) => {
                self.invalid(self.invalid_access(address, Size::Long, function_code, cycle, None));
                Some(None)
            }
        }
    }

    /// A long write the bus takes in one access ([`Bus::write_long`]), as
    /// `long_read`.
    #[inline(never)]
    fn long_write(
        &mut self,
        address: Address,
        function_code: FunctionCode,
        value: u32,
    ) -> Option<Option<()>> {
        let address = self.cpu.memory.mask(address);
        if function_code.is_memory_space() {
            let space = Space::of(function_code);
            let protected = self.cpu.memory.is_special()
                && self.cpu.memory.permissions(space, address) != Permissions::ALL;
            let tables = self.cpu.memory.write_served(space, address);
            if protected || tables {
                return None;
            }
        }
        let access = self.access(address, Size::Long, function_code);
        let written = self.bus.write_long(access, value)?;
        self.bus_stop_requested();
        match written {
            Ok(stall) => {
                self.cpu.stall += stall;
                Some(Some(()))
            }
            Err(BusError) => {
                let access = self.invalid_access(
                    address,
                    Size::Long,
                    function_code,
                    CycleKind::Write,
                    Some(value),
                );
                self.invalid(access);
                Some(None)
            }
        }
    }

    /// Marks the cycles that follow as exception stacking's, or not.
    pub(crate) fn set_stacking(&mut self, stacking: bool) {
        self.cpu.stacking = stacking;
        self.bus.stacking(stacking);
    }

    /// Records a stop the bus asks for after a cycle.
    fn bus_stop_requested(&mut self) {
        if self.bus.stop_requested() {
            self.cpu.stop_request.get_or_insert(Stop::Requested);
        }
    }

    /// A bus cycle at `address`, stamped with the instruction's start.
    const fn access(&self, address: Address, size: Size, function_code: FunctionCode) -> BusAccess {
        BusAccess {
            address,
            size,
            function_code,
            now: self.now,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Core;
    use super::Cpu;
    use super::Halt;
    use super::Stop;
    use crate::bus::Bus;
    use crate::bus::BusAccess;
    use crate::bus::BusError;
    use crate::bus::Read;
    use crate::cycles::Cycles;
    use crate::exception::MICROCODE_REVISION;
    use crate::exception::Movem;
    use crate::exception::MovemResume;
    use crate::exception::Vector;
    use crate::instructions::tests::CODE;
    use crate::instructions::tests::NoBus;
    use crate::instructions::tests::NoInterrupts;
    use crate::instructions::tests::RAM_BYTES;
    use crate::instructions::tests::machine;
    use crate::instructions::tests::peek_long;
    use crate::instructions::tests::poke_long;
    use crate::interrupt::Acknowledge;
    use crate::interrupt::Acknowledged;
    use crate::interrupt::InterruptLevel;
    use crate::interrupt::Interrupts;
    use crate::memory::Memory;
    use crate::registers::status;
    use mc683xx_target::address::Address;
    use mc683xx_target::entry::Entry;
    use mc683xx_target::function_code::FunctionCode;
    use mc683xx_target::size::Size;
    use std::cell::Cell;
    use std::rc::Rc;

    /// The supervisor stack's top.
    const STACK: u32 = 0x4000;
    /// Where vector n's handler is: here plus 16 bytes each.
    const HANDLERS: u32 = 0x2000;
    /// NOP.
    const NOP: u16 = 0x4e71;
    /// TRAP #0.
    const TRAP: u16 = 0x4e40;
    /// RTE.
    const RTE: u16 = 0x4e73;
    /// ILLEGAL.
    const ILLEGAL: u16 = 0x4afc;
    /// MOVE.L D0, (A0).
    const MOVE_D0_TO_INDIRECT: u16 = 0x2080;

    /// An interrupt source at a level, answering IACK one way.
    #[derive(Debug)]
    struct Source {
        /// The level requested.
        level: InterruptLevel,
        /// The IACK answer.
        answer: Acknowledge,
        /// Its acknowledge's wait states, as a stall.
        stall: Cycles,
        /// The levels acknowledged.
        acknowledged: Vec<InterruptLevel>,
    }

    impl Source {
        fn new(level: u8, answer: Acknowledge) -> Self {
            Self {
                level: InterruptLevel::new(level).unwrap(),
                answer,
                stall: Cycles::ZERO,
                acknowledged: Vec::new(),
            }
        }
    }

    impl Interrupts for Source {
        fn level(&self) -> InterruptLevel {
            self.level
        }

        fn acknowledge(&mut self, level: InterruptLevel, _now: Cycles) -> Acknowledged {
            self.acknowledged.push(level);
            Acknowledged {
                answer: self.answer,
                stall: self.stall,
            }
        }
    }

    const fn handler(vector: Vector) -> u32 {
        HANDLERS + 16 * vector.0 as u32
    }

    fn prepared(code: &[u16], sr: u16) -> Cpu {
        let mut cpu = machine(code);
        cpu.registers.ssp = STACK;
        cpu.registers.set_sr(sr);
        for vector in 0..=u8::MAX {
            poke_long(&mut cpu, Vector(vector).offset(), handler(Vector(vector)));
        }
        cpu
    }

    /// One run of budget 1: an interrupt, or an instruction with its
    /// exceptions.
    fn step(cpu: &mut Cpu, interrupts: &mut dyn Interrupts) -> Stop {
        cpu.run_for(&mut NoBus, interrupts, Cycles::ZERO, Cycles(1))
            .stop
    }

    /// The format/vector word of the frame at SSP.
    fn top_format_vector(cpu: &Cpu) -> u16 {
        peek_long(cpu, cpu.registers.ssp + 4) as u16
    }

    #[test]
    fn an_interrupt_above_the_mask_is_taken_with_its_autovector() {
        let mut cpu = prepared(&[NOP], status::S | 2 << status::INTERRUPT_SHIFT);
        let mut source = Source::new(3, Acknowledge::Autovector);
        step(&mut cpu, &mut source);
        assert_eq!(source.acknowledged, [InterruptLevel::new(3).unwrap()]);
        let vector = Vector::autovector(InterruptLevel::new(3).unwrap());
        assert_eq!(cpu.registers.pc, handler(vector));
        assert_eq!(cpu.registers.interrupt_mask(), 3);
        // Figure 6-3: the PC of the instruction not yet executed.
        assert_eq!(peek_long(&cpu, STACK - 8), 0x2200_0000 | CODE >> 16);
        assert_eq!(peek_long(&cpu, STACK - 4), (CODE & 0xffff) << 16 | 0x006c);
    }

    #[test]
    fn an_interrupt_at_the_mask_waits() {
        let mut cpu = prepared(&[NOP], status::S | 3 << status::INTERRUPT_SHIFT);
        let mut source = Source::new(3, Acknowledge::Autovector);
        step(&mut cpu, &mut source);
        assert!(source.acknowledged.is_empty());
        assert_eq!(cpu.registers.pc, CODE + 2);
    }

    #[test]
    fn vectored_and_spurious_interrupts() {
        let mut cpu = prepared(&[NOP], status::S);
        step(&mut cpu, &mut Source::new(5, Acknowledge::Vector(0x40)));
        assert_eq!(cpu.registers.pc, handler(Vector(0x40)));
        assert_eq!(top_format_vector(&cpu), 0x0100);
        let mut cpu = prepared(&[NOP], status::S);
        step(&mut cpu, &mut Source::new(5, Acknowledge::Spurious));
        assert_eq!(cpu.registers.pc, handler(Vector::SPURIOUS_INTERRUPT));
    }

    #[test]
    fn an_acknowledges_wait_states_add_to_the_interrupt() {
        let cycles = |stall| {
            let mut cpu = prepared(&[NOP], status::S);
            let mut source = Source::new(5, Acknowledge::Vector(0x40));
            source.stall = Cycles(stall);
            cpu.run_for(&mut NoBus, &mut source, Cycles::ZERO, Cycles(1))
                .cycles
        };
        assert_eq!(cycles(5), cycles(0) + Cycles(5));
    }

    #[test]
    fn level_7_is_taken_on_its_transitions() {
        let mut cpu = prepared(&[NOP, NOP, NOP], status::S | status::INTERRUPT_MASK);
        poke_long(
            &mut cpu,
            handler(Vector(31)),
            u32::from(NOP) << 16 | u32::from(NOP),
        );
        let mut source = Source::new(7, Acknowledge::Autovector);
        step(&mut cpu, &mut source);
        assert_eq!(source.acknowledged.len(), 1);
        // Held at 7 with the mask at 7: once.
        step(&mut cpu, &mut source);
        step(&mut cpu, &mut source);
        assert_eq!(source.acknowledged.len(), 1);
        // The mask drops below 7 while the request stays: again.
        cpu.registers.set_sr(status::S);
        step(&mut cpu, &mut source);
        assert_eq!(source.acknowledged.len(), 2);
        // A new transition to 7.
        source.level = InterruptLevel::NONE;
        step(&mut cpu, &mut source);
        source.level = InterruptLevel::NONMASKABLE;
        step(&mut cpu, &mut source);
        assert_eq!(source.acknowledged.len(), 3);
    }

    /// A bus and an interrupt source sharing a stop that the
    /// acknowledge asks for, as a chip whose acknowledge ends its
    /// quantum does.
    #[derive(Debug, Default)]
    struct AcknowledgeStops(Rc<Cell<bool>>);

    impl Bus for AcknowledgeStops {
        fn read(&mut self, _access: BusAccess) -> Result<Read, BusError> {
            Err(BusError)
        }

        fn write(&mut self, _access: BusAccess, _value: u32) -> Result<Cycles, BusError> {
            Err(BusError)
        }

        fn stop_requested(&mut self) -> bool {
            self.0.take()
        }
    }

    impl Interrupts for AcknowledgeStops {
        fn level(&self) -> InterruptLevel {
            InterruptLevel::new(1).unwrap()
        }

        fn acknowledge(&mut self, _level: InterruptLevel, _now: Cycles) -> Acknowledged {
            self.0.set(true);
            Acknowledge::Autovector.into()
        }
    }

    #[test]
    fn a_stop_asked_at_an_acknowledge_ends_the_run_after_the_interrupt() {
        let mut cpu = prepared(&[NOP], status::S);
        let mut bus = AcknowledgeStops::default();
        let mut interrupts = AcknowledgeStops(Rc::clone(&bus.0));
        let run = cpu.run_for(&mut bus, &mut interrupts, Cycles::ZERO, Cycles(10_000));
        assert_eq!(run.stop, Stop::Requested);
        assert_eq!(cpu.registers.pc, handler(Vector(25)));
    }

    #[test]
    fn an_interrupt_wakes_a_stopped_cpu() {
        let mut cpu = prepared(&[NOP], status::S);
        cpu.stopped = true;
        let run = cpu.run_for(&mut NoBus, &mut NoInterrupts, Cycles::ZERO, Cycles(100));
        assert_eq!(run.stop, Stop::Stopped);
        assert_eq!(run.cycles, Cycles(100));
        assert_eq!(
            step(&mut cpu, &mut Source::new(1, Acknowledge::Autovector)),
            Stop::Budget
        );
        assert!(!cpu.stopped);
        assert_eq!(cpu.registers.pc, handler(Vector(25)));
        assert_eq!(peek_long(&cpu, STACK - 6), CODE);
    }

    #[test]
    fn t1_traces_every_instruction_with_a_six_word_frame() {
        let sr = status::S | status::T1;
        let mut cpu = prepared(&[NOP], sr);
        step(&mut cpu, &mut NoInterrupts);
        // Figure 6-4: the next PC, then the traced instruction's.
        assert_eq!(
            peek_long(&cpu, STACK - 12),
            u32::from(sr) << 16 | (CODE + 2) >> 16
        );
        assert_eq!(peek_long(&cpu, STACK - 8), (CODE + 2) << 16 | 0x2024);
        assert_eq!(peek_long(&cpu, STACK - 4), CODE);
        assert_eq!(cpu.registers.pc, handler(Vector::TRACE));
        assert_eq!(cpu.registers.sr(), status::S);
    }

    #[test]
    fn t0_traces_only_a_change_of_flow() {
        let mut cpu = prepared(&[NOP], status::S | status::T0);
        step(&mut cpu, &mut NoInterrupts);
        assert_eq!(cpu.registers.pc, CODE + 2);
        // RTE returns: traced, its frame popped first.
        let mut cpu = prepared(&[RTE], status::S | status::T0);
        cpu.registers.ssp = STACK - 8;
        poke_long(&mut cpu, STACK - 8, u32::from(status::S) << 16);
        poke_long(&mut cpu, STACK - 4, 0x1234_0000);
        step(&mut cpu, &mut NoInterrupts);
        assert_eq!(cpu.registers.pc, handler(Vector::TRACE));
        assert_eq!(peek_long(&cpu, STACK - 8) & 0xffff, 0x2024);
    }

    #[test]
    fn an_illegal_instruction_is_not_traced() {
        let mut cpu = prepared(&[ILLEGAL], status::S | status::T1);
        step(&mut cpu, &mut NoInterrupts);
        assert_eq!(cpu.registers.pc, handler(Vector::ILLEGAL_INSTRUCTION));
        assert_eq!(cpu.registers.ssp, STACK - 8);
    }

    #[test]
    fn trap_then_trace_then_interrupt() {
        let mut cpu = prepared(&[TRAP], status::S | status::T1 | status::INTERRUPT_MASK);
        let mut source = Source::new(4, Acknowledge::Autovector);
        step(&mut cpu, &mut NoInterrupts);
        cpu.registers.set_sr(status::S);
        step(&mut cpu, &mut source);
        // The interrupt handler runs first, returning to the trace
        // handler, which returns to the trap handler (CPU32RM 6.1.5).
        let interrupt = STACK - 8 - 12 - 8;
        assert_eq!(cpu.registers.ssp, interrupt);
        assert_eq!(peek_long(&cpu, interrupt + 2), handler(Vector::TRACE));
        assert_eq!(peek_long(&cpu, interrupt + 8 + 2), handler(Vector::TRAP));
        assert_eq!(peek_long(&cpu, interrupt + 8 + 8), CODE);
        assert_eq!(peek_long(&cpu, interrupt + 20 + 2), CODE + 2);
    }

    #[test]
    fn a_released_write_fault_keeps_the_trace_in_tr() {
        let mut cpu = prepared(&[MOVE_D0_TO_INDIRECT, NOP], status::S | status::T1);
        poke_long(&mut cpu, handler(Vector::BUS_ERROR), u32::from(RTE) << 16);
        cpu.registers.set_address(0, 0x0001_0000);
        step(&mut cpu, &mut NoInterrupts);
        assert_eq!(cpu.registers.ssp, STACK - 24);
        // SSW: TR, RR, LG, FUNC 5.
        assert_eq!(
            peek_long(&cpu, STACK - 4),
            u32::from(MICROCODE_REVISION) << 24 | 0x1225
        );
        // Completed in software: RR cleared.  RTE requeues the trace.
        poke_long(
            &mut cpu,
            STACK - 4,
            u32::from(MICROCODE_REVISION) << 24 | 0x1025,
        );
        step(&mut cpu, &mut NoInterrupts);
        assert_eq!(cpu.registers.pc, handler(Vector::TRACE));
        assert_eq!(cpu.registers.ssp, STACK - 12);
        assert_eq!(peek_long(&cpu, STACK - 12 + 2), CODE + 2);
        assert_eq!(peek_long(&cpu, STACK - 12 + 8), CODE);
    }

    #[test]
    fn a_movem_fault_is_type_iii_and_rte_hands_its_progress_back() {
        let mut cpu = prepared(&[RTE], status::S);
        cpu.instruction_pc = Address(CODE);
        let mut bus = NoBus;
        let mut core = Core {
            cpu: &mut cpu,
            bus: &mut bus,
            now: Cycles::ZERO,
        };
        core.movem_progress(3, Address(0x3006));
        let Err(crate::exception::Exception::Fault(fault)) = core.write_unreleased(
            Address(0x0001_0000),
            Size::Word,
            FunctionCode::SUPERVISOR_DATA,
            7,
        ) else {
            panic!("a bus error");
        };
        core.instruction_fault(fault, Address(CODE));
        let sp = STACK - 24;
        assert_eq!(cpu.registers.ssp, sp);
        // Figure 6-7: the next operand's address, 16 less 3 transfers,
        // SSW MV, RR, SIZ word, FUNC 5.
        assert_eq!(peek_long(&cpu, sp + 8), 0x3006);
        assert_eq!(
            peek_long(&cpu, sp + 0x14),
            u32::from(MICROCODE_REVISION) << 24 | 13 << 16 | 0x4215
        );
        cpu.registers.pc = CODE;
        step(&mut cpu, &mut NoInterrupts);
        let progress = Movem {
            attempted: 3,
            next: Address(0x3006),
        };
        assert_eq!(
            cpu.movem_resume,
            Some(MovemResume {
                pc: Address(CODE),
                progress
            })
        );
        assert_eq!(cpu.registers.pc, CODE);
        // An instruction elsewhere leaves it; the one at its PC finds
        // it, once.
        cpu.instruction_pc = Address(CODE + 2);
        let mut core = Core {
            cpu: &mut cpu,
            bus: &mut bus,
            now: Cycles::ZERO,
        };
        assert_eq!(core.movem_continuation(), None);
        core.cpu.instruction_pc = Address(CODE);
        assert_eq!(core.movem_continuation(), Some(progress));
        assert_eq!(core.movem_continuation(), None);
    }

    #[test]
    fn an_interrupt_s_movem_leaves_the_continuation_to_the_faulted_one() {
        // MOVEM.L D0-D3, (A0): D2's write bus-errors at RAM's end.
        let ram_end = RAM_BYTES;
        let mut cpu = prepared(&[0x48d0, 0x000f], status::S | status::INTERRUPT_MASK);
        cpu.registers.set_address(0, ram_end - 8);
        cpu.registers.data = [0xd0, 0xd1, 0xd2, 0xd3, 0xd4, 0xd5, 0, 0];
        poke_long(&mut cpu, handler(Vector::BUS_ERROR), u32::from(RTE) << 16);
        // The level 1 handler: MOVEM.L D4-D5, -(A7); MOVEM.L (A7)+,
        // D4-D5; RTE.
        let autovector = Vector::autovector(InterruptLevel::new(1).unwrap());
        poke_long(&mut cpu, handler(autovector), 0x48e7_0c00);
        poke_long(&mut cpu, handler(autovector) + 4, 0x4cdf_0030);
        poke_long(&mut cpu, handler(autovector) + 8, u32::from(RTE) << 16);
        step(&mut cpu, &mut NoInterrupts);
        assert_eq!(cpu.registers.pc, handler(Vector::BUS_ERROR));
        // The handler points the rest of the operands at RAM and
        // returns with the mask down.
        let sp = STACK - 24;
        poke_long(&mut cpu, sp + 8, 0x3008);
        let frame_pc = peek_long(&cpu, sp) & 0xffff;
        poke_long(&mut cpu, sp, u32::from(status::S) << 16 | frame_pc);
        step(&mut cpu, &mut NoInterrupts);
        assert_eq!(cpu.registers.pc, CODE);
        let mut source = Source::new(1, Acknowledge::Autovector);
        step(&mut cpu, &mut source);
        assert_eq!(cpu.registers.pc, handler(autovector));
        let isr_sp = cpu.registers.ssp;
        step(&mut cpu, &mut NoInterrupts);
        assert_eq!(cpu.registers.ssp, isr_sp - 8);
        assert_eq!(peek_long(&cpu, isr_sp - 8), 0xd4);
        assert_eq!(peek_long(&cpu, isr_sp - 4), 0xd5);
        assert!(cpu.movem_resume.is_some());
        step(&mut cpu, &mut NoInterrupts);
        step(&mut cpu, &mut NoInterrupts);
        assert_eq!(cpu.registers.pc, CODE);
        // The faulted MOVEM continues from D2.
        step(&mut cpu, &mut NoInterrupts);
        assert_eq!(cpu.registers.pc, CODE + 4);
        assert_eq!(cpu.exception_taken(), None);
        assert_eq!(peek_long(&cpu, 0x3008), 0xd2);
        assert_eq!(peek_long(&cpu, 0x300c), 0xd3);
        assert_eq!(cpu.registers.ssp, STACK);
        assert_eq!(cpu.movem_resume, None);
    }

    /// A bus that ends breakpoint acknowledge cycles with DSACK after
    /// `stall` clocks, recording them, and nothing else.
    #[derive(Debug, Default)]
    struct BreakpointResponder {
        /// The clocks each acknowledge takes beyond the tables'.
        stall: Cycles,
        /// The acknowledge cycles.
        acknowledges: Vec<BusAccess>,
    }

    impl Bus for BreakpointResponder {
        fn read(&mut self, access: BusAccess) -> Result<Read, BusError> {
            if !access.function_code.is_cpu_space() {
                return Err(BusError);
            }
            self.acknowledges.push(access);
            // the word is ignored (CPU32RM 6.2.6)
            Ok(Read {
                value: ILLEGAL.into(),
                stall: self.stall,
            })
        }

        fn write(&mut self, _access: BusAccess, _value: u32) -> Result<Cycles, BusError> {
            Err(BusError)
        }
    }

    /// The six-word frame at SSP, as the long words SR and the high
    /// word of the next PC; its low word and the format and vector; the
    /// faulted instruction's PC (CPU32RM Figure 6-4).
    fn six_word_frame(sr: u16, next: u32, vector: Vector, faulted: u32) -> [u32; 3] {
        [
            u32::from(sr) << 16 | next >> 16,
            (next & 0xffff) << 16 | 0x2000 | vector.offset(),
            faulted,
        ]
    }

    /// The three long words at SSP plus `offset`.
    fn frame_at(cpu: &Cpu, offset: u32) -> [u32; 3] {
        let at = cpu.registers.ssp + offset;
        [
            peek_long(cpu, at),
            peek_long(cpu, at + 4),
            peek_long(cpu, at + 8),
        ]
    }

    #[test]
    fn a_hardware_breakpoint_acknowledge_ended_by_dsack_is_ignored() {
        let mut cpu = prepared(&[NOP, NOP], status::S);
        step(&mut cpu, &mut NoInterrupts);
        cpu.set_breakpoint_pending(true);
        let mut bus = BreakpointResponder {
            stall: Cycles(3),
            ..BreakpointResponder::default()
        };
        let run = cpu.run_for(&mut bus, &mut NoInterrupts, Cycles::ZERO, Cycles(1));
        // CPU space, type 0000, BKPT #7 on A[4:2], T on A1, a word
        // (CPU32RM 5.3.1.1, 6.2.6; MC68331UM Figure 4-12)
        assert_eq!(bus.acknowledges.len(), 1);
        let acknowledge = bus.acknowledges[0];
        assert_eq!(acknowledge.address, Address(0x1e));
        assert_eq!(acknowledge.function_code, FunctionCode::CPU_SPACE);
        assert_eq!(acknowledge.size, Size::Word);
        assert!(!cpu.breakpoint_pending());
        assert_eq!(cpu.registers.pc, CODE + 2, "nothing taken");
        assert_eq!(cpu.registers.ssp, STACK);
        // CPU32RM 8.3.13, Breakpoint (Acknowledged), and the wait states
        let acknowledged = u64::from(crate::timing::exception::BREAKPOINT.cycles);
        assert_eq!(run.cycles, Cycles(acknowledged + 3));
        // the next run executes the next instruction
        step(&mut cpu, &mut NoInterrupts);
        assert_eq!(cpu.registers.pc, CODE + 4);
        assert_eq!(bus.acknowledges.len(), 1);
    }

    #[test]
    fn a_hardware_breakpoint_acknowledge_ended_by_berr_takes_vector_12() {
        let sr = status::S | 2 << status::INTERRUPT_SHIFT | status::Z;
        let mut cpu = prepared(&[NOP, NOP], sr);
        step(&mut cpu, &mut NoInterrupts);
        cpu.set_breakpoint_pending(true);
        let run = cpu.run_for(&mut NoBus, &mut NoInterrupts, Cycles::ZERO, Cycles(1));
        assert!(!cpu.breakpoint_pending());
        assert_eq!(cpu.registers.pc, handler(Vector::HARDWARE_BREAKPOINT));
        assert_eq!(cpu.registers.ssp, STACK - 12);
        // format $2: SR, the next PC, $2030, the instruction executing
        // when the breakpoint was sensed (CPU32RM 6.2.6, 6.4.2)
        assert_eq!(
            frame_at(&cpu, 0),
            six_word_frame(sr, CODE + 2, Vector::HARDWARE_BREAKPOINT, CODE)
        );
        assert_eq!(cpu.registers.sr(), sr, "S set, the mask kept");
        // CPU32RM 8.3.13, Breakpoint (Bus Error)
        let cost = u64::from(crate::timing::exception::BREAKPOINT_BUS_ERROR.cycles);
        assert_eq!(run.cycles, Cycles(cost));
        assert_eq!(
            cpu.exception_taken().map(|taken| taken.vector()),
            Some(Vector::HARDWARE_BREAKPOINT)
        );
    }

    #[test]
    fn trace_then_hardware_breakpoint_then_interrupt() {
        let sr = status::S | status::T1 | status::INTERRUPT_MASK;
        let mut cpu = prepared(&[NOP], sr);
        // the NOP and its trace
        step(&mut cpu, &mut NoInterrupts);
        cpu.registers.set_sr(status::S);
        cpu.set_breakpoint_pending(true);
        let mut source = Source::new(4, Acknowledge::Autovector);
        let breakpoint = u64::from(crate::timing::exception::BREAKPOINT_BUS_ERROR.cycles);
        cpu.run_for(
            &mut NoBus,
            &mut source,
            Cycles::ZERO,
            Cycles(breakpoint + 1),
        );
        // Table 6-2: the trace's processing first, then the
        // breakpoint's, then the interrupt's, whose handler runs first
        // and returns to the breakpoint's, which returns to the trace's
        // (CPU32RM 6.1.5)
        let interrupt = STACK - 12 - 12 - 8;
        assert_eq!(cpu.registers.ssp, interrupt);
        assert_eq!(
            cpu.registers.pc,
            handler(Vector::autovector(InterruptLevel::new(4).unwrap()))
        );
        assert_eq!(
            peek_long(&cpu, interrupt + 2),
            handler(Vector::HARDWARE_BREAKPOINT)
        );
        assert_eq!(
            frame_at(&cpu, 8),
            six_word_frame(
                status::S,
                handler(Vector::TRACE),
                Vector::HARDWARE_BREAKPOINT,
                CODE
            )
        );
        assert_eq!(
            frame_at(&cpu, 20),
            six_word_frame(sr, CODE + 2, Vector::TRACE, CODE)
        );
    }

    #[test]
    fn a_stopped_cpu_acknowledges_its_breakpoint_after_the_interrupt_that_wakes_it() {
        let mut cpu = prepared(&[NOP], status::S);
        cpu.stopped = true;
        cpu.set_breakpoint_pending(true);
        let run = cpu.run_for(&mut NoBus, &mut NoInterrupts, Cycles::ZERO, Cycles(100));
        assert_eq!(run.stop, Stop::Stopped);
        assert!(cpu.breakpoint_pending(), "STOP leaves it pending");
        assert_eq!(cpu.registers.ssp, STACK);
        step(&mut cpu, &mut Source::new(1, Acknowledge::Autovector));
        assert!(!cpu.stopped);
        assert!(!cpu.breakpoint_pending());
        // the breakpoint's frame on the interrupt's, returning to its
        // handler (CPU32RM 6.2.6)
        assert_eq!(cpu.registers.pc, handler(Vector::HARDWARE_BREAKPOINT));
        assert_eq!(cpu.registers.ssp, STACK - 8 - 12);
        let level_1 = Vector::autovector(InterruptLevel::new(1).unwrap());
        assert_eq!(peek_long(&cpu, cpu.registers.ssp + 2), handler(level_1));
        assert_eq!(peek_long(&cpu, STACK - 6), CODE);
    }

    #[test]
    fn a_pending_breakpoint_enters_background_mode_if_enabled_and_reset_drops_it() {
        let mut cpu = prepared(&[NOP], status::S);
        cpu.set_bdm_enabled(true);
        cpu.set_breakpoint_pending(true);
        let run = cpu.run_for(&mut NoBus, &mut NoInterrupts, Cycles::ZERO, Cycles(100));
        assert_eq!(run.stop, Stop::Background(Entry::Breakpoint));
        assert_eq!(cpu.background(), Some(Entry::Breakpoint));
        assert!(!cpu.breakpoint_pending());
        assert_eq!(cpu.registers.ssp, STACK, "no acknowledge, no exception");
        cpu.set_breakpoint_pending(true);
        let _ = cpu.reset(&mut NoBus, Cycles::ZERO);
        assert!(!cpu.breakpoint_pending());
    }

    #[test]
    fn a_double_bus_fault_fetching_the_vectors_enters_background_mode_if_enabled() {
        // nothing mapped: both vector fetches end in BERR
        let mut cpu = Cpu::new(Memory::new(24).unwrap());
        assert_eq!(
            cpu.reset(&mut NoBus, Cycles::ZERO),
            Err(Halt::DoubleBusFault)
        );
        assert_eq!(cpu.background(), None);
        cpu.set_bdm_enabled(true);
        assert_eq!(cpu.reset(&mut NoBus, Cycles::ZERO), Ok(()));
        assert_eq!(cpu.halted(), None);
        let Some(Entry::DoubleBusFault { ssw }) = cpu.background() else {
            panic!("{:?}", cpu.background());
        };
        // a read of a long in supervisor program space (CPU32RM 6.3.1.2)
        assert_eq!(ssw & 0b111, 6);
        assert_eq!(cpu.pcc, Address(1), "CPU32RM 7.2.5.3");
        assert_eq!(cpu.atemp, Entry::DoubleBusFault { ssw }.atemp());
        assert_eq!(
            step(&mut cpu, &mut NoInterrupts),
            Stop::Background(Entry::DoubleBusFault { ssw })
        );
    }

    /// A bus answering the reset vectors' words below `answered`, each
    /// address read recorded, and faulting the rest.
    struct VectorBus {
        /// The first address that faults.
        answered: u32,
        /// The addresses read.
        reads: Vec<u32>,
    }

    impl Bus for VectorBus {
        fn read(&mut self, access: BusAccess) -> Result<Read, BusError> {
            self.reads.push(access.address.0);
            if access.address.0 >= self.answered {
                return Err(BusError);
            }
            Ok(Read {
                value: 0x1234,
                stall: Cycles::ZERO,
            })
        }

        fn write(&mut self, _access: BusAccess, _value: u32) -> Result<Cycles, BusError> {
            Err(BusError)
        }
    }

    #[test]
    fn reset_loads_ssp_before_fetching_pc_and_stops_at_the_first_fault() {
        // CPU32RM 6.2.1, Figure 6-2
        let mut cpu = Cpu::new(Memory::new(24).unwrap());
        cpu.set_bdm_enabled(true);
        let mut bus = VectorBus {
            answered: 4,
            reads: Vec::new(),
        };
        assert_eq!(cpu.reset(&mut bus, Cycles::ZERO), Ok(()));
        assert!(cpu.background().is_some());
        assert_eq!(cpu.registers.ssp, 0x1234_1234, "SSP loaded");
        assert_eq!(bus.reads, [0, 2, 4]);
        let mut bus = VectorBus {
            answered: 0,
            reads: Vec::new(),
        };
        assert_eq!(cpu.reset(&mut bus, Cycles::ZERO), Ok(()));
        assert_eq!(bus.reads, [0], "PC not fetched");
    }

    #[test]
    fn a_fault_before_the_first_instruction_after_reset_halts() {
        let mut cpu = prepared(&[], status::S);
        poke_long(&mut cpu, 0, STACK);
        poke_long(&mut cpu, 4, 0x0001_0000);
        assert_eq!(cpu.reset(&mut NoBus, Cycles::ZERO), Ok(()));
        assert_eq!(
            step(&mut cpu, &mut NoInterrupts),
            Stop::Halted(Halt::DoubleBusFault)
        );
        poke_long(&mut cpu, 4, CODE);
        poke_long(&mut cpu, CODE, u32::from(NOP) << 16);
        assert_eq!(cpu.reset(&mut NoBus, Cycles::ZERO), Ok(()));
        assert_eq!(step(&mut cpu, &mut NoInterrupts), Stop::Budget);
    }

    /// Loop mode (CPU32RM 1.1.2, 8.3.11): ADDQ.L #1, D0 (8.3.6, ADDQ
    /// #, Rn: 0, 0, 2(0/1/0)) looped by DBF D1, *-2 (the displacement
    /// -4), then BEQ.B not taken (8.3.11: 2, 0, 4(0/1/0)).
    mod loop_mode {
        use super::NOP;
        use super::RTE;
        use super::STACK;
        use super::Source;
        use super::handler;
        use super::step;
        use crate::cpu::Cpu;
        use crate::cycles::Cycles;
        use crate::exception::Vector;
        use crate::instructions::tests::CODE;
        use crate::instructions::tests::NoBus;
        use crate::instructions::tests::NoInterrupts;
        use crate::instructions::tests::RAM_BYTES;
        use crate::instructions::tests::machine_with_clocks;
        use crate::instructions::tests::poke_long;
        use crate::interrupt::Acknowledge;
        use crate::interrupt::InterruptLevel;
        use crate::registers::status;
        use mc683xx_target::address::Address;

        /// ADDQ.L #1, D0.
        const ADDQ: u16 = 0x5280;
        /// DBF D1.
        const DBF: u16 = 0x51c9;
        /// DBEQ D1.
        const DBEQ: u16 = 0x57c9;
        /// The loop displacement, -4.
        const BACK: u16 = 0xfffc;
        /// BEQ.B *+4.
        const BEQ: u16 = 0x6702;
        /// Clocks of each bus cycle: two wait states a word.
        const CLOCKS: u8 = 4;
        /// The wait states of one word.
        const WAIT: u64 = 2;
        /// Where the DBcc is.
        const DBCC: Address = Address(CODE + 2);

        /// The loop on slow RAM, D1 counting `count`.
        fn looping(code: &[u16], count: u32) -> Cpu {
            let mut cpu = machine_with_clocks(code, CLOCKS);
            cpu.registers.ssp = STACK;
            cpu.registers.data[1] = count;
            for vector in 0..=u8::MAX {
                poke_long(&mut cpu, Vector(vector).offset(), handler(Vector(vector)));
            }
            cpu
        }

        /// One instruction's cycles.
        fn cycles(cpu: &mut Cpu) -> u64 {
            cpu.run_for(&mut NoBus, &mut NoInterrupts, Cycles::ZERO, Cycles(1))
                .cycles
                .0
        }

        #[test]
        fn a_loop_runs_its_loop_mode_rows_unfetched_until_its_count_expires() {
            let mut cpu = looping(&[ADDQ, DBF, BACK, BEQ], 2);
            assert_eq!(cycles(&mut cpu), 2 + WAIT);
            // Entered: the taken row, 10(0/2/0), its words fetched.
            assert_eq!(cycles(&mut cpu), 10 + 2 * WAIT);
            assert_eq!(cpu.loop_mode, Some(DBCC));
            // In loop mode no fetch, so no wait states, and the loop
            // mode rows: taken 10(0/0/0), then (F, -1) 8(0/1/0).
            assert_eq!(cycles(&mut cpu), 2);
            assert_eq!(cycles(&mut cpu), 10);
            assert_eq!(cycles(&mut cpu), 2);
            assert_eq!(cycles(&mut cpu), 8);
            assert_eq!(cpu.registers.data[1], 0xffff);
            assert_eq!(cpu.registers.data[0], 3);
            // Out of the loop: fetched again, and the loop mode row's
            // tail of 0 overlaps none of BEQ's head of 2.
            assert_eq!(cycles(&mut cpu), 4 + WAIT);
            assert_eq!(cpu.loop_mode, None);
            assert_eq!(cpu.registers.pc, CODE + 8);
        }

        #[test]
        fn a_true_condition_ends_loop_mode() {
            // ADDQ.W #1, D0 from 0xFFFE: Z on the second pass.
            let mut cpu = looping(&[0x5240, DBEQ, BACK], 10);
            cpu.registers.data[0] = 0xfffe;
            assert_eq!(cycles(&mut cpu), 2 + WAIT);
            assert_eq!(cycles(&mut cpu), 10 + 2 * WAIT);
            assert_eq!(cycles(&mut cpu), 2);
            // (T, not taken) in loop mode: 6(0/1/0).
            assert_eq!(cycles(&mut cpu), 6);
            assert_eq!(cpu.registers.data[1], 9);
            assert_eq!(cpu.registers.pc, CODE + 6);
        }

        #[test]
        fn an_interrupt_ends_loop_mode() {
            let mut cpu = looping(&[ADDQ, DBF, BACK], 5);
            let level = InterruptLevel::new(1).unwrap();
            let autovector = handler(Vector::autovector(level));
            poke_long(&mut cpu, autovector, u32::from(RTE) << 16);
            cpu.registers.set_sr(status::S);
            cycles(&mut cpu);
            cycles(&mut cpu);
            assert_eq!(cpu.loop_mode, Some(DBCC));
            step(&mut cpu, &mut Source::new(1, Acknowledge::Autovector));
            assert_eq!(cpu.loop_mode, None);
            assert_eq!(cpu.registers.pc, autovector);
            cycles(&mut cpu);
            assert_eq!(cpu.registers.pc, CODE);
            // Fetched again, then entered again.
            assert_eq!(cycles(&mut cpu), 2 + WAIT);
            assert_eq!(cycles(&mut cpu), 10 + 2 * WAIT);
            assert_eq!(cycles(&mut cpu), 2);
            assert_eq!(cpu.registers.data[1], 3);
        }

        #[test]
        fn an_exception_in_the_looped_instruction_ends_loop_mode() {
            // MOVE.W (A0)+, D2 from the last word of RAM: the second
            // pass reads past it, a bus error.
            let mut cpu = looping(&[0x3418, DBF, BACK], 5);
            cpu.registers.set_address(0, RAM_BYTES - 2);
            cycles(&mut cpu);
            cycles(&mut cpu);
            assert_eq!(cpu.loop_mode, Some(DBCC));
            cycles(&mut cpu);
            assert_eq!(cpu.registers.pc, handler(Vector::BUS_ERROR));
            assert_eq!(cpu.loop_mode, None);
            assert_eq!(cpu.loopable, None);
        }

        #[test]
        fn a_trace_ends_loop_mode() {
            // Trace on change of flow traces the taken DBcc (6.2.10).
            let mut cpu = looping(&[ADDQ, DBF, BACK], 5);
            cpu.registers
                .set_sr(status::S | status::INTERRUPT_MASK | status::T0);
            cycles(&mut cpu);
            cycles(&mut cpu);
            assert_eq!(cpu.registers.pc, handler(Vector::TRACE));
            assert_eq!(cpu.loop_mode, None);
        }

        #[test]
        fn only_the_one_word_instruction_just_run_loops() {
            // ADDI.W #1, D0, two words, under DBF D1, *-4 (-6).
            let mut cpu = looping(&[0x0640, 0x0001, DBF, 0xfffa], 5);
            cycles(&mut cpu);
            cycles(&mut cpu);
            assert_eq!(cpu.loop_mode, None);
            // DBF D1, *-2 back to a NOP it did not follow.
            let mut cpu = looping(&[NOP, DBF, BACK], 5);
            cpu.registers.pc = DBCC.0;
            cycles(&mut cpu);
            assert_eq!(cpu.loop_mode, None);
            assert_eq!(cycles(&mut cpu), 2 + WAIT);
            cycles(&mut cpu);
            assert_eq!(cpu.loop_mode, Some(DBCC));
        }

        #[test]
        fn a_moved_pc_ends_loop_mode() {
            let mut cpu = looping(&[ADDQ, DBF, BACK, NOP], 5);
            cycles(&mut cpu);
            cycles(&mut cpu);
            cpu.registers.pc = CODE + 6;
            assert_eq!(cycles(&mut cpu), 2 + WAIT);
            assert_eq!(cpu.loop_mode, None);
        }

        #[test]
        fn a_snapshot_keeps_loop_mode() {
            let mut cpu = looping(&[ADDQ, DBF, BACK], 5);
            cycles(&mut cpu);
            cycles(&mut cpu);
            let snapshot = cpu.save();
            let mut restored = looping(&[ADDQ, DBF, BACK], 0);
            restored.restore(&snapshot).unwrap();
            assert_eq!(restored.loop_mode, Some(DBCC));
            assert_eq!(cycles(&mut restored), 2);
        }
    }
}
