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

use std::fmt;
use std::rc::Rc;
use std::str::FromStr;

use mc683xx_cpu32::Cpu;
use mc683xx_cpu32::CpuControl;
use mc683xx_cpu32::Cycle;
use mc683xx_cpu32::Cycles;
use mc683xx_cpu32::Frequency;
use mc683xx_cpu32::PortSize;
use mc683xx_cpu32::Run;
use mc683xx_cpu32::StopRequest;
use mc683xx_cpu32::interrupt::IrqPin;
use mc683xx_cpu32::memory::MemoryKind;
use mc683xx_cpu32::state::StateError;
use mc683xx_modules::chip_select::ChipSelect;
use mc683xx_modules::chip_select::Dsack;
use mc683xx_modules::chip_select::Termination;
use mc683xx_modules::peripheral::AnyMappedPeripheral;
use mc683xx_modules::peripheral::AnySpiPeripheral;
use mc683xx_modules::peripheral::BusDecode;
use mc683xx_modules::sim::ResetCause;
use mc683xx_modules::sim::ResetScope;
use mc683xx_target::Address;
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::description::LoadError;
use crate::signal::Direction;
use crate::signal::Kind;
use crate::signal::Signal;
use crate::time::Time;
use crate::time::TimeBase;

/// What seals [`Chip`], [`ChipBuilder`] and [`ChipPin`]: only the
/// workspace's chips implement them, so their methods may change in any
/// release.
pub(crate) mod sealed {
    /// Implemented by the workspace's chips, their builders and their
    /// pins alone.
    pub trait Sealed {}
}

/// A chip pin a net reaches, `chip.NAME` in a board file: the chip's own
/// set, each with what it carries and which way it faces.  It prints
/// as its name after `chip.` in upper case (`PE0`, `RESET`), and parses
/// from it in any case.
///
/// Sealed: a host cannot implement it.
pub trait ChipPin:
    Copy
    + Ord
    + fmt::Debug
    + fmt::Display
    + FromStr
    + Serialize
    + DeserializeOwned
    + sealed::Sealed
    + 'static
{
    /// Its name after `chip.`, as `Display` writes it, for a record
    /// that holds it ([`PinRecord::pin`](crate::trace::PinRecord::pin)).
    ///
    /// Not part of the semver API: plumbing the board runs the chip with.
    #[doc(hidden)]
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
    ///
    /// Not part of the semver API: plumbing the board runs the chip with.
    #[doc(hidden)]
    fn scheduled(self) -> bool;

    /// Whether it is one of the QSPI's peripheral chip selects,
    /// PCS\[3:0\]: a net that senses one keeps the QSPI's calls from
    /// being deferred, so it sees each change at its time.
    ///
    /// Not part of the semver API: plumbing the board runs the chip with.
    #[doc(hidden)]
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
///
/// Not part of the semver API: plumbing the board runs the chip with.
#[doc(hidden)]
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

    /// Within a cycle on the external bus, while the chip publishes it
    /// ([`Chip::set_publish_output_horizon`]): the earliest an output
    /// whose change ends the run ([`Chip::stop_at_outputs`]) may change
    /// after the cycle's start, its start if one changed earlier in its
    /// instruction.  [`None`] if none will.
    fn output_horizon(&self) -> Option<Cycle>;
}

/// Where a board memory answers.
///
/// Not part of the semver API: plumbing the board runs the chip with.
#[doc(hidden)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum MemoryPlace<'a> {
    /// On these chip-select pins.
    Selects(&'a [ChipSelect]),
    /// At a fixed address on no pin, emulator-only.
    Fixed {
        /// Its first byte's address.
        base: Address,
        /// Its port's width.
        port: PortSize,
        /// How its cycles end: fast termination or wait states.
        termination: Termination,
    },
}

/// A board memory as the board hands it to the chip.
///
/// Not part of the semver API: plumbing the board runs the chip with.
#[doc(hidden)]
#[derive(Debug)]
pub struct MemoryPlan<'a> {
    /// Its contents.
    pub bytes: Vec<u8>,
    /// Whether writes reach it.
    pub kind: MemoryKind,
    /// Where it answers.
    pub place: MemoryPlace<'a>,
    /// The resets that put its contents back.
    pub reload: ResetScope,
}

/// The chip being wired, before its power-on reset.
///
/// Sealed: a host cannot implement it.  Not part of the semver API:
/// only the board builds a chip.
#[doc(hidden)]
pub trait ChipBuilder: sealed::Sealed {
    /// The chip it builds.
    type Chip: Chip;

    /// The system clock's frequency out of power-on.
    fn system_frequency(&self) -> Frequency;

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
    fn memory<R: AnyMappedPeripheral>(
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
    fn peripheral<R: AnyMappedPeripheral>(
        &mut self,
        router: R,
        select: ChipSelect,
        dsack: Option<Dsack>,
    ) -> Result<(), LoadError>;

    /// `router` on no select, taking the cycles `decodes` give, its
    /// peripherals driving DSACK `pins`.
    ///
    /// # Errors
    ///
    /// Wiring the chip refuses ([`LoadError::Build`]).
    fn bus_peripheral<R: AnyMappedPeripheral>(
        &mut self,
        router: R,
        decodes: &[BusDecode],
        pins: Dsack,
    ) -> Result<(), LoadError>;

    /// A peripheral's interrupt output and reset input, on no pin: the
    /// resets that reach it and the IRQ pin its output is wired to.
    fn unwired<R: AnyMappedPeripheral>(&mut self, pins: R, reset: ResetScope, irq: Option<IrqPin>);

    /// `router` on the QSPI.
    ///
    /// # Errors
    ///
    /// A chip with no QSPI ([`LoadError::Build`]).
    fn set_qspi<R: AnySpiPeripheral>(&mut self, router: R) -> Result<(), LoadError>;

    /// The chip, through its power-on reset, its pins at the levels its
    /// section gives.  It ends its run after an instruction that
    /// changes the system clock's frequency, for the board to move its
    /// time base there.
    fn build(self) -> Self::Chip;
}

/// A 683xx chip a board runs around.
///
/// Sealed: a host cannot implement it.  A new part (a 68332) is added
/// here, beside [`crate::mc68331`]; a host names the chip a board runs
/// (`Board<Mc68331>`) or is generic over it (`Board<C: Chip>`) and calls
/// its methods, which is all a host needs.  So this does not compile:
///
/// ```compile_fail,E0277
/// use mc683xx_board::chip::Chip;
///
/// struct Mine;
///
/// impl Chip for Mine {}
/// ```
pub trait Chip: Sized + sealed::Sealed + 'static {
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
    ///
    /// Not part of the semver API: plumbing the board runs the chip with.
    #[doc(hidden)]
    fn builder(section: &Self::Section) -> Result<Self::Builder, LoadError>;

    /// Now, on the system clock.
    fn now(&self) -> Cycle;

    /// Runs whole instructions from now for `budget` system clocks, or
    /// until the CPU halts, a hook or a router stops it, or a router
    /// asks it to end early ([`Requests::sync`]) or a watched output
    /// changes ([`stop_at_outputs`](Self::stop_at_outputs)), ending
    /// [`Stop::Budget`](mc683xx_cpu32::Stop::Budget) short of the
    /// budget.  A run that starts halted spends its budget as time
    /// passes.
    fn run_for(&mut self, budget: Cycles) -> Run;

    /// A reset now for `cause`, as its source makes it:
    /// [`ResetCause::External`] is RESET driven by the board.
    fn reset_for(&mut self, cause: ResetCause);

    /// RESET driven by the board with BKPT held low through its rising
    /// edge, as a BDM debugger's reset: background mode enabled, and the
    /// CPU in it before its first instruction (CPU32RM 7.2.1).
    fn reset_with_breakpoint(&mut self);

    /// BKPT asserted between runs (CPU32RM 7.2.2.1): with background
    /// mode enabled the CPU enters it at once, and this is true;
    /// disabled, a hardware breakpoint is left pending for the next run,
    /// and this is false; halted, nothing, and false.
    fn assert_breakpoint(&mut self) -> bool;

    /// Makes the memory diversions the board's routers asked for
    /// between runs ([`Requests::divert`], [`Requests::release`]) now,
    /// so that a snapshot holds them.
    ///
    /// Not part of the semver API: plumbing the board runs the chip with.
    #[doc(hidden)]
    fn apply_diversions(&mut self);

    /// The CPU.
    fn cpu(&self) -> &Cpu;

    /// The CPU's registers, hooks and page permissions, to change.
    fn cpu_control(&mut self) -> CpuControl<'_>;

    /// Instructions run since the chip was built.
    fn instructions(&self) -> u64;

    /// The system clock's frequency now.
    fn system_frequency(&self) -> Frequency;

    /// A memory's bytes.
    fn memory(&self, memory: Self::Memory) -> &[u8];

    /// A memory's bytes, to change.
    fn memory_mut(&mut self, memory: Self::Memory) -> &mut [u8];

    /// The output pins on nets, whose changes the chip reports: set
    /// once, as the board is built.
    ///
    /// Not part of the semver API: plumbing the board runs the chip with.
    #[doc(hidden)]
    fn watch_outputs(&mut self, on_nets: &[Self::Pin]);

    /// Those of the output pins on nets whose change ends its run: those
    /// a peripheral, the chip or an observer senses.  Set as the board is
    /// built and again as observers come and go.
    ///
    /// Not part of the semver API: plumbing the board runs the chip with.
    #[doc(hidden)]
    fn stop_at_outputs(&mut self, sensed: &[Self::Pin]);

    /// Whether it gives [`Requests::output_horizon`] within its cycles:
    /// set as the board is built, on if a peripheral asks for its
    /// accesses' horizons.
    ///
    /// Not part of the semver API: plumbing the board runs the chip with.
    #[doc(hidden)]
    fn set_publish_output_horizon(&mut self, publish: bool);

    /// `signal` on input `pin` now, or for a [`ChipPin::scheduled`]
    /// pin at `at`.  Not RESET, which the board handles.
    ///
    /// Not part of the semver API: plumbing the board runs the chip with.
    #[doc(hidden)]
    fn input(&mut self, pin: Self::Pin, signal: Signal, at: Cycle);

    /// Withdraws the levels `input` scheduled on [`ChipPin::scheduled`]
    /// pins for after now, those up to now coming first: the board's
    /// time base moved (a frequency change, a reset held), and it
    /// schedules them again at their new clocks.
    ///
    /// Not part of the semver API: plumbing the board runs the chip with.
    #[doc(hidden)]
    fn unschedule_inputs(&mut self);

    /// The frequency a clock output (a pin of [`Kind::Clock`]) runs at
    /// now.
    fn clock_output(&self, pin: Self::Pin) -> Option<Frequency>;

    /// What the chip's outputs did since the last call, in the order it
    /// did them, each on board time through `base`: every pin of a
    /// changed port, changed or not (the board drives only changes),
    /// a frame on a serial pin.  Not clock outputs and not RESET.
    ///
    /// Not part of the semver API: plumbing the board runs the chip with.
    #[doc(hidden)]
    fn collect<F: FnMut(Time, Self::Pin, Signal)>(&mut self, base: TimeBase, emit: F);

    /// What the chip reported since the last call.
    fn take_events(&mut self) -> Vec<Self::Event>;

    /// When the chip drives RESET low and high again for `event`, if it
    /// is a reset the chip drives: the board converts the release with
    /// the system clock after the reset.
    ///
    /// Not part of the semver API: plumbing the board runs the chip with.
    #[doc(hidden)]
    fn reset_pulse(event: &Self::Event) -> Option<(Cycle, Cycle)>;

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
