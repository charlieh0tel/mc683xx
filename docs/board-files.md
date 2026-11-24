# Board files

The reference for `board.toml`, version 1, which `mc683xx-board`
loads and `mc683xx-run` runs, and for the Rust API peripheral types are
written against.  Why it is shaped this way is in `PLAN.md`, Boards.
The rule: the file says what is wired where and how the chip is
configured, never what a peripheral does.

Version 1 is frozen from 1.0.0: from then a change that would make a
version 1 file mean something else, or refuse one this reference
accepts, is version 2.  Before 1.0.0 a minor release may still make
such a change and keep version 1, and its release notes say how to
convert a file (docs/releasing.md, Semver scope).  A minor release may
add keys to version 1, each marked here as added within it; a file
without one means what it did, and a file that uses one needs at least
the release that added it, as an earlier one refuses the key as
unknown.  Every 1.x release reads every version 1 file.  The built-in
types below and the stock types (`docs/peripherals.md`) are under the
same freeze: their names, parameters and ports.

Version 1 keeps one oddity once frozen, which a version 2, if there is
one, would drop: two keys for which ADDR lines a part sees (a
decoder's `address_bits`, which takes tied levels too, and an
attachment's `lines`), which version 2 would make one.  No version 2
is planned.

Two words, used one way throughout: a **peripheral** is anything
outside the chip that the board wires to it (a memory's command
logic, a UART, a decoder's targets, an oscillator on a net), and a
**module** is a block on the chip (the SIM, the QSM, the GPT).  A
peripheral the CPU reaches by address is a mapped peripheral; one on
the QSPI an SPI peripheral.

Every table refuses keys it does not know, but every table takes a
`note`, a string the loader ignores, for provenance and confidence: a
peripheral's `parameters` and a decoder's `alias` too, where it is the
loader's, never a parameter or a code.  Numbers are TOML integers,
with TOML's own syntax: hex (`0xff`), binary (`0b1110`), octal
(`0o17`), a sign and `_` between digits.  A number inside a string, as
a decoder's `alias` codes are, takes the one syntax the command line,
init scripts and gdb take (`mc683xx_target::integer::parse_integer`):
decimal, `0x` or `0b`, `_` between digits, no sign, no octal and no
decimal leading zero.  Names
(memories, decoders, peripherals, nets) are unique across the file, and
are not `chip` or `external` and hold no `.`.  Any memory, decoder,
peripheral, unconnected select or net may carry `option = "name"`, one of
`[board] options`: it is on the board only when the run enables that
option (`--option`; a host may enable every option the file declares,
`Options::all_enabled`), so one file states a board with and without an
option (the 58503A's front panel).  A net's endpoints on a peripheral an
option leaves off go with it, and a net left with no driver or no sink
goes too.

## An example

```toml
version = 1

[board]
name = "example"
title = "An example board"
images = ["firmware.bin"]          # the images it takes, the default first
options = ["panel"]

[chip]
type = "mc68331"          # the chip the board is built around
data = 0xffff            # DATA[15:0] as reset reads them
reference_hz = 32768     # MODCLK high: the synthesizer's reference
autovector = [5]         # the board asserts AVEC at these IPLs

[chip.pins]              # what the board drives on input pins at power-on
f = 0xff

[[memory]]
name = "flash_low"       # the array the core serves
image = "firmware.bin"   # relative to this file; the run's firmware if listed in [board] images
image_offset = 0x00000
size = 0x40000
attach = [
  { select = "CSBOOT", role = "read" },
  { select = "CS6", role = "write" },   # writes ignored: a ROM's
]

[[memory]]
name = "flash_high"
image = "firmware.bin"
image_offset = 0x40000   # the image's second half
size = 0x40000
attach = [{ select = "CS1", role = "read" }, { select = "CS7", role = "write" }]

[[memory]]
name = "ram"             # two 8-bit chips, one 16-bit memory
size = 0x10000
writable = true
attach = [
  { select = "CS0", role = "read" },
  { select = "CS2", lane = "upper" },
  { select = "CS3", lane = "lower" },
]

[[decoder]]
name = "u31"             # a 74AC138 on ADDR13 and ADDR14, A2 tied low
select = "CS5"
address_bits = [13, 14, "low"]
undecoded = "open-bus"

[[peripheral]]
name = "duart"
type = "recorder"
attach = [
  { select = "CS8", lines = [1, 2, 3, 4] },  # RS1-RS4 on ADDR1-ADDR4
  { select = "CS10", role = "iack" },
]
irq = 2
reset = ["power-on", "external", "system"]
dsack = "DSACK1"
parameters = { value = 0x0f }

[[peripheral]]
name = "latch"
type = "registers"
attach = [{ decoder = "u31", output = 0, role = "write", lines = 0 }]
parameters = { bytes = 1 }
reset = []               # a '574: no reset reaches it

[[peripheral]]
name = "panel"
type = "recorder"
option = "panel"
attach = [{ qspi = 0, mask = 0xf }]

[[peripheral]]
name = "reference"
type = "clock"
parameters = { hz = 10_000_000 }

[[peripheral]]
name = "pps"
type = "divider"
parameters = { divide = 10_000_000 }

[[unconnected]]
select = "CS4"           # programmed by the firmware, nothing wired

[[net]]
name = "ten-megahertz"
type = "clock"
from = "reference.out"
to = ["pps.in"]

[[net]]
name = "one-pps"
type = "edge"
from = "pps.out"
to = ["chip.PGP0", "chip.PAI"]

[[net]]
name = "done"            # open drain: a peripheral and PF3 both pull it low
type = "wired-and"
from = ["fpga.DONE", "chip.PF3"]
to = ["chip.PF3"]

[[net]]
name = "host-out"
type = "serial"
from = "chip.TXD"
to = ["external.host"]

[[net]]
name = "host-in"
type = "serial"
from = "external.host"
to = ["chip.RXD"]
```

(`fpga.DONE` needs a peripheral `fpga` whose type declares `DONE`.)

## Keys

### Top level

| Key | Type | Meaning |
| --- | --- | --- |
| `version` | integer | `1`.  Required; any other is refused. |
| `board` | table | The board's name, its images and options. |
| `chip` | table | The chip's configuration. |
| `memory` | array of tables | RAM, ROM and flash arrays the core serves. |
| `decoder` | array of tables | Address decoders on a select. |
| `peripheral` | array of tables | Peripherals from the registry. |
| `unconnected` | array of tables | Selects with nothing wired. |
| `net` | array of tables | Nets between ports and pins. |

### `[board]`

| Key | Type | Default | Meaning |
| --- | --- | --- | --- |
| `name` | string | -- | A short name: what a host chooses the file by. |
| `title` | string | -- | What the board is. |
| `images` | array of paths | `[]` | The firmware images the board takes, relative to the file, the default first.  A memory whose `image` is one of them takes the run's choice instead (`--firmware`). |
| `options` | array of strings | `[]` | The options a run may enable. |

### `[chip]`

The chip the board is built around and its own keys.  `type` names it;
the rest are the chip's, which it checks when the board loads (an
unknown key is an error with its line), and which the chip's
documentation lists.  The MC68331's (`type = "mc68331"`,
`mc683xx_board::mc68331`):

| Key | Type | Default | Meaning |
| --- | --- | --- | --- |
| `type` | string | required | The chip: `"mc68331"`.  A file without it is refused (`LoadError::NoChip`), and so is a board loaded as another chip (`LoadError::ChipType`). |
| `data` | integer, 16 bits | `0xffff` | DATA[15:0] as reset reads them (MC68331UM 4.6.3). |
| `reference_hz` | integer | `32768` | MODCLK high: the synthesizer's reference, 25000 to 50000 (MC68331UM 4.3.1; SIMRM 4.4.1). |
| `external_hz` | integer | -- | MODCLK low: EXTAL is the system clock.  Not with `reference_hz`. |
| `autovector` | array of IPLs 1-7 | `[]` | The board asserts AVEC (PE2) at each acknowledge of these (SIMRM 6.4). |
| `pins` | table | -- | The levels the board drives on input pins out of power-on: `e`, `f`, `qs`, `gp` (8-bit masks), `pai`, `pclk` (booleans). |

MODCLK is the clock key; AVEC is `autovector`; RESET's reach is each
peripheral's `reset`, and a peripheral's DSACK pins its `dsack`.  None of
these is a net endpoint.  The chip's pins on nets (`chip.PIN`) are
the chip's too, checked when the board loads: an unknown pin is
`LoadError::Endpoint` then, not when the file is read.

### `[[memory]]`

| Key | Type | Default | Meaning |
| --- | --- | --- | --- |
| `name` | string | required | Its name. |
| `image` | path | -- | Contents from a file, relative to the board file; the run may replace it (`--firmware`, `--image name=path`). |
| `image_offset` | integer | `0` | Where in the image its contents start: a bank's half of the image. |
| `size` | integer | the image's | Bytes; a whole number of 2-Kbyte pages, at most 16 Mbytes (what ADDR[23:0] reach).  Required without `image`; at least what it takes of the image. |
| `fill` | integer, 8 bits | `0` | The bytes past the image, or all of a memory without one. |
| `writable` | boolean | `false` | Writes reach it (RAM); otherwise they are ignored (ROM). |
| `attach` | array of attachments | required, but with `base` | Its selects: `select`, `role`, `lane` only; no `iack`. |
| `base` | integer | -- | Emulator-only: the memory at this fixed address, on no select, in place of `attach` (see below).  On a 2-Kbyte page boundary, the memory inside ADDR[23:0]. |
| `port` | `8` or `16` | `16` | Emulator-only, with `base` alone: its port width in bits.  A word on an 8-bit port takes two cycles. |
| `wait_states` | integer | fast | Emulator-only, with `base` alone: each cycle's wait states, 0 to 13, as a select's DSACK field gives them (MC68331UM D.3.26); absent, fast termination, two clocks. |
| `reload` | array of causes | `[]` | Emulator-only: the resets that put its contents back as the file gives them, as a peripheral's `reset` names them (`"external"`, `"power-on"`, `"watchdog"`, `"halt-monitor"`, `"system"`, `"loss-of-clock"`, `"test"`).  By default none: its contents survive every reset, as RAM's do.  Any memory, with `attach` or `base`. |

A select's block larger than the memory repeats it: there is no
mirroring key.  Every select of a memory reaches it from its first
byte.  The chip serves a memory in the core, so its attachments'
roles and lanes record the board's wiring and the SIM's decode decides
which cycles reach it: a ROM ignores writes, through a write select
too.  A memory has its select to itself.  Flash is an array and its
command logic: the array a memory (fast reads, code fetch), the
command cycles a peripheral on its own write selects that programs the
array through `Drive::write_memory`.  Its writes land as the
instruction that made them ends: the chip's run ends there.  A write
or a take made between quanta (`advance`, `input`) lands before the
next quantum and before the run returns, so a snapshot holds it.  While it programs, erases or is in autoselect mode it
takes the array (`Drive::take_memory("flash_low")`): from the end of
that cycle every cycle that reaches the memory reaches the peripheral
(`Peripheral::memory_read`, `memory_write`) and the core serves none of its
pages, until `Drive::release_memory`, so firmware polling the array
reads status.  Fetches stay on the fast path the rest of the time.

A memory with `base` is at a fixed address, on no select: **it is
emulator-only, as the chip has no such decode.**  It is for code and
data a run maps beside an image it must not change (rewritten
routines the image's calls are diverted to, a test stub), where on
hardware they would need a select the reset code programs.

```toml
[[memory]]
name = "patches"
note = "emulator-only: rewritten routines"
image = "patches.bin"
size = 0x10000
base = 0x600000
reload = ["external", "watchdog", "system"]   # fresh at every warm restart

[[memory]]
name = "patch_data"
size = 0x10000
writable = true
base = 0x6f0000                               # survives resets
```

It takes every program and data cycle at its addresses that no chip
select matches: a select the firmware programs over it wins, as on the
chip, and the internal modules win over both (MC68331UM 4.8); CPU-space
cycles never reach it.  Out of reset only CSBOOT decodes, its first
Mbyte (MC68331UM Table 4-24), so it answers from the first
instruction above that.  Its cycles end at once by default (16 bits,
fast termination, no DSACK); `port` and `wait_states` time it like
the memory it stands in for, a flash at 16 bits and one wait state,
say, and the bus monitor ends a cycle longer than its period, as it
does a select's.  A memory takes `attach` or `base`, not both, and
needs one; two with `base` may not overlap.  No peripheral takes it
(`Drive::take_memory` leaves it as it is).  A board snapshot holds its
bytes as every memory's.  `base` is set in the file only, not by the
run.

### `[[decoder]]`

| Key | Type | Default | Meaning |
| --- | --- | --- | --- |
| `name` | string | required | Its name. |
| `select` | pin | required | The select that enables it. |
| `address_bits` | array of ADDR lines, `"low"` or `"high"` | required | Its inputs, least significant first: an ADDR line number or a tied level.  Their levels are the code, which picks the output. |
| `undecoded` | `"open-bus"` or `"bus-error"` | `"open-bus"` | A code with no attachment: reads 0 and writes go nowhere, or BERR. |
| `alias` | table, code to output | `{}` | A code that selects another code's output, e.g. `{ "3" = 1 }`. |

A decoder gated by R/W is its outputs' `role`s: a write-only latch is
`role = "write"`.  An input not known is left out and said so in a
`note`.  A decoder has its select to itself.

### `[[peripheral]]`

| Key | Type | Default | Meaning |
| --- | --- | --- | --- |
| `name` | string | required | Its name; ports are `name.port`. |
| `type` | string | required | A type the registry knows. |
| `marking` | string | -- | Free-form text for people: the chip's marking or part number as fitted (`"U12, MC144110P"`).  Every peripheral takes it and no peripheral reads it, so it is not a parameter and the unread-parameter check never sees it.  `Description::marking`, `Board::marking` and Python's `Board.marking` give it back.  Added within version 1: a file without it means what it did. |
| `parameters` | table | `{}` | Checked by the type's constructor; a key it does not read is an error.  The run may set any of them over the file's (`--set name.key=value`, `Options::parameters`): each key replaces the file's, checked the same way. |
| `attach` | array of attachments | `[]` | Its selects, decoder outputs and QSPI codes.  None: a peripheral on nets only, or a model. |
| `irq` | IPL 1-7 | -- | Its interrupt output wired to that IRQ pin.  Several peripherals on one level are wired-OR, as open-drain /IRQ lines are. |
| `reset` | array of causes | all | The resets that reach it: `"external"`, `"power-on"`, `"watchdog"`, `"halt-monitor"`, `"system"`, `"loss-of-clock"`, `"test"` (SIMRM 8.2; MC68331UM D.3.4).  `[]` is legal: no reset reaches it. |
| `dsack` | `"DSACK0"`, `"DSACK1"`, `"both"` or `"open"` | -- | The DSACK pins it drives on an externally terminated select, `open` if not known.  Checked at each such cycle against PEPAR (SIMRM 9.1): with none of its pins assigned as DSACK, its DSACK cannot end the cycle and the bus monitor does (BERR).  Otherwise the port size it encodes (SIMRM Table 5-2: DSACK0 8 bits, DSACK1 or both 16) sizes the cycle, not the select's port field (SIMRM 5.3.3): a word to a `DSACK0` peripheral is two byte cycles.  `open`, or a select with its own DSACK, leaves the field to size it (SIMRM 7.6).  So a byte-wide peripheral whose DSACK reaches the chip as DSACK1 (the Z3805A's DUART: `both` pins, PEPAR assigning DSACK1 alone) is on a 16-bit port whatever the field says: wired to DATA7-0 and addressed at odd addresses, it states `lane = "lower"`, and a word cycle to it is one cycle, its lower byte the peripheral's.  A fault is an `Event::Dsack`, once a peripheral.  Two peripherals on one select's or decoder output's two lanes (`upper` and `lower`) whose pins encode different ports are refused, as the halves of one port are one width; `open` conflicts with nothing.  A read and a write peripheral sharing one may state different pins: each cycle takes its own peripheral's. |

### `[[unconnected]]`

| Key | Type | Meaning |
| --- | --- | --- |
| `select` | pin | A select the firmware programs with nothing wired to it, said so.  A cycle there ends as the chip ends one nothing answers: with the select's own DSACK it reads 0; with an external DSACK the bus monitor ends it in BERR. |

### Attachments

One of:

| Form | Meaning |
| --- | --- |
| `{ select = "CS0" }` | A chip-select pin: `CSBOOT`, `CS0`-`CS10`. |
| `{ decoder = "u31", output = 2 }` | A decoder's output. |
| `{ qspi = 0b1110, mask = 0xf }` | The QSPI, while PCS[3:0] under `mask` (default `0xf`) equal the code (QSMRM 4.3.6.3).  Several attachments may match the same levels (`mask = 0` matches every transfer): each gets the transfer. |
| `{ base = 0x800000, size = 0x100 }` | No select: the peripheral's own decode takes program and data cycles, user or supervisor, at `base` up to `base + size`, inside ADDR[23:0].  Board logic doing what the selects do not (a PAL on the address lines). |
| `{ iack = 4 }` | No select: the peripheral's own decode takes the interrupt acknowledges of IRQ level 1 to 7, its read giving the vector; the attachment's role is `iack`. |

A peripheral on no select drives DSACK itself, so it states `dsack`
(not `open`): its wait states time the cycle, the pins size it, and
PEPAR assigning none of them as DSACK leaves the cycle to the bus
monitor, as on an externally terminated select.  Its accesses carry no
select (`PeripheralAccess::select` is `None`).  It takes a cycle beside
a select that matches it, the first DSACK ending it (two at once
their pins together, DSACK1 from either a 16-bit port: SIMRM Table
5-2), and before a
fixed memory.  Peripherals with the same `base` and `size`, or the same
`iack`, share it as peripherals share a select, split by role and
lane, and drive the same pins; a window that only overlaps another
contends for the bus, as two devices decoding one address do.

and, for a select, a decoder output or a window:

| Key | Default | Meaning |
| --- | --- | --- |
| `role` | `"read-write"` | `"read"`, `"write"`, `"read-write"`, or `"iack"` (acknowledge cycles, CPU space). |
| `lane` | `"both"` | `"both"`, `"upper"` (DATA[15:8], even addresses) or `"lower"` (DATA[7:0], odd). |
| `lines` | `24` | The ADDR lines wired to the peripheral's address pins: a count, ADDR0 up, or a list of at most 24, the peripheral's A0 first (`[1, 2, 3, 4]`: ADDR1 to the peripheral's A0). |
| `offset` | `0` | Added to the address the peripheral sees. |

Several peripherals may share a select, or a decoder output, when their
attachments split it: no two take the same cycles on the same lane (a
read peripheral and a write peripheral, or an upper and a lower peripheral).  A word
cycle through a select with an upper and a lower peripheral is a byte access
to each, at the cycle's address.  A write through a `read` attachment
latches nothing; a read through a `write` one reads 0.

An 8-bit peripheral (a latch, a buffer, a DUART, an EEPROM, `registers`)
follows one rule (SIMRM 5.3.3): an 8-bit port's data is on DATA[15:8],
and a word to an 8-bit port is two byte cycles, as the SIM splits it.
So on an 8-bit port (the select's field, or a `DSACK0` peripheral) the
peripheral sees byte cycles only.  On a 16-bit port, `lane = "upper"` or
`"lower"` gives it byte cycles with its lane's byte; `lane = "both"`
gives it the word cycle as it is, and it takes the upper byte of a
write and drives its byte on DATA[15:8] of a read, the lower byte not
driven.  A peripheral on DATA7-0 of a 16-bit port states `lane = "lower"`.

### `[[net]]`

| Key | Type | Default | Meaning |
| --- | --- | --- | --- |
| `name` | string | required | Its name. |
| `type` | `"logic"`, `"edge"`, `"clock"`, `"serial"`, `"analog"`, `"bus"`, `"wired-and"` | required | What it carries: a level whose state matters; a level whose changes' times matter (1 PPS); a clock (frequency and exact phase, re-driven at each change of frequency); UART frames; a value; parallel lines as one word; or an open-drain logic net, pulled up. |
| `from` | endpoint, or array of endpoints | required | Its driver; several only on a `wired-and` net, whose level is the AND of theirs (a driver not driving is high). |
| `to` | endpoint, or array of endpoints | required | Its sinks. |

A net goes one way: a serial link is two nets, one each way.  On a
`wired-and` net an `in-out` port, or a chip port pin, may be both a
driver and a sink.

Endpoints:

| Endpoint | Types | Direction |
| --- | --- | --- |
| `peripheral.port` | as the type declares | as the type declares |
| `chip.PGP0`-`chip.PGP7`, `chip.PE0`-`PE7`, `chip.PF0`-`PF7`, `chip.PQS0`-`PQS7` | logic, edge, wired-and | either |
| `chip.PAI`, `chip.PCLK`, `chip.IRQ1`-`IRQ7` | logic, edge, wired-and | into the chip |
| `chip.RESET` | logic, edge, wired-and | either: low while a reset drives it; a peripheral driving it low resets the chip and holds it in reset, running no instruction while board time goes on, and as it goes high the chip's reset processing begins (MC68331UM 4.6.6).  On a `wired-and` net the chip senses the AND of the other drivers: its own reset output does not reset it (MC68331UM 4.6.6).  The host resets the chip by cause with `Board::reset_for` (host guide, Reset). |
| `chip.PC0`-`PC6`, `chip.PWMA`, `chip.PWMB` | logic, edge | out of the chip |
| `chip.ECLK` | clock | out of the chip: the system clock / 8 or / 16 (SYNCR EDIV), a rising edge at power-on (the manuals give no phase; our choice), retuned when the system clock or EDIV changes.  A peripheral on an ECLK-mode select (CSOR MODE, SIMRM 7.5.2) takes it as its E input and aligns its cycles to it; the chip charges each such cycle an ECLK period and a half. |
| `chip.RXD` | serial | into the chip |
| `chip.TXD` | serial | out of the chip |
| `external.name` | any | either; the runner or host binds it (`--bind name=stdio`).  The runner refuses input (stdio, tty, pty) to one that drives no serial net: `stdout`, `file:PATH` or `null` there |

A chip pin's name is taken in any case (`chip.pe0` is `chip.PE0`, the
same endpoint, so it drives or senses one net however it is written),
as a chip select's or a reset cause's.  PGP0 is IC1, PGP1 IC2, PGP2 IC3,
PGP3 to PGP6 OC1 to OC4, PGP7 IC4/OC5 (MC68331UM Figure 7-1).  A peripheral port takes a net of its own
kind only (a `wired-and` net is `logic`); a chip pin takes `logic` or
`edge`.  A strobe that a peripheral samples as a level (a latch enable such
as the AD569's /LBE) is a `logic` net: its low time matters, and an
`edge` net carries only the moments it changes.  Levels are electrical: an IRQ pin is asserted low.  A chip
pin's level while the chip does not drive it is high.  A serial
frame's time is its start bit's leading edge, and it carries its baud
and length (`signal::Serial`), so a receiver times its start or its
end.  `chip.TXD` drives each frame the SCI sends at its start bit, with
SCCR0's baud and its length, as other chip outputs do: a chip output
on a net that a peripheral or the chip senses ends the chip's run where it
changes, after the instruction or at the compare, PWM edge or frame
start that changed it, so a peripheral sensing it answers by the next
instruction, however the host cuts the run; an output nothing on
the board senses costs nothing.  A frame driven into
`chip.RXD` starts on RXD at its time (the SCI takes it between quanta,
at the first instruction boundary at or after it), or as the frames
before it end, and is received a frame time later at SCCR0's baud: the
SCI does not check the sender's baud.  `Board::externals` lists the
run-time endpoints, each with its nets' type and direction (`Out`
when it drives a net, the host sending; `In` when it is driven;
`InOut` both).  `Board::send(name, signal)` drives the endpoint's nets
whose type carries the value, and fails with `SendError` if it drives
none.  A frame from a run-time endpoint
(the runner's input) is untimed: baud and length zero.  An analog sink reads its input when it wants it
(`Drive::sample`): the driver's `Peripheral::analog_output` at that time, or else
the value last driven.  A peripheral may drive a clock net as often as its
frequency changes.

## Built-in peripheral types

| Type | Parameters | Ports | Does |
| --- | --- | --- | --- |
| `fixed` | `value` (0) | -- | Reads `value`; ignores writes. |
| `registers` | `bytes` (power of two to 16 Mbytes, 1), `vector` (none) | -- | A byte-addressed register file, an 8-bit peripheral (attachments); an IACK read gives `vector`, or BERR without one. |
| `recorder` | `value` (0) | -- | Reads `value`, logs every access.  Its snapshot is its value: the log is the host's record, and a restore leaves it. |
| `clock` | `hz`, `phase_ps` (0) | `out` clock | A steady clock, a rising edge at `phase_ps`. |
| `divider` | `divide`, `high` (half) | `in` clock, `out` edge | A rising edge at every `divide`th input edge, high for `high` of them, each change driven one ahead: exact while the input's frequency holds.  A drive cannot be withdrawn, so across a retune the change already driven keeps the old clock's time, and those after it follow the new. |
| `edge-counter` | -- | `in` edge | Counts rising edges, keeping the last one's time. |
| `dc-source` | `volts` | `out` analog | A fixed analog source: drives `volts` (any finite number; a missing, NaN or infinite one is a load error) from power-on and answers every sample with it.  Its snapshot is empty: its voltage is its parameter. |

Each shows its state to a front end (`Peripheral::inspect`,
docs/host-guide.md, Front ends): `fixed` its `value`; `registers` its
`registers` (bytes) and `vector`; `recorder` its `value` and the
`accesses` logged; `clock` its `clock` (a group: `frequency` in Hz,
`at`, when it took effect) and whether it has `driven` it; `divider`
`divide`, `high`, its input `clock` and the `pending` change (the
input `edge` it follows and its time, `at`); `edge-counter` its input's
`level`, `count` and `last` edge's time; `dc-source` its `volts`.  What has no value yet shows
as the text `none`.

A host builds one itself with its type's constructor, `None` for
parameters the file would refuse: `builtin::Fixed::new(value)`,
`RegisterFile::new(bytes, vector)`, `Recorder::new(value)`,
`Oscillator::new(clock)`, `Divider::new(divide, high)`,
`EdgeCounter::new()` and `DcSource::new(volts)`.

## The Rust API for peripheral types

The contract a host's peripheral models target (rustdoc in
`crates/mc68331-board`).  A crate of peripheral models depends on
`mc683xx-board`, `mc683xx-cpu32` (`BusError`, `PortSize`,
`StateError`), `mc683xx-modules` (`QspiClock`, `Frame`) and
`mc683xx-target`, every one at the same version, as no crate
re-exports another's types (`docs/host-guide.md`, Crates).

- `registry::Registry`: `register(name, ports, constructor)` for a
  type with fixed ports, `register_ported(name, constructor)` for one
  whose ports depend on its parameters (an FPGA's design, a latch's
  width; the constructor returns `registry::Constructed::new(peripheral,
  ports)`).  Ports are `registry::PortSpec`s: `PortSpec::input(name,
  kind)`, `PortSpec::output(name, kind)`, `PortSpec::in_out(name,
  kind)` (`signal::Direction::InOut`, an open-drain pin on a wired
  net), or `PortSpec::new(name, kind, direction)`;
  kinds `signal::Kind::{Logic, Edge, Clock, Serial, Analog, Bus}`.  The
  constructor takes a `registry::Context` (`Context::new(name,
  parameters, environment)` builds one to test it, and
  `Context::with_read(name, parameters, environment, read)` one whose
  `contents` come from `read`): the peripheral's `name`, its
  `parameters` (typed getters `integer`, `required_integer`, `boolean`,
  `float`, `string`, `value`, `table`, the values `registry::ParameterValue`s,
  the board's own type, not the TOML parser's; a key no getter read is an
  unknown-parameter error), `contents(key)` (the bytes of the file a
  path parameter names, relative to the board file: an image, which a
  host with no files supplies through `Board::load_with`), `path(key)`
  (that path, for a file the peripheral itself opens), and the run's
  `environment` (`seed`,
  `seed_for(name)`, `work_dir`; a test of a constructor through
  `Context::new` gives it the board directory with `set_board_dir`, read
  back by `board_dir()`,
  which a load sets itself).  `Parameters::unread()`
  is the first key no getter read, the load's unknown-parameter error,
  for a host's tests of its constructors, which build their `Parameters`
  with `Parameters::parse(toml_text)` (text that is not a TOML table an
  `description::SyntaxError`, its `line()` and `column()` from 1, as a board
  file's `LoadError::Syntax` carries; boxed, so a `LoadError`, and a
  host's error holding one, stays small).  A host holding TOML that sets
  a peripheral's parameters (`description::Options::parameters`) takes the
  table as `Parameters::parse(toml_text)?.into_table()`, and one value
  with `Options::set("PERIPHERAL.KEY=VALUE".parse()?)` (a
  `description::Setting`, its parse error a `ParseSettingError`,
  printing back as `PERIPHERAL.KEY=VALUE` with the value as TOML writes
  it); the TOML parser's types are
  not in the API, so a host that parsed its own TOML passes the text.
  `Registry::with_builtins()` starts from the types above.  A host's
  tests reach a type by name: `contains(name)`, `ports(name)` (`None`
  for a type whose ports depend on its parameters, or none registered),
  and `construct(name, &context) -> Result<Constructed,
  ConstructError>` (`ConstructError::Unknown` for no such type,
  `Parameter` for what the constructor refuses, which `into_parameter()`
  gives as `Ok`, the error itself `Err` otherwise, with no wildcard arm
  for the variants `#[non_exhaustive]` leaves room for; the unread-key check
  is `Parameters::unread`'s, as a load makes it).
  `Description::check_ports(&registry, &environment)` checks a board
  file's net ends at peripherals against their types' ports (name, type,
  direction) as a load does, without building the board: a type whose
  ports depend on its parameters is built to give them;
  `check_ports_with(&registry, &environment, read)` reads that type's
  path parameters through `read`, as `Board::load_with` does.
- `peripheral::Peripheral`: what the constructor builds.  Every method has a
  default, so a peripheral implements only the sides it is on; one with
  only net ports (an OCXO, a GPS engine, an oven) is as first-class
  as one on the bus.  The trait is `Any`: a host reaches its own peripheral's state
  (a UI's widget) by its type with `Board::peripheral::<T>(name)` (a `Ref`,
  `None` if it is not a `T`, `NoPeripheral` for a name the board lacks) or
  `peripheral_mut`, and a `Box<dyn Peripheral>` upcasts to `Box<dyn Any>`.
  `inspect()` gives what a front end shows of it, an
  `inspect::Inspection` of named fields (empty by default;
  `Board::inspect(name)`).  A call's time is a `time::Moment`, board
  time and the chip's system clock then: a call that takes a
  `peripheral::Drive` takes it from the drive (`drive.moment()`, or
  `drive.now()` for the time alone), and one without a drive takes it
  as its last argument, `moment` (`wait_states`, `dsack_port`,
  `set_qspi_clock`, `analog_output`, `next_event`).  No call has two
  times.  A chip-level model (`adapter`) takes `now: Cycle` last
  instead, as every chip-level callback with no cycle struct does
  (docs/host-guide.md, Board time).
  - Stop conditions, for a debugger and the emulator's alone
    ([docs/gdb.md](gdb.md), Stop conditions): `conditions()` lists
    what it offers (`condition::ConditionInfo::new(name, arguments,
    help)`, a `const fn` for a static list; none by default), `arm(condition, arguments)` arms one and gives
    its own `condition::ConditionToken` (`ConditionToken::from(number)`,
    `u32::from(token)` its number; or a `condition::ArmError`),
    and `disarm(token)` disarms it.  Each time an armed condition
    holds the peripheral calls `drive.fire(token)`, which stops the run
    as `drive.stop()` does and names the condition to the board.  It
    keeps its armed conditions out of `save`, and `restore` leaves them
    armed.
  - Bus: `read(access, drive)`, `write(access, value, drive)`,
    `wait_states(access, moment)` for an external DSACK, and
    `dsack_port(access, moment)`
    for a peripheral whose DSACK pins differ cycle by cycle (`Some(PortSize)`,
    16 bits on DSACK1, 8 on DSACK0 alone; `None` by default, its `dsack`
    saying), both asked of the peripheral that takes the cycle's
    direction, so on a select a read-only and a write-only peripheral
    share, a write's DSACK is the write-only one's.  A
    `peripheral::PeripheralAccess`
    carries `attachment` (the index in the peripheral's `attach`), its
    `role` and `lane`, `address` (`offset` plus ADDR on its `lines`),
    `size`, `function_code`, `write` (R/W low, set for `wait_states` and
    `dsack_port` too), `select`, the chip select that carried it (the
    decoder's, through a decoder; `None` on no select), and `horizon`;
    its time is the call's, when its instruction started.  A
    peripheral that samples a cycle at its end rather than at `now` (an
    FPGA's netlist behind wait states) returns true from
    `wants_horizon()`, asked once as the board is built, and its reads
    and writes carry `horizon`: the board time before which everything
    that could reach it has (the net changes waiting, the other
    peripherals' next events and undelivered drives, the next change
    of a chip output the board senses, `now` if one changed earlier in
    the instruction, the chip's run's end; not the drives a deferred
    QSPI peripheral has yet to make).  A sample before it, the
    peripheral may advance its own model there and answer; at or past
    it, an input dated before the sample (an edge within the cycle) may
    arrive after the access, and the peripheral answers as it chooses.
    The run's end is where the host cuts the run, so the answer must be
    the same either side of `horizon` unless an input lands before the
    sample, or the run depends on how it is cut.  `None` for the
    others, which costs the board nothing.
    `PeripheralAccess` is `#[non_exhaustive]`: a host's test builds one with
    `PeripheralAccess::new(select, address, size, function_code)`
    (`PeripheralAccess::unselected(address, size, function_code)`
    for a peripheral on no select), a read, and
    `.through(attachment, role, lane)` and `.writing(true)`.  The board layer turns each
    chip `mc683xx_cpu32::peripheral::BusCycle` into a `PeripheralAccess`.  An 8-bit
    peripheral takes its byte of a write with `access.written_byte(value)`
    and gives a read's with `access.read_byte(byte)`, the rule above
    (`[[peripheral]]` attachments).
  - A memory it has taken (`Drive::take_memory`): `memory_read(memory,
    access, drive)` and `memory_write(memory, access, value, drive)`,
    the access's `address` the offset in the memory and its
    `attachment` the memory's attachment; by default BERR and ignored.
  - IRQ: `interrupt_request()`.  RESET: `reset(drive)`, as the
    peripheral's `reset` scope says.  Power: `power_on(drive)`, every peripheral
    at time zero before the chip's power-on reset, whatever its scope,
    where a peripheral drives its outputs' first levels (a latch with `reset
    = []`); a port nothing drives has no value until its first drive.
  - QSPI: `transfer(transfer, drive)` (a `peripheral::Transfer`,
    `#[non_exhaustive]`, built with `Transfer::new(out, bits, clock)`,
    a frame's first through attachment 0, entry 0, PCS all low,
    and `.through(attachment)`, `.at_entry(entry)` and
    `.selecting(chip_selects)` for the rest: `attachment`, `entry`, the
    queue entry 0-15, `chip_selects`, `out`, the bits out, right-aligned,
    MSB first, `bits`, `clock`, the `QspiClock` it is shifted with
    (CPOL, CPHA, the SCK period), `starts_frame`, whether it is the
    first transfer since the attachment's select asserted
    (`.continuing()` builds a later one; `.shifting(out, bits)` another
    word; `.clocked(clock)` another clock, such as
    `QspiClock::at_reset(system_clock)`, SPCR0's out of reset); it ends at the drive's time) returns the bits in, all ones by
    default (`mc683xx_modules::peripheral::floating_miso`),
    `set_selected(attachment, selected, drive)`, `set_qspi_clock(clock,
    moment)`.  Framing: the board decodes PCS[3:0] by each attachment's
    `qspi` code and `mask`, and a peripheral sees `set_selected` only when its own
    decode changes, true as a frame starts and false as it ends; that
    is the canonical framing for a board peripheral.  Fan-out: a transfer
    reaches every attachment whose decode the PCS levels select, in
    file order (more than one peripheral may be selected at once,
    QSMRM 4.3.6.3; MISO is the master's one data input, 4.4.1), and
    MISO is the AND of their replies: an open line pulled high, a peripheral that
    leaves it undriven answering all ones, and a 0 from any driver
    winning a contention.  A peripheral whose output is not wired to MISO
    must say so (`74x164`'s `miso = "none"`), or its bits mask the
    others'; no match shifts all ones in.  The chip's
    `SpiPeripheral::set_chip_selects`, every change of the levels whatever
    they select, is for a host on the chip without this crate; on a
    board the decode is that peripheral.  A peripheral's RESET is its peripheral's
    `reset`, by its own scope; the QSPI's own reset scope plays no
    peripheral.
  - SPI edges (`mc683xx_board::spi`): what a slave shifts in and what
    the master captures depend on the SCK edges each acts on, so a
    peripheral that reads MOSI and drives MISO declares its datasheet's
    edges and shift logic and lets `spi::exchange(transfer, edges,
    slave)` clock it, rather than tabulate the modes.
    `spi::Edges::new(sample, change, first_bit_at_select)`: the edge
    (`spi::Edge::Rising` or `Falling`) it samples MOSI on, the edge it
    changes MISO on, and whether its first MISO bit is out when its
    select asserts (a register's last stage) or only from the frame's
    first change edge (MISO undriven until then).  `spi::Slave`:
    `sample(mosi)` at its sample edge, `change()` at its change edge
    (nothing by default, for an output that moves as it samples), and
    `miso()`, its level or `None` undriven.  `exchange` walks the
    transfer's edges in its mode (`spi::Mode::of(transfer.clock)`:
    the leading edge rising with CPOL 0; the master captures on the
    leading edge with CPHA 0, the trailing with CPHA 1, and changes
    MOSI on the other, QSMRM 4.3.1) and returns what the master
    captures.  At each edge every capture takes the level before it
    and every output changes after, so a slave sampling on the edge
    the master changes MOSI takes the level before the change (with
    CPHA 0 the bit meant for it, with CPHA 1 the bit before), and a master
    capturing on the edge the slave changes MISO the bit before.  MOSI
    before a transfer's first bit is PORTQS's MOSI bit, taken as 0, its
    value out of reset, since a peripheral does not see PORTQS; undriven MISO
    reads 1.  A peripheral that ignores MOSI, or leaves MISO undriven, can
    answer with the word alone and skip the bit-by-bit walk.
  - QSPI batching: `qspi_deferrable()`, asked once as the board is
    built, false by default.  The decode lets the QSPI batch its calls
    (`SpiPeripheral::deferrable`) only if every peripheral on the QSPI says
    true; one that does not keeps the run synced at each call to any
    of them, and so does a PCS pin (`chip.PQS3` to `chip.PQS6`) on a net
    a peripheral, the chip or an observer senses, since a batch shows
    PCS's changes only at its end.  A batch runs where the CPU next sees the QSPI (its
    interrupt, a read or write of the QSM) or at the sync point,
    before `Board::run_until` goes on to the board's events: each
    `transfer`, `set_selected` and `set_qspi_clock` keeps its order and its own
    time, but the CPU, board time and the other peripherals may be past
    it.  A peripheral may say true only if, in those calls, it drives no net
    and asks for no IRQ, RESET or stop (no `drive`, `drive_at`, `stop`,
    `write_memory`, `take_memory` or `release_memory`, and no change
    to `interrupt_request`); shares no state with a bus peripheral the CPU
    reads or writes during the exchange, its own bus attachments
    included; and reads neither board time nor another peripheral
    (`Drive::sample` included), nor relies on its net inputs having
    arrived (a select on a port pin, a reset line): nets the CPU drove
    during the quantum reach it only after.  Then the CPU sees the same
    results, SPIF at the same instruction, with far fewer quanta
    (docs/performance.md, QSPI queues).  The board file has no say: a
    model's calls are the model's to vouch for, not the wiring's, so
    it is code only (`SpiAdapter` and Python's `qspi_deferrable`).
  - Nets: `input(port, signal, drive)` for each value arriving on
    an input; `analog_output(port, moment)` for an analog output a sink
    reads at its time (`Drive::sample`).  Ports are indices in the declared list.
  - Time: `next_event(moment)`, `advance(drive)`: `advance` must move
    `next_event` past the drive's time or to `None`.  A peripheral advanced to one time
    1000 times running without that stalls the board, and `Board::run_until`
    fails with `RunError::Stalled`, naming it, rather than spin; several
    events at one time may each take an advance.  A call within the
    chip's run (a bus cycle, an acknowledge, a QSPI transfer, a reset)
    that drives a port before the run's end, writes a memory, or moves
    `next_event` before the run's end ends the chip's run after that
    instruction, and the board settles there: the drive reaches the
    chip by the next instruction, and a run is the same however the
    host quanta `Board::run_until`.  Each such call costs a sync.
  - The chip's system clock on board time (`time::TimeBase`) comes with
    every call, in its moment: `Moment::new(now, system_clock)`, with
    `now()`, `system_clock()` and `cycle()`, the chip's clock at it
    rounded up.  The chip runs at 8.388608 MHz until the firmware writes
    SYNCR, and the base moves right after the instruction that changes
    the frequency, so a peripheral keeps no base past a call.  A test
    builds its drive with one: `Drive::new(moment)`, or
    `Drive::with_sampler(moment, sampler)`.
  - State: `save()`, `restore(bytes)`; by default it cannot, and the
    board's save fails with `StateError::Peripheral`, which names the peripheral
    (`stub: the peripheral cannot save its state`); so does a peripheral that
    refuses its snapshot on a restore, which leaves the board
    unchanged: the board saves each peripheral first and puts back the
    ones it restored if another peripheral or the chip refuses, so a
    peripheral's `restore` must take its own `save`'s bytes back, and
    leave it unchanged when it refuses.
- `adapter::MappedAdapter::new(model)` and `adapter::SpiAdapter::new(model)`:
  a chip-level `mc683xx_cpu32::peripheral::MappedPeripheral` and `Timed` model, or a
  `SpiPeripheral`, as a peripheral with no hand-mapping.  The model sees a
  `BusCycle` with the access's select and address and its start in system
  clocks, and its times converted through the board's `TimeBase`; its
  `advance` runs at its `next_event`'s time.  A `SpiAdapter`'s model
  hears the board's framing as `set_chip_selects(0)` and
  `set_chip_selects(0xf)` and each transfer as a
  `QspiTransfer`, its time in system clocks.  `peripheral()` and `peripheral_mut()` give the model
  back (`Board::peripheral::<MappedAdapter<M>>`).  An adapted peripheral drives
  no nets, unless its model implements `adapter::Driver` and its adapter
  is made `.driving()`, as `adapter::Inspect` and `.inspecting()` show
  its state: then the model's `drive(&mut Drive)` runs after each call
  that may change the model (power-on, `reset`; a `MappedAdapter`'s
  `read`, `write`, `advance`; a `SpiAdapter`'s transfer, select and
  release) and drives the ports the type was registered with from what
  the model shows (a strobe's level), or stops the run.  The model implements `mc683xx_cpu32::cpu::state::Save` (`save()
  -> bytes`, `restore(bytes)`, in its own versioned format), so it is in
  the board's snapshot, its bytes inside the adapter's own versioned
  snapshot; one that cannot save is `mc683xx_cpu32::unsavable!`, and the
  board's save fails (`Unsupported`), naming it.  A `SpiAdapter`'s `qspi_deferrable` is its
  model's `deferrable`; the model's calls are then batched under the
  conditions above.  A model that implements `adapter::Inspect` (`inspect() ->
  Inspection`, empty by default), its adapter made `.inspecting()`
  (`MappedAdapter::new(model).inspecting()`),
  shows its state as the adapter's `Peripheral::inspect`; otherwise
  the adapter shows nothing.  `Inspect` is this crate's, so a model
  written against `mc683xx_modules::peripheral` alone shows nothing.
- `peripheral::Drive`, given to each call: `drive(port, signal)` and
  `drive_at(time, port, signal)` (now or later; a commitment);
  `sample(port)` (an analog input now); `moment()`, the call's
  `time::Moment`, and `now()`, its time; `write_memory(memory, offset,
  bytes)` (into a board memory by name, as the instruction ends, or
  between quanta before the next);
  `take_memory(memory)` and `release_memory(memory)` (its cycles to
  this peripheral, off the fast path, from the end of this cycle); `stop()`
  (end the run after this instruction).
- `signal::Signal`: `Level(bool)`, `Clock(time::Clock)`,
  `Serial(signal::Serial)`, `Analog(f64)`, `Bus(u32)`.  A
  `signal::Serial` (`#[non_exhaustive]`) is `frame` (`Frame::Data`, the
  bits as on the line, or `Frame::Break`), `baud` (a `Frequency`) and
  `length` (a `time::Span`, from the start bit to the end of the stop
  bits), timed from its start bit; build one with `Serial::new(frame,
  baud, bits)`, `Serial::timed(frame, baud, length)` or
  `Serial::untimed(frame)`, and `end(start)` gives its end.
- `time::TimeBase`: the chip's system clock on board time, `cycle(at)`
  (the clock at board time `at`) and `time(at)` (the board time of
  clock `at`), and `frequency()`.  Toward the chip a time rounds up, to
  the first clock at or after it; toward the board a clock rounds down,
  to its picosecond or the one before; so `cycle(time(c))` is `c`.  The
  board moves its base only where the frequency changes, so one base
  maps all the time between changes without drift.
  `TimeBase::steady(frequency)` is a base on board time alone, clock
  zero at power-on, never moved: for a part the chip does not clock
  (an oscillator, an oven, a GPS engine) that counts cycles of a rate
  of its own, which on the chip's base would fall behind at each reset.
- `time::Time` (picoseconds since power-on) and `time::Span` (a span
  of it, picoseconds: `Time + Span`, `Time - Time` a `Span`),
  `mc683xx_cpu32::frequency::Frequency`
  (nanohertz, `from_nanohertz` and `nanohertz`; the chip's clocks too:
  `TimeBase::frequency`),
  `time::Phase` (cycles, 10^21 units each, `from_units` and `units`) and `time::Clock`:
  `new(frequency, edge)`, `with_phase(frequency, at, phase)`,
  `phase(at)` (with `cycles()` and
  `fraction()`), `time(phase)` (the first picosecond at that phase),
  `edges(from, to)` (rising edges in (from, to]),
  `edge_after(from, n)`, `edge(number)`, and `retuned(frequency, at)`:
  a source whose frequency moves (an OCXO following its EFC) drives a
  retuned clock at each change, its phase carried across exactly, and
  a sink counts and times edges across the change with no rounding.
  A sink that queries back across changes keeps a
  `time::ClockHistory` (`new(span)`, `push(clock)` at each input;
  `Span::from_secs(64)` of history is plenty): `phase(at)`,
  `time(phase)`, `edges(from, to)`,
  `edge(number)`, `edge_after(from, n)` and `to_next_edge(at)`
  (a `Span` rounded up to a picosecond, and the exact fraction of a
  cycle) integrate
  each segment at its own frequency.  Edge numbers are absolute, so a
  sink counts from any edge it likes (after a sync step, from that
  edge).  Frequencies resolve 10^-16 of 10 MHz, and a phase stays
  exact over all of board time, 213 days; arithmetic is integers
  throughout.
