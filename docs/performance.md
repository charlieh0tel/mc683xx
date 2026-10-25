# Performance

How fast the emulator runs, how to measure it, and where the time
goes.  AGENTS.md: performance matters, but not cycle accuracy; measure
before and after any optimization.

## Reproducing

```
make bench                                  # everything, a few minutes
make bench BENCH_ARGS="--save-baseline before"
make bench BENCH_ARGS="--baseline before"   # after a change: the difference
cargo bench -p mc683xx-bench --bench core -- 'core/nop$'   # one benchmark
```

The benchmarks are [criterion](https://crates.io/crates/criterion)
0.8, without its plotting and rayon features, in the workspace member
`benches/` (package `mc683xx-bench`, not published).  `make ci` does
not run them; its clippy compiles them, and its tests run the shared
setup's own tests (`benches/src/lib.rs`), so a kernel that stops
working fails `make ci`.

## What is measured

- `core/*` (`benches/core.rs`): the core alone, on 256 Kbytes of
  zero-wait RAM, nothing on the bus, no interrupts.  Each kernel
  (`benches/src/kernel.rs`) is a few instructions repeated end to end
  into a sled; an iteration runs the sled from its start for 20,000
  clocks.  A kernel the core does not have yet takes an illegal
  instruction or line A/F exception in calibration and is skipped, so
  the list names instructions before they land.
- `chip/*` (`benches/chip.rs`): the MC68331 with the Z3805A's map
  (`benches/src/board.rs`): boot code programs its chip selects, stops
  the watchdog, then runs a sled from the boot flash on CSBOOT (one
  wait state).  `flash_nop` fetches only; `ram_move` reads RAM through
  the page tables (`move.w (abs).l,d0`); `peripheral_move` reads a peripheral
  on CS5 (8-bit, 12 wait states) through the `Bus` and the chip-select
  decode.  `flash_nop_pit_masked` runs the PIT at its shortest period
  with interrupts masked, so the run syncs every 1024 clocks;
  `flash_nop_pit_interrupting` also takes each PIT interrupt, whose
  handler is `rte`.  `flash_nop_qspi_words` and `flash_nop_qspi_bytes`
  run a 16-entry QSPI queue in wraparound beside the sled, to a
  peripheral that echoes, at the fastest SCK and the shortest delays
  the 58503A's firmware uses (SPBR 2, DSCKL 1, the standard delay
  after transfer, CONT): 16-bit transfers, 83 clocks an entry, or
  8-bit, 51.  `flash_nop_qspi_bytes_deferred` runs the 8-bit queue to
  a peripheral whose calls may be deferred (`SpiPeripheral::deferrable`).
- `spi/*` (`benches/spi.rs`): one 16-bit QSPI transfer to a 36-stage
  shift register on the falling edge, an MC144110's, walked edge by
  edge (`mc683xx_board::spi::exchange`) in modes 2 and 3, against the
  same register shifted as a word (`word`).  Throughput is transfers.
  On the machine below: 40-50 ns a walked transfer, under 1 ns a
  word.  A 16-bit transfer at the fastest SCK takes 64 system
  clocks, so a peripheral that walks its bits costs well under the chip's
  time for them; a peripheral that does not care about edges answers with
  the word.  The `chip/*qspi*` benchmarks do not go through a board's
  peripherals and are unchanged by it.
- `peripherals/*` (`benches/peripherals.rs`): the board layer (`mc683xx-board`),
  a board file's peripheral on CS1 that the firmware writes in a loop, a
  word a write, from the reset clock, an iteration 1 ms of board time.
  `registers_writes` writes a `registers` peripheral, which drives nothing;
  `latch_writes` a 74x374 whose Q0 is on PQS0 and changes at each
  write.
- `*/short_runs`: 1000 runs of one clock each, the cost of a run call:
  `Cpu::run_for` alone, and `Mc68331::run_for` with its syncing.
- `tables/*` (`benches/tables.rs`): building the 64K-entry instruction
  and timing tables.  The core builds each once per process, so each
  sample is a fresh process timing the first run that builds it.

Throughput is instructions per second (criterion's "elem/s"), counted
in a calibration run one instruction at a time; interrupt entries
count as instructions.  Each benchmark prints its calibration: the
instructions and emulated clocks an iteration runs.  The emulated
clock rate is the clocks per iteration over the time per iteration,
and the speed relative to a real 68331 that rate over 16.777216 MHz
(the 68331's usual clock from a 32.768 kHz crystal) or 25 MHz (its
fastest part).

## Baseline

The plain interpreter, with the QSM, the GPT and most instructions,
without any of the optimizations below: the reference they are
measured against.  Machine: Intel Core i9-13900H (6 performance and 8
efficiency cores, 20 threads), 64 GB, Linux 7.0.0, rustc 1.98.1, the
default release profile (no LTO, 16 codegen units), under a load
average of about 30 from other builds on the same machine, so the numbers are pessimistic and
noisy: runs of the same code differed by up to 10% (`chip/*`) and 2x
(single kernels, at worst).  Treat them as a floor and an order of
magnitude; compare before and after on one machine, interleaved.

| Benchmark | Clocks per instruction | ns per instruction | MIPS | Emulated MHz | vs 16.78 MHz | vs 25 MHz |
| --- | --- | --- | --- | --- | --- | --- |
| `core/nop` | 2.00 | 24.6 | 40.7 | 81 | 4.9x | 3.3x |
| `core/moveq` | 2.00 | 24.8 | 40.4 | 81 | 4.8x | 3.2x |
| `core/move_register` | 2.00 | 47.0 | 21.3 | 43 | 2.5x | 1.7x |
| `core/move_indirect` | 4.00 | 60.4 | 16.6 | 66 | 3.9x | 2.6x |
| `core/move_postincrement` | 10.00 | 92.2 | 10.8 | 109 | 6.5x | 4.3x |
| `core/move_displacement` | 7.00 | 72.4 | 13.8 | 97 | 5.8x | 3.9x |
| `core/move_indexed` | 10.00 | 77.1 | 13.0 | 130 | 7.7x | 5.2x |
| `core/move_immediate_absolute` | 10.00 | 89.5 | 11.2 | 112 | 6.7x | 4.5x |
| `core/add_register` | 2.00 | 48.3 | 20.7 | 41 | 2.5x | 1.7x |
| `core/addq` | 2.00 | 48.5 | 20.6 | 41 | 2.5x | 1.6x |
| `core/cmp_register` | 2.00 | 44.1 | 22.7 | 45 | 2.7x | 1.8x |
| `core/tst` | 2.00 | 36.6 | 27.3 | 55 | 3.3x | 2.2x |
| `core/lsl_immediate` | 6.00 | 44.2 | 22.6 | 136 | 8.1x | 5.4x |
| `core/mulu` | 26.00 | 36.7 | 27.3 | 709 | 42.3x | 28.4x |
| `core/lea_displacement` | 5.00 | 44.9 | 22.3 | 111 | 6.6x | 4.5x |
| `core/bra_self` | 8.00 | 27.6 | 36.2 | 290 | 17.3x | 11.6x |
| `core/dbra_self` | 9.00 | 36.4 | 27.5 | 247 | 14.7x | 9.9x |
| `chip/flash_nop` | 4.00 | 40.5 | 24.7 | 99 | 5.9x | 4.0x |
| `chip/ram_move` | 16.00 | 99.9 | 10.0 | 160 | 9.5x | 6.4x |
| `chip/peripheral_move` | 28.00 | 253.0 | 4.0 | 111 | 6.6x | 4.4x |
| `chip/flash_nop_pit_masked` | 4.00 | 40.7 | 24.6 | 98 | 5.9x | 3.9x |
| `chip/flash_nop_pit_interrupting` | 4.24 | 42.4 | 23.6 | 100 | 6.0x | 4.0x |

| Benchmark | Time |
| --- | --- |
| `core/short_runs` | 29.3 ns per run |
| `chip/short_runs` | 183.2 ns per run |
| `tables/instructions` | 5.66 ms; 0.97 ms with registration enumerating only a pattern's free bits |
| `tables/timing` | 5.22 ms |

So the plain interpreter runs firmware about 2 to 10 times faster than
a 16.78 MHz 68331, not the "tens of times" PLAN.md expects; register
to register instructions, which the real part does in 2 clocks, are
the slowest relative to it, and those the tables charge many clocks
(`mulu`, at its maximum of 26) the fastest.

## Where the time goes

`perf` is not available to unprivileged users on the measuring
machine (`perf_event_paranoid` 4), so the evidence is from gdb: a
sampling profile (SIGINT every 20 ms, 400 samples, inlined frames
resolved from debug info, `CARGO_PROFILE_BENCH_DEBUG=true`), and exact
counts of host instructions per emulated instruction (`stepi` from one
entry of an instruction's handler to the next, in the measured loop),
on the baseline.

| Benchmark | Host instructions per emulated instruction |
| --- | --- |
| `core/nop` | 240 |
| `core/move_register` | 473 |
| `chip/flash_nop` | 358 |
| `chip/peripheral_move` | 1923 |
| `chip/short_runs` | 1736 per run |

The baseline's hot spots, largest first, each with what
Optimizations, below, does about it:

1. **The run loop's per-instruction bookkeeping** (`Cpu::run_for`,
   `Cpu::step`): about 100 of a NOP's 240 host instructions.  Each
   instruction resets about ten fields, builds a `Core`, samples the
   interrupt level through `dyn Interrupts`, loads both tables through
   their `OnceLock`s, and matches a `(Result<Flow, Exception>,
   Option<Fault>)` pair through closures.  Cut by 70 a NOP, in two
   passes (Optimizations, a leaner `step`).
2. **Instruction fetch** (`fetch_word`, `access_read`, `cycle_read`,
   `Memory::read`): about 80 of a NOP's.  Every fetch goes through the
   page table: the address mask, `program_space` (11 alone), three
   bounds-checked indexings and the stall computation.  Cut by a
   cached code page (Optimizations).
3. **Effective addresses decoded at run time**: MOVE.L D0,D1 costs
   473, twice a NOP; `move_` and `effective_address` are 70% of its
   samples, with one handler per family that decodes both fields each
   time.  Cut by specializing the families by size and mode
   (Optimizations).
4. **The chip's interrupt level, every instruction** (`Board::level`):
   about 120 host instructions per instruction (`chip/flash_nop` 358
   against `core/nop` 240), polling the SIM, every module's request
   and every wired peripheral's `interrupt_request`.  Cached
   (Optimizations).
5. **Chip-select decode on every external cycle**: of a peripheral read's
   1923, about 600 are `ChipSelects::matches` scanning all twelve
   selects, collecting into a `Vec` (a heap allocation and free per
   cycle); a word on an 8-bit port decodes three times.  Decoded per
   page, with no allocation (Optimizations).
6. **Next-event syncing** (`chip/short_runs`): a one-clock run costs
   1736, of which about 800 are the GPT's `next_event` (`tick_matters`,
   `next_tick`, `matches`) and its `advance`, though the benchmark
   never enables a GPT interrupt: 183 ns a run, against 64 ns for the
   same run without the GPT.  It matters to hosts that run in short
   quanta and to firmware that keeps the PIT or a GPT channel busy.
   With the GPT's next event cached, and the SIM's and the modules' in
   the chip (Optimizations): 1225.
7. **Table builds**: registering each of about 90 opcode patterns by
   filtering all 65536 opcodes is 6 million tests, so the instruction
   table enumerates only the opcodes each pattern leaves free; the
   timing table decodes every opcode by a linear search of the
   encoding rows.  About 11 ms once per process by filtering, about 6
   by enumerating.

## Candidate optimizations

Measured with an interleaved A/B harness (the original and a modified
copy of the crates in one binary, alternating trials, minimum of 1000
each; an A/A run differs by up to 9%).  Those marked done are in
Optimizations; the others are open, each needing its own design and
review.

| Change | Measured or estimated gain |
| --- | --- |
| Specialize MOVE (and the other families) by mode, as PLAN.md intends | Measured: a register-to-register fast path made `core/move_register` 0.55x the time (1.8x faster).  Done (below). |
| Cache the chip's interrupt level, recomputed after a bus cycle to the board, a quantum's `advance`, an acknowledge, a reset or a host call that changes pins or peripherals | Measured (sampled once per quantum, an upper bound): `chip/flash_nop` 0.79x, `flash_nop_pit_interrupting` 0.69x, `ram_move` 0.90x.  Done (below). |
| Cache each module's next event, recomputed only when its registers are written or it is advanced past it | Estimated from counts: `chip/short_runs` about 2x faster (1736 to about 900).  Done for the GPT: measured with criterion, `chip/short_runs` 218 to 126 ns per run (0.58x), `chip/flash_nop` within noise (+4%, load average 27).  A debug assertion checks the cache against the search.  Then for the SIM and the modules together, in the chip (below): 1335 to 1225 host instructions a run. |
| Leaner `step`: fewer per-instruction resets, no `OnceLock` loads in the loop, the common no-fault path first | Estimated from counts: about 100 of a NOP's 240, up to 1.5x on simple instructions.  Done in two passes, 29 and then 41 (below); about 9 more are the `dyn Interrupts` call and handler results in memory. |
| Fetch through a cached code page (PLAN.md, Memory) | Estimated from counts: about half of the 80 fetch instructions.  A naive shortcut through `Memory::read` measures slower (1.05-1.2x the time), so this needs the real cache, not a bypass.  Done (below). |
| Chip-select decode cached per page for peripheral pages, no `Vec` | Estimated from counts: about 30% of a peripheral cycle.  Removing the `Vec` alone measured 0.94x on `peripheral_move`, within noise of the level cache's share.  Done (below). |
| Fat LTO and one codegen unit | Measured with criterion: 5-20% on most benchmarks.  A host's profile, not ours: a library's profile settings do not reach its users. |
| Instruction table registration by enumerating only a pattern's free bits | Measured: `tables/instructions` 5.7 ms to 0.97 ms (done). |
| Timing table built at compile time (its decoder is `const fn`) | Estimated: its 5.2 ms to zero, at some compile time. |

A predecoded block cache and lazy condition codes (PLAN.md, Deferred
optimizations) come after these: the per-instruction overhead they
would amortize is mostly the loop and fetch above, which are cheaper
to cut directly.

## Against silicon

The only real-hardware timing so far
([smartclock](https://github.com/charlieh0tel/smartclock),
`docs/firmware/restart.md`, "The watchdog"): on a bench Z3801A (3543-A), a watchdog
reset to the `scpi >` prompt took roughly 1.0 to 2.1 s by hand.  The
chip reaches the Z3805A 3543B's prompt (same boot code and checksums)
at 1.1 s of emulated time, Unicorn's 2-cycles-a-byte estimate at
0.68 s.  Coarse (a Z3801A, a warm reset, a hand timing), but it fits
the CPU32RM section 8 model and not the flat estimate.  A host-
timestamped first console byte after power-on is the better number
still to get.

## Optimizations

Each measured two ways on the same machine under the same load: host
instructions per emulated instruction, counted with gdb (`stepi`
across one whole `Cpu::run_for` of the benchmark's kernel, a few hundred
clocks, divided by the instructions it ran; exact and repeatable, but
a rebuild that changes only inlining elsewhere moves a count by up to
10%), and criterion's median time per iteration, the before and after
trees run interleaved three times and the lowest median of each
taken (load average about 26).

**Handlers specialized by size and effective address kind** (PLAN.md,
Interpreter): MOVE by size, source and destination; MOVEA, TST and
ADD, SUB, CMP, AND, OR `<ea>, Dn` by size and source.

| Benchmark | Host instructions before | After | Criterion before | After | Speedup |
| --- | --- | --- | --- | --- | --- |
| `core/nop` | 240 | 240 | 244 us | 236 us | 1.0x |
| `core/move_register` | 473 | 258 | 474 us | 256 us | 1.85x |
| `core/move_indirect` | 592 | 400 | 318 us | 181 us | 1.75x |
| `core/move_postincrement` | 917 | 742 | 182 us | 138 us | 1.3x |
| `core/move_displacement` | 692 | 503 | 202 us | 126 us | 1.6x |
| `core/move_indexed` | 741 | 551 | 151 us | 105 us | 1.45x |
| `core/move_immediate_absolute` | 921 | 681 | 180 us | 127 us | 1.4x |
| `core/add_register` | 446 | 284 | 438 us | 242 us | 1.8x |
| `core/tst` | 365 | 254 | 364 us | 174 us | 2.1x |
| `chip/ram_move` | 982 | 780 | 73 us | 48 us | 1.5x |

Building the instruction table, picking a handler per opcode, takes
as long as without it (`tables/instructions`, 0.76 ms and 0.70 ms).

**The chip's interrupt level cached** (PLAN.md, Interrupts): the board
recomputes it only after an event that can change a source (a bus
cycle to the board, an acknowledge, a quantum's `advance`, a reset, the
host's change of a pin, a peripheral or a module), not at every
instruction.  A debug assertion checks the cached level against a
fresh one at every sample, so every debug test checks it; a test runs
a PIT and an SCI interrupt program in one long run and one
instruction at a time and checks both logs against a log recorded
without the cache.  Criterion's medians are noisier here (load average
about 26).

| Benchmark | Host instructions before | After | Criterion before | After | Speedup |
| --- | --- | --- | --- | --- | --- |
| `chip/flash_nop` | 422 | 279 | 103 us | 65 us | 1.5x |
| `chip/flash_nop_pit_interrupting` | 428 | 282 | 89 us | 59 us | 1.5x |
| `chip/ram_move` | 780 | 640 | 51 us | 38 us | 1.2x to 1.35x |
| `chip/peripheral_move` | 1813 | 1819 | 65 us | 65 us | 1.0x: a peripheral cycle each instruction recomputes it |

**Chip-select decode per page** (PLAN.md, Memory): a page table
rebuild also records, for each 2-Kbyte page, the selects whose blocks
hold it (`ChipSelects::at`; a block is at least a page and aligned to
its size), and an external cycle tries only those, collected on the
stack, not into a `Vec`.  While a decode change awaits its rebuild a
cycle tries all twelve; a debug assertion checks every cycle's
matches against all twelve.

| Benchmark | Host instructions before | After | Criterion before | After | Speedup |
| --- | --- | --- | --- | --- | --- |
| `chip/peripheral_move` | 1819 | 1539 | 61.8 us | 52.2 us | 1.18x |

**Fetch through a cached code page** (PLAN.md, Memory): the fetch
fast path, inlined, reads the word through the page fetch last used,
with no page table lookup; the rest of fetch, out of line.  Criterion
did not separate this change from the load (medians moved both ways
by up to 1.6x between rounds), so the counts are the evidence.

| Benchmark | Host instructions before | After |
| --- | --- | --- |
| `core/nop` | 250 | 214 |
| `core/move_register` | 268 | 232 |
| `core/move_indirect` | 412 | 375 |
| `core/add_register` | 294 | 257 |
| `chip/flash_nop` | 282 | 246 |
| `chip/ram_move` | 639 | 522 |
| `chip/peripheral_move` | 1626 | 1507 |

**A leaner `step`**: the common case (the instruction completed,
untraced, no released write's fault) is taken first, with no
exception matching; the stall, extra and refunded clocks are cleared
once, by `Cpu::run_for`, not again by `step`; MOVEM's continuation is moved
only when there is one.

| Benchmark | Host instructions before | After |
| --- | --- | --- |
| `core/nop` | 216 | 187 |
| `core/move_register` | 234 | 205 |
| `core/move_indirect` | 377 | 348 |
| `core/add_register` | 259 | 230 |
| `core/tst` | 230 | 201 |
| `chip/flash_nop` | 248 | 219 |
| `chip/ram_move` | 524 | 495 |
| `chip/peripheral_move` | 1509 | 1479 |

**A leaner per-instruction loop still** (PLAN.md, Interpreter): one
`Core` serves a whole run, its start time moved to each instruction,
not one for the interrupt sample and another for the instruction; the
tables are loaded once a run, as arrays an opcode indexes with no
bounds check, not through their `OnceLock`s at every instruction; the
opcode fetch's fast path, `Memory::fetch` included, is inlined into
the loop, not a call returning its `Result<u16, Exception>` through
memory; an instruction that does not simply complete, and an
interrupt taken, finish out of line.  Counted before and after in one
small driver binary per tree (gdb `stepi` across one whole `Cpu::run_for` of 2000 clocks, or 4000 on
the chip, divided by the instructions it ran), so the before column
differs from the table above's, counted in the benchmark binary.

| Benchmark | Host instructions before | After |
| --- | --- | --- |
| `core/nop` | 198 | 157 |
| `core/move_register` | 216 | 175 |
| `core/move_indirect` | 352 | 310 |
| `core/add_register` | 241 | 200 |
| `core/tst` | 212 | 171 |
| `chip/flash_nop` | 223 | 182 |
| `chip/ram_move` | 489 | 441 |
| `chip/peripheral_move` | 1496 | 1448 |

Of a NOP's 157, about 40 are the fetch (the address mask, the program
space from SR, the cached page's checks and bounds), about 15 the
interrupt sample, about 10 the handler's call and its result.  Two
costs the estimate counted are left: the interrupt sample is still a
call through `dyn Interrupts` (about 5: a run generic over its
`Interrupts` would remove it, at a change to `Cpu::run_for`'s signature
and a copy of the loop per host type), and a handler still returns its
`Result<Flow, Exception>` through memory (about 4 for a NOP: an
`Exception` is 16 bytes, so only a smaller error, the exception kept
in the CPU, would return in registers, a change to every handler).

**The other families specialized by size and mode** (PLAN.md,
Interpreter): `Dn, <ea>` ADD, SUB, AND, OR and EOR by size and
destination; ADDI, SUBI, ANDI, ORI, EORI, CMPI, ADDQ and SUBQ by size
and destination; ADDA, SUBA and CMPA by size and source; NEG, NEGX,
NOT and CLR by size and operand; the register shifts by size, type,
direction and where the count is, the memory shifts by type, direction
and operand; the bit instructions by operation and operand; LEA and
PEA by source.  Counted without the leaner loop above, with the
kernels added for it, `stepi` across one 400-clock `Cpu::run_for` (its entry and exit included,
so the per-instruction count is a little higher than above for
kernels that run few instructions).  Not timed with criterion: other
builds loaded the machine.

| Benchmark | Host instructions before | After | Fewer |
| --- | --- | --- | --- |
| `core/nop` | 198 | 198 | 1.0x |
| `core/add_register` | 241 | 241 | 1.0x, specialized already |
| `core/add_to_memory` | 641 | 511 | 1.25x |
| `core/eor_register` | 376 | 217 | 1.75x |
| `core/adda` | 315 | 208 | 1.5x |
| `core/addq` | 408 | 231 | 1.75x |
| `core/addi` | 604 | 358 | 1.7x |
| `core/cmpi` | 498 | 296 | 1.7x |
| `core/neg` | 392 | 263 | 1.5x |
| `core/clr_indirect` | 411 | 338 | 1.2x |
| `core/lsl_immediate` | 326 | 223 | 1.45x |
| `core/lsr_register` | 304 | 250 | 1.2x |
| `core/btst_immediate` | 421 | 274 | 1.55x |
| `core/bset_memory` | 606 | 465 | 1.3x |
| `core/lea_displacement` | 335 | 285 | 1.2x |
| `core/pea_displacement` | 540 | 490 | 1.1x |

The cost is code: the instruction handlers' machine code (`nm`, the
symbols under `mc683xx_cpu32::instructions`) is 588 Kbytes against
295 unspecialized, a release binary's text 1120 Kbytes against 764.  A firmware's working
set takes few of the variants, but whether the instruction cache
notices on a whole firmware run is not measured yet.

**The SIM's and the modules' next event cached** (PLAN.md,
Peripherals, Time): the GPT keeps its own; the chip keeps
the earliest of the SIM's, the QSM's and the GPT's, found again only
after an access to the SIM or a module, an acknowledge, a reset, a
restore, the host's change of a module or a SIM pin, or an advance
that reaches it.  The peripherals' are asked every quantum.  A debug
assertion checks the cache against a fresh search every quantum.
Measured as host instructions in one
`Mc68331::run_for` of one clock (gdb `stepi`, as above, the same in five
runs), and criterion's median per 1000 runs, the before and after
binaries run interleaved three times, the lowest median of each
(load average 4 to 10).  What a one-clock run spends with it: the GPT's
advance about 250 (TCNT's catch-up, the fast edges' search, the PWM's
sync), the diversions' check about 50, the CPU's run and step about
160.

| Benchmark | Host instructions before | After | Criterion before | After |
| --- | --- | --- | --- | --- |
| `chip/short_runs` | 1335 per run | 1225 | 64.1 us | 61.5 us |
| `chip/flash_nop` | not counted | | 48.5 us | 48.2 us |
| `chip/flash_nop_pit_interrupting` | not counted | | 49.5 us | 49.5 us |

**Forced inlining**: the core keeps `#[inline(always)]` only where
`#[inline]` alone changes the optimized code of the benchmarks, found
by dropping each in turn and comparing the `core` and `chip` benchmark
binaries' symbols and function sizes.  `Core::interrupt` compiles the
same either way and has no `always`.

| Function | With `#[inline]` alone |
| --- | --- |
| `Core::step` | `Cpu::run_instructions`, the run loop, changes: 2091 bytes to 2041 |
| `Core::fetch_fast` | out of line, seven copies: a call per opcode fetch |
| `Memory::fetch` | out of line, seven copies, in `fetch_fast` |
| `Core::effective_address_of` | out of line: a handler decodes every kind, not its own |
| `arithmetic::read_modify_write` | out of line, one per operation |
| `shift::shift` | three rotate handlers change |
| `shift::set_flags` | 85 shift and rotate handlers change |

## QSPI queues

A run syncs only where the queue shows (PLAN.md, Peripherals): with a
peripheral that is not deferrable, once an entry, at its transfer;
with a deferrable one, or none, only at an interrupt, a halt or a
buffered SPCR2 taking effect.  Entry by entry, each entry would end a
run's quantum twice, at its transfer and at the end of its delay,
about 1100 host instructions a quantum.  Measured on `flash_nop` with the queues of `chip/flash_nop_qspi_*` beside it,
against the same sled with the QSPI idle: host instructions in a
2000-clock iteration (gdb `stepi`, as above), and the time, the
minimum of 2000 trials of each, the loads interleaved, the two
binaries run in turn twice (load average 21).

| Queue | Quanta entry by entry / batched (20,000 clocks) | Host instructions entry by entry | Batched | Time entry by entry | Batched |
| --- | --- | --- | --- | --- | --- |
| idle | 1 / 1 | 111,020 | 111,054 | 1.00 | 1.00 |
| 16-bit | 483 / 242 | 1.48x | 1.30x | 1.64x | 1.43x |
| 8-bit | 785 / 393 | 1.79x | 1.50x | 1.97x | 1.69x |
| 8-bit, deferrable | 785 / 1 | 1.79x | 1.08x | 1.97x | 1.07x |

What is left with a deferrable peripheral is the entries' own work; a
peripheral that is not deferrable still pays a sync a transfer, the
cost of a quantum (`chip/short_runs`) the next thing to cut.
The smartclock firmware (58503A 3704, `qspi_queue_setup`) runs no
queue in wraparound: each transfer is one pass of up to 16 entries
ending in SPIF, polled or interrupting, so these are its worst case
held for ever, not its load.

That quantum a call is the floor for a peripheral that is not
deferrable: its calls must come with the CPU and the board's other
peripherals at their time.  In the randomized test of the QSPI's batching
(`batched_entries_show_what_entry_by_entry_does`, 300 programs) such a
peripheral is called in 58,930 distinct instructions, and its runs
sync 62,351 times, against 9,413 with a deferrable peripheral or none
and 66,717 entry by entry; the difference from the calls is mostly
the interrupts, halts and buffered SPCR2 writes any peripheral syncs
for.  A queue's stop without wraparound, or its turn to a slave,
after an entry without CONT, whose release of PCS already called the
peripheral at the transfer, does not sync: 336 syncs in that test.  A polled pass of a queue (no SPIFIE) whose last entry has
no CONT saves its last sync.  Anything more needs a peripheral that
says its calls may wait: on a board of `mc683xx-board` peripherals, every
peripheral on the QSPI saying so (`Peripheral::qspi_deferrable`; a `SpiAdapter`
asks its model's `deferrable`) and no PCS pin on a sensed net.

## Board peripherals

A peripheral called within the chip's run that drives a net, writes a
memory or moves its next event before the run's end ends the run
after that instruction, for the board to settle there (PLAN.md,
Boards, Board time): the run is then the same however the host
quanta it.  Measured with criterion, without and with the end, the
same machine under other builds (load average 7 to 20):

| Benchmark | Without | With |
| --- | --- | --- |
| `peripherals/registers_writes` (a call a write, no drive) | 12.25 us | 12.98 us (+4%) |
| `peripherals/latch_writes` (a drive and a sync a write) | 30.4 us | 46.7 us (+51%) |
| `chip/*` | | within noise (-14% to +10%, no direction) |

An iteration is 1 ms of board time, 282 instructions, 94 of them
writes.  A peripheral that drives at every write costs a sync a write,
about 170 ns each; one that drives nothing pays only the check of
its next event after each call.  The chip pays a flag's test per bus
cycle (`SyncRequest`).

A chip output on a net a peripheral or the chip senses ends the run where
it changes, likewise (`Mc68331::set_output_stop`).  Measured the
same way (load average 3 to 4), without the stop set and with it:

| Benchmark | Without | With |
| --- | --- | --- |
| `peripherals/port_unwired` (PF0 toggled at each write, on no net) | 11.84 us | 9.11 us (layout; the same work) |
| `peripherals/port_sensed` (PF0 on an edge counter: a sync a write) | 29.1 us | 42.3 us (+45%) |
| `peripherals/registers_writes`, `peripherals/latch_writes` | | within 2% |
| `chip/*` | | within noise (-5% to +1%; `short_runs` +0.4% rerun) |

A sensed output that changes at every write costs a sync a write,
about 140 ns each; an output nothing senses, or a board that senses
none, costs a comparison of `OutputStop` with none per quantum.

## The board generic over its chip

`Board<C: Chip>` (PLAN.md, Boards, Chip) and the merge of the chip's
and the bare machine's mapped peripherals into one trait.  Measured
with criterion, the whole suite, against a board for the MC68331
alone with a trait for each (load average 2 to 3).  Two alternatives
measure slower and are not taken: the chip telling its peripherals'
cycles whether exception processing is stacking costs a borrow of
its bus at each stacking change (`chip/short_runs` +6%,
`chip/flash_nop_qspi_bytes` +8%), so the chip leaves
`BusCycle::stacking` false; and the board taking the chip's outputs
through a `dyn FnMut` into a fresh vector each quantum costs
`peripherals/port_unwired` +102% and `port_sensed` +53%, so
`Chip::collect` is generic over its closure and the board filters as
the chip emits, into a vector it keeps.

| Benchmark | Change |
| --- | --- |
| `peripherals/*` | -8% to +1% |
| `chip/flash_nop`, `ram_move`, `pit_masked`, `qspi_bytes_deferred` | within 0.4% |
| `chip/peripheral_move` | -5% |
| `chip/flash_nop_pit_interrupting`, `qspi_words`, `qspi_bytes` | +3% to +5% |
| `chip/short_runs` | +9% to +10% |
| `core/*` | -7% to +3% |
| `spi/exchange_mode_3` | +10% |

The chip's own code in the run loop is unchanged but for names, and
`spi::exchange` and `core/*` not at all, yet they move as much: the
changes are code placement across the crates (no LTO, 16 codegen
units), not work, and within this machine's noise for a moved build.

## The bare CPU

Two-level page tables (PLAN.md, Memory and Bare CPU): page tables in slabs, then limited runs, stop-on-fault,
intercepts, hook context and the parity deviations, none costing a run
that does not ask.  Measured as the best of 40 rounds of 100
iterations, without and with them built side by side and run
interleaved three times on one pinned core (no gdb); within the noise
but `move_postincrement`.  A run loop generic over its limits
measures +20% on `nop`: two copies of the loop stop `step` being
inlined, so the loop is not generic.

| Benchmark | Without | With |
| --- | --- | --- |
| `core/nop` | 81 us | 79 us |
| `core/move_indirect` | 75 us | 75 us |
| `core/move_displacement` | 51 us | 49 us |
| `core/move_postincrement` | 55 us | 59 us |
| `core/add_register` | 97 us | 99 us |
| `core/dbra_self` | 26 us | 26 us |

From Python, `mc683xx.Machine` runs about 700,000 calls a second of a
three-instruction function to its return (`test_cpu.py`'s
`test_thousands_of_short_calls_a_second`, a release build; 180,000 in
the debug build `make python` makes), and makes 15,000 CPUs a second
with 64 Kbytes mapped.
