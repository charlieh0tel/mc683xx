//! The board's peripherals as the chip sees them.
//!
//! One peripheral on each select that peripherals share or a decoder splits, a
//! model for each peripheral's IRQ output and RESET input, and a router on
//! the QSPI ([PLAN.md, Boards][plan-boards]).  Each turns the chip's cycle
//! into the peripheral's [`PeripheralAccess`] on board time, through the
//! attachment's role, lane and lines.
//!
//! [plan-boards]: https://github.com/charlieh0tel/mc683xx/blob/main/PLAN.md#boards

use std::cell::Cell;
use std::cell::RefCell;
use std::mem;
use std::rc::Rc;

use mc683xx_cpu32::bus::BusError;
use mc683xx_cpu32::bus::StopRequest;
use mc683xx_cpu32::cpu::state::Stateless;
use mc683xx_cpu32::cycles::Cycles;
use mc683xx_cpu32::peripheral::BusCycle;
use mc683xx_cpu32::peripheral::MappedPeripheral;
use mc683xx_cpu32::size::Size;
use mc683xx_modules::chip_select::ChipSelect;
use mc683xx_modules::peripheral::ClockPhase;
use mc683xx_modules::peripheral::ClockPolarity;
use mc683xx_modules::peripheral::QspiClock;
use mc683xx_modules::peripheral::QspiTransfer;
use mc683xx_modules::peripheral::SpiPeripheral;
use mc683xx_modules::peripheral::Timed;
use mc683xx_modules::peripheral::floating_miso;
use serde::Deserialize;
use serde::Serialize;

use crate::chip::Requests;
use crate::description::DecoderInput;
use crate::description::PCS_MASK;
use crate::peripheral::Drive;
use crate::peripheral::Driven;
use crate::peripheral::Lane;
use crate::peripheral::MemoryWrite;
use crate::peripheral::Peripheral;
use crate::peripheral::PeripheralAccess;
use crate::peripheral::Role;
use crate::peripheral::Transfer;
use crate::signal::Signal;
use crate::time::Time;
use crate::time::TimeBase;
use crate::trace;
use crate::trace::Record;

/// A peripheral, shared by the chip's peripherals and the board's scheduler.
pub(crate) type PeripheralCell = Rc<RefCell<Box<dyn Peripheral>>>;

/// What the board's peripherals, models and router share with the board.
pub(crate) struct Shared {
    /// The chip's clocks on board time, as of the quantum running.
    pub(crate) base: Cell<TimeBase>,
    /// The peripherals, in file order.
    pub(crate) peripherals: Vec<PeripheralCell>,
    /// For each peripheral and input port, the analog net that drives it.
    pub(crate) analog_inputs: Vec<Vec<Option<usize>>>,
    /// For each net, the peripheral and port that drive it, if a peripheral
    /// does.
    pub(crate) net_drivers: Vec<Option<(usize, usize)>>,
    /// Each net's last value.
    pub(crate) last: RefCell<Vec<Option<Signal>>>,
    /// What peripherals drove in calls the board has not handled yet.
    pub(crate) outbox: RefCell<Vec<(usize, Driven)>>,
    /// Memory writes not yet applied.
    pub(crate) writes: RefCell<Vec<MemoryWrite>>,
    /// Whether a peripheral took or released a memory since the board
    /// last made the chip's diversions.
    pub(crate) diverted: Cell<bool>,
    /// Whether a peripheral asked the run to stop.
    pub(crate) stop: Cell<bool>,
    /// The chip's stop request, for a peripheral that asks during a cycle.
    pub(crate) chip_stop: StopRequest,
    /// The chip's handles, to end its run early for a peripheral that
    /// drives within it, and to divert a memory to its router.
    pub(crate) requests: Rc<dyn Requests>,
    /// While the chip runs, the board time its run is to end at: a
    /// peripheral called then is called from within it.
    pub(crate) quantum_end: Cell<Option<Time>>,
    /// Whether peripheral cycles are traced.
    pub(crate) trace_bus_cycles: Cell<bool>,
    /// Whether QSPI transfers are traced.
    pub(crate) trace_qspi: Cell<bool>,
    /// The PC of the instruction running, while peripheral cycles are
    /// traced: a code hook sets it.
    pub(crate) pc: Rc<Cell<u32>>,
    /// The peripheral and QSPI records, until the board takes them.
    pub(crate) records: RefCell<Vec<Record>>,
    /// The board's memories, in file order, as peripherals take them.
    pub(crate) memories: RefCell<Vec<SharedMemory>>,
    /// The PCS decode's state, if the board has peripherals on the QSPI:
    /// here, beside the peripherals, so the board's snapshot holds it.
    pub(crate) qspi: RefCell<Option<QspiState>>,
    /// Whether a PCS pin is on a net a peripheral, the chip or an observer
    /// senses: the QSPI's calls are then never deferred.
    pub(crate) pcs_sensed: Cell<bool>,
}

/// A board memory as peripherals take it.
#[derive(Debug, Clone)]
pub(crate) struct SharedMemory {
    /// Its name.
    pub(crate) name: String,
    /// Its bytes.
    pub(crate) length: u32,
    /// Its attachments' selects, in order.
    pub(crate) pins: Vec<ChipSelect>,
    /// The peripheral that has it.
    pub(crate) taker: Option<usize>,
}

impl Shared {
    /// Board time at `cycles`.
    pub(crate) fn time(&self, cycles: Cycles) -> Time {
        self.base.get().time(cycles)
    }

    /// Calls `call` on peripheral `peripheral` at `now`, and keeps what it
    /// drove.
    pub(crate) fn call<R>(
        &self,
        peripheral: usize,
        now: Time,
        call: impl FnOnce(&mut dyn Peripheral, &mut Drive<'_>) -> R,
    ) -> R {
        let sampler = |port: usize, at: Time| self.sample(peripheral, port, at);
        let mut drive = Drive::with_sampler(now, &sampler);
        drive.set_system_clock(self.base.get());
        let result = call(
            self.peripherals[peripheral].borrow_mut().as_mut(),
            &mut drive,
        );
        let driving = drive.finish();
        // What a peripheral does within the chip's run reaches the nets and
        // the chip only as the run ends: a drive, a memory write or an
        // event before its end ends it after this instruction, for the
        // board to settle there, so the run is the same however the
        // host quanta it.
        if let Some(quantum_end) = self.quantum_end.get()
            && (driving.driven.iter().any(|driven| driven.at < quantum_end)
                || !driving.writes.is_empty()
                || self.peripherals[peripheral]
                    .borrow()
                    .next_event()
                    .is_some_and(|next| next < quantum_end))
        {
            self.requests.sync();
        }
        self.outbox.borrow_mut().extend(
            driving
                .driven
                .into_iter()
                .map(|driven| (peripheral, driven)),
        );
        self.writes.borrow_mut().extend(driving.writes);
        for (name, take) in driving.memories {
            self.take_memory(peripheral, &name, take);
        }
        if driving.stop {
            self.stop.set(true);
            self.chip_stop.request();
        }
        result
    }

    /// `peripheral` takes the memory `name`, or releases it.
    fn take_memory(&self, peripheral: usize, name: &str, take: bool) {
        let mut memories = self.memories.borrow_mut();
        let Some(index) = memories.iter().position(|memory| memory.name == name) else {
            return;
        };
        let memory = &mut memories[index];
        if take {
            memory.taker = Some(peripheral);
            self.requests.divert(index);
        } else if memory.taker == Some(peripheral) {
            memory.taker = None;
            self.requests.release(index);
        } else {
            return;
        }
        self.diverted.set(true);
    }

    /// The analog value on `peripheral`'s input `port` at `at`.
    fn sample(&self, peripheral: usize, port: usize, at: Time) -> Option<f64> {
        let net = (*self.analog_inputs.get(peripheral)?.get(port)?)?;
        if let Some((source, source_port)) = self.net_drivers[net]
            && let Ok(mut source) = self.peripherals[source].try_borrow_mut()
            && let Some(value) = source.sample(source_port, at)
        {
            return Some(value);
        }
        match self.last.borrow()[net] {
            Some(Signal::Analog(value)) => Some(value),
            _ => None,
        }
    }
}

/// One attachment of a peripheral on a select or a decoder output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Route {
    /// The peripheral.
    pub(crate) peripheral: usize,
    /// The attachment's index.
    pub(crate) attachment: usize,
    /// Its role.
    pub(crate) role: Role,
    /// Its lane.
    pub(crate) lane: Lane,
    /// The ADDR lines on the peripheral's A0 up.
    pub(crate) lines: Vec<u8>,
    /// Added to the address.
    pub(crate) offset: u32,
}

impl Route {
    /// Whether it takes a cycle of this space and direction.
    const fn takes(&self, cpu_space: bool, write: bool) -> bool {
        match self.role {
            Role::Iack => cpu_space,
            Role::Read => !cpu_space && !write,
            Role::Write => !cpu_space && write,
            Role::ReadWrite => !cpu_space,
        }
    }

    /// The address the peripheral sees for ADDR `address`.
    fn address(&self, address: u32) -> u32 {
        let gathered = self
            .lines
            .iter()
            .enumerate()
            .fold(0, |gathered, (bit, &line)| {
                gathered | (address >> line & 1) << bit
            });
        self.offset.wrapping_add(gathered)
    }
}

/// The peripherals on one select, or one decoder output.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Routes(pub(crate) Vec<Route>);

/// A piece of a cycle for one peripheral: the route, and the byte or word.
type Piece<'a> = (&'a Route, Size, Lane);

impl Routes {
    /// The pieces of `cycle` and who takes each: the peripheral on both
    /// lanes, or a byte to the peripheral on each lane the cycle drives.
    fn pieces(&self, cycle: BusCycle, write: bool) -> Vec<Piece<'_>> {
        let cpu_space = cycle.function_code.is_cpu_space();
        let taking = || {
            self.0
                .iter()
                .filter(move |route| route.takes(cpu_space, write))
        };
        if let Some(route) = taking().find(|route| route.lane == Lane::Both) {
            return vec![(route, cycle.size, Lane::Both)];
        }
        let lanes: &[Lane] = match cycle.size {
            Size::Byte if cycle.address & 1 == 0 => &[Lane::Upper],
            Size::Byte => &[Lane::Lower],
            _ => &[Lane::Upper, Lane::Lower],
        };
        lanes
            .iter()
            .filter_map(|&lane| {
                taking()
                    .find(|route| route.lane == lane)
                    .map(|route| (route, Size::Byte, lane))
            })
            .collect()
    }

    /// `route`'s access for a piece of `cycle`.
    fn access(shared: &Shared, route: &Route, cycle: BusCycle, size: Size) -> PeripheralAccess {
        PeripheralAccess::new(
            select_of(cycle),
            route.address(cycle.address),
            size,
            cycle.function_code,
            shared.time(cycle.now),
        )
        .through(route.attachment, route.role, route.lane)
    }

    /// Notes a cycle in the trace, if it is on and a peripheral took it.
    fn traced(shared: &Shared, cycle: BusCycle, write: bool, value: Option<u32>, answered: bool) {
        if answered && shared.trace_bus_cycles.get() {
            shared
                .records
                .borrow_mut()
                .push(Record::BusCycle(trace::BusCycleRecord {
                    cycle: cycle.now,
                    pc: shared.pc.get(),
                    write,
                    address: cycle.address,
                    size: cycle.size.bytes(),
                    value,
                }));
        }
    }

    /// A read: each piece's peripheral's value on its lane, 0 where no
    /// peripheral drives.
    pub(crate) fn read(&self, shared: &Shared, cycle: BusCycle) -> Result<u32, BusError> {
        let pieces = self.pieces(cycle, false);
        let read = pieces.iter().try_fold(0, |value, &(route, size, lane)| {
            let access = Self::access(shared, route, cycle, size);
            let read = shared.call(route.peripheral, access.now, |peripheral, drive| {
                peripheral.read(access, drive)
            })?;
            Ok(value
                | match lane {
                    Lane::Upper if cycle.size != Size::Byte => (read & 0xff) << 8,
                    Lane::Both => read,
                    _ => read & 0xff,
                })
        });
        Self::traced(shared, cycle, false, read.ok(), !pieces.is_empty());
        read
    }

    /// A write: each piece's peripheral gets its lane's byte, or the whole.
    pub(crate) fn write(
        &self,
        shared: &Shared,
        cycle: BusCycle,
        value: u32,
    ) -> Result<(), BusError> {
        let pieces = self.pieces(cycle, true);
        Self::traced(shared, cycle, true, Some(value), !pieces.is_empty());
        for (route, size, lane) in pieces {
            let access = Self::access(shared, route, cycle, size);
            let data = match lane {
                Lane::Upper if cycle.size != Size::Byte => value >> 8 & 0xff,
                Lane::Both => value,
                _ => value & 0xff,
            };
            shared.call(route.peripheral, access.now, |peripheral, drive| {
                peripheral.write(access, data, drive)
            })?;
        }
        Ok(())
    }

    /// The wait states of the first peripheral to take the cycle.
    pub(crate) fn wait_states(&self, shared: &Shared, cycle: BusCycle, write: bool) -> Option<u8> {
        let Some(&(route, size, _)) = self.pieces(cycle, write).first() else {
            return Some(0);
        };
        let access = Self::access(shared, route, cycle, size);
        shared.peripherals[route.peripheral]
            .borrow_mut()
            .wait_states(access)
    }
}

/// The peripheral on a select that peripherals take, split by role and lane.
pub(crate) struct SelectRouter {
    /// The board's shared state.
    pub(crate) shared: Rc<Shared>,
    /// The peripherals on it.
    pub(crate) routes: Routes,
}

impl MappedPeripheral for SelectRouter {
    fn read(&mut self, cycle: BusCycle) -> Result<u32, BusError> {
        self.routes.read(&self.shared, cycle)
    }

    fn write(&mut self, cycle: BusCycle, value: u32) -> Result<(), BusError> {
        self.routes.write(&self.shared, cycle, value)
    }

    fn wait_states(&mut self, cycle: BusCycle) -> Option<u8> {
        // The chip asks before the cycle, not saying its direction; a
        // read's peripherals answer.
        self.routes.wait_states(&self.shared, cycle, false)
    }
}

impl Timed for SelectRouter {}

// The chip-side peripherals route to the peripherals, whose state is in
// the board's snapshot: they keep none of their own.
impl Stateless for SelectRouter {}

/// The peripheral on a decoder's select: the code picks an output.
pub(crate) struct DecoderRouter {
    /// The board's shared state.
    pub(crate) shared: Rc<Shared>,
    /// Its inputs, least significant first.
    pub(crate) inputs: Vec<DecoderInput>,
    /// The output each code selects.
    pub(crate) outputs: Vec<u32>,
    /// The peripherals on each output.
    pub(crate) routes: Vec<Routes>,
    /// Whether a code with nothing attached is a bus error.
    pub(crate) bus_error: bool,
}

impl DecoderRouter {
    /// The peripherals `address` selects, or [`None`] for an undecoded code.
    fn routes(&self, address: u32) -> Option<&Routes> {
        let code = self
            .inputs
            .iter()
            .enumerate()
            .fold(0_usize, |code, (bit, input)| {
                let level = match *input {
                    DecoderInput::Line(line) => address >> line & 1 != 0,
                    DecoderInput::Tied(level) => level,
                };
                code | usize::from(level) << bit
            });
        let routes = &self.routes[self.outputs[code] as usize];
        (!routes.0.is_empty()).then_some(routes)
    }

    /// An undecoded cycle's end: open bus or BERR.
    const fn undecoded(&self) -> Result<(), BusError> {
        if self.bus_error {
            Err(BusError)
        } else {
            Ok(())
        }
    }
}

impl MappedPeripheral for DecoderRouter {
    fn read(&mut self, cycle: BusCycle) -> Result<u32, BusError> {
        match self.routes(cycle.address) {
            Some(routes) => routes.read(&self.shared, cycle),
            None => self.undecoded().map(|()| 0),
        }
    }

    fn write(&mut self, cycle: BusCycle, value: u32) -> Result<(), BusError> {
        match self.routes(cycle.address) {
            Some(routes) => routes.write(&self.shared, cycle, value),
            None => self.undecoded(),
        }
    }

    fn wait_states(&mut self, cycle: BusCycle) -> Option<u8> {
        self.routes(cycle.address).map_or(Some(0), |routes| {
            routes.wait_states(&self.shared, cycle, false)
        })
    }
}

impl Timed for DecoderRouter {}

impl Stateless for DecoderRouter {}

/// A board memory's cycles while a peripheral has taken it: the chip diverts
/// them here, and this passes them to the peripheral.
pub(crate) struct MemoryRouter {
    /// The board's shared state.
    pub(crate) shared: Rc<Shared>,
    /// The memory, by its index in [`Shared::memories`].
    pub(crate) memory: usize,
}

impl MemoryRouter {
    /// The peripheral that has the memory, its name, and the access `cycle`
    /// is, in `role`.
    fn access(&self, cycle: BusCycle, role: Role) -> Option<(usize, String, PeripheralAccess)> {
        let memories = self.shared.memories.borrow();
        let memory = &memories[self.memory];
        let attachment = memory
            .pins
            .iter()
            .position(|&pin| pin == select_of(cycle))
            .unwrap_or(0);
        let access = PeripheralAccess::new(
            select_of(cycle),
            cycle.address % memory.length,
            cycle.size,
            cycle.function_code,
            self.shared.time(cycle.now),
        )
        .through(attachment, role, Lane::Both);
        Some((memory.taker?, memory.name.clone(), access))
    }
}

impl MappedPeripheral for MemoryRouter {
    fn read(&mut self, cycle: BusCycle) -> Result<u32, BusError> {
        let (peripheral, name, access) = self.access(cycle, Role::Read).ok_or(BusError)?;
        self.shared
            .call(peripheral, access.now, |peripheral, drive| {
                peripheral.memory_read(&name, access, drive)
            })
    }

    fn write(&mut self, cycle: BusCycle, value: u32) -> Result<(), BusError> {
        let Some((peripheral, name, access)) = self.access(cycle, Role::Write) else {
            return Ok(());
        };
        self.shared
            .call(peripheral, access.now, |peripheral, drive| {
                peripheral.memory_write(&name, access, value, drive)
            })
    }
}

impl Timed for MemoryRouter {}

impl Stateless for MemoryRouter {}

/// A peripheral's IRQ output and RESET input, as a model on no pin.
pub(crate) struct PeripheralPins {
    /// The board's shared state.
    pub(crate) shared: Rc<Shared>,
    /// The peripheral.
    pub(crate) peripheral: usize,
}

impl MappedPeripheral for PeripheralPins {
    fn read(&mut self, _: BusCycle) -> Result<u32, BusError> {
        Err(BusError)
    }

    fn write(&mut self, _: BusCycle, _: u32) -> Result<(), BusError> {
        Err(BusError)
    }

    fn interrupt_request(&self) -> bool {
        self.shared.peripherals[self.peripheral]
            .try_borrow()
            .is_ok_and(|peripheral| peripheral.interrupt_request())
    }

    fn reset(&mut self, now: Cycles) {
        let now = self.shared.time(now);
        self.shared.call(self.peripheral, now, |peripheral, drive| {
            peripheral.reset(now, drive)
        });
    }
}

impl Timed for PeripheralPins {}

impl Stateless for PeripheralPins {}

/// A peripheral on the QSPI under a PCS code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct QspiTarget {
    /// The peripheral.
    pub(crate) peripheral: usize,
    /// Its attachment's index.
    pub(crate) attachment: usize,
    /// The levels that select it.
    pub(crate) code: u8,
    /// The PCS pins compared.
    pub(crate) mask: u8,
}

impl QspiTarget {
    /// Whether PCS `levels` select it.
    const fn selected(self, levels: u8) -> bool {
        levels & self.mask == self.code
    }
}

/// What the PCS decode keeps between transfers, saved in the board's
/// snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct QspiState {
    /// The levels on PCS\[3:0\] now.
    levels: u8,
    /// For each target, whether no transfer has reached it since its
    /// select asserted.
    framing: Vec<bool>,
}

impl QspiState {
    /// PCS\[3:0\] at `levels`, and every one of `targets` before a
    /// transfer.
    pub(crate) fn new(levels: u8, targets: usize) -> Self {
        Self {
            levels,
            framing: vec![true; targets],
        }
    }

    /// The targets it frames.
    pub(crate) fn targets(&self) -> usize {
        self.framing.len()
    }

    /// Whether its levels are PCS\[3:0\]'s alone, as a run leaves them.
    pub(crate) const fn levels_fit(&self) -> bool {
        self.levels & !PCS_MASK == 0
    }
}

/// The QSPI's peripheral: the PCS decode to the peripherals.  A transfer
/// reaches every peripheral whose attachment the PCS levels select, in file
/// order, and MISO is the AND of their replies: an open line pulled
/// high, any driver of a 0 winning.
pub(crate) struct QspiRouter {
    /// The board's shared state.
    pub(crate) shared: Rc<Shared>,
    /// The peripherals, by code.
    pub(crate) targets: Vec<QspiTarget>,
    /// Whether every peripheral on the QSPI says its calls may come late
    /// ([`Peripheral::qspi_deferrable`]): they do if no net senses PCS
    /// as well ([`Shared::pcs_sensed`]).
    pub(crate) deferrable: bool,
}

// Its own state is in the board's snapshot, beside the peripherals', so
// that the chip's holds none of it.
impl Stateless for QspiRouter {}

impl SpiPeripheral for QspiRouter {
    fn transfer(&mut self, transfer: QspiTransfer) -> u16 {
        let QspiTransfer {
            entry,
            chip_selects,
            out,
            bits,
            clock,
            now: cycle,
            ..
        } = transfer;
        let now = self.shared.time(cycle);
        // every selected peripheral, in file order; MISO wired-AND, an
        // undriven peripheral's all ones
        let mut input = floating_miso(bits);
        for (index, target) in self.targets.iter().enumerate() {
            if !target.selected(chip_selects) {
                continue;
            }
            let transfer = Transfer::new(
                target.attachment,
                entry,
                chip_selects,
                out,
                bits,
                clock,
                now,
            );
            let framing = self
                .shared
                .qspi
                .borrow_mut()
                .as_mut()
                .is_some_and(|state| mem::replace(&mut state.framing[index], false));
            let transfer = if framing {
                transfer
            } else {
                transfer.continuing()
            };
            input &= self
                .shared
                .call(target.peripheral, now, |peripheral, drive| {
                    peripheral.transfer(transfer, drive)
                });
        }
        if self.shared.trace_qspi.get() {
            self.shared
                .records
                .borrow_mut()
                .push(Record::Qspi(trace::QspiRecord {
                    cycle,
                    pcs: chip_selects,
                    bits,
                    out,
                    input,
                    cpol: clock.polarity == ClockPolarity::IdleHigh,
                    cpha: clock.phase == ClockPhase::ChangeLeading,
                }));
        }
        input
    }

    fn chip_selects(&mut self, levels: u8, now: Cycles) {
        let now = self.shared.time(now);
        let was = self
            .shared
            .qspi
            .borrow()
            .as_ref()
            .map_or(PCS_MASK, |state| state.levels);
        for (index, target) in self.targets.iter().enumerate() {
            let (before, after) = (target.selected(was), target.selected(levels));
            if before != after {
                if after && let Some(state) = self.shared.qspi.borrow_mut().as_mut() {
                    state.framing[index] = true;
                }
                self.shared
                    .call(target.peripheral, now, |peripheral, drive| {
                        peripheral.select(target.attachment, after, now, drive);
                    });
            }
        }
        if let Some(state) = self.shared.qspi.borrow_mut().as_mut() {
            state.levels = levels;
        }
    }

    fn clock(&mut self, clock: QspiClock, now: Cycles) {
        let now = self.shared.time(now);
        let mut told: Vec<usize> = Vec::new();
        for target in &self.targets {
            if !told.contains(&target.peripheral) {
                told.push(target.peripheral);
                self.shared.peripherals[target.peripheral]
                    .borrow_mut()
                    .qspi_clock(clock, now);
            }
        }
    }

    fn deferrable(&self) -> bool {
        // a deferred queue moves PCS only where the CPU can see it, so a
        // net sensing PCS would see its changes late
        self.deferrable && !self.shared.pcs_sensed.get()
    }
}

/// The select that carried `cycle`: the chip's cycles always carry one.
fn select_of(cycle: BusCycle) -> ChipSelect {
    cycle
        .select
        .and_then(ChipSelect::from_select)
        .expect("a chip's cycle carries its select")
}
