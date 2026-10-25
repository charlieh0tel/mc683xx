# mc683xx

An emulator for the Motorola MC68331 microcontroller, in Rust, with
Python bindings: the CPU32 core, the chip's own modules (SIM, QSM,
GPT), and a board layer that wires memories and peripherals around the
chip as a board does.  It runs real firmware inside a host program:
board emulators, firmware test harnesses, fuzzers, debuggers.

Licensed `MIT OR Apache-2.0`.

## Crates

One repository and one version; this README is every crate's on
crates.io.

| Crate | What it is |
| --- | --- |
| `mc683xx-cpu32` | The CPU32 core, and a bare CPU on flat memory (`flat`). |
| `mc683xx-modules` | The SIM and the QSM, and the interfaces host peripherals plug into. |
| `mc683xx-mc68331` | The GPT, and the MC68331 chip built from the core and the modules. |
| `mc683xx-board` | Board files: memories, peripherals from a registry, decoders and nets around the chip, on board time. |
| `mc683xx-peripherals` | Stock peripherals for board files, each from its datasheet. |
| `mc683xx-gdb` | A GDB remote stub over the bare CPU, the chip or a board. |
| `mc683xx-run` | The runner: runs a board file from the command line. |
| `mc683xx` (PyPI) | The Python package. |

## Status

- **CPU32 core:** every instruction family (data movement, integer
  arithmetic and logic, shifts and rotates, bit operations, BCD,
  multiply and divide, `chk2`/`cmp2`, `tbl*`, program and system
  control, `movec`/`moves`, `stop`/`lpstop`/`bkpt`), exception
  processing with the CPU32's frames, interrupts, trace, and
  table-based cycle estimates.  Checked against the manuals,
  SingleStepTests' 68000 cases, Musashi and a firmware function
  corpus.
- **Chip:** the SIM (clock synthesizer, chip selects, bus monitor,
  PIT, watchdog, ports, resets), the QSM (QSPI and SCI) and the GPT,
  on next-event timing, deterministic run to run.
- **Host interface:** peripherals on chip-select pins, QSPI peripherals,
  module pins, code, block and memory hooks, page permissions,
  tracing, and save/restore.
- **Boards:** board files in TOML (memories, peripherals from a registry,
  decoders and nets on picosecond board time), and `mc683xx-run`, a
  command that runs one.
- **Stock peripherals:** 74x373/374 latches, 74x244 buffer, 74x164
  shift register, AD569 DAC, ADC0838 ADC, AT28C64B EEPROM, MC144110
  DAC, MC68681 and SCC68692 DUARTs, each modeled from its datasheet.
- **gdb:** a GDB remote stub over the bare CPU, the chip or a board.
- **Python:** the chip, boards and a bare CPU, with Python objects as
  peripherals and callables as hooks, and type stubs.

Early: 0.x, and the API may change between minor versions.

## Install

```
cargo add mc683xx-board      # boards, with the chip and core below it
cargo install mc683xx-run    # the runner
pip install mc683xx          # the Python package
```

## Rust: a board (`mc683xx-board`)

Load a board file with the stock peripherals and run it for a second
of board time (from the repository's root):

```rust
use std::error::Error;
use std::path::Path;

use mc683xx_board::board::Board;
use mc683xx_board::description::Options;
use mc683xx_board::registry::Environment;
use mc683xx_board::registry::Registry;
use mc683xx_board::time::Time;
use mc683xx_mc68331::chip::Mc68331;

fn main() -> Result<(), Box<dyn Error>> {
    let mut registry = Registry::with_builtins();
    mc683xx_peripherals::register(&mut registry)?;
    let mut board = Board::<Mc68331>::read(
        Path::new("examples/boards/echo.toml"),
        &Options::default(),
        &registry,
        Environment::default(),
    )?;
    let run = board.run_until(Time::from_seconds(1))?;
    println!("{:?} at {:?}", run.stop, run.now);
    Ok(())
}
```

`Registry::with_builtins()` holds the built-in peripheral types,
`mc683xx_peripherals::register` adds the stock ones, and a host
registers its own peripheral models the same way.  A host can also
build the chip directly, without a board file, with
`mc683xx_mc68331::chip::Builder` (the host guide, below).

## Python: a board (`mc683xx`)

The same board, with bytes sent to its console and echoed back:

```python
import mc683xx

board = mc683xx.Board("examples/boards/echo.toml")
board.run_for(seconds=0.001)
for byte in b"hello":
    board.send("console", byte)
run = board.run_for(seconds=0.01)
print(run.stop, [frame for _, frame in board.take_received("console")])
```

## The runner (`mc683xx-run`)

```
cargo run --release -p mc683xx-run -- examples/boards/echo.toml --bind console=stdio --seconds 5
```

runs the board the file describes for five seconds of board time, its
`external.console` on standard input and output, and says on standard
error why it stopped.  `--trace bus-cycles,exceptions,pins,qspi` traces
the run; `--gdb 127.0.0.1:1234` waits at reset for gdb to run it.
Standard input that is a file or a pipe is read to its end when input
first goes in and fed a byte a turn, so the run is the same each time;
a terminal's is live.  `--bind NAME=stdout` writes an endpoint's
output as `stdio` does and reads nothing, for an endpoint that only
listens or to leave standard input to another binding.
`mc683xx-run --help` gives the rest; `--firmware` gives a board
another image.  [examples/README.md](https://github.com/charlieh0tel/mc683xx/blob/main/examples/README.md) describes the example
boards and their firmware.

## Documentation

- [Host guide](https://github.com/charlieh0tel/mc683xx/blob/main/docs/host-guide.md):
  embedding the chip in a Rust host: building a chip, running,
  peripherals, pins, hooks, save/restore, errors.
- [Board files](https://github.com/charlieh0tel/mc683xx/blob/main/docs/board-files.md):
  the format, the built-in peripheral types and the Rust API for peripheral
  models.
- [Stock peripherals](https://github.com/charlieh0tel/mc683xx/blob/main/docs/peripherals.md):
  the peripherals a board file names by part number, their parameters and
  ports.
- [Python](https://github.com/charlieh0tel/mc683xx/blob/main/docs/python.md):
  the same from Python.
- [gdb](https://github.com/charlieh0tel/mc683xx/blob/main/docs/gdb.md):
  debugging firmware with gdb.
- [Firmware under gdb](https://github.com/charlieh0tel/mc683xx/blob/main/docs/gdb-firmware.md):
  a recipe: the demo board's C firmware under gdb, source level.
- [Tracing](https://github.com/charlieh0tel/mc683xx/blob/main/docs/tracing.md):
  the run trace's format.
- [Performance](https://github.com/charlieh0tel/mc683xx/blob/main/docs/performance.md):
  measurements and the benchmarks.
- [PLAN.md](https://github.com/charlieh0tel/mc683xx/blob/main/PLAN.md):
  the architecture and the decisions behind it.
- [Releasing](https://github.com/charlieh0tel/mc683xx/blob/main/docs/releasing.md):
  versions and the release procedure.

## Workspace

The crates above are in `crates/`: `cpu32` (`mc683xx-cpu32`),
`mc683xx` (`mc683xx-modules`), `mc68331` (`mc683xx-mc68331`),
`mc68331-board` (`mc683xx-board`), `peripherals`
(`mc683xx-peripherals`), `gdb` (`mc683xx-gdb`) and `mc68331-run`
(`mc683xx-run`).  The other members are not on crates.io:

- `cpu32-py` (`crates/cpu32-py`): the Python package `mc683xx`, on PyPI.
- `examples`: the example boards and their firmware, and the
  documentation's examples run as tests ([examples/README.md](https://github.com/charlieh0tel/mc683xx/blob/main/examples/README.md)).
- `benches`: the criterion benchmarks.
- `cpu32-harness`, `cpu32-oracle`: test-only checks of the core
  against SingleStepTests' 68000 cases, Musashi, Unicorn and the
  firmware functions of [smartclock](https://github.com/charlieh0tel/smartclock)
  (HP's firmware images, not in this repository).
- `snapshot-golden`: test-only golden files of the snapshot formats.

## Building

```
make hooks   # once per clone
make ci      # fmt check, license copies, commit-msg hook, clippy, tests, rustdoc
make python      # the Python module in .venv, with its tests
make singlestep  # SingleStepTests m68000 (fetches about 140 MB once)
make oracle      # differential runs against Musashi (fetches it once)
make corpus SMARTCLOCK_FIRMWARE=...  # smartclock firmware functions
make boot-smoke SMARTCLOCK_FIRMWARE=...  # whole images from reset
make boot-diff SMARTCLOCK_FIRMWARE=... BOOT_DIFF_REFERENCE=...  # against smartclock-emu's trace
make bench       # benchmarks (docs/performance.md)
make deny        # dependencies' licenses and advisories (cargo-deny)
make smartclock-boards SMARTCLOCK_BOARDS=...  # smartclock's board files, stock peripherals real
make corpus-musashi SMARTCLOCK_FIRMWARE=...  # their results against Musashi
make corpus-unicorn SMARTCLOCK_FIRMWARE=...  # against Unicorn
make corpus-oracles SMARTCLOCK_FIRMWARE=...  # against both
make unicorn     # the Unicorn oracle's own tests and clippy
make gdb         # gdb-multiarch against the runner's --gdb, the demo board too
```

`make python` creates `.venv` with maturin, pytest and mypy the first
time; after it, `.venv/bin/python` imports `mc683xx`.

The Unicorn targets build Unicorn and QEMU's m68k from C through the
`unicorn-engine` crate the first time, which takes some minutes and
needs `cmake`, a C compiler (`cc` or `clang`) and `pkg-config`.
Unicorn is GPL-2.0; it is a test-only oracle, off by default and never
part of the shipped crates (`third_party/NOTICE`).

The manuals in `third_party/` are kept in Git LFS; `git lfs install`
before cloning, or `git lfs pull` after.

## Contributing

See [CONTRIBUTING.md](https://github.com/charlieh0tel/mc683xx/blob/main/CONTRIBUTING.md): changes follow the manuals and
datasheets in `third_party/`, cite them, and come with tests.

## Releases

One version for the crates and the Python package, semver from 0.1.0.
Release notes are on GitHub's
[releases](https://github.com/charlieh0tel/mc683xx/releases).

## Specification

The vendor manuals in `third_party/motorola/` are the specification;
see `third_party/NOTICE`.
