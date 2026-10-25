# Firmware under gdb

A recipe: a C firmware on a board, under gdb, source level.  The
worked example is the demo board, `examples/boards/demo.toml`, and its
firmware in `examples/firmware/demo`.  `docs/gdb.md` is the reference
(registers, memory, stops), `docs/board-files.md` the board file;
`make gdb` runs this session as a check (`scripts/gdb-demo.sh`).

## Build

`demo.bin` and `demo.elf` (DWARF, its sources mapped to `.`) are
committed in `examples/boards`.  To rebuild them (needs
`m68k-linux-gnu-gcc`; the build is reproducible):

```text
make demo-image
```

Your own board wires its image the same way: `[[memory]]` with
`image = "rom.bin"` on the select the reset code programs (`CSBOOT`
for the boot ROM), and `[board] images = ["rom.bin"]` so `--firmware
build/rom.bin` replaces it.  A board with a peripheral of its own
needs a host that registers it (`docs/host-guide.md`) and serves gdb
itself (`mc683xx_gdb::stub`, or `serve_gdb` in Python).

## Run

```text
mc683xx-run examples/boards/demo.toml --bind console=stdio \
    --seconds 60 --gdb 127.0.0.1:1234
```

The runner runs the power-on reset and waits, the PC at `reset`,
until gdb connects; gdb then runs the board.  `--seconds` counts the
time gdb runs it too.  On `detach` the run goes on to its end; on
`kill` it ends with exit status 4.  The console and gdb must not share
a terminal: bind the console to a pty (`--bind console=pty`, which
standard error names; `picocom` or `screen` on it), or feed
it a file (`< input.txt`; the demo's commands are `p`, `f`, `t`...,
one a line, with `--input-after 0.01` so they land after the SCI is
up).

## Attach

```text
$ gdb-multiarch examples/boards/demo.elf
(gdb) directory examples/firmware/demo
(gdb) target remote 127.0.0.1:1234
reset () at ./crt0.S:40
```

With the ELF gdb takes its byte order from it: no `set endian`.  The
build maps its directory to `.`, hence `directory`.

## Moves

Break in an ISR.  At its first instruction SP points at the exception
frame, SR then PC (CPU32RM 6.4); `bt` cannot unwind it (below), so
read the interrupted PC from the frame:

```text
(gdb) break pit_isr
(gdb) continue
Breakpoint 1, pit_isr () at main.c:80
(gdb) x/a $sp+2
0x10ffe6:       0x4aa <put_string+26>
(gdb) info registers system
```

Watch the tick count.  The stop is after the instruction that wrote
it:

```text
(gdb) delete
(gdb) watch ticks
(gdb) continue
Old value = 0
New value = 1
0x00000a8c in pit_isr () at main.c:82
```

Step C.  A command breaks once a byte arrives (`p` here); `next` and
`finish` step over the interrupts that land meanwhile:

```text
(gdb) break primes
(gdb) continue
(gdb) bt
#0  primes () at main.c:248
#1  0x00000f78 in command_loop () at main.c:412
#2  0x000010dc in main () at main.c:478
(gdb) next
(gdb) finish
```

Catch a fault.  Every unexpected vector is `fault_entry`; the `f`
command reads a word at an odd address.  The frame is format $C
(CPU32RM 6.4, Figure 6-6): SR, PC, format and vector offset, fault
address:

```text
(gdb) break fault_entry
(gdb) continue
Breakpoint 4, fault_entry () at ./crt0.S:77
(gdb) x/6hx $sp
0x10ff7c:       0x2014  0x0000  0x08d4  0xc00c  0x0010  0x0001
(gdb) x/a $sp+2
0x10ff7e:       0x8d4 <fault+98>
```

Reset: `monitor reset`, then `maintenance flush register-cache`.

## Raw image

Without an ELF gdb takes the host's byte order: `set endian big`
before `target remote`, or the PC at `reset` reads `0x40000`:

```text
$ gdb-multiarch -nx
(gdb) set endian big
(gdb) target remote 127.0.0.1:1234
(gdb) display/i $pc
(gdb) break *0xa7c
(gdb) continue
=> 0xa7c:       movel %d0,%sp@-
```

## Attach later

Run to the point of interest, save, and attach to the restore: the
runner restores the snapshot before it waits.

```text
mc683xx-run examples/boards/demo.toml --seconds 0.5 --save boot.snap
mc683xx-run examples/boards/demo.toml --restore boot.snap --gdb 127.0.0.1:1234
```

gdb then attaches with the demo waiting for input, `ticks` 511:

```text
#0  get_char () at main.c:177
#1  read_line (line=0x10ffbc "") at main.c:191
#2  command_loop () at main.c:404
```

## Unwinding an exception

GCC's call frame information for an `interrupt_handler` describes a
call's frame, the return address the long at SP, so `bt` in an ISR
takes SR and the PC's high word for it (`#1 0x20040000 in ?? ()`).  This is the
compiler's, not the stub's; the frame is at SP as the CPU wrote it.

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
