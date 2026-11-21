//! The MC68331 as a board's [`Chip`](crate::chip::Chip): its pins, its `[chip]` keys and
//! its wiring through [`Builder`] (MC68331UM).
//!
//! The implementation is here, not in `mc683xx-mc68331`, because the
//! trait is the board's and the chip's crate stays below the board: the
//! gdb stub and hosts that wire a chip by hand build it without the
//! board stack.  Behind the default feature `mc68331`, so a board crate
//! for another part builds without this one.

use std::cell::RefCell;
use std::fmt;
use std::rc::Rc;
use std::str::FromStr;

use mc683xx_cpu32::bus::StopRequest;
use mc683xx_cpu32::cpu::Cpu;
use mc683xx_cpu32::cpu::Run;
use mc683xx_cpu32::cpu::control::CpuControl;
use mc683xx_cpu32::cpu::state::StateError;
use mc683xx_cpu32::cycles::Cycles;
use mc683xx_cpu32::frequency::Frequency;
use mc683xx_cpu32::interrupt::InterruptLevel;
use mc683xx_cpu32::interrupt::IrqPin;
use mc683xx_cpu32::peripheral::PeripheralId;
use mc683xx_mc68331::chip::Builder;
use mc683xx_mc68331::chip::Diversion;
use mc683xx_mc68331::chip::Mc68331;
use mc683xx_mc68331::chip::MemoryId;
use mc683xx_mc68331::chip::OutputHorizon;
use mc683xx_mc68331::chip::OutputStop;
use mc683xx_mc68331::chip::SyncRequest;
use mc683xx_mc68331::event::Event;
use mc683xx_mc68331::gpt::Input;
use mc683xx_mc68331::gpt::PortGpPin;
use mc683xx_mc68331::gpt::pwm::Channel;
use mc683xx_mc68331::port::OutputChange;
use mc683xx_mc68331::port::Port;
use mc683xx_modules::chip_select::ChipSelect;
use mc683xx_modules::peripheral::BusDecode;
use mc683xx_modules::pins::PortLevels;
use mc683xx_modules::sim::Configuration;
use mc683xx_modules::sim::ResetCause;
use mc683xx_modules::sim::chip_selects::Dsack;
use mc683xx_modules::sim::clock::ClockSource;
use mc683xx_modules::sim::reset::ResetScope;
use mc683xx_target::address::Address;
use serde::Deserialize;
use serde::Deserializer;
use serde::Serialize;
use serde::Serializer;
use serde::de::Error as _;

use crate::chip;
use crate::chip::BoardMapped;
use crate::chip::BoardSpi;
use crate::chip::ChipBuilder;
use crate::chip::ChipPin;
use crate::chip::MemoryPlace;
use crate::chip::MemoryPlan;
use crate::chip::Requests;
use crate::chip::sealed::Sealed;
use crate::error::LoadError;
use crate::signal::Direction;
use crate::signal::Kind;
use crate::signal::Serial;
use crate::signal::Signal;
use crate::time::Time;
use crate::time::TimeBase;

/// System clocks the chip's RESET output stays low: the `reset`
/// instruction's 512 (SIMRM 8.2.6), an internal reset's at least 512 of
/// the clock after it (MC68331UM 4.6.6).
const RESET_CLOCKS: u64 = 512;
/// Pins in an 8-bit port.
const PORT_PINS: u8 = 8;
/// Port C's pins, PC0 to PC6 (MC68331UM 4.8.1.4).
const PORT_C_PINS: u8 = 7;
/// PQS3, PCS0/SS: the first of the QSPI's peripheral chip selects,
/// PQS3 to PQS6 (MC68331UM Table 3-7).
const PCS0_PIN: u8 = 3;
/// PCS\[3:0\]'s pins.
const PCS_PINS: u8 = 4;

/// A pin of one of the MC68331's general-purpose ports, `P` and the
/// port and its bit: `PE0`, `PGP7`, `PC6`.  Built by [`PortPin::new`],
/// so its bit is always one the port has.  Ordered port E, F, QS, GP,
/// then C, each by bit (see the `Ord` impl).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PortPin {
    /// The port.
    port: Port,
    /// Its bit: 0 to 7, port C's 0 to 6.
    bit: u8,
}

impl PortPin {
    /// Bit `bit` of `port`, if the port has it: 0 to 7, port C's 0 to 6
    /// (MC68331UM 4.8.1.4).
    #[must_use]
    pub const fn new(port: Port, bit: u8) -> Option<Self> {
        if bit < port_pins(port) {
            Some(Self { port, bit })
        } else {
            None
        }
    }

    /// The port.
    #[must_use]
    pub const fn port(self) -> Port {
        self.port
    }

    /// Its bit.
    #[must_use]
    pub const fn bit(self) -> u8 {
        self.bit
    }

    /// Its name: `PE0`.
    const fn name(self) -> &'static str {
        let names: &[&str] = match self.port {
            Port::C => &PC,
            Port::E => &PE,
            Port::F => &PF,
            Port::Gp => &PGP,
            Port::Qs => &PQS,
        };
        names[self.bit as usize]
    }

    /// Every pin of `port`, by bit.
    fn all(port: Port) -> impl Iterator<Item = Self> {
        (0..port_pins(port)).map(move |bit| Self { port, bit })
    }
}

/// The order the board visits pins in where several act at one time,
/// power-on's first levels among them: port E, F, QS, GP, then C, each
/// by bit, as the pins' variants ran before they were keyed by
/// [`Port`].  Changing it would change which of two same-time edges a
/// peripheral sees first, so it is kept, not derived from `Port`'s.
impl Ord for PortPin {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        const fn rank(port: Port) -> u8 {
            match port {
                Port::E => 0,
                Port::F => 1,
                Port::Qs => 2,
                Port::Gp => 3,
                Port::C => 4,
            }
        }
        (rank(self.port), self.bit).cmp(&(rank(other.port), other.bit))
    }
}

impl PartialOrd for PortPin {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// Its name: `PE0`.
impl fmt::Display for PortPin {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.name())
    }
}

/// The port pin its name, as it displays, names: `PE0`.
impl FromStr for PortPin {
    type Err = ParsePortPinError;

    fn from_str(name: &str) -> Result<Self, ParsePortPinError> {
        Port::ALL
            .into_iter()
            .flat_map(Self::all)
            .find(|pin| pin.name() == name)
            .ok_or_else(|| ParsePortPinError(name.to_owned()))
    }
}

/// Text that names no MC68331 port pin.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("no MC68331 port pin {0:?}")]
pub struct ParsePortPinError(String);

/// The pins `port` has.
const fn port_pins(port: Port) -> u8 {
    match port {
        Port::C => PORT_C_PINS,
        Port::E | Port::F | Port::Gp | Port::Qs => PORT_PINS,
    }
}

/// An MC68331 pin a net reaches.
///
/// A snapshot holds it by its name, so a pin added or reordered here
/// leaves the board's format as it was.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum Pin {
    /// A pin of a general-purpose port: on port GP, IC1 to IC3 on PGP0 to
    /// PGP2, OC1 to OC4 on PGP3 to PGP6, IC4/OC5 on PGP7 (MC68331UM
    /// Figure 7-1); on port C, a chip-select pin as a discrete output.
    Port(PortPin),
    /// PAI, the pulse accumulator's input (GPTRM 4).
    Pai,
    /// PCLK, the GPT's external clock (GPTRM 2.7).
    Pclk,
    /// An interrupt request pin, asserted low.
    Irq(IrqPin),
    /// RESET, asserted low.
    Reset,
    /// PWMA.
    PwmA,
    /// PWMB.
    PwmB,
    /// ECLK, the M6800 bus clock: the system clock / 8 or / 16 by
    /// SYNCR EDIV (MC68331UM 4.3.3), where CSPAR1 assigns the pin.
    Eclk,
    /// TXD, the SCI's transmitter.
    Txd,
    /// RXD, the SCI's receiver.
    Rxd,
}

/// Port E's pins' names, by bit.
const PE: [&str; 8] = ["PE0", "PE1", "PE2", "PE3", "PE4", "PE5", "PE6", "PE7"];
/// Port F's.
const PF: [&str; 8] = ["PF0", "PF1", "PF2", "PF3", "PF4", "PF5", "PF6", "PF7"];
/// Port QS's.
const PQS: [&str; 8] = [
    "PQS0", "PQS1", "PQS2", "PQS3", "PQS4", "PQS5", "PQS6", "PQS7",
];
/// Port GP's.
const PGP: [&str; 8] = [
    "PGP0", "PGP1", "PGP2", "PGP3", "PGP4", "PGP5", "PGP6", "PGP7",
];
/// Port C's.
const PC: [&str; 7] = ["PC0", "PC1", "PC2", "PC3", "PC4", "PC5", "PC6"];
/// The IRQ pins', by level less one.
const IRQ: [&str; 7] = ["IRQ1", "IRQ2", "IRQ3", "IRQ4", "IRQ5", "IRQ6", "IRQ7"];

/// Text that names no MC68331 pin.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("no MC68331 pin {0:?}")]
pub struct ParsePinError(String);

/// The pin named so, after `chip.`, exactly as `Display` writes it.
impl FromStr for Pin {
    type Err = ParsePinError;

    fn from_str(name: &str) -> Result<Self, ParsePinError> {
        Self::all()
            .find(|pin| pin.name() == name)
            .ok_or_else(|| ParsePinError(name.to_owned()))
    }
}

/// Its name.
impl Serialize for Pin {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.name())
    }
}

/// The pin a snapshot names, refused if there is none.
impl<'de> Deserialize<'de> for Pin {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let name = String::deserialize(deserializer)?;
        name.parse().map_err(D::Error::custom)
    }
}

/// Its name after `chip.`: `PE0`, `IRQ7`, `RESET`.
impl fmt::Display for Pin {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.name())
    }
}

impl Pin {
    /// Every pin.
    fn all() -> impl Iterator<Item = Self> {
        Port::ALL
            .into_iter()
            .flat_map(PortPin::all)
            .map(Self::Port)
            .chain(
                (1..=InterruptLevel::NONMASKABLE.get())
                    .filter_map(|level| IrqPin::new(level).map(Self::Irq)),
            )
            .chain([
                Self::Pai,
                Self::Pclk,
                Self::Reset,
                Self::PwmA,
                Self::PwmB,
                Self::Eclk,
                Self::Txd,
                Self::Rxd,
            ])
    }

    /// The port pin it is, if it is one.
    const fn port(self) -> Option<PortPin> {
        match self {
            Self::Port(pin) => Some(pin),
            _ => None,
        }
    }

    /// The GPT input it is, if it is one.
    fn gpt_input(self) -> Option<Input> {
        match self {
            Self::Port(PortPin {
                port: Port::Gp,
                bit,
            }) => PortGpPin::new(bit).map(Input::PortGp),
            Self::Pai => Some(Input::Pai),
            Self::Pclk => Some(Input::Pclk),
            _ => None,
        }
    }
}

impl Sealed for Pin {}

impl ChipPin for Pin {
    fn name(self) -> &'static str {
        match self {
            Self::Port(pin) => pin.name(),
            Self::Irq(pin) => IRQ[usize::from(pin.get() - 1)],
            Self::Pai => "PAI",
            Self::Pclk => "PCLK",
            Self::Reset => "RESET",
            Self::PwmA => "PWMA",
            Self::PwmB => "PWMB",
            Self::Eclk => "ECLK",
            Self::Txd => "TXD",
            Self::Rxd => "RXD",
        }
    }

    fn kind(self) -> Kind {
        match self {
            Self::Txd | Self::Rxd => Kind::Serial,
            Self::Eclk => Kind::Clock,
            _ => Kind::Logic,
        }
    }

    fn direction(self) -> Direction {
        match self {
            Self::Port(PortPin { port: Port::C, .. })
            | Self::PwmA
            | Self::PwmB
            | Self::Eclk
            | Self::Txd => Direction::Out,
            Self::Port(_) | Self::Reset => Direction::InOut,
            Self::Pai | Self::Pclk | Self::Irq(_) | Self::Rxd => Direction::In,
        }
    }

    fn scheduled(self) -> bool {
        self.gpt_input().is_some()
    }

    fn pcs(self) -> bool {
        matches!(
            self,
            Self::Port(PortPin { port: Port::Qs, bit })
                if (PCS0_PIN..PCS0_PIN + PCS_PINS).contains(&bit)
        )
    }
}

/// The levels the board drives on the chip's input pins out of
/// power-on, `[chip.pins]`.
#[derive(Debug, Default, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Pins {
    /// Ignored.
    #[serde(rename = "note")]
    _note: Option<String>,
    /// Port E.
    e: Option<u8>,
    /// Port F.
    f: Option<u8>,
    /// Port QS.
    qs: Option<u8>,
    /// Port GP.
    gp: Option<u8>,
    /// PAI.
    pai: Option<bool>,
    /// PCLK.
    pclk: Option<bool>,
}

/// The MC68331's `[chip]` table.
#[derive(Debug, Default, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Section {
    /// `type`, the board's.
    #[serde(rename = "type")]
    _type: Option<String>,
    /// Ignored.
    #[serde(rename = "note")]
    _note: Option<String>,
    /// DATA\[15:0\] at reset (MC68331UM 4.6.3).
    data: Option<u16>,
    /// MODCLK high: the synthesizer's reference (MC68331UM 4.3.1).
    reference_hz: Option<u64>,
    /// MODCLK low: the system clock.
    external_hz: Option<u64>,
    /// IPLs the board autovectors (SIMRM 6.4).
    #[serde(default)]
    autovector: Vec<u8>,
    /// Input pin levels at power-on.
    #[serde(default)]
    pins: Pins,
}

/// The MC68331's handles behind the board's [`Requests`].
#[derive(Debug, Default)]
struct Handles {
    /// The chip's sync request.
    sync: SyncRequest,
    /// Its memory diversions.
    diversion: Diversion,
    /// When a watched output may next change.
    horizon: OutputHorizon,
    /// Each board memory and the router its cycles go to while diverted,
    /// in the order built.
    memories: RefCell<Vec<(MemoryId, PeripheralId)>>,
}

impl Requests for Handles {
    fn sync(&self) {
        self.sync.request();
    }

    fn divert(&self, memory: usize) {
        if let Some(&(id, router)) = self.memories.borrow().get(memory) {
            self.diversion.divert(id, router);
        }
    }

    fn release(&self, memory: usize) {
        if let Some(&(id, _)) = self.memories.borrow().get(memory) {
            self.diversion.release(id);
        }
    }

    fn output_horizon(&self) -> Option<Cycles> {
        self.horizon.get()
    }
}

/// The MC68331 being wired for a board.
///
/// Not part of the semver API: plumbing the board builds the chip with.
#[doc(hidden)]
pub struct Mc68331Builder {
    /// The chip's builder.
    builder: Builder,
    /// Its handles, shared with the board.
    handles: Rc<Handles>,
    /// The levels the board drives out of power-on.
    pins: Pins,
}

impl fmt::Debug for Mc68331Builder {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Mc68331Builder")
            .field("builder", &self.builder)
            .finish_non_exhaustive()
    }
}

/// `error`, the chip's, as a load's.
fn built(error: mc683xx_mc68331::chip::BuildError) -> LoadError {
    LoadError::Build(Box::new(error))
}

impl Sealed for Mc68331Builder {}

impl ChipBuilder for Mc68331Builder {
    type Chip = Mc68331;

    fn system_frequency(&self) -> Frequency {
        self.builder.system_frequency()
    }

    fn stop_request(&self) -> StopRequest {
        self.builder.stop_request()
    }

    fn requests(&self) -> Rc<dyn Requests> {
        Rc::clone(&self.handles) as Rc<dyn Requests>
    }

    fn memory<R: BoardMapped>(
        &mut self,
        plan: MemoryPlan<'_>,
        router: R,
    ) -> Result<MemoryId, LoadError> {
        let id = match plan.place {
            MemoryPlace::Fixed {
                base,
                port,
                termination,
            } => self
                .builder
                .fixed_memory(Address(base), plan.bytes, plan.kind)
                .and_then(|id| {
                    self.builder
                        .set_fixed_timing(id, port, termination)
                        .map(|()| id)
                }),
            MemoryPlace::Selects(pins) => self.builder.memory(plan.bytes, plan.kind, pins),
        }
        .map_err(built)?;
        self.builder.set_reload_scope(id, plan.reload);
        let router = self.builder.unwired_peripheral(router);
        self.handles.memories.borrow_mut().push((id, router));
        Ok(id)
    }

    fn peripheral<R: BoardMapped>(
        &mut self,
        router: R,
        select: ChipSelect,
        dsack: Option<Dsack>,
    ) -> Result<(), LoadError> {
        let id = self.builder.peripheral(router, &[select]).map_err(built)?;
        if let Some(pins) = dsack {
            self.builder.set_dsack(id, pins);
        }
        Ok(())
    }

    fn bus_peripheral<R: BoardMapped>(
        &mut self,
        router: R,
        decodes: &[BusDecode],
        pins: Dsack,
    ) -> Result<(), LoadError> {
        self.builder
            .bus_peripheral(router, decodes, pins)
            .map(|_| ())
            .map_err(built)
    }

    fn unwired<R: BoardMapped>(&mut self, pins: R, reset: ResetScope, irq: Option<IrqPin>) {
        let id = self.builder.unwired_peripheral(pins);
        self.builder.set_reset_scope(id, reset);
        if let Some(level) = irq {
            self.builder.set_interrupt(id, level);
        }
    }

    fn qspi<R: BoardSpi>(&mut self, router: R) -> Result<(), LoadError> {
        self.builder.qspi(router);
        Ok(())
    }

    fn build(self) -> Mc68331 {
        let mut chip = self.builder.build();
        chip.set_clock_stop(true);
        let pins = &self.pins;
        for (port, levels) in [
            (Port::E, pins.e),
            (Port::F, pins.f),
            (Port::Qs, pins.qs),
            (Port::Gp, pins.gp),
        ] {
            if let Some(levels) = levels {
                chip.set_pins(port, levels, u8::MAX).expect("an input port");
            }
        }
        if let Some(pai) = pins.pai {
            chip.set_gpt_input(Input::Pai, pai);
        }
        if let Some(pclk) = pins.pclk {
            chip.set_gpt_input(Input::Pclk, pclk);
        }
        chip
    }
}

/// Each pin of `port`, at its level in `outputs`, undriven pins high.
fn emit_port(emit: &mut impl FnMut(Time, Pin, Signal), at: Time, port: Port, outputs: PortLevels) {
    let levels = outputs.pin_levels(u8::MAX);
    for pin in PortPin::all(port) {
        emit(
            at,
            Pin::Port(pin),
            Signal::Level(levels & 1 << pin.bit != 0),
        );
    }
}

impl Sealed for Mc68331 {}

impl chip::Chip for Mc68331 {
    const TYPE: &'static str = "mc68331";
    const RESET: Pin = Pin::Reset;

    type Section = Section;
    type Pin = Pin;
    type Event = Event;
    type Memory = MemoryId;
    type Builder = Mc68331Builder;

    fn builder(section: &Section) -> Result<Mc68331Builder, LoadError> {
        let clock = match (section.reference_hz, section.external_hz) {
            (Some(_), Some(_)) => return Err(LoadError::Clock),
            (_, Some(hz)) => ClockSource::external(Frequency::from_hz(hz)),
            (Some(hz), None) => ClockSource::synthesizer(Frequency::from_hz(hz)),
            (None, None) => Ok(ClockSource::default()),
        }
        .map_err(|error| LoadError::Frequency { error })?;
        let configuration = Configuration::new(section.data.unwrap_or(u16::MAX), clock);
        let mut builder = Builder::new(configuration);
        for &level in &section.autovector {
            let pin = IrqPin::new(level).ok_or_else(|| LoadError::Level {
                owner: "chip".to_owned(),
                level,
            })?;
            builder.set_autovector(pin);
        }
        let handles = Rc::new(Handles {
            sync: builder.sync_request(),
            diversion: builder.diversion(),
            horizon: builder.output_horizon(),
            memories: RefCell::default(),
        });
        Ok(Mc68331Builder {
            builder,
            handles,
            pins: section.pins.clone(),
        })
    }

    fn now(&self) -> Cycles {
        Self::now(self)
    }

    fn run_for(&mut self, budget: Cycles) -> Run {
        Self::run_for(self, budget)
    }

    fn reset_for(&mut self, cause: ResetCause) {
        Self::reset_for(self, cause);
    }

    fn reset_with_breakpoint(&mut self) {
        Self::reset_with_breakpoint(self);
    }

    fn assert_breakpoint(&mut self) -> bool {
        Self::assert_breakpoint(self)
    }

    fn apply_diversions(&mut self) {
        // a run makes the diversions asked between runs as it starts,
        // and one of no budget does nothing else
        Self::run_for(self, Cycles::ZERO);
    }

    fn cpu(&self) -> &Cpu {
        Self::cpu(self)
    }

    fn cpu_control(&mut self) -> CpuControl<'_> {
        Self::cpu_control(self)
    }

    fn instructions(&self) -> u64 {
        Self::instructions(self)
    }

    fn system_frequency(&self) -> Frequency {
        self.sim().clock().frequency()
    }

    fn memory(&self, memory: MemoryId) -> &[u8] {
        Self::memory(self, memory)
    }

    fn memory_mut(&mut self, memory: MemoryId) -> &mut [u8] {
        Self::memory_mut(self, memory)
    }

    fn watch_outputs(&mut self, on_nets: &[Pin]) {
        for (pin, channel) in [(Pin::PwmA, Channel::A), (Pin::PwmB, Channel::B)] {
            if on_nets.contains(&pin) {
                self.set_report_pwm_edges(channel, true);
            }
        }
    }

    fn stop_at_outputs(&mut self, sensed: &[Pin]) {
        let mut stop = OutputStop::default();
        for &pin in sensed {
            if let Some(pin) = pin.port() {
                stop.set_port(pin.port, stop.port(pin.port) | 1 << pin.bit);
                continue;
            }
            match pin {
                Pin::PwmA => stop.pwm_a = true,
                Pin::PwmB => stop.pwm_b = true,
                Pin::Txd => stop.txd = true,
                Pin::Reset => stop.reset = true,
                // ECLK changes with the clock, which ends the run itself
                // (`set_clock_stop`); the rest are inputs, or ports' pins
                // watched above
                Pin::Eclk | Pin::Pai | Pin::Pclk | Pin::Irq(_) | Pin::Rxd | Pin::Port(_) => {}
            }
        }
        self.set_output_stop(stop);
    }

    fn set_publish_output_horizon(&mut self, publish: bool) {
        Self::set_publish_output_horizon(self, publish);
    }

    fn input(&mut self, pin: Pin, signal: Signal, at: Cycles) {
        match (pin, signal) {
            (_, Signal::Level(level)) if pin.gpt_input().is_some() => {
                let input = pin.gpt_input().expect("a GPT input");
                self.schedule_gpt_input(at, input, level);
            }
            (
                Pin::Port(PortPin {
                    port: port @ (Port::E | Port::F | Port::Qs),
                    bit,
                }),
                Signal::Level(level),
            ) => {
                self.set_pins(port, if level { u8::MAX } else { 0 }, 1 << bit)
                    .expect("an input port");
            }
            (Pin::Irq(irq), Signal::Level(level)) => self.set_interrupt_pin(irq, !level),
            (Pin::Rxd, Signal::Serial(serial)) => self.sci_receive(serial.frame),
            _ => {}
        }
    }

    fn unschedule_inputs(&mut self) {
        self.unschedule_gpt_inputs();
    }

    fn clock_output(&self, pin: Pin) -> Option<Frequency> {
        (pin == Pin::Eclk).then(|| {
            let clock = self.sim().clock();
            Frequency::from_nanohertz(clock.frequency().nanohertz() / clock.eclk_divisor())
        })
    }

    fn collect<F: FnMut(Time, Pin, Signal)>(&mut self, base: TimeBase, mut emit: F) {
        // one stream, so a port GP pin and a PWM pin the firmware changes
        // in one instruction reach the nets in that order
        for change in self.take_output_changes() {
            match change {
                OutputChange::Port(change) => emit_port(
                    &mut emit,
                    base.time(change.now),
                    change.port,
                    change.outputs,
                ),
                OutputChange::Pwm(change) => {
                    let pin = if change.channel == Channel::A {
                        Pin::PwmA
                    } else {
                        Pin::PwmB
                    };
                    emit(base.time(change.now), pin, Signal::Level(change.level));
                }
                _ => {}
            }
        }
        let system_clock = self.sim().clock().frequency();
        for sent in self.take_sci_transmitted() {
            let start = base.time(sent.start);
            let baud = u128::from(system_clock.nanohertz()) / u128::from(sent.bit.0);
            let serial = Serial::timed(
                sent.frame,
                Frequency::from_nanohertz(u64::try_from(baud).unwrap_or(u64::MAX)),
                base.time(sent.end).since(start),
            );
            emit(start, Pin::Txd, Signal::Serial(serial));
        }
    }

    fn take_events(&mut self) -> Vec<Event> {
        Self::take_events(self)
    }

    fn reset_pulse(event: &Event) -> Option<(Cycles, Cycles)> {
        match *event {
            Event::Reset { now, .. } => Some((now, now + Cycles(RESET_CLOCKS))),
            _ => None,
        }
    }

    fn save(&self) -> Result<Vec<u8>, StateError> {
        Self::save(self)
    }

    fn restore(&mut self, snapshot: &[u8]) -> Result<(), StateError> {
        Self::restore(self, snapshot)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn pins_keep_their_order_port_e_f_qs_gp_then_c() {
        let pin = |port, bit| PortPin::new(port, bit).unwrap();
        let mut pins = [
            pin(Port::C, 0),
            pin(Port::Gp, 0),
            pin(Port::Qs, 0),
            pin(Port::F, 0),
            pin(Port::E, 7),
            pin(Port::E, 0),
        ];
        pins.sort();
        let names: Vec<String> = pins.iter().map(ToString::to_string).collect();
        assert_eq!(names, ["PE0", "PE7", "PF0", "PQS0", "PGP0", "PC0"]);
    }

    use mc683xx_cpu32::cpu::state::StateError;
    use mc683xx_cpu32::cpu::state::decode;
    use mc683xx_cpu32::cpu::state::encode;
    use mc683xx_cpu32::interrupt::IrqPin;
    use mc683xx_mc68331::port::Port;

    use super::PORT_C_PINS;
    use super::PORT_PINS;
    use super::ParsePortPinError;
    use super::Pin;
    use super::PortPin;
    use crate::chip::ChipPin;

    #[test]
    fn pins_show_as_they_parse() {
        let pins: Vec<Pin> = Pin::all().collect();
        assert_eq!(pins.len(), 4 * 8 + 7 + 7 + 8);
        for pin in pins {
            assert_eq!(pin.name().parse(), Ok(pin), "{pin}");
            assert_eq!(pin.to_string(), pin.name());
        }
        assert_eq!("PGP0".parse(), Ok(gp(0)));
        assert_eq!(
            "PC6".parse(),
            Ok(Pin::Port(PortPin::new(Port::C, 6).unwrap()))
        );
        assert_eq!("IRQ7".parse(), Ok(Pin::Irq(IrqPin::IRQ7)));
        for bad in ["IRQ0", "PC7", "PE8", "PE05", "NMI", ""] {
            assert!(bad.parse::<Pin>().is_err(), "{bad}");
        }
    }

    #[test]
    fn port_pins_show_as_they_parse() {
        for pin in Port::ALL.into_iter().flat_map(PortPin::all) {
            assert_eq!(pin.to_string().parse(), Ok(pin));
        }
        assert_eq!("PE0".parse(), Ok(PortPin::new(Port::E, 0).unwrap()));
        assert_eq!(
            "IRQ7".parse::<PortPin>(),
            Err(ParsePortPinError("IRQ7".to_owned()))
        );
    }

    #[test]
    fn a_snapshot_holds_a_pin_by_its_name() {
        for pin in Pin::all() {
            assert_eq!(decode::<Pin>(1, &encode(1, &pin)).unwrap(), pin);
        }
        assert_eq!(encode(1, &gp(3)), encode(1, &"PGP3"));
        assert!(matches!(
            decode::<Pin>(1, &encode(1, &"PE8")),
            Err(StateError::Decode(_))
        ));
    }

    #[test]
    fn a_port_pin_has_only_the_port_s_bits() {
        assert_eq!(PortPin::new(Port::C, PORT_C_PINS), None);
        assert_eq!(PortPin::new(Port::Qs, PORT_PINS), None);
        let pin = PortPin::new(Port::Gp, 7).unwrap();
        assert_eq!((pin.port(), pin.bit()), (Port::Gp, 7));
        assert_eq!(pin.to_string(), "PGP7");
    }

    /// PGP`bit`.
    fn gp(bit: u8) -> Pin {
        Pin::Port(PortPin::new(Port::Gp, bit).unwrap())
    }

    /// PE`bit`.
    fn pe(bit: u8) -> Pin {
        Pin::Port(PortPin::new(Port::E, bit).unwrap())
    }

    #[test]
    fn a_pin_carries_its_kind_and_faces_its_way() {
        use crate::signal::Direction;
        use crate::signal::Kind;

        assert_eq!(Pin::Txd.kind(), Kind::Serial);
        assert_eq!(Pin::Eclk.kind(), Kind::Clock);
        assert_eq!(pe(0).kind(), Kind::Logic);
        assert_eq!(Pin::Pai.direction(), Direction::In);
        assert_eq!(Pin::PwmA.direction(), Direction::Out);
        assert_eq!(Pin::Reset.direction(), Direction::InOut);
        assert!(gp(0).scheduled() && Pin::Pclk.scheduled());
        assert!(!pe(0).scheduled());
        assert_eq!(pe(0).direction(), Direction::InOut);
        assert_eq!(
            Pin::Port(PortPin::new(Port::C, 0).unwrap()).direction(),
            Direction::Out
        );
        assert!(Pin::Port(PortPin::new(Port::Qs, 3).unwrap()).pcs());
        assert!(!Pin::Port(PortPin::new(Port::Qs, 7).unwrap()).pcs());
    }
}
