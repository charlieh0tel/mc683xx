# mc683xx plan

Architecture, decisions and why, and open work.  Update it when a
decision changes.

## Purpose

An embeddable MC68331 (CPU32) emulator under a permissive license, for
running real firmware inside a host program: board emulators, firmware
test harnesses, fuzzers.  Exceptions are taken exactly as the CPU32
takes them, and the host sees every peripheral access, exception and
interrupt entry.

Two goals, equal in weight: the CPU32 core, and a SoC the host can
surround with its peripherals.  A board emulator is only as good as
the peripherals its firmware sees, so the peripheral interface is designed
with the core, not after it.

First user: `smartclock-emu` and the smartclock firmware harness
([smartclock](https://github.com/charlieh0tel/smartclock)), replacing
Unicorn.  Unicorn is GPL-2.0-only, which keeps
`smartclock-emu` a feature-gated local tool, and its m68k has bugs the
harness works around (lazy condition codes marked computed before they
are).

## Generality

General enough for any board built around a 68331.  smartclock and the
Z3805A are the first user and a must: the core has to run them.  They
are not the design target; do not overindex on them.  Other hardware
should fit without changing the interface.

The interface is the chip's: bus cycles, function codes, interrupt
levels, chip-select pins, module pins.  It names nothing about any one
board.  Where the chip offers something smartclock does not use (other
bus masters, a pin assignment, a module mode), the interface must not
foreclose it, even when the implementation waits for a user.

## Scope

In:

- CPU32 instruction set (CPU32RM): the 68000's plus 32-bit multiply
  and divide, `extb`, `chk2`/`cmp2`, `bkpt`, `rtd`, `movec`, `moves`,
  `lpstop`, `tbl*`, `bgnd` (illegal, or a halt the host sees).
- Exception processing as CPU32RM section 6 specifies: frame formats
  0, 2 and C and `rte` of each, VBR, double fault halt, traps, `chk`,
  divide by zero, privilege, trace, line A and F.
- Supervisor and user modes, two stack pointers, function codes on
  every access.
- MC68331 modules: SIM, QSM, GPT, board time.
- Python bindings.
- A GDB remote stub (see Debugger): all-stop, one CPU, over the bare
  CPU, the chip and a board.

Out:

- Cycle accuracy.  Not cycle accurate, but not cycle stupid: see
  Cycle estimates.  Exact timing can be a later layer if a routine
  needs it.
- FPU, MMU, cache, bit fields: the CPU32 has none.
- Board-specific peripherals and the wiring: the host's.  Stock chips are
  in scope, in `mc683xx-peripherals` (see Stock peripherals).  The memory map is not the
  host's: it comes from the chip selects, as on the chip.
- Other 683xx modules (68332 TPU, 68340 DMA, timers, serial) until a
  user needs them.
- Multiple CPUs sharing a bus, threads in the core, async buses: no
  user needs them.
- External bus masters (BR/BG/BGACK): not built yet, but the bus and
  `run_for` leave room for them; adding one must not change the
  interface.

## Crates

- `mc683xx-cpu32` (`crates/cpu32`): the core.  Depends on nothing
  board- or chip-specific.
- `mc683xx-modules` (`crates/mc683xx`): SIM (with board time, which
  derives from SYNCR) and QSM, shared by 683xx parts, on the core's
  bus and IPL/IACK traits.
- `mc683xx-mc68331` (`crates/mc68331`): GPT; composes the core and the
  modules into the chip.
- `cpu32-py`: PyO3 bindings, the Python module `mc683xx` (see
  Python); `publish = false`, released as the PyPI wheel.
- `examples` (`mc683xx-examples`, `publish = false`): the host
  guide, `docs/host-guide.md`, as its crate documentation, so its
  examples are doctests `make ci` runs.
- `mc683xx-board` (`crates/mc68331-board`): board files and the board
  around a 683xx chip: peripherals from a registry, decoders, nets and
  board time (see Boards), generic over the chip (`chip::Chip`), the
  MC68331's implementation behind its default feature `mc68331`.
- `mc683xx-run` (`crates/mc68331-run`): the runner, the workspace's one
  binary, for a board file (see Boards).
- `mc683xx-gdb` (`crates/gdb`): the GDB remote stub (see Debugger).
- `mc683xx-peripherals` (`crates/peripherals`): stock chips as board-file peripherals
  (see Stock peripherals).
- `cpu32-harness`, `cpu32-oracle`: instruction-level tests against
  SingleStepTests and Musashi (see Verification); `publish = false`.
- `snapshot-golden`: the snapshot formats' golden-file checks (see
  State); test-only, `publish = false`.

`mc683xx-cpu32`, `mc683xx-modules`, `mc683xx-mc68331`,
`mc683xx-board`, `mc683xx-gdb`, `mc683xx-peripherals` and
`mc683xx-run` are published to crates.io, depended on through the root
`[workspace.dependencies]` with a path and a version
(`docs/releasing.md`).  The published crates share the `mc683xx-`
prefix; their directories keep the short names.

## Interface

- **Registers:** read and write every one: D0-D7, A0-A7, PC, SR, SSP,
  USP, VBR, SFC, DFC.
- **Memory:** the host wires RAM, ROM and peripherals to chip-select pins
  (see Peripherals), and, emulator-only, memory at a fixed address on
  no pin (Fixed memory, below).  RAM and ROM are served in the core; peripherals get
  their accesses through the `Bus`.  A bus access carries address,
  size (byte, word, long), function code and the host's time.
- **Reset:** load SSP and PC from the vectors at zero and reset the
  modules.
- **Run:** `run_for(now, budget)` runs whole instructions until the cycle
  estimate reaches `budget`.  Returns cycles used and the stop reason:
  budget, `stop`/`lpstop`, halt (with reason), a hook's stop
  (`Stop::Hook`), or the bus's (`Stop::Requested`, see Hooks).  The
  chip's `Mc68331::run_for(budget)` keeps the board's time itself, runs
  the core in quanta to the next event, and takes the watchdog's and
  the halt monitor's resets as they come, time passing while the CPU
  is halted (A halted CPU); `take_events` reports resets and IARB
  ties.  A run overshoots its budget by up to an
  instruction, so `Mc68331::run_until(target)` runs to the first
  instruction boundary at or after a time instead: its end depends
  only on the chip and the target, so a series of them is one long
  run however the host cuts it.
- **Host bus access:** `Mc68331::bus_read`, `bus_write` and `peek`
  (`mc683xx_mc68331::host`) reach the bus as a debugger does (BDM's memory
  commands, CPU32RM 7), by address, size and function code, through
  the chip's decode: module registers at MM's base, and whatever a
  chip select reaches.  `bus_read` and `bus_write` are real cycles at
  the chip's time, side effects and all, outside any run: no time
  passes, the trace does not see them, and a peripheral's stop request
  in one is dropped.  `peek` has no side effects: a module answers
  through `Module::peek` (required, as `save` is), the SIM's reads
  have none, and memory answers byte by byte where memory alone
  answers the cycle (`Board::memory_alone`, the page-table rule).  A
  peripheral's bytes are `AccessError::Unpeekable`, never a guess.
  Errors are typed: `Unmapped`, `BusError`, `Misaligned`, `Width`.
- **Interrupts:** the host sets an IPL level; the core calls the
  host's IACK callback when it accepts, for a vector or autovector.
- **Peripherals:** see below.
- **Hooks:** plain in-process callbacks, cheap when absent: code
  address (with registers and top of stack), block, memory read and
  write, invalid access, exception taken.  Any hook can stop `run_for`.
  `Cpu::add_code_hook`, `add_block_hook`, `add_read_hook`,
  `add_write_hook`, `add_invalid_hook` (each over an address range),
  `add_exception_hook`, `remove_hook`; the chip's through
  `Mc68331::cpu_control`, a `CpuControl` that lends the registers, the
  hooks and the pages' permissions and shows the rest read-only, so a
  host cannot remap the core's memory apart from the chip's decode or
  run the core itself (the gdb stub's `Target::with_cpu` lends one
  too).  A callback returns `ControlFlow`; `Break`
  stops the run.
- **Memory protection:** `Memory::protect(space, start, length,
  Permissions)`, read, write and execute per page and space.  A cycle
  a page does not permit is a bus error, shown to the invalid access
  hooks as `Invalid::Protected`.
- **Tracing:** peripheral accesses, exceptions taken, instruction count.
  The chip's events carry them: `Mc68331::set_tracing(Tracing {
  accesses, exceptions })`, both off at first, adds `Event::Access`
  for each bus cycle the core sends the chip (module registers,
  peripherals, memory a peripheral shares a select with; not RAM and ROM the
  core serves, which memory hooks see) and `Event::Exception` for
  each exception entered (`Bus::exception_entered`).
  `Cpu::instructions` (and `Mc68331::instructions`) counts
  instructions run.
- **State:** save and restore the whole core, module and peripheral
  state.  `Cpu::save() -> Vec<u8>` and
  `Cpu::restore(&[u8])`: registers, the run loop's timing and
  exception state, every memory region (ROM's too; a freed one
  empty), page tables and permissions; not hooks, which stay.  `Mc68331::save()` and
  `restore` add the SIM, each module's `Module::save` (required of
  every module; one that cannot returns `StateError::Unsupported`,
  and then nor can the chip), each peripheral's and model's and the QSPI
  peripheral's `Save::save`, board time, the halt and
  the IRQ pins; the chip restores into one built the same way, its
  fixed memories' bases and lengths checked
  (`StateError::mismatch("fixed memories")`), and rebuilds its page
  tables from its own wiring rather than take the snapshot's.
  Every peripheral attached to the chip (`Builder::peripheral`, `unwired_peripheral`) or the
  bare `Machine` (`add_peripheral`) says whether it saves:
  `mc683xx_cpu32::cpu::state::Save` is a bound of the attach, as `Timed`
  is, both its methods required, so that no peripheral saves what its own
  `restore` refuses: a peripheral gives both,
  `mc683xx_cpu32::unsavable!(X, "its role")` is one that cannot save
  (both refuse, `StateError::Unsupported("its role")`) and fails the
  snapshot, named, rather than one silently left out, and
  `impl Stateless for X {}` is one with no state, whose snapshot holds
  nothing (`STATELESS_VERSION`).  `unsavable!` is a macro, not a marker
  trait beside `Stateless`: two blanket impls of `Save` conflict.  A
  Python peripheral or QSPI peripheral likewise saves only with both
  `save` and `restore`.  A bound, not a default
  method of `MappedPeripheral`, so that a host must say, and so that the bare
  machine's `MappedPeripheral` shares it (the core cannot see
  `mc683xx-modules`).  The snapshot holds each peripheral's bytes in the
  order added, each in the peripheral's own versioned format; a peripheral that
  cannot save is named by that place (`peripheral 2`; the bare machine's by
  its first address, `the peripheral at 0xf00000`), as the board names a
  peripheral; a snapshot with another count of peripherals is
  `StateError::Mismatch("peripherals")`.  The QSPI's peripheral
  (`Builder::qspi`) is held the same way: `Save` is a bound of the
  attach (`AnySpiPeripheral`, in `mc683xx-modules`, since the QSM holds
  it), and the chip's snapshot, not the QSM's, holds its bytes, so the
  QSM's format stays the module's own and the chip saves and puts back
  every host part in one place.  One that cannot save is named `the
  QSPI peripheral`; a snapshot with one where the chip has none, or none
  where it has one, is `StateError::Mismatch("the QSPI peripheral")`.  The
  board's own chip-side peripherals (select and decoder routers, memory
  diversions, peripheral models, and the PCS decode on the QSPI) are
  `Stateless`: the peripherals behind them are in the board's snapshot,
  and so is the PCS decode's own state (the PCS levels and each
  target's framing), beside the peripherals it is checked against,
  rather than saved twice.  What
  is cached for speed (the interrupt level, the next events, the
  GPT's among them) is not saved: a restore finds it again.  Each
  format is a little-endian `u32` version, then postcard (serde)
  bytes; another version, short or undecodable bytes or bytes past its end, other
  hardware (address lines, region sizes, page counts, module count),
  or values no run could save (`StateError::Invalid`: a register bit
  the peripheral does not implement, an index past what it indexes, a
  function code or interrupt level out of range, which refuse to
  decode) is a typed `StateError`, and the target is left unchanged.
  Each layer checks what it can before it applies any of it; a
  peripheral's or module's snapshot is checked by restoring it, so the chip
  (and the bare machine, for its peripherals) saves each peripheral and module
  first and, if a later one or the core (the last
  restored, its checks all made before it changes) refuses, puts back
  only the ones restored before it (`state::restore_each`): the
  refusing part is unchanged by contract, and the parts after it are
  untouched.  A peripheral's
  `restore` must take its own `save`'s bytes back, and leave the
  peripheral unchanged when it refuses; one that breaks this is left as
  it is, the rest still put back, never a panic.  The
  board does the same with its peripherals, the chip restored last; a peripheral's
  `restore` must take its own `save`'s bytes back, and refuses, as
  the layers do, what no run of it could save; a stock peripheral's snapshot
  holds the parameters that shape it (a cascade, references, a
  variant, a clock), so that one of a peripheral built with others is
  `StateError::Mismatch`.  A board's save or
  restore that a peripheral refuses names the peripheral (`StateError::Peripheral`).  The same snapshot and the same inputs
  give the same run: tested by tracing a run, restoring, and running
  again.  Each format (the core's, `Machine`'s, the chip's, the QSM's,
  the GPT's, the board's, the built-in peripherals' and the adapters') has a
  golden test (`snapshot-golden`): a fixed state saved and checked
  against `tests/golden/<layer>.golden` in its crate, which names the
  versions of the layers in the snapshot (its own, then those nested
  in it), its length and a hash.  Bytes that change under the same
  versions fail: bump the layer's version, then regenerate with
  `UPDATE_GOLDEN=1 cargo test`, which refuses to rewrite a golden
  under the same versions (a fixture change alone deletes the golden
  first).  A bump of a nested layer bumps every layer that holds it
  (a CPU bump bumps `Machine`, the chip and the board; a chip bump the
  board); a golden
  whose nested versions changed but not its own fails.  Two branches that change one format so conflict on its
  golden at rebase, where they once bumped the same number apart and
  merged silently.

### Peripherals

As important as the core.  Two words, one meaning each, everywhere
(code, docs, Python, board files): a **peripheral** is anything
outside the chip, a **module** a block on it (SIM, QSM, GPT; the
`Module` trait, the `mc683xx-modules` crate).  A peripheral the CPU
reaches by address is a mapped peripheral, one trait for the chip's
selects and the bare machine's ranges
(`mc683xx_cpu32::peripheral::MappedPeripheral`, its cycle a `BusCycle`
that carries the select on a chip and none on the bare machine); one
on the QSPI an SPI peripheral (`SpiPeripheral`); a board component a
`Peripheral` of the board crate, which adapts the other two
(`MappedAdapter`, `SpiAdapter`).  "Peripheral" names nothing (decided
2026-10-24, before 0.1.0): it meant both the chip-level trait and the
board's components, and a reader had to tell which.  The host
surrounds the SoC with its board peripherals through:

- **Bus peripherals:** follow the hardware.  The host wires each peripheral to
  a chip-select pin, statically, as the board does, with its byte
  lanes.  The SIM decodes addresses to chip selects from CSPARx,
  CSBARx and CSORx, with their block sizes, wait states, read/write
  and DSACK, starting from CSBOOT at reset.  The firmware's writes move
  the map; the host never states addresses.  An access no chip select
  claims follows the SIM bus monitor.  Internal modules win over a
  chip select at the same address (MC68331UM 4.8).  Overlapping
  selects both assert, as on the chip (SIMRM 7.5); the first DSACK
  ends the cycle.  Board decode logic within a
  select's window (the Z3805A's 74AC138s) is part of the host's
  peripheral: it gets the address and decodes it, as the board does.  A
  peripheral on an externally terminated select (DSACK %1111, SIMRM
  7.10.3) states its wait states per cycle (`MappedPeripheral::wait_states`),
  or none, for the bus monitor.  A peripheral can drive an IRQ pin
  (`MappedPeripheral::interrupt_request`) and sees RESET (`MappedPeripheral::reset`).
  Every reset source drives RESET (SIMRM Table 8-1), but a board need
  not wire every peripheral to it: `Builder::reset_scope` names the
  causes that reach a peripheral (a `ResetScope`), all by default.
  smartclock's DUART, for one, sees no watchdog reset.
- **Fixed memory (emulator-only):** `Builder::fixed_memory(base,
  bytes, writable)` maps memory at a fixed, page-aligned address on no
  select, for code and data a host maps beside an image it leaves as
  it is (a consumer's hybrid boot loads rewritten routines at $600000
  and their data at $6F0000 and diverts the image's calls there).
  The chip has no such decode.  It takes the program and data cycles
  at its addresses that no chip select matches: a matching select
  wins, so the firmware's map behaves as on hardware wherever it
  decodes, and the internal modules win over both (MC68331UM 4.8).
  Out of reset only CSBOOT decodes, its first Mbyte (MC68331UM Table
  4-24), so it answers at once above that.  CPU-space cycles never
  reach it.  Its cycles end at once by default, a 16-bit port with
  fast termination costing nothing beyond the timing tables, as an
  internal module's, with no DSACK.  `Builder::fixed_timing(memory,
  PortSize, Termination)` (board file `port`, `wait_states`) gives it
  a select's internal timing instead, fast or 0 to 13 wait states, on
  8 or 16 bits, so code it holds runs at the bus speed of the memory
  it stands in for; the bus monitor then ends a cycle longer than its
  period, as it does a select's.  Two that overlap are a build error; a
  fixed memory is never diverted (no select carries its cycles).  Its
  bytes are a region of the core's memory, in the snapshot as every
  memory's: no format changed.  Contents survive every reset unless
  `Builder::reload_scope(memory, ResetScope)` names the causes that
  put them back as built (for any memory, emulator-only too): before
  the reset vectors are fetched, the `reset` instruction's after the
  instruction (the core asks the bus for a stop after it, as after a
  cycle, so the quantum ends there).  The board file's `[[memory]]`
  takes `base` in place of `attach`, and `reload` (the causes, as a
  peripheral's `reset`): one table, as every other key of a memory means
  the same at a fixed address, and `base` and `attach` say where it
  answers, one or the other; a version 1 file means what it did.
- **Vectored interrupts through a chip select:** a select programmed
  for CPU space at an IPL (SIMRM 7.8.1) carries the acknowledge to its
  peripheral as a CPU-space byte read at $FFFFF1 + 2 x level, the lower
  byte of a 16-bit port; the value read is the vector, after the
  peripheral's wait states on an external DSACK.  A bus error, or a DSACK
  later than the bus monitor allows, is the spurious vector.
- **QSPI:** the host's `SpiPeripheral` (`Builder::qspi`) sees each
  transfer (`QspiTransfer`: the queue entry, the levels on PCS[3:0],
  the word and its bits, and the serial clock it is shifted with, its
  mode), one method for every peripheral, and `chip_selects` whenever
  those levels change, so a peripheral framed by its select (CONT)
  sees the frame end; whether the board uses them as four selects or
  decodes them to a peripheral number (smartclock does) is the host's.
  It is the one peripheral, so it sees every level change; on a board
  of `mc683xx-board` peripherals the board's PCS decode is it, and each peripheral
  sees only its own select (`Peripheral::select`), the canonical framing
  there.  `reset` is RESET by `Builder::qspi_reset_scope`
  (all causes by default; smartclock's DACs clear on RESET).
  Each queue entry takes the time SPCR0's baud and SPCR1's delays give
  it (QSMRM 4.3.1, 4.3.2).  A run syncs only where the queue shows:
  an entry's end that sets SPIF with SPIFIE, halts or takes a
  buffered SPCR2, and each call to the peripheral, unless it says its
  calls may be deferred (`SpiPeripheral::deferrable`); entries between
  run in a batch when the QSM is next read, written or advanced.  A
  queue in wraparound synced twice an entry before (2026-10-23,
  docs/performance.md).  A queue's stop, or its turn to a slave,
  syncs for such a peripheral only if PCS moves, which calls it: not
  after an entry without CONT, which released PCS at its transfer.
  Otherwise a sync a call is the floor for it: the call must see the
  CPU and the board's other peripherals at its time, and a peripheral behind
  `mc683xx-board`'s PCS decode may drive, stop or share state with a
  peripheral the CPU reads, so only the peripheral can say its calls may
  wait (2026-10-24, docs/performance.md).  On a board each QSPI peripheral
  says so itself (`Peripheral::qspi_deferrable`, false by default;
  `SpiAdapter` asks its model's `deferrable`), and the decode is
  deferrable only if every one is: the decode cannot know what a peripheral
  does in its calls.  Nor while a PCS pin (`ChipPin::pcs`) is on a net a
  peripheral, the chip or an observer senses: a batch moves PCS only
  where the CPU sees the QSPI, so the net would see its changes late
  (`a_net_sensing_pcs_keeps_the_qspi_from_deferring`; checked again as
  observers attach and detach).  Code only, not in the board file: a peripheral's calls
  are its code's to vouch for, and a v1 file would mean something new
  (2026-10-24, docs/board-files.md).  `SpiPeripheral::clock`, defaulted to nothing,
  tells it the serial clock (`QspiClock`: CPOL, CPHA, the SCK period
  in system clocks and the system clock's frequency, so `baud` is
  QSMRM equation 4-1) at the first entry and at any entry after one of
  them changed; a peripheral that cares checks its SPI mode there.
  Each transfer carries the clock too, the one told at its entry's
  start, so a peripheral that answers by its mode needs no state of
  its own for it.  On a board a peripheral's `Transfer` carries the same,
  the word and its bits, and whether it starts the peripheral's frame (its
  first since its select asserted), since what a slave shifts and
  what the master captures depend on the edges, not on the word.  The
  decode's own state (the PCS levels, each attachment's frame) is in
  the board's snapshot, so a board restored into a fresh load frames
  its peripherals as the saved one did.
  `mc683xx_board::spi` is the one place SPI edges are reckoned, for
  stock peripherals and a consumer's alike: a peripheral declares its datasheet's
  edges (sample, change, first bit at its select) and its shift logic,
  and `spi::exchange` walks the transfer's SCK edges in its mode,
  every capture taking the level before its edge and every output
  changing after it.  No peripheral keeps a table of modes: a mode is only
  which edge leads and which edges the master changes and captures
  on, so what a mismatched mode does (a bit early, a bit late) falls
  out of the edges, as on the bench.  Two choices the manuals leave:
  a slave that samples on the edge the master changes MOSI takes the
  level before the change (the QSPI's data hold after SCK is 0 ns,
  MC68331UM Table A-9; a slave's own hold, the MC144110's 5 us, is
  then not met, and real parts differ): with CPHA 0 that is the bit
  meant for it, with CPHA 1, whose leading edge puts each bit, the bit
  before; and MOSI before a transfer's first bit is
  PORTQS's MOSI bit ("PORT DATA", MC68331UM Figures A-16 and A-17),
  which a peripheral does not see, taken as 0, its value out of reset.  The
  walk is bit by bit; a peripheral that does not care (MISO undriven, MOSI
  ignored) answers with the word.
  The board tells the QSM the system clock after a SYNCR write and at
  reset.  With MSTR clear the QSPI is a slave and the host is the
  external master: `set_qspi_slave_select` drives PCS0/SS (the board's
  pin, as `set_port_qs_pins` does), and `qspi_slave_shift(mosi, bits,
  period)` clocks up to 16 bits in, one an SCK period of system
  clocks, running the chip between them, and returns what the QSPI
  drove on MISO (`Shifted`: levels and a driven mask).  The QSM's
  `clock_slave` is the same one bit at a time, for a host that keeps
  its own time.
- **SCI:** whole frames in and out (`Mc68331::sci_receive`,
  `take_sci_transmitted`), each a frame time on the line at SCCR0's
  baud: the data bits as on the line (parity included), or a break.
  Frames the host sends arrive back to back after those before them.
  A frame's time is its start bit's, both ways: `sci_receive` starts
  one on RXD now, and `take_sci_transmitted` reports each frame as
  the shifter takes it (`Transmitted`: the frame, its start, its end
  and its bit time), so a receiver modeled from the start bit (a
  DUART) is not a frame late.  Stamping the end, as before, made a
  board receiver a character late and a frame driven into RXD arrive
  a frame after its time (2026-10-23).
- **Pins:** port and module pins in and out, including GPT input
  capture and pulse accumulator inputs (1 PPS-style signals): the host
  sets the levels it drives (`set_port_qs_pins`, `set_port_gp_pins`,
  `set_pai`, `set_pclk`, `set_port_e_pins`, `set_port_f_pins`) and
  reads the levels the chip drives (`port_c_outputs`,
  `port_e_outputs`, `port_f_outputs`, `port_qs_outputs`,
  `port_gp_outputs`, `pwm`).  An
  edge the host makes takes effect at the chip's time between `run_for`
  calls.  `take_port_changes` gives each change in what ports C, E
  and F drive with its time, as `take_sci_transmitted` gives frames,
  so a pin the firmware pulses within a quantum is both its edges;
  `take_qsm_changes` gives port QS's the same way, the QSM brought up
  to date change by change (each SCI event and QSPI phase end) so
  PCS pulses in a batch of QSPI entries keep their times.
  Port C is the latch on the CS3-CS9 pins CSPARx assigns as discrete
  outputs (MC68331UM 4.8.1.4).
- **GPT input edges:** board models clocked apart from the quanta (an
  FPGA's 1 PPS on IC1, a GPS engine's on PAI, an INIT line on PCLK,
  read through PCLKS) schedule edges ahead:
  `schedule_gpt_input(time, Input, level)`, `Input::PortGp(PGPn)`,
  `Input::Pai` or `Input::Pclk`.  The GPT takes each at its time,
  in order (those at one time in the order given): an input capture
  latches TCNT then, PAI counts, PCLK clocks.  The earliest is the
  GPT's next event, so a run's quantum ends there and a capture's
  interrupt waits at most one instruction; a firmware read inside
  the quantum sees it if it came before the read.  An edge scheduled
  in the past takes effect at the GPT's time.  A reset keeps them:
  they are the board's.  `unschedule_gpt_inputs()` withdraws those
  past now (those up to now coming first), for a host whose clocks
  move against its own time.  Times are whole CPU clocks: the GPT samples
  its inputs on the system clock, and a capture latches TCNT, whose
  resolution is a prescaler tick (GPTRM), so no sub-clock phase can
  reach the firmware.  A board model with a finer phase (an FPGA
  interpolator) keeps it on its own side and schedules the edge at
  the first clock at or after it (rounding up).
- **GPT output edges:** a peripheral may react to an edge between run
  quanta, so the host sees each with the time it happened, not only
  the level at a sync point: `take_gpt_changes` returns
  `(time, OutputChange)` in time order, in the shape of the SIM's
  `take_port_changes`: `PortGp(PortLevels)`, the whole port as a SIM
  port's `Change` (both carry a `PortLevels`), or `Pwm { channel, level }`.  The two
  stay separate streams of one shape rather than one: the SIM's
  `Change` lives in `mc683xx-modules`, shared by 683xx parts, and port GP
  and the PWM pins are the MC68331's GPT's, so one stream would need
  a chip-level wrapper around both and change the SIM's API; a host
  that wants one order merges the two by time.  A port change takes
  its write cycle's start; a GPT change takes the time it happened,
  which is the cycle's for a write.  A compare's edge takes the time TCNT matched (GPTRM 3.3),
  a write's (CFORC, PORTGP, DDRGP, TCTL1, OC1M) and a reset's the
  cycle's, whenever the GPT catches up; quanta still end at the
  compares with pin actions, for their flags.  Port GP's edges are
  always found: a compare makes at most one a match.  The PWM pins'
  are found only for a channel the host asks for
  (`set_report_pwm_edges(channel, true)`; off out of power-on, kept over
  a reset), each at its PWMCNT count from the schedule (GPTRM 6.2):
  PWMCNT at the system clock / 2 makes up to two edges every 512
  clocks, too many for a host that only filters the waveform, which
  reads `pwm()`'s `Output` as before.  Reporting waveform changes
  instead was rejected: the host would have to rebuild the edges from
  a period, a duty and a phase, the buffered first period
  (PWMxBUF) included, and an edge-reacting peripheral would still need
  the edges.  The clock outputs, PPROUT's PWMCNT clock on PWMA and
  CPROUT's TCNT clock on OC1, make an edge each way every tap period:
  high the one system clock in which the tap clocks its counter (its
  prescaler count a multiple of the divisor), from its count to the
  next (GPTRM 5.1, 6.3.1); on PCLK, the clock from each rise the GPT
  sees.  Up to an edge every clock, so they are opt-in as the PWM
  edges are: PWMA's come with `set_report_pwm_edges(A, true)`, whatever
  the pin carries, and OC1's with `set_report_oc1_clock_edges(true)`, as
  port GP changes at their own times, ahead of a compare at the same
  time (OC1's clock rises at every TCNT count); without it the
  changes show OC1's pin driven low under CPROUT.  The level queries
  (`port_gp_outputs`, PORTGP, `pwm_level`, PWMC's F1A) show the clock
  either way.  Like the PWM edges they are found as the GPT catches
  up, never as events: the next event is untouched and no quantum ends
  at one.  The queue grows until taken, as the SCI's does.
- **Interrupts:** external sources at an IPL level, vectored at IACK.
- **Reference clock:** the 32.768 kHz input.
- **Quantum, sync point, sync:** the words are SystemC TLM-2.0's for
  temporal decoupling (IEEE 1666-2011, TLM-2.0's global quantum): the
  CPU runs ahead of the rest of the system for a *quantum*, the
  stretch between two synchronizations; at its end, the *sync point*,
  the modules and the peripherals are brought up to date; and
  ending a quantum early, after the instruction running, is a *sync*
  ("the chip syncs at the next instruction boundary").  A host's own
  loop that runs the chip in pieces (the bindings' Ctrl-C checks,
  gdb's continue, the runner's turns) runs it in *quanta*, which are
  not the model's and change nothing it does.
- **Time:** each peripheral reports its next event; `run_for` ends a quantum
  there, so a peripheral sees time move only between quanta and an
  interrupt waits at most one quantum.  Models alongside the CPU (a GPS
  engine) take part the same way.  An access to the SIM or a module
  that moves its next event before the sync point (SPE starting a
  QSPI queue, a PIT or compare written) ends the quantum after that
  instruction (`Bus::stop_requested`, which the run loop takes as a
  sync point, not a stop, unless the host's stop came in the same
  instruction), so the chip's own interrupts come on time
  however long the run; a peripheral's event a bus cycle moves still
  waits for the sync point.  The wake from LPSTOP at an acknowledge
  and the `reset` instruction's module resets are taken the same way,
  ending the quantum after the interrupt's entry or the instruction
  (the GPT's and the QSM's resets make no event earlier than they
  had: the GPT keeps only its scheduled inputs, the QSM goes idle; the
  sync is for a module whose reset would).  The chip keeps the SIM's and the
  modules' earliest next event and finds it again only after an
  access to the SIM or a module, an acknowledge, a reset, a restore,
  the host's change of a module or a SIM pin, or an advance that
  reaches it; the GPT keeps its own the same way.  That rests on a
  module's rule: an advance short of its next event leaves it where
  it was (nothing happens before it, by its meaning).  A debug
  assertion checks both against a fresh search every quantum.  The
  peripherals' next events are the host's, asked every quantum.

Fast paths for RAM and ROM must never bypass a peripheral access or
reorder it.

### Stock peripherals

Stock chips a 68331 board carries, as peripherals a board file names by
part number (`mc683xx-peripherals`), so a board gets them without
code (decided 2026-10-24).  Each models
its datasheet, cited as the manuals are, saves and restores, and
reaches the board through the bus, the QSPI and nets only: nothing
of a consumer's (indicators, host lines) is wired in.  The runner and
Python's `Board` register them after the host's own types, so a host
type of the same name takes precedence.  A board's
own logic (an FPGA's design, a custom panel) stays with its host.
Each peripheral's ports, parameters and the choices where its datasheet is
silent are in `docs/peripherals.md`; the datasheets are in
`third_party/datasheets/`.  The larger choices:
- *Logic peripherals.*  `74x374` and `74x373` are one model: strobed by a
  write cycle, the edge-triggered register and the transparent latch
  both hold the byte written from the cycle's end, and the '373's
  outputs following D within the cycle are below what the bus
  resolves.  Each Q is its own logic net, so lamps and LEDs are
  whatever the board wires to the pins, and an observer sees them
  change on the nets; a consumer's indicator reporting is not wired
  in.  A logic net has no high-impedance state, so a Q that OE
  releases drives its `pull`.  A buffer's inputs are tied by a
  parameter until a net drives them; a switch bank the host moves at
  run time is an external on its nets, not code.
- *AT28C64B.*  A `Peripheral`, not a `MappedPeripheral` through `MappedAdapter`: its
  tBLC and tWC are real time, so it keeps board time, which a SYNCR
  write does not move.  Its cells live in a host file (`file`), every
  programmed byte written through as its cycle ends.  The file itself
  is locked before it is read and held while the peripheral lives, and
  is never renamed over, so two runs always contend for the same lock,
  the one refused never reads or changes the file the other holds, and
  a run leaves no lock file behind.  A new file is made whole in a sibling
  of a unique name, synced and hard-linked into place, which fails
  rather than replace a file another run made first; a short one is
  padded erased through the locked handle and synced, padding only
  appending, so a kill leaves a prefix the next run pads the same.  A
  kill while making a new file may leave the sibling, never a torn
  file.  Software data
  protection persists with the cells, as power transitions do not
  disable it (DS20006432B 6.6.2), in a sidecar, `<file>.sdp`, not a
  trailer, so the file stays a bare 8 KiB image a programmer reads and
  an existing file still loads; `protected` gives it to a file without
  the sidecar.  tWC is a parameter, the datasheet's maximum by default,
  since a peripheral's own is anything below it and firmware that polls a
  fixed count may depend on it.
- *Power-on state.*  The datasheets of the MC144110, the AD569 and
  the 74x164 give none: each powers on with its registers at 0 and
  drives that output at power-on.  The AD569 and the octal latches
  take `initial`, the word their latches power on holding (0 by
  default), since what a real part holds is unknown and a board's
  firmware may assume one: smartclock sets its AD569 to 0x8000,
  mid-scale, an inferred choice, not the datasheet's.  A snapshot
  holds the latches, so `initial` is not checked on restore.  None has a reset input (the '164's
  CLR is a pin of its own), so RESET changes none.
- *Read-back and SPI modes.*  The MC144110 shifts on CLK's fall
  (MC144110/D 4.1) and the 74x164 on CLK's rise (SN74HC164 9.1); Dout
  and QH are the last stage.  The ADC0838 samples DI on the rise and
  changes DO on the fall (SNAS531B, THE DIGITAL INTERFACE).  Each
  declares those edges to `mc683xx_board::spi` and answers in the
  transfer's mode as the edges make it, not as its intended mode: a
  master that captures before the edge that shifts reads back what
  went in a register's length before; one whose leading edge is the
  shift edge (the MC144110 in mode 3, the '164 in mode 1) reads a bit
  early and shifts in a bit late, as smartclock's hex DAC self-test
  relies on (it reads the MC144110 in mode 3 and shifts the word back
  down); the ADC0838 reads a clock late in modes 1 and 2
  (docs/peripherals.md).  The firmware's own mode is not checked
  against the peripheral's: a mismatch is a board's, and shows as it does
  on the bench.
- *Parallel peripherals and their serial front ends.*  The AD569 is a
  parallel peripheral (Table I: CS, HBE, LBE, LDAC), and `ad569` is that
  peripheral, DB15-DB0 a bus net, each control a net or tied.  A board that
  drives it from the QSPI does so through a shift register, as the
  board does: `74x164` on the QSPI, its outputs a bus net to DB
  (2026-10-24).  A stock peripheral is the chip as its datasheet gives it,
  not the chip with an assumed front end; the board file wires the
  two.  The '164 drives its outputs as each transfer ends, not at each
  clock: a transparent latch behind it sees the word, not the bits
  arriving.
- *MC68681 and SCC68692.*  One model, two names: the SCC68692 differs
  where firmware sees it (CR bit 7 starts its extra commands, which
  the MC68681 ignores; what RESET does to the counter/timer), so a
  board file says which it has.  Both show IACK in input port bit 6:
  the 2004 SCC68692 datasheet's "IP2" there is taken as an error,
  since its 1990 specification, the SCC68681, the SCN68681 and the
  MC68681 say IACKN and IP2 is bit 2 (docs/peripherals.md, "Input
  port").  RESET stops the SCC68692's counter/timer until a start
  command, as its datasheet says twice (RESETN; "will not be running
  until it receives an initial 'Start Counter' command"), and leaves
  the MC68681's running in timer mode, as its says (2.4, 3.5: timer
  mode "runs continuously"), the SCC68681's and SCN68681's
  counter/timer sections agreeing against their own pin tables
  (docs/peripherals.md, MC68681).  A `Peripheral`, not a
  `MappedPeripheral` through `MappedAdapter`: it drives serial, logic and IRQ nets,
  and keeps its own time in X1/CLK ticks, which a SYNCR write does not
  move.  A frame goes out at its start bit; a frame in is assembled at
  its first stop bit's center at the receiver's own rate, which does
  not check the sender's baud, as the SCI does not.  The counter/timer
  is counted in closed form and is an event only where it shows (OP3,
  or ISR[3] unmasked and clear), so a fast timer nothing watches
  quanta nothing.  Each output pin is its own logic net at the level
  it shows, an open-drain output released high, so a consumer's
  indicators (LEDs on OP2-OP7) are peripherals on those nets, not code here.
  External clocks on the input pins are not modeled; a channel or
  counter/timer on one has no clock.

### Python

`cpu32-py` builds the Python module `mc683xx` with PyO3 and maturin
(`crates/cpu32-py/pyproject.toml`, abi3 from Python 3.10).  The
module is named for the family, not the part: a 68332 would add its
chip beside `Mc68331`.

- **Names:** each Python item has its Rust item's name and words
  (AGENTS.md); where Python's ways differ the binding says so
  (docs/python.md, Names): a Rust type with `Display` and `FromStr`
  crosses as its string, any other enum as a Python enum class (a
  class per variant when they carry data), never as a string; a struct
  of plain values a call returns as a tuple of its fields (a port's
  outputs a `(driven, levels)` pair); a frequency an `int` named `_hz`
  in whole hertz or `_nanohertz`; a setter `set_*`, never a writable
  property.  A board's chip is a `BoardChip`
  (`Board.chip`), its methods the `Mc68331` ones that read or hook
  the chip, defined once (`chip_methods!`) for both classes over each
  one's `with_chip` and `with_chip_mut`: the pins, runs and snapshots
  stay the board's.
- **Shape:** the Rust host API, one to one.  `Builder(data=,
  reference_hz= | external_hz=)` wires `memory(contents, pins,
  writable=)`, `peripheral(object, pins)`, `unwired_peripheral(object)`,
  `interrupt(peripheral, pin)`, `autovector(pin)` and
  `reset_scope(peripheral, [ResetCause])`, `dsack(peripheral, Dsack)`,
  `qspi_reset_scope([ResetCause])`, and the handles a peripheral holds,
  `stop_request()`, `sync_request()` and `diversion()` (a `Diversion`
  by memory and peripheral numbers); `build()` gives
  an `Mc68331` through its power-on reset.  Pins are the manual's
  names, `"CSBOOT"`, `"CS0"`-`"CS10"`; registers too, `"D0"`-`"A7"`,
  `"PC"`, `"SR"`, `"CCR"`, `"USP"`, `"SSP"`, `"VBR"`, `"SFC"`, `"DFC"`.
  Memories and peripherals are the numbers `Builder` returned.
- **The chip:** `run_for(budget) -> Run(cycles, stop)`, Rust's
  `cpu::Run`, its `Stop` a class per variant as Rust's (`Stop.Budget()`,
  `Stop.Halted(halt)`, `Stop.Hook(hook)`, ...),
  `run_until(target)`, `bus_read`, `bus_write`, `peek` (`size` in
  bytes, `function_code` 5 by default; `BusError`, `UnpeekableError`),
  `reset()`,
  `now`, `halted`, `exception_taken` (a vector), `read_register`,
  `write_register`, `registers()`, `memory`, `read_memory`,
  `write_memory` (a memory's own bytes, by offset), `peripheral(n)` (the
  object back), `set_interrupt_pin`, `set_port_e_pins`,
  `set_port_f_pins`, `port_c`, `port_outputs(PortName)`,
  `take_port_changes()` (`(now, PortName, driven, levels)`),
  `system_hz`, `take_events()` (`Event.Reset`, `Event.ArbitrationTie`,
  `Event.Dsack`, and with `set_tracing(accesses=, exceptions=)` `Event.Access` and
  `Event.Exception`), `instructions`.
  `Stop`, `Halt` and `ResetCause` are enums; they, the other enums
  and the value classes that compare equal (`Frame`, `Output`,
  `Serial`) hash, so they key dicts and sets.
- **Hooks:** `add_code_hook`, `add_block_hook`, `add_read_hook`,
  `add_write_hook` and `add_invalid_hook(callback, begin=0,
  end=0xFFFFFFFF)` (both ends included) and
  `add_exception_hook(callback)` install a Python callable as the
  Rust hook and return a `HookId`; `remove_hook(hook) -> bool`.
  The callable gets what the Rust hook does: an `Instruction` (`pc`,
  `registers` as `registers()` gives them, `stack_top`), a
  `MemoryAccess` (`address`, `size`, `function_code`, `value`), an
  `InvalidAccess` (those, `cycle`, a `CycleKind`, `value` or `None`,
  `reason`, an `Invalid`: `BUS_ERROR` or `PROTECTED`) or an
  `ExceptionEntry` (`vector`, `pc`, `handler`, `stack`, `level`).  A
  truthy result stops the run (`Stop.Hook(hook)`),
  where the Rust hook's `Break` would.  An exception it raises stops
  the run there too and is raised when `run_for` returns, as a peripheral's
  is, but without a stop request left behind.  The run holds the
  chip, so a hook calling into it raises `RuntimeError` (and stops
  the run); what it needs comes in its argument.
- **Protection and state:** `protect(space, start, length,
  permissions)`, `space` a `Space` (`USER`, `SUPERVISOR`),
  `permissions` a string of `r`, `w` and `x` (`""` for none), whole
  pages.  `save() -> bytes` and `restore(bytes)`: the chip's snapshot
  (State).  Each Python peripheral is in it by its own `save() -> bytes`
  and `restore(bytes)`, as a Python board peripheral is; one without
  them makes `save` raise `StateError` naming it (`peripheral 0: a Python
  peripheral cannot save its state`), and an exception either raises is
  raised.  The QSPI's peripheral (`Builder.qspi`) is in it the same
  way, named `the QSPI peripheral`; hooks stay across a restore.
  A snapshot of another format or other hardware, or bytes that do
  not decode, raise `mc683xx.StateError` (a `ValueError`); a module
  that cannot save, `NotImplementedError`.
- **Peripherals:** any object with `read(cycle) -> int` and `write(cycle,
  value)`, and optionally `wait_states(cycle) -> int | None`,
  `interrupt_request() -> bool`, `reset(now)`, `next_event() -> int |
  None` and `advance(now)`, the `MappedPeripheral` and `Timed` traits' methods;
  a missing one takes the trait's default.  A model on no pin needs
  neither `read` nor `write`.  The methods are looked up once, when
  the object is wired.  A `BusCycle` carries `select`, `address`, `size`
  (bytes), `function_code` and `now`.  A read's value must fit the
  cycle.
- **Errors:** raising `mc683xx.BusError` in `read` or `write` asserts
  BERR.  Any other exception from a peripheral (a `KeyboardInterrupt`
  among them) cannot cross the run, and does not fault the bus: a read
  answers as undriven data lanes do, 0 (Undriven data lanes, below), a
  write goes nowhere, another method's trait default answers, the
  first such exception waits (but a `KeyboardInterrupt` goes before
  any other, waiting or from the run's signal check, that one its
  `__context__`, so Ctrl-C is never hidden behind an error), the run stops once that instruction
  ends (the chip's `StopRequest`; `Stop.Requested()`), and the call
  into the chip that ran the peripheral (`run_for`, `reset`, `build`) raises
  it when it returns, the chip as it then is.  The bindings' own
  errors are `ValueError` (`StateError` among them), `IndexError`,
  `OverflowError`, `RuntimeError` and `NotImplementedError`; nothing
  panics across the boundary.
- **Ctrl-C:** `Mc68331.run_for` and `run_until` run as `run_until`s
  100,000 clocks apart, `Board.run_for` as runs 5 ms of board time apart,
  and the bare `Machine`'s `run_limited` as core runs of at most 100,000
  instructions, each quantum starting where the last ended; between
  quanta `check_signals` runs Python's handlers, so Ctrl-C raises
  `KeyboardInterrupt` at an instruction boundary.  `Board.run_for` is
  Rust's `Board::run_until_paced`: paced by the board's control
  (`pacing=` sets it for that call alone, put back when it returns or
  raises unless another thread's `set_pacing` changed it meanwhile,
  which stays), it releases the GIL before each quantum, paced or
  not, and while it sleeps between quanta (`Pacer::wait_with`, sleeps
  of at most 20 ms, the signal handlers after each), so a UI thread's
  `stop` or `send` gets in during an unpaced run too.  Releasing it
  once a quantum costs nothing measurable unpaced (4 board seconds of
  the control test's board, 800 quanta: 0.191 s either way, release
  build).  A series of
  `run_until`s is one long run, and so is a series of board runs or
  of instruction-limited core runs, so the quanta change nothing the
  target does; a quantum that ends early (a stop, a halt, a clock
  change) ends the run as it would have.  The GIL stays held during
  native quanta: the targets hold `Rc`s and Python objects, so they
  are not `Send`, and `Python::detach` needs `Send` (or `unsafe`).
- **Cycles:** every Python object the Rust side holds (a peripheral and
  its methods, a hook's callable, an intercept, a peripheral and its class,
  a waiting exception) is in a slot (`held::Held`) that the target's
  `Pending` keeps weakly, and the pyclass owning the target (`Machine`,
  `Builder` until `build`, `Mc68331`, `Board`) shows them to Python's
  collector in `__traverse__` and drops them in `__clear__`.  A harness
  holding its CPU and hooked with its own bound methods is collected;
  without this, each such harness leaked its memory.  One owner
  visits each slot, so no reference is reported twice; a call takes
  its object out of the slot first, so the collector can visit it
  during the call.
- **Build:** the bindings compile only with the crate's `python`
  feature, which maturin enables.  Without it the crate is its
  Python-free peripherals (pin, register and permission names, the
  errors) and their
  tests, so `make ci` and a bare `cargo test` need no Python.  `make
  python` builds a `.venv`, runs clippy and the adapter's Rust tests
  with `python` (they embed the interpreter), `maturin develop`s the
  module into the venv, and runs `pytest`, `stubtest` and mypy.
- **QSM and GPT:** `Builder.qspi(object)` puts an object with
  `transfer(transfer) -> int` (an `mc683xx.QspiTransfer`, as the Rust
  one) and optionally
  `chip_selects(levels, now)` and `clock(clock, now)` on the QSPI, the
  `SpiPeripheral` trait's methods (`QspiClock` carries `polarity`, a
  `ClockPolarity`, `phase`, a `ClockPhase`, `period`, `system_clock_hz`
  and `baud_hz`); the bits shifted back must fit the transfer, and an
  exception waits as a bus peripheral's does, zeros shifting in.  The chip
  adds `qspi_peripheral()`, `qspi_chip_selects`, `set_qspi_slave_select`,
  `qspi_slave_shift(mosi, bits, period) -> (miso, driven)` (a word
  wider than its bits, or more than 16, is a `ValueError`),
  `set_port_qs_pins`, `port_qs_outputs`, `sci_receive(frame)` and
  `take_sci_transmitted()` (`Frame.Data(bits)`, `Frame.Break()`, with
  the time each ended), `set_port_gp_pins`, `set_pai`, `set_pclk`,
  `port_gp_outputs`, `pwm(Channel)` (`Output.Level`, `Output.Wave`,
  `Output.Clock`), `oc1()` (the same `Output`), `take_gpt_changes()`
  (`(now, OutputChange)`, `OutputChange.PortGp(driven, levels)` or
  `OutputChange.Pwm(channel, level)`),
  `set_report_pwm_edges(channel, on)`, `set_report_oc1_clock_edges(on)` and
  `schedule_gpt_input(time, input, level)` (input `"PGP0"`-`"PGP7"`,
  `"PAI"` or `"PCLK"`, `gpt::Input`'s `FromStr`).  The modules' registers are the firmware's,
  through the bus; the host reads their pins, and their registers
  only through `bus_read` and `peek`.
- **Type stubs:** `crates/cpu32-py/python/mc683xx/__init__.pyi`,
  with `py.typed` and an `__init__.py` that takes the compiled
  module's names (`mc683xx.mc683xx`), a maturin `python-source`
  package, so a wheel built from the sdist carries them too; written
  by hand, not generated, and checked against
  the built module by mypy's `stubtest` in `make python`, so a binding
  without its stub (or a stub without its binding) fails it.  The
  duck-typed peripheral and QSPI peripheral parameters are private
  protocols there.  `docs/python.md`'s examples run as a pytest and
  pass `mypy --strict` against the stubs.
- **Bare CPU:** `Machine(address_bits=32)`, `mc683xx_cpu32::flat::Machine` bound
  (see Bare CPU; docs/python.md, Bare CPU, with the Unicorn mapping):
  `map`, `unmap`, `protect` (`start, length`), `read`, `write`,
  `add_peripheral(begin, end, object)` (`read(BusCycle)`,
  `write(BusCycle, value)`), registers as the chip's,
  `run_limited(begin=, until=, instructions=, cycles=)`,
  `stop_request()` (a `StopRequest`, Rust's `bus::StopRequest`: a
  callable's request ends the run `Stop.Requested()` once the callables have
  run, one left from before a run is withdrawn as it starts),
  `set_intercepted(vector, on)` and `set_intercept_callback(callable)`
  (no callback: taken natively), `set_stop_on_fault` and
  `stops_on_fault`, `set_deviations(Deviations)` and `deviations`,
  `request_interrupt(pin, vector)`, `clear_interrupt`, `reset`,
  `save`, `restore`.  `run_limited` gives a `LimitedRun(cycles, end)`,
  its `End` Rust's (`End.Stop(stop)`, `End.Until()`,
  `End.Instructions()`, `End.Fault(fault)`) less `End::Intercepted`,
  which the run answers within it.  Its hooks may write the
  CPU (decided 2026-10-23; the chip keeps the copy model):
  a Rust hook only queues what it saw and stops the core's run, and
  `run_limited` calls the Python callables with nothing borrowed, then runs
  on.  So code and block hooks act before the instruction (a PC write
  redirects), memory and invalid access hooks after it, exception
  hooks before the handler.  The run loop's cost per hook call is a
  core run's start; about 700,000 three-instruction calls per second
  from Python in a release build (`test_cpu.py`), 15,000 constructions
  with 64 Kbytes mapped.
- **gdb:** `serve_gdb(address, ready=None)` on `Mc68331`, `Board` and
  `Machine` serves gdb (see Debugger) until it detaches or kills the
  target (an `Ending`: `DETACHED`, `KILLED`), so a harness hands control to
  gdb mid-run and gets it back.  It blocks, holding the target;
  `ready(port)` is called once it listens, before the target is
  held, so a test starts its client then (port 0 picks one) and may
  look at the target.  It waits for gdb (to connect, or for its next
  packet) in waits of `stub::POLL`, 20 ms, through the stub's
  `Waiter` (`accept_polled`, a `Polled` connection): the GIL released
  during each, so another thread (an in-process client) runs, and
  `check_signals` between them.  Between a continue's quanta it
  releases the GIL around the look for gdb's Ctrl-C, and runs the
  signal handlers.  Python peripherals, hooks and intercepts run
  as in `run_limited`; an exception one raises, or Ctrl-C in Python, ends the
  session, the stub's breakpoints and watchpoints removed, and is
  raised.
- **Boards:** `Board(path, types={name: class})` loads a board file,
  its runs giving a `BoardRun(stop, now)` as Rust's,
  with Python classes as peripheral types, each declaring its `PORTS`
  and called with the peripheral's parameters; their objects answer the
  `Peripheral` trait's methods, with a `Drive` to drive outputs
  (docs/python.md, Board files), `PORTS` a list of `(name, kind,
  direction)`, as `PortSpec`.  A serial frame crosses as an
  `mc683xx.Serial` (`Serial(frame, baud_nanohertz, bits)`,
  `Serial.timed`, `Serial.untimed`, as Rust's), or, for code written
  before timed frames, as its data alone: a peripheral opts in with
  `TIMED_SERIAL`, `take_received` with `timed`, so old classes keep
  working.  A QSPI peripheral's `transfer(transfer, drive)` gets an
  `mc683xx.Transfer`, as the Rust peripheral's, and `qspi_clock(clock, now)`
  as the Rust peripheral is told it.
  `externals()` gives `mc683xx.External`s, name, type and direction
  as the Rust `External`; each is its name as a string and equals
  it, so code that took the names keeps working.  `observe(net,
  callback)` is `Board::observe` (Boards, Front ends), its
  `ObserverId` for `unobserve`; `inspect(name)` is `Board::inspect`, as
  a dict.  `control()` is `Board::control`, an `mc683xx.BoardControl`
  (frozen, so any thread may call it: `stop`, `send`, `set_pacing`),
  and `mc683xx.Pacing` is `pacing::Pacing` (`UNPACED`, `REAL_TIME`,
  `scaled(factor)`).

### Boards

A board file (`board.toml`) describes a board around the chip as
data, and the `mc683xx-board` crate loads it into a running board: the
chip, its memories, the host's peripherals, the decode logic between them
and the nets that join them.  The rule: **the file says what is wired
where and how the chip is configured, never what a peripheral does.**
Peripheral behavior lives in the host's code, behind the registry; this
repository ships only small generic peripherals for tests and examples.
`mc683xx_mc68331::chip::Builder` stays the underlying API: a loaded board is
Builder calls plus the layers below, and a host can still build by
hand.  The keys are in `docs/board-files.md`.

- **Format:** TOML, versioned: `version = 1`; any other version, or
  none, is refused with a typed error.  Unknown keys are errors
  (serde's `deny_unknown_fields`), with the line, but every table takes
  a `note` the loader ignores, for provenance and confidence.
  Version 1 is frozen from 0.1.0, the first release: from then a change
  that makes a version 1 file mean something else, or refuses one, is
  version 2.  Before it the schema changed and stayed version 1, since
  no release read the earlier form: `[[device]]` became
  `[[peripheral]]` and `[chip] type` became required (2026-10-24), and
  smartclock's six board files are the consumer's to convert.  `[board]` names the board (`name`,
  `title`: what a host chooses it by), the firmware `images` it takes,
  the default first, and the `options` a run may enable.  Any memory,
  decoder, peripheral, unconnected select or net may carry `option =
  "name"`, on the board only when the run enables it: one file for a
  board with and without an option (the 58503A's front panel).  A
  net's endpoints on a peripheral left off go with it.
- **Chip:** the board is generic over the chip it is built around,
  `Board<C: chip::Chip>`, and `[chip]` holds `type`, the chip's name
  (`"mc68331"`, `Chip::TYPE`), required (`LoadError::NoChip`; a file
  names what it is built around, as a schematic does, rather than a
  host guessing), another than the one the board is loaded as
  `LoadError::ChipType`, beside the chip's own keys,
  which the chip's `Section` reads from the file's text, so an unknown
  key is an error with its line.  The MC68331's: DATA[15:0] as reset
  reads them (`data`), the clock as MODCLK selects it (`reference_hz`,
  MODCLK high, or `external_hz`, MODCLK low; MC68331UM 4.3.1, 4.6.3),
  the levels the board drives on port input pins out of power-on
  (`[chip.pins]`), the IPLs the board autovectors (`autovector`, SIMRM
  6.4).  What the board needs of a chip is `Chip` and its
  `ChipBuilder`: building from its section and the board's
  attachments (memories on selects or at a fixed address with their
  reload scope, a router per select or decoder with its DSACK pins,
  each peripheral's IRQ output and reset scope on no pin, the QSPI's
  PCS decode), running for a budget with its sync and stop requests
  (`chip::Requests`, the stop the core's `StopRequest`), the system
  clock and the clock outputs, its pins (`ChipPin`: name, kind,
  direction, and whether it takes a level ahead at its time), the
  outputs it watches, its events and the RESET pulse each reset
  drives, the CPU, and its snapshot.  The board's generic code names
  the CPU32 core and the SIM/QSM vocabulary of `mc683xx-modules`
  (chip selects, DSACK, reset causes, ADDR[23:0], the QSPI's PCS) that
  every part here is built from, and nothing of one part: `make
  features` builds it without the MC68331.  The impl is in the board
  crate (`mc683xx_board::mc68331`, its pins `mc68331::Pin`), not the
  chip's, which stays below the board: the trait is the board's, and
  the gdb stub and hosts that wire a chip by hand build the chip
  without the board.  `Board` is generic, not a `Box<dyn Chip>`: the
  run loop calls the chip a dozen times a quantum and a quantum may be
  an instruction, so the calls stay static, a host gets its chip back
  typed (`board.chip()` is an `&Mc68331`), and a pin is the chip's own
  type rather than a string; a host that serves several chips
  dispatches on `Description::chip_type` (decided 2026-10-24, before
  0.1.0).  The board's snapshot holds the chip's pins (its outputs'
  last levels, its clock outputs' clocks) in the chip's own pin
  type, and the levels the board drives on the chip's inputs are the
  chip's, in its snapshot, no longer the board's.
  `[[unconnected]]` names the selects the firmware programs with
  nothing wired; a cycle there ends as the chip ends one nothing
  answers.
- **Memories:** RAM of a `size`, or ROM or a flash array from an
  `image`, a path relative to the file, from `image_offset` (a bank's
  half), padded to `size` with `fill`; a whole number of 2-Kbyte pages
  (SIMRM Table 7-2).  An image that is one of `[board] images` takes
  the run's choice (`--firmware`).
  Mirroring is not a key: a select's block larger than the memory
  repeats it, as the address lines do.  A memory is served in the
  core, so the chip, not its attachments' roles, decides which cycles
  reach it: a read select the firmware also programs for writes
  writes it, and a ROM ignores writes, through a write select too.  Two 8-bit RAMs on the two
  lanes are one 16-bit memory.  Flash is its array, a memory, and its
  command logic, a peripheral on the write selects that programs the
  array with `Drive::write_memory`, the write landing as its
  instruction ends (the run ends there); a write, take or release
  between quanta (`advance`, `input`) lands as the board settles,
  before the next quantum and before `run_until` returns, so the CPU
  never reads past it and a snapshot holds it, however the host cuts
  the run.  Code fetch
  stays on the fast path.  While the command logic programs, erases
  or is in autoselect mode it takes the array (`Drive::take_memory`):
  the chip diverts the memory's cycles to a peripheral that passes them
  to the peripheral (`mc683xx_mc68331::chip::Diversion`, asked within a cycle and
  made as it ends, the page tables rebuilt), so status and autoselect
  reads see the peripheral, until it releases the array.  Chosen over a
  read hook on every fetch, which would cost the fast path always.
  A memory with `base` in place of `attach` is a fixed memory,
  emulator-only (Peripherals, Fixed memory), never taken; `reload`
  names the resets that put any memory's contents back.
- **Attachments, by role and lane:** a peripheral or memory attaches to
  one or more selects, each `{ select = "CS0", role = ..., lane = ...
  }`: role `read`, `write`, `read-write` (default) or `iack` (CPU
  space; SIMRM 7.8.1), lane `both` (default), `upper` or `lower`.
  Examples: flash read through CSBOOT and CS1 and written through CS6
  and CS7 over the same windows; RAM read through CS0 and written
  through CS2 (upper) and CS3 (lower); a DUART's acknowledge through
  CS10.  The file never states a window: the SIM decodes CSBAR and
  CSOR at run time, and a board that remaps at boot needs nothing
  more.  A peripheral's attachment may also give `lines`, the ADDR lines
  on the peripheral's address pins (a count from ADDR0, or an ordered list:
  a DUART's RS1 to RS4 on ADDR1 to ADDR4), and an `offset`; the peripheral
  sees the offset plus that address, with the attachment's index, role
  and lane.  Roles are the board's wiring, so the board layer applies
  what the chip cannot know: a write through a `read` attachment
  latches nothing, and a read through a `write` attachment is not
  driven and reads 0, as the chip reads a select nothing drives.
  Peripherals may share a select when their attachments split it (no two
  take one cycle on one lane: a read peripheral and a write peripheral, an upper
  and a lower peripheral); a word cycle through an upper and a lower peripheral
  is a byte access to each, and a command written to both bytes of a
  flash pair reaches both.  Every 8-bit peripheral follows one rule (SIMRM
  5.3.3; the user's choice, 2026-10-24): its data is on DATA[15:8],
  and a word to an 8-bit port is two byte cycles, as the SIM splits
  it; a word cycle through `lane = "both"` of a 16-bit port gives it
  the upper byte of a write and its read's byte on DATA[15:8]
  (`PeripheralAccess::written_byte`, `PeripheralAccess::read_byte`), the latch, the
  buffer, the DUART, the EEPROM and `registers` alike.  A peripheral on
  DATA7-0 states `lane = "lower"`.  A memory, a decoder or a pin of
  `unconnected` has its select to itself; any other sharing is an
  error naming both users.  A peripheral's `dsack` names the DSACK pins it
  drives on an externally terminated select (`open` if not known), and
  the chip checks them at each such cycle (`Builder::dsack`): PEPAR
  assigning none of them as DSACK leaves the cycle to the bus monitor,
  as the pins are then port E's (SIMRM 9.1, told as `Event::Dsack`,
  once a peripheral, what was told kept in the chip's snapshot); otherwise the port size they encode (SIMRM Table
  5-2) sizes the cycle, as the EBI sizes each cycle from the DSACK it
  gets (SIMRM 5.3.3), so a word to a DSACK0 peripheral on a 16-bit select
  is two byte cycles.  The select's port field sizes a cycle it ends
  itself (SIMRM 7.6), and one whose peripheral's pins are open.  The case
  that settled it is the Z3805A's DUART: CS8's field 8 bits, its DSACK
  external, PEPAR $32 assigning DSACK1 alone, and the firmware
  addressing the DUART at odd addresses only, as the lower byte of a
  16-bit port (smartclock's restart.md: the acknowledge is a 16-bit
  cycle with its byte on DATA7-0).  Its board file states `lane =
  "lower"`.  The
  earlier `DsackFault::PortSize`, which told the mismatch while the
  field still sized the cycle, went with it; `Event` and `DsackFault`
  are `#[non_exhaustive]`.
- **Address decoders, as data:** a decoder on one select (a 74AC138)
  splits its window by its inputs into outputs, each attached to a
  peripheral as a select is: `{ decoder = "u31", output = 2 }`.  Its
  inputs are ADDR lines or tied levels (`"low"`, `"high"`).  What a
  code with nothing attached does is the decoder's `undecoded`:
  `open-bus` (reads 0, writes go nowhere) or `bus-error`, and an
  `alias` table may send a code to another code's output.  Address
  bits outside the decoder alias, as on the board.  An enable gated by
  R/W is the outputs' roles.
- **QSPI PCS decode:** a peripheral attaches to the QSPI with `{ qspi =
  code, mask = pins }`: it is selected while PCS[3:0] under `mask`
  equal `code`, so four selects and a decoded peripheral number are both
  data (QSMRM 4.3.6.3).  No match shifts all ones in.  Attachments may
  overlap: more than one peripheral may be selected at once (QSMRM
  4.3.6.3, broadcast), and a peripheral on SCK ungated (`mask = 0`) sees
  every transfer.  So a transfer reaches every selected attachment, in
  file order, each through its own `Peripheral::transfer`, and MISO is
  resolved as wired-AND: the AND of the replies, an undriven peripheral's
  all ones.  That is an open MISO pulled high, contention resolving
  to 0; the manuals give no resolution for two drivers, and the board
  file has no pull to say otherwise.  A peripheral's output that a board
  leaves off MISO is that peripheral's parameter (`74x164`'s `miso`), not
  the decode's.
- **Peripherals:** a `type` from the registry, a unique `name`, a
  `parameters` table the type's constructor checks (an unknown or
  malformed parameter is an error naming the peripheral), an optional
  `marking` (the chip's marking or part number as fitted, free text
  for people; a peripheral key, not a parameter, since no peripheral reads it
  and a parameter no peripheral reads is an error; added within version 1,
  as it changes no file that loaded and refuses none), its
  attachments, an `irq` level (the peripheral's interrupt output wired to
  that IRQ pin; several on one level are wired-OR, as open-drain /IRQ
  lines are) and its `reset` scope (`ResetScope`; all causes by
  default, `[]` for a peripheral RESET does not reach; `Peripheral::power_on`, at
  time zero whatever the scope, is where such a peripheral states its
  outputs' first levels; chosen over initial levels in `PortSpec`, which
  could not depend on the peripheral's parameters or image).  A peripheral with no
  attachment is a peripheral on nets only (an OCXO, a GPS engine, an oven
  model), as first-class as one on the bus.
- **Registry:** type names to constructors and the ports each type
  declares, or, for a type whose ports depend on its parameters (an
  FPGA's design, a latch's width), each peripheral's own.  Built-in types
  are generic only: `fixed` (reads a value), `registers` (a register
  file), `recorder` (logs accesses), `clock`, `divider` and
  `edge-counter`.  A host registers its own types (smartclock its
  DUART, EEPROM, FPGA and GPS engine), Python its classes by name.
  The file names a type by its name alone, so a plugin source (a
  dynamic library, later) registers names into the same registry and
  no file changes.  A model written against the chip's traits
  (`MappedPeripheral` and `Timed`, `SpiPeripheral`) registers through an adapter
  (`adapter::MappedAdapter`, `SpiAdapter`) that converts cycles and times
  through the board's `TimeBase`, so smartclock's models serve both a
  hand-built chip and a board.  An adapted model implements
  `mc683xx_cpu32::cpu::state::Save`, as every peripheral does, and is in
  the board's snapshot: a trait, not serde, so a model keeps its own
  format and versions it, and one with state it cannot serialize (a
  file handle) is `unsavable!`, which the board's save names.  One
  constructor, `new`: the adapter needs no opt-in.
- **Nets:** named and typed, `{ name, type, from, to = [...] }`.
  Types: `logic` (a level whose state matters), `edge` (a level whose
  changes' times matter, 1 PPS), `clock` (a frequency in nanohertz
  and an exact phase: a source whose frequency moves, an OCXO on its
  EFC, drives a retuned clock at each change, its phase carried across
  exactly, so a consumer counts and times edges across it with no
  rounding), `serial` (UART frames, `signal::Serial`: an `sci::Frame`
  timed from its start bit, with its baud and length, so a receiver
  times either edge; one direction, so a link is two nets), `analog` (an `f64`; data only, never time; a
  sink samples it at its own time, which asks the driver's
  `Peripheral::sample` then, so a value that moves continuously, a warming
  oven, need not be driven at every change), `bus` (parallel lines as
  one word, so an ADC's byte into an FPGA changes in one drive, not
  eight).  One driver, except on a `wired-and` net: open drain with a
  pull-up, several drivers, the level
  the AND of theirs (an FPGA's DONE and the port F pin the firmware
  pulls low), an `in-out` port or a chip port pin both driving and
  sensing.  Endpoints are peripheral ports the registry declares
  (`fpga.pps`, each with a direction and a type), chip pins
  (`chip.PGP0`-`PGP7`, `chip.PAI`, `chip.PCLK`, `chip.PWMA`,
  `chip.PWMB`, `chip.PC0`-`PC6`, `chip.PE0`-`PE7`, `chip.PF0`-`PF7`,
  `chip.PQS0`-`PQS7`, `chip.IRQ1`-`IRQ7`, `chip.RESET`, `chip.ECLK`
  (a clock, the system clock / 8 or / 16 by SYNCR EDIV, a rising edge
  at power-on, our choice since the manuals give no phase, retuned
  when the system clock or EDIV changes; a peripheral on an ECLK-mode select
  aligns its cycles to it through its E input), `chip.TXD`, `chip.RXD`), and external
  endpoints (`external.host`) the runner
  binds.  Types and directions are checked at load.  Examples a board
  needs: GPS serial to DUART A; GPS 1 PPS to the FPGA and to PAI; the
  OCXO's 10 MHz to the FPGA; the FPGA's 1 PPS to IC1, INIT to PCLK,
  DONE to port F; the EFC DAC to the OCXO's control (analog); latch
  bits to front panel inputs.  A peripheral's IRQ stays `irq` (the chip
  samples `MappedPeripheral::interrupt_request`); a net to `chip.IRQn` serves a
  peripheral that drives the pin as a net.  Levels are electrical
  throughout: IRQ pins and RESET are active low.  `chip.RESET` goes
  low at each reset the chip reports and high 512 system clocks later
  (the `reset` instruction's pulse, SIMRM 8.2.6; an internal reset's
  shortest, in clocks of SYNCR's reset rate, MC68331UM 4.6.6), and a peripheral driving it low resets the chip
  and holds it in reset: no instruction runs while board time and the
  peripherals go on, and as RESET rises reset processing begins, the
  chip's clocks mapped to board time from there (MC68331UM 4.6.6; the
  512 clocks the chip then drives RESET are its reported reset's
  pulse, and the CPU starts at once, as at every reset here).
  Chip time stops while RESET is held, rather than running on as when
  halted: a halted chip's modules run, and the software watchdog,
  enabled out of reset (SYPCR SWE, SIMRM 3.5), would time out and
  reset it again and again, while a chip held in reset runs nothing.  So its
  clocks are anchored to board time again at the release, a move of
  the time base as a SYNCR write's is (Board time).
  On a wired-and net the chip senses the AND of the net's other
  drivers, its own left out: the RESET pin is an open-drain output
  and an input (MC68331UM 4.6.6), and the chip's own pulse, a
  `reset` instruction's, resets the board but not the chip.
  DSACK, AVEC and MODCLK are configuration, not endpoints.
- **Board time:** `Time`, integer picoseconds in a `u64` (213 days),
  with no floats in scheduling.  The board's scheduler holds the
  peripherals' next events and the drives in flight, in (time, sequence)
  order, and runs the chip with `run_for(budget)` in quanta that end at
  the earliest of them.  Chip cycles convert to board time from an
  anchor (`TimeBase`: cycles, time, system clock), public so peripherals
  convert as the board does, moved only at a sync point in
  which the frequency changed (SYNCR, a reset), so it never drifts;
  toward the chip a time rounds up to the first clock at or after it
  (Peripherals, GPT input edges), toward the board a clock rounds
  down.  A peripheral gets the anchor in every `Drive` and, at load and at
  each change, through `Peripheral::system_clock`.  Edges between
  peripherals are never quantized.  A peripheral drives a port at its time or
  later; a drive to a GPT input is scheduled into the GPT at once
  (`schedule_gpt_input`), so a peripheral that drives an edge ahead of time
  (a divider of a clock knows its next edge) is captured at exactly
  its clock.  The board keeps each such edge on board time until its
  clock comes (in its snapshot), and at every move of the time base
  (a SYNCR write, a held RESET's release) withdraws the GPT's
  (`Chip::unschedule_inputs`) and schedules them again at their new
  clocks, so an edge keeps its board time across the move
  (`a_gpt_input_edge_driven_ahead_keeps_its_time_across_a_syncr_write`,
  `a_gpt_input_edge_during_a_held_reset_shows_as_reset_ends`: one
  driven during the hold shows as RESET rises).  A peripheral called within the chip's run (a bus cycle, an
  acknowledge, a QSPI transfer, a reset) that drives a port before the
  run's end, writes a memory, or moves its next event before the run's
  end ends the run after that instruction (`Shared::call` asks the
  chip's `SyncRequest`, and `run_for` returns `Stop::Budget` short of its
  budget), and the board settles there: the drive reaches its nets,
  ports E, F, QS, the IRQ pins, RXD and RESET by the next instruction,
  and the run is the same however the host cuts `Board::run_until`
  (`a_peripheral_driven_within_a_run_gives_the_same_run_however_it_is_cut`:
  1 ms, 10 ms, 100 ms runs and one).  Before, the drive waited for the
  sync point, which could be the whole run: firmware polling PQS0
  after writing a latch that drives it saw the change after 790 polls
  in 10 ms runs and never in 100 ms ones.  Each such call costs a
  quantum (docs/performance.md, Board peripherals).  Chip outputs reach the
  nets after the quantum in which they changed, each at its own
  converted time (`take_port_changes`, `take_gpt_changes`,
  `take_sci_transmitted`, `take_qsm_changes`; a port QS pulse within
  a quantum is both its edges:
  `port_qs_edges_keep_their_times_however_the_run_is_cut`).  An output on a net that a peripheral, the chip or an
  observer (Front ends) senses
  ends the chip's run where it changes (`Mc68331::set_output_stop`,
  as `set_clock_stop` does for the clock): after the instruction whose
  write changed it, or at the sync point where a compare, a PWM edge,
  a frame's start on TXD or the QSPI's PCS changed it, and after a
  reset the chip drives on a sensed RESET.  So a loop from a chip
  output through a peripheral back to the chip is the same however the host
  cuts the run
  (`a_chip_output_through_a_peripheral_gives_the_same_run_however_it_is_cut`:
  PF0 through a buffer to PQS0, 1 ms, 10 ms, 100 ms runs and one).  An
  observer senses as a sink does, so one that stops the run (a
  `BoardStopRequest`) stops it at the change
  (`an_observer_that_stops_at_a_chip_output_stops_the_run_there`); the watched outputs are set again as observers attach
  and detach (`Chip::stop_at_outputs`).  A run-time endpoint does not
  sense: nothing reads what reaches it before the run returns
  (`take_received`), each value with its time.  An
  output on no net, or on a net only run-time endpoints sense, costs
  nothing; a sensed one a sync a change (docs/performance.md, Board
  peripherals).  ECLK changes only with the clock, which ends the run
  already.  The chip compares levels after each write and quantum, so a
  pulse within one instruction (two writes of one `movem`) does not
  end the run; both its edges still reach the net at their times.  A
  drive is a commitment: it cannot be withdrawn.
- **Tracing:** records, not text (`docs/tracing.md`): peripheral cycles
  with their instruction's PC (a code hook on every instruction, only
  while traced), exceptions with the SR their frame stacked and code
  watches with the registers and stack (each a hook that stops the
  run, which the board records and goes on from), pin edges with
  their clock and board time, QSPI transfers, positions and the run's
  end.  The text, `CHIP: KIND key=value ...` after a `# mc683xx trace
  2 chip=CHIP` header, the chip's `Chip::TYPE` the prefix, is
  smartclock-emu's shape, so a smartclock run and a board run diff
  line for line.  smartclock-emu's own traces call a bus cycle
  `device`; boot-diff reads that dialect.
- **Run-time endpoints stay out of the file:** the file names them,
  the runner binds them (`--bind host=stdio`, `stdout`, `pty`,
  `null`, `file:PATH`).
  Paths for working copies (an EEPROM's), the seed and the run length
  are runner options, handed to constructors as an `Environment`;
  every noise model draws from the one seed (each peripheral its own
  stream, derived from the seed and its name).
- **Run-time parameters:** a run sets a peripheral's parameters over the
  file's (`Options::parameters`, `set_parameter`, the runner's `--set
  PERIPHERAL.KEY=VALUE`, Python's `parameters=`): an EEPROM's working
  file, a GPS receiver's start, an OCXO's settings.  Each key replaces
  the file's and is checked as the file's are, by the constructor;
  a peripheral the file does not name is an error, so a typo is not
  silently ignored, but one an option leaves off is not, so one
  command line serves the board with and without it.  The merge is
  per key, not deep: a table-valued parameter is replaced whole.  A
  host holding TOML builds the values from its text
  (`Parameters::parse(text)?.into_table()`, `Options::set`), not from
  the parser's `toml::Value`: a `From<toml::Value>` would put the
  parser's types, and its major versions, in the API.
- **Board choice:** the runner takes a board file; a host may choose
  one itself (smartclock from the image's model string) and load it
  with its registry.
- **Clock changes:** the board maps board time to the chip's clocks
  through one `TimeBase` between frequency changes.  It moved the base
  at the sync point that wrote SYNCR, so board time ran at the
  old rate for the rest of that quantum (up to the next event, a 50000
  clock error in the test that found it).  Now the board sets the
  chip's `set_clock_stop`: a frequency change ends `run_for` after its
  instruction, and the board moves the base and its time there.  The
  synthesizer locks at once here (always SLOCK), so the change is at
  the write.
- **Run loop:** `Board::run_until(until)` is the library's, for hosts to
  embed (smartclock-emu builds its own command on it); `send` and
  `take_received` move values through run-time endpoints between
  runs.  `Board::run_until_quantized(until, quantum, before)` runs it in quanta
  of board time (a `NonZeroU64`: a quantum of 0 would never end), calling `before` before each (the Python bindings'
  Ctrl-C checks), `Break` ending it `Stop::Requested`; a board halted
  before a quantum goes on to the next, so the quanta change nothing.
  `Board::stop_request()` is the host's `board::BoardStopRequest`
  (not the core's: it holds the board weakly, so a peripheral may keep
  it, and stops the board's loop as well as the chip), which stops the
  run as a peripheral's `Drive::stop` does: for a peripheral that fails
  in a call with no `Drive` (the Python bindings' peripherals, in
  `next_event` or `interrupt_request`).  The loop looks for a request
  before each quantum and after asking the peripherals' `next_event`, and
  withdraws the chip's when it stops for one, so the next run does not
  stop for it again.  `send` drives the nets the endpoint drives
  whose type carries the value, and fails (`SendError`) if there are
  none, so a typo or a value of the wrong type is not silently
  dropped; `take_received` likewise fails (`ReceiveError`) for an
  endpoint no net drives, rather than giving nothing for ever; the runner refuses at start-up a binding with
  input (stdio with standard input, a pty) to an endpoint that drives
  no serial net, naming it, and binds such an endpoint only to
  output-only `stdout`, `file:PATH` or `null`.  `stdout` is `stdio`'s
  output alone: it never reads standard input, so the first `stdio`
  binding on the command line (not by endpoint name) still takes it; bindings sharing standard output each write
  to it, a turn's bytes in endpoint order.  The board advances a peripheral whenever its `next_event` is due,
  so a peripheral whose `advance` leaves it due would spin the loop: after
  1000 advances of one peripheral to one time running, its next event never
  past it, `run_until` fails with `RunError::Stalled` naming the peripheral (a
  library does not panic on a host's bug).  The bound allows a peripheral
  several events at one time; counting per peripheral, not per loop, keeps
  two stuck peripherals from resetting each other.  A run's end is never
  past its `until`, though the chip's last instruction may end past
  it, as a quantum's does.  `run_until` returns
  `Result<BoardRun, RunError>`.
- **Front ends:** a TUI or GUI is the host's, outside this
  repository, and the board file stays hardware only: no widgets,
  windows or key bindings in it (decided 2026-10-24).  A front end
  watches the board through observers and moves it only through the
  board's own inputs (externals and `send`); a widget that is a
  peripheral on the board (a lamp, a key) is a peripheral of the host's
  type, read back with `Board::peripheral::<T>`.  `Board::observe(net,
  observer)` attaches an `observe::Observer` by net name after load,
  told each change at its board time, in `process`, after the net's
  sinks and only where they are told (a wired-and net's level when it
  changes): the same stream in the same order.  It gets the change by
  value and nothing of the board, so it can drive nothing and stop
  nothing; one that should stop the run holds a `BoardStopRequest`.
  So observing cannot change a run, and observers are outside the
  snapshot.  A restore keeps them attached and, once it succeeds,
  tells each its net's restored value and board time
  (`Observer::restored`, default nothing, so closures keep working
  through the blanket impl): a UI's view would otherwise show the
  abandoned future until the next change.  It is a call apart from
  `changed`, not a change: the restored value need not differ from
  the last one told, and a logger of changes should not log it.  In
  Python it is `observe`'s `on_restore`, whose exception `restore`
  raises without stopping anything.  The observers live in a list per net, so an unobserved
  net's change costs one empty-list check.  An observer is not told
  the net's value as it attaches, which would be a call at no board
  time; `Board::net_value` gives it, and changes still waiting (the
  power-on levels before the first run) do reach it.  In Python the
  callback's exception is held and stops the run through the board's
  stop request, as a peripheral's, and the board's `__traverse__`
  reaches the callbacks.  A callback stops the run without raising
  through `Board.stop_request()`, Rust's `BoardStopRequest` as a
  Python class, rather than `BoardControl.stop()`: the Rust name, and
  it ends the run at the change, where the control's stop is taken
  only at the next sync point, up to a quantum later.
  `Peripheral::inspect(&self) -> Inspection`
  (default empty) shows a peripheral's state as a tree of named
  fields (integers with a radix hint, booleans, reals with a unit,
  text, bytes, groups), for `Board::inspect(name)`: read-only (`&self`,
  so a DUART shows itself as of its last call rather than catching
  up), outside the snapshot and no part of determinism, so its fields
  may change without a format version.  A typed tree rather than
  text, so a front end lays it out itself; Python gets plain dicts,
  the hints dropped, and a Python peripheral defines `inspect()`.
  An adapted `MappedPeripheral` or `SpiPeripheral` model shows itself through
  `adapter::Inspect`, the board crate's own trait, its adapter made
  `inspecting` as `saving` takes `Save`: an opt-in, since stable Rust
  cannot ask whether a model implements a trait, and the modules crate
  stays free of the board's types.
  `Board::control() -> control::BoardControl` is the board's one
  thread-safe handle (an `Arc` of an atomic flag, a mutex and a
  condition variable; `Send`, `Sync`, `Clone`), for a front end whose
  event loop is another thread: `stop()`, `send(name, signal)`, checked
  as it is queued against the endpoints fixed at load (the same
  `SendError`s as `Board::send`), and `set_pacing(pacing)`.  `run_until`
  takes what it holds at each sync point, one atomic load a quantum
  when nothing waits: the inputs driven at that board time in the
  order queued, the stop ending the run there (`Stop::Requested`).
  Nothing in the chip or a peripheral is touched from another thread:
  the core stays single-threaded, and a stop cannot cut a quantum
  short as a `BoardStopRequest` does, so it waits for the quantum's
  end.  `Board::send` takes what the control holds first, so an input
  queued through it never lands after a later direct send.  The same
  inputs at the same board times give the same run; a
  live UI's input lands at the sync point after it is queued, timed
  by when the UI sent it, which is inherent to live input.  Pacing
  (`pacing::Pacing`: `Unpaced`, the default, `RealTime`, `Scaled(f64)`
  board seconds a wall second) is host-side only: `Board::run_until_paced(until,
  quantum)` runs as `run_until_quantized` does (`pacing::QUANTUM`, 5 ms, a good
  quantum) and, before each quantum, a `pacing::Pacer` sleeps until board
  time is no longer ahead of wall time times the factor, waking for a
  stop or a change of pacing; a run that reaches its end waits once
  more after its last quantum, so it never returns ahead of wall time
  and short paced runs in a loop (a UI's frame at a time) are paced
  too.  The wall clock (`Instant`) is read
  there and nowhere else in the board, and only decides how long to
  sleep, so a run is the same whatever its pacing
  (`a_paced_run_is_the_unpaced_run`).  A run that falls behind is not
  held back and runs unpaced until it catches up; a new factor or a
  restore starts the pacing again from where the board is.
- **Runner:** `mc683xx-run board.toml [--cycles N | --seconds S]
  [--firmware PATH] [--image NAME=PATH] [--option NAME]
  [--set PERIPHERAL.KEY=VALUE]
  [--bind NAME=stdio|stdout|pty|null|file:PATH]
  [--trace bus-cycles,exceptions,pins,qspi,nets] [--watch-code ADDR]
  [--position S] [--seed N] [--work-dir DIR] [--restore FILE]
  [--save FILE] [--turn-us N] [--input-after S] [--gdb HOST:PORT]
  [--pace unpaced|realtime|FACTOR]`,
  a thin command on
  the run loop with the built-in types only: it runs in turns of
  board time, between which input goes in and output comes out, and
  ends with a `stopped` record.  With `--cycles` the last turn ends at
  that cycle's board time, so the run ends within an instruction of
  it.  `--save` saves the board once before the run and creates a
  temporary file beside its target (`.NAME.PID.partial`), so a
  peripheral that cannot save, or a directory that cannot be written,
  fails at once rather than after the whole run; at the end the
  snapshot is written whole to it and renamed over the target, so an
  old snapshot is replaced whole or not at all, and a second Ctrl-C
  removes the temporary file and leaves the target as it was.  Exit status 0 when the run spent its
  time, 1 for a board or file error, 2 for a wrong command, 3 when the
  CPU halted, 4 when a hook or a peripheral stopped it, or Ctrl-C did.
  `--pace` paces the turns against wall time (`Board::run_until_paced`'s
  pacing, the run the same whatever it is); Ctrl-C (SIGINT), through
  the board's control handle, ends the run at the next sync point, so
  the `stopped` record and `--save` still come.  A second Ctrl-C ends
  the process at once, exit status 130 (the shell's for SIGINT),
  writing nothing more and saving nothing, for a run that cannot reach
  a sync point (one waiting on piped input).  The handler is set
  before `--gdb`'s session, whose waits for gdb are polled
  (`stub::accept_polled`, a `Waiter` that looks for Ctrl-C between
  them): Ctrl-C while waiting for gdb, or while gdb holds the board,
  ends the session and the run, with the `stopped` record and
  `--save`, at the next wait.  A wrong command
  includes seconds (`--seconds`, `--position`, `--input-after`) that
  are not finite or are negative, and a `--turn-us` of 0 or one whose
  picoseconds overflow.  Piped input is read
  whole and fed a byte a turn from `--input-after`, so a run with it
  is reproducible; a terminal's or a pty's is not.  It is read when
  input first goes in, after `--gdb` has said where it waits, and the
  run waits there for the pipe's end: a writer that waits on the
  runner's output deadlocks, which is the price of determinism.  The one binary in
  the workspace, with typed errors as the libraries have.  It builds
  on any host; `pty`, through `nix`, is Unix's, and elsewhere the
  binding is refused.  A pty's master does not block: output its
  buffer cannot take, with nobody reading the slave, is dropped, as a
  UART's frames are with nothing on the line, so a run never waits on
  a reader.

Adversarial review of the chip trait (2026-10-24):

- *What breaks.*  Every host names its chip: `Board::<Mc68331>::load`,
  `Board<Mc68331>` in its types.  Chip pins are checked when the
  board loads, not when the file is read, so `Description::parse`
  accepts `chip.NMI` and the load refuses it.  `trace::PinRecord::pin`
  is the pin's name, a `&'static str`.
- *What it costs.*  Nothing measurable in the run loop: the trait is
  static, its few dynamic calls (`Requests`, the outputs a quantum
  emits) are per sync, not per instruction (docs/performance.md).
- *What it forecloses.*  A chip whose selects, DSACK or reset causes
  are not the SIM's (a SIM40 part) needs those types generalized in
  `mc683xx-modules` first; a chip that is not a CPU32 does not fit
  (`Chip::cpu`).  Two chips on one board are out, as they were.

Adversarial review of the board layer (2026-10-23):

- *Cost.*  A peripheral on the bus is two dynamic calls deep (the chip's
  peripheral, then the shared peripheral through `Rc<RefCell<..>>`), and a
  decoder a third; only peripheral cycles pay it, never RAM or ROM in the
  core.  The scheduler ends a quantum at every board event, so a 10 MHz
  clock as edges would sync the chip to death; clock nets carry a
  frequency and phase instead, and peripherals that count a clock compute
  its edges.  Edge-heavy nets (a peripheral toggling a pin every few
  cycles) cost a sync each, as the chip's own events do.
- *What it forecloses.*  Strict per-pin read or write on memories
  served in the core (the chip's page builder would need roles; a
  peripheral does it today).  Flash status reads leave the fast path
  for the memory while its peripheral holds it, a page table rebuild at
  each take and release.
  Retractable drives (a model that changes its mind about an edge it
  already drove).  Resolved nets other than wired-AND (a bus with
  several push-pull drivers is an error, as on the board).  Clock
  frequencies finer than a nanohertz.
- *Determinism.*  Integer time, a (time, sequence) queue, peripherals and
  nets in file order, one seed: the same file, image, seed and
  bindings give the same run.  A binding to a terminal or pty is host
  timed, so a run with live input is not reproducible; piped input is
  fed one frame per turn of fixed length, which is.  A SYNCR write
  moves the cycle-to-time anchor only at the sync point, so board
  time drifts by at most that quantum at the old rate; firmware writes
  SYNCR once, at boot, before any edge matters.
- *Latency.*  A peripheral's drive within the chip's run reaches the chip
  by the next instruction, and a chip output on a sensed net reaches
  its peripherals after the instruction that changed it, so a loop from the
  chip through a peripheral back closes by the instruction after.

### Debugger

`mc683xx-gdb` serves gdb's remote serial protocol through the
`gdbstub` crate (MIT OR Apache-2.0, a Cargo dependency like any other,
so not in `third_party/NOTICE`), over a small trait, `target::Target`:
`with_cpu` (registers and hooks), `peek` and `poke` (memory with no
bus cycle), `run_for(cycles, hit)` (a quantum) and `reset`.  The bare CPU
(`flat::Machine`), the chip and a board implement it; the runner
(`--gdb`) and the Python bindings (`serve_gdb` on `Machine`, `Mc68331` and
`Board`) implement it over those, keeping their own work between
quanta.  docs/gdb.md is the guide.  The chip's and the board's
`Target` impls are features (`mc68331`, `board`), on by default, so a
bare-CPU host builds the stub without the chip and board crates; the
runner and the Python bindings name `board`, and `make features`
checks the bare build.

- **Registers:** gdb's m68k core feature (`d0`-`d7`, `a0`-`a5`, `fp`,
  `sp`, `ps`, `pc`), with no FP feature (the CPU32 has no FPU), and a
  feature of ours with USP, SSP, VBR, SFC and DFC; the architecture
  `m68k:cpu32`.  A `G` packet without our feature (a gdb that did not
  read the description) sets only the core registers.  The gdb manual
  defines `G` only with the whole block: a shorter one sets the
  registers it gives whole, from D0, and leaves the rest; one that ends
  within a register gets `E16` (EINVAL).  gdbstub would end the
  session on either.  The stub serves the description itself
  (gdbstub's override), so a read of an annex other than
  `target.xml` is `E00` (the gdb manual, `qXfer`), not the session's
  end.  A packet gdbstub cannot parse (`M0,4:zz`, `Z0,zz,2`) still
  ends it, as a `Protocol` error: gdbstub has no reply for one.  The
  runner takes a failed session (`Connection` or `Protocol`) as a
  detach, said on standard error, so one bad packet does not cost the
  run; a `Target` error, the board's, still ends it.
- **Memory:** supervisor data space, no bus cycle: memory the core
  serves (ROM too, so `load` works), module registers read through
  `Module::peek`; peripherals neither read nor written (`E0e`).  A
  debugger's look never has a side effect; a host wanting real cycles
  makes them with `bus_read` and `bus_write`.
- **Breakpoints and watchpoints:** code hooks for `Z0` and `Z1`,
  nothing written to memory; read and write hooks for `Z2` to `Z4`,
  widened by three bytes below so a word or long that starts before a
  watched byte is seen, then filtered by overlap.
- **Steps:** a code hook on every address, removed after; if it
  stops the run before the first instruction (no hook stop to resume
  past), a second run executes it.  Exact: one instruction, or into
  an exception's handler.
- **Continue:** quanta of 100,000 clocks, the chip's by `run_until`;
  between quanta a non-blocking look at the connection, for Ctrl-C.
  No thread: the core stays single-threaded, and the stub owns the
  target while gdb is attached.
- **Stops gdb did not ask for:** the host's own hook stops are
  `SIGTRAP`, a halt and a bare host's fault stop `SIGBUS`, a stop
  request `SIGINT`.

Adversarial review (2026-10-24):

- *Cost without gdb.*  None: the stub is outside the core, which it
  reaches only through hooks and registers that exist already; no
  flag, test or branch was added to the run loop.  The Python bare
  `Machine`'s dispatch loop asks one predicate per core run (`false`
  outside a session), so the stub's stop is not taken for one of its
  callables' and run past.
- *Cost with gdb.*  A breakpoint is a code hook (its page flagged,
  `special` set, so every instruction tests one flag and pages with
  hooks look the hook up); a watchpoint takes its pages off the fast
  path; a step puts a hook on every page for one instruction, and
  setting and clearing hooks rebuilds the fast tables, O(mapped
  memory) per step.  Quanta cost two system calls each (TCP's
  non-blocking peek) per 100,000 clocks; three from Python, whose
  polled connection sets the stream back to blocking, and a GIL
  release.
- *Determinism.*  A session is as deterministic as the run it
  debugs, but for Ctrl-C, which lands at the quantum boundary the byte
  arrives by.  Stops are where the same run would stop; a series of
  quanta is one long run (`run_until`).
- *What it forecloses.*  Non-stop mode and threads: one CPU, all
  stop.  gdb writes reach only memory the core serves, so `set var`
  on a peripheral or module register fails (a debugger mode with real
  cycles could be a monitor command).  The stub's hooks are ordinary
  hooks: the host sees their ids allocated, and its own stops reach
  gdb as `SIGTRAP`.  Firmware's own `bkpt` is the CPU32's (CPU32RM
  4, BKPT), never gdb's: breakpoints never touch memory.
- *What it needs of the core.*  Nothing new: `instructions()`,
  `add_code_hook` and the memory hooks, `Memory::entry` and
  `region_mut` for pokes, `Mc68331::peek` for peeks.

## Architecture

### Interpreter

- 64K-entry opcode table of handler functions, built at startup.
  Handlers are generic over size and effective-address mode with const
  generics, specialized by family x size x mode, not per raw opcode.
  Built for MOVE (size, source and destination kinds), MOVEA, TST,
  ADD, SUB, CMP, AND, OR and EOR in both directions, ADDA, SUBA, CMPA,
  the immediate and quick arithmetic and logic, NEG, NEGX, NOT, CLR
  (size and operand kind), the shifts (size, type, direction and count
  or operand kind), the bit instructions (operation and operand kind),
  LEA and PEA (source kind).  A kind is the mode field with mode 7's
  submodes told apart (`effective_address::kind`); the family picks
  its handler per opcode when the table is built (`Table::set_by`,
  the `specialize!` macros), and the decode, inlined, folds to the one
  kind.  The rest (ADDX, SUBX, CMPM, TAS, MOVEM, the BCD, multiply
  and divide, control and system instructions) still decode their
  fields at run time; each is rarer, longer or both.
- Handlers return `Result<Flow, Exception>`: a fault in any bus cycle
  propagates with `?` to the run loop, which takes the exception.
- The run loop keeps its per-instruction work small: one `Core` for
  the whole run; the handler and timing tables loaded once a run, as
  arrays an opcode indexes unchecked; the opcode fetch's fast path
  inlined; the common case (completed, untraced, no released fault)
  first, and the rest (exceptions, trace, a released fault, an
  interrupt taken) out of line (`docs/performance.md`).
- Each family registers its opcodes from its own module
  (`instructions/<family>.rs`); an opcode registered twice panics when
  the table is built, which the tests catch.
- Shared by every family: effective addresses (`effective_address.rs`,
  CPU32RM 3.4; a full-format extension word with BD SIZE 00 or bits
  3:0 other than 0000 is an illegal instruction, 6.2.8) and the
  general condition codes and tests (`flags.rs`, CPU32RM Tables 4-1
  and 4-10).

### Memory

- Two page tables, user and supervisor: all the chip-select decode can
  tell apart outside CPU space (SIMRM 7.10.3).  Program and data share
  a table; hooks and tracing still see the FC of every access.
- A RAM or ROM entry points at host memory; an empty one routes to the
  `Bus`.  Per-page permissions and hook bits live in tables beside
  the entries, not in them, so the chip's rebuilds from the decode
  leave them; the host sets permissions, the hooks set their bits.
- The tables are two-level: a directory of slabs of 1024 pages (2
  Mbytes), each allocated when a map or a protection first reaches it;
  a page in no slab is on the bus with every permission, and a hook
  over it marks its slab, so the 32 address lines a bare host wants
  cost only what it maps.  A snapshot holds the slabs allocated.  The
  lookup adds one load to the fast path's reads and writes, within the
  noise (+-5%) of the core benchmarks, measured interleaved on one
  pinned core; fetch, through its cached page, is unchanged.  A slab
  is built on the stack and moved to the heap: about 230 Kbytes, which
  an optimized build places directly but a debug build may not, so a
  debug build on a thread with a small stack makes its `Machine` (or
  `Memory`) on a thread with a bigger one.  Building each table on the
  heap instead measured 6% slower on `core/nop`.
- The entries are rebuilt from the chip-select decode whenever a
  CSPARx, CSBARx or CSORx write changes it (rare: at boot).  Pages are
  no larger than the smallest chip-select block (2K).  A fixed
  memory's pages are served where no select matches a direction's
  cycles, at its timing.
- A page is served in the core, for reads or for writes, only where
  host memory alone answers its even byte, odd byte and word cycles
  in that direction, on lanes the selects cover; it takes the timing
  of the select whose DSACK comes first.  A page shared with a peripheral,
  and the module space, go to the `Bus`.
- The same rebuild records the selects whose blocks hold each page;
  a cycle to the `Bus` tries only those (all twelve while a decode
  change awaits its rebuild).
- The `Bus` gets the core's memory with each cycle
  (`Bus::read_with_memory`, `write_with_memory`), so the chip rebuilds
  the tables in the cycle that changed the decode, and serves a
  region itself on a page the core cannot (a select shared with a
  peripheral).  Neither can wait for the end of a quantum.
- The core splits a long operand into two word cycles, high word
  first, as the CPU32 does on its 16-bit bus; the `Bus` sees only byte
  and word cycles.
- Addresses are masked to the chip's address lines (24 on the 68331,
  ADDR[23:0], MC68331UM 4.4.1.1), so the upper lines alias.  The
  manuals do not say what the CPU32 drives on A[31:24] inside the
  68331; aliasing is our choice.
- Instruction fetch caches the page it is on: its space, its region
  and offset, and a word's stall (`Memory::fetch`), so fetch looks no
  page table up while the PC stays on the page.  It caches the
  mapping, not the words, so fetch always sees memory as it is now,
  with no stale prefetched words, whoever wrote it (the CPU, the
  host's `bus_write` or `memory_mut`, a restore), and only a change to
  the tables (a rebuild, a map, permissions, hooks, a restore) or to
  a region (freed or trimmed) drops it.  CPU32RM
  does not say what a write to an already-prefetched word does; this
  is our choice.  It serves only what the fast table serves, so a
  hooked or protected page still fetches through the slow path.

### Interrupts

- Level-sensitive, as on the chip: taken between instructions when the
  mask admits it.  Level 7 is taken on a transition to 7, and when the
  mask drops below 7 while the request stays at 7 (CPU32RM 6.2.11).
- A bus error at IACK gives the spurious interrupt vector, 24 (CPU32RM
  6.2.11; SIMRM 6.4.3 says $F, and CPU32RM governs).  Autovectors 25-31
  come from AVEC, a chip select's AVEC bit included.
- IARB arbitration among SIM, QSM, GPT and external sources lives in
  `mc683xx-modules`/`mc683xx-mc68331`, at IACK time.
- The CPU samples the request level every instruction; the board
  caches it and recomputes it only after a bus cycle to the board, an
  acknowledge, a quantum's `advance`, a reset, or the host's change of a
  pin, peripheral or module, the only events that change a source (1.5x
  on code from flash, `docs/performance.md`).  Debug builds check the
  cache against a fresh level at every sample.
- Hosts with an edge-style API (smartclock's
  `interrupt(level, vector) -> taken`) wrap this.

### Hooks

- Code address: a per-page flag, then a lookup only on flagged pages.
- Block: straight-line code to the next change of flow.
- Memory: installing one marks the pages slow-path; other RAM stays
  fast.
- The fast path reads its own copy of the page tables, in which a
  page with a read or write hook, or less than every permission,
  serves nothing: its cycles miss, as a peripheral's do, and the miss path
  (out of line) finds the page special and runs the permission check,
  the real table's memory and the hooks.  So a cycle on other RAM
  costs what it did; an instruction tests one flag, `Memory`'s
  "special" (some page has a hook bit or a permission), before
  looking for code and block hooks.  Invalid access and exception
  hooks need no flag: bus errors and exceptions are slow already.
- When hooks run: code and block hooks before the instruction; a stop
  there leaves it unexecuted, and the next run executes it without
  calling the hooks that already ran for it.  Memory hooks after each
  byte or word cycle (a long is two), operand reads only, not
  fetches; on the bare CPU (`Cpu::set_long_bus`) a long operand
  instead once, as size 4 at its address, once all of it has ended
  well, as a peripheral sees it and as a Unicorn hook does: a hook
  matches by that address, and a long that faults midway shows
  nothing, its finished first word included (the invalid access hook
  sees the faulting word); invalid access hooks before the bus error is taken;
  exception hooks once the frame is stacked and PC is the handler's.
  A stop from these takes effect when the instruction (or interrupt
  entry) ends.  Hook addresses are on the address lines (masked).
- A block starts after a change of flow (`Flow::Changed`), an
  exception, reset, restore, or a host jump: a run that finds PC
  other than where the last run, reset or restore left it (the core
  remembers that PC, so a resume with PC unchanged, or written to the
  same value, is no jump and keeps the stop's skip).  A snapshot taken
  at a hook stop keeps that hook's skip on restore, so no block hook
  fires at that instruction: restore resumes exactly where the run
  stopped (decided 2026-10-23).  While no hook or permission is
  set nothing records it, so the first instruction after one is set
  may count as a block start.
- The bus can ask for a stop too (`Bus::stop_requested`, asked after
  each cycle it ends, after the `reset` instruction's RESET and after
  an interrupt acknowledge): the chip's `StopRequest` handle, for its
  peripherals, ends the run after that instruction, or between quanta if
  a peripheral's `advance` asked.  The Python bindings use it: a peripheral's
  exception stops the run there.
- Cost when absent (`make bench`'s `core/*`, pinned to one
  performance core, before and after run in turn, on a loaded host
  whose results vary by up to 2x between runs): `nop` +6%,
  `move_indirect` +5%, `dbra_self` +1%, `short_runs` +5% in the best
  paired runs, within the noise in others.  A first version that
  tested the special flag before the fast read cost 15 to 20% on
  every benchmark; testing it on the miss path removed that.  With
  hooks (`cpu::hooks::tests::hook_cost`, ignored, a measurement): a
  write hook on an unused page about +30%, a code hook on every
  instruction about twice the time.  Measure again on a quiet host.

### Exceptions

- Format C (bus and address error) frames follow CPU32RM 6.3 and
  6.4.3 word for word: the SSW, fault address, data buffer, current
  instruction PC and transfer count are documented, and handlers may
  complete a faulted write and clear RR, or clear MV, before `rte`.
  `rte` of a format C frame does all CPU32RM 6.3.2 describes: reruns a
  released write when RR is set, restarts a type II fault, continues
  MOVEM.
- The four fault types (CPU32RM 6.3.1): released write, faulted
  prefetch or operand (restarted), MOVEM (continued), and fault during
  exception processing (a second one is a double bus fault halt).
- Address error: odd instruction, word or long access (CPU32RM 6.2.3),
  same frame as bus error.
- Run loop order per Table 6-2: the instruction's own exception
  (group 2) or group 3, then trace, then an interrupt; each stacks
  over the last, so the interrupt handler runs first (6.1.5).
- Writes through `Core::write` are released (type I): a fault is
  recorded and taken when the instruction ends, the instruction
  complete.  TAS, MOVEP and MOVEM use `write_unreleased` (type II/III).
  Instructions return `Flow::Changed` for T0 tracing.  MOVEM
  reports progress with `movem_progress` and resumes from
  `movem_continuation`, which only the MOVEM at the type III frame's
  return PC takes (Where the manuals are silent).
- Firmware use: in all seven smartclock images every exception goes to
  one entry that records SR, PC, format/vector and fault address, then
  spins until the watchdog; none reads the SSW or does `rte` of a
  format C frame.  Other hardware may.
- `reset` asserts RESET for 512 clocks and resets external peripherals and
  internal modules other than the CPU (SIMRM 8.2.6); RSR SYS records it
  (SIMRM 8.2.8).  The host sees it.  The instruction tells the chip
  through `Bus::reset_instruction`.

### CPU space

FC 7 cycles: breakpoint acknowledge, the LPSTOP broadcast to $3FFFE
(the SIM answers), and interrupt acknowledge (CPU32RM 5.3.1;
MC68331UM 4.5.4).  `moves` can reach any FC through SFC/DFC; FCs 0, 3
and 4 are reserved and their behavior undefined (CPU32RM Table 5-1).  None
of these go through the RAM page tables.

### Bare CPU

A CPU32 with no chip around it, for function-level firmware tests
(smartclock's harness, off Unicorn) and hosts that are not a 68331
board: `mc683xx_cpu32::flat::Machine` in the core crate, no SIM, QSM or GPT,
and `mc683xx.Machine` in Python (see Python).

- **Memory:** 32 address lines by default (`flat::ADDRESS_BITS`), so
  nothing aliases and fault addresses are whole; fewer on request.
  RAM is mapped in whole 2-Kbyte pages (finer than Unicorn's 4K), the
  same in both spaces, never over a mapping; the two-level page tables
  (Memory) make a 32-bit space cost only what is mapped.  Each map is
  one region; unmapping frees its bytes, so a host that maps and
  unmaps again and again holds no more.  A map wholly unmapped frees
  its region, whose slot the next map reuses (lowest first, so the
  same maps give the same regions and snapshots); one trimmed at an
  end keeps the rest in its region; one split in the middle keeps its
  front and moves its back to a new region.  The pages kept keep
  their permissions.  The machine frees or trims only the regions its
  own maps made, each where it mapped it (it records them, outside
  the snapshot); a region the host added and mapped through the core's
  memory, or a page the host mapped from a map's region elsewhere, is
  only unmapped, its bytes left alone.  A host does not map a map's
  region elsewhere: unmapping the map frees or trims it under that
  alias.  A snapshot's regions must match the machine's
  (State), so one taken before an unmap does not restore after it, as
  one taken before a map never did.  Host reads and writes reach
  mapped bytes whatever their permissions, with no cycle.
- **Peripherals:** host objects at any byte range no peripheral shares; a
  cycle whose bytes one peripheral covers and no memory serves goes to
  it, anything else is a bus error.  CPU space (FC 7) reaches no
  peripheral: the machine acknowledges the LPSTOP broadcast itself, as the
  SIM would (MC68331UM 4.5.4.2), so `lpstop` stops; any other CPU
  space cycle is a bus error, so BKPT takes the illegal instruction
  exception (CPU32RM 6.2.5) and `moves` with FC 7 faults.  A long operand reaches a peripheral
  as one access of 4 (`Bus::read_long`, `write_long`, offered only
  when the host asks with `Cpu::set_long_bus`; the chip never does, as
  the CPU32's 16-bit bus splits it), and so do the read and write
  hooks, whatever served it (see Hooks).  A peripheral sees whether exception
  processing is stacking (`Bus::stacking`).
- **Interrupts:** a request line per level 1 to 7 that the host sets
  with a vector or the autovector, level-sensitive until it clears it.
- **Runs:** `Cpu::run_limited(limits)`: `until` (checked at each
  instruction boundary before an interrupt is taken or a hook called
  there), `instructions`, and an optional cycle budget; with none, a
  `stop` ends the run at once instead of waiting it out.  The limit
  check is a runtime test, not a const generic: a generic run loop,
  monomorphized twice, stopped `step` being inlined and cost 20% on
  `core/nop`.  `clear_resume` forgets a hook stop's skip, so a run
  from where a hook stopped calls it again.
- **Faults:** `set_stop_on_fault`: a bus or address error ends the
  run (`End::Fault`): fetch, read or write; bus error (unmapped or a
  peripheral's), protected, or address error; the whole address; the
  registers restored as the instruction (or interrupt entry) began,
  nothing stacked.  A fault while stacking (a bad SSP) stops the same
  way instead of a double bus fault, so the host fixes SSP and goes on
  with no reset.  Writes the instruction made before the fault stay in
  memory, and so do frame words stacked before a stacking fault.
- **Intercepts:** `set_intercepted(vector)`: an exception an
  instruction raises (traps, illegal, line A and F, privilege, zero
  divide, CHK, TRAPV, format error; not faults, trace or interrupts)
  ends the run before anything is stacked (`End::Intercepted`), its
  (An)+ and -(An) undone, PC at the instruction; flags it set (CHK's
  N) stay.  `take_natively_once` lets the instruction at PC take it
  natively when it runs again, which the host uses for an exception it
  declines: the instruction reruns, exactly, as its effects were
  undone.
- **Hook context:** memory and invalid access hooks see the
  instruction's address (`pc`) and whether the cycle stacks a frame
  (`stacking`); `Cpu::instruction_registers` holds the registers as
  the instruction began while such a hook is installed or faults stop
  the run.  Stacking writes are flagged, not hidden: a host that wants
  them hidden skips them.
- **Deviations, for parity runs only:** `Deviations { misaligned,
  untraced }`: odd word and long data accesses run as byte cycles
  rather than address errors (fetches stay strict), and T1/T0 trace
  nothing.  Neither is the CPU32's behavior; both are off by default.
- **Cost:** none of it on a run that does not ask, within the noise
  of the core benchmarks (best of interleaved runs on one pinned core,
  2026-10-23: `nop`, `dbra_self`, `move_indirect` -2 to +1%,
  `add_register` +2%, `move_postincrement` +8%).
- **State:** `Machine::save` adds the clock, the interrupt requests and
  each peripheral's snapshot (`MappedPeripheral: Save`), in the order added, to
  the core's (State).

### Where the manuals are silent

Our choices, not the CPU's.  Those marked provisional could only be
settled on a real 68331 over BDM, which is not available yet; until
then they stay our choices.

- **Microcode revision** (format C transfer count word, bits 15:8;
  CPU32RM 6.4.3): a named constant of ours, $C8.  `rte` checks it, as
  the manual says the CPU does, and takes a format error on a
  mismatch.  Its bit 3 falls on SR bit 11, always zero on the CPU32
  (Figure 2-3), so no SR reads as it: not the faulted frame's SR that
  sits at +$14 of a ten-word type IV frame.
- **Type IV SSW TP:MV** (provisional): 10, as CPU32RM 6.3.1 and 6.4.3
  say twice; Figure 6-8's 01 is taken as the misprint.
- **Type IV SSW RW** (provisional): set, as 6.3.1.4's pattern shows,
  though the faulted cycle is usually a stacking write.
- **Type IV frame placement** (provisional; 6.4.3's "SP - 6" is
  unclear): the format $C frame goes just below the space the faulted
  four- or six-word frame takes; `rte` rewrites that frame there.  The
  ten-word variant (four-word faulted frame) has its transfer count at
  +$10 and its SSW at +$12, where the twelve-word frame has its
  current instruction PC.  `rte` takes the ten-word layout when all of
  it fits, our revision at +$10, TP set and MV clear at +$12 and a
  faulted format $0 at +$0E; else the twelve-word one, our revision at
  +$14, with TP clear (types I to III), or TP set and a faulted format
  $2 (type IV).  So memory above a ten-word frame is never read: it is
  the faulted frame's SR, or, when stacking that frame faulted, stale,
  and an earlier twelve-word frame at the same depth leaves our
  revision and a TP-clear SSW there.  What stays ambiguous: a types I
  to III frame whose current instruction PC's high byte is $C8, with
  bit 15 of its low word set and bit 14 clear, and whose DBUF low word
  is below $1000, reads as a ten-word frame.
- **MOVEM continuation across an interrupt** (provisional): CPU32RM
  6.3.2.6 has `rte` restore MOVEM's state, refetch it and continue,
  and 6.3.1.3 stacks what that takes, but neither says what an
  interrupt taken at the boundary after `rte`, before MOVEM runs,
  does to that state.  Interrupt sampling is unchanged: one due is
  taken there.  The progress `rte` restored is kept with the frame's
  return PC and only the MOVEM at that PC continues from it, once its
  own words are fetched; it stays pending across any interrupt or
  exception until then, so a handler's own MOVEM never takes it.
  Reset or another type III `rte` drops it; software that abandons
  the faulted MOVEM and later runs that same MOVEM afresh would see
  it continue.
- **Type II and III return PC**: the faulted instruction's address,
  which `rte` refetches (6.3.2.3); the current instruction PC is the
  same, but for an odd PC, where it is the instruction before
  (6.2.3).
- **DBUF of a read fault**: zero.  **B1, B0**: always clear; no
  hardware breakpoint input yet.
- **T1:T0 = 11** (reserved, Table 6-3): traced as T1.
- **T0 and group 2**: an instruction raising TRAP, TRAPcc, CHK and the
  like counts as a change of flow.  BKPT ending in an illegal
  instruction exception is not traced, as group 3.
- **`rte` corner cases**: a rerun cycle that faults is taken as a
  released write fault when `rte` ends; with MV set, RR is ignored and
  MOVEM repeats the faulted transfer itself; a type IV frame's
  requeued trace stacks the faulted instruction PC, or for a
  four-word frame the return PC.  SIZ 11 with RR set, TP and MV both
  set, or a faulted format that does not fit, is a format error.
- **Level 7 after reset**: the edge detector starts at no request, so
  a request held at 7 is taken at the first boundary the mask allows
  by transition.
- **A bus error in `rte`** is charged "RTE (BERR on four-word
  frame)" (CPU32RM 8.3.14) whatever the frame, whose format may not
  have been read yet.
- **Stacking order**: a frame is written a word a cycle, highest
  address first, as TRAP's operation pushes.
- **`moves` to FC 0, 3 or 4** (provisional): the cycle goes to the
  `Bus` with that FC.  For the chip-select SPACE match, FC 0-3 count
  as user and 4-6 as supervisor, by FC2.  The page tables serve only
  FC 1, 2, 5 and 6.
- **MOVES second word, bits 10:0** (CPU32RM 4.4, MOVES, prints them
  zero): ignored, as Musashi's 68020 does.
- **`stop` with S clear in its word**: loaded as given, as STOP's
  operation reads; only LPSTOP's description makes it a privilege
  violation (CPU32RM 4.4, LPSTOP).
- **A breakpoint acknowledge that returns BKPT**: the BKPT returned
  runs another acknowledge with its own number, up to 16 in one
  instruction; then BKPT ends with PC at itself, an instruction
  boundary where a run can end or an interrupt be taken, and runs
  again from memory.  CPU32RM 6.2.5 executes the word returned, so a
  responder that always returns BKPT would keep the CPU in one
  instruction.
- **`bgnd`**: background mode is enabled only by BKPT asserted at
  reset (CPU32RM 7.2.1), which no host can do yet, so it is always the
  illegal instruction BGND's operation gives for that case.
- **LPSTOP and TBL share $F800** (CPU32RM 4.4): one handler, by the
  second word.  Until TBL is in, `instructions/system.rs` claims $F800
  and takes any second word but LPSTOP's $01C0 as line F (6.2.8); TBL's
  family then claims it and calls `system::lpstop` for $01C0.
- **Equal nonzero IARB values** (provisional; "unpredictable", SIMRM
  6.3): a fixed order, SIM, QSM, GPT, external, with a trace
  warning (`Event::ArbitrationTie`).  The PIT and the IRQ pins share
  the SIM's IARB; the PIT goes first by hardware (MC68331UM 4.7.3), so
  the order is PIT, QSM, GPT, IRQ pins.  At an external acknowledge a
  select's AVEC comes before the board's AVEC pin, then a vector.
- **Data movement and arithmetic corner cases** (CPU32RM 4.4 leaves
  their order unstated; Table 4-1 leaves no flag of these undefined):
  - `link a7` pushes A7 after its decrement, as LINK's operation
    lists the steps (SP - 4 -> SP; An -> (SP)); the 68000 pushes it
    before.  `unlk a7` leaves A7 the long pulled: the pull is read
    before SP + 4 -> SP and the load of An.
  - `movem` to `-(An)` with An in the list stores An less one operand
    size (CPU32RM, M68000PRM MOVEM).  With `(An)+` and An in the list,
    An ends past the last operand, the value loaded ignored: CPU32RM's
    "the value fetched plus the operand length" read as M68000PRM's
    explicit rule.  An is written when the transfers end, so a type
    III fault leaves it as it was and the continuation after `rte`
    ends it from the stacked operand address.  The extra read after
    a load (CPU32RM MOVEM note) is a word at the next operand; a fault
    in it continues from the last transfer.
  - `clr` writes without reading, as its timing (8.3.8, calculate
    effective address) implies.
  - `tas`'s read and write are one indivisible pair by construction:
    the core runs alone and no access comes between them.
- **Undefined condition codes** (CPU32RM 4.4; Table 4-1 "U"): left
  unchanged.  ABCD, SBCD, NBCD: N and V.  CHK: Z, V and C, and N when
  it does not trap; with Dn < 0 and Dn > the bound both, N is set.
  CHK2, CMP2: N and V.  DIVS, DIVU: N and Z after an overflow, and V
  after a divide by zero (C is cleared, as always).  The harness
  compares none of them (`differences::undefined_flags`).
- **BCD of a digit above 9** (undefined; operands are packed BCD): the
  low digit is adjusted past 9 or on its borrow; the high digit and
  ABCD's carry go by the binary sum passing $99, SBCD's high digit by
  the binary difference's borrow and its borrow by the low-adjusted
  difference's, which matches every SingleStepTests case (the
  68000's).  Musashi differs, so the BCD opcodes are not compared
  with it.
- **CHK2 and CMP2 compare unsigned**: Table 4-1's C, with its UB < LB
  term, makes the result the same for signed and unsigned bounds.
  Musashi differs on address registers, which are not compared with
  it.
- **MULS.L and MULU.L Dh:Dl with Dh = Dl** (undefined): the register
  holds the low half, as on Musashi's 68020.
- **Reserved bits of second words** (CPU32RM 4.4 formats): set in
  MUL.L, DIV.L, CHK2 or CMP2, an illegal instruction; in TBL (bits
  15, 9, 5:3), or a size of 11, or bit 8 not matching the form, line
  F, as "F-line (second word illegal)" (CPU32RM 8.3.13) implies.
  TBL's table form ignores bits 2:0, which "should be zero".
- **TBL rounding** (provisional): CPU32RM says it two ways.  TBLS's
  rounding table (4.4, TBLS) takes n <= -1/2 to -1 and n >= 1/2 to +1,
  which is half away from zero; 4.6.4 calls it "the TBLS
  round-to-nearest-even algorithm", which would take a half to the
  even integer, though its example rounds no half.  The core follows
  the table: TBLS rounds the adjusted difference half away from zero;
  TBLU's table has no negative n, and a falling table's difference
  is negative, so its fraction is taken as the two's complement low
  byte and a half rounds up.  N and Z of an unrounded result are the
  whole register's, as written; V only for a long's integer part.
- **Timing, where CPU32RM 8.3 is unclear**:
  - "X" on a long operand: two more cycles per "X", two more tail once
    per row (8.3: "usually add a value of two to the tail").  ADDX,
    SUBX and CMPM, unmarked, count each operand access as an "X".
  - Register-count shifts print head -2: read as cycles less two, as
    the #, Dm rows are; the table charges count 0 (head 4, 6 cycles).
  - A full extension word is charged its row's cycles against the
    brief format's, more or fewer, in the table the instruction takes:
    the handler names it (`AddressTable`; `calculated_address` for
    LEA, PEA, JMP, JSR, MOVEM, MOVES, CLR, Scc, TAS, MOVE from SR and
    CCR and TBL's table form, `timing::move_tables` for MOVE by the
    row its operands take, 8.3.3), else the fetch table (8.3.1).  The
    head and tail stay the brief form's.  (d16, Xm) and (d32, Xm), unlisted, take the (d16,
    An, Xm) and (d32, An, Xm) rows.
  - BKPT acknowledged: its row (14 clocks) is the acknowledge; the
    word returned is charged its own table entry beyond it, or, if it
    raises an exception, that exception as its own (an ILLEGAL
    returned costs ILLEGAL's row and the acknowledge, never "BKPT (Bus
    Error)", which is for an acknowledge that bus-errors).  Each
    further acknowledge, for a BKPT returned, is another BKPT row.
  - `lpstop` shares $F800 with TBL, whose handler dispatches on the
    second word; the table charges TBL Dym:Dyn, and `lpstop` takes the
    difference to its own row (8.3.13), the overlap with the last tail
    by its own head included.
  - DBcc loop mode (CPU32RM 1.1.2, MC68331UM 5.11, 8.3.11's starred
    rows).  The manuals say only: any single-word instruction that does
    not change the flow loops, under a DBcc of displacement -4; in
    loop mode only data cycles run, no fetches; the CPU leaves it on
    interrupts and other exceptions.  Modeled: a taken DBcc of
    displacement -4 enters loop mode when the instruction just before
    it was the word it branches to, completed with no exception and no
    change of flow (`Flow::Next`, T0's sense of one, writes of SR
    included) and PC past it.  The entering DBcc costs its ordinary
    taken row, having been fetched.  In loop mode the looped
    instruction and the DBcc charge no wait states for their words
    (still read, so hooks and memory see what they did); the looped
    instruction keeps its own row, for which the manual gives no loop
    mode time; the DBcc takes its loop mode row in place of its
    ordinary one, its head overlapping the last tail, its tail zero.
    Loop mode ends when the DBcc does not branch (the row of its
    outcome charged), on any exception (trace and interrupts too, so
    T1 or a taken DBcc under T0 never loops), and when the next
    instruction is not the loop's (the host moved PC).  The entering
    DBcc's tail is its taken row's, -2 so 0, not the condition-true
    row's 1 the table keeps elsewhere.  Not modeled: the
    three-word loop cache holding a word the loop overwrites.
  - Not listed: address error costs as bus error on instruction,
    format error as illegal.  `bgnd` takes the illegal instruction
    exception (7.2.2.2), ILLEGAL's row.  Divide by zero is charged on
    top of the divide.
  - Never charged, for the core cannot raise them: hardware breakpoint
    (no BKPT pin), and RTE's bus error on a six-word frame (RTE reads
    nothing of it past the format word, so a bus error comes before
    the format is known and costs the four-word row).
- **QSM and GPT on `reset`**: QSMRM and GPTRM are silent; both return
  to their reset state, as SIMRM 8.2.6's "internal modules other than
  the CPU" reads.
- **QSM** (QSMRM; MC68331UM 6, D.4):
  - QSMCR resets to $0080, as QSMRM 3.2.1's and MC68331UM D.4.1's
    figures give; D.4.1's text "STOP is set during reset" is not
    followed.  STOP and FRZ[1:0] read back but stop nothing: there is
    no BDM to assert FREEZE, and firmware stops the submodules first.
  - The QSPI RAM, undefined at power-up, starts at 0 and survives
    resets.  No wait states for CPU cycles to it while the QSPI runs
    (QSMRM 4.3.6 allows one to four).
  - SCI: NF is never set (no noise on a host's frame); the mark after
    a break (QSMRM 5.3) is not sent; a character in TDR is not sent
    once TE is clear; TC clears when the shifter takes a frame.  With
    SCBR 0 the receiver cannot sample, so frames on RXD are lost.
  - QSPI master: with SPBR below 2 the queue waits with SPE set.  SPE
    from 0 starts at NEWQP (QSMRM 4.4.1.1).  The peripheral shifts at
    the end of the serial transfer; SPIF and the end of the queue come
    after the delay after transfer (Figure 4-4).  A buffered SPCR2
    (4.3.3) takes effect at the end of an entry, after that entry's
    end-of-queue check; one that wrote NEWQP moves the queue there.
    MODF needs PCS0/SS given to the QSPI as an input, and clears SPE
    as 4.3.5 says, though 4.4 says only software does.  SCK, MOSI and
    MISO show PORTQS when read, not their serial levels.  A run syncs
    at an entry's end only when the CPU or a peripheral that is not
    deferrable could see it (Peripherals, QSPI); a randomized test
    holds the batched queue to the entry-by-entry one in every
    request, read, state and peripheral call.
  - QSPI slave (QSMRM 4.4.2; MC68331UM 6.3.5.3, Table 6-1): an entry's
    transmit word is read at its start, as Figure 4-5 has it, so
    transmit RAM written after that goes out at the next pass; the
    prefetch time between words (about 1 us, 4.4.2.1) is not modeled,
    and no bit is lost however fast the master clocks.  The bits go
    MSB first from the transmit word's low BITS bits, right-aligned as
    the master's are (4.3.6.2); 4.4.2.1's "16 bits" is read as BITS.
    The slave is selected only with PCS0/SS assigned to the QSPI and an
    input; it takes a bit only with SCK an input, and a 0 unless MOSI
    is the QSPI's input ("disables data input"); it drives MISO only
    when MISO is the QSPI's output, and not while halted or once the
    queue has stopped.  SPBR, DT, DSCK and the command RAM have no
    effect.  NEWQP written between words moves the queue at once, and
    mid-word at the word's end.  Changing MSTR with SPE set (which
    4.3 says may disrupt operation) takes effect at the next entry, or
    at once for a slave between words.  CPOL and CPHA choose edges,
    which the bit-level host interface has none of: a bit is one
    exchange.  FREEZE is not modeled (no BDM).
  - WOMQ: a QSPI pin (PQS0 to PQS6) that an output would drive high
    floats instead, to the board's level, whether the QSPI or PORTQS
    drives it (QSMRM 4.3.1); TXD is not affected.  WOMS does the same
    for TXD, whether the SCI's mark or PORTQS drives it (5.2.2, 5.3).
- **GPT** (GPTRM; MC68331UM 7, D.2):
  - A compare matches as TCNT counts to its value, so writing the
    value TCNT holds matches a period later; the reset value $FFFF
    matches too, setting the flags as TCNT passes it.  A run syncs at
    a match only when it interrupts or moves a pin.
  - OC1M's pins are the compare logic's whatever DDRGP; the compare
    output levels start at 0.  OC1M3 takes OC1's pin from CPROUT
    (GPTRM Table B-1).  GPTRM 5.1 gives the clock outputs' period and
    high time, one system clock, not their phase: our choice
    (decided 2026-10-23) is high in the system clock in which the tap
    advances its counter, so a compare's pin action comes with a rise.
    While STOP or STOPP holds the prescaler, the clock holds its
    level.  PPROUT's clock is on PWMA's pin, so `set_report_pwm_edges` for
    channel A reports it; it has no opt-in of its own (decided
    2026-10-23).
  - Input pins take no synchronizer delay or filter (GPTRM 2.2): an
    edge captures TCNT at the clock it comes in.  PCLK counts on its
    rising edges.
  - CPR's one write (GPTRM 5.1) is the first write to TMSK2's byte.
    STOP stops what STOPP stops; FREEZE is never asserted; test-mode
    writes are not modeled.
  - The PWM buffers start at 0, so the first period after reset is low.
- **SIM reset values where the manuals differ**: SIMCR $00CF
  (MC68331UM D.3.1) over SIMRM Table 8-4's FRZSW and FRZBM set;
  CSBARBT's 1 Mbyte (MC68331UM Table 4-24, SIMRM Table 7-8) over the
  512 Kbytes both texts also give.  SWP and PTP set when MODCLK is low
  (MC68331UM Tables 4-3, 4-5).  Port E and F latches, undefined at
  power-up, start at 0; the board's pins start high.
- **Clock synthesizer**: always locked (SLOCK set, SLIMP clear), no
  relock delay; a SYNCR write takes effect at once, the EXTAL period
  running finishing at the old rate, in time: EXTAL is the fixed
  reference (MC68331UM 4.3.2), so what is left of the period in old
  system clocks scales to new ones, rounded up to a whole clock.  The
  SIM moves to the new rate at the instruction's start, the board's
  time base at its end (Clock changes); both are fixed by the run, so
  the gap of one instruction is deterministic, and left.  Only the
  25-50 kHz reference range (SIMRM 4.4.1) or an external clock:
  `ClockSource::synthesizer` refuses a reference outside it
  (`ClockSourceError::ReferenceRange`).  The chip's clocks, its
  reference or external clock and so its system clock, are whole
  hertz: `ClockSource::synthesizer` and `external` refuse a fraction
  (`ClockSourceError::FractionalHertz`), so the snapshot's hertz
  restore them exactly.  Board clock nets keep nanohertz.
- **Watchdog and PIT phase**: a service restarts the time-out at the
  EXTAL period of its $AA write.  A new SWT waits for a service
  (MC68331UM 4.2.10); a new SWP does not, since 4.2.10 names SWT only
  and SWP switches the prescaler in front of the divider chain
  (Figure 4-3): the count left runs on at the new rate (its phase
  within the prescaler not modeled).  The Z3805A 3543B settles it:
  its boot code writes SYPCR $CC (SWP set, SWT 00) out of reset and
  sums the primary for about 0.1 s before any service, which the
  reset's 2^9 EXTAL periods (15.6 ms) unprescaled would cut off with
  a watchdog reset (`make boot-smoke`); prescaled they are 2^18, 8 s.  PITR from zero starts a
  count at the period it is written in; a zero stops it at once.  A
  PIT request is made only if PIRQL is nonzero when the count ends,
  and clears at its acknowledge.
- **A halted CPU** (provisional; the user's choice, 2026-10-24, to
  follow MC68331UM 4.2.10 and 4.2.12 until BDM): time passes while the
  CPU is halted.  `run_for` ends at the halt; a run that starts halted
  advances the SIM, the modules and the peripherals quantum by quantum to its
  budget without the CPU, and a reset the SIM asks for, the
  watchdog's (SWE set out of reset, SIMRM Table 8-4), clears the halt.
- **LPSTOP** (provisional; the user's choice, 2026-10-24, to follow
  MC68331UM 4.2.12 and 4.3.4 until BDM): from the broadcast cycle only
  SIMCLK runs, the PIT and the RESET and IRQ pin logic.  The GPT stops
  as STOP stops it (TCNT, the prescaler and the inputs held, a level
  the board drives seen at the wake); the QSM's SCI and QSPI make no
  progress, every time they wait for coming the stop's length later,
  frames sent on RXD pass unseen, and SCI frames already reported keep
  their times.  Only the PIT or an IRQ pin above the stored mask wakes
  the CPU, at its acknowledge; the other modules' requests wait for
  the wake, and a reset ends the stop too.  Everything runs on from
  where it stopped.
- **A cycle no DSACK ends** (provisional): a bus error even when BME
  is clear, where the chip would wait forever; with BME set, also one
  whose DSACK comes after BMT's period.  An address in the module
  space no module acknowledges goes to the external bus.
- **Port output changes** take the time of the write cycle that
  makes them, its start; a reset's take the reset's time.  The
  manuals give no pin timing below the bus cycle.
- **Wait states at an interrupt acknowledge** cost time as any bus
  cycle's do: `Interrupts::acknowledge` returns the cycle's clocks
  beyond the tables' two (`Acknowledged::stall`), which the core adds
  to the interrupt's estimate.  A peripheral's vector through a select
  costs its cycle's clocks.  An external acknowledge no DSACK ends in
  time is spurious after the bus monitor's period, BMT's clocks with
  BME set (SIMRM 3.2, 6.4.3), and at once with BME clear (see A cycle
  no DSACK ends).  An internal module's, a select's or the board's
  autovector, and the spurious interrupt monitor's BERR when nothing
  arbitrates (SIMRM 3.4, which gives it no time), cost nothing beyond
  the estimate (CPU32RM 8 gives the interrupt's time with a zero-wait
  acknowledge).
- **Undriven data lanes** read 0: a select asserted with nothing
  wired, or a pin with its other function whose select still ends the
  cycle (SIMRM 7.3).
- **ECLK cycles** (MODE set) are charged 1.5 ECLK periods, the
  synchronization's average.  Host memory on an externally terminated
  select answers with no wait states.
- **User-mode cycles to supervisor-only SIM registers** (provisional):
  not acknowledged, so they go on to the external bus, as QSMRM 3.3
  says of the QSM.  The factory test registers read 0 and ignore
  writes.

### Cycle estimates

Not cycle accurate, but not cycle stupid.  Firmware timing loops,
watchdog service, PIT and GPT intervals and peripheral pacing must come
out close to the real part's.

- Per instruction from the instruction timing tables (CPU32RM 8.3),
  not a flat rate per byte or per instruction: the listed cycles plus
  the effective-address cost (8.3.1, 8.3.2), with 2 more clocks for
  each long access where the tables say "X".
- Overlap per 8.1.6: each instruction is charged its cycles less
  min(previous tail, its head).  The core keeps one number, the last
  tail.
- Wait states per 8.1.5: each read, instruction fetch and write the
  table entry counts (r/p/w) adds its chip select's wait states, so
  slow peripherals cost what they cost on the board.
- Entries the tables mark "<" (maximum; data-dependent, such as
  multiply, divide and `tbl`) are charged their maximum.
- Negative tails count as 0, as on a two-clock bus (8.1.7); the
  slow-bus formula is not applied.
- Exceptions, interrupt entry, `stop`/`lpstop` and `rte` are charged
  from the same tables (8.3.13).
- The table is built from opcode decoding (`timing/decode.rs`), apart
  from the handlers.  What the opcode cannot tell (branch taken, DBcc
  outcome, register shift count, MOVEM list, MUL.L/DIV.L/MOVES/TBL
  second word, full extension words, traps, RTE frame format,
  LPSTOP on TBL's opcode) the handler adds with `Core::charge`, or
  takes back with `Core::refund` (a full extension word cheaper than
  the brief, LPSTOP), from `timing`'s functions; an adjustment changes
  cycles only, so the head and tail stay the base form's.  A refund
  comes off the instruction's cycles, never its wait states, and
  saturates at zero.  Exception processing charges
  `timing::exception`'s costs.  A test holds the decoder and the
  handler table to the same opcodes.

### Determinism

Same inputs, same run: no host time but what the host passes, no
host-dependent order, no threads in the core.  A paced run reads the
wall clock only to sleep between quanta, and a control handle's
inputs and stop reach the board at its sync points on its own thread
(Boards, Front ends).

## Deferred optimizations

Add only if measurements show the need.  The plain interpreter was
expected to run tens of times faster than a real 68331; measured, it
runs 2 to 10 times faster than one at 16.78 MHz (`docs/performance.md`,
`make bench`), so these now have numbers behind them, cheapest first:

- Done: handlers specialized by size and mode for the rest of the hot
  families (see Interpreter): `Dn, <ea>` and immediate arithmetic and
  logic, ADDA, the single-operand, shift and bit families, LEA and
  PEA.  Measured 1.1x (PEA) to 1.75x (EOR, ADDQ) fewer host
  instructions, at twice the handlers' code, 295 to 588 Kbytes
  (`docs/performance.md`).
- A cheaper quantum still: each module's next event is now cached
  (Peripherals, Time; `docs/performance.md`), and a one-clock run
  costs 1225 host instructions, down from 1736.  The largest part
  left is the GPT's advance, about 250: TCNT's catch-up, the fast
  edges' search and the PWM's sync run every quantum though nothing
  shows, but the GPT's state is the host's to read (`Mc68331::gpt`),
  so it cannot simply be left behind; then the diversions' check,
  about 50.
- The last of the per-instruction loop's overhead (see Interpreter;
  a NOP is now 157 host instructions, `docs/performance.md`): the
  interrupt sample's call through `dyn Interrupts` (about 5; a run
  generic over its `Interrupts` changes `Cpu::run_for`'s signature and
  copies the loop per host type), and handlers' `Result<Flow,
  Exception>` returned through memory (about 4 a NOP; `Exception` is 16
  bytes, so only an error kept in the CPU, with a one-byte result,
  would return in registers, a change to every handler).
- Predecoded block cache, with per-page invalidation on writes.
- Lazy condition codes.

## Verification

- Manuals first (`third_party/motorola/`): CPU32RM for the core,
  exceptions (section 6) and timing (section 8); MC68331UM for the
  chip; SIMRM, QSMRM and GPTRM for the modules.  Upstream Musashi has
  no CPU32 model and Unicorn's frames differ.
- smartclock firmware function corpus (`make corpus`, outside
  `make ci`; the images by path in `SMARTCLOCK_FIRMWARE`): ports of
  smartclock's `libc_check.py`, 111 checks of the Z3805A 3543B's C
  library with known answers, and of `function_check.py`'s calls, each
  C function of an image it can lay out (about 1300 an image, Z3805A
  3543B and 58503A 3704) on six generated cases.  smartclock compares
  the image's copy with gcc's and stores no outputs, so a function
  here passes when it returns keeping its convention (SP, the
  registers it keeps, but D7 for the kernel's I/O functions, which
  return in it; SR's system byte, but the interrupt mask
  `set_interrupt_mask` is given) or stops where smartclock's harness
  stops one: a trap it models or that never returns, a spin, a zero
  divide, a write to ROM.  A call smartclock's harness would skip,
  reaching beyond the memory and peripherals it maps, running data or
  jumping to an odd address, is counted apart, as is a word or long
  at an odd address, an address error on the CPU32 (CPU32RM 6.2.3)
  and a misaligned access on its 68020; so is an exception
  `functions::EXPLAINED` gives the reason for (`longjmp`'s generated
  jmp_buf sets T1).  Once no function of an image stops at an
  instruction the core lacks, any other exception or a broken
  convention fails `make corpus`.  The peripherals are smartclock's: zeroed memory at
  its chip selects and modules, its polled status registers ready at
  once (its first peripheral case).  Not yet: smartclock's kernel tables
  in RAM, its kernel models, its other peripheral cases, its pointer and
  task cases, the installer overlay.  The report counts
  functions by the first instruction the core lacks.
- The corpus against oracles (`make corpus-musashi`, `corpus-unicorn`,
  `corpus-oracles` for both; none in `make ci`; `oracles.rs`): each
  case the core ends at a return, a trap or spin it stops at, a zero
  divide or a bus error (a write to ROM, an access beyond the map) is
  run again on each oracle from the same registers, memory and
  peripherals, and passes only if the oracle ends it the same way and, for
  a return, trap or spin, leaves the same registers, SR (but the
  68020's M bit and condition codes an instruction of the case left
  undefined) and memory and peripheral bytes.  A case that runs an
  instruction `differences.rs` says the 68020 runs otherwise is not
  comparable, as is one the core does not end so.  A function
  mismatches at its first mismatched case; a mismatch every oracle
  agrees on against the core is listed first as a probable core bug.
  Reported, not asserted.  Musashi runs a call itself, its exceptions
  its own, on `RegionMemory`: the image shared read-only between
  cases, the rest a copy per case.  Unicorn, one engine per image,
  works around its known faults inside the oracle
  (`cpu32-oracle/src/unicorn.rs`; the faults themselves stay
  regression tests of the core, `unicorn_faults.rs`): SR read between
  runs through a saved context and a CCR probe; hooks that only stop
  the run; traps, `rte`, `rtr` and `move (sp)+,sr` done by the oracle;
  `stop` and `movec` of VBR not comparable.  Unicorn has 32 address
  lines where the 68331 has 24 that its memory repeats above (MC68331UM
  4.4.1.1): the first access above them maps that alias, a read-only
  region's as a copy, a writable region's or peripheral's as a peripheral
  forwarding to it; code fetched from the latter is not comparable.
  Unicorn does not retranslate code after a write to it, so code run
  from writable memory is not comparable, and each image gets a new
  engine.  Musashi is given the 24 lines.  Measured 2026-10-23: no
  mismatch on either oracle; Musashi matches all but 2 Z3805A and 3
  58503A functions, Unicorn all but 14 and 17, mostly cases the core
  skips.  A build with the oracle features off, from an empty Cargo
  home, downloads no Unicorn crate.
- Boot smoke test (`make boot-smoke`, outside `make ci`; the images
  by path in `SMARTCLOCK_FIRMWARE`): the Z3805A 3543B and 58503A 3704
  booted from power-on reset on `mc683xx-mc68331` for a few emulated seconds,
  wired as the Z3805A's chip selects are (smartclock
  `docs/firmware/restart.md`): the flash on CSBOOT and CS1 (writes on
  CS6 and CS7 to a stub), the RAM on CS0 with CS2 and CS3, every other
  select on one recording stub that reads back what was written, but
  the DUART's SRA and SRB, ready.  No peripheral's logic: that is the
  smartclock port's.  It traces the selects' programming, the first
  cycle on each stubbed select, the exceptions, the resets, the
  milestones from each image's `names.tsv` and where it ends, and
  fails nothing on its own; it is a probe for core and chip faults.
  Both images (2026-10-23) checksum the primary, move VBR to $10000,
  start pSOS (`kernel_init`), init the DUART, run `root_task` with
  the PIT ticking at level 5, and settle in its idle loop in user
  state, its tasks presumably waiting on peripherals the stubs do not
  model (the DUART's interrupt, the FPGA's DONE, the QSPI's
  peripherals); no self-test, console or SCPI output yet.  No
  bus or address error, illegal or unimplemented instruction, or
  watchdog reset after the SWP fix (Where the manuals are silent).
- Boot compared with smartclock-emu (`make boot-diff`, outside `make
  ci`; the trace by path in `BOOT_DIFF_REFERENCE`, whose header names
  the image and the EEPROM dump): a whole image on the boot smoke
  test's wiring, its peripheral and module cycles and exceptions compared
  with a trace of smartclock-emu's Unicorn backend booting it.
  Unicorn's time is block estimates, so the comparison drops
  interrupts and their handlers' traps, collapses polls, and aligns
  the rest greedily, sorting each divergence as moved (the same
  records nearby on the other side), modules (the modules' registers
  alone) or board; then the board peripherals alone, and the writes
  address by address.  Run once with stubs, then replaying: each
  board peripheral read answered with the reference's read there, in
  order; the EEPROM an AT28C64B (tBLC, tWC, DATA polling, toggle bit)
  from the dump; each QSPI transfer the word the reference read from
  its receive RAM entry in its run of the same first command; port E,
  F and GP's inputs and PACTL's PAI and PCLK driven as the reference
  read them; the DUART's level 2 interrupt requested, and IC1's edge
  made (the FPGA's 1 PPS), once the chip has made the board cycle the
  reference made before it.  The run ends as long past the port's
  last byte as the reference's did.  Unicorn reads a long at 2 mod 4
  as two aligned longs, whose extra words are passed over.  Measured
  2026-10-23, replaying: the Z3805A 3543B writes TBB as the
  reference, `scpi > ` at 1.1 s of the chip's time (0.68 s of
  Unicorn's); the 58503A 3704 as the reference too, through its
  self-tests, the script's `*IDN?` and the answer
  `HEWLETT-PACKARD,58503A,3710A01056,3704-C`; every board peripheral
  write the same, address by address, in value and count, but three
  of the 58503A's latch writes in bit 6 (the front panel's framing).
  That is timing: the latch is one shadow byte (`0x136c0`, `0x13702`,
  `0x1373c`) that `fpnl` writes bit 6 of around each panel command and
  `hmon_task` writes the lamps of, its first pass and then every 51st,
  each 10 PIT ticks (8355840 board cycles apart on both sides), with
  bit 6 as the shadow has it.  The first lands inside the reference's
  first `fpnl` frame, open 17.2 ms there with `log_page_write`'s EEPROM
  write and its 5.2 ms poll inside it; on the chip that page write
  comes after the frame (closed in 4.9 ms), and so does hmon's write,
  with bit 6 high; two later ones land inside the
  chip's key polls, its frames' phase against hmon's drifted.  The
  front panel's `display` records, formed from the latch and the QSPI
  transfers to PCS code 0 as smartclock's `front_panel.rs` forms them
  (ported), are the reference's: all 28, in order.  The divergences
  left are timing: the EEPROM's polls, the EFC strobe's,
  the latch's and the FPGA port's periodic writes, the QSPI runs'
  order (front panel, ADC, FPGA), the QSPI, PIT and DUART handlers'
  places, and the chip's slower boot.  The one value that differs is
  TFLG1 and TFLG2: TOF and I4/O5F set on the chip, as GPTRM 7.1.2.1
  has them, where smartclock models neither the overflow nor OC5.  No
  chip or core fault found, and no Unicorn fault identified.
- SingleStepTests `m68000` (MIT; generated from MAME's 68000 core,
  whose bugs it inherits) for the 68000 subset, excluding exception
  cases.  Not `680x0`: it carries no license.
- Differential runs per instruction against Musashi (MIT since 2013;
  68020 mode) and Unicorn.  A case where Unicorn is known wrong is
  left out of the comparison only; the fault itself is a regression
  test (`crates/cpu32/tests/unicorn_faults.rs`), never excluded.
- Harness (`cpu32-harness`, `cpu32-oracle`; test-only, unpublished):
  `make singlestep` and `make oracle` fetch the data and Musashi at
  pinned commits into the ignored `external/` and run them; neither is
  in `make ci`, which needs no network.  CI runs `make oracle` as its
  own job.  Both run whatever the core
  implements and count the rest as unimplemented.  Cycle counts are
  not compared.  Known differences are excluded with their reasons in
  `differences.rs`: the 68000's frames and `rte`, `move from sr` in
  user state, the 68020's misaligned operands, CPU32-only opcodes,
  the 68000's 8-bit branch displacement $FF (the CPU32's 32-bit
  form), the 68020's M bit, `link a7` (68000), `movem` storing its
  own `-(An)` (68000 and Musashi), `bkpt` (Musashi takes it as
  illegal), and against Musashi the BCD instructions, CHK2 and CMP2
  of an address register, and a divide overflow, after which Musashi
  keeps C.  Undefined condition codes are compared on neither side.
  SingleStepTests end before the trace
  their T bit calls for, so the runner runs a case with T1 clear and
  puts it back, unless the instruction replaced SR; one that reads SR
  with T1 set is excluded.  The 68000 has no T0, so the runner drops
  it, and its STOP prefetches nothing past the instruction.
  The 68000 ignores a brief extension word's SCALE and bit 8
  (M68000PRM 2.4); SingleStepTests sets them at random, so the runner
  clears them first, as the 68000 read them.  Until exception
  processing lands, a case where the core raises one is compared only
  as "raised", not by its frame.
- Not MAME's CPU32 (`fscpu32_peripheral`, its `M68340`'s core), tried
  and rejected (2026-10-23) as an oracle for CPU32-only behavior.  It
  builds outside MAME: MAME 0.285 (3bd358f7), a 66 MB sparse fetch of
  `src/peripherals/cpu/m68000/` and the SoftFloat 3 headers, compiles in
  about 15 s against a 500-line stand-in for `emu.h` of our own (flat
  bus, register access through the debugger's state entries, bus
  error through `set_buserror_details` and the bus error line), with
  none of MAME's framework, FPU or SoftFloat compiled.  The files are
  BSD-3-Clause, except the generated `m68kops.cpp`/`m68kops.h` and
  their source `m68k_in.lst`, which carry no header and so fall under
  MAME's GPL-2.0.  But it is Musashi's 68020 core again (the same
  lineage as the oracle above, so its errors are Musashi's) with a
  CPU32 cycle table, and has none of what Musashi cannot check.
  Probed as a black box: `tbl*` and `lpstop` take line F, vector 11
  (CPU32RM 4.4, TBLU and LPSTOP); a bus error stacks the 68020's
  format $B frame, not the CPU32's $C (CPU32RM 6.4.3); RTE of a
  format $C frame takes a format error (CPU32RM 6.2.12); `movec`
  takes CACR, MSP and ISP, which the CPU32 rejects as illegal
  (CPU32RM 4.4, MOVEC).  Only `bgnd` and `bkpt` without a debugger
  (illegal instruction, CPU32RM 6.2.5, 6.2.8) and privilege
  violations agree, and Musashi checks the last already.  Every
  difference on the target list would be MAME's, so it would add a
  GPL build for no check.  What checks CPU32-only behavior instead:
  CPU32RM itself (section 6.4's frames and section 4's TBL examples
  as unit tests), the firmware corpus, and a real 68331 over BDM.
  Moira and UAE model the 68000-68060, not the CPU32; whether
  Unicorn's QEMU models TBL or LPSTOP is not checked.
- Unicorn's known faults (smartclock's `unicorn_check.py`, its
  harness's and emulator's workarounds) as regression tests,
  `crates/cpu32/tests/unicorn_faults.rs`: SR reads after a run and in
  a trap, `rtr`, `move (sp)+,sr`, `stop`, traps and `rte`, VBR.  The
  code hook faults (a PC write, an SR read or write part way through
  a block, an SR write before a bus error) run through a real code
  hook at the instruction, which reads in the hook or stops the run
  for the host to write, and again between two runs.  Beside them,
  hooks are checked to see the state the manual gives at the boundary:
  the codes the instruction before left, SR's system byte, A7 and the
  stack's top following S, and a handler's SR after a trap or an
  interrupt (CPU32RM 6.1.3).
- Peripheral interface: its own tests from Phase 1.  No oracle models
  the peripherals; smartclock's peripheral models and firmware serial
  transcripts are the nearest.
- Boot of the Z3805A 3543B firmware to the same serial transcript:
  the core and the peripheral interface together.
- A real 68331 over BDM, once a pod is available: the only check of
  exception frames and undefined flags against silicon.

## Phases

| Phase | Work |
| --- | --- |
| 1 | Interpreter, function codes, cycle estimates; bus, peripheral and next-event interface |
| 2 | Exceptions, interrupts, trace |
| 3 | Hooks, memory protection, save/restore |
| 4 | SIM, QSM, GPT, board time, ported from smartclock and generalized |
| 5 | Python bindings |
| 6 | Verification to the Z3805A transcript |

Phase 3 is in: hooks, page permissions, tracing, save and restore.
The QSM and GPT save their registers and timing (the QSM not its
host's QSPI peripheral, which a restore keeps).  Left: the Python
bindings bind hooks and state; the Unicorn fault tests' code hook
checks move onto real code hooks.

## Decisions

| Decision | Why |
| --- | --- |
| `MIT OR Apache-2.0` | Lets hosts ship it; the Rust default. |
| Peripheral interface equal to the core | The first user is a board emulator; firmware runs only with its peripherals wired right. |
| No QEMU/Unicorn code | GPL; used only as a feature-gated test oracle. |
| Unicorn as a corpus oracle, through the `unicorn-engine` crate | GPL-2.0, so a black box only: its API, never its source; the `unicorn` feature of the unpublished `cpu32-oracle` and `cpu32-harness`, off by default, outside `make ci`; test binaries never distributed, never linked into the shipped crates.  Its known faults are worked around inside the oracle, where its answer can then be trusted, and a case they leave untrustworthy is not comparable; the core's tests keep them as regressions. |
| Interface is the chip's; smartclock necessary, not sufficient | Other 68331 hardware should fit; implementation can wait for a user, the interface cannot foreclose it. |
| SIM and QSM shared in `mc683xx-modules` | A 68332 reuses both. |
| Port smartclock's SIM, QSM, GPT and time | Generalized past smartclock's simplifications (fixed clock, unenforced chip selects). |
| Manuals kept in the repo, in Git LFS | The specification, beside the code that cites it.  NXP no longer serves them; its terms of use allow reproduction for personal non-commercial use only, a risk accepted for the public repo. |
| Firmware images by path, not committed | Vendor images are not ours; feature-gated system tests read them from a path in an environment variable, for any board. |
| Runs named `run_until` and `run_for` | A run to an absolute time is `run_until`, a run for a duration `run_for`, at every layer (the core, the chip, the board, gdb's targets, Python); a bare machine's run to limits is `run_limited`.  No bare `run`, whose meaning a reader must look up (decided 2026-10-24, before 0.1.0). |
| The modules crate's assembly API sealed and hidden | `Module` and the plumbing only `mc683xx-mc68331` uses to compose the chip (register cycles, decode requests, the SIM's timer and reset handshakes, the serde helpers) change with the chip; sealing `Module` and `#[doc(hidden)]` with a "Not part of the semver API" note keep them out of semver, so the chip can change them in a minor release, while what a host needs stays public (decided 2026-10-24, before 0.1.0). |
| Board time with the SIM | It derives from SYNCR. |
| Not cycle accurate, not cycle stupid | Firmware timing comes from peripherals, but timing loops and peripheral pacing need table-based estimates with wait states. |
| Level-sensitive IPL plus IACK | Matches the chip; makes IARB natural. |
| Two page tables, user and supervisor | Chip-select SPACE tells only user, supervisor and CPU space apart; CPU space goes to the `Bus`. |
| Full format C `rte` | Other hardware's firmware may complete faults; smartclock's never does. |
| No stale prefetch | The manual is silent; simplest, and code-page writes are rare. |
| Peripherals wired to chip-select pins; the SIM decodes | Follows the hardware: the wiring is static, the map is the firmware's.  Z3815A and 58503B boot code remaps. |
| Musashi and SingleStepTests fetched at pinned commits, not vendored | Keeps third-party code (and Musashi's non-MIT SoftFloat) out of the tree; the oracle is optional and test-only, so a one-time fetch costs little. |
| Musashi's bundled SoftFloat 2b accepted, unmodified, in test-only builds | Its license (use at your own risk, indemnify the author; notice on derivative works) binds distribution, and the oracle is never distributed: fetched, not in the tree, `publish = false`, never linked into the shipped crates.  Stubbing it out would mean patching Musashi's build. |
| Block cache and lazy flags deferred | Simpler core; the per-instruction overhead measured first (`docs/performance.md`) is cheaper to cut directly. |
| Benchmarks with criterion in the `benches/` workspace member | Statistics and saved baselines for before and after; its shared setup is tested by `make ci`, which only compiles the benchmarks. |
| Python bindings behind a crate feature | The workspace forbids default-members; a `python` feature keeps PyO3, and a Python interpreter, out of `make ci`. |
| Python mirrors the Rust types it wraps: `Run`, `LimitedRun` and `BoardRun`, `Stop` and `End` a class per variant, `Machine`, `HookId`, plain enums as enum classes | One name per item across both APIs (AGENTS.md); a host porting between them translates nothing.  A Rust type with `FromStr` crosses as its string; `End::Intercepted` alone has no Python variant, as the run answers it (decided 2026-10-24, before 0.1.0). |
| A peripheral's Python exception stops the run after its instruction and is raised when `run_for` returns | The `MappedPeripheral` trait can report only BERR; the chip's `StopRequest` ends the run there, and the exception cannot cross it. |
| A bare `Machine`'s hooks may write the CPU; their callables run between core runs | Unicorn's harness stubs routines and peripherals from hooks; queuing in the core and calling Python with nothing borrowed needs no `unsafe` and costs only hooks that fire. |
| A Python hook stops the run on a truthy result; its exception stops it as `Break` and is raised when `run_for` returns | A breakpoint is `return True`; a hook returns `Control`, so it can stop where it is, with no stop request left for the next run. |
| Hook stops before an instruction skip that instruction's hooks once on resume | A breakpoint must stop before the instruction yet let the next run past it. |
| A snapshot taken at a hook stop keeps that hook's skip on restore | Restore resumes exactly where the run stopped; no block hook fires again at that instruction (decided 2026-10-23). |
| GPT clock outputs high in the system clock the tap advances its counter in | GPTRM 5.1 gives no phase; ours lines a compare's pin action up with a rise (decided 2026-10-23). |
| PPROUT's edges share PWMA's `set_report_pwm_edges` opt-in | Same pin; one opt-in per pin (decided 2026-10-23). |
| Permissions and hook bits beside the page entries, not in them | The chip rebuilds the entries from the decode; the host's protection and hooks must outlive that. |
| Snapshots as a version then postcard bytes, one version per layer | serde derives keep the state types the code's own; postcard is small and deterministic; the core, the chip and each module change apart. |
| A golden file per snapshot format, keyed by the versions in it | Two branches changing one format each bumped its version apart and merged cleanly; a golden they both regenerate conflicts.  A hash, not the bytes, keeps it small; nested versions in the key catch an inner bump that left an outer version alone: the outer layer's bytes changed, so it bumps too, and a reader of the outer version alone knows the format changed. |
| `Module::save` and `restore` required, with no default; a module that cannot save makes the chip's fail | A snapshot missing a module's state would not reproduce the run; failing says so, and a new module cannot forget it silently. |
| A peripheral's `Save::save` and `restore` both required: both, `Stateless` or `unsavable!` | A default for each would let a peripheral give `save` alone, saving snapshots its own `restore` refuses, which a rollback cannot put back.  A macro, not an `Unsavable` marker trait: a second blanket impl of `Save` beside `Stateless`'s conflicts (decided 2026-10-24). |
| `Module::peek` required, beside `read`, for the host's peeks | A debugger's look must not clear a flag; a module that cannot answer says `Unpeekable` rather than letting a peek lie or cycle. |
| Host bus cycles take no chip time and drop a peripheral's stop request | They are a debugger's, between runs: no instruction to charge, no run to stop. |
| A bus stop request (`Bus::stop_requested`) beside hooks | A peripheral has no hook of its own; one question per bus cycle costs nothing on RAM. |
| Board files in TOML, peripheral types from a registry | The file says what is wired where and how the chip is configured, never what a peripheral does; behavior stays the host's code.  A registry of names leaves room for plugins without a format change.  No dynamic loading yet (decided 2026-10-23). |
| A bare CPU (`mc683xx_cpu32::flat`, `mc683xx.Machine`) beside the chip, 32 address lines by default | Function-level firmware tests map code and stubs at addresses no 68331 decodes (smartclock's harness uses $31400000); a chip-select decode would only get in the way. |
| Bare-host stops (faults, intercepts) and parity deviations opt-in, off by default | The default stays the CPU32's behavior; a host porting from Unicorn asks for what it relied on, by name. |
| Published to crates.io and PyPI, one semver version for the workspace from 0.1.0, notes in GitHub releases | Decided 2026-10-24, with the repository public.  One version keeps the crates and the wheel in step; a release is tagged only on a commit that has passed every check (`docs/releasing.md`). |
| abi3 wheels and the crates published by the `release` workflow, both by trusted publishing | One wheel per platform covers every Python from 3.10; no token is stored.  crates.io's trusted publishing needs the crate to exist first, so only the first release is published by hand (decided 2026-10-24). |
| A GDB stub on the gdbstub crate, generic over a small `Target` trait, its breakpoints and steps hooks | Decided 2026-10-24; gdbstub is MIT OR Apache-2.0.  Hooks cost nothing until installed and never touch memory; one trait serves the bare CPU, the chip, a board and hosts with run loops of their own. |
| gdb's memory access without bus cycles | A debugger's look must not clear a flag or feed a peripheral (the reason `Module::peek` exists); real cycles stay the host's (`bus_read`, `bus_write`). |
| The public API names no foreign type but gdbstub's `ConnectionExt` | A dependency's major release must not break ours: board parameters are `registry::ParameterValue`, a syntax error a `SyntaxError`, a snapshot that does not decode a `DecodeError`, each opaque or ours.  `stub::serve` keeps gdbstub's connection trait, which an own trait would only forward to (decided 2026-10-24). |
| `Cpu::registers` and `Cpu::memory` stay public fields | The programming model is the host's to read and write as it likes (a test's setup, a harness's comparison); no invariant spans them that a setter would guard, and hosts reach them a thousand times as fields (decided 2026-10-24). |
| A fixed memory on no select, emulator-only; a matching select wins | A host's hybrid boot maps rewritten code beside an image it must not change, where the only alternative was patching the reset code to program a select the firmware leaves unused.  A select winning keeps the firmware's decode as on hardware wherever it decodes; fast termination and no DSACK by default, as the chip has nothing to model, a select's internal timing (port, wait states) on request so code runs at its original bus speed; contents survive resets as RAM's unless a reload scope says (decided 2026-10-24). |
| Pacing host-side, a front end's control taken at sync points | A GUI wants board time at wall speed and a stop and keys from its own thread; sleeping between quanta against the wall clock, outside the board's model, and taking a thread-safe handle's stop and inputs at sync points on the board's thread, keeps the core single-threaded, host time out of the model and the run the same whatever the pacing (decided 2026-10-24). |
| Front ends outside the repository and the board file; they observe nets and inspect peripherals | Board files describe hardware, which a UI is not; a front end's needs are the host API's: read-only observers on nets, peripherals' state by type, inputs through externals.  Read-only keeps every run deterministic whatever watches it (decided 2026-10-24). |
| Board nets with their own picosecond time | Peripheral-to-peripheral edges need sub-cycle time (an FPGA interpolator resolves about 0.5 ns against 59.6 ns cycles); integer time keeps it deterministic, and only chip inputs round to a clock (decided 2026-10-23). |
| `#[non_exhaustive]` on the outcome enums, the structs hosts read, and the configuration structs | A field or variant added later is not a breaking change.  Configuration is `X::default()` and set fields; a struct a host must build has a constructor.  Enums the hardware fixes (sizes, selects, ports, channels) stay exhaustive, so a match on one needs no wildcard; the bindings' matches on the others end in a panic naming what they do not know (decided 2026-10-24). |

## Open

- Booting real images: smartclock-emu moves onto `mc683xx-mc68331`,
  keeping its Unicorn emulator as the differential oracle until the
  Z3805A 3543B and 58503A 3704 transcripts match (decided 2026-10-23).
  API gaps it finds come back here.
- A real 68331 over BDM: no BDM pod is available yet (2026-10-23).
  Until then the provisional behaviors stay provisional: settled by
  the manuals, the oracles and
  the firmware corpus where they can be, and otherwise recorded as our
  choices.
- Whether `smartclock-emu` moves off its feature gate once it uses this
  core.
- After 0.1.0, as additive API (the user's choice, 2026-10-24):
  - a bus peripheral on no chip select: external decode with its own
    DSACK and IACK vector, for a board whose logic decodes addresses
    the selects do not;
  - DSACK port size per cycle, not per peripheral (`Builder::dsack`): a
    peripheral that answers some cycles on DSACK0 and others on DSACK1.
