# Host guide

How to embed the MC68331 in a Rust host: build the chip in code, run
firmware on it, and connect the host's peripherals and pins.  `PLAN.md`
says why the interface is the way it is; this says how to use it.  The
Python module follows the same shape (`docs/python.md`).  A board file
describes a whole board as data instead, memories, peripherals and nets
around the chip, which `mc683xx-board` loads and runs
(`docs/board-files.md`); Front ends below works on a loaded board.

Every Rust block here is a doctest of the `mc683xx-examples` crate
(`examples/`), so `cargo test` compiles and runs them.  Their programs
are a few instructions written as words by `mc683xx_examples::firmware`
(`examples/src/firmware.rs`): `rom` lays out a boot ROM with the reset
vectors and a program at `START`, `select` programs a chip select,
`store_word`, `load_word` and the rest are single instructions, and
`booting` is the first example's board as a function.  A real host
loads a real image instead.

Crates: `mc683xx-cpu32` (the core: `Cpu`, `Bus`, `Memory`, hooks,
`Cycles`), `mc683xx-modules` (SIM, QSM, the `MappedPeripheral`, `Timed` and
`SpiPeripheral` traits), `mc683xx-mc68331` (GPT, and the chip: `Builder`,
`Mc68331`), `mc683xx-board` (board files, the `Registry` of peripheral
types, nets, board time and `Board`), `mc683xx-peripherals` (the stock
peripherals, `docs/peripherals.md`) and `mc683xx-gdb` (the GDB remote
stub, `docs/gdb.md`).

## Building a chip

The host wires memory and peripherals to chip-select pins, as the board
does; the firmware programs the SIM's chip-select registers, which
decide which cycles reach each pin (SIMRM 7).  The host never states
addresses, but for a fixed memory, which is emulator-only (see Fixed
memory).  Out of reset only CSBOOT decodes, the first Mbyte from
address 0 (MC68331UM Table 4-24), so the boot ROM goes there.

```rust
use mc683xx_cpu32::cpu::Stop;
use mc683xx_cpu32::cycles::Cycles;
use mc683xx_modules::chip_select::ChipSelect;
use mc683xx_modules::sim::Configuration;
use mc683xx_examples::firmware;
use mc683xx_mc68331::chip::BuildError;
use mc683xx_mc68331::chip::Builder;

fn main() -> Result<(), BuildError> {
    // CS0 at RAM_BASE, then a word into it.
    let program = [
        firmware::select(0, firmware::RAM_BASE, firmware::CSOR_FAST),
        firmware::store_word(firmware::RAM_BASE, 0x1234).to_vec(),
    ]
    .concat();
    // DATA[15:0] high (a 16-bit boot port), the 32.768 kHz crystal.
    let mut builder = Builder::new(Configuration::default());
    builder.memory(firmware::rom(&program), false, &[ChipSelect::Boot])?;
    let ram = builder.memory(vec![0; firmware::RAM_BYTES], true, &[firmware::cs(0)])?;
    // The power-on reset: SSP and PC from the vectors.
    let mut chip = builder.build();
    assert_eq!(chip.cpu().registers.pc, firmware::START);
    let run = chip.run_for(Cycles(1000));
    assert_eq!(run.stop, Stop::Budget);
    assert_eq!(chip.memory(ram)[..2], [0x12, 0x34]);
    Ok(())
}
```

- `Builder::new(Configuration)` takes what the SIM reads as reset
  releases: the levels on `DATA[15:0]` (boot port width, pin
  assignments; MC68331UM 4.6.3) and the clock source, a synthesizer
  reference (`ClockSource::synthesizer(reference)`) or, with MODCLK
  low, an external clock (`ClockSource::external(frequency)`), each a
  `Frequency` (`mc683xx_cpu32::frequency`, nanohertz;
  `Frequency::from_hz`) of whole hertz: either refuses a fraction
  with `ClockSourceError::FractionalHertz`, since the chip's snapshot
  saves its clocks as hertz, and `synthesizer` a reference outside 25
  to 50 kHz (SIMRM 4.4.1) with `ClockSourceError::ReferenceRange`.  `Builder::system_frequency()` gives the
  system clock out of reset, `sim().clock().frequency()` as SYNCR sets
  it.
  `Builder::default()` is DATA high and the 32.768 kHz crystal.
- `memory(bytes, writable, pins)`: RAM or ROM, a whole number of
  2-Kbyte pages, on one or more pins.  A select's block larger than
  the memory repeats it.  The core serves memory itself, fast, except
  where a peripheral shares the cycle.  `memory` and `memory_mut` give
  the bytes back by `MemoryId`.
- `fixed_memory(base, bytes, writable)`: memory at a fixed address on
  no pin, emulator-only, and `fixed_timing(memory, port, termination)`
  its port and wait states (see Fixed memory).
- `reload_scope(memory, ResetScope)` says which resets put a memory's
  contents back as built; none do by default (see Fixed memory).
- `peripheral(peripheral, pins)` puts a `MappedPeripheral + Timed + Save` on pins; a peripheral on
  several (separate read and write selects, a register select and an
  acknowledge select) tells them apart by `BusCycle::select`.  `unwired_peripheral`
  adds one on no pin: something that only keeps time or drives an
  interrupt.  `Mc68331::peripheral::<D>(id)` and `peripheral_mut` give it back
  by type.
- `interrupt(peripheral, pin)` wires a peripheral's interrupt output to an
  IRQ pin, an `IrqPin` (levels 1 to 7; `IrqPin::new(level)`);
  `autovector(pin)` ties AVEC for every acknowledge of its level, as
  decode logic does (SIMRM 6.4).
- `reset_scope(peripheral, ResetScope)` says which reset causes reach a
  peripheral; all do by default (see Reset).
- `qspi(peripheral)` puts a `SpiPeripheral` on the QSPI's pins (see QSPI).
- `stop_request()` gives a handle a peripheral can hold to stop the run,
  and `sync_request()` one to end it early (see Running).
- `build()` runs the power-on reset and gives the `Mc68331`.

The host can also reach the core: `cpu()` gives the `Cpu` to read;
`registers_mut()` its registers to change; `cpu_control()` a
`cpu::control::CpuControl`, which adds and removes its hooks, sets its
pages' permissions (see Hooks), changes its registers and reads the
rest.  Neither hands out the core's memory map, which the chip keeps
in step with its decode, nor runs the core apart from the chip.
`poke(address, bytes)` writes the memories the core serves without a
cycle, as a debugger does, a ROM's included (`Unserved` for a byte it
does not serve).

### Fixed memory

`Builder::fixed_memory(base, bytes, writable)` maps memory at `base`
(an `Address` on a 2-Kbyte page boundary, the memory inside
ADDR[23:0]) up to its size, on no chip select.  **It is emulator-only:
the chip has no such decode.**  It is for code and data the host maps
beside an image it leaves as it is: rewritten routines the image's
calls are diverted to, a test stub, a scratch area, without patching
the reset code to program a select the firmware leaves unused.

- It takes every program and data cycle, user or supervisor, at an
  address it holds that no chip select matches.  A select that matches
  a cycle takes it, as on the chip, so the firmware's own map behaves
  as on hardware wherever it decodes; the internal modules win over
  both (MC68331UM 4.8).  CPU-space cycles never reach it.  Out of
  reset only CSBOOT decodes, so it answers at once wherever CSBOOT's
  block does not reach (its reset block is the first Mbyte,
  MC68331UM Table 4-24).
- Its cycles end at once by default: a 16-bit port with fast
  termination, costing nothing beyond the timing tables, as an
  internal module's, with no DSACK.  `fixed_timing(memory, port,
  termination)` times it as a select's DSACK field would (MC68331UM
  D.3.26): `Termination::Fast` or `Termination::WaitStates(0..=13)`
  on `PortSize::Bits8` or `Bits16`, a word on 8 bits two cycles; any
  other termination is `BuildError::Termination`, a memory on pins
  `BuildError::NotFixed`.  With wait states the bus monitor ends a
  cycle longer than its period, as it does a select's.  The core
  serves it itself.
- Two fixed memories may not overlap (`BuildError::Overlap`); a base
  off a page boundary or a memory past ADDR[23:0] is
  `BuildError::Placement`.
- Written when `writable`; a read-only one ignores writes, as ROM on
  a select does.  It is never diverted: `Diversion::divert` and
  `divert_memory` leave it as it is.
- `memory` and `memory_mut` reach its bytes by its `MemoryId`, and a
  snapshot holds them as every memory's.

Its contents survive every reset, as RAM's do, unless
`reload_scope(memory, scope)` says otherwise: each reset in `scope`
puts them back to the bytes it was built with, before the CPU fetches
the reset vectors, so a warm restart finds the code fresh.  The
`reset` instruction's reload (`ResetCause::System`) comes after the
instruction, before the next.  `reload_scope` takes any memory, one on
pins too; reloading is emulator-only as well.

```rust
use mc683xx_cpu32::address::Address;
use mc683xx_cpu32::cycles::Cycles;
use mc683xx_examples::firmware;
use mc683xx_modules::sim::reset::ResetScope;

// No select programmed: the boot code stores straight into it.
let code = 0x60_0000;
let (mut builder, _) = firmware::booting(firmware::rom(&firmware::store_word(code, 0x1234)));
let fixed = builder.fixed_memory(Address(code), vec![0; 2048], true).unwrap();
builder.reload_scope(fixed, ResetScope::ALL);
let mut chip = builder.build();
chip.run_for(Cycles(1000));
assert_eq!(chip.memory(fixed)[..2], [0x12, 0x34]);
chip.reset();
assert_eq!(chip.memory(fixed)[..2], [0, 0], "reloaded");
```

## Running

`Mc68331::run_for(budget)` runs whole instructions for `budget` system
clocks and returns a `Run`: the cycles it took (at least the budget,
unless it stopped early, and up to an instruction past it) and why it
ended, a `Stop`:

- `Budget`: it spent the budget, or a peripheral asked it to end early
  through its `SyncRequest`, after the instruction whose cycle asked
  (fewer cycles than the budget): not a stop, for a host whose own
  scheduler must see what a peripheral did before the run goes on, as
  `mc683xx-board` does for its nets.
- `Stopped`: `stop` or `lpstop`; the rest of the budget passed waiting
  for an interrupt.
- `Halted(Halt::DoubleBusFault)`: only a reset restarts it.  A halt
  the SIM's halt monitor catches is a reset instead (an
  `Event::Reset`), and the run goes on.  The run ends at the halt; a
  run that starts halted spends its budget as time passes for the
  SIM, the modules and the peripherals without the CPU, so the watchdog
  can reset the chip (MC68331UM 4.2.10), and ends `Halted` if no
  reset restarted it.
- `Hook(id)`: a hook returned `ControlFlow::Break`.
- `Requested`: a peripheral asked through its `StopRequest`.

`now()` is the chip's time in system clocks, from zero at power-on.
The run keeps it: the host passes only budgets.  Inside a run the
chip runs the core in quanta that end at the earliest next event of
the SIM (PIT, watchdog), the QSM, the GPT and every peripheral's
`Timed::next_event`, then brings each up to the sync point with
`advance`.  So a peripheral sees time move between quanta, and an
interrupt it raises at its event waits at most one instruction.  The
watchdog's resets are taken as they come.

```rust
use mc683xx_cpu32::cpu::Stop;
use mc683xx_cpu32::cycles::Cycles;
use mc683xx_examples::firmware;

let program = [firmware::prologue(), firmware::stop(firmware::UNMASKED).to_vec()].concat();
let (builder, _) = firmware::booting(firmware::rom(&program));
let mut chip = builder.build();
let run = chip.run_for(Cycles(10_000));
assert_eq!(run.stop, Stop::Stopped);
assert!(run.cycles >= Cycles(10_000));
assert_eq!(chip.now(), run.cycles);
```

A host with a real-time display or a socket runs in budgets of a
frame or a millisecond and does its own work between them.  A run
ends at the first instruction boundary at or past its budget, so the
overshoot adds up over many runs.  A host that keeps to a schedule
runs to times instead: `run_until(target)` runs whole instructions
until `now()` is `target` or past it, and returns the same `Run`.  It
ends at the first instruction boundary at or after `target` (at once
if `now()` already is; exactly there if the CPU is stopped), which
depends only on the chip and `target`, so a series of `run_until` to
rising targets is one long run to the last, however the host cuts it.

Determinism: the same build, the same inputs at the same chip times,
give the same run.  There is no host time, no threads and no
host-ordered iteration in the chip.  A host keeps it so by making its
inputs (pins, frames, edges) at chip times, not wall-clock ones.

```rust
use mc683xx_cpu32::cycles::Cycles;
use mc683xx_examples::firmware;

let chip = || {
    let program = [firmware::prologue(), firmware::delay(500).to_vec()].concat();
    firmware::booting(firmware::rom(&program)).0.build()
};
let (mut one, mut two) = (chip(), chip());
one.run_until(Cycles(5000));
for quantum in 1..=5 {
    two.run_until(Cycles(quantum * 1000));
}
assert_eq!(one.now(), two.now());
assert_eq!(one.cpu().registers.data, two.cpu().registers.data);
assert_eq!(one.instructions(), two.instructions());
```

### Stopping from a peripheral

A peripheral holds the `StopRequest` (`mc683xx_cpu32::bus`, the one type
the bare machine's `stop_request` gives too) its builder gave and
calls `request`;
the run stops once the instruction whose cycle asked ends, or between
quanta if `advance` asked, with `Stop::Requested`.  The request is
taken by the stop.  A host whose own run loop stopped for the same
reason before the chip ran withdraws it (`withdraw`), so the next run
does not stop for it.

```rust
use mc683xx_cpu32::bus::BusError;
use mc683xx_cpu32::cpu::Stop;
use mc683xx_cpu32::cpu::state::Stateless;
use mc683xx_cpu32::cycles::Cycles;
use mc683xx_cpu32::peripheral::BusCycle;
use mc683xx_cpu32::peripheral::MappedPeripheral;
use mc683xx_modules::peripheral::Timed;
use mc683xx_examples::firmware;
use mc683xx_cpu32::bus::StopRequest;

/// Stops the run at each write: a breakpoint on a port.
#[derive(Debug)]
struct Breaker(StopRequest);

impl MappedPeripheral for Breaker {
    fn read(&mut self, _: BusCycle) -> Result<u32, BusError> {
        Ok(0)
    }

    fn write(&mut self, _: BusCycle, _: u32) -> Result<(), BusError> {
        self.0.request();
        Ok(())
    }
}

impl Timed for Breaker {}

impl Stateless for Breaker {}

const BASE: u32 = 0x20_0000;
let program = [
    firmware::prologue(),
    firmware::select(1, BASE, firmware::CSOR_FAST),
    firmware::store_word(BASE, 1).to_vec(),
]
.concat();
let (mut builder, _) = firmware::booting(firmware::rom(&program));
let stop = builder.stop_request();
builder.peripheral(Breaker(stop), &[firmware::cs(1)]).unwrap();
let mut chip = builder.build();
assert_eq!(chip.run_for(Cycles(10_000)).stop, Stop::Requested);
assert_eq!(chip.run_for(Cycles(100)).stop, Stop::Budget);
```

## Peripherals

A host peripheral implements `mc683xx_cpu32::peripheral::MappedPeripheral`,
`mc683xx_modules::peripheral::Timed` (`impl Timed for X {}` if it keeps no time)
and `mc683xx_cpu32::cpu::state::Save`, which says whether it saves (see
Save and restore): both its methods if it saves, `impl Stateless for X {}`
(`mc683xx_cpu32::cpu::state`) if it has no state, or
`mc683xx_cpu32::unsavable!(X, "its role")` if it cannot save.  `Save`
has no defaults, so that a peripheral never saves what its own `restore`
refuses.

`MappedPeripheral`:

- `read(cycle) -> Result<u32, BusError>` and `write(cycle, value)`.  A
  `BusCycle` carries the select that asserted, the address on ADDR (board
  decode within a select's window is the peripheral's: it reads these),
  the size (byte or word: a long is two word cycles, high word first,
  and a word on an 8-bit port two byte cycles), the function code and
  the cycle's start time.  `BusCycle` is `#[non_exhaustive]`: a host
  that builds one (a test) calls `BusCycle::new(select, address, size,
  function_code, now)`, so a field added later gets a default.  Values are right-aligned; a read must fit
  the cycle.  `Err(BusError)` asserts BERR: the CPU takes a bus error.
- `wait_states(cycle) -> Option<u8>`: on a select whose DSACK field is
  external (%1111, SIMRM 7.10.3), the wait states before the peripheral
  asserts DSACK, or `None` if it never does, so the bus monitor ends
  the cycle in a bus error.  On other selects the option register
  times the cycle and this is not asked.  Each wait state costs the
  CPU a clock, so slow peripherals cost what they do on the board.
  `Builder::dsack(peripheral, Dsack)` states which DSACK pins it drives:
  the chip then checks PEPAR at each such cycle (a pin assigned as
  port E ends no cycle, SIMRM 9.1, told as `Event::Dsack`; `Event` is
  `#[non_exhaustive]`), and the port size they encode sizes the cycle
  (SIMRM Table 5-2, 5.3.3): a word to a DSACK0 peripheral is two byte
  cycles whatever the select's port field.  Without it the field
  sizes the cycle, as it does any the select ends itself (SIMRM 7.6).
- `interrupt_request() -> bool`: its interrupt output, level-sensitive,
  on the IRQ pin `Builder::interrupt` wired.  It may change only at a
  board event: a bus cycle to the board, an acknowledge, `advance`, a
  reset, the host's `peripheral_mut`, or between runs.  The chip caches
  the level it requests between them.
- `reset(now)`: RESET asserted, by a reset in its scope or the `reset`
  instruction.

`Timed`:

- `next_event() -> Option<Cycles>`: when it next changes on its own.
- `advance(now)`: brings it to `now`, between quanta.

`Save`:

- `save() -> Result<Vec<u8>, StateError>`: its state, in its own
  format, which it versions (`encode` and `decode` in
  `mc683xx_cpu32::cpu::state` do).
- `restore(&[u8])`: puts back what `save` gave, and must take its own
  `save`'s bytes; it refuses, unchanged, what no run of it could save.

Neither has a default: a peripheral gives both, is `Stateless`, or is
`mc683xx_cpu32::unsavable!(X, "its role")`, whose both refuse with
`StateError::unsupported("its role")`.

### Interrupt acknowledge through a select

A vectored peripheral answers its own interrupt acknowledge: the firmware
programs a select for CPU space at the peripheral's level (SIMRM 7.8.1),
and the acknowledge reaches the peripheral as a CPU-space byte read at
$FFFFF1 + 2 x level; the value read is the vector.  With an external
DSACK the peripheral's wait states apply; one later than the bus monitor
allows, or a bus error, is the spurious vector.  Without one, a level
the board autovectors (`Builder::autovector`, or a select's AVEC bit)
takes the autovector.

This peripheral has a vector register and a request register on CS1,
answers with its own DSACK after two wait states, and acknowledges
on CS10:

```rust
use mc683xx_cpu32::bus::BusError;
use mc683xx_cpu32::cycles::Cycles;
use mc683xx_cpu32::interrupt::IrqPin;
use mc683xx_cpu32::peripheral::BusCycle;
use mc683xx_cpu32::peripheral::MappedPeripheral;
use mc683xx_modules::peripheral::Timed;
use mc683xx_examples::firmware;

/// Its registers' offsets, on the lower byte.
const VECTOR_REGISTER: u32 = 1;
const REQUEST_REGISTER: u32 = 3;

/// A peripheral that interrupts when told to and gives its vector.
#[derive(Debug, Default)]
struct Interrupter {
    vector: u8,
    requesting: bool,
    acknowledges: Vec<BusCycle>,
}

impl MappedPeripheral for Interrupter {
    fn read(&mut self, cycle: BusCycle) -> Result<u32, BusError> {
        if cycle.function_code.is_cpu_space() {
            self.acknowledges.push(cycle);
            self.requesting = false;
            return Ok(self.vector.into());
        }
        Ok(0)
    }

    fn write(&mut self, cycle: BusCycle, value: u32) -> Result<(), BusError> {
        match cycle.address & 0xf {
            VECTOR_REGISTER => self.vector = value as u8,
            REQUEST_REGISTER => self.requesting = value != 0,
            _ => return Err(BusError),
        }
        Ok(())
    }

    fn wait_states(&mut self, _: BusCycle) -> Option<u8> {
        Some(2)
    }

    fn interrupt_request(&self) -> bool {
        self.requesting
    }

    fn reset(&mut self, _: Cycles) {
        *self = Self::default();
    }
}

impl Timed for Interrupter {}

// It cannot save: nor then can the chip.
mc683xx_cpu32::unsavable!(Interrupter, "an interrupter");

const BASE: u32 = 0x20_0000;
const VECTOR: u8 = 0x40;
const LEVEL: u8 = 2;
let program = [
    firmware::prologue(),
    firmware::select(1, BASE, firmware::CSOR_EXTERNAL),
    firmware::acknowledge_select(10, LEVEL),
    firmware::store_byte(BASE + VECTOR_REGISTER, VECTOR).to_vec(),
    firmware::store_byte(BASE + REQUEST_REGISTER, 1).to_vec(),
    firmware::set_sr(firmware::UNMASKED).to_vec(),
    vec![firmware::BRANCH_TO_SELF],
]
.concat();
let mut image = firmware::rom(&program);
firmware::set_vector(&mut image, VECTOR, firmware::HANDLER);
let handler = [
    firmware::store_word(firmware::RAM_BASE, 0xbeef).to_vec(),
    vec![firmware::BRANCH_TO_SELF],
]
.concat();
firmware::patch(&mut image, firmware::HANDLER, &handler);

let (mut builder, ram) = firmware::booting(image);
let peripheral = builder
    .peripheral(Interrupter::default(), &[firmware::cs(1), firmware::cs(10)])
    .unwrap();
builder.interrupt(peripheral, IrqPin::new(LEVEL).unwrap());
let mut chip = builder.build();
chip.run_for(Cycles(5000));
assert_eq!(chip.memory(ram)[..2], [0xbe, 0xef], "the handler ran");
let acknowledges = &chip.peripheral::<Interrupter>(peripheral).unwrap().acknowledges;
assert_eq!(acknowledges.len(), 1);
assert_eq!(acknowledges[0].select, Some(firmware::cs(10).into()));
assert_eq!(acknowledges[0].address, 0xff_fff1 + 2 * u32::from(LEVEL));
```

The host can also drive an IRQ pin itself:
`Mc68331::set_interrupt_pin(pin, asserted)`.

### Models with their own time

A model beside the CPU (a GPS engine, a timer chip, a 1 PPS source)
reports its next event, and the run syncs there:

```rust
use mc683xx_cpu32::bus::BusError;
use mc683xx_cpu32::cpu::state::Save;
use mc683xx_cpu32::cpu::state::StateError;
use mc683xx_cpu32::cpu::state::decode;
use mc683xx_cpu32::cpu::state::encode;
use mc683xx_cpu32::cycles::Cycles;
use mc683xx_cpu32::interrupt::IrqPin;
use mc683xx_cpu32::peripheral::BusCycle;
use mc683xx_cpu32::peripheral::MappedPeripheral;
use mc683xx_modules::peripheral::Timed;
use mc683xx_examples::firmware;

/// The format of an `Alarm`'s snapshot.
const ALARM_VERSION: u32 = 1;

/// Asserts its interrupt output from `at` on.
#[derive(Debug)]
struct Alarm {
    at: Cycles,
    now: Cycles,
}

impl MappedPeripheral for Alarm {
    fn read(&mut self, _: BusCycle) -> Result<u32, BusError> {
        Err(BusError)
    }

    fn write(&mut self, _: BusCycle, _: u32) -> Result<(), BusError> {
        Err(BusError)
    }

    fn interrupt_request(&self) -> bool {
        self.now >= self.at
    }
}

impl Timed for Alarm {
    fn next_event(&self) -> Option<Cycles> {
        (self.now < self.at).then_some(self.at)
    }

    fn advance(&mut self, now: Cycles) {
        self.now = now;
    }
}

impl Save for Alarm {
    fn save(&self) -> Result<Vec<u8>, StateError> {
        Ok(encode(ALARM_VERSION, &(self.at, self.now)))
    }

    fn restore(&mut self, snapshot: &[u8]) -> Result<(), StateError> {
        (self.at, self.now) = decode(ALARM_VERSION, snapshot)?;
        Ok(())
    }
}

// The level 7 autovector (CPU32RM Table 6-1).
const AUTOVECTOR_7: u8 = 31;
let program = [
    firmware::prologue(),
    firmware::set_sr(firmware::UNMASKED).to_vec(),
    vec![firmware::BRANCH_TO_SELF],
]
.concat();
let mut image = firmware::rom(&program);
firmware::set_vector(&mut image, AUTOVECTOR_7, firmware::HANDLER);
firmware::patch(&mut image, firmware::HANDLER, &[firmware::BRANCH_TO_SELF]);
let (mut builder, _) = firmware::booting(image);
let alarm = builder.unwired_peripheral(Alarm {
    at: Cycles(5000),
    now: Cycles::ZERO,
});
let seven = IrqPin::IRQ7;
builder.interrupt(alarm, seven);
builder.autovector(seven);
let mut chip = builder.build();
chip.run_for(Cycles(4000));
assert_ne!(chip.cpu().registers.pc, firmware::HANDLER);
let before = chip.save().unwrap();
let then = chip.peripheral::<Alarm>(alarm).unwrap().now;
chip.run_for(Cycles(4000));
assert_eq!(chip.cpu().registers.pc, firmware::HANDLER);
assert_eq!(chip.cpu().registers.interrupt_mask(), 7);
// The alarm's time is in the chip's snapshot.
chip.restore(&before).unwrap();
assert_eq!(chip.peripheral::<Alarm>(alarm).unwrap().now, then);
```

## Reset

`Mc68331::reset()` is RESET driven by the board, an external reset.
The watchdog, the halt monitor and the `reset` instruction make their
own.  Each resets the SIM and the modules, returns the decode to
CSBOOT alone, resets the peripherals in its scope, and appears as an
`Event::Reset(now, cause)` in `take_events` (the power-on reset
included).  Every reset source drives RESET (SIMRM Table 8-1), but a
board need not wire every peripheral to it: `ResetScope` names the causes
that reach one.

```rust
use mc683xx_cpu32::bus::BusError;
use mc683xx_cpu32::cycles::Cycles;
use mc683xx_cpu32::peripheral::BusCycle;
use mc683xx_cpu32::peripheral::MappedPeripheral;
use mc683xx_modules::peripheral::Timed;
use mc683xx_modules::sim::ResetCause;
use mc683xx_examples::firmware;
use mc683xx_mc68331::event::Event;
use mc683xx_modules::sim::reset::ResetScope;

#[derive(Debug, Default)]
struct Counter(usize);

impl MappedPeripheral for Counter {
    fn read(&mut self, _: BusCycle) -> Result<u32, BusError> {
        Ok(0)
    }

    fn write(&mut self, _: BusCycle, _: u32) -> Result<(), BusError> {
        Ok(())
    }

    fn reset(&mut self, _: Cycles) {
        self.0 += 1;
    }
}

impl Timed for Counter {}

mc683xx_cpu32::unsavable!(Counter, "a reset counter");

// No prologue: the watchdog runs.
let (mut builder, _) = firmware::booting(firmware::rom(&[firmware::BRANCH_TO_SELF]));
let everything = builder.unwired_peripheral(Counter::default());
let not_watchdog = builder.unwired_peripheral(Counter::default());
builder.reset_scope(not_watchdog, ResetScope::ALL.without(ResetCause::Watchdog));
let mut chip = builder.build();
// The watchdog's 2^9 EXTAL periods of 256 clocks (MC68331UM 4.2.10).
chip.run_for(Cycles(512 * 256 + 100));
chip.reset();
let causes: Vec<ResetCause> = chip
    .take_events()
    .into_iter()
    .filter_map(|event| match event {
        Event::Reset(_, cause) => Some(cause),
        _ => None,
    })
    .collect();
assert_eq!(
    causes,
    [ResetCause::PowerOn, ResetCause::Watchdog, ResetCause::External]
);
let resets = |id| chip.peripheral::<Counter>(id).unwrap().0;
assert_eq!((resets(everything), resets(not_watchdog)), (3, 2));
```

## Host I/O

The modules' registers are the firmware's (the host can look at them:
see Host bus cycles); the host drives and reads their pins.  A level
the host sets takes effect at the chip's time between runs.  What
the chip drives within a run comes back as a queue of changes, each
with its time, so a pulse shorter than a run is both its edges; the
queues grow until taken.

### Diverting a memory

A peripheral that must answer a memory's cycles for a while (a flash's
command logic, during a program or in autoselect mode) holds
`Builder::diversion()`, a `Diversion`: `divert(memory, peripheral)` sends
every cycle that reaches the memory to the peripheral, as a cycle on the
select that carried it, and takes the memory's pages off the core's
fast path; `restore(memory)` gives them back.  Asked during a cycle,
it takes effect as the cycle ends; between runs, as the next run
starts.  `Mc68331::divert_memory(memory, peripheral)` does it from the
host at once.  A snapshot keeps the diversions.  A fixed memory is
never diverted: no select carries its cycles.

### SCI

A frame's time is its start bit's.  Whole frames: `sci_receive(Frame)`
starts one on RXD now, or as the frames sent before it end, each a
frame time at SCCR0's baud.  `take_sci_transmitted()` gives the frames
the SCI sent on TXD as each starts, a `Transmitted`: the `frame`, its
`start` and `end`, and `bit`, the system clocks in its bit time.  `Frame::Data(bits)` carries the
data bits as on the line, LSB first, eight or nine, parity included;
`Frame::Break` a break.

```rust
use mc683xx_cpu32::cycles::Cycles;
use mc683xx_modules::qsm::sci::Frame;
use mc683xx_examples::firmware;
use mc683xx_examples::firmware::QSM;

// SCCR0, SCCR1, SCSR, SCDR (QSMRM 5.2); TE and RE, RDRF.
const SCCR0: u32 = QSM + 0x08;
const SCCR1: u32 = QSM + 0x0a;
const SCSR: u32 = QSM + 0x0c;
const SCDR: u32 = QSM + 0x0e;
const TE_RE: u16 = 0x000c;
const RDRF: u16 = 0x0040;

// SCBR 1 (32 clocks a bit), then echo each frame received.
let poll = [firmware::load_word(SCSR, 1).to_vec(), vec![firmware::ANDI_WORD_D1, RDRF]].concat();
let mut echo = poll.clone();
echo.push(firmware::short_branch(firmware::BEQ_SHORT, poll.len() as u16 + 1));
echo.extend(firmware::load_word(SCDR, 0));
echo.extend(firmware::store_d0_byte(SCDR + 1));
echo.push(firmware::short_branch(firmware::BRA_SHORT, echo.len() as u16 + 1));
let program = [
    firmware::prologue(),
    firmware::store_word(SCCR0, 1).to_vec(),
    firmware::store_word(SCCR1, TE_RE).to_vec(),
    echo,
]
.concat();
let (builder, _) = firmware::booting(firmware::rom(&program));
let mut chip = builder.build();
chip.run_for(Cycles(1000));
chip.sci_receive(Frame::Data(u16::from(b'!')));
chip.run_for(Cycles(2000));
let sent = chip.take_sci_transmitted();
assert_eq!(sent.len(), 1);
assert_eq!(sent[0].frame, Frame::Data(u16::from(b'!')));
assert!(sent[0].start < sent[0].end);
```

### Ports C, E, F and QS

- Inputs: `set_port_e_pins`, `set_port_f_pins`, `set_port_qs_pins`
  set the levels the board drives.
- Outputs: `port_c_outputs()`, `port_e_outputs()`,
  `port_f_outputs()` and `port_qs_outputs()` (and the GPT's
  `port_gp_outputs()`) give the pins a port drives and their levels, a
  `PortLevels { driven, levels }` (the levels zero on pins not driven;
  `pin_levels(pins)` fills them).  Port C is the latch on the CS3-CS9 pins assigned as discrete
  outputs (MC68331UM 4.8.1.4).
- Changes: `take_port_changes()` gives `(time, Change { port,
  outputs })`, `outputs` a `PortLevels`, for ports C, E and F, each at its write cycle's start.
  `take_qsm_changes()` gives `(time, PortLevels)` for port QS, a
  register write's at its instruction's start, the QSPI's PCS and the
  SCI's TXD at the entry's or frame's own time.

```rust
use mc683xx_cpu32::address::Address;
use mc683xx_cpu32::cycles::Cycles;
use mc683xx_cpu32::function_code::FunctionCode;
use mc683xx_cpu32::size::Size;
use mc683xx_modules::pins::PortLevels;
use mc683xx_modules::sim::ports::PortName;
use mc683xx_examples::firmware;
use mc683xx_examples::firmware::SIM;

// PORTE, DDRE, PEPAR (MC68331UM D.3.6 to D.3.8).
const PORTE: u32 = SIM + 0x10;
const DDRE: u32 = SIM + 0x14;
const PEPAR: u32 = SIM + 0x16;
const PE0: u8 = 0x01;
const PE1: u8 = 0x02;

// PE0 a port output, pulsed.
let program = [
    firmware::prologue(),
    firmware::store_word(PEPAR, u16::from(!PE0 & 0xff)).to_vec(),
    firmware::store_word(DDRE, PE0.into()).to_vec(),
    firmware::store_word(PORTE, PE0.into()).to_vec(),
    firmware::store_word(PORTE, 0).to_vec(),
]
.concat();
let (builder, _) = firmware::booting(firmware::rom(&program));
let mut chip = builder.build();
chip.set_port_e_pins(PE1);
chip.run_for(Cycles(1000));
assert_eq!(
    chip.port_e_outputs(),
    PortLevels {
        driven: PE0,
        levels: 0
    }
);
let edges: Vec<(u8, u8)> = chip
    .take_port_changes()
    .into_iter()
    .filter(|(_, change)| change.port == PortName::E)
    .map(|(_, change)| (change.outputs.driven, change.outputs.levels))
    .collect();
assert_eq!(edges, [(PE0, 0), (PE0, PE0), (PE0, 0)]);
// PE1, an input, as PORTE reads it.
let porte = chip.peek(Address(PORTE), Size::Word, FunctionCode::SUPERVISOR_DATA);
assert_eq!(porte.unwrap() & u32::from(PE1), u32::from(PE1));
```

### GPT

- Inputs now: `set_port_gp_pins` (IC1 to IC4/OC5), `set_pai`,
  `set_pclk`.
- Inputs ahead: `schedule_gpt_input(at, Input, level)` for an edge a
  board model times itself (an FPGA's 1 PPS on IC1): the GPT captures,
  counts or clocks at `at`, and a run's quantum ends there.  `Input` is
  `PortGp(PortGpPin)`, `Pai` or `Pclk`.  Times are whole system
  clocks; a model with a finer phase rounds up.
  `unschedule_gpt_inputs()` withdraws those past now, for a host whose
  time base moved (a SYNCR write) to schedule again.
- Outputs: `port_gp_outputs()` (a `PortLevels`, TCNT's clock on OC1
  under CPROUT); `pwm(Channel)` gives what a PWM pin carries as an
  `Output`: `Level`, `Wave { period, high, clocks }` or `Clock`
  (PPROUT); `oc1()` gives OC1's pin as an `Output` too, `Clock` under
  CPROUT unless OC1M3 takes the pin back, `Level` otherwise.
- Changes: `take_gpt_changes()` gives `(time, OutputChange)` in time
  order, `PortGp(PortLevels)` always and `Pwm { channel, level }`
  for a channel `set_report_pwm_edges(channel, true)` asks for, PPROUT's
  clock edges included.  A compare's edge has the time TCNT matched.
  CPROUT's clock edges on OC1 come in `PortGp` only while
  `set_report_oc1_clock_edges(true)` asks; otherwise the changes show the
  pin low.

```rust
use mc683xx_cpu32::address::Address;
use mc683xx_cpu32::cycles::Cycles;
use mc683xx_cpu32::function_code::FunctionCode;
use mc683xx_cpu32::size::Size;
use mc683xx_examples::firmware;
use mc683xx_examples::firmware::GPT;
use mc683xx_mc68331::gpt::Input;
use mc683xx_mc68331::gpt::OutputChange;
use mc683xx_mc68331::gpt::PortGpPin;
use mc683xx_mc68331::gpt::pwm::Channel;
use mc683xx_mc68331::gpt::pwm::Output;
use mc683xx_modules::pins::PortLevels;

// TCTL1:TCTL2, TOC2, TIC1 (GPTRM 3).  OM2:OL2 01 toggles OC2, PGP4,
// on a match; EDGE1 01 captures IC1's rising edges.
const TCTL: u32 = GPT + 0x1e;
const TOC2: u32 = GPT + 0x16;
const TIC1: u32 = GPT + 0x0e;
const OC2_TOGGLE_IC1_RISING: u16 = 0x0101;
const OC2: u8 = 0x10;
// TCNT counts the system clock / 4 out of reset.
const CLOCKS_PER_COUNT: u64 = 4;

let program = [
    firmware::prologue(),
    firmware::store_word(TCTL, OC2_TOGGLE_IC1_RISING).to_vec(),
    firmware::store_word(TOC2, 0x1000).to_vec(),
]
.concat();
let (builder, _) = firmware::booting(firmware::rom(&program));
let mut chip = builder.build();
let ic1 = Input::PortGp(PortGpPin::new(0).unwrap());
chip.schedule_gpt_input(Cycles(2000), ic1, false);
chip.schedule_gpt_input(Cycles(3000), ic1, true);
chip.run_for(Cycles(CLOCKS_PER_COUNT * 0x1000 + 100));
let changes = chip.take_gpt_changes();
let last = changes.last().unwrap();
assert_eq!(last.0, Cycles(CLOCKS_PER_COUNT * 0x1000));
assert_eq!(
    last.1,
    OutputChange::PortGp(PortLevels {
        driven: OC2,
        levels: OC2
    })
);
let captured = chip.peek(Address(TIC1), Size::Word, FunctionCode::SUPERVISOR_DATA);
assert_eq!(u64::from(captured.unwrap()), 3000 / CLOCKS_PER_COUNT);
assert_eq!(chip.pwm(Channel::A), Output::Level(false));
assert!(matches!(chip.oc1(), Output::Level(_)));
```

### QSPI

As a master the QSPI's peripheral is the host's `SpiPeripheral`
(`Builder::qspi`).  `transfer(QspiTransfer) -> u16` shifts one queue
entry and returns the bits in.  A `QspiTransfer` (`#[non_exhaustive]`,
`QspiTransfer::new(entry, chip_selects, out, bits, clock, now)`)
carries the queue entry's index (0-15), the levels on `PCS[3:0]` (four
selects or a decoded peripheral number, as the board wires them), the bits
out, right-aligned, MSB first, their number, the `QspiClock` they are
shifted with (its mode, CPOL and CPHA, and SCK period) and when the
transfer ends; `QspiClock::reset(system_clock)` is the clock out of
reset (SPCR0 $0104: CPOL clear, CPHA set, SPBR 4, QSMRM 4.3.1) at the
system clock the caller gives, 8.388608 MHz out of reset from a
32.768-kHz reference; `floating_miso(bits)` is MISO no peripheral drives, all
ones.  Optional:
`chip_selects(levels, now)` whenever `PCS[3:0]` change, so a peripheral
framed by its select sees the frame end, and `clock(QspiClock, now)`
when the serial clock changes (CPOL, CPHA, the SCK period,
`baud()`, a `Frequency`).  The one peripheral is the board's whole QSPI side: it
sees every transfer and every level change, whatever they select.
`reset(now)` is
RESET, for the causes `Builder::qspi_reset_scope` names (all by
default).  `qspi_peripheral::<D>()` gives the peripheral back.  A run
quanta at each call, so the peripheral is called in time with the
CPU's cycles to the other peripherals; one that shares nothing with them
and asks for no interrupt or stop can return true from `deferrable()`,
and then the queue runs on without a sync, its calls coming in a batch, in
order and each with its own `now`, where the CPU next sees the QSPI.

```rust
use mc683xx_cpu32::address::Address;
use mc683xx_cpu32::cpu::state::Stateless;
use mc683xx_cpu32::cycles::Cycles;
use mc683xx_cpu32::function_code::FunctionCode;
use mc683xx_cpu32::size::Size;
use mc683xx_modules::peripheral::SpiPeripheral;
use mc683xx_modules::peripheral::QspiTransfer;
use mc683xx_examples::firmware;
use mc683xx_examples::firmware::QSM;

/// Records what it is sent; answers `0xef`.
#[derive(Debug, Default)]
struct Peripheral(Vec<(u8, u16, u8)>);

impl SpiPeripheral for Peripheral {
    fn transfer(&mut self, transfer: QspiTransfer) -> u16 {
        self.0
            .push((transfer.chip_selects, transfer.out, transfer.bits));
        0xef
    }
}

// Its record is the example's, not state.
impl Stateless for Peripheral {}

// PORTQS, PQSPAR:DDRQS, SPCR0, SPCR1, the RAMs (QSMRM 4.2).
const PORTQS: u32 = QSM + 0x15;
const PQSPAR: u32 = QSM + 0x16;
const SPCR0: u32 = QSM + 0x18;
const SPCR1: u32 = QSM + 0x1a;
const RECEIVE_RAM: u32 = QSM + 0x100;
const TRANSMIT_RAM: u32 = QSM + 0x120;
const COMMAND_RAM: u32 = QSM + 0x140;

// PCS[3:0] the QSPI's outputs, idle high; one 8-bit entry to PCS
// 0101 at SPBR 2.
let program = [
    firmware::prologue(),
    firmware::store_byte(PORTQS, 0x78).to_vec(),
    firmware::store_word(PQSPAR, 0x7878).to_vec(),
    firmware::store_word(TRANSMIT_RAM, 0x1234).to_vec(),
    firmware::store_byte(COMMAND_RAM, 0b0101).to_vec(),
    firmware::store_word(SPCR0, 0x8002).to_vec(),
    firmware::store_word(SPCR1, 0x8000).to_vec(),
]
.concat();
let (mut builder, _) = firmware::booting(firmware::rom(&program));
builder.qspi(Peripheral::default());
let mut chip = builder.build();
chip.run_for(Cycles(5000));
assert_eq!(chip.qspi_peripheral::<Peripheral>().unwrap().0, [(0b0101, 0x34, 8)]);
let received = chip.peek(Address(RECEIVE_RAM), Size::Word, FunctionCode::SUPERVISOR_DATA);
assert_eq!(received, Ok(0xef));
```

With MSTR clear the QSPI is a slave and the host is the master:
`set_qspi_slave_select(asserted)` drives PCS0/SS, and
`qspi_slave_shift(mosi, bits, period)` clocks up to 16 bits in, one an
SCK `period` of system clocks, running the chip between them, and
returns what the QSPI drove on MISO (`Shifted { miso, driven }`).

```rust
use mc683xx_cpu32::address::Address;
use mc683xx_cpu32::cycles::Cycles;
use mc683xx_cpu32::function_code::FunctionCode;
use mc683xx_cpu32::size::Size;
use mc683xx_examples::firmware;
use mc683xx_examples::firmware::QSM;

const PQSPAR: u32 = QSM + 0x16;
const SPCR0: u32 = QSM + 0x18;
const SPCR1: u32 = QSM + 0x1a;
const RECEIVE_RAM: u32 = QSM + 0x100;
const TRANSMIT_RAM: u32 = QSM + 0x120;

// MISO an output, MOSI and PCS0/SS inputs; 8 bits, slave; one entry
// (MC68331UM 6.3.5.3).
let program = [
    firmware::prologue(),
    firmware::store_word(PQSPAR, 0x0b01).to_vec(),
    firmware::store_word(TRANSMIT_RAM, 0x00c3).to_vec(),
    firmware::store_word(SPCR0, 0x2000).to_vec(),
    firmware::store_word(SPCR1, 0x8000).to_vec(),
]
.concat();
let (builder, _) = firmware::booting(firmware::rom(&program));
let mut chip = builder.build();
chip.run_for(Cycles(1000));
chip.set_qspi_slave_select(true);
let shifted = chip.qspi_slave_shift(0x5a, 8, Cycles(10));
assert_eq!((shifted.miso, shifted.driven), (0xc3, 0xff));
chip.set_qspi_slave_select(false);
let received = chip.peek(Address(RECEIVE_RAM), Size::Word, FunctionCode::SUPERVISOR_DATA);
assert_eq!(received, Ok(0x5a));
```

## Hooks and tracing

Hooks are closures on the core (`Mc68331::cpu_control()`), each returning
`ControlFlow`: `Break(())` stops the run with `Stop::Hook(id)`.  All
but the exception hook cover an address range, on the address lines.

- `add_code_hook(range, f(&Instruction))`: before each instruction,
  with its PC, the registers and the long at the stack top.  A stop
  leaves it unexecuted; the next run executes it without calling the
  code hooks for it again, so a breakpoint can be stepped past.
- `add_block_hook(range, f(&Instruction))`: before the first
  instruction of each straight-line block.
- `add_read_hook`, `add_write_hook(range, f(&MemoryAccess))`: after
  each operand byte or word cycle (not fetches); a stop takes effect
  when the instruction ends.  The chip's long is two word cycles; a
  bare `Cpu` with `set_long_bus` (`flat::Machine`'s default) shows a
  long operand once, size `Long` at its address, and a long that
  faults midway not at all.
- `add_invalid_hook(range, f(&InvalidAccess))`: before a bus error is
  taken, with its reason, `Invalid::BusError` or `Invalid::Protected`.
- `add_exception_hook(f(&ExceptionEntry))`: once the frame is stacked:
  vector, return PC, handler, stack, level.
- `remove_hook(id)`.

`cpu_control().protect(space, start, length, Permissions)` sets
read, write and execute per 2-Kbyte page in `Space::User` or
`Space::Supervisor`; a cycle a page does not permit is a bus error,
and the invalid hooks see it as `Protected`.  Hooks and permissions
outlive the chip's remaps.  Hooks cost little when absent; a memory
hook makes its pages leave the fast path.  The GDB stub
(`mc683xx-gdb`, `docs/gdb.md`) sets its breakpoints, watchpoints and
steps as hooks like these.

```rust
use std::cell::RefCell;
use std::ops::ControlFlow;
use std::rc::Rc;

use mc683xx_cpu32::address::Address;
use mc683xx_cpu32::cpu::Stop;
use mc683xx_cpu32::cpu::hooks::Invalid;
use mc683xx_cpu32::cycles::Cycles;
use mc683xx_cpu32::memory::PAGE_BYTES;
use mc683xx_cpu32::memory::Permissions;
use mc683xx_cpu32::memory::Space;
use mc683xx_examples::firmware;

// The bus error vector (CPU32RM Table 6-1).
const BUS_ERROR: u8 = 2;
let prologue = firmware::prologue();
let program = [
    prologue.clone(),
    firmware::store_word(firmware::RAM_BASE, 1).to_vec(),
    vec![firmware::BRANCH_TO_SELF],
]
.concat();
let mut image = firmware::rom(&program);
firmware::set_vector(&mut image, BUS_ERROR, firmware::HANDLER);
firmware::patch(&mut image, firmware::HANDLER, &[firmware::BRANCH_TO_SELF]);
let (builder, ram) = firmware::booting(image);
let mut chip = builder.build();

// A breakpoint on the store, after the prologue.
let store = Address(firmware::START + 2 * prologue.len() as u32);
let breakpoint = chip
    .cpu_control()
    .add_code_hook(store..=store, |_| ControlFlow::Break(()));
assert_eq!(chip.run_for(Cycles(10_000)).stop, Stop::Hook(breakpoint));
assert_eq!(chip.cpu().registers.pc, store.0);
chip.cpu_control().remove_hook(breakpoint);

// The RAM's first page read-only: the store faults.
let ram_page = Address(firmware::RAM_BASE);
chip.cpu_control()
    .protect(Space::Supervisor, ram_page, PAGE_BYTES, Permissions::READ)
    .unwrap();
let reasons = Rc::new(RefCell::new(Vec::new()));
let seen = Rc::clone(&reasons);
chip.cpu_control()
    .add_invalid_hook(ram_page..=ram_page, move |access| {
        seen.borrow_mut().push(access.reason);
        ControlFlow::Continue(())
    });
let vectors = Rc::new(RefCell::new(Vec::new()));
let entered = Rc::clone(&vectors);
chip.cpu_control().add_exception_hook(move |entry| {
    entered.borrow_mut().push(entry.vector.0);
    ControlFlow::Continue(())
});
chip.run_for(Cycles(1000));
assert_eq!(*reasons.borrow(), [Invalid::Protected]);
assert_eq!(*vectors.borrow(), [BUS_ERROR]);
assert_eq!(chip.memory(ram)[..2], [0, 0]);
assert_eq!(chip.cpu().registers.pc, firmware::HANDLER);
```

A host on a time base of its own (a board's picoseconds) calls
`set_clock_stop(true)`: a change of the system clock's frequency (a
SYNCR write, a reset) then ends `run_for` right after its instruction,
`Stop::Budget` with budget left, so the host moves its base there and
not at the end of the run.  Off by default, `run_for` spends its budget.
Likewise `set_output_stop(OutputStop)` names the outputs (port pins
by mask, PWMA and PWMB, TXD, RESET) whose change ends `run_for`, after
the instruction that changed one or at the sync point where a
compare, a PWM edge or a frame's start did: for a host whose peripherals
sense them and may answer within the run.  None by default.

Tracing goes through the chip's events (`take_events`), off by
default: `set_tracing(tracing)`, a `ChipTracing::default()` with
`accesses` and `exceptions` set as wanted, adds an
`Event::Access` for each bus cycle the core sends the chip (module
registers, peripherals, memory a peripheral shares a select with; not the RAM
and ROM the core serves, which memory hooks see) and an
`Event::Exception` for each exception entered.  `instructions()`
counts instructions run.  `Event::ArbitrationTie` reports two
interrupt sources with one IARB, which the manual leaves
unpredictable.

```rust
use mc683xx_cpu32::cycles::Cycles;
use mc683xx_examples::firmware;
use mc683xx_examples::firmware::SIM;
use mc683xx_mc68331::event::Event;
use mc683xx_mc68331::event::ChipTracing;

let program = [firmware::prologue(), vec![firmware::BRANCH_TO_SELF]].concat();
let (builder, _) = firmware::booting(firmware::rom(&program));
let mut chip = builder.build();
let mut tracing = ChipTracing::default();
tracing.accesses = true;
tracing.exceptions = true;
chip.set_tracing(tracing);
chip.run_for(Cycles(1000));
let writes: Vec<u32> = chip
    .take_events()
    .into_iter()
    .filter_map(|event| match event {
        Event::Access(access) if access.write => Some(access.address.0),
        _ => None,
    })
    .collect();
// CSBAR0, CSOR0 and SYPCR (MC68331UM Table D-3).
assert_eq!(writes, [SIM + 0x4c, SIM + 0x4e, SIM + 0x20]);
```

## Host bus cycles

The host reaches the bus itself, as a debugger does with BDM's memory
commands (CPU32RM 7), through the chip's own decode: the SIM's, QSM's
and GPT's registers where SIMCR's MM puts them, and whatever a chip
select reaches.  Each takes an `Address` (on the 24 address lines), a
`Size` and a `FunctionCode`; a long is two word cycles, high word
first.  None moves the chip's time or shows in the trace.

- `bus_read(address, size, function_code) -> Result<u32, AccessError>`
  and `bus_write(address, size, function_code, value)` are real
  cycles at `now()`, side effects and all: a read of SCSR arms its
  flags and one of SCDR clears them, a peripheral sees the cycle, a write
  to a chip-select register remaps at once.  A stop a peripheral asks for
  in one is dropped: no run is there to stop.
- `peek(address, size, function_code) -> Result<u32, AccessError>` is
  a look without a cycle: a module's register as a read would find it
  (no flag armed or cleared), memory's bytes as the decode reaches
  them.  What only a cycle can tell, a peripheral's bytes, is
  `AccessError::Unpeekable`.  A module answers through
  `Module::peek`.

```rust
use mc683xx_cpu32::address::Address;
use mc683xx_cpu32::cycles::Cycles;
use mc683xx_cpu32::function_code::FunctionCode;
use mc683xx_cpu32::size::Size;
use mc683xx_examples::firmware;
use mc683xx_examples::firmware::SIM;
use mc683xx_mc68331::host::AccessError;

// CSBAR0, CSOR0, CSBARBT (MC68331UM Table D-3).
const CSBAR0: u32 = SIM + 0x4c;
const CSOR0: u32 = SIM + 0x4e;
const CSBARBT: u32 = SIM + 0x48;
const SUPERVISOR: FunctionCode = FunctionCode::SUPERVISOR_DATA;

// No prologue: the host maps the RAM, as a debugger would.
let (builder, ram) = firmware::booting(firmware::rom(&[firmware::BRANCH_TO_SELF]));
let mut chip = builder.build();
let base = (firmware::RAM_BASE >> 8) | u32::from(firmware::BLOCK_64K);
chip.bus_write(Address(CSBAR0), Size::Word, SUPERVISOR, base).unwrap();
let option = firmware::CSOR_FAST.into();
chip.bus_write(Address(CSOR0), Size::Word, SUPERVISOR, option).unwrap();
let at = Address(firmware::RAM_BASE);
chip.bus_write(at, Size::Long, SUPERVISOR, 0x1234_5678).unwrap();
assert_eq!(chip.memory(ram)[..4], [0x12, 0x34, 0x56, 0x78]);
assert_eq!(chip.bus_read(at, Size::Long, SUPERVISOR), Ok(0x1234_5678));
assert_eq!(chip.peek(Address(CSOR0), Size::Word, SUPERVISOR), Ok(option));
// CSBOOT down to 64 Kbytes: past it nothing answers.
let boot = firmware::BLOCK_64K.into();
chip.bus_write(Address(CSBARBT), Size::Word, SUPERVISOR, boot).unwrap();
let nowhere = Address(0x40_0000);
assert_eq!(chip.peek(nowhere, Size::Word, SUPERVISOR), Err(AccessError::Unmapped));
assert_eq!(chip.now(), Cycles::ZERO);
```

## Save and restore

`Mc68331::save() -> Result<Vec<u8>, StateError>` snapshots the core
(registers, memory, timing), the SIM, the modules, every peripheral and
model the builder added, by its `Save`, in the order added, board time
the QSPI's peripheral (`Builder::qspi`, which is `Save` too), board
time and the IRQ pins; `restore(&[u8])` puts it back into a chip built
the same way.  A peripheral that cannot save fails both, named by its place
in that order (`peripheral 1: the peripheral cannot save its state`,
`StateError::Peripheral`), as does a QSPI peripheral that cannot,
named `the QSPI peripheral`; a snapshot with another count of peripherals, or
with a QSPI peripheral where the chip has none or none where it has
one, is `StateError::Mismatch`.  A restore checks everything before it
changes anything: each peripheral's, the QSPI peripheral's and each
module's snapshot by restoring it, each put back if a later one or the
core refuses, so the chip and its peripherals are unchanged on an error.
Not in it: hooks (which stay), tracing and events not yet taken.  The same snapshot and the same inputs give the
same run.

The bare `Machine` (`mc683xx_cpu32::flat`) does the same with its
peripherals: a `MappedPeripheral` is `Save`, and `Machine::save()` holds each
peripheral's snapshot in the order added, an error naming the peripheral by its
first address (`the peripheral at 0xf00000: ...`).

```rust
use mc683xx_cpu32::cycles::Cycles;
use mc683xx_examples::firmware;

let program = [firmware::prologue(), firmware::delay(1000).to_vec()].concat();
let (builder, _) = firmware::booting(firmware::rom(&program));
let mut chip = builder.build();
chip.run_for(Cycles(500));
let snapshot = chip.save().unwrap();
chip.run_for(Cycles(2000));
let (registers, now) = (chip.cpu().registers.clone(), chip.now());
chip.restore(&snapshot).unwrap();
chip.run_for(Cycles(2000));
assert_eq!(chip.cpu().registers, registers);
assert_eq!(chip.now(), now);
```

## Front ends (TUI, GUI)

A front end (a TUI, a GUI, a logger) is the host's, outside this
repository, and outside the board file, which stays hardware only
(PLAN.md, Boards): it attaches to a loaded `mc683xx_board::board::Board`
and watches it, and moves it only through the inputs the board file
already names.

A board is generic over the chip it is built around
(`mc683xx_board::chip::Chip`): `Board::<Mc68331>::load(&description,
...)` loads one around the MC68331, and `Description::chip_type()`
gives the file's `[chip] type`, for a host that serves several chips
to choose by.  `board.chip()` is the chip itself, an `&Mc68331` with
every method of the chip's.

- **Nets.** `Board::observe(net, observer) -> Result<ObserverId,
  NetError>` attaches an `observe::Observer` (a closure
  `FnMut(&str, Time, Signal)` is one) to a net by name; `NetError::Unknown`
  names a net the board lacks.  It is told each change the net
  carries, at its board time, after the net's sinks, in the order
  they see them, the changes already waiting included (the power-on
  levels, before the first run); a wired-and net's resolved level
  only when it changes.  It is not told the net's present value as
  it attaches: `Board::net_value(net)` gives that, `None` before the
  net's first change; `Board::nets()` gives each net's name and type.
  `Board::unobserve(id)` detaches one and gives it back.  An observer
  drives nothing and cannot stop the run; one that should holds a
  `BoardStopRequest` (`Board::stop_request`).  On a net a chip output
  drives, an observer makes the output sensed: its change ends the
  chip's run, so a stop asked from the observer is taken at the change,
  not up to a quantum later; that costs a sync a change, as a
  peripheral sink does.  Observing changes no
  run: the same inputs give the same snapshot with or without
  observers, which are outside the snapshot, and a net with none
  costs nothing more.  A restore keeps them attached and, once it
  succeeds, tells each its net's restored value, so a UI resyncs:
  `Observer::restored(&mut self, net, at, signal: Option<Signal>)`,
  at the restored board time, `None` before the net's first change;
  nets in file order, a net's observers in the order attached.  It
  defaults to nothing, so a closure is told nothing; a failed restore
  tells no one.
- **Peripherals' state.**  `Board::inspect(name) -> Option<Inspection>`
  gives what a peripheral shows (`Peripheral::inspect`): an
  `inspect::Inspection`, its `fields` each a name, a `Value`
  (`Integer`, `Boolean`, `Real`, `Text`, `Bytes`, or a `Group` of
  fields) and hints, a `unit` and a `Radix` for an integer.  Built by
  chaining (`Inspection::default().register("sr", sr).real("vout",
  volts, "V")`); `get(name)` finds a field.  A peripheral's own type
  defines it, empty by default (an adapted `MappedPeripheral` or `SpiPeripheral`
  model through `adapter::Inspect`, its adapter made `inspecting`);
  the built-in types show their state
  (`docs/board-files.md`) and the stock peripherals their
  registers, FIFOs and levels (`docs/peripherals.md`).  It is
  read-only, outside the snapshot and no part of determinism: what a
  peripheral shows may change between versions.
- **A front end's own peripheral.**  A widget that is a peripheral on the
  board (a lamp the firmware drives, a key it reads) is a peripheral of
  the host's own type, registered and named in the board file as any
  other; the host reads its state back by its type,
  `Board::peripheral::<T>(name)` (and `peripheral_mut`).
- **Input.**  The host drives what the board file names as external
  endpoints (`external.switch`): `Board::externals()` lists them and
  `Board::send(name, signal)` drives one now, between runs, after the
  inputs the control (below) holds, so a queued input never lands
  after a later direct one; `Board::take_received(name)` gives what
  reached one since the last call, with each value's time, and fails
  (`ReceiveError::Endpoint`) for an endpoint no net drives.
- **Control from another thread.**  `Board::control() ->
  control::BoardControl` is the board's thread-safe handle (`Send`,
  `Sync`, cheap to `Clone`; every call gives a handle to the one
  control), for a front end whose event loop is another thread:
  `stop()` asks the run to stop, `send(name, signal) -> Result<(),
  SendError>` queues an input, checked at once as `Board::send` checks
  it, and `set_pacing(pacing) -> Result<(), PacingError>` sets the
  pacing (`pacing()` reads it, `stopping()` says whether a stop waits).
  The run takes what the handle holds at each sync point, on the
  board's own thread: the inputs are driven at that board time in the
  order queued, and the stop ends the run there, `Stop::Requested`.
  Nothing in the chip or a peripheral is touched from the other
  thread.  A stop or input asked between runs is taken at the next
  run's start.  The same inputs at the same board times give the same
  run; an input from a live UI lands at the sync point after it was
  queued, so it is timed by when the UI sent it, which is inherent to
  live input.  A stop waits for the quantum running, which a plain
  `Board::run_until` may make long; a paced run's quanta bound it.
- **Pacing.**  `pacing::Pacing` is `Unpaced` (as fast as the host runs
  it, the default), `RealTime`, or `Scaled(factor)`, board seconds a
  wall second (2.0 twice real time, 0.5 half; finite and above 0,
  else `PacingError::Factor`); it prints and parses as `unpaced`,
  `realtime` or the factor.  `Board::run_until_paced(until, quantum)` runs to
  `until` in quanta of `quantum` picoseconds of board time, a
  `NonZeroU64` so the loop always ends
  (`pacing::QUANTUM`, 5 ms, is a good one), and before each sleeps
  until board time is no longer ahead of wall time times the
  control's factor, waking at once for a stop or a change of pacing;
  a run that reaches `until` sleeps once more after its last quantum,
  so it never returns ahead of wall time.  The wall clock is host-side only: it decides how long to sleep and
  never reaches the board, so a paced run is the same as the unpaced
  run.  A run that falls behind (a slow host) is not held back.  A
  host with a loop of its own (the runner's turns) keeps a
  `pacing::Pacer` and calls `wait(now, &control)` between its runs;
  `Board::run_until_quantized(until, quantum, before)` is the quantum loop alone.

```rust
use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;

use mc683xx_board::board::Board;

use mc683xx_mc68331::chip::Mc68331;
use mc683xx_board::description::Description;
use mc683xx_board::description::Options;
use mc683xx_board::inspect::Value;
use mc683xx_board::registry::Environment;
use mc683xx_board::registry::Registry;
use mc683xx_board::signal::Signal;
use mc683xx_board::time::Time;

// A 1 MHz clock divided by 1000 onto a 1 kHz net to PAI, a switch the
// host moves, and a buffer of tied levels on CS1.
let text = r#"
version = 1

[chip]
type = "mc68331"
[[memory]]
name = "ram"
size = 0x800
attach = [{ select = "CSBOOT" }]
[[peripheral]]
name = "reference"
type = "clock"
parameters = { hz = 1_000_000 }
[[peripheral]]
name = "divider"
type = "divider"
parameters = { divide = 1000 }
[[peripheral]]
name = "switches"
type = "74x244"
attach = [{ select = "CS1" }]
parameters = { levels = 0xa5 }
[[net]]
name = "megahertz"
type = "clock"
from = "reference.out"
to = ["divider.in"]
[[net]]
name = "tick"
type = "edge"
from = "divider.out"
to = ["chip.PAI"]
[[net]]
name = "switch"
type = "logic"
from = "external.switch"
to = ["chip.PGP1"]
"#;
let description = Description::parse(text, Path::new("."), &Options::default()).unwrap();
let mut registry = Registry::with_builtins();
mc683xx_peripherals::register(&mut registry).unwrap();
let mut board = Board::<Mc68331>::load(&description, &registry, Environment::default()).unwrap();
let rises = Rc::new(RefCell::new(Vec::new()));
let seen = Rc::clone(&rises);
board
    .observe("tick", move |_net: &str, at: Time, signal: Signal| {
        if signal == Signal::Level(true) {
            seen.borrow_mut().push(at);
        }
    })
    .unwrap();
board.send("switch", Signal::Level(false)).unwrap();
// 3 ms
board.run_until(Time(3_000_000_000)).unwrap();
assert_eq!(rises.borrow().len(), 3);
assert_eq!(board.net_value("switch"), Ok(Some(Signal::Level(false))));
let switches = board.inspect("switches").unwrap();
assert_eq!(switches.get("inputs"), Some(&Value::Integer(0xa5)));
```

A second thread, a GUI's event loop, presses a key and then stops a
paced run:

```rust
use std::path::Path;
use std::thread;
use std::time::Duration;

use mc683xx_board::board::Board;

use mc683xx_mc68331::chip::Mc68331;
use mc683xx_board::description::Description;
use mc683xx_board::description::Options;
use mc683xx_board::pacing::QUANTUM;
use mc683xx_board::pacing::Pacing;
use mc683xx_board::registry::Environment;
use mc683xx_board::registry::Registry;
use mc683xx_board::signal::Signal;
use mc683xx_board::time::Time;
use mc683xx_cpu32::cpu::Stop;

let text = r#"
version = 1

[chip]
type = "mc68331"
[[memory]]
name = "ram"
size = 0x800
attach = [{ select = "CSBOOT" }]
[[net]]
name = "key"
type = "logic"
from = "external.key"
to = ["chip.PGP1"]
"#;
let description = Description::parse(text, Path::new("."), &Options::default()).unwrap();
let mut board = Board::<Mc68331>::load(&description, &Registry::with_builtins(), Environment::default()).unwrap();
let control = board.control();
control.set_pacing(Pacing::RealTime).unwrap();
let front_end = control.clone();
let ui = thread::spawn(move || {
    thread::sleep(Duration::from_millis(20));
    front_end.send("key", Signal::Level(false)).unwrap();
    thread::sleep(Duration::from_millis(20));
    front_end.stop();
});
// an hour of board time, cut short by the stop
let outcome = board.run_until_paced(Time(3_600_000_000_000_000), QUANTUM).unwrap();
ui.join().unwrap();
assert_eq!(outcome.stop, Stop::Requested);
assert_eq!(board.net_value("key"), Ok(Some(Signal::Level(false))));
```

## Errors

- `BuildError`, from the builder: a pin already wired (`Wired`),
  memory not a whole number of pages (`Length`), or the core's memory
  refusing a region (`Memory`).  An interrupt cannot be wired to level
  0: `IrqPin` holds levels 1 to 7 only.
- `StateError`, from `save` and `restore`: a snapshot of another
  format (`Version`), too short (`Truncated`), undecodable (`Decode`, an opaque `DecodeError`),
  of other hardware (`Mismatch`: address lines, region sizes, module
  count), holding values no run could save (`Invalid`: a register bit
  not implemented, an index past what it indexes), a peripheral that
  cannot save (`Unsupported`), or, from a board, one of these from the
  peripheral it names (`Peripheral { name, error }`).  It is `#[non_exhaustive]`.  A failed
  restore leaves the chip unchanged.  `Mismatch`, `Invalid` and
  `Unsupported` carry a `Cow<'static, str>`, built with
  `StateError::mismatch`, `invalid` and `unsupported` from a literal or
  a `String`, so a model can say what it found.  It is neither `Clone`
  nor `PartialEq`, so that a later variant can hold an `io::Error`: a
  test matches it (`matches!(error, StateError::Invalid(reason) if
  reason == "...")`) or compares its message.
- `MemoryError`, from `protect`: a range not of whole pages, or past
  the address lines.
- `AccessError`, from the host's bus cycles and peeks: nothing
  answers (`Unmapped`), a bus error (`BusError`: a peripheral's BERR, no
  DSACK, the bus monitor), a word or long at an odd address
  (`Misaligned`), a value wider than its cycle (`Width`), or a peek
  only a cycle can answer (`Unpeekable`).
- `BusError` is a peripheral's answer, not a host error: the CPU takes the
  bus error exception, as the firmware sees it.
- Nothing in a run returns an error: a halt is a `Stop`, a reset an
  event.
- Types print with `Display` and parse with `FromStr`: `ChipSelect`
  (`CSBOOT`, `CS0`, any case), `Address` (`0x00001234`, and
  `LowerHex`), `Cycles` (`12 clocks`), `Frequency` (`16777216 Hz`),
  `Vector` and `FunctionCode` (their numbers), and on a board `Time`
  (`1000 ps`), `Endpoint` (`chip.IRQ7`), the MC68331's `mc68331::Pin` (`IRQ7`), `signal::Kind`,
  `Direction`, `peripheral::Role` (`read-write`) and `Lane` (`upper`),
  as the board file names them; `PortName` (`E`) and the GPT's
  `pwm::Channel` (`A`), any case.
  The newtypes convert `From` and into what they
  wrap.
- Every public error enum is `#[non_exhaustive]`, and so are the
  enums that may grow: `Stop`, `Halt`, the bare run's `End` and
  `FaultCause`, `hooks::Invalid`, the GPT's `OutputChange` and
  `pwm::Output`, and the board's `Signal`, `signal::Kind`, `Endpoint`,
  `mc68331::Pin`, `peripheral::Role`, `chip::MemoryPlace` and
  `trace::Record`.  A match on one needs a wildcard arm.  Enums of
  what the hardware fixes (`Size`, `ChipSelect`, `PortName`,
  `pwm::Channel`, `Lane`, `Direction`) are not.
- So are the structs a host reads but does not build: the hook
  payloads (`hooks::Instruction`, `MemoryAccess`, `InvalidAccess`,
  `ExceptionEntry`), `bus::BusAccess`, `peripheral::BusCycle`, the run
  results (`Run`, `LimitedRun`, `FaultStop`, `Intercepted`,
  `BoardRun`), `event::TracedAccess`, the trace's record structs,
  `board::External`, `peripheral::Driving`, and `registry::Constructed`
  and `Context`.  A host that must build one has a constructor:
  `Run::new`, `BoardRun::new`, `BusCycle::new`, `Constructed::new`,
  `Context::new` (to test a constructor).
- The configuration structs (`Limits`, `Deviations`, `BoardTracing`,
  `ChipTracing`, `description::Options`, `registry::Environment`) are
  `#[non_exhaustive]` too, with every field public: build one from
  `X::default()` and set the fields it needs
  (`let mut tracing = BoardTracing::default(); tracing.pins = true;`),
  so a later field takes its default.
- A wrapper error either adds context and prints its cause in its
  message, the cause a field (`error`), not its `source()`, or adds
  none and is `#[error(transparent)]`: a reporter that walks the chain
  prints each cause once.

```rust
use mc683xx_modules::chip_select::ChipSelect;
use mc683xx_mc68331::chip::BuildError;
use mc683xx_mc68331::chip::Builder;

let mut builder = Builder::default();
builder.memory(vec![0; 2048], false, &[ChipSelect::Boot]).unwrap();
assert_eq!(
    builder.memory(vec![0; 2048], true, &[ChipSelect::Boot]),
    Err(BuildError::Wired(ChipSelect::Boot))
);
assert_eq!(
    builder.memory(vec![0; 100], true, &[]),
    Err(BuildError::Length(100))
);
let mut chip = builder.build();
assert!(chip.restore(&[1, 2]).is_err());
```

## What the host cannot do yet

- Master the bus (BR/BG/BGACK) or run more than one CPU on a bus.
