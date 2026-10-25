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

use mc683xx_cpu32::address::Address;
use mc683xx_cpu32::bus::StopRequest;
use mc683xx_cpu32::cpu::Cpu;
use mc683xx_cpu32::cpu::Halt;
use mc683xx_cpu32::cpu::Run;
use mc683xx_cpu32::cpu::control::CpuControl;
use mc683xx_cpu32::cpu::state::StateError;
use mc683xx_cpu32::cycles::Cycles;
use mc683xx_cpu32::frequency::Frequency;
use mc683xx_cpu32::interrupt::IrqPin;
use mc683xx_cpu32::peripheral::PeripheralId;
use mc683xx_mc68331::chip::Builder;
use mc683xx_mc68331::chip::Diversion;
use mc683xx_mc68331::chip::Mc68331;
use mc683xx_mc68331::chip::MemoryId;
use mc683xx_mc68331::chip::OutputStop;
use mc683xx_mc68331::chip::SyncRequest;
use mc683xx_mc68331::event::Event;
use mc683xx_mc68331::gpt::Input;
use mc683xx_mc68331::gpt::OutputChange;
use mc683xx_mc68331::gpt::PortGpPin;
use mc683xx_mc68331::gpt::pwm::Channel;
use mc683xx_modules::chip_select::ChipSelect;
use mc683xx_modules::pins::PortLevels;
use mc683xx_modules::sim::Configuration;
use mc683xx_modules::sim::chip_selects::Dsack;
use mc683xx_modules::sim::clock::ClockSource;
use mc683xx_modules::sim::ports::PortName;
use mc683xx_modules::sim::reset::ResetScope;
use serde::Deserialize;
use serde::Serialize;

use crate::chip;
use crate::chip::BoardMapped;
use crate::chip::BoardSpi;
use crate::chip::ChipBuilder;
use crate::chip::ChipPin;
use crate::chip::MemoryPlace;
use crate::chip::MemoryPlan;
use crate::chip::Requests;
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

/// An MC68331 pin a net reaches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[non_exhaustive]
pub enum Pin {
    /// PE`n`, a pin of port E.
    PortE(u8),
    /// PF`n`.
    PortF(u8),
    /// PQS`n`.
    PortQs(u8),
    /// PGP`n`: IC1 to IC3 on PGP0 to PGP2, OC1 to OC4 on PGP3 to PGP6,
    /// IC4/OC5 on PGP7 (MC68331UM Figure 7-1).
    PortGp(u8),
    /// PC`n`: a chip-select pin as a discrete output.
    PortC(u8),
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
pub struct ParsePinError(pub String);

/// The pin named so, after `chip.`.
impl FromStr for Pin {
    type Err = ParsePinError;

    fn from_str(name: &str) -> Result<Self, ParsePinError> {
        Self::parse(name).ok_or_else(|| ParsePinError(name.to_owned()))
    }
}

/// Its name after `chip.`: `PE0`, `IRQ7`, `RESET`.
impl fmt::Display for Pin {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.name())
    }
}

impl Pin {
    /// The pin named `name`, after `chip.`.
    fn parse(name: &str) -> Option<Self> {
        let numbered = |prefix: &str, pins: u8| {
            name.strip_prefix(prefix)
                .and_then(|number| number.parse::<u8>().ok())
                .filter(|&number| number < pins)
        };
        if let Ok(input) = name.parse::<Input>() {
            return Some(match input {
                Input::PortGp(pin) => Self::PortGp(pin.get()),
                Input::Pai => Self::Pai,
                Input::Pclk => Self::Pclk,
            });
        }
        let pin = match name {
            "RESET" => Self::Reset,
            "PWMA" => Self::PwmA,
            "PWMB" => Self::PwmB,
            "ECLK" => Self::Eclk,
            "TXD" => Self::Txd,
            "RXD" => Self::Rxd,
            _ => {
                if let Some(number) = numbered("PQS", PORT_PINS) {
                    Self::PortQs(number)
                } else if let Some(number) = numbered("PE", PORT_PINS) {
                    Self::PortE(number)
                } else if let Some(number) = numbered("PF", PORT_PINS) {
                    Self::PortF(number)
                } else if let Some(number) = numbered("PC", PORT_C_PINS) {
                    Self::PortC(number)
                } else {
                    Self::Irq(IrqPin::new(numbered("IRQ", 8)?)?)
                }
            }
        };
        Some(pin)
    }

    /// The GPT input it is, if it is one.
    fn gpt_input(self) -> Option<Input> {
        match self {
            Self::PortGp(bit) => PortGpPin::new(bit).map(Input::PortGp),
            Self::Pai => Some(Input::Pai),
            Self::Pclk => Some(Input::Pclk),
            _ => None,
        }
    }
}

impl ChipPin for Pin {
    fn name(self) -> &'static str {
        match self {
            Self::PortE(bit) => PE[usize::from(bit)],
            Self::PortF(bit) => PF[usize::from(bit)],
            Self::PortQs(bit) => PQS[usize::from(bit)],
            Self::PortGp(bit) => PGP[usize::from(bit)],
            Self::PortC(bit) => PC[usize::from(bit)],
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
            Self::PortE(_) | Self::PortF(_) | Self::PortQs(_) | Self::PortGp(_) | Self::Reset => {
                Direction::InOut
            }
            Self::Pai | Self::Pclk | Self::Irq(_) | Self::Rxd => Direction::In,
            Self::PortC(_) | Self::PwmA | Self::PwmB | Self::Eclk | Self::Txd => Direction::Out,
        }
    }

    fn scheduled(self) -> bool {
        self.gpt_input().is_some()
    }

    fn pcs(self) -> bool {
        matches!(self, Self::PortQs(bit) if (PCS0_PIN..PCS0_PIN + PCS_PINS).contains(&bit))
    }
}

/// The levels the board drives on the chip's input pins out of
/// power-on, `[chip.pins]`.
#[derive(Debug, Default, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Pins {
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
}

/// The MC68331 being wired for a board.
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

impl ChipBuilder for Mc68331Builder {
    type Chip = Mc68331;

    fn system_clock(&self) -> Frequency {
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
                .fixed_memory(Address(base), plan.bytes, plan.writable)
                .and_then(|id| {
                    self.builder
                        .fixed_timing(id, port, termination)
                        .map(|()| id)
                }),
            MemoryPlace::Selects(pins) => self.builder.memory(plan.bytes, plan.writable, pins),
        }
        .map_err(built)?;
        self.builder.reload_scope(id, plan.reload);
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
            self.builder.dsack(id, pins);
        }
        Ok(())
    }

    fn unwired<R: BoardMapped>(&mut self, pins: R, reset: ResetScope, irq: Option<IrqPin>) {
        let id = self.builder.unwired_peripheral(pins);
        self.builder.reset_scope(id, reset);
        if let Some(level) = irq {
            self.builder.interrupt(id, level);
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
        if let Some(e) = pins.e {
            chip.set_port_e_pins(e);
        }
        if let Some(f) = pins.f {
            chip.set_port_f_pins(f);
        }
        if let Some(qs) = pins.qs {
            chip.set_port_qs_pins(qs);
        }
        if let Some(gp) = pins.gp {
            chip.set_port_gp_pins(gp);
        }
        if let Some(pai) = pins.pai {
            chip.set_pai(pai);
        }
        if let Some(pclk) = pins.pclk {
            chip.set_pclk(pclk);
        }
        chip
    }
}

/// `byte` with `bit` at `level`.
const fn with_bit(byte: u8, bit: u8, level: bool) -> u8 {
    if level {
        byte | 1 << bit
    } else {
        byte & !(1 << bit)
    }
}

/// Each pin of a port of `pins` pins, at its level in `outputs`, undriven
/// pins high.
fn emit_port(
    emit: &mut impl FnMut(Time, Pin, Signal),
    at: Time,
    pins: u8,
    pin: impl Fn(u8) -> Pin,
    outputs: PortLevels,
) {
    let levels = outputs.pin_levels(u8::MAX);
    for bit in 0..pins {
        emit(at, pin(bit), Signal::Level(levels & 1 << bit != 0));
    }
}

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
        let configuration = Configuration {
            data: section.data.unwrap_or(u16::MAX),
            clock,
        };
        let mut builder = Builder::new(configuration);
        for &level in &section.autovector {
            let pin = IrqPin::new(level).ok_or_else(|| LoadError::Level {
                owner: "chip".to_owned(),
                level,
            })?;
            builder.autovector(pin);
        }
        let handles = Rc::new(Handles {
            sync: builder.sync_request(),
            diversion: builder.diversion(),
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

    fn halted(&self) -> Option<Halt> {
        Self::halted(self)
    }

    fn reset(&mut self) {
        Self::reset(self);
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

    fn system_clock(&self) -> Frequency {
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
            match pin {
                Pin::PortC(bit) => stop.port_c |= 1 << bit,
                Pin::PortE(bit) => stop.port_e |= 1 << bit,
                Pin::PortF(bit) => stop.port_f |= 1 << bit,
                Pin::PortGp(bit) => stop.port_gp |= 1 << bit,
                Pin::PortQs(bit) => stop.port_qs |= 1 << bit,
                Pin::PwmA => stop.pwm_a = true,
                Pin::PwmB => stop.pwm_b = true,
                Pin::Txd => stop.txd = true,
                Pin::Reset => stop.reset = true,
                // ECLK changes with the clock, which ends the run itself
                // (`set_clock_stop`); the rest are inputs
                Pin::Eclk | Pin::Pai | Pin::Pclk | Pin::Irq(_) | Pin::Rxd => {}
            }
        }
        self.set_output_stop(stop);
    }

    fn input(&mut self, pin: Pin, signal: Signal, at: Cycles) {
        match (pin, signal) {
            (_, Signal::Level(level)) if pin.gpt_input().is_some() => {
                let input = pin.gpt_input().expect("a GPT input");
                self.schedule_gpt_input(at, input, level);
            }
            (Pin::PortE(bit), Signal::Level(level)) => {
                let pins = with_bit(self.sim().port_e().pins, bit, level);
                self.set_port_e_pins(pins);
            }
            (Pin::PortF(bit), Signal::Level(level)) => {
                let pins = with_bit(self.sim().port_f().pins, bit, level);
                self.set_port_f_pins(pins);
            }
            (Pin::PortQs(bit), Signal::Level(level)) => {
                let pins = with_bit(self.qsm().port_pins(), bit, level);
                self.set_port_qs_pins(pins);
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
            Frequency(clock.frequency().0 / clock.eclk_divisor())
        })
    }

    fn collect<F: FnMut(Time, Pin, Signal)>(&mut self, base: TimeBase, mut emit: F) {
        for (at, change) in self.take_port_changes() {
            let (pins, pin): (u8, fn(u8) -> Pin) = match change.port {
                PortName::C => (PORT_C_PINS, Pin::PortC),
                PortName::E => (PORT_PINS, Pin::PortE),
                PortName::F => (PORT_PINS, Pin::PortF),
            };
            emit_port(&mut emit, base.time(at), pins, pin, change.outputs);
        }
        for (at, change) in self.take_gpt_changes() {
            let at = base.time(at);
            match change {
                OutputChange::PortGp(outputs) => {
                    emit_port(&mut emit, at, PORT_PINS, Pin::PortGp, outputs);
                }
                OutputChange::Pwm { channel, level } => {
                    let pin = if channel == Channel::A {
                        Pin::PwmA
                    } else {
                        Pin::PwmB
                    };
                    emit(at, pin, Signal::Level(level));
                }
                // a change on no pin a board net reaches
                _ => {}
            }
        }
        let system_clock = self.sim().clock().frequency();
        for sent in self.take_sci_transmitted() {
            let start = base.time(sent.start);
            let baud = u128::from(system_clock.0) / u128::from(sent.bit.0);
            let serial = Serial::timed(
                sent.frame,
                Frequency(u64::try_from(baud).unwrap_or(u64::MAX)),
                base.time(sent.end).since(start),
            );
            emit(start, Pin::Txd, Signal::Serial(serial));
        }
        for (at, outputs) in self.take_qsm_changes() {
            emit_port(&mut emit, base.time(at), PORT_PINS, Pin::PortQs, outputs);
        }
    }

    fn take_events(&mut self) -> Vec<Event> {
        Self::take_events(self)
    }

    fn reset_pulse(event: &Event) -> Option<(Cycles, Cycles)> {
        match *event {
            Event::Reset(at, _) => Some((at, at + Cycles(RESET_CLOCKS))),
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
    use mc683xx_cpu32::interrupt::IrqPin;

    use super::PORT_C_PINS;
    use super::PORT_PINS;
    use super::Pin;
    use crate::chip::ChipPin;

    #[test]
    fn pins_show_as_they_parse() {
        let mut pins: Vec<Pin> = (0..PORT_PINS)
            .flat_map(|bit| {
                [
                    Pin::PortE(bit),
                    Pin::PortF(bit),
                    Pin::PortQs(bit),
                    Pin::PortGp(bit),
                ]
            })
            .chain((0..PORT_C_PINS).map(Pin::PortC))
            .chain((1..=7).map(|level| Pin::Irq(IrqPin::new(level).unwrap())))
            .collect();
        pins.extend([
            Pin::Pai,
            Pin::Pclk,
            Pin::Reset,
            Pin::PwmA,
            Pin::PwmB,
            Pin::Eclk,
            Pin::Txd,
            Pin::Rxd,
        ]);
        for pin in pins {
            assert_eq!(pin.name().parse(), Ok(pin), "{pin}");
            assert_eq!(pin.to_string(), pin.name());
        }
        assert_eq!("PGP0".parse(), Ok(Pin::PortGp(0)));
        assert_eq!("IRQ7".parse(), Ok(Pin::Irq(IrqPin::IRQ7)));
        for bad in ["IRQ0", "PC7", "PE8", "NMI", ""] {
            assert!(bad.parse::<Pin>().is_err(), "{bad}");
        }
    }

    #[test]
    fn a_pin_carries_its_kind_and_faces_its_way() {
        use crate::signal::Direction;
        use crate::signal::Kind;

        assert_eq!(Pin::Txd.kind(), Kind::Serial);
        assert_eq!(Pin::Eclk.kind(), Kind::Clock);
        assert_eq!(Pin::PortE(0).kind(), Kind::Logic);
        assert_eq!(Pin::Pai.direction(), Direction::In);
        assert_eq!(Pin::PwmA.direction(), Direction::Out);
        assert_eq!(Pin::Reset.direction(), Direction::InOut);
        assert!(Pin::PortGp(0).scheduled() && Pin::Pclk.scheduled());
        assert!(!Pin::PortE(0).scheduled());
    }
}
