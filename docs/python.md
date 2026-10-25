# Python guide

How to embed the MC68331 in a Python host with the `mc683xx` module:
build a board, run firmware, and connect Python objects as its peripherals.
The module is the Rust host API one to one (`docs/host-guide.md`), with
pins and registers by the manual's names and wiring by number: each
class, method, property, argument and enum has the Rust item's name and
words (`Machine`, `HookId`, `halted`, `request_interrupt(pin)`,
`Ending`), in Python's case.  Where Python's ways differ, the binding
says so (Names, below).  `PLAN.md` (Python) records the decisions.  A
board file (`docs/board-files.md`) describes a whole board as data;
`mc683xx.Board` loads one (Board files, below).

Every Python block here runs, in order, in one namespace, as a test:
`crates/cpu32-py/tests/test_python_doc.py`, part of `make python`,
which also type-checks the blocks with mypy against the module's
stubs.

## Names

A Python name is its Rust item's: CamelCase classes, snake_case
methods, properties and arguments, UPPER_CASE enum members.  A Rust
rename renames the Python name with it.  The deliberate differences:

- **Enums:** a Rust type with `Display` and `FromStr` crosses as its
  string, in and out: a chip select (`"CSBOOT"`, `"CS0"`), a GPT input
  (`"PGP0"`-`"PGP7"`, `"PAI"`, `"PCLK"`), a net type (`"serial"`), a
  port direction (`"in-out"`), a `Pacing` from its text.  Registers are
  by name, Python's own (`"D0"`, `"SR"`).  Any other Rust enum is a
  Python class whose members are its variants (`Channel.A`,
  `PortName.E`, `Role.READ_WRITE`, `Lane.UPPER`, `ResetCause.WATCHDOG`,
  `Space.USER`), and is never taken as a string; one whose variants
  carry data has a class for each (`Event.Reset(now, cause)`,
  `Frame.Data(bits)`, `OutputChange.PortGp(driven, levels)`).
- **Tuples:** a struct of plain values a call returns is a tuple of its
  fields, in order: `PortLevels` as `(driven, levels)`, a port's change
  as `(now, port, driven, levels)`, a transmitted frame as `(start,
  frame)`, a QSPI slave shift's `Shifted` as `(miso, driven)`.
- **Units:** a frequency Rust gives in whole hertz (`Frequency::hz`) is
  an `int` named `_hz` (`system_hz`, `baud_hz`); one it keeps in
  nanohertz (`Frequency` itself: a serial frame's baud, a clock net's
  frequency) is an `int` named `_nanohertz`.  Times are `int`s, system
  clocks or picoseconds.
- **Settings:** a struct of settings is keyword arguments
  (`set_tracing(accesses=, exceptions=)` for `ChipTracing`,
  `Builder(data=, reference_hz=, external_hz=)` for `Configuration`); a
  `ResetScope` is a sequence of `ResetCause`s.
- **Ranges and defaults:** an inclusive address range is `begin, end`,
  both included.  Arguments with defaults come last: a hook's callback
  before its range (all addresses by default), Rust's
  `add_code_hook(range, hook)`; `bus_write(address, size, value,
  function_code=5)`, Rust's `(address, size, function_code, value)`;
  `Builder.memory(contents, pins, writable=True)`, Rust's `(bytes,
  writable, pins)`.  The bare `Machine`'s `map` takes the pages'
  `permissions` too, Rust's `map` then `protect`.
- **Builtins:** an argument Rust names `id` or `bytes`, Python
  builtins, is named for what it holds: `remove_hook(hook)`,
  `unobserve(observer)`, `peripheral(peripheral)`, `write(address,
  data)`.
- **Properties:** a Rust accessor of plain state, with no arguments, is
  a read-only property (`now`, `halted`, `instructions`,
  `exception_taken`, `port_c`, `system_hz`, `qspi_chip_selects`,
  `stops_on_fault`, `deviations`, `pacing`, `stopping`); one that takes
  arguments or computes its value is a method (`registers()`,
  `port_c_outputs()`, `port_gp_outputs()`, `port_qs_outputs()`, the
  levels a port's pins resolve to; `pwm(channel)`, `oc1()`, an output
  worked out from the GPT's registers).  A setter is `set_*`, never a
  writable property.
- **Handles:** memories and peripherals are the numbers `Builder`
  returns, not `MemoryId` and `PeripheralId`; a Rust callback trait
  (`MappedPeripheral`, `SpiPeripheral`, `Peripheral`, `Observer`) is a
  duck-typed object or callable.

Not offered, as a Python host has another way or no use for them:
`Machine::remove_peripheral`, `take_natively_once` (a declined
intercept is taken natively), `clear_resume` (`run_limited(begin=)`),
`set_long_bus` and `instruction_registers` (a hook's `registers`);
`Mc68331::set_clock_stop`, `set_output_stop` and `divert_memory` (a
host's own scheduler's, as the board's); the `_mut` accessors and
`cpu`, `sim`, `qsm` and `gpt` (registers by name, a module's registers
through `bus_read` and `peek`, memory through `write_memory`);
`Builder::system_frequency`; `Board::run_until_quantized`;
`StopRequest::take` (the run loop's).

## Installing

```
make python                # builds .venv, the module, and runs the tests
.venv/bin/python -c 'import mc683xx'
```

The package is built with maturin from `crates/cpu32-py` (abi3, Python
3.10 and later) and ships type stubs (`python/mc683xx/__init__.pyi`,
`py.typed`), so mypy and pyright see every class, method and argument.

Released versions are on PyPI as `mc683xx`, abi3 wheels for Linux,
macOS and Windows, and an sdist (`docs/releasing.md`).

## Firmware for the examples

The examples' programs are a few CPU32 instructions (CPU32RM 4) as
words, laid out in a boot ROM with the reset vectors.  A real host
loads a real image.

```python
import struct

import mc683xx

# The modules' registers with MM set (MC68331UM Table D-1).
SIM, QSM, GPT = 0xFFFA00, 0xFFFC00, 0xFFF900
# SYPCR: zero stops the watchdog (MC68331UM 4.2.10).
SYPCR = SIM + 0x20
# CSOR: both bytes, read and write, supervisor and user space; fast
# termination, or the peripheral's own DSACK (MC68331UM D.3.26).
CSOR_FAST, CSOR_EXTERNAL = 0x7BB0, 0x7BF0
# CSBARx's 64-Kbyte block size (MC68331UM Table 4-21).
BLOCK_64K = 0b011

ROM_BYTES = RAM_BYTES = 64 << 10
RAM_BASE = 0x100000
STACK = RAM_BASE + RAM_BYTES
START, HANDLER = 0x400, 0x800

NOP, BRANCH_TO_SELF, UNMASKED = 0x4E71, 0x60FE, 0x2000


def store_word(address: int, value: int) -> list[int]:
    """MOVE.W #value,address."""
    return [0x33FC, value, address >> 16, address & 0xFFFF]


def store_byte(address: int, value: int) -> list[int]:
    """MOVE.B #value,address."""
    return [0x13FC, value, address >> 16, address & 0xFFFF]


def load_word(address: int, register: int) -> list[int]:
    """MOVE.W address,Dn."""
    return [0x3039 | register << 9, address >> 16, address & 0xFFFF]


def set_sr(value: int) -> list[int]:
    """MOVE #value,SR."""
    return [0x46FC, value]


def delay(count: int) -> list[int]:
    """MOVE.W #count,D3 and DBRA D3 to itself."""
    return [0x363C, count, 0x51CB, 0xFFFE]


def select(number: int, base: int, option: int) -> list[int]:
    """CS`number` at `base`, 64 Kbytes: CSBARx, CSORx (Table D-3)."""
    bar = SIM + 0x4C + 4 * number
    return store_word(bar, base >> 8 | BLOCK_64K) + store_word(bar + 2, option)


# What every example program does first: RAM on CS0, no watchdog.
PROLOGUE = select(0, RAM_BASE, CSOR_FAST) + store_word(SYPCR, 0)


Patches = dict[int, list[int]]


def rom(program: list[int], patches: Patches | None = None) -> bytes:
    """NOPs, the reset vectors, `program` at START, and each address's
    words in `patches` (vectors, handlers)."""
    image = bytearray(struct.pack(">H", NOP) * (ROM_BYTES // 2))
    struct.pack_into(">II", image, 0, STACK, START)
    for address, words in {START: program, **(patches or {})}.items():
        struct.pack_into(f">{len(words)}H", image, address, *words)
    return bytes(image)


def vector(number: int, handler: int) -> Patches:
    """Vector `number`'s table entry (CPU32RM 6.1.3), as a patch."""
    return {4 * number: [handler >> 16, handler & 0xFFFF]}
```

## Building a chip

The host wires memory and peripherals to chip-select pins, `"CSBOOT"` and
`"CS0"` to `"CS10"`, as the board does; the firmware's chip-select
registers decide which cycles reach each (SIMRM 7).  Out of reset only
CSBOOT decodes, so the boot ROM goes there.

```python
builder = mc683xx.Builder()  # DATA[15:0] high, the 32.768 kHz crystal
builder.memory(rom(PROLOGUE + store_word(RAM_BASE, 0x1234)), ["CSBOOT"], writable=False)
ram = builder.memory(bytes(RAM_BYTES), ["CS0"])
chip = builder.build()  # the power-on reset
assert chip.read_register("PC") == START
run = chip.run_for(1000)
assert run.stop == mc683xx.Stop.Budget()
assert chip.read_memory(ram, 0, 2) == b"\x12\x34"
```

- `Builder(data=0xFFFF, reference_hz=None, external_hz=None)`: the
  levels on DATA[15:0] at reset (boot port width, pin assignments;
  MC68331UM 4.6.3), and the clock: a synthesizer reference
  (`reference_hz`, 25000 to 50000, SIMRM 4.4.1; 32768 by default) or,
  with MODCLK low, an external clock (`external_hz`).
- `memory(contents, pins, writable=True)`: RAM or ROM, a whole number
  of 2-Kbyte pages; returns its number for `memory`, `read_memory` and
  `write_memory`.
- `fixed_memory(base, contents, writable=True)`: emulator-only memory
  at a fixed address on no pin, which takes the cycles there no chip
  select matches (the host guide's Fixed memory); a memory number as
  `memory`'s.  `fixed_timing(memory, port=16, wait_states=None)`: its
  port, 8 or 16 bits, and wait states, 0 to 13, `None` fast
  termination (Rust's `fixed_timing` takes a `PortSize` and a
  `Termination`; Python has neither).  `reload_scope(memory, scope)`: the `ResetCause`s that
  put a memory's contents back as built, none by default.
- `peripheral(object, pins)` and `unwired_peripheral(object)` (on no pin) return a
  peripheral number for `interrupt`, `reset_scope` and `Mc68331.peripheral`.
- `interrupt(peripheral, pin)` (IRQ1 to IRQ7 as 1 to 7), `autovector(pin)`,
  `reset_scope(peripheral, scope)`, `qspi(object)`: as in Rust.
- `dsack(peripheral, Dsack.DSACK0 | DSACK1 | BOTH)`: the DSACK pins a
  peripheral drives on an externally terminated select, checked
  against PEPAR at each such cycle (a disagreement is an
  `Event.Dsack(now, DsackFault.Unassigned(select))`, once for each
  peripheral) and sizing the cycle (SIMRM Table 5-2); unstated by
  default.  `qspi_reset_scope(scope)`: the resets that reach the QSPI's
  peripheral, every one by default.
- `stop_request()`, `sync_request()` and `diversion()` are the handles a
  peripheral holds, as in Rust: a `StopRequest` whose `request()` ends
  the run `Stop.Requested()` after the instruction whose cycle asked
  (or at the sync point), a `SyncRequest` whose `request()` ends it
  early as `Stop.Budget()`, the clocks run so far, and a `Diversion`
  whose `divert(memory, peripheral)` sends a memory's cycles to a
  peripheral, by their numbers, until `release(memory)` (a flash
  answering status reads while it programs).
- `build()` spends the builder; a second call raises `RuntimeError`.

Registers are by name: `"D0"`-`"D7"`, `"A0"`-`"A7"`, `"PC"`, `"SR"`,
`"CCR"`, `"USP"`, `"SSP"`, `"VBR"`, `"SFC"`, `"DFC"`, through
`read_register`, `write_register` and `registers()`.

## Running

`run_for(budget)` runs for `budget` system clocks and returns a `Run`,
Rust's `cpu::Run`: `cycles` and `stop`, a `Stop`, whose variants are
classes as Rust's are (`Stop.Budget()`, `Stop.Stopped()`,
`Stop.Halted(halt)`, `Stop.Hook(hook)` with the `HookId` that stopped
it, `Stop.Requested()`), equal by value and matched by pattern (`case
mc683xx.Stop.Hook(hook):`).  A run ends at a halt; one that starts
halted spends its budget as time passes for the SIM, the modules and
the peripherals without the CPU, so the watchdog can reset the chip,
and ends `Stop.Halted` if nothing did.  `now` is the chip's time in system clocks.  Inside a run
the chip syncs at the next event of the SIM, the modules and every
peripheral's `next_event`, as in Rust, and the same inputs at the same chip
times give the same run.  A run ends at the first instruction boundary
at or past its budget, so many short runs drift from one long one; a
host on a schedule runs to times instead.  `run_until(target)` runs to
the first instruction boundary at or after `target` (at once if `now`
already is), so a series of them to rising targets is one long run.
A long run is that series inside: it runs to times 100,000 clocks
apart and runs Python's signal handlers between them, so Ctrl-C
raises `KeyboardInterrupt` within milliseconds, the chip at an
instruction boundary.  A board's `run_for` does the same every 5 ms of
board time, and the bare `Machine`'s every 100,000 instructions.  The GIL
stays held during a run: the chip holds Python objects.

```python
def booting(program: list[int], patches: Patches | None = None) -> tuple[mc683xx.Builder, int]:
    """The board above as a function: the builder and the RAM."""
    builder = mc683xx.Builder()
    builder.memory(rom(PROLOGUE + program, patches), ["CSBOOT"], writable=False)
    return builder, builder.memory(bytes(RAM_BYTES), ["CS0"])


def twin() -> mc683xx.Mc68331:
    return booting(delay(500))[0].build()


one, two = twin(), twin()
one.run_until(5000)
for target in range(100, 5001, 100):
    two.run_until(target)
assert one.now == two.now and one.registers() == two.registers()
```

## Peripherals

A peripheral is any object with `read(cycle) -> int` and `write(cycle,
value)`, and optionally:

- `wait_states(cycle) -> int | None`: on a select with an external
  DSACK, the wait states before the peripheral asserts DSACK, or `None`
  for none at all (the bus monitor ends the cycle).
- `interrupt_request() -> bool`: its interrupt output.  It may change
  only in one of the peripheral's own calls or between runs; the chip
  caches the level between them.
- `reset(now)`: RESET, for a reset in its scope.
- `next_event() -> int | None` and `advance(now)`: its own time.
- `save() -> bytes` and `restore(snapshot)`: its state, in the chip's
  snapshot (Save and restore); both or neither: without both the chip
  cannot save, so that a peripheral never saves what it cannot restore.  A
  peripheral with no state saves `b""`.

The methods but `save` and `restore` are looked up once, when the
object is wired; a missing one takes the trait's default.  `save` and
`restore` are looked up at each snapshot, as a board peripheral's are.  A `BusCycle` carries `select` (the pin's
name), `address`, `size` (1 or 2 bytes), `function_code` (7 for CPU
space) and `now`.  A read's value must fit the cycle.  `raise
mc683xx.BusError()` in `read` or `write` asserts BERR.  Any other
exception (a `KeyboardInterrupt` too) does not fault the bus: the read
answers as undriven data lanes read, 0, the write goes nowhere, the run
stops once that instruction ends (`Stop.Requested()`), and the call that
ran the peripheral (`run_for`, `reset`, `build`) raises it.

The chip holds its peripherals, hooks and QSPI peripheral where Python's
cycle collector sees them, as do `Builder`, `Board` (its peripherals and
their classes) and the bare `Machine`: a harness that holds its chip and
wires or hooks it with its own bound methods is collected once it is
garbage.  A removed hook lets its callable go.

A vectored peripheral answers its own interrupt acknowledge through a
select programmed for CPU space at its level (SIMRM 7.8.1): a
CPU-space byte read at $FFFFF1 + 2 x level, whose value is the vector.

```python
PERIPHERAL_BASE, VECTOR_REGISTER, REQUEST_REGISTER = 0x200000, 1, 3
VECTOR, LEVEL = 0x40, 2
CPU_SPACE = 7
# CS10 for the acknowledge: CPU space type $F at $FFFFFx, the lower
# byte, read, external DSACK, IPL in bits 3:1 (SIMRM 7.8.1, 7.10.3).
CSBAR_ACKNOWLEDGE, CSOR_ACKNOWLEDGE = 0xFFF8, 0x2BC0


class Interrupter:
    """Interrupts when told to; gives its vector; two wait states."""

    def __init__(self) -> None:
        self.vector = 0
        self.requesting = False
        self.acknowledges: list[mc683xx.BusCycle] = []

    def read(self, cycle: mc683xx.BusCycle) -> int:
        if cycle.function_code == CPU_SPACE:
            self.acknowledges.append(cycle)
            self.requesting = False
            return self.vector
        return 0

    def write(self, cycle: mc683xx.BusCycle, value: int) -> None:
        offset = cycle.address & 0xF
        if offset == VECTOR_REGISTER:
            self.vector = value
        elif offset == REQUEST_REGISTER:
            self.requesting = value != 0
        else:
            raise mc683xx.BusError()

    def wait_states(self, cycle: mc683xx.BusCycle) -> int | None:
        return 2

    def interrupt_request(self) -> bool:
        return self.requesting

    def reset(self, now: int) -> None:
        self.vector, self.requesting = 0, False


acknowledge_bar = SIM + 0x4C + 4 * 10
program = (
    select(1, PERIPHERAL_BASE, CSOR_EXTERNAL)
    + store_word(acknowledge_bar, CSBAR_ACKNOWLEDGE)
    + store_word(acknowledge_bar + 2, CSOR_ACKNOWLEDGE | LEVEL << 1)
    + store_byte(PERIPHERAL_BASE + VECTOR_REGISTER, VECTOR)
    + store_byte(PERIPHERAL_BASE + REQUEST_REGISTER, 1)
    + set_sr(UNMASKED)
    + [BRANCH_TO_SELF]
)
handler = store_word(RAM_BASE, 0xBEEF) + [BRANCH_TO_SELF]
builder, ram = booting(program, {**vector(VECTOR, HANDLER), HANDLER: handler})
interrupter = Interrupter()
number = builder.peripheral(interrupter, ["CS1", "CS10"])
builder.interrupt(number, LEVEL)
chip = builder.build()
chip.run_for(5000)
assert chip.read_memory(ram, 0, 2) == b"\xbe\xef", "the handler ran"
assert chip.peripheral(number) is interrupter
[acknowledge] = interrupter.acknowledges
assert (acknowledge.select, acknowledge.address) == ("CS10", 0xFFFFF1 + 2 * LEVEL)
```

A model keeps time beside the CPU; the run syncs at its event:

```python
AUTOVECTOR_7 = 31  # CPU32RM Table 6-1


class Alarm:
    """Asserts its interrupt output from `at` on."""

    def __init__(self, at: int) -> None:
        self.at, self.now = at, 0

    def next_event(self) -> int | None:
        return self.at if self.now < self.at else None

    def advance(self, now: int) -> None:
        self.now = now

    def interrupt_request(self) -> bool:
        return self.now >= self.at


builder, _ = booting(
    set_sr(UNMASKED) + [BRANCH_TO_SELF],
    {**vector(AUTOVECTOR_7, HANDLER), HANDLER: [BRANCH_TO_SELF]},
)
builder.interrupt(builder.unwired_peripheral(Alarm(5000)), 7)
builder.autovector(7)
chip = builder.build()
chip.run_for(4000)
assert chip.read_register("PC") != HANDLER
chip.run_for(4000)
assert chip.read_register("PC") == HANDLER
```

The host drives an IRQ pin itself with `set_interrupt_pin(pin,
asserted)`.  A peripheral's `mc683xx.BusError` is the cycle's bus
error.  Any other exception it raises (a `KeyboardInterrupt` too)
does not fault the bus: the read answers as undriven data lanes do,
0, a write goes nowhere, the run stops once the instruction ends, and
`run_for` raises the exception:

```python
class Broken:
    def read(self, cycle: mc683xx.BusCycle) -> int:
        raise KeyError("not modeled")

    def write(self, cycle: mc683xx.BusCycle, value: int) -> None:
        pass


BUS_ERROR = 2  # CPU32RM Table 6-1
builder, _ = booting(
    select(1, PERIPHERAL_BASE, CSOR_FAST) + load_word(PERIPHERAL_BASE, 0),
    {**vector(BUS_ERROR, HANDLER), HANDLER: [BRANCH_TO_SELF]},
)
builder.peripheral(Broken(), ["CS1"])
chip = builder.build()
try:
    chip.run_for(5000)
    raise AssertionError("run should raise")
except KeyError:
    pass
assert chip.now < 5000
assert chip.read_register("PC") != HANDLER  # no bus error taken
assert chip.run_for(100).stop == mc683xx.Stop.Budget()
```

## Reset

`reset()` is RESET from the board.  Each reset, the power-on one
included, is an `Event.Reset(now, cause)` in `take_events()`, and
reaches the peripherals whose scope holds its `ResetCause`; all by default.

```python
class Counter:
    def __init__(self) -> None:
        self.resets: list[int] = []

    def reset(self, now: int) -> None:
        self.resets.append(now)


builder = mc683xx.Builder()  # no prologue: the watchdog runs
builder.memory(rom([BRANCH_TO_SELF]), ["CSBOOT"], writable=False)
everything, not_watchdog = Counter(), Counter()
builder.unwired_peripheral(everything)
causes = [mc683xx.ResetCause.POWER_ON, mc683xx.ResetCause.EXTERNAL]
builder.reset_scope(builder.unwired_peripheral(not_watchdog), causes)
chip = builder.build()
chip.run_for(512 * 256 + 100)  # the watchdog's 2^9 EXTAL periods
chip.reset()
events = chip.take_events()
assert all(isinstance(event, mc683xx.Event.Reset) for event in events)
assert [event.cause for event in events if isinstance(event, mc683xx.Event.Reset)] == [
    mc683xx.ResetCause.POWER_ON,
    mc683xx.ResetCause.WATCHDOG,
    mc683xx.ResetCause.EXTERNAL,
]
assert (len(everything.resets), len(not_watchdog.resets)) == (3, 2)
```

## Host I/O

The modules' registers are the firmware's; the host drives and reads
pins.  Levels it sets take effect at the chip's time between runs;
what the chip drives within a run comes back as timed changes.

- SCI: `sci_receive(Frame.Data(bits))` or `Frame.Break()`, and
  `take_sci_transmitted() -> [(start, Frame)]`, each frame at its
  start bit.  A frame is the data bits
  as on the line, LSB first, parity included (`frame._0`).
- Ports: `set_port_e_pins`, `set_port_f_pins`, `set_port_qs_pins`;
  `port_c_outputs()`, `port_e_outputs()`, `port_f_outputs()` and
  `port_qs_outputs()` as
  `(driven, levels)` masks, Rust's `PortLevels` as a pair Python
  unpacks; `take_port_changes() -> [(now, PortName, driven,
  levels)]`; `take_qsm_changes() -> [(now, driven, levels)]` for port
  QS (a host with no board takes them, or they gather in the chip).
- GPT: `set_port_gp_pins`, `set_pai`, `set_pclk`; `schedule_gpt_input(at,
  input, level)` with `input` `"PGP0"`-`"PGP7"`, `"PAI"` or `"PCLK"`;
  `port_gp_outputs()`; `pwm(Channel.A | B)` (`Output.Level`,
  `Output.Wave`, `Output.Clock`); `oc1()` (`Output.Clock` under CPROUT,
  else `Output.Level`); `take_gpt_changes() -> [(now, OutputChange)]`,
  `OutputChange.PortGp(driven, levels)` and, while
  `set_report_pwm_edges(channel, True)` asks, `OutputChange.Pwm(channel,
  level)` (PPROUT's clock included), OC1's CPROUT clock in port GP's
  while `set_report_oc1_clock_edges(True)` does.

```python
SCCR0, SCCR1, SCSR, SCDR = QSM + 0x08, QSM + 0x0A, QSM + 0x0C, QSM + 0x0E  # QSMRM 5.2
TE_RE = 0x000C
# PORTE, DDRE, PEPAR (MC68331UM D.3.6 to D.3.8); PE0 an output.
PORTE, DDRE, PEPAR, PE0 = SIM + 0x10, SIM + 0x14, SIM + 0x16, 0x01
# TCTL1:TCTL2, TOC2, TIC1 (GPTRM 3): OC2 (PGP4) toggles, IC1 rising.
TCTL, TOC2, TIC1, OC2 = GPT + 0x1E, GPT + 0x16, GPT + 0x0E, 0x10

program = (
    store_word(SCCR0, 1)
    + store_word(SCCR1, TE_RE)
    + load_word(SCSR, 1)  # TDRE read, then the write clears it (QSMRM 5.2.3)
    + store_byte(SCDR + 1, ord("H"))
    + store_word(PEPAR, 0xFF & ~PE0)
    + store_word(DDRE, PE0)
    + store_word(PORTE, PE0)
    + store_word(PORTE, 0)
    + store_word(TCTL, 0x0101)
    + store_word(TOC2, 0x1000)
)
chip = booting(program)[0].build()
chip.schedule_gpt_input(2000, "PGP0", False)
chip.schedule_gpt_input(3000, "PGP0", True)
chip.run_for(4 * 0x1000 + 100)  # TCNT counts the system clock / 4
assert [frame for _, frame in chip.take_sci_transmitted()] == [mc683xx.Frame.Data(ord("H"))]
E = mc683xx.PortName.E
assert [change[1:] for change in chip.take_port_changes() if change[1] == E] == [
    (E, PE0, 0),
    (E, PE0, PE0),
    (E, PE0, 0),
]
assert chip.take_gpt_changes()[-1] == (4 * 0x1000, mc683xx.OutputChange.PortGp(OC2, OC2))
assert chip.peek(TIC1, 2) == 3000 // 4  # the capture, as a read finds it
assert chip.pwm(mc683xx.Channel.A) == mc683xx.Output.Level(False)
assert isinstance(chip.oc1(), mc683xx.Output.Level)
```

### QSPI

`Builder.qspi(object)` puts a peripheral on the QSPI: `transfer(
transfer) -> int`, given an `mc683xx.QspiTransfer` (`entry`, the queue
entry, 0 to 15, its index in the QSPI RAM, QSMRM 4.3.6; `chip_selects`,
the levels on PCS[3:0]; `out`, the bits out, right-aligned, MSB first;
`bits`; `clock`, the `QspiClock` it is shifted with, its mode and SCK
period; `now`, when it ends, in system clocks), returns the bits in;
optionally
`chip_selects(levels, now)` (every change of the PCS levels),
`clock(QspiClock, now)`, `reset(now)` (each reset in
`Builder.qspi_reset_scope`, every one by default) and `deferrable() -> bool` (whether its
calls may come in a batch where the CPU next sees the QSPI).  As a slave
(MSTR clear) the host is the master: `set_qspi_slave_select(asserted)`
and `qspi_slave_shift(mosi, bits, period) -> (miso, driven)`.

```python
PORTQS, PQSPAR, SPCR0, SPCR1 = QSM + 0x15, QSM + 0x16, QSM + 0x18, QSM + 0x1A
RECEIVE_RAM, TRANSMIT_RAM, COMMAND_RAM = QSM + 0x100, QSM + 0x120, QSM + 0x140


class Peripheral:
    def __init__(self) -> None:
        self.transfers: list[tuple[int, int, int]] = []
        self.clocks: list[mc683xx.QspiClock] = []

    def transfer(self, transfer: mc683xx.QspiTransfer) -> int:
        self.transfers.append((transfer.chip_selects, transfer.out, transfer.bits))
        return 0xEF

    def clock(self, clock: mc683xx.QspiClock, now: int) -> None:
        self.clocks.append(clock)


# Master: PCS[3:0] outputs, one 8-bit entry to PCS 0101 at SPBR 2.
master = (
    store_byte(PORTQS, 0x78)
    + store_word(PQSPAR, 0x7878)
    + store_word(TRANSMIT_RAM, 0x1234)
    + store_byte(COMMAND_RAM, 0b0101)
    + store_word(SPCR0, 0x8002)
    + store_word(SPCR1, 0x8000)
)
builder, _ = booting(master)
peripheral = Peripheral()
builder.qspi(peripheral)
chip = builder.build()
chip.run_for(5000)
assert peripheral.transfers == [(0b0101, 0x34, 8)]
assert chip.peek(RECEIVE_RAM, 2) == 0xEF
assert peripheral.clocks[0].baud_hz == chip.system_hz // 4

# Slave: MISO an output, MOSI and PCS0/SS inputs; 8 bits; one entry.
slave = (
    store_word(PQSPAR, 0x0B01)
    + store_word(TRANSMIT_RAM, 0x00C3)
    + store_word(SPCR0, 0x2000)
    + store_word(SPCR1, 0x8000)
)
chip = booting(slave)[0].build()
chip.run_for(1000)
chip.set_qspi_slave_select(True)
assert chip.qspi_slave_shift(0x5A, 8, 10) == (0xC3, 0xFF)
chip.set_qspi_slave_select(False)
```

## Host bus cycles

The host reaches the bus as a debugger does (BDM's memory commands,
CPU32RM 7), through the chip's decode: the modules' registers where
MM puts them, and whatever a chip select reaches.  `size` is 1, 2 or 4
bytes (a long is two word cycles); `function_code` is FC[2:0], 5
(supervisor data) by default.  None moves `now` or shows in the trace.

- `bus_read(address, size, function_code=5)` and `bus_write(address,
  size, value, function_code=5)`: real cycles at `now`, side effects
  and all (a read of SCDR clears RDRF after one of SCSR; a peripheral sees
  the cycle; a chip-select write remaps at once).  A peripheral's
  exception is raised here, as by `run_for`.
- `peek(address, size, function_code=5)`: a look without a cycle, a
  module's register as a read would find it, memory's bytes through
  the decode.  A peripheral's bytes raise `UnpeekableError`.

```python
CSBARBT = SIM + 0x48  # MC68331UM Table D-3

builder = mc683xx.Builder()
builder.memory(rom([BRANCH_TO_SELF]), ["CSBOOT"], writable=False)
ram = builder.memory(bytes(RAM_BYTES), ["CS0"])
number = builder.peripheral(Interrupter(), ["CS1"])
chip = builder.build()
# CS0 and CS1 mapped by the host, not the firmware
for bar, base in ((SIM + 0x4C, RAM_BASE), (SIM + 0x50, PERIPHERAL_BASE)):
    chip.bus_write(bar, 2, base >> 8 | BLOCK_64K)
    chip.bus_write(bar + 2, 2, CSOR_FAST)
chip.bus_write(RAM_BASE, 4, 0x12345678)
assert chip.read_memory(ram, 0, 4) == b"\x12\x34\x56\x78"
assert chip.bus_read(RAM_BASE + 2, 2) == chip.peek(RAM_BASE + 2, 2) == 0x5678
chip.bus_write(PERIPHERAL_BASE + VECTOR_REGISTER, 1, VECTOR)
wired = chip.peripheral(number)
assert isinstance(wired, Interrupter) and wired.vector == VECTOR
try:
    chip.peek(PERIPHERAL_BASE, 2)
    raise AssertionError("a peripheral cannot be peeked")
except mc683xx.UnpeekableError:
    pass
chip.bus_write(CSBARBT, 2, BLOCK_64K)  # CSBOOT down to 64 Kbytes
try:
    chip.bus_read(0x400000, 2)
    raise AssertionError("nothing answers there")
except mc683xx.BusError:
    pass
assert chip.now == 0
```

## Hooks, protection and tracing

`add_code_hook(callback, begin=0, end=0xFFFFFFFF)` and its siblings
`add_block_hook`, `add_read_hook`, `add_write_hook`, `add_invalid_hook`
(both ends of the range included), and `add_exception_hook(callback)`,
call a Python callable with what the Rust hook sees: an `Instruction`
(`pc`, `registers`, `stack_top`), a `MemoryAccess` (`address`, `size`,
`function_code`, `value`; on the chip a long operand is two word
cycles, on the bare `Machine` one access of 4), an `InvalidAccess` (those, `cycle`, a
`CycleKind`, and `reason`, an `Invalid`: `BUS_ERROR` or `PROTECTED`)
or an `ExceptionEntry` (`vector`, `pc`, `handler`, `stack`, `level`).
Each returns a `HookId` for `remove_hook`.  A truthy result stops the
run (`Stop.Hook(hook)`); a code hook's stop leaves its
instruction unexecuted, and the next run goes past it.  A hook's
exception stops the run and is raised by `run_for`.  The run holds the
chip, so a hook that calls into it raises `RuntimeError`; what it
needs comes in its argument.

`protect(space, start, length, permissions)` sets whole 2-Kbyte pages
of `Space.USER` or `Space.SUPERVISOR` to a string of `r`, `w` and `x`;
a cycle they do not permit is a bus error, `Invalid.PROTECTED` to the
invalid hooks.

`set_tracing(accesses=, exceptions=)` adds `Event.Access` for each bus
cycle that reaches the chip (not the RAM and ROM the core serves) and
`Event.Exception` for each exception entered; `instructions` counts
instructions.

```python
store = START + 2 * len(PROLOGUE)
builder, ram = booting(
    store_word(RAM_BASE, 1) + [BRANCH_TO_SELF],
    {**vector(BUS_ERROR, HANDLER), HANDLER: [BRANCH_TO_SELF]},
)
chip = builder.build()
breakpoint = chip.add_code_hook(lambda instruction: True, store, store)
run = chip.run_for(10_000)
assert (run.stop, chip.read_register("PC")) == (mc683xx.Stop.Hook(breakpoint), store)
assert chip.remove_hook(breakpoint)

reasons: list[mc683xx.Invalid] = []
vectors: list[int] = []
chip.protect(mc683xx.Space.SUPERVISOR, RAM_BASE, 2048, "r")
chip.add_invalid_hook(lambda access: reasons.append(access.reason), RAM_BASE, RAM_BASE)
chip.add_exception_hook(lambda entry: vectors.append(entry.vector))
chip.set_tracing(exceptions=True)
chip.run_for(1000)
assert reasons == [mc683xx.Invalid.PROTECTED]
assert vectors == [BUS_ERROR]
assert chip.read_memory(ram, 0, 2) == b"\x00\x00"
assert any(isinstance(event, mc683xx.Event.Exception) for event in chip.take_events())
```

## Save and restore

`save() -> bytes` is the chip's whole state; `restore(snapshot)` puts
it back into a chip built the same way.  Each peripheral and model is in
it, in the order added, by its own `save() -> bytes` and
`restore(snapshot)` (its bytes in its own format), as a Python board
peripheral is; one without both (`save` alone, say) makes `save` and
`restore` raise `StateError` naming it by its peripheral number (`peripheral 0:
a Python peripheral cannot save its state`), and an exception `save` or
`restore` raises is raised, a `KeyboardInterrupt` too, the chip
unchanged.  The QSPI peripheral (`Builder.qspi`) is in it the
same way, one without `save` and `restore` named `the QSPI peripheral`
(`the QSPI peripheral: a Python peripheral cannot save its state`).  A snapshot
with another count of peripherals, or with a QSPI peripheral where the chip
has none or none where it has one, raises `StateError`; on any error
the chip and its peripherals are unchanged.  Hooks stay across a
restore.

```python
chip = booting(delay(1000))[0].build()
chip.run_for(500)
snapshot = chip.save()
chip.run_for(2000)
after = (chip.now, chip.registers())
chip.restore(snapshot)
chip.run_for(2000)
assert (chip.now, chip.registers()) == after
```

## Debugging with gdb

`serve_gdb(address="127.0.0.1:1234", ready=None)`, on `Mc68331`,
`Board` and the bare `Machine`, hands the target to gdb mid-run
(`docs/gdb.md`): it listens at `address` (port 0 picks one), calls
`ready(port)` once it does (the target not yet held, so `ready` may
read it), takes one gdb, and lets it read and write
registers and memory, set breakpoints and watchpoints, step, continue
and interrupt, until it detaches (`Ending.DETACHED`) or kills the target
(`Ending.KILLED`), an enum.  Then the harness has the target back, as gdb left it.
Peripherals and hooks run under gdb as in `run_for`; an exception one
raises, or Ctrl-C in Python (`KeyboardInterrupt`, while gdb is
connected or not, idle or running the target), ends the session, the
stub's breakpoints and watchpoints removed, and is raised by
`serve_gdb`.  While it waits for gdb it releases the GIL, so a client
in another thread of the harness is served.  Memory goes through peeks, never a peripheral's cycle.  A
harness hands over with `ready=lambda port: print(f"target remote
:{port}")` and then `target remote :PORT` in gdb; here a stand-in for
gdb, a few packets from a subprocess, reads the registers and
detaches.  It reads each reply through its checksum, as gdb does: the
stub writes a reply byte by byte, so a client that closes at the `#`
may make `serve_gdb` raise `BrokenPipeError`.

```python
import subprocess
import sys

STAND_IN = """
import socket, sys
with socket.create_connection(("127.0.0.1", int(sys.argv[1]))) as gdb:
    for packet in (b"$g#67", b"$D#44"):  # read the registers, detach
        gdb.sendall(packet)
        reply = b""
        while b"#" not in reply[:-2]:  # the reply to its checksum
            reply += gdb.recv(4096)
"""
stand_ins: list[subprocess.Popen[bytes]] = []


def ready(port: int) -> None:
    stand_ins.append(subprocess.Popen([sys.executable, "-c", STAND_IN, str(port)]))


chip = booting(delay(1000))[0].build()
chip.run_for(500)
assert chip.serve_gdb("127.0.0.1:0", ready=ready) == mc683xx.Ending.DETACHED
assert stand_ins[0].wait(timeout=30) == 0
chip.run_for(500)
```

## Errors

- `ValueError`: a bare `Machine`'s map over another or of peripheral pages, or
  overlapping peripherals; a bad pin, register, level, channel, GPT input,
  permission string, frame or shift; wiring a pin twice; memory not of
  whole pages; a fixed memory off a page boundary, past ADDR[23:0] or
  overlapping another; a register value that does not fit; a host cycle of a
  size other than 1, 2 or 4, at an odd address, of a function code
  past 7, or of a value wider than it; a bare `Machine`'s `read` of more
  bytes than 32 bits of addresses.
- `mc683xx.BusError`: a host cycle that ended in a bus error, or that
  nothing answers; a bare `Machine`'s `read` or `write` of bytes no map
  covers.
- `mc683xx.UnpeekableError`: a peek only a real cycle can answer.
- `mc683xx.StateError` (a `ValueError`): a snapshot of another format
  or other hardware, another count of peripherals, bytes that do not
  decode, or values no run could save; a peripheral that
  cannot save, named.
- `NotImplementedError`: a module that cannot save.
- `IndexError`: an unknown memory or peripheral number, or a memory range
  past its end.
- `OverflowError`: a run past the end of time.
- `RuntimeError`: a spent builder, a hook calling into the chip, a
  bare `Machine`'s hook calling `run_limited` or `serve_gdb`, or gdb sending what
  the stub cannot take.
- `OSError`: `serve_gdb` could not listen, or gdb's connection failed
  or closed without a detach.
- `TypeError`: a peripheral without `read` and `write`, a QSPI peripheral
  without `transfer`, a hook that is not callable.

```python
builder = mc683xx.Builder()
builder.memory(bytes(2048), ["CSBOOT"])
for mistake in (
    lambda: builder.memory(bytes(2048), ["CSBOOT"]),
    lambda: builder.memory(bytes(100), ["CS1"]),
    lambda: builder.interrupt(0, 0),
):
    try:
        mistake()
        raise AssertionError("should raise")
    except (ValueError, IndexError):
        pass
chip = builder.build()
try:
    chip.restore(b"\x01\x02")
    raise AssertionError("should raise")
except mc683xx.StateError:
    pass
```

## Board files

`mc683xx.Board(path, types={...}, options=[...], firmware=None,
images=None, seed=0, work_dir=None, parameters=None)` loads a board
file (`docs/board-files.md`) through its power-on reset, `parameters`
(`{peripheral: {key: value}}`, values a `bool`, `int`, `float`, `str`,
`list` or mapping) set over the file's and checked as they are;
`types`, `images` and `parameters` take any mapping, a `dict` or
another.  The board is built around the MC68331 (Rust's
`Board<Mc68331>`): a file whose `[chip] type` names another chip raises
`ValueError`.  Its peripheral types
are the built-in ones and `types`' classes, by the name the file's
`type` gives.  A class declares its ports in `PORTS`, a list of
`(name, kind, direction)`, as Rust's `PortSpec` (a net type; `"in"`,
`"out"` or `"in-out"`), and is called with the peripheral's
`parameters` as keyword arguments.
Its object has any of the peripheral's methods, each optional:
`read(access, drive) -> int`, `write(access, value, drive)`,
`wait_states(access) -> int | None`, `interrupt_request() -> bool`,
`power_on(drive)`, `reset(now, drive)`, `transfer(transfer, drive) ->
int` (an `mc683xx.Transfer`: `attachment`, `entry`, `chip_selects`,
`out`, `bits`, `clock`, the `QspiClock` it is shifted with, its mode,
`starts_frame`, whether it is the first since its select asserted, and
`now`, when it ends; the Rust edge walk, `mc683xx_board::spi`, is not
exposed, so a Python peripheral reckons its edges from `clock.polarity` and
`clock.phase` by the rules docs/board-files.md, SPI edges, gives),
`qspi_clock(clock, now)` (the QSPI's serial
clock, as the QSPI tells it: at the first entry and after CPOL, CPHA,
SPBR or the system clock change), `select(attachment,
selected, now, drive)`, `input(port, value, now, drive)`,
`sample(port, now) -> float | None`, `next_event() -> int | None`,
`advance(now, drive)`, `qspi_deferrable() -> bool` (asked once, as
the board is built: whether its QSPI calls may come in a batch, as
`Peripheral::qspi_deferrable` says, docs/board-files.md), `memory_read(memory,
access, drive) -> int` and
`memory_write(memory, access, value, drive)` (a memory it took),
`save() -> bytes` and `restore(snapshot)`.  Times are
board time in picoseconds; a `PeripheralAccess` carries `attachment`,
`select` (the chip select's name), `role` (a `Role`), `lane` (a
`Lane`), `address`, `size`, `function_code` and `now`.  A `Drive`
drives outputs by port index (`drive(port, value)`, `drive_at(at,
port, value)`), reads an analog input (`sample(port)`, its value as
the call began, `None` if nothing drove it), writes a board memory
(`write_memory`), takes one off the fast path and gives it back
(`take_memory`, `release_memory`) and stops the run (`stop()`).  Values are a `bool` for a level, a serial frame (below), an
`int` for a bus's word, a `float` for an analog value, and `(at,
nanohertz, phase)` for a clock.  A serial frame is an
`mc683xx.Serial`, as Rust's `signal::Serial`: `Serial(frame,
baud_nanohertz, bits)` reckons its length from the bits on the line,
start and stop included (Rust's `Serial::new`), `Serial.timed(frame,
baud_nanohertz, length)` gives it in picoseconds from start bit to the
end of the stop bits, and `Serial.untimed(frame)` has neither; it has
`frame`, `baud_nanohertz`, `length` and `end(start)`, when it ends.  An
untimed frame may also be an `int` for its data (`None` a break).  A peripheral is
told frames as `Serial`s if its class sets `TIMED_SERIAL = True`, else
their data alone.  A peripheral's `BusError` in `read`, `write` or
`memory_read` is the cycle's bus error; any other exception (a
`KeyboardInterrupt` too) does not fault the bus, the read answering 0
as undriven data lanes do and the write going nowhere.  A peripheral's
exception stops the run after the instruction running, or before the
next quantum for one raised in `next_event` and the other methods asked
between the chip's runs, and `run_for` raises it (a `KeyboardInterrupt`
before any other error, that error its `__context__`); a peripheral whose `advance`
leaves its `next_event` at or before the time it was given, 1000
times running, stalls the board, and `run_for` raises `RuntimeError`
naming it.

`run_for(seconds=...)` (finite, 0 or more, or `ValueError`) or
`run_for(picoseconds=...)` gives a `BoardRun` (Rust's: `stop`, a `Stop`, and `now`, board time at its end), as does `run_until` with a board time (Rust's `Board::run_until`; `run_for` is `Board::run_for`); `now`
(board time in picoseconds), `cycles` and `instructions` (the chip's
system clocks and instructions since power-on);
`peripheral(name)` gives a Python peripheral's object back; `chip` the
board's chip, a `BoardChip` with the `Mc68331` methods that read or
hook it (registers, `bus_read`, `bus_write`, `peek`, the hooks,
`protect`, what the pins drive, `now`, `halted`, `instructions`),
Rust's `Board::chip`; `take_events()` what the chip did, as
`Mc68331.take_events`; `marking(name)` the
peripheral's `marking`, or `None`; `memory(name)`,
`externals()` (`External`s: `name`, `kind` the nets' type, `direction`
`in`, `out` or `in-out`; each is its name as a string and equals it),
`send(name, value, kind="serial")`,
`take_received(name, timed=False)` (frames as `Serial`s if `timed`;
`ValueError` for an endpoint no net drives, as `send` for one that
drives none),
`set_tracing(bus_cycles=, exceptions=, pins=,
qspi=, nets=)`, `watch_code(pc)` (a `HookId`, which `chip.remove_hook`
removes) and `take_trace()` (the
`docs/tracing.md` lines) follow the Rust `Board`; `save()` and
`restore(snapshot)` need every peripheral to save: a Python peripheral saves with
its own `save() -> bytes` and `restore(snapshot)`, its bytes inside
the board's snapshot, and without both the board's save raises
`StateError` naming the peripheral (`probe: a Python peripheral cannot save its
state`).  An exception either raises from the board's call.
`send` raises `ValueError` for an endpoint that drives no net, or none
of `kind`.  A frame sent as an `int` or `None` is untimed; one received without
`timed` drops its baud and length.

## Front ends (TUI, GUI)

A front end is the host's, outside the board file, which stays
hardware only (PLAN.md, Boards), as in Rust (`docs/host-guide.md`,
Front ends):

- `Board.observe(net, callback, timed=False) -> ObserverId` calls
  `callback(net, time, value)` at each change the net carries from
  now on, at its board time in picoseconds, after the net's sinks,
  in the order they see them, the changes already waiting included
  (the power-on levels, before the first run); values as
  `take_received` gives them.  It is not called with the net's
  present value as it attaches: `net_value(net, timed=False)` gives
  that, `None` before the net's first change, and `nets()` each net's
  `(name, type)`.  `unobserve(observer)` detaches one and says
  whether it was attached.  A net the board lacks raises
  `ValueError`.  `on_restore`, a keyword argument, is called
  `on_restore(net, time, value)` once after each successful
  `restore`, with the net's restored value (`None` before its first
  change) at the restored time, so a UI resyncs (Rust's
  `Observer::restored`): nets in file order, a net's observers in the
  order attached.  An exception it raises is raised by `restore`, the
  board restored; it stops nothing.  An exception the callback raises stops the run at
  that change and `run_for` raises it, as a peripheral's does; a callback
  must not call the board (it is in its run), and it drives nothing.
  One that should stop the run without raising holds
  `Board.stop_request()`, a `BoardStopRequest` (Rust's
  `Board::stop_request`), and calls its `request()`: the run ends at
  that change, `Stop.Requested()`, where an exception would have ended
  it.  It is for the board's own thread (`unsendable`) and holds the
  board weakly.  `BoardControl.stop()` is the other thread's: it is
  taken at the next sync point, up to a quantum later.
  Observing changes no run, and the board's collector methods reach
  the callbacks.
- `inspect(name)` gives what a peripheral shows (Rust's
  `Board::inspect`) as a dict, a field's name to its value, an `int`,
  `bool`, `float`, `str`, `bytes` or, for a group, a dict; `None` for
  a peripheral the board lacks.  Rust's unit and radix hints are
  dropped.  A Python peripheral shows its own by defining `inspect()`,
  returning such a mapping (an empty one without it); an exception it
  raises, or a value of another type, raises from `inspect`, and an
  `int` outside 64 bits signed raises `OverflowError`.  It is
  read-only and outside the snapshot.
- A front end's own peripheral is a Python peripheral class of its
  own, named in the board file; `peripheral(name)` gives its object.
- Input goes through the board's external endpoints: `externals()`
  and `send(name, value, kind=...)`, between runs, after the inputs
  the control holds, so a queued input never lands after a later
  direct one.
- `control() -> BoardControl` is the board's thread-safe handle
  (Rust's `Board::control`), for a UI thread: `stop()`, `send(name,
  value, kind="serial")`, which raises `ValueError` at once as
  `Board.send` would, and `set_pacing(pacing)`; the properties
  `pacing` and `stopping`.  The run takes what it holds at each sync
  point: the inputs driven there in the order queued, the stop ending
  the run (`Stop.Requested()`).  An input from a live UI lands at the
  sync point after it was queued, timed by when the UI sent it.
- `Pacing` is Rust's `pacing::Pacing`: `Pacing.UNPACED` (the default),
  `Pacing.REAL_TIME`, `Pacing.scaled(factor)` (board seconds a wall
  second; not finite or not above 0 raises `ValueError`), or
  `Pacing(text)` from `unpaced`, `realtime` or a factor; `factor` is
  `None` unpaced.  It compares and hashes by value, so it may key a
  dict (`scaled(1.0)` is not `REAL_TIME`).  `run_for(seconds=..., pacing=None,
  quantum_picoseconds=5_000_000_000)` is Rust's `Board::run_until_paced`: it
  runs in quanta, paced by the control, releases the GIL before each
  quantum, paced or not, and sleeps between quanta with the GIL released, so another thread
  runs (a UI's `stop` reaches an unpaced run too), and Python's signal handlers run between sleeps of at most
  20 ms, so Ctrl-C raises `KeyboardInterrupt` promptly.  The GIL is
  held while a quantum runs, and the board itself is not for another
  thread (it is `unsendable`): a UI thread holds the control alone.
  A run that reaches its end waits out its last quantum too, so it
  never returns ahead of wall time, and short paced runs in a loop are
  paced.  The wall clock decides only how long the run sleeps: a paced run is
  the same as the unpaced one.  `pacing` paces that call alone: the
  control's pacing is set for the run and put back when it returns or
  raises, unless a `set_pacing` from another thread changed it during
  the run, which then stays (a UI's choice outlives the call).

```python
import tempfile
from pathlib import Path

FRONT_END_BOARD = """
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
"""

with tempfile.TemporaryDirectory() as directory:
    path = Path(directory) / "board.toml"
    path.write_text(FRONT_END_BOARD)
    front = mc683xx.Board(str(path))
rises: list[int] = []
front.observe("tick", lambda net, at, value: rises.append(at) if value is True else None)
front.send("switch", False, kind="logic")
front.run_for(seconds=0.003)
assert len(rises) == 3
assert front.net_value("switch") is False
assert front.inspect("switches") == {"inputs": 0xA5}

import threading
import time

control = front.control()


def ui() -> None:
    """A UI thread: a key press, then a stop."""
    time.sleep(0.02)
    control.send("switch", True, kind="logic")
    time.sleep(0.02)
    control.stop()


thread = threading.Thread(target=ui)
thread.start()
# an hour of board time at real time, cut short by the stop
paced = front.run_for(seconds=3600.0, pacing=mc683xx.Pacing.REAL_TIME)
thread.join()
assert paced.stop == mc683xx.Stop.Requested()
assert front.net_value("switch") is True
```

## Bare CPU

`mc683xx.Machine` is a CPU32 with no chip around it: flat RAM mapped at
any address, Python peripherals at byte ranges, interrupt requests, and
hooks that may read and write the CPU from inside a run.  It is for
function-level firmware tests and hosts that are not a 68331 board
(`PLAN.md`, Bare CPU).  The address bus is 32 bits wide by default
(`Machine(address_bits=24)` for a 68331's), so high addresses neither alias
nor fold into a fault's address.  Defaults are the CPU32's behavior;
everything else is asked for by name.

```python
CALL, CALL_RETURN, CALL_STACK = 0x31400000, 0x800000, 0x8000
MOVEQ_5_D0, ADDQ_1_D0, RTS = 0x7005, 0x5280, 0x4E75

bare = mc683xx.Machine()
bare.map(0, 64 << 10)  # RAM, zero, readable, writable, executable
code = struct.pack(">3H", MOVEQ_5_D0, ADDQ_1_D0, RTS)
bare.map(CALL, 2048, permissions="rx", contents=code)
bare.write(CALL_STACK - 4, struct.pack(">I", CALL_RETURN))
bare.write_register("SR", 0x2700)
bare.write_register("A7", CALL_STACK - 4)
limited = bare.run_limited(begin=CALL, until=CALL_RETURN)
assert limited.end == mc683xx.End.Until()
assert bare.read_register("D0") == 6
```

- **Memory:** `map(start, length, permissions="rwx", contents=None)`
  maps RAM in whole 2-Kbyte pages (Unicorn's 4K pages are two), in
  user and supervisor space alike, never over another map;
  `unmap(start, length)`, which frees the bytes (the rest of a map
  partly unmapped stays, with its permissions), `protect(start,
  length, permissions)`.  A hook's callable may map and unmap.  `restore`
  needs the maps the snapshot was saved with.
  `read(address, length) -> bytes` and `write(address, data)` reach
  mapped bytes whatever their permissions, with no bus cycle;
  unmapped bytes raise `BusError`, and a `read` longer than 32 bits of
  addresses `ValueError`, before anything is allocated.
- **Peripherals:** `add_peripheral(begin, end, peripheral)` puts an object with
  `read(cycle) -> int` and `write(cycle, value)`, and `save() -> bytes`
  and `restore(snapshot)` if it saves (both or neither), at addresses `begin`
  to `end`, both included, byte-granular, no two overlapping.  A cycle
  no mapped memory serves and one peripheral covers goes to it, before it
  lands: the read's value is the peripheral's, the write is the peripheral's
  to keep.  A `BusCycle` has `address` (32 bits), `size` (1, 2, or 4: a
  long operand the peripheral covers whole comes once, as 4), `function_code`,
  `now` and `stacking`.  Raising `BusError` is a bus error; any other
  exception does not fault the bus (a read answers 0, a write goes
  nowhere), stops the run, and `run_limited` (or `reset`, for the
  vectors' reads) raises it.  A peripheral cannot call into the `Machine`: the run
  holds it.  No peripheral sees a CPU-space cycle
  (function code 7): the `Machine` acknowledges LPSTOP's broadcast itself,
  and any other, a BKPT's acknowledge among them, is a bus error, so
  BKPT is an illegal instruction.
- **Runs:** `run_limited(begin=None, until=None, instructions=None,
  cycles=None) -> LimitedRun`, Rust's `bare::LimitedRun`: `cycles` and
  `end`.  `until` is checked at each instruction
  boundary, before an interrupt is taken or the hooks there are
  called; `begin` sets PC and forgets a hook stop's skip, so the
  hooks at `begin` fire even if the last run stopped there.  `end`,
  an `End` as Rust's, says why it ended: `End.Until()`,
  `End.Instructions()`, `End.Fault(fault)` (a `FaultStop`), or
  `End.Stop(stop)` with a chip's `Stop`: `Budget()` (cycles),
  `Hook(hook)`, `Requested()` (`stop_request().request()`, from a
  callable or a peripheral), `Stopped()` (a `stop` or `lpstop`
  instruction: with no `cycles` it ends the run at once), or
  `Halted(halt)`.  Rust's `End::Intercepted` never ends a Python run:
  `run_limited` answers an intercepted exception within it (below).  A
  stop requested between runs is withdrawn as the next starts.
- **Interrupts:** `request_interrupt(pin, vector=None)` requests IRQ1
  to IRQ7 as 1 to 7, answered by `vector` or the autovector; it stays
  requested, as a level-sensitive line does, until `clear_interrupt(pin)`.
- **Also:** `reset()` (the reset exception from the vectors at zero),
  `read_register`, `write_register`, `registers()`, `instructions`,
  `now`, `halted`, `exception_taken`, `save()` and `restore(snapshot)`
  (registers, memory, clock, requests and each peripheral, in the order
  added, by its `save()` and `restore(snapshot)`, as on `Mc68331`: one
  without them raises `StateError` naming it by its first address, `the
  peripheral at 0xf00000: ...`; not hooks or settings).

### Hooks that change the CPU

The hooks are the chip's (`add_code_hook`, `add_block_hook`,
`add_read_hook`, `add_write_hook`, `add_invalid_hook`,
`add_exception_hook`, `remove_hook`), but on the bare `Machine` a callable
may read and write registers and memory, map, protect and stop the
run (`stop_request().request()`), as a Unicorn hook does; a truthy
result stops it too.  It
cannot `run_limited`.  A hook stops the core's run where it fires; `run_limited` calls
the callables, in order, with nothing held, and goes on.  So when a
write takes effect:

| Hook | Called | A register or memory write |
| --- | --- | --- |
| code, block | before the instruction | is seen by it; a PC write redirects, the instruction at the old PC not run, and the hooks at the new PC are called |
| read, write | once the instruction has completed (a write hook sees memory written) | lands after the instruction, before the next |
| invalid access | once the bus error exception is taken, PC the handler's (or the run stopped for it, with `stop_on_fault`) | lands before the handler's first instruction |
| exception | once the frame is stacked and PC is the handler's | lands before the handler runs; writing SSP, SR and PC from the frame returns past it |
| intercept callback | before anything is stacked, PC at the instruction | lands before the CPU goes on from PC |

A read or write hook sees a long operand once, `size` 4 at its
address, as a peripheral and a Unicorn hook do (memory or peripheral, MOVEM.L
a long per register); a hook matches by that address.  A long whose
second word faults is not seen at all; the invalid access hook sees
the faulting word.

`MemoryAccess` and `InvalidAccess` carry `pc`, the instruction's
address, `stacking`, whether exception processing is stacking a frame
(flagged, not hidden), and `registers`, as the instruction began.

```python
calls: list[int] = []


def stub(instruction: mc683xx.Instruction) -> None:
    """A stubbed routine: D0 = 99, then return as RTS would."""
    calls.append(instruction.pc)
    sp = bare.read_register("A7")
    (back,) = struct.unpack(">I", bare.read(sp, 4))
    bare.write_register("D0", 99)
    bare.write_register("A7", sp + 4)
    bare.write_register("PC", back)


stubbed = bare.add_code_hook(stub, CALL, CALL)
bare.write(CALL_STACK - 4, struct.pack(">I", CALL_RETURN))
bare.write_register("A7", CALL_STACK - 4)
assert bare.run_limited(begin=CALL, until=CALL_RETURN).end == mc683xx.End.Until()
assert bare.read_register("D0") == 99 and calls == [CALL]
bare.remove_hook(stubbed)
```

### Exceptions and faults

`set_intercepted(vector, True)` intercepts the exceptions an
instruction raises with `vector` (traps, illegal, line A and F,
privilege, divide by zero, CHK, TRAPV, format error) before anything
is stacked, and `set_intercept_callback(callback)` answers them:
`callback(Intercepted)` gets the `vector` and the instruction's `pc`,
where PC is, its (An)+ and -(An) undone.  A truthy result means it
handled it, and the run goes on from PC as the callable left it;
otherwise, or with no callback (`set_intercept_callback(None)`), the
CPU takes it natively.  `set_stop_on_fault(True)` (`stops_on_fault`)
stops the run on a bus or address error instead of taking it:
`End.Fault(fault)`, its `FaultStop` giving the
`cycle` (`CycleKind`), the `cause` (`FaultCause.BUS_ERROR` for
unmapped or a peripheral's, `PROTECTED`, `ADDRESS_ERROR`), the whole
`address`, `size`, `function_code`, `pc` and `registers` as the
instruction began, which the CPU holds again, nothing stacked.  A
fault while stacking, a bad SSP, stops it the same way rather than
halting the CPU, so the host fixes SSP and runs on.

```python
TRAP_1, TRAP_7 = 0x4E41, 0x4E47
bare.write(CALL, struct.pack(">3H", TRAP_1, TRAP_7, RTS))
bare.write(4 * (32 + 7), struct.pack(">I", 0x4000))  # trap #7's handler
bare.write(0x4000, struct.pack(">H", RTS))


def trap_1(intercepted: mc683xx.Intercepted) -> bool:
    """Trap #1 as a stubbed kernel call; trap #7 goes to its handler."""
    bare.write_register("D0", 1)
    bare.write_register("PC", intercepted.pc + 2)
    return True


bare.set_intercepted(32 + 1, True)
bare.set_intercept_callback(trap_1)
assert bare.run_limited(begin=CALL, until=0x4000).end == mc683xx.End.Until()
bare.set_intercept_callback(None)

bare.set_stop_on_fault(True)
bare.write(CALL, struct.pack(">2H", 0x2010, RTS))  # move.l (a0),d0
bare.write_register("A0", 0x90000000)
match bare.run_limited(begin=CALL, until=CALL_RETURN).end:
    case mc683xx.End.Fault(fault):
        assert fault.cause == mc683xx.FaultCause.BUS_ERROR
        assert (fault.address, fault.size, fault.pc) == (0x90000000, 4, CALL)
    case end:
        raise AssertionError(end)
bare.set_stop_on_fault(False)
```

`set_deviations(Deviations(misaligned=True, untraced=True))`
(`deviations`), Rust's `bare::Deviations`: odd word and long data
accesses as byte cycles, and T1 and T0 tracing nothing, for parity
runs against Unicorn only: not CPU32 behavior, off by default.

### From Unicorn

The port of a Unicorn harness is mechanical:

| Unicorn | `mc683xx.Machine` |
| --- | --- |
| `Uc(UC_ARCH_M68K, UC_MODE_BIG_ENDIAN)`, `ctl_set_cpu_model` | `Machine()`: a CPU32, 32 address lines |
| `mem_map(address, size, perms)` | `map(start, length, permissions="rwx", contents=...)`, 2K pages |
| `mem_unmap`, `mem_protect(address, size, perms)` | `unmap`, `protect(start, length, "rx")` |
| `UC_PROT_READ`, `_WRITE`, `_EXEC` | `"r"`, `"w"`, `"x"` |
| `mem_write(address, data)`, `mem_read(address, size)` | `write(address, data)`, `read(address, length)` |
| `reg_write(UC_M68K_REG_D0, value)`, `reg_read` | `write_register("D0", value)`, `read_register("D0")` |
| `emu_start(begin, until, timeout, count)` | `run_limited(begin=begin, until=until, instructions=count, cycles=...)`: no host time |
| `emu_stop()` in a hook | `stop_request().request()`, or return a truthy value |
| `hook_add(UC_HOOK_CODE, f, begin=, end=)` | `add_code_hook(f, begin, end)`: `f(Instruction)`; `until` before it |
| `hook_add(UC_HOOK_BLOCK, ...)` | `add_block_hook` |
| `hook_add(UC_HOOK_MEM_READ, ...)` to feed a read | `add_peripheral(begin, end, peripheral)`: `read` answers before the read |
| `hook_add(UC_HOOK_MEM_WRITE, ...)` before the write lands | `add_peripheral` `write`; or `add_write_hook`, after the instruction |
| `UC_HOOK_MEM_READ` to watch | `add_read_hook`, after the instruction |
| `UC_HOOK_MEM_INVALID`, `UC_MEM_WRITE_PROT` | `add_invalid_hook` (`reason` `BUS_ERROR` or `PROTECTED`), or `set_stop_on_fault` and `End.Fault` |
| `UcError` on a fault | `set_stop_on_fault(True)`, `End.Fault(fault)`; by default the CPU takes the bus error exception |
| `UC_HOOK_INTR` (`intno`) | `set_intercepted(vector, True)` and `set_intercept_callback(f)` before stacking (the vector, not `intno`), or `add_exception_hook` after |
| interrupts stacked by hand | `request_interrupt(pin, vector)` |
| `hook_del(handle)` | `remove_hook(hook)` |
| `ctl_flush_tb()` | nothing: there is no translation cache |
| `context_save()`, `context_restore` | `save()`, `restore()`: memory included |
| a code hook's register write | the same: before the instruction, PC writes redirect |
| misaligned access, ignored trace | `set_deviations(Deviations(misaligned=True, untraced=True))`: parity only |
