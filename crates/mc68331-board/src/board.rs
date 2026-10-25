//! A board running: the chip, the peripherals, the nets between them and
//! the scheduler on board time.
//!
//! The run loop is this library's, for hosts to embed; `mc683xx-run` is
//! a thin binary on it ([PLAN.md, Boards][plan-boards]).
//!
//! [plan-boards]: https://github.com/charlieh0tel/mc683xx/blob/main/PLAN.md#boards

use std::any::Any;
use std::array;
use std::cell::Cell;
use std::cell::Ref;
use std::cell::RefCell;
use std::cell::RefMut;
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::fmt;
use std::mem;
use std::num::NonZeroU64;
use std::ops::ControlFlow;
use std::path::Path;
use std::rc::Rc;
use std::rc::Weak;

use mc683xx_cpu32::address::Address;
use mc683xx_cpu32::cpu::Stop;
use mc683xx_cpu32::cpu::hooks::ExceptionEntry;
use mc683xx_cpu32::cpu::hooks::HookId;
use mc683xx_cpu32::cpu::state::StateError;
use mc683xx_cpu32::cpu::state::decode;
use mc683xx_cpu32::cpu::state::encode;
use mc683xx_cpu32::cpu::state::restore_each;
use mc683xx_cpu32::cycles::Cycles;
use mc683xx_cpu32::memory::Space;
use mc683xx_cpu32::size::Size;
use mc683xx_modules::chip_select::ChipSelect;
use serde::Deserialize;
use serde::Serialize;

use crate::chip::Chip;
use crate::chip::ChipBuilder;
use crate::chip::ChipPin;
use crate::chip::MemoryPlace;
use crate::chip::MemoryPlan;
use crate::chip::takes;
use crate::control::BoardControl;
use crate::control::Endpoints;
use crate::description::Description;
use crate::description::NetSpec;
use crate::description::Options;
use crate::description::Target;
use crate::endpoint::Endpoint;
use crate::error::LoadError;
use crate::error::NetError;
use crate::error::ReceiveError;
use crate::error::RunError;
use crate::error::SendError;
use crate::inspect::Inspection;
use crate::observe::Attached;
use crate::observe::Observer;
use crate::observe::ObserverId;
use crate::pacing::Pacer;
use crate::registry::Context;
use crate::registry::Environment;
use crate::registry::Parameters;
use crate::registry::PortSpec;
use crate::registry::Registry;
use crate::registry::repeated_port;
use crate::schema::Undecoded;
use crate::signal::Direction;
use crate::signal::Kind;
use crate::signal::Signal;
use crate::time::Clock;
use crate::time::Time;
use crate::time::TimeBase;
use crate::trace;
use crate::trace::NetRecord;
use crate::trace::Record;
use crate::wiring::DecoderRouter;
use crate::wiring::MemoryRouter;
use crate::wiring::PeripheralCell;
use crate::wiring::PeripheralPins;
use crate::wiring::QspiRouter;
use crate::wiring::QspiState;
use crate::wiring::QspiTarget;
use crate::wiring::Route;
use crate::wiring::Routes;
use crate::wiring::SelectRouter;
use crate::wiring::Shared;
use crate::wiring::SharedMemory;

/// The format of [`Board::save`]'s bytes.  The chip's and the peripherals'
/// snapshots inside carry their own.
pub const VERSION: u32 = 13;

/// The levels on PCS\[3:0\] before the QSPI drives them: high.
const PCS_IDLE: u8 = 0xf;

/// What drives a net.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum Source<P> {
    /// A peripheral's output, by peripheral and port.
    Peripheral(usize, usize),
    /// A chip pin.
    Chip(P),
    /// A run-time endpoint.
    External(String),
}

/// What a net drives.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Sink<P> {
    /// A peripheral's input, by peripheral and port.
    Peripheral(usize, usize),
    /// A chip pin.
    Chip(P),
    /// A run-time endpoint.
    External(String),
}

/// A net as the board runs it.
#[derive(Debug, Clone, PartialEq)]
struct Net<P> {
    /// Its name.
    name: String,
    /// Its type.
    kind: Kind,
    /// Open drain.
    wired: bool,
    /// Its drivers.
    drivers: Vec<Source<P>>,
    /// Its sinks.
    sinks: Vec<Sink<P>>,
    /// A wired net's drivers' levels.
    driven: Vec<bool>,
    /// A wired net's level.
    level: bool,
}

impl<P: PartialEq> Net<P> {
    /// A wired net's level with `source`'s drive left out: the AND of
    /// its other drivers', high if there are none.
    fn level_without(&self, source: &Source<P>) -> bool {
        self.drivers
            .iter()
            .zip(&self.driven)
            .all(|(driver, &level)| driver == source || level)
    }
}

/// A change a driver made, waiting for its time.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
struct Change {
    /// The net.
    net: usize,
    /// The driver, by index in the net's drivers.
    driver: usize,
    /// The value.
    signal: Signal,
}

/// A level on a [`ChipPin::scheduled`] input that the chip holds for a
/// later clock: kept on board time, to schedule again at its new clock
/// when the chip's clocks move against board time.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
struct Ahead<P> {
    /// When it comes.
    at: Time,
    /// The clock the chip holds it for.
    cycles: Cycles,
    /// The pin.
    pin: P,
    /// The level.
    signal: Signal,
}

/// What the board traces, off by default: each costs on every change,
/// cycle or exception ([`docs/tracing.md`][tracing]).
///
/// It may gain fields: build one from [`BoardTracing::default()`] and set
/// those it needs (`let mut x = BoardTracing::default(); x.pins = ...;`).
///
/// [tracing]: https://github.com/charlieh0tel/mc683xx/blob/main/docs/tracing.md
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct BoardTracing {
    /// Each value a net carries.
    pub nets: bool,
    /// Each bus cycle a peripheral answers, with its instruction's PC (a code
    /// hook on every instruction: the run takes about twice as long).
    pub bus_cycles: bool,
    /// Each exception entered.
    pub exceptions: bool,
    /// Each level change of a chip pin on a net.
    pub pins: bool,
    /// Each QSPI transfer.
    pub qspi: bool,
}

/// What the board traced since the last `take_trace`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Trace {
    /// The records, in cycle order.
    pub records: Vec<Record>,
    /// Net changes, in time order.
    pub nets: Vec<NetRecord>,
}

/// The chip hooks the board's tracing holds.
#[derive(Debug, Default)]
struct Hooks {
    /// The code hook that keeps the PC for peripheral records.
    pc: Option<HookId>,
    /// The exception hook that stops the run to read the frame.
    exception: Option<HookId>,
    /// The exceptions entered and not yet recorded.
    entered: Rc<RefCell<Vec<ExceptionEntry>>>,
    /// The code watches, each a hook that stops the run.
    watches: BTreeSet<HookId>,
}

/// How many times running a peripheral may be advanced to one time, its next
/// event never past it, before it stalls the board: enough for a peripheral
/// with several events at one time, few enough to fail at once.
const STALL_REPEATS: u32 = 1_000;

/// A run-time endpoint, `external.name`, as [`Board::externals`] gives it.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct External {
    /// Its name.
    pub name: String,
    /// Its nets' type.
    pub kind: Kind,
    /// Which way it faces: `Out` if it drives a net, `In` if it is
    /// driven, `InOut` if both.
    pub direction: Direction,
}

/// Why a run ended, and when.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct BoardRun {
    /// Why: its time ran out ([`Stop::Budget`]), the CPU halted, a hook
    /// stopped it, or the host or a peripheral asked ([`Stop::Requested`]).
    pub stop: Stop,
    /// Board time at its end, never past the time it ran to, though
    /// the chip's last instruction may end past it.
    pub now: Time,
}

impl BoardRun {
    /// A run that ended for `stop` at `now`: a board's own, or a host's
    /// stand-in for one (a debugger that ended the session).
    #[must_use]
    pub const fn new(stop: Stop, now: Time) -> Self {
        Self { stop, now }
    }
}

/// A peripheral on the board.
#[derive(Debug)]
struct PeripheralInfo {
    /// Its name.
    name: String,
    /// The chip's marking or part number as fitted.
    marking: Option<String>,
    /// Its ports.
    ports: Vec<PortSpec>,
}

/// The board's own state in a snapshot.
#[derive(Debug, Serialize, Deserialize)]
struct BoardState<P> {
    /// The chip's snapshot.
    chip: Vec<u8>,
    /// Each peripheral's snapshot.
    peripherals: Vec<Vec<u8>>,
    /// Board time.
    now: Time,
    /// The chip's clocks on board time.
    base: TimeBase,
    /// The changes waiting.
    queue: Vec<((Time, u64), Change)>,
    /// The next change's sequence number.
    sequence: u64,
    /// The chip output pins' last levels.
    outputs: Vec<(P, bool)>,
    /// The wired nets' levels.
    wired: Vec<(Vec<bool>, bool)>,
    /// Each net's last value.
    last: Vec<Option<Signal>>,
    /// The chip's clock outputs as last driven.
    clocks: Vec<(P, Clock)>,
    /// RESET's level as the nets drive it.
    reset_in: bool,
    /// The peripheral that has each memory, in file order.
    takers: Vec<Option<usize>>,
    /// The PCS decode's state, if the board has peripherals on the QSPI.
    qspi: Option<QspiState>,
    /// The levels the chip holds for later clocks on its scheduled inputs.
    ahead: Vec<Ahead<P>>,
}

/// A board: the chip and everything the board file wires to it.
pub struct Board<C: Chip> {
    /// The chip.
    chip: C,
    /// What the chip's peripherals share with the board.
    shared: Rc<Shared>,
    /// The peripherals' names and ports.
    peripherals: Vec<PeripheralInfo>,
    /// The memories, by name.
    memories: BTreeMap<String, C::Memory>,
    /// The nets.
    nets: Vec<Net<C::Pin>>,
    /// The nets each driver drives.
    sources: BTreeMap<Source<C::Pin>, Vec<usize>>,
    /// Changes waiting, by time and the order they were made.
    queue: BTreeMap<(Time, u64), Change>,
    /// The levels the chip holds for later clocks on its scheduled
    /// inputs, in the order driven.
    ahead: Vec<Ahead<C::Pin>>,
    /// The next change's sequence number.
    sequence: u64,
    /// Board time.
    now: Time,
    /// The chip output pins on nets, and their last levels.
    outputs: BTreeMap<C::Pin, bool>,
    /// The chip's clock outputs on nets, as last driven.
    clocks: BTreeMap<C::Pin, Clock>,
    /// The changes of a quantum's `collect`, kept for the next: scratch.
    emitted: Vec<(Time, C::Pin, Signal)>,
    /// RESET's level as the nets drive it.
    reset_in: bool,
    /// What reached each run-time endpoint.
    received: BTreeMap<String, Vec<(Time, Signal)>>,
    /// The chip's events, until the host takes them.
    events: Vec<C::Event>,
    /// What is traced.
    tracing: BoardTracing,
    /// The nets traced.
    net_trace: Vec<NetRecord>,
    /// The pin, exception and code records.
    records: Vec<Record>,
    /// The chip hooks tracing holds.
    hooks: Hooks,
    /// Each net's observers, by net, in the order attached: outside
    /// the snapshot, which they never change.
    observers: Vec<Vec<Attached>>,
    /// The next observer's id.
    next_observer: u64,
    /// The control handle's shared state: outside the snapshot.
    control: BoardControl,
}

impl<C: Chip + fmt::Debug> fmt::Debug for Board<C> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Board")
            .field("chip", &self.chip)
            .field("peripherals", &self.peripherals)
            .field("now", &self.now)
            .finish_non_exhaustive()
    }
}

/// The handle by which the host asks the board's run to stop, as a
/// peripheral's [`Drive::stop`] does: after the instruction running, or before
/// the next quantum.
///
/// For a host whose peripheral fails in a call with no
/// [`Drive`] (`next_event`, `interrupt_request`).  It holds the board
/// weakly, so a peripheral may keep it; once the board is gone it does
/// nothing.
///
/// [`Drive::stop`]: crate::peripheral::Drive::stop
/// [`Drive`]: crate::peripheral::Drive
#[derive(Clone)]
pub struct BoardStopRequest(Weak<Shared>);

impl BoardStopRequest {
    /// Asks the run to stop.
    pub fn request(&self) {
        if let Some(shared) = self.0.upgrade() {
            shared.stop.set(true);
            shared.chip_stop.request();
        }
    }
}

impl fmt::Debug for BoardStopRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BoardStopRequest")
            .finish_non_exhaustive()
    }
}

impl<C: Chip> Board<C> {
    /// The board file at `path`, loaded with `registry`'s types.
    ///
    /// # Errors
    ///
    /// What [`Description::read`] and [`Board::load`] fail with.
    pub fn read(
        path: &Path,
        options: &Options,
        registry: &Registry,
        environment: Environment,
    ) -> Result<Self, LoadError> {
        let description = Description::read(path, options)?;
        Self::load(&description, registry, environment)
    }

    /// The board `description` says, its peripherals built from `registry`,
    /// through the chip's power-on reset.
    ///
    /// # Errors
    ///
    /// A peripheral whose type `registry` lacks ([`LoadError::Type`]), whose
    /// parameters its constructor refuses or leaves unread
    /// ([`LoadError::Parameter`]) or whose ports repeat a name
    /// ([`LoadError::Port`]); a net endpoint no peripheral has, or of
    /// another type or direction than its net needs
    /// ([`LoadError::Endpoint`], [`LoadError::Kind`],
    /// [`LoadError::Direction`]); or wiring the chip refuses
    /// ([`LoadError::Build`]).
    pub fn load(
        description: &Description,
        registry: &Registry,
        environment: Environment,
    ) -> Result<Self, LoadError> {
        let environment = Environment {
            board_dir: description.directory.clone(),
            ..environment
        };
        let mut cells: Vec<PeripheralCell> = Vec::new();
        let mut peripherals = Vec::new();
        for spec in &description.peripherals {
            let kind = registry.get(&spec.kind).ok_or_else(|| LoadError::Type {
                peripheral: spec.name.clone(),
                kind: spec.kind.clone(),
            })?;
            let parameters = Parameters::new(spec.parameters.clone());
            let context = Context {
                name: &spec.name,
                parameters: &parameters,
                environment: &environment,
            };
            let parameter_error = |error| LoadError::Parameter {
                peripheral: spec.name.clone(),
                error,
            };
            let constructed = kind.construct(&context).map_err(parameter_error)?;
            if let Some(key) = parameters.unread() {
                return Err(parameter_error(crate::registry::ParameterError::Unknown(
                    key,
                )));
            }
            if let Some(port) = repeated_port(&constructed.ports) {
                return Err(LoadError::Port {
                    peripheral: spec.name.clone(),
                    port: port.to_owned(),
                });
            }
            cells.push(Rc::new(RefCell::new(constructed.peripheral)));
            peripherals.push(PeripheralInfo {
                name: spec.name.clone(),
                marking: spec.marking.clone(),
                ports: constructed.ports,
            });
        }
        let nets = resolve_nets::<C::Pin>(description, &peripherals)?;
        let mut analog_inputs: Vec<Vec<Option<usize>>> = peripherals
            .iter()
            .map(|peripheral| vec![None; peripheral.ports.len()])
            .collect();
        let mut net_drivers = vec![None; nets.len()];
        for (index, net) in nets.iter().enumerate() {
            for sink in &net.sinks {
                if let Sink::Peripheral(peripheral, port) = *sink
                    && peripherals[peripheral].ports[port].kind == Kind::Analog
                {
                    analog_inputs[peripheral][port] = Some(index);
                }
            }
            if let [Source::Peripheral(peripheral, port)] = net.drivers[..] {
                net_drivers[index] = Some((peripheral, port));
            }
        }
        let section = description.chip_section::<C::Section>(C::TYPE)?;
        let mut builder = C::builder(&section)?;
        let shared = Rc::new(Shared {
            base: Cell::new(TimeBase::new(
                Cycles::ZERO,
                Time::ZERO,
                builder.system_clock(),
            )),
            peripherals: cells,
            analog_inputs,
            net_drivers,
            last: RefCell::new(vec![None; nets.len()]),
            outbox: RefCell::default(),
            writes: RefCell::default(),
            diverted: Cell::new(false),
            stop: Cell::new(false),
            chip_stop: builder.stop_request(),
            requests: builder.requests(),
            quantum_end: Cell::new(None),
            trace_bus_cycles: Cell::new(false),
            trace_qspi: Cell::new(false),
            pc: Rc::default(),
            records: RefCell::default(),
            memories: RefCell::default(),
            qspi: RefCell::default(),
            pcs_sensed: Cell::new(false),
        });
        let mut memories = BTreeMap::new();
        for memory in &description.memories {
            let length = u32::try_from(memory.bytes.len()).map_err(|_| LoadError::Size {
                memory: memory.name.clone(),
                reason: "more bytes than ADDR[23:0] reach".to_owned(),
            })?;
            let place = match memory.base {
                Some(base) => MemoryPlace::Fixed {
                    base,
                    port: memory.timing.0,
                    termination: memory.timing.1,
                },
                None => MemoryPlace::Selects(&memory.pins),
            };
            let plan = MemoryPlan {
                bytes: memory.bytes.clone(),
                writable: memory.writable,
                place,
                reload: memory.reload,
            };
            let router = MemoryRouter {
                shared: Rc::clone(&shared),
                memory: shared.memories.borrow().len(),
            };
            let id = builder.memory(plan, router)?;
            memories.insert(memory.name.clone(), id);
            shared.memories.borrow_mut().push(SharedMemory {
                name: memory.name.clone(),
                length,
                pins: memory.pins.clone(),
                taker: None,
            });
        }
        Self::wire(description, &mut builder, &shared)?;
        for peripheral in 0..shared.peripherals.len() {
            shared.call(peripheral, Time::ZERO, |peripheral, drive| {
                peripheral.power_on(drive)
            });
        }
        let mut chip = builder.build();
        shared
            .base
            .set(shared.base.get().moved(Cycles::ZERO, chip.system_clock()));
        let mut sources: BTreeMap<Source<C::Pin>, Vec<usize>> = BTreeMap::new();
        for (index, net) in nets.iter().enumerate() {
            for driver in &net.drivers {
                sources.entry(driver.clone()).or_default().push(index);
            }
        }
        let on_nets: Vec<C::Pin> = sources
            .keys()
            .filter_map(|source| match source {
                Source::Chip(pin) => Some(*pin),
                _ => None,
            })
            .collect();
        chip.watch_outputs(&on_nets);
        let outputs = on_nets
            .iter()
            .filter(|pin| pin.kind() == Kind::Logic)
            .map(|&pin| (pin, true))
            .collect();
        let observers = nets.iter().map(|_| Vec::new()).collect();
        let mut endpoints = Endpoints::default();
        for (source, driven) in &sources {
            if let Source::External(name) = source {
                endpoints.0.insert(
                    name.clone(),
                    driven.iter().map(|&net| (net, nets[net].kind)).collect(),
                );
            }
        }
        let mut board = Self {
            chip,
            shared,
            peripherals,
            memories,
            nets,
            sources,
            queue: BTreeMap::new(),
            ahead: Vec::new(),
            sequence: 0,
            now: Time::ZERO,
            outputs,
            clocks: BTreeMap::new(),
            emitted: Vec::new(),
            reset_in: true,
            received: BTreeMap::new(),
            events: Vec::new(),
            tracing: BoardTracing::default(),
            net_trace: Vec::new(),
            records: Vec::new(),
            hooks: Hooks::default(),
            observers,
            next_observer: 0,
            control: BoardControl::new(endpoints),
        };
        board.stop_at_outputs();
        board.power_on();
        board.tell_system_clock();
        Ok(board)
    }

    /// Wires the peripherals' attachments to the chip.
    fn wire(
        description: &Description,
        builder: &mut C::Builder,
        shared: &Rc<Shared>,
    ) -> Result<(), LoadError> {
        let mut selects: BTreeMap<ChipSelect, Routes> = BTreeMap::new();
        let mut outputs: Vec<Vec<Routes>> = description
            .decoders
            .iter()
            .map(|decoder| vec![Routes::default(); decoder.outputs.len()])
            .collect();
        let mut targets = Vec::new();
        for (peripheral, entry) in description.peripherals.iter().enumerate() {
            builder.unwired(
                PeripheralPins {
                    shared: Rc::clone(shared),
                    peripheral,
                },
                entry.reset,
                entry.irq,
            );
            for (attachment, spec) in entry.attachments.iter().enumerate() {
                let route = Route {
                    peripheral,
                    attachment,
                    role: spec.role,
                    lane: spec.lane,
                    lines: spec.lines.clone(),
                    offset: spec.offset,
                };
                match spec.target {
                    Target::Select(select) => selects.entry(select).or_default().0.push(route),
                    Target::Decoder { decoder, output } => {
                        outputs[decoder][output as usize].0.push(route);
                    }
                    Target::Qspi { code, mask } => targets.push(QspiTarget {
                        peripheral,
                        attachment,
                        code,
                        mask,
                    }),
                }
            }
        }
        // the DSACK pins the peripherals on a router drive, where they agree
        let dsack = |routes: &mut dyn Iterator<Item = &Route>| {
            let mut stated =
                routes.filter_map(|route| description.peripherals[route.peripheral].dsack);
            let first = stated.next()?;
            stated.all(|other| other == first).then_some(first)
        };
        for (select, routes) in selects {
            let pins = dsack(&mut routes.0.iter());
            builder.peripheral(
                SelectRouter {
                    shared: Rc::clone(shared),
                    routes,
                },
                select,
                pins,
            )?;
        }
        for (decoder, routes) in description.decoders.iter().zip(outputs) {
            let pins = dsack(&mut routes.iter().flat_map(|routes| &routes.0));
            builder.peripheral(
                DecoderRouter {
                    shared: Rc::clone(shared),
                    inputs: decoder.inputs.clone(),
                    outputs: decoder.outputs.clone(),
                    routes,
                    bus_error: decoder.undecoded == Undecoded::BusError,
                },
                decoder.select,
                pins,
            )?;
        }
        if !targets.is_empty() {
            let deferrable = targets.iter().all(|target| {
                shared.peripherals[target.peripheral]
                    .borrow()
                    .qspi_deferrable()
            });
            *shared.qspi.borrow_mut() = Some(QspiState::new(PCS_IDLE, targets.len()));
            builder.qspi(QspiRouter {
                shared: Rc::clone(shared),
                targets,
                deferrable,
            })?;
        }
        Ok(())
    }

    /// Tells the chip which outputs end its run ([`sensed`]), and the
    /// QSPI's router whether a PCS pin is among them: as the board is
    /// built and as observers come and go.
    fn stop_at_outputs(&mut self) {
        let sensed = sensed(&self.nets, &self.sources, &self.observers, C::RESET);
        self.shared
            .pcs_sensed
            .set(sensed.iter().any(|pin| pin.pcs()));
        self.chip.stop_at_outputs(&sensed);
    }

    /// The board out of power-on: the chip's outputs' first levels, its
    /// clocks, and what the peripherals drove at their reset.
    fn power_on(&mut self) {
        let first: Vec<C::Pin> = self.outputs.keys().copied().collect();
        for pin in first {
            self.drive(&Source::Chip(pin), Time::ZERO, Signal::Level(true));
        }
        self.drive_clocks();
        self.collect();
    }

    /// Runs the board to `until`, or until the CPU halts, a hook stops
    /// it, or the host or a peripheral asks it to stop.  At each sync
    /// point it takes what its [`control`](Self::control) holds.  A
    /// board whose CPU is already halted runs to `until` as time passes
    /// without the CPU ([`Chip::run_for`]), ending [`Stop::Halted`] if it
    /// still is.
    ///
    /// # Errors
    ///
    /// A peripheral that stalls the board ([`RunError::Stalled`]).
    pub fn run_until(&mut self, until: Time) -> Result<BoardRun, RunError> {
        loop {
            self.take_control();
            self.settle()?;
            if self.take_stop() {
                return Ok(self.outcome(Stop::Requested));
            }
            if self.now >= until {
                let stop = self.chip.halted().map_or(Stop::Budget, Stop::Halted);
                return Ok(self.outcome(stop));
            }
            let next = self.next_event().map_or(until, |next| next.min(until));
            // a peripheral's `next_event` may have asked (`BoardStopRequest`)
            if self.take_stop() {
                return Ok(self.outcome(Stop::Requested));
            }
            // RESET held low: the chip stays in reset and runs nothing,
            // while board time and the peripherals go on (SIMRM 8.3.1)
            if !self.reset_in {
                self.collect();
                self.now = self.now.max(next);
                continue;
            }
            let target = self.shared.base.get().cycles(next);
            let halted = self.chip.halted().is_some();
            let stop = if target > self.chip.now() {
                self.shared.quantum_end.set(Some(next));
                let stop = self.chip.run_for(self.chip.now().until(target)).stop;
                self.shared.quantum_end.set(None);
                stop
            } else {
                Stop::Budget
            };
            self.collect();
            let stop = match stop {
                // halted throughout: as a budget spent
                Stop::Halted(_) if halted => Stop::Budget,
                other => other,
            };
            match stop {
                // short of the target: a peripheral asked within the run
                // (`Shared::call`), or the clock's frequency changed and
                // board time follows the chip's clocks at the new rate
                Stop::Budget | Stop::Stopped if self.chip.now() < target => self.catch_up(next),
                Stop::Budget | Stop::Stopped => self.now = self.now.max(next),
                Stop::Hook(id) if self.own_hook(id) => self.catch_up(next),
                _ => {
                    self.catch_up(next);
                    self.settle()?;
                    self.take_stop();
                    return Ok(self.outcome(stop));
                }
            }
        }
    }

    /// Whether a peripheral or the host asked the run to stop; withdraws the
    /// request, the chip's with it, so the next run does not stop for
    /// it again.
    fn take_stop(&self) -> bool {
        self.shared.chip_stop.withdraw();
        self.shared.stop.take()
    }

    /// Board time to the chip's, but no later than `next`, where the
    /// quantum was to end: an instruction that ends past it, the run
    /// stopping there, leaves board time at `next`, as a quantum that
    /// runs out does.
    fn catch_up(&mut self, next: Time) {
        self.now = self.now.max(self.shared.time(self.chip.now()).min(next));
    }

    /// Whether hook `id` is tracing's: records what it stopped for and
    /// says so, for the run to go on.
    fn own_hook(&mut self, id: HookId) -> bool {
        let cycle = self.chip.now();
        if Some(id) == self.hooks.exception {
            let entered = mem::take(&mut *self.hooks.entered.borrow_mut());
            for entry in entered {
                let memory = &self.chip.cpu().memory;
                let sr = memory
                    .peek(Space::Supervisor, Address(entry.stack), Size::Word)
                    .map(|sr| sr as u16);
                self.records.push(Record::Exception(trace::ExceptionRecord {
                    cycle,
                    vector: entry.vector.0,
                    pc: entry.pc.0,
                    sr,
                    level: entry.level.map(|level| level.get()),
                }));
            }
            return true;
        }
        if self.hooks.watches.contains(&id) {
            let cpu = self.chip.cpu();
            let registers = &cpu.registers;
            let a7 = registers.address(7);
            let space = Space::of(registers.data_space());
            let stack = array::from_fn(|index| {
                let at = Address(a7).offset(4 * index as u32);
                cpu.memory.peek(space, at, Size::Long)
            });
            self.records.push(Record::Code(trace::CodeRecord {
                cycle,
                pc: registers.pc,
                sr: registers.sr(),
                data: registers.data,
                address: array::from_fn(|index| registers.address(index)),
                stack,
            }));
            return true;
        }
        false
    }

    /// Traces each time the instruction at `pc` is about to run: the
    /// run stops there, records it and goes on.
    pub fn watch_code(&mut self, pc: u32) -> HookId {
        let id = self
            .chip
            .cpu_control()
            .add_code_hook(Address(pc)..=Address(pc), |_| ControlFlow::Break(()));
        self.hooks.watches.insert(id);
        id
    }

    /// Where the CPU is now, as a `position` record.
    #[must_use]
    pub fn position(&self) -> Record {
        let registers = &self.chip.cpu().registers;
        Record::Position(trace::PositionRecord {
            cycle: self.chip.now(),
            pc: registers.pc,
            sr: registers.sr(),
            vbr: registers.vbr,
            a7: registers.address(7),
            instructions: self.chip.instructions(),
        })
    }

    /// The run's end after `outcome`, as a `stopped` record.
    #[must_use]
    pub fn stopped(&self, outcome: BoardRun) -> Record {
        Record::Stopped(trace::StoppedRecord {
            cycle: self.chip.now(),
            instructions: self.chip.instructions(),
            reason: outcome.stop,
        })
    }

    /// The handle by which the host asks the run to stop.
    #[must_use]
    pub fn stop_request(&self) -> BoardStopRequest {
        BoardStopRequest(Rc::downgrade(&self.shared))
    }

    /// The board's thread-safe control handle, for a front end on
    /// another thread: a stop, inputs and the pacing, taken by the run
    /// at its sync points.  Every call gives a handle to the one
    /// control.
    #[must_use]
    pub fn control(&self) -> BoardControl {
        self.control.clone()
    }

    /// Runs the board for `picoseconds` from now.
    ///
    /// # Errors
    ///
    /// What [`Board::run_until`] fails with.
    pub fn run_for(&mut self, picoseconds: u64) -> Result<BoardRun, RunError> {
        self.run_until(self.now + picoseconds)
    }

    /// Runs the board to `until` as [`run_until`](Self::run_until) does, in quanta
    /// of `quantum` picoseconds of board time, calling `before` before
    /// each: a host's loop, for its Ctrl-C checks or its pacing.  A
    /// series of runs to rising times is one long run, so the quanta
    /// change nothing the board does.  A board halted before a quantum
    /// runs it to its end with no instruction, so its halt goes on to
    /// the next quantum.  `before` returning `Break` ends the run where
    /// it is, [`Stop::Requested`].  A quantum is never empty, so the
    /// loop always comes to `until`:
    ///
    /// ```compile_fail
    /// # use std::ops::ControlFlow;
    /// # use mc683xx_board::board::Board;
    /// # use mc683xx_board::time::Time;
    /// # use mc683xx_mc68331::chip::Mc68331;
    /// # fn run(board: &mut Board<Mc68331>) {
    /// board.run_until_quantized(Time(1), 0, |_| ControlFlow::Continue(()));
    /// # }
    /// ```
    ///
    /// # Errors
    ///
    /// As [`run_until`](Self::run_until).
    pub fn run_until_quantized(
        &mut self,
        until: Time,
        quantum: NonZeroU64,
        mut before: impl FnMut(&mut Self) -> ControlFlow<()>,
    ) -> Result<BoardRun, RunError> {
        loop {
            if before(self).is_break() {
                return Ok(self.outcome(Stop::Requested));
            }
            let target = Time(self.now.0.saturating_add(quantum.get())).min(until);
            let halted = self.chip.halted().is_some();
            let outcome = self.run_until(target)?;
            let quantized =
                outcome.stop == Stop::Budget || (halted && matches!(outcome.stop, Stop::Halted(_)));
            if !quantized || self.now >= until {
                return Ok(outcome);
            }
        }
    }

    /// Runs the board to `until` as [`run_until_quantized`](Self::run_until_quantized)
    /// does, in quanta of `quantum` picoseconds
    /// ([`pacing::QUANTUM`](crate::pacing::QUANTUM) is a good one), paced
    /// by its control's [`Pacing`](crate::pacing::Pacing), read before
    /// each quantum: before each, it sleeps until board time is no longer
    /// ahead of wall time times the pacing's factor, waking for a stop
    /// or a change of pacing.  Wall time decides only how long it
    /// sleeps, so the run is the same whatever the pacing.  A run that
    /// reaches `until` waits once more after its last quantum, so it
    /// returns no earlier than wall time says, and short paced runs in a
    /// loop are paced too.  A stop through the control ends it at the
    /// next sync point, without waiting out the sleep.
    ///
    /// # Errors
    ///
    /// As [`run_until`](Self::run_until).
    pub fn run_until_paced(
        &mut self,
        until: Time,
        quantum: NonZeroU64,
    ) -> Result<BoardRun, RunError> {
        let control = self.control.clone();
        let mut pacer = Pacer::new();
        let outcome = self.run_until_quantized(until, quantum, |board| {
            pacer.wait(board.now, &control);
            ControlFlow::Continue(())
        })?;
        if self.now >= until {
            pacer.wait(self.now, &control);
        }
        Ok(outcome)
    }

    /// The outcome of a run that ends now.
    const fn outcome(&self, stop: Stop) -> BoardRun {
        BoardRun {
            stop,
            now: self.now,
        }
    }

    /// The earliest change waiting or peripheral's next event.
    fn next_event(&self) -> Option<Time> {
        let change = self.queue.keys().next().map(|&(at, _)| at);
        let peripheral = self
            .shared
            .peripherals
            .iter()
            .filter_map(|peripheral| peripheral.borrow().next_event())
            .min();
        change.into_iter().chain(peripheral).min()
    }

    /// Handles everything due by now, in time order: changes before
    /// peripheral events at one time, peripherals in file order.  A
    /// peripheral advanced `STALL_REPEATS` times running to one time, its
    /// next event never past it, stalls the board.
    fn settle(&mut self) -> Result<(), RunError> {
        // each peripheral's last advance's time and how many times running
        let mut repeats: Vec<(Time, u32)> = vec![(Time::ZERO, 0); self.peripherals.len()];
        loop {
            self.flush();
            let change = self
                .queue
                .first_key_value()
                .map(|(&key, _)| key)
                .filter(|&(at, _)| at <= self.now);
            let peripheral = self
                .shared
                .peripherals
                .iter()
                .enumerate()
                .filter_map(|(index, peripheral)| {
                    peripheral.borrow().next_event().map(|at| (at, index))
                })
                .filter(|&(at, _)| at <= self.now)
                .min();
            match (change, peripheral) {
                (Some(key), peripheral) if peripheral.is_none_or(|(at, _)| key.0 <= at) => {
                    let change = self.queue.remove(&key).expect("the first change");
                    self.process(key.0, change);
                }
                (Some(_) | None, Some((at, index))) => {
                    let (last, count) = repeats[index];
                    let count = if last == at { count + 1 } else { 1 };
                    if count > STALL_REPEATS {
                        return Err(RunError::Stalled {
                            peripheral: self.peripherals[index].name.clone(),
                            at,
                            repeats: STALL_REPEATS,
                        });
                    }
                    repeats[index] = (at, count);
                    self.shared
                        .call(index, at, |peripheral, drive| peripheral.advance(at, drive));
                }
                (_, None) => return Ok(()),
            }
        }
    }

    /// Takes what peripherals drove into the nets, and makes the memory
    /// writes and diversions they asked for: before every quantum and
    /// before a run returns, so the CPU and a snapshot see them however
    /// the host cuts the run.
    fn flush(&mut self) {
        let driven = mem::take(&mut *self.shared.outbox.borrow_mut());
        for (peripheral, driven) in driven {
            self.drive(
                &Source::Peripheral(peripheral, driven.port),
                driven.at,
                driven.signal,
            );
        }
        for write in mem::take(&mut *self.shared.writes.borrow_mut()) {
            if let Some(&id) = self.memories.get(&write.memory) {
                let memory = self.chip.memory_mut(id);
                let start = write.offset.min(memory.len());
                let end = (start + write.bytes.len()).min(memory.len());
                memory[start..end].copy_from_slice(&write.bytes[..end - start]);
            }
        }
        if self.shared.diverted.take() {
            self.chip.apply_diversions();
        }
    }

    /// `source` drives `signal` from `at`: a change waits for its time,
    /// but a [`ChipPin::scheduled`] input on a net of one driver (a GPT
    /// input) is scheduled into the chip at once, so a peripheral's edge
    /// driven ahead is captured at its clock.
    fn drive(&mut self, source: &Source<C::Pin>, at: Time, signal: Signal) {
        let Some(nets) = self.sources.get(source).cloned() else {
            return;
        };
        for net in nets {
            self.drive_net(net, source, at, signal);
        }
    }

    /// `source` drives `signal` onto `net` from `at`, as `drive` does.
    fn drive_net(&mut self, net: usize, source: &Source<C::Pin>, at: Time, signal: Signal) {
        let state = &self.nets[net];
        let driver = state
            .drivers
            .iter()
            .position(|driver| driver == source)
            .expect("a driver of its net");
        if !state.wired {
            let ahead: Vec<C::Pin> = state
                .sinks
                .iter()
                .filter_map(|sink| match sink {
                    Sink::Chip(pin) if pin.scheduled() => Some(*pin),
                    _ => None,
                })
                .collect();
            for pin in ahead {
                self.schedule(pin, signal, at);
            }
        }
        self.queue.insert(
            (at, self.sequence),
            Change {
                net,
                driver,
                signal,
            },
        );
        self.sequence += 1;
    }

    /// A change at its time: resolved on a wired net, then to each sink.
    fn process(&mut self, at: Time, change: Change) {
        let state = &mut self.nets[change.net];
        let (signal, changed) = if state.wired {
            let Signal::Level(level) = change.signal else {
                return;
            };
            state.driven[change.driver] = level;
            let resolved = state.driven.iter().all(|&level| level);
            let changed = resolved != state.level;
            state.level = resolved;
            (Signal::Level(resolved), changed)
        } else {
            (change.signal, true)
        };
        let wired = state.wired;
        // RESET as the chip senses it on a wired net: its own driver
        // left out, so its reset output does not reset it again
        // (SIMRM 8.3.1); a change of another driver's may not
        // change the net's level
        let reset_in = state.level_without(&Source::Chip(C::RESET));
        let sinks = state.sinks.clone();
        if changed {
            if self.tracing.nets {
                self.net_trace.push(NetRecord {
                    at,
                    net: state.name.clone(),
                    signal,
                });
            }
            self.shared.last.borrow_mut()[change.net] = Some(signal);
        }
        for sink in sinks {
            match sink {
                Sink::Chip(pin) if wired && pin == C::RESET => {
                    if changed {
                        self.trace_pin(pin, signal, at);
                    }
                    self.apply(pin, Signal::Level(reset_in), at);
                }
                _ if !changed => {}
                Sink::Peripheral(peripheral, port) => {
                    self.shared.call(peripheral, at, |peripheral, drive| {
                        peripheral.input(port, signal, at, drive);
                    });
                }
                Sink::Chip(pin) => {
                    self.trace_pin(pin, signal, at);
                    if wired || !pin.scheduled() {
                        self.apply(pin, signal, at);
                    }
                }
                Sink::External(name) => self.received.entry(name).or_default().push((at, signal)),
            }
        }
        if changed {
            let name = &self.nets[change.net].name;
            for attached in &mut self.observers[change.net] {
                attached.observer.changed(name, at, signal);
            }
        }
    }

    /// `signal` on [`ChipPin::scheduled`] input `pin` from `at`, at its
    /// clock, kept until it comes in case the clocks move first.
    fn schedule(&mut self, pin: C::Pin, signal: Signal, at: Time) {
        let cycles = self.shared.base.get().cycles(at);
        self.chip.input(pin, signal, cycles);
        if cycles > self.chip.now() {
            self.ahead.push(Ahead {
                at,
                cycles,
                pin,
                signal,
            });
        }
    }

    /// The chip's clocks on board time from `base`: the levels the chip
    /// holds for later clocks scheduled again at their new clocks.
    fn move_base(&mut self, base: TimeBase) {
        let old = self.shared.base.replace(base);
        if old == base || self.ahead.is_empty() {
            return;
        }
        // those up to now come as they are withdrawn
        self.chip.unschedule_inputs();
        let now = self.chip.now();
        for ahead in mem::take(&mut self.ahead) {
            if ahead.cycles > now {
                self.schedule(ahead.pin, ahead.signal, ahead.at);
            }
        }
    }

    /// `signal` on chip input `pin` from `at`.
    fn apply(&mut self, pin: C::Pin, signal: Signal, at: Time) {
        if pin != C::RESET {
            let at_cycles = self.shared.base.get().cycles(at);
            self.chip.input(pin, signal, at_cycles);
            return;
        }
        let Signal::Level(level) = signal else {
            return;
        };
        // asserted, the chip resets and is held in reset; released, its
        // reset processing begins (SIMRM 8.3.1), its clocks on board
        // time from then, as none ran while it was held
        match (self.reset_in, level) {
            (true, false) => self.chip.reset(),
            (false, true) => {
                let base = self.shared.base.get();
                let cycles = self.chip.now();
                let at = at.max(base.time(cycles));
                self.move_base(TimeBase::new(cycles, at, base.frequency()));
                self.tell_system_clock();
                self.chip.reset();
            }
            _ => {}
        }
        self.reset_in = level;
    }

    /// The chip's outputs since the last quantum onto their nets, its
    /// events kept for the host, and the clock base moved to now.
    fn collect(&mut self) {
        let base = self.shared.base.get();
        // a level the board takes only where a net has the pin and it
        // changed, its last level kept as the chip emits
        let mut emitted = mem::take(&mut self.emitted);
        let outputs = &mut self.outputs;
        self.chip.collect(base, |at, pin, signal| {
            if let Signal::Level(level) = signal {
                match outputs.get_mut(&pin) {
                    Some(last) if *last != level => *last = level,
                    _ => return,
                }
            }
            emitted.push((at, pin, signal));
        });
        for &(at, pin, signal) in &emitted {
            if matches!(signal, Signal::Level(_)) {
                self.trace_pin(pin, signal, at);
            }
            self.drive(&Source::Chip(pin), at, signal);
        }
        emitted.clear();
        self.emitted = emitted;
        let reset = Source::Chip(C::RESET);
        let system_clock = self.chip.system_clock();
        for event in self.chip.take_events() {
            if let Some((low, high)) = C::reset_pulse(&event)
                && self.sources.contains_key(&reset)
            {
                // the release counts clocks of the clock after the reset
                // (MC68331UM 4.6.6), which a reset that moves it ends the
                // quantum at
                let after = base.moved(low, system_clock);
                self.drive(&reset, base.time(low), Signal::Level(false));
                self.drive(&reset, after.time(high), Signal::Level(true));
            }
            self.events.push(event);
        }
        let now = self.chip.now();
        self.ahead.retain(|ahead| ahead.cycles > now);
        self.move_base(base.moved(now, system_clock));
        if system_clock != base.frequency() {
            self.tell_system_clock();
        }
        let moved = self.clocks.iter().any(|(&pin, clock)| {
            self.chip
                .clock_output(pin)
                .is_some_and(|frequency| frequency != clock.frequency)
        });
        if moved {
            self.drive_clocks();
        }
    }

    /// Every peripheral learns the system clock's base
    /// ([`Peripheral::system_clock`]).
    ///
    /// [`Peripheral::system_clock`]: crate::peripheral::Peripheral::system_clock
    fn tell_system_clock(&self) {
        let base = self.shared.base.get();
        for peripheral in &self.shared.peripherals {
            peripheral.borrow_mut().system_clock(base);
        }
    }

    /// The chip's system clock on board time, as of the last quantum's
    /// end: where a peripheral converts between board time and the chip's
    /// clocks.
    #[must_use]
    pub fn system_clock(&self) -> TimeBase {
        self.shared.base.get()
    }

    /// Each clock output on a net onto it, from now: out of power-on a
    /// rising edge at power-on (the chip's choice of phase where its
    /// manual gives none), and from a change of its frequency on, at the
    /// new frequency, its phase carried across.
    fn drive_clocks(&mut self) {
        let at = self.shared.time(self.chip.now());
        let pins: Vec<C::Pin> = self
            .sources
            .keys()
            .filter_map(|source| match source {
                Source::Chip(pin) if pin.kind() == Kind::Clock => Some(*pin),
                _ => None,
            })
            .collect();
        for pin in pins {
            let Some(frequency) = self.chip.clock_output(pin) else {
                continue;
            };
            let clock = self.clocks.get(&pin).map_or_else(
                || Clock::new(frequency, at),
                |clock| clock.retuned(at, frequency),
            );
            self.clocks.insert(pin, clock);
            self.drive(&Source::Chip(pin), at, Signal::Clock(clock));
        }
    }

    /// A pin record of `signal` on `pin` at `at`, if pins are traced.
    fn trace_pin(&mut self, pin: C::Pin, signal: Signal, at: Time) {
        if self.tracing.pins
            && let Signal::Level(level) = signal
        {
            self.records.push(Record::Pin(trace::PinRecord {
                cycle: self.shared.base.get().cycles(at),
                time: at,
                pin: pin.name(),
                level,
            }));
        }
    }

    /// Board time.
    #[must_use]
    pub const fn now(&self) -> Time {
        self.now
    }

    /// The chip.
    #[must_use]
    pub const fn chip(&self) -> &C {
        &self.chip
    }

    /// The chip, to change its registers, hooks and tracing.  The board
    /// takes the chip's output changes and its events after each
    /// quantum ([`Chip::collect`], [`Chip::take_events`]): read them from
    /// the nets and `take_events`.
    pub const fn chip_mut(&mut self) -> &mut C {
        &mut self.chip
    }

    /// The peripheral `name`, if there is one and it is a `P`: a host's
    /// own peripheral's state, by its type.
    #[must_use]
    pub fn peripheral<P: Any>(&self, name: &str) -> Option<Ref<'_, P>> {
        let cell = &self.shared.peripherals[self.peripheral_index(name)?];
        Ref::filter_map(cell.borrow(), |peripheral| {
            let peripheral: &dyn Any = peripheral.as_ref();
            peripheral.downcast_ref::<P>()
        })
        .ok()
    }

    /// The peripheral `name`, to change, if there is one and it is a `P`.
    pub fn peripheral_mut<P: Any>(&mut self, name: &str) -> Option<RefMut<'_, P>> {
        let cell = &self.shared.peripherals[self.peripheral_index(name)?];
        RefMut::filter_map(cell.borrow_mut(), |peripheral| {
            let peripheral: &mut dyn Any = peripheral.as_mut();
            peripheral.downcast_mut::<P>()
        })
        .ok()
    }

    /// The index of the peripheral `name`.
    fn peripheral_index(&self, name: &str) -> Option<usize> {
        self.peripherals
            .iter()
            .position(|peripheral| peripheral.name == name)
    }

    /// What the peripheral `name` shows a front end
    /// ([`Peripheral::inspect`]), if there is one.
    ///
    /// [`Peripheral::inspect`]: crate::peripheral::Peripheral::inspect
    #[must_use]
    pub fn inspect(&self, name: &str) -> Option<Inspection> {
        let cell = &self.shared.peripherals[self.peripheral_index(name)?];
        Some(cell.borrow().inspect())
    }

    /// The marking of the peripheral `name`, if its peripheral gives one.
    #[must_use]
    pub fn marking(&self, name: &str) -> Option<&str> {
        self.peripherals
            .iter()
            .find(|peripheral| peripheral.name == name)?
            .marking
            .as_deref()
    }

    /// The memory `name`'s bytes, if there is one.
    #[must_use]
    pub fn memory(&self, name: &str) -> Option<&[u8]> {
        self.memories.get(name).map(|&id| self.chip.memory(id))
    }

    /// The memory `name`'s bytes, to change, if there is one.
    pub fn memory_mut(&mut self, name: &str) -> Option<&mut [u8]> {
        let id = *self.memories.get(name)?;
        Some(self.chip.memory_mut(id))
    }

    /// The run-time endpoints the nets name, by name and then type:
    /// each with its nets' type and which way it faces, `Out` if it
    /// drives a net (the host sends), `In` if it is driven (the host
    /// receives), `InOut` if both.  A name on nets of two types is two
    /// endpoints.
    #[must_use]
    pub fn externals(&self) -> Vec<External> {
        let mut externals: Vec<External> = Vec::new();
        let mut add = |name: &str, kind: Kind, drives: bool| {
            let direction = if drives {
                Direction::Out
            } else {
                Direction::In
            };
            match externals
                .iter_mut()
                .find(|external| external.name == name && external.kind == kind)
            {
                Some(external) if external.direction != direction => {
                    external.direction = Direction::InOut;
                }
                Some(_) => {}
                None => externals.push(External {
                    name: name.to_owned(),
                    kind,
                    direction,
                }),
            }
        };
        for net in &self.nets {
            for source in &net.drivers {
                if let Source::External(name) = source {
                    add(name, net.kind, true);
                }
            }
            for sink in &net.sinks {
                if let Sink::External(name) = sink {
                    add(name, net.kind, false);
                }
            }
        }
        externals.sort_by(|a, b| (&a.name, a.kind.to_string()).cmp(&(&b.name, b.kind.to_string())));
        externals
    }

    /// The run-time endpoint `name` drives `signal` now, onto each net
    /// it drives whose type carries it, after what the
    /// [`control`](Self::control) holds is taken: so an input queued
    /// through it before lands before this one.
    ///
    /// # Errors
    ///
    /// Driving nothing, if it drives no net ([`SendError::Endpoint`]), or
    /// none that carries `signal` ([`SendError::Kind`]).
    pub fn send(&mut self, name: &str, signal: Signal) -> Result<(), SendError> {
        self.take_control();
        self.drive_endpoint(name, signal)
    }

    /// The run-time endpoint `name` drives `signal` now: [`send`](Self::send)
    /// without the control.
    fn drive_endpoint(&mut self, name: &str, signal: Signal) -> Result<(), SendError> {
        let source = Source::External(name.to_owned());
        let carrying = self.control.endpoints().carrying(name, &signal)?;
        let at = self.now;
        for net in carrying {
            self.drive_net(net, &source, at, signal);
        }
        Ok(())
    }

    /// What the control holds, taken at a sync point: its inputs sent,
    /// in the order queued, and its stop asked.
    fn take_control(&mut self) {
        let Some((stop, inputs)) = self.control.take() else {
            return;
        };
        for (name, signal) in inputs {
            // checked as it was queued, against the same endpoints
            let sent = self.drive_endpoint(&name, signal);
            debug_assert!(sent.is_ok(), "{name}: {sent:?}");
        }
        if stop {
            self.shared.stop.set(true);
        }
    }

    /// What reached the run-time endpoint `name` since the last call,
    /// with each value's time.
    ///
    /// # Errors
    ///
    /// Taking nothing, if no net drives it ([`ReceiveError::Endpoint`]),
    /// so a typo is not silently an endpoint that never receives.
    pub fn take_received(&mut self, name: &str) -> Result<Vec<(Time, Signal)>, ReceiveError> {
        let driven = self.nets.iter().any(|net| {
            net.sinks
                .iter()
                .any(|sink| matches!(sink, Sink::External(sink) if sink == name))
        });
        if !driven {
            return Err(ReceiveError::Endpoint(name.to_owned()));
        }
        Ok(self.received.remove(name).unwrap_or_default())
    }

    /// The nets, in file order: each one's name and type.
    pub fn nets(&self) -> impl Iterator<Item = (&str, Kind)> {
        self.nets.iter().map(|net| (net.name.as_str(), net.kind))
    }

    /// The index of the net `name`.
    fn net_index(&self, name: &str) -> Result<usize, NetError> {
        self.nets
            .iter()
            .position(|net| net.name == name)
            .ok_or_else(|| NetError::Unknown(name.to_owned()))
    }

    /// The value the net `name` last carried, [`None`] before its first
    /// change: what an observer attached now starts from.
    ///
    /// # Errors
    ///
    /// A board with no net `name` ([`NetError::Unknown`]).
    pub fn net_value(&self, name: &str) -> Result<Option<Signal>, NetError> {
        let net = self.net_index(name)?;
        Ok(self.shared.last.borrow()[net])
    }

    /// Attaches `observer` to the net `name`.
    ///
    /// From now on it is told each change the net carries, at its board
    /// time, after the net's sinks, in the order they see them
    /// ([`observe::Observer`]), the changes already waiting included (the
    /// power-on levels, before the first run).  It is not told the net's
    /// present value (`net_value`).  Observers are outside the snapshot: a
    /// restore keeps them attached, and tells each the net's restored
    /// value ([`Observer::restored`]).  An observer on
    /// a net costs that net's changes a call each; other nets, nothing.
    /// On a net a chip output drives, the output's change ends the
    /// chip's run, as a peripheral sink's does, so a stop the observer
    /// asks ([`BoardStopRequest`]) is taken at the change.
    ///
    /// # Errors
    ///
    /// A board with no net `name` ([`NetError::Unknown`]).
    ///
    /// [`observe::Observer`]: crate::observe::Observer
    pub fn observe(&mut self, name: &str, observer: impl Observer) -> Result<ObserverId, NetError> {
        let net = self.net_index(name)?;
        let id = ObserverId(self.next_observer);
        self.next_observer += 1;
        self.observers[net].push(Attached {
            id,
            observer: Box::new(observer),
        });
        if self.observers[net].len() == 1 {
            self.stop_at_outputs();
        }
        Ok(id)
    }

    /// Detaches the observer `id`, giving it back, or [`None`] if it is
    /// not attached.
    pub fn unobserve(&mut self, id: ObserverId) -> Option<Box<dyn Observer>> {
        let (observer, unobserved) = self.observers.iter_mut().find_map(|attached| {
            let index = attached.iter().position(|attached| attached.id == id)?;
            Some((attached.remove(index).observer, attached.is_empty()))
        })?;
        if unobserved {
            self.stop_at_outputs();
        }
        Some(observer)
    }

    /// The chip's events since the last call.
    pub fn take_events(&mut self) -> Vec<C::Event> {
        mem::take(&mut self.events)
    }

    /// What the board traces from now on.
    pub fn set_tracing(&mut self, tracing: BoardTracing) {
        self.tracing = tracing;
        self.shared.trace_bus_cycles.set(tracing.bus_cycles);
        self.shared.trace_qspi.set(tracing.qspi);
        let mut cpu = self.chip.cpu_control();
        match (tracing.bus_cycles, self.hooks.pc) {
            (true, None) => {
                let pc = Rc::clone(&self.shared.pc);
                self.hooks.pc = Some(cpu.add_code_hook(
                    Address(0)..=Address(u32::MAX),
                    move |instruction| {
                        pc.set(instruction.pc.0);
                        ControlFlow::Continue(())
                    },
                ));
            }
            (false, Some(id)) => {
                cpu.remove_hook(id);
                self.hooks.pc = None;
            }
            _ => {}
        }
        match (tracing.exceptions, self.hooks.exception) {
            (true, None) => {
                let entered = Rc::clone(&self.hooks.entered);
                self.hooks.exception = Some(cpu.add_exception_hook(move |entry| {
                    entered.borrow_mut().push(*entry);
                    ControlFlow::Break(())
                }));
            }
            (false, Some(id)) => {
                cpu.remove_hook(id);
                self.hooks.exception = None;
            }
            _ => {}
        }
    }

    /// What was traced since the last call, the records in cycle order.
    pub fn take_trace(&mut self) -> Trace {
        let mut records = mem::take(&mut self.records);
        records.append(&mut self.shared.records.borrow_mut());
        records.sort_by_key(Record::cycle);
        Trace {
            records,
            nets: mem::take(&mut self.net_trace),
        }
    }

    /// The whole board's state: the chip's snapshot, every peripheral's, and
    /// the board's time, nets and changes waiting.
    ///
    /// # Errors
    ///
    /// A peripheral that cannot save, named ([`StateError::Peripheral`]),
    /// or a chip that cannot.
    pub fn save(&self) -> Result<Vec<u8>, StateError> {
        let peripherals = self.save_peripherals()?;
        let state = BoardState {
            chip: self.chip.save()?,
            peripherals,
            now: self.now,
            base: self.shared.base.get(),
            queue: self
                .queue
                .iter()
                .map(|(&key, &change)| (key, change))
                .collect(),
            sequence: self.sequence,
            outputs: self
                .outputs
                .iter()
                .map(|(&pin, &level)| (pin, level))
                .collect(),
            wired: self
                .nets
                .iter()
                .map(|net| (net.driven.clone(), net.level))
                .collect(),
            last: self.shared.last.borrow().clone(),
            clocks: self
                .clocks
                .iter()
                .map(|(&pin, &clock)| (pin, clock))
                .collect(),
            reset_in: self.reset_in,
            takers: self
                .shared
                .memories
                .borrow()
                .iter()
                .map(|memory| memory.taker)
                .collect(),
            qspi: self.shared.qspi.borrow().clone(),
            ahead: self.ahead.clone(),
        };
        Ok(encode(VERSION, &state))
    }

    /// Each peripheral's snapshot, in file order; an error names the
    /// peripheral.
    fn save_peripherals(&self) -> Result<Vec<Vec<u8>>, StateError> {
        self.shared
            .peripherals
            .iter()
            .zip(&self.peripherals)
            .map(|(peripheral, info)| {
                peripheral
                    .borrow()
                    .save()
                    .map_err(|error| error.of_peripheral(&info.name))
            })
            .collect()
    }

    /// Whether a snapshot's own state fits this board: its peripherals, nets,
    /// memories and chip output pins, its QSPI's PCS levels four bits,
    /// each change waiting on a net and driver the board has, with a
    /// sequence number before the next, each memory taken by a peripheral, a
    /// system clock that runs.
    fn check(&self, state: &BoardState<C::Pin>) -> Result<(), StateError> {
        let memories = self.shared.memories.borrow();
        if state.peripherals.len() != self.peripherals.len()
            || state.wired.len() != self.nets.len()
            || state.last.len() != self.nets.len()
            || state.takers.len() != memories.len()
        {
            return Err(StateError::mismatch("peripherals, nets or memories"));
        }
        let targets = self.shared.qspi.borrow().as_ref().map(QspiState::targets);
        if targets != state.qspi.as_ref().map(QspiState::targets) {
            return Err(StateError::mismatch("the QSPI's peripherals"));
        }
        if state.qspi.as_ref().is_some_and(|qspi| !qspi.levels_fit()) {
            return Err(StateError::invalid("the QSPI's PCS levels"));
        }
        if !state
            .outputs
            .iter()
            .map(|(pin, _)| pin)
            .eq(self.outputs.keys())
        {
            return Err(StateError::mismatch("chip output pins"));
        }
        if !state
            .clocks
            .iter()
            .all(|(pin, _)| self.sources.contains_key(&Source::Chip(*pin)))
        {
            return Err(StateError::mismatch("chip clock outputs"));
        }
        if self
            .nets
            .iter()
            .zip(&state.wired)
            .any(|(net, (driven, _))| driven.len() != net.driven.len())
        {
            return Err(StateError::invalid("a wired net's drivers"));
        }
        let fits = |&((_, sequence), change): &((Time, u64), Change)| {
            sequence < state.sequence
                && self.nets.get(change.net).is_some_and(|net| {
                    change.driver < net.drivers.len()
                        && (!net.wired || change.driver < net.driven.len())
                })
        };
        let keys: BTreeSet<(Time, u64)> = state.queue.iter().map(|&(key, _)| key).collect();
        if keys.len() != state.queue.len() || !state.queue.iter().all(fits) {
            return Err(StateError::invalid("a change waiting"));
        }
        if state
            .takers
            .iter()
            .flatten()
            .any(|&peripheral| peripheral >= self.peripherals.len())
        {
            return Err(StateError::invalid("a memory taken by no peripheral"));
        }
        if state.base.frequency().hz() == 0 {
            return Err(StateError::invalid("the system clock's frequency"));
        }
        if !state.ahead.iter().all(|ahead| ahead.pin.scheduled()) {
            return Err(StateError::invalid("a level held for a later clock"));
        }
        Ok(())
    }

    /// Restores a snapshot `save` made of a board loaded from the same
    /// file and registry.
    ///
    /// Once it succeeds, each observer is told the restored value of the
    /// net it observes ([`Observer::restored`]), nets in file order, a
    /// net's observers in the order attached.
    ///
    /// On an error the board is unchanged, and no observer is told: its
    /// own state is checked first,
    /// then each peripheral's snapshot by restoring it, each peripheral
    /// saved beforehand and the ones restored put back if a later
    /// peripheral or the chip (restored last, and unchanged when it
    /// refuses) refuses ([`restore_each`]).  So every peripheral must
    /// save, take its own snapshot back and be unchanged by one it
    /// refuses ([`Peripheral::restore`]).
    ///
    /// # Errors
    ///
    /// A snapshot of another version or board, or one the chip refuses; a
    /// peripheral that refuses its snapshot, named
    /// ([`StateError::Peripheral`]).
    ///
    /// [`Peripheral::restore`]: crate::peripheral::Peripheral::restore
    pub fn restore(&mut self, snapshot: &[u8]) -> Result<(), StateError> {
        let state: BoardState<C::Pin> = decode(VERSION, snapshot)?;
        self.check(&state)?;
        let backups = self.save_peripherals()?;
        let parts: Vec<&[u8]> = state
            .peripherals
            .iter()
            .map(Vec::as_slice)
            .chain([state.chip.as_slice()])
            .collect();
        let (peripherals, infos, chip) =
            (&self.shared.peripherals, &self.peripherals, &mut self.chip);
        restore_each(&parts, &backups, |part, bytes| {
            match peripherals.get(part) {
                Some(peripheral) => peripheral
                    .borrow_mut()
                    .restore(bytes)
                    .map_err(|error| error.of_peripheral(&infos[part].name)),
                None => chip.restore(bytes),
            }
        })?;
        self.now = state.now;
        self.shared.base.set(state.base);
        self.tell_system_clock();
        self.queue = state.queue.into_iter().collect();
        self.sequence = state.sequence;
        self.outputs = state.outputs.into_iter().collect();
        for (net, (driven, level)) in self.nets.iter_mut().zip(state.wired) {
            net.driven = driven;
            net.level = level;
        }
        *self.shared.last.borrow_mut() = state.last;
        self.clocks = state.clocks.into_iter().collect();
        self.reset_in = state.reset_in;
        for (memory, taker) in self
            .shared
            .memories
            .borrow_mut()
            .iter_mut()
            .zip(state.takers)
        {
            memory.taker = taker;
        }
        *self.shared.qspi.borrow_mut() = state.qspi;
        self.ahead = state.ahead;
        self.received.clear();
        self.events.clear();
        self.shared.outbox.borrow_mut().clear();
        self.shared.writes.borrow_mut().clear();
        self.shared.diverted.set(false);
        self.shared.stop.set(false);
        self.tell_restored();
        Ok(())
    }

    /// Tells each observer its net's restored value.
    fn tell_restored(&mut self) {
        let last = self.shared.last.borrow();
        for ((net, observers), signal) in self.nets.iter().zip(&mut self.observers).zip(&*last) {
            for attached in observers {
                attached.observer.restored(&net.name, self.now, *signal);
            }
        }
    }
}

/// The chip outputs whose change ends the chip's run: those on a net
/// a peripheral, the chip or an observer senses, so a loop through the
/// net back to the chip is the same however the host cuts the run, and
/// an observer that stops the run stops it at the change.  The chip's
/// own RESET on a wired net it senses is not such a loop
/// ([`Net::level_without`]).  A run-time endpoint does not sense: what
/// reaches it waits, with its time, for `take_received`, after the run.
fn sensed<P: ChipPin>(
    nets: &[Net<P>],
    sources: &BTreeMap<Source<P>, Vec<usize>>,
    observers: &[Vec<Attached>],
    reset: P,
) -> Vec<P> {
    sources
        .iter()
        .filter_map(|(source, driven)| match *source {
            Source::Chip(pin) => Some((pin, driven)),
            _ => None,
        })
        .filter(|&(pin, driven)| {
            driven.iter().any(|&net| {
                !observers[net].is_empty()
                    || nets[net].sinks.iter().any(|sink| match sink {
                        Sink::Peripheral(..) => true,
                        Sink::Chip(sensing) => !(pin == reset && *sensing == reset),
                        Sink::External(_) => false,
                    })
            })
        })
        .map(|(pin, _)| pin)
        .collect()
}

/// The chip's pin `name` on net `spec`, checked against the net's kind
/// and, as its driver if `driving`, its direction.
fn chip_pin<P: ChipPin>(spec: &NetSpec, name: &str, driving: bool) -> Result<P, LoadError> {
    let endpoint = || format!("chip.{name}");
    let pin: P = name.parse().map_err(|_| LoadError::Endpoint {
        net: spec.name.clone(),
        endpoint: endpoint(),
    })?;
    if !takes(pin, spec.kind) {
        return Err(LoadError::Kind {
            net: spec.name.clone(),
            kind: spec.kind,
            endpoint: endpoint(),
            found: pin.kind(),
        });
    }
    let fits = if driving {
        pin.direction().drives()
    } else {
        pin.direction().sinks()
    };
    if !fits {
        return Err(LoadError::Direction {
            net: spec.name.clone(),
            endpoint: endpoint(),
            what: if driving { "drive" } else { "be driven" },
        });
    }
    Ok(pin)
}

/// The description's nets on the chip and the board's peripherals, their
/// chip pins' and peripheral ports' names, kinds and directions checked.
fn resolve_nets<P: ChipPin>(
    description: &Description,
    peripherals: &[PeripheralInfo],
) -> Result<Vec<Net<P>>, LoadError> {
    description
        .nets
        .iter()
        .map(|spec| {
            let resolve =
                |endpoint: &Endpoint, driving: bool| -> Result<(usize, usize), LoadError> {
                    let Endpoint::Peripheral {
                        peripheral: owner,
                        port,
                    } = endpoint
                    else {
                        unreachable!("only peripheral endpoints resolve here");
                    };
                    let name = format!("{owner}.{port}");
                    let peripheral = peripherals
                        .iter()
                        .position(|peripheral| &peripheral.name == owner)
                        .expect("the description checked the peripheral");
                    let (index, declared) = peripherals[peripheral]
                        .ports
                        .iter()
                        .enumerate()
                        .find(|(_, declared)| &declared.name == port)
                        .ok_or_else(|| LoadError::Endpoint {
                            net: spec.name.clone(),
                            endpoint: name.clone(),
                        })?;
                    if declared.kind != spec.kind {
                        return Err(LoadError::Kind {
                            net: spec.name.clone(),
                            kind: spec.kind,
                            endpoint: name,
                            found: declared.kind,
                        });
                    }
                    let fits = if driving {
                        declared.direction.drives()
                    } else {
                        declared.direction.sinks()
                    };
                    if !fits {
                        return Err(LoadError::Direction {
                            net: spec.name.clone(),
                            endpoint: name,
                            what: if driving { "drive" } else { "be driven" },
                        });
                    }
                    Ok((peripheral, index))
                };
            let drivers = spec
                .from
                .iter()
                .map(|endpoint| match endpoint {
                    Endpoint::Peripheral { .. } => resolve(endpoint, true)
                        .map(|(peripheral, port)| Source::Peripheral(peripheral, port)),
                    Endpoint::Chip(name) => chip_pin(spec, name, true).map(Source::Chip),
                    Endpoint::External(name) => Ok(Source::External(name.clone())),
                })
                .collect::<Result<Vec<_>, _>>()?;
            let sinks = spec
                .to
                .iter()
                .map(|endpoint| match endpoint {
                    Endpoint::Peripheral { .. } => resolve(endpoint, false)
                        .map(|(peripheral, port)| Sink::Peripheral(peripheral, port)),
                    Endpoint::Chip(name) => chip_pin(spec, name, false).map(Sink::Chip),
                    Endpoint::External(name) => Ok(Sink::External(name.clone())),
                })
                .collect::<Result<Vec<_>, _>>()?;
            Ok(Net {
                name: spec.name.clone(),
                kind: spec.kind,
                wired: spec.wired,
                driven: vec![true; drivers.len()],
                level: true,
                drivers,
                sinks,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests;
