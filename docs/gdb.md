# Debugging with gdb

The `mc683xx-gdb` crate (`crates/gdb`) is a GDB remote stub: gdb's
remote serial protocol, through the `gdbstub` crate, over the bare
CPU (`mc683xx_cpu32::flat::Machine`), the chip
(`mc683xx_mc68331::chip::Mc68331`) or a board
(`mc683xx_board::board::Board`).  gdb connects over TCP,
reads and writes registers and memory, sets breakpoints and
watchpoints, steps, continues and interrupts.  `PLAN.md` (Debugger)
records why it is built this way; this says how to use it.
`docs/gdb-firmware.md` is the short recipe for a real image.

The crate's API is documented on docs.rs; this guide's Rust block is
compiled by `cargo test` (the `examples` crate runs it).

## gdb

Any gdb with the m68k target: `gdb-multiarch` on Debian and Ubuntu,
or a cross gdb (`m68k-elf-gdb`).  The stub sends a target
description, so gdb needs no `set architecture`:

```text
$ gdb-multiarch firmware.elf
(gdb) target remote 127.0.0.1:1234
(gdb) info registers
(gdb) break main
(gdb) continue
(gdb) watch counter
(gdb) stepi
(gdb) x/8xh $pc
(gdb) monitor reset
(gdb) flushregs
(gdb) detach
```

Without an ELF, `set architecture m68k:cpu32` before `target
remote` lets gdb disassemble (`x/4i $pc`) a raw image.

## From the runner

```text
mc683xx-run examples/boards/echo.toml --bind console=stdio --gdb 127.0.0.1:1234
```

loads the board through its power-on reset, says `mc683xx-run:
waiting for gdb on 127.0.0.1:1234` on standard error, and waits there,
the CPU at the reset vector's PC.  gdb then runs the board: its
bindings, trace and positions go on between the stub's quanta as
between the runner's turns.  When gdb detaches the run goes on to its
end (`--seconds` and `--cycles` count the time gdb ran it too); when
gdb kills it the runner ends with `stopped reason=requested` and exit
status 4.  A session that fails (gdb gone without detaching, or a
packet the stub cannot parse) is said on standard error and ends as a
detach does: the run, its trace and `--save` go on.  Ctrl-C to the
runner, while it waits for gdb or while gdb holds the board, ends the
session and the run, `stopped reason=requested` and exit status 4,
`--save` still saving.  Piped standard
input bound with `stdio` is read to its end the first time the board
runs past `--input-after`, under gdb too: the first continue or step
then waits for the pipe to close.  Port 0 picks a
free port, which the line names.

## From Python

`serve_gdb(address="127.0.0.1:1234", ready=None)` on `mc683xx.Machine`,
`Mc68331` and `Board` (`docs/python.md`, Debugging with gdb) hands the
target to gdb mid-run and returns `Ending.DETACHED` or `Ending.KILLED`; the
harness then has it back, as gdb left it.  `ready(port)` is called
once it listens.  The Python peripherals and hooks run under gdb as
in `run_for`, and an exception from one ends the session and is raised.
While the stub waits for gdb it releases the GIL, 20 ms at a time
(`stub::accept_polled`, `stub::Polled`, a host's `stub::Waiter`), so a
client in another thread of the same process is served, and Ctrl-C
ends the session with `KeyboardInterrupt`, whether gdb is connected
or not, idle or running the target; the stub's breakpoints and
watchpoints go with it.

## From Rust

```rust,no_run
use mc683xx_cpu32::address::Address;
use mc683xx_cpu32::flat::ADDRESS_BITS;
use mc683xx_cpu32::flat::Machine;
use mc683xx_gdb::stub;
use mc683xx_gdb::stub::Ending;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // The reset vectors, then the program: `bra.s` to itself.
    let image = [0, 0, 0x80, 0, 0, 0, 0x04, 0, 0x60, 0xfe];
    let mut machine = Machine::new(ADDRESS_BITS)?;
    machine.map(Address(0), 0x1_0000, &[])?;
    machine.write(Address(0), &image[..8])?;
    machine.write(Address(0x400), &image[8..])?;
    machine.reset().map_err(|halt| format!("{halt:?}"))?;
    let connection = stub::accept("127.0.0.1:1234", |address| {
        eprintln!("waiting for gdb on {address}");
        Ok::<(), std::io::Error>(())
    })?;
    if stub::serve(&mut machine, connection)? == Ending::Detached {
        // gdb has let go: the host runs the machine on.
    }
    Ok(())
}
```

- `stub::accept(address, ready)` listens at `address`, calls `ready`
  with the address it listens at (port 0 picks one), and takes one
  connection.
- `stub::serve(target, connection)` serves gdb until it detaches
  (`Ending::Detached`) or kills the target (`Ending::Killed`).  The
  target runs only inside it, as gdb asks: it stays where it is until
  gdb continues or steps it.  Any `gdbstub::conn::ConnectionExt`
  serves, a `TcpStream` or the tests' in-memory one.  It is the one
  foreign type in the stub's API, deliberately: a connection is
  gdbstub's to drive, and an own trait would only forward to it, so
  the crate's major version follows gdbstub's.
- `stub::SessionError`: the target's run failed (`Target`, a board's peripheral
  that stalled), the connection did (`Connection`, gdb gone without
  detaching), or gdb sent what the stub could not take (`Protocol`).
  `Ending`, `stub::SessionError` and `target::TargetRun` are `#[non_exhaustive]`:
  a match on one needs a wildcard arm.
- Features: `mc68331` (the chip as a target) and `board` (a board,
  and the chip with it), both on by default.  A bare-CPU host takes
  `default-features = false` and builds without the chip and board
  crates.
- `target::Target` is the trait the three implement, and a host with
  a run loop of its own implements, delegating to one of them:
  `with_cpu` (a `CpuControl`: registers and hooks), `peek` and `poke` (memory at an `Address`, with no
  bus cycle), `run_for(cycles, hit)` (a quantum, its end a `target::TargetRun`)
  and `reset`.  `mc683xx-run` and the Python bindings do.

## Registers

gdb's m68k core registers (`org.gnu.gdb.m68k.core`): `d0`-`d7`,
`a0`-`a5`, `fp` (A6), `sp` (A7, the stack pointer S selects), `ps`
(SR) and `pc`; then a feature of the stub's own,
`org.mc683xx.cpu32.system`, with `usp`, `ssp`, `vbr`, `sfc` and `dfc`
(`info registers system`).  The CPU32 has no FPU, so the description
has no floating-point feature and gdb shows no `fp0`-`fp7`.  A write
of `ps` keeps SR's unimplemented bits clear (CPU32RM 2.3.1); `sp`,
`usp` and `ssp` take whichever gdb changed.  A `G` packet shorter
than the core feature's sets the registers it gives whole, from `d0`,
and leaves the rest; one that ends within a register is refused with
`E16`.  A read of a description other than `target.xml` is `E00`.

## Memory

Every access gdb makes is in supervisor data space and has no bus
cycle, so a debugger's look never has a peripheral's side effect:

| Access | Reads | Writes |
| --- | --- | --- |
| RAM and ROM the core serves | yes | yes, ROM included, whatever the page permissions |
| The chip's module registers (SIM, QSM, GPT) | yes, as a peek (`Module::peek`): no flag armed or cleared | no |
| A peripheral (a peripheral on a select, a bare CPU's host peripheral) | no: `E0e` | no: `E0e` |
| Memory a peripheral shares a select with, or a memory a peripheral has taken | no | no |
| Nothing | no | no |

On the chip a byte is what the decode reaches now (`Mc68331::peek`),
so the memory map is the firmware's: out of reset only CSBOOT
decodes.  A read stops at the first byte it cannot give; gdb shows
what came before it.  To make a real cycle, with its side effects, a
Rust or Python host uses `Mc68331::bus_read` and `bus_write` itself.

## Breakpoints, watchpoints and steps

- `break` and `hbreak` (`Z0`, `Z1`) are code hooks (`Cpu::add_code_hook`)
  at the address: nothing is written to memory, so they work in ROM,
  firmware that sums its code never sees them, and there is no limit
  on their number.  The CPU stops before the instruction; a continue
  runs it.
- `watch`, `rwatch` and `awatch` (`Z2`, `Z3`, `Z4`) are write and read
  hooks on the bytes: an operand access that overlaps them stops the
  CPU once its instruction ends, as a hardware watchpoint does.
  Fetches are not reads.  On the chip a long is two word cycles; on the
  bare CPU one access of 4.
- `stepi` (`s`) runs one instruction: a code hook on every address
  stops the CPU before the next.  An instruction that takes an
  exception steps to the handler's first instruction; `stop` steps past
  itself, the CPU then waiting.
- `continue` (`c`) runs in quanta of 100,000 system clocks; between
  quanta the stub looks for gdb's Ctrl-C.
- The hooks are the stub's, installed while gdb wants them and removed
  when the session ends, however it ends.

## Stops

| Why the CPU stopped | gdb sees |
| --- | --- |
| A breakpoint | `SIGTRAP`, at it (`swbreak` or `hwbreak`) |
| A watchpoint | `SIGTRAP`, with the watched address |
| A step | `SIGTRAP` |
| Ctrl-C | `SIGINT` |
| The host's own hook (a Rust closure, a Python callable) | `SIGTRAP` |
| A peripheral's or the host's stop request | `SIGINT` |
| A double bus fault halt | `SIGBUS`; it stays halted until a reset: `monitor reset`, or the watchdog's as time passes |
| A bare CPU's fault or intercept stop (`set_stop_on_fault`, `set_intercepted`) | `SIGBUS` |

A signal gdb passes back on continue (`signal SIGINT`) is dropped: the
CPU has none.

## Monitor commands

- `monitor reset`: the bare CPU's reset exception (SSP and PC from
  the vectors at zero), the chip's or board's external reset (the
  modules, the peripherals it reaches, the board's RESET net).  gdb's
  register cache is then stale: `flushregs`.
- `monitor help`.

## Determinism and cost

- A session is deterministic but for Ctrl-C: breakpoints, watchpoints
  and steps stop the CPU where the same run would be, and a continue
  runs the chip with `run_until`, so a series of quanta is one long
  run.  A Ctrl-C lands at the quantum boundary the byte arrives by, which
  is host-timed.
- Nothing is spent without gdb: the stub is outside the core and adds
  nothing to the run loop.  With gdb attached the hooks cost what hooks
  cost (PLAN.md, Hooks): a breakpoint flags its page, a watchpoint
  takes its pages off the fast path, a step is a hook on every page for
  one instruction.

## Testing it

`cargo test -p mc683xx-gdb` drives the stub with raw packets over an
in-memory connection: registers, the target description, memory,
steps, breakpoints, watchpoints, Ctrl-C, `monitor reset`, detach and
kill, on the bare CPU, the chip and a board.  `make gdb`, opt-in and
not in `make ci`, runs `gdb-multiarch` against the runner's example
board (`scripts/gdb-smoke.sh`).
