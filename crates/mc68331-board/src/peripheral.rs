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

use mc683xx_cpu32::BusError;
use mc683xx_cpu32::Cycles;
use mc683xx_cpu32::PortSize;
use mc683xx_cpu32::state::StateError;
use mc683xx_modules::chip_select::ChipSelect;
use mc683xx_modules::peripheral::QspiClock;
use mc683xx_modules::peripheral::floating_miso;
use mc683xx_target::FunctionCode;
use mc683xx_target::Size;
use serde::Deserialize;
use serde::Serialize;

use crate::condition::ArmError;
use crate::condition::ConditionInfo;
use crate::condition::ConditionToken;
use crate::inspect::Inspection;
use crate::signal::Signal;
use crate::time::Moment;
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
    const ALL: [Self; 4] = [Self::Read, Self::Write, Self::ReadWrite, Self::Iack];

    /// Its name, as an attachment's `role` gives it: `read`, `write`,
    /// `read-write` or `iack`.
    #[must_use]
    const fn name(self) -> &'static str {
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

/// The attachment role its name, as it displays, names.
impl FromStr for Role {
    type Err = ParseRoleError;

    fn from_str(name: &str) -> Result<Self, ParseRoleError> {
        Self::ALL
            .into_iter()
            .find(|candidate| candidate.name() == name)
            .ok_or_else(|| ParseRoleError(name.to_owned()))
    }
}

/// Text that names no attachment role.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("no attachment role {0:?}")]
pub struct ParseRoleError(String);

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
    pub const ALL: [Self; 3] = [Self::Both, Self::Upper, Self::Lower];

    /// Its name, as an attachment's `lane` gives it: `both`, `upper` or
    /// `lower`.
    #[must_use]
    const fn name(self) -> &'static str {
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

/// The data lane its name, as it displays, names.
impl FromStr for Lane {
    type Err = ParseLaneError;

    fn from_str(name: &str) -> Result<Self, ParseLaneError> {
        Self::ALL
            .into_iter()
            .find(|candidate| candidate.name() == name)
            .ok_or_else(|| ParseLaneError(name.to_owned()))
    }
}

/// Text that names no data lane.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("no data lane {0:?}")]
pub struct ParseLaneError(String);

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
    /// The address the peripheral sees, local to it, not the CPU's: the
    /// attachment's `offset` plus the address on the peripheral's own
    /// address pins, the ADDR lines its `lines` wire to them, in order.
    /// For a taken memory ([`Peripheral::memory_read`]), the offset in
    /// the memory.
    pub address: u32,
    /// Byte or word, after dynamic bus sizing to the port: the select's
    /// field, or with an external DSACK the peripheral's DSACK pins'.
    pub size: Size,
    /// The cycle's function code.
    pub function_code: FunctionCode,
    /// Whether it is a write: R/W low (SIMRM 5.1.5).  Set for
    /// `wait_states` and `dsack_port` too, which a peripheral that
    /// times or sizes its writes apart from its reads reads.
    pub write: bool,
    /// On a read or a write of a peripheral that asks for it
    /// ([`Peripheral::wants_horizon`]), the board time before which
    /// everything that could reach it has: the earliest of the net
    /// changes waiting, the other peripherals' next events (`now`, the
    /// call's time, when its instruction started, for one busy) and
    /// undelivered drives, the next change of a chip output the board
    /// senses (`now` if one changed earlier in the instruction), and the
    /// chip's run's end.  A peripheral that samples a cycle later than
    /// [`Drive::now`] (at its end, after its wait
    /// states) and before this may advance its own model to its sample
    /// and answer from it; at or past this, an input dated before its
    /// sample may still arrive after the access.  Not counted: the
    /// drives a deferred QSPI peripheral
    /// ([`Peripheral::qspi_deferrable`]) has yet to make.  [`None`] otherwise.
    ///
    /// The run's end is where the host cuts the run, so the peripheral's
    /// answer must not depend on which side of this its sample falls
    /// unless an input lands before its sample: else the run depends on
    /// how the host cuts it.
    pub horizon: Option<Time>,
}

impl PeripheralAccess {
    /// A read cycle on `select` at `address`, of `size` and
    /// `function_code`, through attachment 0, read-write, on both lanes.
    #[must_use]
    pub const fn new(
        select: ChipSelect,
        address: u32,
        size: Size,
        function_code: FunctionCode,
    ) -> Self {
        Self {
            attachment: 0,
            select: Some(select),
            role: Role::ReadWrite,
            lane: Lane::Both,
            address,
            size,
            function_code,
            write: false,
            horizon: None,
        }
    }

    /// A read cycle a peripheral on no select took by its own decode, at
    /// `address`, of `size` and `function_code`, through attachment 0,
    /// read-write, on both lanes.
    #[must_use]
    pub const fn unselected(address: u32, size: Size, function_code: FunctionCode) -> Self {
        Self {
            select: None,
            ..Self::new(ChipSelect::Boot, address, size, function_code)
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

    /// This access, a write or a read.
    #[must_use]
    pub const fn writing(self, write: bool) -> Self {
        Self { write, ..self }
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
    pub const fn read_byte(&self, byte: u8) -> u32 {
        match self.size {
            Size::Byte => byte as u32,
            Size::Word | Size::Long => (byte as u32) << 8,
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
}

impl Transfer {
    /// A frame's first transfer of `bits` of `out`, shifted with
    /// `clock`: through attachment 0, queue entry 0, PCS\[3:0\] all low.
    #[must_use]
    pub const fn new(out: u16, bits: u8, clock: QspiClock) -> Self {
        Self {
            attachment: 0,
            entry: 0,
            chip_selects: 0,
            out,
            bits,
            clock,
            starts_frame: true,
        }
    }

    /// The transfer through attachment `attachment`.
    #[must_use]
    pub const fn through(self, attachment: usize) -> Self {
        Self { attachment, ..self }
    }

    /// The transfer of queue entry `entry`.
    #[must_use]
    pub const fn at_entry(self, entry: u8) -> Self {
        Self { entry, ..self }
    }

    /// The transfer with `chip_selects` on PCS\[3:0\].
    #[must_use]
    pub const fn selecting(self, chip_selects: u8) -> Self {
        Self {
            chip_selects,
            ..self
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
#[non_exhaustive]
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
#[non_exhaustive]
pub struct MemoryWrite {
    /// The memory's name in the board file.
    pub memory: String,
    /// Where in it.
    pub offset: usize,
    /// The bytes.
    pub bytes: Vec<u8>,
}

/// A board memory a peripheral took or gave back during a call
/// ([`Drive::take_memory`], [`Drive::release_memory`]), by name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MemoryClaim {
    /// It took the memory off the core's fast path.
    Taken(String),
    /// It gave the memory back.
    Released(String),
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
    /// The time of the call and the chip's system clock then.
    moment: Moment,
    /// What was driven.
    driven: Vec<Driven>,
    /// Memory writes, applied when the chip's quantum ends.
    writes: Vec<MemoryWrite>,
    /// Whether the peripheral asked the run to stop.
    stop: bool,
    /// The conditions that held, by the peripheral's tokens.
    fired: Vec<ConditionToken>,
    /// The peripheral's analog inputs, if the board gives them.
    sampler: Option<Sampler<'a>>,
    /// The memories taken or released, in order.
    memories: Vec<MemoryClaim>,
}

impl fmt::Debug for Drive<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Drive")
            .field("moment", &self.moment)
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
    /// The memories taken or released, in order.
    pub memories: Vec<MemoryClaim>,
    /// The conditions that held, by the peripheral's tokens, in order.
    pub fired: Vec<ConditionToken>,
}

impl<'a> Drive<'a> {
    /// A drive for a call at `moment`, with no analog inputs.
    #[must_use]
    pub const fn new(moment: Moment) -> Self {
        Self {
            moment,
            driven: Vec::new(),
            writes: Vec::new(),
            stop: false,
            fired: Vec::new(),
            sampler: None,
            memories: Vec::new(),
        }
    }

    /// A drive for a call at `moment`, whose analog inputs `sampler`
    /// reads.
    #[must_use]
    pub fn with_sampler(moment: Moment, sampler: Sampler<'a>) -> Self {
        Self {
            sampler: Some(sampler),
            ..Self::new(moment)
        }
    }

    /// The call's moment: its time and the chip's system clock then, for
    /// converting between board time and the chip's clocks.
    #[must_use]
    pub const fn moment(&self) -> Moment {
        self.moment
    }

    /// The time of the call: its moment's `now`.
    #[must_use]
    pub const fn now(&self) -> Time {
        self.moment.now()
    }

    /// The chip's system clock on board time at the call: its moment's
    /// `system_clock`.
    #[must_use]
    pub const fn system_clock(&self) -> TimeBase {
        self.moment.system_clock()
    }

    /// Drives `signal` on `port` now.
    pub fn drive(&mut self, port: usize, signal: Signal) {
        self.drive_at(self.now(), port, signal);
    }

    /// Drives `signal` on `port` from `at`, or now if that is past.
    pub fn drive_at(&mut self, at: Time, port: usize, signal: Signal) {
        self.driven.push(Driven {
            at: at.max(self.now()),
            port,
            signal,
        });
    }

    /// The value on analog input `port` now: its driver's
    /// [`Peripheral::analog_output`], or else the value last driven on the
    /// net; [`None`] if nothing has driven it.
    #[must_use]
    pub fn sample(&self, port: usize) -> Option<f64> {
        self.sampler.and_then(|sampler| sampler(port, self.now()))
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
        self.memories.push(MemoryClaim::Taken(memory.into()));
    }

    /// Gives the memory `memory` back to the core's fast path, if this
    /// peripheral holds it.
    pub fn release_memory(&mut self, memory: impl Into<String>) {
        self.memories.push(MemoryClaim::Released(memory.into()));
    }

    /// Asks the board's run to stop, after the instruction running now.
    pub const fn stop(&mut self) {
        self.stop = true;
    }

    /// Says that the condition the peripheral armed as `token`
    /// ([`Peripheral::arm`]) holds, and asks the run to stop as `stop`
    /// does: the board names the condition ([`Conditions::fired`]).
    ///
    /// [`Conditions::fired`]: crate::condition::Conditions::fired
    pub fn fire(&mut self, token: ConditionToken) {
        self.fired.push(token);
        self.stop();
    }

    /// What the call did.
    #[must_use]
    pub fn finish(self) -> Driving {
        Driving {
            driven: self.driven,
            writes: self.writes,
            stop: self.stop,
            memories: self.memories,
            fired: self.fired,
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
///
/// A call's time is a [`Moment`], board time and the chip's system clock
/// then: one given a [`Drive`] takes it from [`Drive::moment`] (`read`,
/// `write`, `memory_read`, `memory_write`, `power_on`, `reset`,
/// `transfer`, `set_selected`, `input`, `advance`); one without a drive
/// takes it as its last argument, `moment` (`peek`, `poke`,
/// `wait_states`, `dsack_port`, `set_qspi_clock`, `analog_output`,
/// `next_event`).  No call has two times.
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

    /// Fills `data` with what byte reads through an attachment from
    /// `access.address` up, on the peripheral's own addresses, would
    /// return at `moment`, one byte an address, with none of their side
    /// effects: a debugger's look (the chip's `peek`, gdb's memory
    /// reads), as [`MappedPeripheral::peek`].  `access` is the first
    /// byte's, of size [`Size::Byte`]; the board gathers the bytes of a
    /// peripheral on one lane, which the CPU sees at every other
    /// address, and calls once for each run of the peripheral's
    /// addresses an access reaches.  Whether it could; by default it
    /// declines, as only a real cycle can tell, and the look fails, all
    /// of `data` with it.  A lazily advanced model, one that brings its
    /// state up to date only inside `read` (settling what happened
    /// since its last call), declines where catching up could change
    /// the answer, as `&self` cannot catch up: a board advances every
    /// peripheral to its time before a host's peek, but a direct call
    /// at a later `moment` gets the state as of the last `advance`.
    ///
    /// [`MappedPeripheral::peek`]: mc683xx_cpu32::peripheral::MappedPeripheral::peek
    fn peek(&self, access: PeripheralAccess, data: &mut [u8], moment: Moment) -> bool {
        false
    }

    /// Stores `data` where byte writes through an attachment from
    /// `access.address` up would at `moment`, one byte an address, with
    /// none of their side effects: a debugger's poke (the chip's `poke`,
    /// gdb's memory writes), as [`MappedPeripheral::poke`], `access` and
    /// the runs as for `peek`.  Whether it took them; by default it
    /// declines.  One that declines should store none of them.  A
    /// lazily advanced model declines where state it has yet to settle
    /// would change what a byte becomes, as for `peek`.
    ///
    /// [`MappedPeripheral::poke`]: mc683xx_cpu32::peripheral::MappedPeripheral::poke
    fn poke(&mut self, access: PeripheralAccess, data: &[u8], moment: Moment) -> bool {
        false
    }

    /// On a select whose DSACK field is external (%1111, SIMRM
    /// 7.10.3), the wait states before the peripheral asserts DSACK, or
    /// [`None`] if it never does, asked at `moment`, when the cycle's
    /// instruction started, before the cycle's `read` or `write`.  By
    /// default it answers at once.
    fn wait_states(&mut self, access: PeripheralAccess, moment: Moment) -> Option<Cycles> {
        Some(Cycles::ZERO)
    }

    /// With an external DSACK, the port this cycle's DSACK gives, for a
    /// peripheral whose pins differ from cycle to cycle: 16 bits on
    /// DSACK1, 8 on DSACK0 alone (SIMRM Table 5-2).  Asked with
    /// `wait_states`, at the same `moment`.  By default [`None`]: its
    /// `dsack` in the board file says.
    fn dsack_port(&mut self, access: PeripheralAccess, moment: Moment) -> Option<PortSize> {
        None
    }

    /// Whether its reads and writes carry
    /// [`PeripheralAccess::horizon`], asked once, as the board is built.
    /// By default no, which costs the board nothing.
    fn wants_horizon(&self) -> bool {
        false
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

    /// RESET reaches it, at the drive's time, as its reset scope says.
    /// By default nothing changes.
    fn reset(&mut self, drive: &mut Drive<'_>) {}

    /// A QSPI transfer while its attachment selects it: shifts its
    /// `bits` of `out`, most significant first, in its clock's mode,
    /// and returns the bits shifted back, right-aligned (QSMRM 4.3.6;
    /// `spi` reckons the edges).  It ends at the drive's time.  By
    /// default MISO floats: all ones (`floating_miso`).
    fn transfer(&mut self, transfer: Transfer, drive: &mut Drive<'_>) -> u16 {
        floating_miso(transfer.bits)
    }

    /// Its QSPI attachment `attachment` becomes selected, or not, at
    /// the drive's time, as the board decodes PCS\[3:0\] for it: a peripheral framed
    /// by its select sees the frame start and end here.  The canonical
    /// framing for a board peripheral: a peripheral sees only its own select's
    /// changes, never another peripheral's ([`SpiPeripheral::set_chip_selects`], every
    /// level change, is the chip's, for a host without this crate).
    /// By default nothing changes.
    ///
    /// [`SpiPeripheral::set_chip_selects`]: mc683xx_modules::peripheral::SpiPeripheral::set_chip_selects
    fn set_selected(&mut self, attachment: usize, selected: bool, drive: &mut Drive<'_>) {}

    /// The QSPI's serial clock from `moment`
    /// ([`SpiPeripheral::set_clock`]).  By default nothing changes.
    ///
    /// [`SpiPeripheral::set_clock`]: mc683xx_modules::peripheral::SpiPeripheral::set_clock
    fn set_qspi_clock(&mut self, clock: QspiClock, moment: Moment) {}

    /// Whether its QSPI calls (`transfer`, `set_selected`,
    /// `set_qspi_clock`) may come late, in a batch
    /// ([`SpiPeripheral::deferrable`]).  Asked once,
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

    /// `signal` arrives on input `port`, at the drive's time.  By
    /// default it is ignored.
    fn input(&mut self, port: usize, signal: Signal, drive: &mut Drive<'_>) {}

    /// The value it drives on analog output `port` at `moment`, for a
    /// sink that samples it then ([`Drive::sample`]; an ADC converting):
    /// a source whose value moves continuously (a warming oven) computes
    /// it here rather than driving every change.  By default [`None`]:
    /// the value it last drove.
    fn analog_output(&mut self, port: usize, moment: Moment) -> Option<f64> {
        None
    }

    /// When it next changes on its own, if it will, asked at `moment`:
    /// the board calls `advance` then.  `moment.now()` is board time as
    /// the board asks, at or after the peripheral's last call (its last
    /// `advance`, `input` or bus cycle), not that call's time, and the
    /// answer is a time in board time, not a span from it: an event
    /// already past is advanced to at once.  The moment's system clock
    /// is the chip's, for a peripheral whose events fall on the chip's
    /// clocks: asked again whenever the board needs it, so a part keeps
    /// no base between calls.  By default never.
    fn next_event(&self, moment: Moment) -> Option<Time> {
        None
    }

    /// Brings it to the drive's time, a time `next_event` gave.  It must
    /// move `next_event` past that, or to [`None`]: a peripheral advanced to one
    /// time over and over without that stalls the board, and the run
    /// fails ([`RunError::Stalled`]).  Several events at one time may each
    /// take an advance.
    ///
    /// [`RunError::Stalled`]: crate::board::RunError::Stalled
    fn advance(&mut self, drive: &mut Drive<'_>) {}

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

    /// The conditions a debugger may stop the run on
    /// ([`condition`](crate::condition)), an emulator's alone: by
    /// default none.
    fn conditions(&self) -> &'static [ConditionInfo] {
        &[]
    }

    /// Arms its condition `condition` with `arguments`: its own token
    /// for it, which it gives [`Drive::fire`] each time the condition
    /// holds, and which [`disarm`](Self::disarm) gives back.  It stays
    /// armed until disarmed.  Armed conditions are outside the
    /// snapshot: `save` leaves them out, and `restore` keeps them.
    ///
    /// # Errors
    ///
    /// [`ArmError`] for a condition it does not offer or arguments it
    /// does not take: by default every condition is unknown.
    fn arm(&mut self, condition: &str, arguments: &[String]) -> Result<ConditionToken, ArmError> {
        Err(ArmError::Unknown(condition.to_owned()))
    }

    /// Disarms the condition it armed as `token`.  By default it is
    /// ignored.
    fn disarm(&mut self, token: ConditionToken) {}
}

#[cfg(test)]
mod tests {
    use super::Lane;
    use super::ParseLaneError;
    use super::ParseRoleError;
    use super::Role;
    use mc683xx_cpu32::Cycle;
    use mc683xx_cpu32::Cycles;
    use mc683xx_cpu32::Frequency;
    use mc683xx_modules::chip_select::ChipSelect;
    use mc683xx_modules::peripheral::ClockPhase;
    use mc683xx_modules::peripheral::ClockPolarity;
    use mc683xx_modules::peripheral::QspiClock;
    use mc683xx_target::FunctionCode;
    use mc683xx_target::Size;

    use super::Drive;
    use super::PeripheralAccess;
    use super::Transfer;
    use crate::time::Moment;
    use crate::time::Time;
    use crate::time::TimeBase;

    /// A data cycle of `size`.
    const fn access(size: Size) -> PeripheralAccess {
        PeripheralAccess::new(ChipSelect::Boot, 0, size, FunctionCode::SupervisorData)
    }

    #[test]
    fn an_8_bit_peripheral_is_on_data_15_8_of_a_word() {
        assert_eq!(access(Size::Byte).written_byte(0xa55a), 0x5a);
        assert_eq!(access(Size::Word).written_byte(0xa55a), 0xa5);
        assert_eq!(access(Size::Byte).read_byte(0x42), 0x42);
        assert_eq!(access(Size::Word).read_byte(0x42), 0x4200);
    }

    #[test]
    fn a_drive_carries_the_system_clock() {
        let base = TimeBase::new(
            Cycle::ZERO,
            Time::from_picos(5),
            Frequency::from_hz(16_777_216),
        );
        let moment = Moment::new(Time::from_picos(5), base);
        assert_eq!(Drive::new(moment).moment(), moment);
        assert_eq!(Drive::new(moment).now(), Time::from_picos(5));
        let sampler = |_: usize, _: Time| Some(1.5);
        let drive = Drive::with_sampler(moment, &sampler);
        assert_eq!((drive.moment(), drive.sample(0)), (moment, Some(1.5)));
    }

    #[test]
    fn a_transfer_takes_another_clock_and_keeps_the_rest() {
        let reset = QspiClock::at_reset(Frequency::from_hz(8_388_608));
        let transfer = Transfer::new(0xa5, 8, reset)
            .through(1)
            .at_entry(2)
            .selecting(0xe)
            .continuing();
        assert_eq!(
            (transfer.attachment, transfer.entry, transfer.chip_selects),
            (1, 2, 0xe)
        );
        assert!(!transfer.starts_frame);
        let fast = QspiClock::new(
            ClockPolarity::IdleHigh,
            ClockPhase::CaptureLeading,
            Cycles::from_clocks(4),
            reset.system_clock,
        );
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
        for role in Role::ALL {
            assert_eq!(role.to_string().parse(), Ok(role));
        }
        assert_eq!("?".parse::<Role>(), Err(ParseRoleError("?".to_owned())));
    }

    #[test]
    fn lanes_print_as_the_board_file_names_them_and_parse_back() {
        for lane in Lane::ALL {
            assert_eq!(lane.to_string().parse(), Ok(lane));
        }
        assert_eq!("?".parse::<Lane>(), Err(ParseLaneError("?".to_owned())));
    }
}
