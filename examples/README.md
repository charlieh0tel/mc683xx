# Examples

Example boards and their firmware, and the documentation's examples
run as tests (`mc683xx-examples`, not published).

| Path | What |
| --- | --- |
| `boards/` | Board files and their committed images |
| `firmware/demo/` | The demo board's C firmware |
| `src/` | `echo.bin`'s generator, in Rust |
| `tests/` | The host guide's and README's examples, the committed images' checks, the demo's console golden |

## Boards

Run from the repository's root; `mc683xx-run --help` gives the options.

### echo

`boards/echo.toml`: a boot ROM, RAM, a recording stub on CS1, a 10
MHz clock divided to 1 PPS on IC1, and the SCI on a console.  Its
firmware echoes each frame the SCI receives.

```text
mc683xx-run examples/boards/echo.toml --bind console=stdio
```

`echo.bin` is hand-assembled by `src/echo.rs`; `make echo-image`
rewrites it, and a test checks the committed file is current.

### demo

`boards/demo.toml`: echo's board without the stub, with OC2 counted.
Its firmware, in C, is a console over the SCI:

| Command | Does |
| --- | --- |
| `m` | Mandelbrot set, fixed point |
| `p` | Primes below 1000 |
| `t` | Uptime from the PIT's 1024 Hz ticks |
| `c` | The 1 PPS period on IC1, in TCNT counts |
| `x ADDR` | Hex dump of 64 bytes |
| `f` | An address error, reported from its frame and recovered |
| `h`, `?` | Help |

```text
mc683xx-run examples/boards/demo.toml --bind console=stdio
```

`demo.bin` and `demo.elf` (with symbols and line info) are committed;
`make demo-image` rebuilds them, reproducibly, with
`m68k-linux-gnu-gcc` (`-mcpu=cpu32`, no libgcc).
[docs/gdb-firmware.md](../docs/gdb-firmware.md) debugs it under gdb,
source level; `tests/demo_console.rs` pins its console
(`UPDATE_GOLDEN=1` rewrites the golden), and the `demo/mandelbrot`
benchmark runs its `m`.

## See also

- [docs/board-files.md](../docs/board-files.md): the board file format.
- [docs/peripherals.md](../docs/peripherals.md): the stock peripherals.
- [docs/host-guide.md](../docs/host-guide.md): the Rust host API.
