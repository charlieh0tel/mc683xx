//! What a board needs of the chip it is built around: any 683xx part
//! made of the workspace's core and modules ([PLAN.md, Boards,
//! Chips][plan-boards]).
//!
//! A board file names its chip (`[chip] type`) and gives the chip's own
//! keys under `[chip]`; a [`Board`](crate::board::Board) is generic over
//! the [`Chip`] it runs.  The board's generic code knows the chip only
//! through this trait, its [`ChipBuilder`] and its pins
//! ([`ChipPin`]): the CPU32 core, the SIM's chip selects, DSACK and
//! reset causes, and the QSPI's peripheral, which every part here is
//! built from, and nothing of one part.  The MC68331's implementation
//! is [`crate::mc68331`].
//!
//! [plan-boards]: https://github.com/charlieh0tel/mc683xx/blob/main/PLAN.md#boards

use std::any::Any;
use std::fmt;
use std::rc::Rc;
use std::str::FromStr;

use mc683xx_cpu32::bus::PortSize;
use mc683xx_cpu32::bus::StopRequest;
use mc683xx_cpu32::cpu::Cpu;
use mc683xx_cpu32::cpu::Halt;
use mc683xx_cpu32::cpu::Run;
use mc683xx_cpu32::cpu::control::CpuControl;
use mc683xx_cpu32::cpu::state::Save;
use mc683xx_cpu32::cpu::state::StateError;
use mc683xx_cpu32::cycles::Cycles;
use mc683xx_cpu32::frequency::Frequency;
use mc683xx_cpu32::interrupt::IrqPin;
use mc683xx_cpu32::peripheral::MappedPeripheral;
use mc683xx_modules::chip_select::ChipSelect;
use mc683xx_modules::peripheral::SpiPeripheral;
use mc683xx_modules::peripheral::Timed;
use mc683xx_modules::sim::chip_selects::Dsack;
use mc683xx_modules::sim::chip_selects::Termination;
use mc683xx_modules::sim::reset::ResetScope;
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::error::LoadError;
use crate::signal::Direction;
use crate::signal::Kind;
use crate::signal::Signal;
use crate::time::Time;
use crate::time::TimeBase;

/// A chip pin a net reaches, `chip.NAME` in a board file: the chip's own
/// set, each with what it carries and which way it faces.
pub trait ChipPin:
    Copy + Ord + fmt::Debug + FromStr + Serialize + DeserializeOwned + 'static
{
    /// Its name after `chip.`: `PE0`, `RESET`.
    fn name(self) -> &'static str;

    /// What it carries: [`Kind::Logic`] for a level (an `edge` net
    /// reaches it too), [`Kind::Serial`] or [`Kind::Clock`].
    fn kind(self) -> Kind;

    /// Which way it faces: into the chip, out of it, or either as the
    /// chip's registers set it.
    fn direction(self) -> Direction;

    /// Whether the chip takes a level on it ahead of time, at the level's
    /// time ([`Chip::input`] at a later cycle): an input the chip times
    /// itself, such as a timer's capture input, so a peripheral that
    /// drives an edge ahead is captured at exactly its clock.
    fn scheduled(self) -> bool;

    /// Whether it is one of the QSPI's peripheral chip selects,
    /// PCS\[3:0\]: a net that senses one keeps the QSPI's calls from
    /// being deferred, so it sees each change at its time.
    fn pcs(self) -> bool;
}

/// Whether a net of `kind` can reach `pin`.
pub(crate) fn takes<P: ChipPin>(pin: P, kind: Kind) -> bool {
    match pin.kind() {
        Kind::Logic => matches!(kind, Kind::Logic | Kind::Edge),
        own => own == kind,
    }
}

/// What a board's routers ask of the chip from within its run, the
/// chip's handles behind one trait object.
pub trait Requests {
    /// Ends the chip's run after the instruction running, short of its
    /// budget, for the board to see what a peripheral did.
    fn sync(&self);

    /// The cycles that reach memory `memory` (by the order the builder
    /// took them, from 0) reach the router it was given instead, from
    /// the end of the cycle running; the core serves none of its pages
    /// meanwhile.
    fn divert(&self, memory: usize);

    /// Memory `memory`'s cycles reach it again.
    fn release(&self, memory: usize);
}

/// Where a board memory answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum MemoryPlace<'a> {
    /// On these chip-select pins.
    Selects(&'a [ChipSelect]),
    /// At a fixed address on no pin, emulator-only.
    Fixed {
        /// Its first byte's address.
        base: u32,
        /// Its port's width.
        port: PortSize,
        /// How its cycles end: fast termination or wait states.
        termination: Termination,
    },
}

/// A board memory as the board hands it to the chip.
#[derive(Debug)]
pub struct MemoryPlan<'a> {
    /// Its contents.
    pub bytes: Vec<u8>,
    /// Whether writes reach it.
    pub writable: bool,
    /// Where it answers.
    pub place: MemoryPlace<'a>,
    /// The resets that put its contents back.
    pub reload: ResetScope,
}

/// A mapped peripheral the board hands the chip: one of the board's
/// routers.  Every type that is one implements it.
pub trait BoardMapped: MappedPeripheral + Timed + Save + Any {}

impl<T: MappedPeripheral + Timed + Save + Any> BoardMapped for T {}

/// An SPI peripheral the board hands the chip: its PCS decode.
pub trait BoardSpi: SpiPeripheral + Save + Any {}

impl<T: SpiPeripheral + Save + Any> BoardSpi for T {}

/// The chip being wired, before its power-on reset.
pub trait ChipBuilder {
    /// The chip it builds.
    type Chip: Chip;

    /// The system clock's frequency out of power-on.
    fn system_clock(&self) -> Frequency;

    /// The handle by which the board's routers ask the chip's runs to
    /// stop.
    fn stop_request(&self) -> StopRequest;

    /// The handles by which the board's routers sync the chip's run and
    /// divert its memories.
    fn requests(&self) -> Rc<dyn Requests>;

    /// A memory, whose cycles go to `router` while a peripheral has it
    /// ([`Requests::divert`]).
    ///
    /// # Errors
    ///
    /// Wiring the chip refuses ([`LoadError::Build`]).
    fn memory<R: BoardMapped>(
        &mut self,
        plan: MemoryPlan<'_>,
        router: R,
    ) -> Result<<Self::Chip as Chip>::Memory, LoadError>;

    /// `router` on `select`, the peripherals there behind it, with the
    /// DSACK pins they drive if they agree on them.
    ///
    /// # Errors
    ///
    /// Wiring the chip refuses ([`LoadError::Build`]).
    fn peripheral<R: BoardMapped>(
        &mut self,
        router: R,
        select: ChipSelect,
        dsack: Option<Dsack>,
    ) -> Result<(), LoadError>;

    /// A peripheral's interrupt output and reset input, on no pin: the
    /// resets that reach it and the IRQ pin its output is wired to.
    fn unwired<R: BoardMapped>(&mut self, pins: R, reset: ResetScope, irq: Option<IrqPin>);

    /// `router` on the QSPI.
    ///
    /// # Errors
    ///
    /// A chip with no QSPI ([`LoadError::Build`]).
    fn qspi<R: BoardSpi>(&mut self, router: R) -> Result<(), LoadError>;

    /// The chip, through its power-on reset, its pins at the levels its
    /// section gives.  It ends its run after an instruction that
    /// changes the system clock's frequency, for the board to move its
    /// time base there.
    fn build(self) -> Self::Chip;
}

/// A 683xx chip a board runs around.
pub trait Chip: Sized + 'static {
    /// Its name as `[chip] type` gives it: `"mc68331"`.
    const TYPE: &'static str;

    /// Its RESET pin: held low by a net, the chip stays in reset; it
    /// drives it low at each reset it reports ([`reset_pulse`]).
    ///
    /// [`reset_pulse`]: Self::reset_pulse
    const RESET: Self::Pin;

    /// The `[chip]` table, `type` and `note` among its keys, which it
    /// refuses others than its own beside.
    type Section: DeserializeOwned;

    /// Its pins.
    type Pin: ChipPin;

    /// What it reports beside its pins: resets, faults, traced cycles.
    type Event: fmt::Debug + Clone;

    /// A memory it holds.
    type Memory: Copy + fmt::Debug;

    /// Its builder.
    type Builder: ChipBuilder<Chip = Self>;

    /// The builder for a chip as `section` configures it.
    ///
    /// # Errors
    ///
    /// A section whose values the chip refuses.
    fn builder(section: &Self::Section) -> Result<Self::Builder, LoadError>;

    /// Now, on the system clock.
    fn now(&self) -> Cycles;

    /// Runs whole instructions from now for `budget` system clocks, or
    /// until the CPU halts, a hook or a router stops it, or a router
    /// asks it to end early ([`Requests::sync`]) or a watched output
    /// changes ([`stop_at_outputs`](Self::stop_at_outputs)), ending
    /// [`Stop::Budget`](mc683xx_cpu32::cpu::Stop::Budget) short of the
    /// budget.  A run that starts halted spends its budget as time
    /// passes.
    fn run_for(&mut self, budget: Cycles) -> Run;

    /// Why the CPU halted, while it stays halted.
    fn halted(&self) -> Option<Halt>;

    /// An external reset: RESET driven by the board.
    fn reset(&mut self);

    /// Makes the memory diversions the board's routers asked for
    /// between runs ([`Requests::divert`], [`Requests::release`]) now,
    /// so that a snapshot holds them.
    fn apply_diversions(&mut self);

    /// The CPU.
    fn cpu(&self) -> &Cpu;

    /// The CPU's registers, hooks and page permissions, to change.
    fn cpu_control(&mut self) -> CpuControl<'_>;

    /// Instructions run since the chip was built.
    fn instructions(&self) -> u64;

    /// The system clock's frequency now.
    fn system_clock(&self) -> Frequency;

    /// A memory's bytes.
    fn memory(&self, memory: Self::Memory) -> &[u8];

    /// A memory's bytes, to change.
    fn memory_mut(&mut self, memory: Self::Memory) -> &mut [u8];

    /// The output pins on nets, whose changes the chip reports: set
    /// once, as the board is built.
    fn watch_outputs(&mut self, on_nets: &[Self::Pin]);

    /// Those of the output pins on nets whose change ends its run: those
    /// a peripheral, the chip or an observer senses.  Set as the board is
    /// built and again as observers come and go.
    fn stop_at_outputs(&mut self, sensed: &[Self::Pin]);

    /// `signal` on input `pin` now, or for a [`ChipPin::scheduled`]
    /// pin at `at`.  Not RESET, which the board handles.
    fn input(&mut self, pin: Self::Pin, signal: Signal, at: Cycles);

    /// Withdraws the levels `input` scheduled on [`ChipPin::scheduled`]
    /// pins for after now, those up to now coming first: the board's
    /// time base moved (a frequency change, a reset held), and it
    /// schedules them again at their new clocks.
    fn unschedule_inputs(&mut self);

    /// The frequency a clock output (a pin of [`Kind::Clock`]) runs at
    /// now.
    fn clock_output(&self, pin: Self::Pin) -> Option<Frequency>;

    /// What the chip's outputs did since the last call, in the order it
    /// did them, each on board time through `base`: every pin of a
    /// changed port, changed or not (the board drives only changes),
    /// a frame on a serial pin.  Not clock outputs and not RESET.
    fn collect<F: FnMut(Time, Self::Pin, Signal)>(&mut self, base: TimeBase, emit: F);

    /// What the chip reported since the last call.
    fn take_events(&mut self) -> Vec<Self::Event>;

    /// When the chip drives RESET low and high again for `event`, if it
    /// is a reset the chip drives: the board converts the release with
    /// the system clock after the reset.
    fn reset_pulse(event: &Self::Event) -> Option<(Cycles, Cycles)>;

    /// The chip's state, versioned.
    ///
    /// # Errors
    ///
    /// A part of the chip that cannot save.
    fn save(&self) -> Result<Vec<u8>, StateError>;

    /// Restores a snapshot `save` made of a chip built the same way;
    /// on an error the chip is unchanged.
    ///
    /// # Errors
    ///
    /// A snapshot of another version or chip, or one a part refuses.
    fn restore(&mut self, snapshot: &[u8]) -> Result<(), StateError>;
}
