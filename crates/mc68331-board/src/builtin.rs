//! The built-in peripheral types: small generic peripherals for tests and
//! examples, never a board's own peripherals ([PLAN.md, Boards][plan-boards]).
//!
//! [plan-boards]: https://github.com/charlieh0tel/mc683xx/blob/main/PLAN.md#boards

use std::mem;

use mc683xx_cpu32::bus::BusError;
use mc683xx_cpu32::cpu::state::StateError;
use mc683xx_cpu32::cpu::state::decode;
use mc683xx_cpu32::cpu::state::encode;
use mc683xx_cpu32::size::Size;
use serde::Deserialize;
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::description::ADDRESS_SPACE;
use crate::inspect::Field;
use crate::inspect::Inspection;
use crate::inspect::Value;
use crate::peripheral::Drive;
use crate::peripheral::Peripheral;
use crate::peripheral::PeripheralAccess;
use crate::peripheral::Role;
use crate::registry::Context;
use crate::registry::ParameterError;
use crate::registry::PortSpec;
use crate::registry::Registry;
use crate::signal::Kind;
use crate::signal::Signal;
use crate::time::Clock;
use crate::time::Time;
use mc683xx_cpu32::frequency::Frequency;

/// The format of the built-in peripherals' snapshots.
const VERSION: u32 = 1;

/// The unit of a frequency.
const HERTZ: &str = "Hz";

/// `value` as an inspection's integer, saturating.
fn saturating(value: impl TryInto<i64>) -> i64 {
    value.try_into().unwrap_or(i64::MAX)
}

/// `inspection` with `clock` named `name`: its frequency, and the time
/// it took effect.
fn with_clock(inspection: Inspection, name: &str, clock: Clock) -> Inspection {
    inspection.group(
        name,
        Inspection::default()
            .field(Field {
                unit: Some(HERTZ),
                ..Field::new(
                    "frequency",
                    Value::Integer(saturating(clock.frequency.hz())),
                )
            })
            .time("at", clock.at),
    )
}

/// The value a read of `size` carries of `value`: its low byte or word.
fn fit(value: u32, size: Size) -> u32 {
    match size {
        Size::Byte => value & 0xff,
        Size::Word => value & 0xffff,
        Size::Long => value,
    }
}

/// `state` as a built-in part's snapshot.
fn save<T: Serialize>(state: &T) -> Result<Vec<u8>, StateError> {
    Ok(encode(VERSION, state))
}

/// A built-in part's snapshot.
fn restore<T: DeserializeOwned>(snapshot: &[u8]) -> Result<T, StateError> {
    decode(VERSION, snapshot)
}

/// A built-in type's constructor.
type Builds = fn(&Context<'_>) -> Result<Box<dyn Peripheral>, ParameterError>;

/// Registers the built-in types in `registry`.
pub(crate) fn register(registry: &mut Registry) {
    let types: [(&str, Vec<PortSpec>, Builds); 6] = [
        ("fixed", Vec::new(), |context| {
            Ok(Box::new(Fixed::from_parameters(context)?))
        }),
        ("registers", Vec::new(), |context| {
            Ok(Box::new(RegisterFile::from_parameters(context)?))
        }),
        ("recorder", Vec::new(), |context| {
            Ok(Box::new(Recorder::from_parameters(context)?))
        }),
        (
            "clock",
            vec![PortSpec::output("out", Kind::Clock)],
            |context| Ok(Box::new(Oscillator::from_parameters(context)?)),
        ),
        (
            "divider",
            vec![
                PortSpec::input("in", Kind::Clock),
                PortSpec::output("out", Kind::Edge),
            ],
            |context| Ok(Box::new(Divider::from_parameters(context)?)),
        ),
        (
            "edge-counter",
            vec![PortSpec::input("in", Kind::Edge)],
            |_| Ok(Box::new(EdgeCounter::default())),
        ),
    ];
    for (name, ports, constructor) in types {
        registry
            .register(name, ports, constructor)
            .expect("the built-in names are distinct");
    }
}

/// `fixed`: reads `value`, ignores writes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Fixed {
    /// What every read gives, cut to the cycle's size.
    value: u32,
}

impl Fixed {
    /// One that reads `value`.
    #[must_use]
    pub const fn new(value: u32) -> Self {
        Self { value }
    }

    /// What every read gives, cut to the cycle's size.
    #[must_use]
    pub const fn value(&self) -> u32 {
        self.value
    }

    /// From `value`, 0 by default.
    fn from_parameters(context: &Context<'_>) -> Result<Self, ParameterError> {
        Ok(Self {
            value: context.parameters.integer("value")?.unwrap_or(0),
        })
    }
}

impl Peripheral for Fixed {
    fn read(&mut self, access: PeripheralAccess, _: &mut Drive<'_>) -> Result<u32, BusError> {
        Ok(fit(self.value, access.size))
    }

    fn write(&mut self, _: PeripheralAccess, _: u32, _: &mut Drive<'_>) -> Result<(), BusError> {
        Ok(())
    }

    /// Its value.
    fn inspect(&self) -> Inspection {
        Inspection::default().register("value", self.value)
    }

    fn save(&self) -> Result<Vec<u8>, StateError> {
        Ok(Vec::new())
    }

    fn restore(&mut self, _: &[u8]) -> Result<(), StateError> {
        Ok(())
    }
}

/// `registers`: a byte-addressed register file of `bytes` (a power of
/// two), repeating over the address it sees.
///
/// An 8-bit peripheral: a word cycle reaches one register, on DATA\[15:8\]
/// ([`PeripheralAccess::written_byte`]).  An IACK read gives `vector`, or
/// BERR without one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegisterFile {
    /// The registers: a power of two of them, at most ADDR\[23:0\]'s
    /// reach.
    bytes: Vec<u8>,
    /// The vector an acknowledge reads, if it answers one.
    vector: Option<u8>,
}

impl RegisterFile {
    /// A file of `length` registers, zero, answering an acknowledge
    /// with `vector`; [`None`] unless `length` is a power of two that
    /// ADDR\[23:0\] reach.
    #[must_use]
    pub fn new(length: usize, vector: Option<u8>) -> Option<Self> {
        (length.is_power_of_two() && length <= ADDRESS_SPACE).then(|| Self {
            bytes: vec![0; length],
            vector,
        })
    }

    /// The registers.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// The registers, to change: their number stays.
    pub fn bytes_mut(&mut self) -> &mut [u8] {
        &mut self.bytes
    }

    /// The vector an acknowledge reads, if it answers one.
    #[must_use]
    pub const fn vector(&self) -> Option<u8> {
        self.vector
    }

    /// From `bytes` (1 by default) and `vector`.
    fn from_parameters(context: &Context<'_>) -> Result<Self, ParameterError> {
        let length: u32 = context.parameters.integer("bytes")?.unwrap_or(1);
        if !length.is_power_of_two() {
            return Err(ParameterError::invalid(
                "bytes",
                format!("{length} is not a power of two"),
            ));
        }
        if length as usize > ADDRESS_SPACE {
            return Err(ParameterError::invalid(
                "bytes",
                format!("{length} is more than ADDR[23:0] reach"),
            ));
        }
        Ok(Self {
            bytes: vec![0; length as usize],
            vector: context.parameters.integer("vector")?,
        })
    }

    /// The register `address` reaches.
    fn index(&self, address: u32) -> usize {
        address as usize & (self.bytes.len() - 1)
    }
}

impl Peripheral for RegisterFile {
    fn read(&mut self, access: PeripheralAccess, _: &mut Drive<'_>) -> Result<u32, BusError> {
        if access.role == Role::Iack {
            return self.vector.map(u32::from).ok_or(BusError);
        }
        Ok(access.read_byte(self.bytes[self.index(access.address)]))
    }

    fn write(
        &mut self,
        access: PeripheralAccess,
        value: u32,
        _: &mut Drive<'_>,
    ) -> Result<(), BusError> {
        let at = self.index(access.address);
        self.bytes[at] = access.written_byte(value);
        Ok(())
    }

    /// Its registers and its vector, if it answers an acknowledge.
    fn inspect(&self) -> Inspection {
        let shown = Inspection::default().bytes("registers", self.bytes.as_slice());
        match self.vector {
            Some(vector) => shown.register("vector", vector),
            None => shown.text("vector", "none"),
        }
    }

    fn save(&self) -> Result<Vec<u8>, StateError> {
        save(self)
    }

    fn restore(&mut self, snapshot: &[u8]) -> Result<(), StateError> {
        let saved: Self = restore(snapshot)?;
        if saved.bytes.len() != self.bytes.len() {
            return Err(StateError::mismatch("a register file of another size"));
        }
        *self = saved;
        Ok(())
    }
}

/// An access a [`Recorder`] logged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Logged {
    /// The access.
    pub access: PeripheralAccess,
    /// A write's value; [`None`] for a read.
    pub written: Option<u32>,
}

/// `recorder`: reads `value`, accepts writes, and logs every access,
/// for tests and for a board's unmodeled selects.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Recorder {
    /// What every read gives, cut to the cycle's size.
    value: u32,
    /// Every access, in order.
    log: Vec<Logged>,
}

impl Recorder {
    /// One that reads `value`, its log empty.
    #[must_use]
    pub const fn new(value: u32) -> Self {
        Self {
            value,
            log: Vec::new(),
        }
    }

    /// What every read gives, cut to the cycle's size.
    #[must_use]
    pub const fn value(&self) -> u32 {
        self.value
    }

    /// Every access, in order.
    #[must_use]
    pub fn log(&self) -> &[Logged] {
        &self.log
    }

    /// The log, emptied.
    pub fn take_log(&mut self) -> Vec<Logged> {
        mem::take(&mut self.log)
    }

    /// From `value`, 0 by default.
    fn from_parameters(context: &Context<'_>) -> Result<Self, ParameterError> {
        Ok(Self {
            value: context.parameters.integer("value")?.unwrap_or(0),
            log: Vec::new(),
        })
    }
}

impl Peripheral for Recorder {
    fn read(&mut self, access: PeripheralAccess, _: &mut Drive<'_>) -> Result<u32, BusError> {
        self.log.push(Logged {
            access,
            written: None,
        });
        Ok(fit(self.value, access.size))
    }

    fn write(
        &mut self,
        access: PeripheralAccess,
        value: u32,
        _: &mut Drive<'_>,
    ) -> Result<(), BusError> {
        self.log.push(Logged {
            access,
            written: Some(value),
        });
        Ok(())
    }

    /// Its value and how many accesses it has logged.
    fn inspect(&self) -> Inspection {
        Inspection::default()
            .register("value", self.value)
            .count("accesses", self.log.len())
    }

    /// Its value: the log is the host's record of the run, not the
    /// peripheral's state, so a restore leaves it as it is.
    fn save(&self) -> Result<Vec<u8>, StateError> {
        save(&self.value)
    }

    fn restore(&mut self, snapshot: &[u8]) -> Result<(), StateError> {
        self.value = restore(snapshot)?;
        Ok(())
    }
}

/// `clock`: drives a steady clock of `hz` on `out`, a rising edge at
/// `phase_ps`, from power-on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Oscillator {
    /// The clock.
    clock: Clock,
    /// Whether it has been driven.
    driven: bool,
}

impl Oscillator {
    /// One that drives `clock` from power-on.
    #[must_use]
    pub const fn new(clock: Clock) -> Self {
        Self {
            clock,
            driven: false,
        }
    }

    /// The clock it drives.
    #[must_use]
    pub const fn clock(&self) -> Clock {
        self.clock
    }

    /// Its one port.
    const OUT: usize = 0;

    /// From `hz` and `phase_ps`.
    fn from_parameters(context: &Context<'_>) -> Result<Self, ParameterError> {
        let hz = context.parameters.required("hz")?;
        let phase = context.parameters.integer("phase_ps")?.unwrap_or(0);
        Ok(Self {
            clock: Clock::new(Frequency::from_hz(hz), Time(phase)),
            driven: false,
        })
    }
}

impl Peripheral for Oscillator {
    fn next_event(&self) -> Option<Time> {
        (!self.driven).then_some(Time::ZERO)
    }

    fn advance(&mut self, _: Time, drive: &mut Drive<'_>) {
        drive.drive(Self::OUT, Signal::Clock(self.clock));
        self.driven = true;
    }

    /// Its clock and whether it has driven it.
    fn inspect(&self) -> Inspection {
        with_clock(Inspection::default(), "clock", self.clock).boolean("driven", self.driven)
    }

    fn save(&self) -> Result<Vec<u8>, StateError> {
        save(self)
    }

    fn restore(&mut self, snapshot: &[u8]) -> Result<(), StateError> {
        *self = restore(snapshot)?;
        Ok(())
    }
}

/// An output change a [`Divider`] has driven ahead.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
struct Transition {
    /// The input edge it follows.
    edge: u128,
    /// Its time.
    at: Time,
}

/// `divider`: on `out`, a rising edge at every `divide`th rising edge
/// of the clock on `in` (those whose number is a multiple of
/// `divide`), falling `high` input cycles later.
///
/// It drives each change one change ahead, so a chip's timed input it feeds (a GPT capture) captures
/// exactly while the input's frequency holds.  A drive cannot be withdrawn:
/// a retune of the input leaves the change already driven at the old
/// clock's time, and the changes after it follow the new clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Divider {
    /// Input cycles in an output cycle.
    divide: u128,
    /// Input cycles the output is high.
    high: u128,
    /// The input clock, once driven.
    clock: Option<Clock>,
    /// The change driven ahead and not yet reached.
    pending: Option<Transition>,
}

impl Divider {
    /// Its input.
    const IN: usize = 0;
    /// Its output.
    const OUT: usize = 1;

    /// A divider by `divide`, high `high` input cycles, not yet driven.
    #[cfg(test)]
    pub(crate) const fn with_ratio(divide: u128, high: u128) -> Self {
        Self {
            divide,
            high,
            clock: None,
            pending: None,
        }
    }

    /// From `divide` (at least 2) and `high` (1 to `divide` - 1, half
    /// by default).
    fn from_parameters(context: &Context<'_>) -> Result<Self, ParameterError> {
        let divide: u64 = context.parameters.required("divide")?;
        if divide < 2 {
            return Err(ParameterError::invalid("divide", "it must be at least 2"));
        }
        let high: u64 = context.parameters.integer("high")?.unwrap_or(divide / 2);
        if high == 0 || high >= divide {
            return Err(ParameterError::invalid(
                "high",
                format!("it must be 1 to {}", divide - 1),
            ));
        }
        Ok(Self {
            divide: u128::from(divide),
            high: u128::from(high),
            clock: None,
            pending: None,
        })
    }

    /// The first output change at an input edge after `edge`.
    const fn after(&self, edge: u128) -> u128 {
        let start = edge - edge % self.divide;
        if start + self.high > edge {
            start + self.high
        } else {
            start + self.divide
        }
    }

    /// Drives the change after input edge `edge`, no earlier than
    /// `not_before`.
    fn drive_after(&mut self, edge: u128, not_before: Time, drive: &mut Drive<'_>) {
        let Some(clock) = self.clock else { return };
        let next = self.after(edge);
        self.pending = clock.edge(next).map(|at| {
            let at = at.max(not_before);
            let rising = next.is_multiple_of(self.divide);
            drive.drive_at(at, Self::OUT, Signal::Level(rising));
            Transition { edge: next, at }
        });
    }
}

impl Peripheral for Divider {
    fn input(&mut self, port: usize, signal: Signal, now: Time, drive: &mut Drive<'_>) {
        if let (Self::IN, Signal::Clock(clock)) = (port, signal) {
            self.clock = Some(clock);
            if self.pending.is_none() {
                self.drive_after(clock.phase_at(now).cycles(), now, drive);
            }
        }
    }

    fn next_event(&self) -> Option<Time> {
        self.pending.map(|pending| pending.at)
    }

    fn advance(&mut self, now: Time, drive: &mut Drive<'_>) {
        if let Some(pending) = self.pending {
            self.drive_after(pending.edge, now, drive);
        }
    }

    /// Its ratio, its input clock once driven, and the change driven
    /// ahead: the input edge it follows and its time.
    fn inspect(&self) -> Inspection {
        let shown = Inspection::default()
            .integer("divide", saturating(self.divide))
            .integer("high", saturating(self.high));
        let shown = match self.clock {
            Some(clock) => with_clock(shown, "clock", clock),
            None => shown.text("clock", "none"),
        };
        match self.pending {
            Some(pending) => shown.group(
                "pending",
                Inspection::default()
                    .integer("edge", saturating(pending.edge))
                    .time("at", pending.at),
            ),
            None => shown.text("pending", "none"),
        }
    }

    fn save(&self) -> Result<Vec<u8>, StateError> {
        save(self)
    }

    fn restore(&mut self, snapshot: &[u8]) -> Result<(), StateError> {
        let saved: Self = restore(snapshot)?;
        if (saved.divide, saved.high) != (self.divide, self.high) {
            return Err(StateError::mismatch("a divider of another ratio"));
        }
        *self = saved;
        Ok(())
    }
}

/// `edge-counter`: counts the rising edges on `in`, keeping the last
/// one's time.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EdgeCounter {
    /// The level on `in`, low before anything drives it.
    level: bool,
    /// Rising edges seen.
    count: u64,
    /// The last one's time.
    last: Option<Time>,
}

impl EdgeCounter {
    /// Rising edges seen.
    #[must_use]
    pub const fn count(&self) -> u64 {
        self.count
    }

    /// The last rising edge's time.
    #[must_use]
    pub const fn last(&self) -> Option<Time> {
        self.last
    }
}

impl Peripheral for EdgeCounter {
    fn input(&mut self, _: usize, signal: Signal, now: Time, _: &mut Drive<'_>) {
        if let Signal::Level(level) = signal {
            if level && !self.level {
                self.count += 1;
                self.last = Some(now);
            }
            self.level = level;
        }
    }

    /// Its input's level, its count and the last edge's time.
    fn inspect(&self) -> Inspection {
        let shown = Inspection::default()
            .boolean("level", self.level)
            .integer("count", saturating(self.count));
        match self.last {
            Some(last) => shown.time("last", last),
            None => shown.text("last", "none"),
        }
    }

    fn save(&self) -> Result<Vec<u8>, StateError> {
        save(self)
    }

    fn restore(&mut self, snapshot: &[u8]) -> Result<(), StateError> {
        *self = restore(snapshot)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use mc683xx_cpu32::cpu::state::StateError;
    use mc683xx_cpu32::function_code::FunctionCode;
    use mc683xx_cpu32::size::Size;
    use mc683xx_modules::chip_select::ChipSelect;

    use super::Divider;
    use super::EdgeCounter;
    use super::Fixed;
    use super::Oscillator;
    use super::Recorder;
    use super::RegisterFile;
    use super::VERSION;
    use crate::inspect::Inspection;
    use crate::inspect::Value;
    use crate::peripheral::Drive;
    use crate::peripheral::Peripheral;
    use crate::peripheral::PeripheralAccess;
    use crate::signal::Signal;
    use crate::time::Clock;
    use crate::time::Time;
    use mc683xx_cpu32::frequency::Frequency;

    #[test]
    fn a_divider_drives_each_change_one_ahead_at_its_exact_edge() {
        let mut divider = Divider {
            divide: 10,
            high: 3,
            clock: None,
            pending: None,
        };
        // 10 MHz with an edge at 37 ps: edges at 37 + 100000n.
        let clock = Clock::new(Frequency::from_hz(10_000_000), Time(37));
        let mut drive = Drive::new(Time(50));
        divider.input(0, Signal::Clock(clock), Time(50), &mut drive);
        let driven = drive.finish().driven;
        assert_eq!(driven.len(), 1);
        assert_eq!(driven[0].at, Time(37 + 300_000));
        assert_eq!(driven[0].signal, Signal::Level(false));
        let mut drive = Drive::new(Time(300_037));
        divider.advance(Time(300_037), &mut drive);
        let driven = drive.finish().driven;
        assert_eq!(driven[0].at, Time(1_000_037));
        assert_eq!(driven[0].signal, Signal::Level(true));
        assert_eq!(divider.next_event(), Some(Time(1_000_037)));
    }

    #[test]
    fn a_register_file_is_an_8_bit_peripheral() {
        let mut registers = RegisterFile {
            bytes: vec![0; 4],
            vector: None,
        };
        let access = |address, size| {
            PeripheralAccess::new(
                ChipSelect::Boot,
                address,
                size,
                FunctionCode::SUPERVISOR_DATA,
                Time::ZERO,
            )
        };
        let mut drive = Drive::new(Time::ZERO);
        registers
            .write(access(2, Size::Word), 0x1234, &mut drive)
            .unwrap();
        registers
            .write(access(1, Size::Byte), 0x56, &mut drive)
            .unwrap();
        assert_eq!(registers.bytes, [0, 0x56, 0x12, 0], "a word's upper byte");
        assert_eq!(
            registers.read(access(2, Size::Word), &mut drive),
            Ok(0x1200)
        );
        assert_eq!(registers.read(access(1, Size::Byte), &mut drive), Ok(0x56));
    }

    #[test]
    fn a_register_file_is_built_only_a_power_of_two_long() {
        assert_eq!(RegisterFile::new(3, None), None);
        assert_eq!(RegisterFile::new(0, None), None);
        assert_eq!(RegisterFile::new(super::ADDRESS_SPACE * 2, None), None);
        let mut registers = RegisterFile::new(4, Some(0x40)).unwrap();
        registers.bytes_mut()[1] = 0x56;
        assert_eq!(registers.bytes(), [0, 0x56, 0, 0]);
        assert_eq!(registers.vector(), Some(0x40));
        let mut recorder = Recorder::new(7);
        assert_eq!(recorder.value(), 7);
        assert!(recorder.take_log().is_empty());
    }

    #[test]
    fn a_snapshot_of_other_parameters_is_refused() {
        let mut registers = RegisterFile {
            bytes: vec![1, 2],
            vector: None,
        };
        let wider = RegisterFile {
            bytes: vec![0; 4],
            vector: None,
        };
        assert!(matches!(
            registers.restore(&wider.save().unwrap()),
            Err(StateError::Mismatch(reason)) if reason == "a register file of another size"
        ));
        assert_eq!(registers.bytes, [1, 2]);
        let divider = |divide, high| Divider {
            divide,
            high,
            clock: None,
            pending: None,
        };
        let mut tenth = divider(10, 3);
        for other in [divider(0, 3), divider(10, 5)] {
            assert!(matches!(
                tenth.restore(&other.save().unwrap()),
                Err(StateError::Mismatch(reason)) if reason == "a divider of another ratio"
            ));
        }
        assert_eq!(tenth, divider(10, 3));
    }

    #[test]
    fn a_retune_moves_a_dividers_changes_after_the_one_driven() {
        let mut divider = Divider {
            divide: 10,
            high: 3,
            clock: None,
            pending: None,
        };
        let clock = Clock::new(Frequency::from_hz(10_000_000), Time(37));
        divider.input(0, Signal::Clock(clock), Time(50), &mut Drive::new(Time(50)));
        let retuned = clock.retuned(Time(100_000), Frequency::from_hz(20_000_000));
        let mut drive = Drive::new(Time(100_000));
        divider.input(0, Signal::Clock(retuned), Time(100_000), &mut drive);
        assert!(drive.finish().driven.is_empty());
        assert_eq!(
            divider.next_event(),
            Some(Time(37 + 300_000)),
            "the fall driven ahead keeps the old clock's time"
        );
        let mut drive = Drive::new(Time(300_037));
        divider.advance(Time(300_037), &mut drive);
        let driven = drive.finish().driven;
        assert_eq!(driven[0].at, retuned.edge(10).unwrap());
        assert_eq!(driven[0].signal, Signal::Level(true));
    }

    #[test]
    fn a_recorder_saves_its_value_and_keeps_its_log() {
        let saved = Recorder {
            value: 0x0f,
            log: Vec::new(),
        }
        .save()
        .unwrap();
        let mut recorder = Recorder::default();
        recorder
            .write(
                PeripheralAccess::new(
                    ChipSelect::Boot,
                    0,
                    Size::Byte,
                    FunctionCode::SUPERVISOR_DATA,
                    Time(1),
                ),
                1,
                &mut Drive::new(Time(1)),
            )
            .unwrap();
        recorder.restore(&saved).unwrap();
        assert_eq!(recorder.value, 0x0f);
        assert_eq!(recorder.log.len(), 1);
    }

    #[test]
    fn each_built_in_type_shows_its_state() {
        assert_eq!(
            Fixed::new(0x1234).inspect().get("value"),
            Some(&Value::Integer(0x1234))
        );
        let registers = RegisterFile::new(2, Some(0x40)).unwrap().inspect();
        assert_eq!(registers.get("registers"), Some(&Value::Bytes(vec![0, 0])));
        assert_eq!(registers.get("vector"), Some(&Value::Integer(0x40)));
        assert_eq!(
            RegisterFile::new(1, None).unwrap().inspect().get("vector"),
            Some(&Value::Text("none".to_owned()))
        );
        let mut recorder = Recorder::new(7);
        recorder
            .write(
                PeripheralAccess::new(
                    ChipSelect::Boot,
                    0,
                    Size::Byte,
                    FunctionCode::SUPERVISOR_DATA,
                    Time(1),
                ),
                1,
                &mut Drive::new(Time(1)),
            )
            .unwrap();
        let recorded = recorder.inspect();
        assert_eq!(recorded.get("value"), Some(&Value::Integer(7)));
        assert_eq!(recorded.get("accesses"), Some(&Value::Integer(1)));
        let clock = Clock::new(Frequency::from_hz(10_000_000), Time(37));
        let shows_clock = |inspection: &Inspection| {
            let Some(Value::Group(clock)) = inspection.get("clock") else {
                panic!("a clock");
            };
            assert_eq!(clock.get("frequency"), Some(&Value::Integer(10_000_000)));
            assert_eq!(clock.fields[0].unit, Some("Hz"));
            assert_eq!(clock.get("at"), Some(&Value::Integer(37)));
        };
        let oscillator = Oscillator::new(clock).inspect();
        shows_clock(&oscillator);
        assert_eq!(oscillator.get("driven"), Some(&Value::Boolean(false)));
        let mut divider = Divider::with_ratio(10, 3);
        assert_eq!(
            divider.inspect().get("pending"),
            Some(&Value::Text("none".to_owned()))
        );
        divider.input(0, Signal::Clock(clock), Time(50), &mut Drive::new(Time(50)));
        let divided = divider.inspect();
        assert_eq!(divided.get("divide"), Some(&Value::Integer(10)));
        assert_eq!(divided.get("high"), Some(&Value::Integer(3)));
        shows_clock(&divided);
        let Some(Value::Group(pending)) = divided.get("pending") else {
            panic!("a pending change");
        };
        assert_eq!(pending.get("edge"), Some(&Value::Integer(3)));
        assert_eq!(pending.get("at"), Some(&Value::Integer(37 + 300_000)));
        let mut counter = EdgeCounter::default();
        assert_eq!(
            counter.inspect().get("last"),
            Some(&Value::Text("none".to_owned()))
        );
        counter.input(0, Signal::Level(true), Time(10), &mut Drive::new(Time(10)));
        let counted = counter.inspect();
        assert_eq!(counted.get("level"), Some(&Value::Boolean(true)));
        assert_eq!(counted.get("count"), Some(&Value::Integer(1)));
        assert_eq!(counted.get("last"), Some(&Value::Integer(10)));
    }

    #[test]
    fn the_format_matches_its_golden() {
        let clock = Clock::new(Frequency::from_hz(10_000_000), Time(37));
        let mut divider = Divider {
            divide: 10,
            high: 3,
            clock: None,
            pending: None,
        };
        divider.input(0, Signal::Clock(clock), Time(50), &mut Drive::new(Time(50)));
        let mut counter = EdgeCounter::default();
        for (level, at) in [(true, 10), (false, 20), (true, 30)] {
            counter.input(0, Signal::Level(level), Time(at), &mut Drive::new(Time(at)));
        }
        let peripherals: [&dyn Peripheral; 5] = [
            &RegisterFile {
                bytes: vec![1, 2, 3, 4],
                vector: Some(0x40),
            },
            &Recorder {
                value: 0x0f,
                log: Vec::new(),
            },
            &Oscillator {
                clock,
                driven: true,
            },
            &divider,
            &counter,
        ];
        let snapshot: Vec<u8> = peripherals
            .iter()
            .flat_map(|peripheral| peripheral.save().unwrap())
            .collect();
        snapshot_golden::check(
            env!("CARGO_MANIFEST_DIR"),
            "builtin",
            &[("builtin", VERSION)],
            &snapshot,
        );
    }
}
