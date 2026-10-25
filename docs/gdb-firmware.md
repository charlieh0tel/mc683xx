# Firmware under gdb

A recipe: a real ROM image on a board, under gdb.  `docs/gdb.md` is
the reference (registers, memory, stops); `docs/board-files.md` the
board file.

## 1. Board file

The board file wires the image to its memories: each `[[memory]]`
with `image = "rom.bin"` (and `image_offset` where two parts share
one image), on the select the firmware's reset code programs
(`CSBOOT` for the boot ROM).  `[board] images` names the images
`--firmware` replaces, so one board file runs any build:

```toml
[board]
images = ["rom.bin"]
```

The stock runner builds only the stock peripherals
(`docs/peripherals.md`).  A board with one of its own needs a host
that registers it (`docs/host-guide.md`) and serves gdb itself
(`mc683xx_gdb::stub`, or `serve_gdb` in Python).

## 2. Runner

```text
mc683xx-run board.toml --firmware build/rom.bin \
    --bind console=stdio --seconds 60 --gdb 127.0.0.1:1234
```

It runs the power-on reset and waits, the PC at the reset vector's,
until gdb connects.  gdb then runs the board.  `--seconds` counts
the time gdb runs too.  On detach the run continues to its end.  On
`kill` it ends with exit status 4.  `--trace exceptions` and
`--save` still apply.

To skip the boot, save a snapshot at the point of interest and
attach there instead: the runner restores it before it waits.

```text
mc683xx-run board.toml --seconds 5 --save boot.snap
mc683xx-run board.toml --restore boot.snap --gdb 127.0.0.1:1234
```

## 3. gdb

`gdb-multiarch`, or `m68k-elf-gdb`.  With an ELF of the build:

```text
$ gdb-multiarch build/rom.elf
(gdb) target remote 127.0.0.1:1234
```

With a raw image only:

```text
$ gdb-multiarch -nx
(gdb) set architecture m68k:cpu32
(gdb) target remote 127.0.0.1:1234
(gdb) display/i $pc
```

Then as usual: `break *0x1234`, `watch *(short *)0xffe00`, `stepi`,
`continue`, Ctrl-C, `info registers system` (`vbr`, `usp`, `ssp`,
`sfc`, `dfc`), `monitor reset` then `flushregs`, `detach`.

## Things that differ from a BDM pod

- Breakpoints are hooks, not patched opcodes: any number, in ROM,
  invisible to a checksum.
- Memory reads see the decode as the firmware has set it up: out of
  reset only CSBOOT answers.  Module registers read without side
  effects.  Writes to module registers are refused.  Peripheral
  memory gives `E0e` both ways: the debugger never makes a bus
  cycle.
- A watchpoint stops after the instruction that hit it.  A long on
  the chip is two word cycles.
- A double bus fault shows as `SIGBUS`, and the CPU stays halted until
  `monitor reset`.
- Sessions are deterministic but for where a Ctrl-C lands: the same
  breakpoints stop at the same place, board time included.
