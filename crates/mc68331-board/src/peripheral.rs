//! A peripheral on the board: what a registered peripheral type builds.
//!
//! One trait for every peripheral, whichever of the bus, the QSPI and the
//! nets it is on: each method has a default, so a peripheral that is only
//! on nets (an oscillator, a GPS engine, an oven) implements its ports and
//! time and nothing of the bus, and a register on the bus nothing of the
//! nets ([PLAN.md, Boards][plan-boards]).
//!
//! [plan-boards]: https://github.com/charlieh0tel/mc683xx/blob/main/PLAN.md#boards

use std::any::Any;
use std::fmt;
use std::str::FromStr;

use mc683xx_cpu32::bus::BusError;
use mc683xx_cpu32::bus::PortSize;
use mc683xx_cpu32::cpu::state::StateError;
use mc683xx_cpu32::function_code::FunctionCode;
use mc683xx_cpu32::size::Size;
use mc683xx_modules::chip_select::ChipSelect;
use mc683xx_modules::peripheral::QspiClock;
use mc683xx_modules::peripheral::floating_miso;
use serde::Deserialize;
use serde::Serialize;

use crate::inspect::Inspection;
use crate::signal::Signal;
use crate::time::Time;
use crate::time::TimeBase;

/// Which cycles of its select or decoder output an attachment carries to
/// the peripheral: the board's wiring of the peripheral's /OE and /WE.
///
/// The SIM decides which cycles assert a select (CSORx R/W, BYTE, SPACE;
/// SIMRM 7.10.3); the role says what the peripheral does with them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum Role {
    /// Reads only: a write through it latches nothing.
    Read,
    /// Writes only: a read through it is not driven, and reads 0, as
    /// the chip reads a select nothing drives.
    Write,
    /// Both.
    #[default]
    ReadWrite,
    /// Interrupt acknowledge cycles, in CPU space (SIMRM 7.8.1).
    Iack,
}

impl Role {
    /// Every one, in declaration order.
    const EVERY: [Self; 4] = [Self::Read, Self::Write, Self::ReadWrite, Self::Iack];

    /// Its name, as an attachment's `role` gives it: `read`, `write`,
    /// `read-write` or `iack`.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Write => "write",
            Self::ReadWrite => "read-write",
            Self::Iack => "iack",
        }
    }
}

impl fmt::Display for Role {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.name())
    }
}

/// The attachment role [`Role::name`] names.
impl FromStr for Role {
    type Err = ParseRoleError;

    fn from_str(name: &str) -> Result<Self, ParseRoleError> {
        Self::EVERY
            .into_iter()
            .find(|candidate| candidate.name() == name)
            .ok_or_else(|| ParseRoleError(name.to_owned()))
    }
}

/// Text that names no attachment role.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("no attachment role {0:?}")]
pub struct ParseRoleError(pub String);

/// The data lanes an attachment wires to the peripheral.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Lane {
    /// Both: the peripheral sees the cycle as it is, byte or word.
    #[default]
    Both,
    /// The upper byte lane, DATA\[15:8\]: an 8-bit peripheral that sees the
    /// byte at the even address of each cycle, a byte access.
    Upper,
    /// The lower byte lane, DATA\[7:0\]: the byte at the odd address.
    Lower,
}

impl Lane {
    /// Every one, in declaration order.
    const EVERY: [Self; 3] = [Self::Both, Self::Upper, Self::Lower];

    /// Its name, as an attachment's `lane` gives it: `both`, `upper` or
    /// `lower`.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Both => "both",
            Self::Upper => "upper",
            Self::Lower => "lower",
        }
    }
}

impl fmt::Display for Lane {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.name())
    }
}

/// The data lane [`Lane::name`] names.
impl FromStr for Lane {
    type Err = ParseLaneError;

    fn from_str(name: &str) -> Result<Self, ParseLaneError> {
        Self::EVERY
            .into_iter()
            .find(|candidate| candidate.name() == name)
            .ok_or_else(|| ParseLaneError(name.to_owned()))
    }
}

/// Text that names no data lane.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("no data lane {0:?}")]
pub struct ParseLaneError(pub String);

/// A bus cycle as a peripheral sees it, through one of its attachments.
/// Built with [`PeripheralAccess::new`], so a field added later gets a default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct PeripheralAccess {
    /// The attachment it came through: its index in the peripheral's
    /// `attach` list.
    pub attachment: usize,
    /// The chip select that carried it: the attachment's, or its
    /// decoder's; [`None`] for a peripheral on no select, which took it
    /// by its own decode.
    pub select: Option<ChipSelect>,
    /// That attachment's role.
    pub role: Role,
    /// Its lane: a peripheral on one lane sees byte accesses only, at the
    /// cycle's address on its lines.
    pub lane: Lane,
    /// The attachment's `offset` plus the address on the peripheral's own
    /// address pins: the ADDR lines its `lines` wire to them, in order.
    pub address: u32,
    /// Byte or word, after dynamic bus sizing to the port: the select's
    /// field, or with an external DSACK the peripheral's DSACK pins'.
    pub size: Size,
    /// The cycle's function code.
    pub function_code: FunctionCode,
    /// When its instruction started, on board time.
    pub now: Time,
}

impl PeripheralAccess {
    /// A cycle on `select` at `address`, of `size` and `function_code`,
    /// at `now`, through attachment 0, read-write, on both lanes.
    #[must_use]
    pub const fn new(
        select: ChipSelect,
        address: u32,
        size: Size,
        function_code: FunctionCode,
        now: Time,
    ) -> Self {
        Self {
            attachment: 0,
            select: Some(select),
            role: Role::ReadWrite,
            lane: Lane::Both,
            address,
            size,
            function_code,
            now,
        }
    }

    /// A cycle a peripheral on no select took by its own decode, at
    /// `address`, of `size` and `function_code`, at `now`, through
    /// attachment 0, read-write, on both lanes.
    #[must_use]
    pub const fn on_no_select(
        address: u32,
        size: Size,
        function_code: FunctionCode,
        now: Time,
    ) -> Self {
        Self {
            select: None,
            ..Self::new(ChipSelect::Boot, address, size, function_code, now)
        }
    }

    /// The access through attachment `attachment` of `role` on `lane`.
    #[must_use]
    pub const fn through(self, attachment: usize, role: Role, lane: Lane) -> Self {
        Self {
            attachment,
            role,
            lane,
            ..self
        }
    }

    /// The byte an 8-bit peripheral takes of a write's `value`.  An 8-bit
    /// port's data is on DATA\[15:8\] (SIMRM 5.3.3): a byte cycle's byte,
    /// or a word cycle's upper byte.  A word reaches an 8-bit peripheral only
    /// where the port is 16 bits wide; on an 8-bit port the SIM splits
    /// it into two byte cycles, and a lane carries byte cycles only.
    #[must_use]
    pub const fn written_byte(&self, value: u32) -> u8 {
        match self.size {
            Size::Byte => value as u8,
            Size::Word | Size::Long => (value >> 8) as u8,
        }
    }

    /// What an 8-bit peripheral's read of `byte` puts on the bus: the byte, on
    /// DATA\[15:8\] for a word cycle (SIMRM 5.3.3), the lower byte not
    /// driven, as `written_byte` takes it.
    #[must_use]
    pub fn read_byte(&self, byte: u8) -> u32 {
        match self.size {
            Size::Byte => u32::from(byte),
            Size::Word | Size::Long => u32::from(byte) << 8,
        }
    }
}

/// A transfer on the QSPI as a peripheral sees it.  Built with
/// [`Transfer::new`], so a field added later gets a default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct Transfer {
    /// The attachment it came through: its index in `attach`.
    pub attachment: usize,
    /// The queue entry, 0 to 15: its index in the QSPI RAM (QSMRM
    /// 4.3.6).
    pub entry: u8,
    /// The levels on PCS\[3:0\].
    pub chip_selects: u8,
    /// The bits shifted out, right-aligned, most significant first.
    pub out: u16,
    /// How many: 8 to 16 (QSMRM Table 4-2).
    pub bits: u8,
    /// The serial clock it is shifted with: SPCR0's CPOL, CPHA and SPBR
    /// (QSMRM 4.3.1).  Its mode says on which SCK edges the master
    /// changes MOSI and captures MISO ([`spi::Mode`]).
    ///
    /// [`spi::Mode`]: crate::spi::Mode
    pub clock: QspiClock,
    /// Whether it is the first transfer to reach the attachment since
    /// the board's decode selected it: the start of a frame.  A
    /// transfer of a frame held selected (CONT, QSMRM 4.3.6.3) after
    /// the first is not.
    pub starts_frame: bool,
    /// When it ends, on board time.
    pub now: Time,
}

impl Transfer {
    /// A transfer through `attachment` of `bits` of `out`, queue entry
    /// `entry`'s, with `chip_selects` on PCS\[3:0\], shifted with `clock`,
    /// ending at `now`: a frame's first.
    #[must_use]
    pub const fn new(
        attachment: usize,
        entry: u8,
        chip_selects: u8,
        out: u16,
        bits: u8,
        clock: QspiClock,
        now: Time,
    ) -> Self {
        Self {
            attachment,
            entry,
            chip_selects,
            out,
            bits,
            clock,
            starts_frame: true,
            now,
        }
    }

    /// The transfer, a later one of its frame.
    #[must_use]
    pub const fn continuing(self) -> Self {
        Self {
            starts_frame: false,
            ..self
        }
    }

    /// The same transfer of `bits` of `out`.
    #[must_use]
    pub const fn shifting(self, out: u16, bits: u8) -> Self {
        Self { out, bits, ..self }
    }

    /// The same transfer shifted with `clock`.
    #[must_use]
    pub const fn clocked(self, clock: QspiClock) -> Self {
        Self { clock, ..self }
    }
}

/// A value a peripheral drives on one of its ports, from a time on.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Driven {
    /// When it takes effect: never before the call that drove it.
    pub at: Time,
    /// The port: its index in the type's declared ports.
    pub port: usize,
    /// The value.
    pub signal: Signal,
}

/// Bytes a peripheral writes into one of the board's memories, by name: a
/// flash's command logic programming the array the core serves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryWrite {
    /// The memory's name in the board file.
    pub memory: String,
    /// Where in it.
    pub offset: usize,
    /// The bytes.
    pub bytes: Vec<u8>,
}

/// The analog value on an input port now, sampled from its net's
/// driver: given the peripheral's port and the time.
pub type Sampler<'a> = &'a dyn Fn(usize, Time) -> Option<f64>;

/// What a peripheral does to the board during a call: it drives its outputs,
/// each from its time (now or later), samples its analog inputs, writes
/// memories and asks the run to stop.
///
/// A drive is a commitment: it cannot be withdrawn.  A peripheral that
/// drives an edge ahead of its time (a divider knows its clock's next
/// edges) is exact to the chip's clock on a scheduled input (a GPT capture); one
/// that drives an edge only as it
/// happens reaches the chip when the instruction running then ends
/// ([PLAN.md, Boards][plan-boards]).
///
/// [plan-boards]: https://github.com/charlieh0tel/mc683xx/blob/main/PLAN.md#boards
pub struct Drive<'a> {
    /// The time of the call.
    now: Time,
    /// What was driven.
    driven: Vec<Driven>,
    /// Memory writes, applied when the chip's quantum ends.
    writes: Vec<MemoryWrite>,
    /// Whether the peripheral asked the run to stop.
    stop: bool,
    /// The peripheral's analog inputs, if the board gives them.
    sampler: Option<Sampler<'a>>,
    /// The chip's system clock on board time, if the board gives it.
    system_clock: Option<TimeBase>,
    /// The memories taken (`true`) or released, in order.
    memories: Vec<(String, bool)>,
}

impl fmt::Debug for Drive<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Drive")
            .field("now", &self.now)
            .field("driven", &self.driven)
            .field("writes", &self.writes)
            .field("stop", &self.stop)
            .finish_non_exhaustive()
    }
}

/// What a call did, as [`Drive::finish`] gives it.
#[derive(Debug, Clone, Default, PartialEq)]
#[non_exhaustive]
pub struct Driving {
    /// What was driven, in the order driven.
    pub driven: Vec<Driven>,
    /// Memory writes, in order.
    pub writes: Vec<MemoryWrite>,
    /// Whether a stop was asked for.
    pub stop: bool,
    /// The memories taken (`true`) or released, in order.
    pub memories: Vec<(String, bool)>,
}

impl<'a> Drive<'a> {
    /// A drive for a call at `now`, with no analog inputs.
    #[must_use]
    pub const fn new(now: Time) -> Self {
        Self {
            now,
            driven: Vec::new(),
            writes: Vec::new(),
            stop: false,
            sampler: None,
            system_clock: None,
            memories: Vec::new(),
        }
    }

    /// A drive for a call at `now` whose analog inputs `sampler` reads.
    #[must_use]
    pub const fn with_sampler(now: Time, sampler: Sampler<'a>) -> Self {
        Self {
            now,
            driven: Vec::new(),
            writes: Vec::new(),
            stop: false,
            sampler: Some(sampler),
            system_clock: None,
            memories: Vec::new(),
        }
    }

    /// Gives the drive the chip's system clock on board time, as the
    /// board gives every call.
    pub const fn set_system_clock(&mut self, system_clock: TimeBase) {
        self.system_clock = Some(system_clock);
    }

    /// The drive with the chip's system clock on board time, as
    /// [`Drive::set_system_clock`] gives it: for a test's or a host's
    /// drive in one expression.
    #[must_use]
    pub const fn with_system_clock(mut self, system_clock: TimeBase) -> Self {
        self.set_system_clock(system_clock);
        self
    }

    /// The time of the call.
    #[must_use]
    pub const fn now(&self) -> Time {
        self.now
    }

    /// The chip's system clock on board time, for converting between
    /// board time and the chip's clocks ([`TimeBase`]); always given by
    /// the board, [`None`] in a drive built without one.
    #[must_use]
    pub const fn system_clock(&self) -> Option<TimeBase> {
        self.system_clock
    }

    /// Drives `signal` on `port` now.
    pub fn drive(&mut self, port: usize, signal: Signal) {
        self.drive_at(self.now, port, signal);
    }

    /// Drives `signal` on `port` from `at`, or now if that is past.
    pub fn drive_at(&mut self, at: Time, port: usize, signal: Signal) {
        self.driven.push(Driven {
            at: at.max(self.now),
            port,
            signal,
        });
    }

    /// The value on analog input `port` now: its driver's `sample`, or
    /// else the value last driven on the net; [`None`] if nothing has
    /// driven it.
    #[must_use]
    pub fn sample(&self, port: usize) -> Option<f64> {
        self.sampler.and_then(|sampler| sampler(port, self.now))
    }

    /// Writes `bytes` at `offset` in the board's memory `memory` before
    /// the chip runs on: asked within the chip's run, as the instruction
    /// running ends (the run ends there); between quanta (`advance`,
    /// `input`), before the next one and before the run returns, so a
    /// snapshot holds it.
    pub fn write_memory(&mut self, memory: impl Into<String>, offset: usize, bytes: Vec<u8>) {
        self.writes.push(MemoryWrite {
            memory: memory.into(),
            offset,
            bytes,
        });
    }

    /// Takes the board's memory `memory` off the core's fast path.
    ///
    /// From the end of this call's cycle (between quanta, from the
    /// next quantum, and in a snapshot taken before it), every cycle that reaches it
    /// reaches this peripheral instead ([`Peripheral::memory_read`],
    /// `memory_write`), until `release_memory`.  A flash takes its array
    /// while it programs or erases, or in autoselect mode, so that firmware
    /// polling it reads status, not the array.  Taking a memory another
    /// peripheral holds takes it from that peripheral.  A memory at a
    /// fixed address (`base`) is never taken: no select carries its
    /// cycles, and they stay the memory's.
    pub fn take_memory(&mut self, memory: impl Into<String>) {
        self.memories.push((memory.into(), true));
    }

    /// Gives the memory `memory` back to the core's fast path, if this
    /// peripheral holds it.
    pub fn release_memory(&mut self, memory: impl Into<String>) {
        self.memories.push((memory.into(), false));
    }

    /// Asks the board's run to stop, after the instruction running now.
    pub const fn stop(&mut self) {
        self.stop = true;
    }

    /// What the call did.
    #[must_use]
    pub fn finish(self) -> Driving {
        Driving {
            driven: self.driven,
            writes: self.writes,
            stop: self.stop,
            memories: self.memories,
        }
    }
}

/// A peripheral on the board.
///
/// Its ports are the ones its type declared to
/// the registry, by index; its time is board time.  It is [`Any`], so a
/// host reaches its own peripheral's state by its type, with no
/// `as_any` of its own: `Board::peripheral::<T>(name)`, or a
/// `Box<dyn Peripheral>` upcast to `Box<dyn Any>` and downcast.
#[expect(
    unused_variables,
    reason = "the defaults ignore their arguments, and the names document them"
)]
pub trait Peripheral: Any {
    /// A read through an attachment.
    ///
    /// # Errors
    ///
    /// [`BusError`] for BERR: by default nothing answers.
    fn read(&mut self, access: PeripheralAccess, drive: &mut Drive<'_>) -> Result<u32, BusError> {
        Err(BusError)
    }

    /// A write of `value`, right-aligned, through an attachment.
    ///
    /// # Errors
    ///
    /// [`BusError`] for BERR: by default nothing answers.
    fn write(
        &mut self,
        access: PeripheralAccess,
        value: u32,
        drive: &mut Drive<'_>,
    ) -> Result<(), BusError> {
        Err(BusError)
    }

    /// A read of the board memory `memory`, which it has taken
    /// ([`Drive::take_memory`]): `access.address` is the offset in the
    /// memory, `access.attachment` the index of the memory's attachment
    /// that carried it, its role `Read`.
    ///
    /// # Errors
    ///
    /// [`BusError`] for BERR: by default nothing answers.
    fn memory_read(
        &mut self,
        memory: &str,
        access: PeripheralAccess,
        drive: &mut Drive<'_>,
    ) -> Result<u32, BusError> {
        Err(BusError)
    }

    /// A write of `value` to the board memory `memory`, which it has
    /// taken, as `memory_read`, its role `Write`.  By default it is
    /// ignored, as a ROM ignores writes.
    ///
    /// # Errors
    ///
    /// [`BusError`] for BERR; by default never.
    fn memory_write(
        &mut self,
        memory: &str,
        access: PeripheralAccess,
        value: u32,
        drive: &mut Drive<'_>,
    ) -> Result<(), BusError> {
        Ok(())
    }

    /// On a select whose DSACK field is external (%1111, SIMRM
    /// 7.10.3), the wait states before the peripheral asserts DSACK, or
    /// [`None`] if it never does.  Asked before the cycle's `read` or
    /// `write`.  By default it answers at once.
    fn wait_states(&mut self, access: PeripheralAccess) -> Option<u8> {
        Some(0)
    }

    /// With an external DSACK, the port this cycle's DSACK gives, for a
    /// peripheral whose pins differ from cycle to cycle: 16 bits on
    /// DSACK1, 8 on DSACK0 alone (SIMRM Table 5-2).  Asked with
    /// `wait_states`.  By default [`None`]: its `dsack` in the board
    /// file says.
    fn dsack_port(&mut self, access: PeripheralAccess) -> Option<PortSize> {
        None
    }

    /// Whether its interrupt output, which the board file wires to an
    /// IRQ pin (`irq`), is asserted now.  By default it is not.
    fn interrupt_request(&self) -> bool {
        false
    }

    /// Power comes up, at board time zero, before the chip's power-on
    /// reset and whatever its reset scope: a peripheral states its outputs'
    /// first levels here (a latch that no reset reaches, `reset = []`,
    /// before its first write).  By default nothing is driven.
    fn power_on(&mut self, drive: &mut Drive<'_>) {}

    /// RESET reaches it at `now`, as its reset scope says.  By default
    /// nothing changes.
    fn reset(&mut self, now: Time, drive: &mut Drive<'_>) {}

    /// A QSPI transfer while its attachment selects it: shifts its
    /// `bits` of `out`, most significant first, in its clock's mode,
    /// and returns the bits shifted back, right-aligned (QSMRM 4.3.6;
    /// `spi` reckons the edges).  By default MISO floats: all ones
    /// (`floating_miso`).
    fn transfer(&mut self, transfer: Transfer, drive: &mut Drive<'_>) -> u16 {
        floating_miso(transfer.bits)
    }

    /// Its QSPI attachment `attachment` becomes selected, or not, at
    /// `now`, as the board decodes PCS\[3:0\] for it: a peripheral framed
    /// by its select sees the frame start and end here.  The canonical
    /// framing for a board peripheral: a peripheral sees only its own select's
    /// changes, never another peripheral's ([`SpiPeripheral::chip_selects`], every
    /// level change, is the chip's, for a host without this crate).
    /// By default nothing changes.
    ///
    /// [`SpiPeripheral::chip_selects`]: mc683xx_modules::peripheral::SpiPeripheral::chip_selects
    fn select(&mut self, attachment: usize, selected: bool, now: Time, drive: &mut Drive<'_>) {}

    /// The QSPI's serial clock from `now` ([`SpiPeripheral::clock`]).  By
    /// default nothing changes.
    ///
    /// [`SpiPeripheral::clock`]: mc683xx_modules::peripheral::SpiPeripheral::clock
    fn qspi_clock(&mut self, clock: QspiClock, now: Time) {}

    /// Whether its QSPI calls (`transfer`, `select`, `qspi_clock`) may
    /// come late, in a batch ([`SpiPeripheral::deferrable`]).  Asked once,
    /// as the board is built; the board's PCS decode lets the QSPI
    /// batch only if every peripheral on the QSPI says so and no PCS pin
    /// is on a net a peripheral, the chip or an observer senses.  A batch runs
    /// where the CPU can next see the QSPI (its interrupt, a read or
    /// write of the QSM) or at the sync point, before [`Board::run_until`]
    /// goes on to the board's events: each call keeps its own `now`
    /// and its order, but the CPU, board time and the other peripherals may
    /// be past it.  So it may say yes only if, in those calls, it
    /// drives no net, asks for no interrupt and stops nothing (no
    /// [`Drive::drive`], `drive_at`, `stop`, `write_memory`,
    /// `take_memory` or `release_memory`, and no change to
    /// `interrupt_request`); shares no state with a bus peripheral the CPU
    /// reads or writes during the exchange, its own attachments on the
    /// bus included; and reads neither board time nor another peripheral
    /// ([`Drive::sample`] included), nor relies on its net inputs having
    /// arrived (a select on a port pin, a reset line), since nets the
    /// CPU drove during the quantum reach it only after.  By default,
    /// no: the run's quantum ends at each call.
    ///
    /// [`SpiPeripheral::deferrable`]: mc683xx_modules::peripheral::SpiPeripheral::deferrable
    /// [`Board::run_until`]: crate::board::Board::run_until
    fn qspi_deferrable(&self) -> bool {
        false
    }

    /// `signal` arrives on input `port` at `now`.  By default it is
    /// ignored.
    fn input(&mut self, port: usize, signal: Signal, now: Time, drive: &mut Drive<'_>) {}

    /// The value it drives on analog output `port` at `now`, for a
    /// sink that samples it then (an ADC converting): a source whose
    /// value moves continuously (a warming oven) computes it here
    /// rather than driving every change.  By default [`None`]: the value
    /// it last drove.
    fn sample(&mut self, port: usize, now: Time) -> Option<f64> {
        None
    }

    /// When it next changes on its own, if it will: the board calls
    /// `advance` then.  A time already past is advanced to at once.  By
    /// default never.
    fn next_event(&self) -> Option<Time> {
        None
    }

    /// Brings it to `now`, a time `next_event` gave.  It must move
    /// `next_event` past `now`, or to [`None`]: a peripheral advanced to one
    /// time over and over without that stalls the board, and the run
    /// fails ([`RunError::Stalled`]).  Several events at one time may each
    /// take an advance.
    ///
    /// [`RunError::Stalled`]: crate::error::RunError::Stalled
    fn advance(&mut self, now: Time, drive: &mut Drive<'_>) {}

    /// The chip's system clock on board time from now on: told once
    /// the board is loaded, at each change of its frequency (SYNCR, a
    /// reset; right after the instruction that made it), and after a
    /// restore.  A peripheral that converts times outside a call (in
    /// `next_event`) keeps it, and keeps no other: the clock runs at
    /// 8.388608 MHz only until SYNCR is written.  By default it is
    /// ignored.
    fn system_clock(&mut self, clock: TimeBase) {}

    /// Its state as a front end shows it: registers, FIFOs, levels,
    /// named ([`inspect::Inspection`]).  Read-only: it must change
    /// nothing, and is no part of the snapshot or the run.  By default
    /// empty.
    ///
    /// [`inspect::Inspection`]: crate::inspect::Inspection
    fn inspect(&self) -> Inspection {
        Inspection::default()
    }

    /// Its state, for the board's snapshot.
    ///
    /// # Errors
    ///
    /// A peripheral that cannot save: by default
    /// [`StateError::Unsupported`], and the board's save fails.
    fn save(&self) -> Result<Vec<u8>, StateError> {
        Err(StateError::unsupported("the peripheral"))
    }

    /// Restores what `save` gave.  It must take what its own `save` gives
    /// without error: the board checks a peripheral's snapshot by restoring
    /// it, and puts back the peripheral's own if another peripheral or the
    /// chip refuses theirs.
    ///
    /// # Errors
    ///
    /// A typed [`StateError`], the peripheral left as it was, for what no
    /// run of this peripheral could have saved: values out of range or
    /// inconsistent, or a snapshot of a peripheral built with other
    /// parameters.  By default [`StateError::Unsupported`].
    fn restore(&mut self, snapshot: &[u8]) -> Result<(), StateError> {
        Err(StateError::unsupported("the peripheral"))
    }
}

#[cfg(test)]
mod tests {
    use super::Lane;
    use super::ParseLaneError;
    use super::ParseRoleError;
    use super::Role;
    use mc683xx_cpu32::cycles::Cycles;
    use mc683xx_cpu32::frequency::Frequency;
    use mc683xx_cpu32::function_code::FunctionCode;
    use mc683xx_cpu32::size::Size;
    use mc683xx_modules::chip_select::ChipSelect;
    use mc683xx_modules::peripheral::ClockPhase;
    use mc683xx_modules::peripheral::ClockPolarity;
    use mc683xx_modules::peripheral::QspiClock;

    use super::Drive;
    use super::PeripheralAccess;
    use super::Transfer;
    use crate::time::Time;
    use crate::time::TimeBase;

    /// A data cycle of `size`.
    const fn access(size: Size) -> PeripheralAccess {
        PeripheralAccess::new(
            ChipSelect::Boot,
            0,
            size,
            FunctionCode::SUPERVISOR_DATA,
            Time::ZERO,
        )
    }

    #[test]
    fn an_8_bit_peripheral_is_on_data_15_8_of_a_word() {
        assert_eq!(access(Size::Byte).written_byte(0xa55a), 0x5a);
        assert_eq!(access(Size::Word).written_byte(0xa55a), 0xa5);
        assert_eq!(access(Size::Byte).read_byte(0x42), 0x42);
        assert_eq!(access(Size::Word).read_byte(0x42), 0x4200);
    }

    #[test]
    fn a_drive_takes_the_system_clock_in_one_expression() {
        let base = TimeBase::new(Cycles::ZERO, Time(5), Frequency::from_hz(16_777_216));
        let drive = Drive::new(Time(5)).with_system_clock(base);
        assert_eq!(drive.system_clock(), Some(base));
        assert_eq!(Drive::new(Time(5)).system_clock(), None);
    }

    #[test]
    fn a_transfer_takes_another_clock_and_keeps_the_rest() {
        let reset = QspiClock::reset(Frequency::from_hz(8_388_608));
        let transfer = Transfer::new(1, 2, 0xe, 0xa5, 8, reset, Time(9)).continuing();
        let fast = QspiClock {
            polarity: ClockPolarity::IdleHigh,
            phase: ClockPhase::CaptureLeading,
            period: Cycles(4),
            ..reset
        };
        let clocked = transfer.clocked(fast);
        assert_eq!(clocked.clock, fast);
        assert_eq!(
            Transfer {
                clock: reset,
                ..clocked
            },
            transfer
        );
    }

    #[test]
    fn roles_print_as_the_board_file_names_them_and_parse_back() {
        for role in Role::EVERY {
            assert_eq!(role.to_string().parse(), Ok(role));
        }
        assert_eq!("?".parse::<Role>(), Err(ParseRoleError("?".to_owned())));
    }

    #[test]
    fn lanes_print_as_the_board_file_names_them_and_parse_back() {
        for lane in Lane::EVERY {
            assert_eq!(lane.to_string().parse(), Ok(lane));
        }
        assert_eq!("?".parse::<Lane>(), Err(ParseLaneError("?".to_owned())));
    }
}
