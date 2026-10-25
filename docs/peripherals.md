# Stock peripherals

The peripherals in `mc683xx-peripherals`, by the name a board file's
`[[peripheral]] type` gives them (PLAN.md, Stock peripherals).  The runner
and Python's `Board` register them after the host's own types.  Each
cites its datasheet, in `third_party/datasheets/`.  Board-file keys are
in `docs/board-files.md`.

The peripherals on the bus are 8-bit peripherals, and follow the board's one rule
for them (docs/board-files.md, attachments; SIMRM 5.3.3): on an 8-bit
port, or on one lane of a 16-bit port, each cycle is a byte; a word
cycle through `lane = "both"` of a 16-bit port carries the peripheral's byte
on DATA[15:8], a write's upper byte, a read's lower byte not driven.

Each peripheral saves and restores with the board (PLAN.md, State).  A
snapshot holds the parameters that shape the peripheral (a pull, a cascade,
references, a variant, a clock); a restore takes them from the loaded
peripheral and refuses, with `StateError::Mismatch`, a snapshot of one built
with others.  It refuses, with `StateError::Invalid`, state no run
could have saved, named under each peripheral.  Either way the peripheral is left
as it was.

Each shows its state to a front end (`Peripheral::inspect`,
`Board::inspect`; docs/host-guide.md, Front ends), read-only and
outside the snapshot: the latches their flip-flops, OE and outputs;
the buffer its inputs; the 74x164 its stages and CLR; the DACs their
latches or codes and each output's volts; the ADC0838 its frame's
phase, address or result, DO and /SE; the AT28C64B a summary of its
cells (`At28c64b::cells` gives them), its protection and the write
cycle running; the DUART each channel's modes, status, receive FIFO
and holding register, its interrupt registers, ports and
counter/timer, as of its last call.

## 74x374 and 74x373: octal latch

Eight D flip-flops the CPU writes through a select, their Q outputs on
nets: a board's lamp or LED register, or any output port.  `74x374`
is the edge-triggered register (SN74HC374 7.1, Table 7-1), `74x373`
the transparent latch (SN74HC373 6.1, Table 6-1).  Strobed by a write
cycle, both hold the byte written from the cycle's end, so one model
serves both: the '373's outputs following D while LE is high happens
within the cycle, which the bus does not resolve.

| Port | Kind | Direction | Meaning |
| --- | --- | --- | --- |
| `Q0`-`Q7` | logic | out | The flip-flops, `Qn` bit n of the byte written. |
| `OE` | logic | in | Output enable, active low. |

| Parameter | Default | Meaning |
| --- | --- | --- |
| `initial` | `0` | The flip-flops out of power-on, which the datasheets give as unknown (SN74HC373 Table 6-1, note 3). |
| `oe` | `"low"` | OE's level until a net drives it: `"low"` (tied low, Q driven) or `"high"` (tied high, Q released). |
| `pull` | `"up"` | A released Q's level on its net: `"up"` (high) or `"down"` (low). |

- Its D inputs take the byte of an 8-bit peripheral (above): a word cycle
  through `lane = "both"` writes its upper byte.  On the lower lane,
  `lane = "lower"`.
- A read drives nothing and reads 0, and leaves Q as it was: attach it
  `role = "write"`.
- It drives every Q at power-on, then each Q whose level changes, at
  the write's time.  Each Q is its own logic net, so a lamp, an LED or
  another peripheral's input attaches to one pin, and everything an observer
  wants (a lamp lighting) is a change on a net.
- OE high puts Q in the high-impedance state without touching the
  flip-flops (SN74HC374 7.1).  A logic net has no high impedance, so a
  released Q drives its `pull`.  Wire `OE` to a net (a port pin, a
  decoder's line) to switch it at run time; the net's first drive
  replaces `oe`.
- No reset pin: RESET leaves it alone.  Say so with `reset = []`.
- Snapshot: the flip-flops, OE, the pull and the levels last driven,
  format version 1.  Refused: another pull's, and levels last driven
  other than the outputs the rest gives.

```toml
[[peripheral]]
name = "u12"
type = "74x374"
attach = [{ select = "CS5", role = "write" }]
reset = []

[[net]]
name = "lamp-gps"
type = "logic"
from = "u12.Q4"
to = ["external.lamp-gps"]
```

## 74x244: octal buffer

Eight inputs the CPU reads through a select: a DIP switch bank, or
status lines from the board.  The SN74HC244 is two 4-bit buffers, each
passing its A inputs to its Y outputs while its OE is low (SN74HC244
7.1, Table 7-1); on the bus both OEs are the read strobe.

| Port | Kind | Direction | Meaning |
| --- | --- | --- | --- |
| `A0`-`A7` | logic | in | The inputs, `An` bit n of the byte read: `A0`-`A3` are 1A1-1A4, `A4`-`A7` 2A1-2A4. |
| `A` | bus | in | All eight at once, from the value's low byte. |

| Parameter | Default | Meaning |
| --- | --- | --- |
| `levels` | `0xff` | The inputs' levels until a net drives them, a one high: tied inputs, or a switch bank's positions (open, on pull-ups, high). |

- A read gives the byte on the inputs, on DATA[15:8] for a word cycle
  (above); the lower byte is not driven and reads 0.  A write reaches
  nothing: attach it `role = "read"`.  A
  latch and a buffer may share a select this way, one per role.
- A switch bank fixed for the run is `levels`, or `--set
  name.levels=...` on the runner.  One the host moves while the board
  runs is a net from an external: `external.switches` to `A`, the
  whole bank as one `Board::send(name, Signal::Bus(byte))`, or one
  external per switch to `An`.  Last drive wins, bit by bit.
- Snapshot: the inputs' levels, format version 1.

```toml
[[peripheral]]
name = "u13"
type = "74x244"
attach = [{ select = "CS5", role = "read" }]
parameters = { levels = 0xfe }

[[net]]
name = "switches"
type = "bus"
from = "external.switches"
to = ["u13.A"]
```

## 74x164: serial-in, parallel-out shift register

`74x164`: the 8-bit shift register (SN74HC164, SCLS115G) on the QSPI,
`cascade` of them chained, its parallel outputs one bus net: a QSPI
port for a parallel peripheral (an AD569's DB15-DB0).  MOSI is the serial
input (A and B, B tied high), SCK its CLK: each rising edge shifts the
serial bit into QA and each stage into the next, QH's bit leaving (9.1;
Table 1).  Chained, a chip's QH is the next chip's A.

| Port | Kind | Direction | Meaning |
| --- | --- | --- | --- |
| `Q` | bus | out | The parallel outputs: bit 8k + n is chip k's stage n (QA 0 to QH 7), chip 0 the one MOSI feeds. |
| `CLR` | logic | in | Clear, active low: every stage low while it is (Table 1). |

| Parameter | Default | Meaning |
| --- | --- | --- |
| `cascade` | `1` | The chips chained, 1 to 4 (a bus net is 32 bits): 2 is 16 bits. |
| `initial` | `0` | The stages out of power-on, driven on `Q` (bit 8k + n is chip k's stage n): the datasheet gives no power-on state; CLR held low clears it. |
| `clr` | `"high"` | CLR's level until a net drives it: `"high"` or `"low"`. |
| `miso` | `"qh"` | What answers on MISO: `"qh"`, the last chip's QH, or `"none"`, MISO not wired and undriven (all ones). |

- Attach it with `{ qspi = code, mask = pins }`: it shifts on the
  transfers that select it, as a board gating SCK (or MOSI) by a PCS
  wires it; `mask = 0` for a chain on SCK ungated, which shifts with
  every transfer beside the peripherals they select.  QH is always driven,
  and the board ANDs the replies of every selected peripheral on MISO
  (docs/board-files.md), so a chain whose QH is not wired to MISO
  takes `miso = "none"`, or its bits mask the other peripherals' replies.
- The last bit shifted is bit 0, so a 16-bit chain holds a 16-bit
  transfer as the QSPI sent it, most significant first: its `Q` bit n
  wired to DBn (in SPI modes 0, 2 and 3, below).
- QH is the last stage's output (9.2, Functional Block Diagram; Table
  1), always driven.
- SPI modes: the QSPI's SPI mode (CPOL and CPHA, QSMRM 4.3.1) decides on
  which edges the master changes MOSI and captures MISO; the peripheral
  declares its own edges and `mc683xx_board::spi` walks the transfer
  (docs/board-files.md, SPI edges).  A capture takes the level before
  its edge, so a side that acts on the edge the other changes its
  output on sees the bit before; MOSI before a transfer's first bit is
  taken as 0.  The '164 samples and shifts on CLK's rise.
  - Modes 0, 2 and 3: the rise takes each bit as the master sent it,
    and the master captures QH before the rise that shifts it, so a
    transfer as long as the chain reads back the word before it.
  - Mode 1: the rise leads each bit, and the master changes MOSI on
    it, so the chain takes each bit one late (the first, 0); the
    master captures QH on the fall after, one bit early: the word
    before shifted up one, the transfer's first bit in last.
- `Q` is driven at power-on and as each transfer ends when it changed:
  the outputs follow each clock on the peripheral, but the clocks within a
  transfer are not resolved.  With CLR low nothing shifts and QH is low.
- The datasheet gives no power-on state: every stage starts low.  Its
  only reset is CLR, so RESET changes nothing.
- Its QSPI calls are not deferrable: they drive `Q`.
- Snapshot: `cascade` and `miso`, the stages and CLR's level, format
  version 2.  Refused: another chain's, a stage past the chain, a
  stage set while CLR is low.

```toml
[[peripheral]]
name = "u40"
type = "74x164"
attach = [{ qspi = 0b1101 }]
parameters = { cascade = 2 }

[[net]]
name = "db"
type = "bus"
from = "u40.Q"
to = ["efc.db"]
```

## AD569

`ad569`: Analog Devices' AD569, a 16-bit voltage-output DAC with a
parallel input (AD569 datasheet, Rev. A).  DB15-DB0 pass through a
first rank of two byte latches, the 8 MSBs and the 8 LSBs, into the
16-bit DAC register, which VOUT follows (Digital Circuit Connections).
It is not on the QSPI: a board's QSPI reaches it through a shift
register on DB15-DB0 (`74x164`).

| Port | Kind | Direction | Meaning |
| --- | --- | --- | --- |
| `db` | bus | in | DB15-DB0, the value's low 16 bits; 0 until driven. |
| `cs` | logic | in | CS, active low: with it high nothing changes (Table I). |
| `hbe` | logic | in | HBE, active low: the 8 MSBs' latch. |
| `lbe` | logic | in | LBE, active low: the 8 LSBs' latch. |
| `ldac` | logic | in | LDAC, active low: the DAC register. |
| `out` | analog | out | VOUT, volts: -VREF + code x (+VREF - -VREF) / 65536, offset binary (Analog Circuit Connections). |

| Parameter | Default | Meaning |
| --- | --- | --- |
| `cs`, `hbe`, `lbe`, `ldac` | `"high"` | Each control's level until a net drives it: `"high"` or `"low"` (tied). |
| `initial` | `0` | The 16-bit word out of power-on, in the MSB and LSB latches and the DAC register, which the datasheet does not give.  The real part's power-on state is unknown; `0x8000` is mid-scale, VOUT halfway between the references. |
| `vref_negative` | `-5.0` | -VREF, volts: VOUT at 0000H. |
| `vref_positive` | `5.0` | +VREF, volts: VOUT a code past FFFFH would give.  The defaults are the specifications' conditions. |

- Table I in full: with CS low, HBE low makes the MSB latch
  transparent, LBE low the LSB latch and LDAC low the DAC register;
  "All four control inputs latches are level-triggered and active
  low", so a latch follows its input while enabled and holds from its
  enable's rise.  All four low is DB straight to VOUT.  CS high is no
  operation, LDAC included.
- An 8-bit bus loads HBE, LBE, then LDAC (Figure 20); a 16-bit one
  strobes HBE and LBE together, LDAC tied low (Figure 19).  With CS,
  HBE and LDAC tied low and LBE the strobe, the MSBs reach VOUT as DB
  changes and LBE's fall completes the word.
- VOUT is driven at power-on and whenever the DAC register changes.
  The datasheet gives no power-on state: the latches start holding
  `initial` (0, VOUT at -VREF, by default), then follow what the tied
  controls make transparent: a latch tied transparent takes DB at once,
  so behind a `74x164` it is the chain's own `initial` that VOUT starts
  at.  It has no reset input, so RESET changes nothing.
- Snapshot: the references, DB, the latches and the controls' levels,
  format version 3.  Refused: other references, a latch its controls
  make transparent that does not hold its input.  `initial` is not
  checked, as the octal latch's is not: a snapshot holds the latches.

```toml
[[peripheral]]
name = "efc"
type = "ad569"
parameters = { cs = "low", hbe = "low", ldac = "low" }

[[net]]
name = "efc-lbe"
type = "logic"
from = "chip.PGP0"
to = ["efc.lbe"]

[[net]]
name = "efc-out"
type = "analog"
from = "efc.out"
to = ["external.control"]
```

## ADC0838

`type = "adc0838"`: Texas Instruments' (formerly National
Semiconductor's) ADC0838, an 8-bit successive-approximation converter
with an 8-channel multiplexer and a serial interface, on the QSPI.
Datasheet: SNAS531B (`third_party/datasheets/ADC0838.pdf`).

```toml
[[peripheral]]
name = "adc"
type = "adc0838"
parameters = { vref_volts = 5.0 }
attach = [{ qspi = 0b1110 }]      # its /CS on PCS0

[[net]]
name = "sensor"
type = "analog"
from = "sensor.out"
to = ["adc.ch3"]
```

Ports:

| Port | Kind | Meaning |
| --- | --- | --- |
| `ch0`-`ch7` | analog input | The channels, in volts. |
| `com` | analog input | COM, the "-" input of each single-ended channel, in volts. |
| `se` | logic input | /SE: high holds D0 on DO after MSB-first data; low lets LSB-first data follow. |

An analog input no net drives reads 0 V, and `se` no net drives is
low, as a board that ties /SE to ground.

Parameters:

| Key | Type | Default | Meaning |
| --- | --- | --- | --- |
| `vref_volts` | number | 5.0 | VREF: 1 LSB is VREF / 256.  Positive. |
| `noise_volts` | number | 0 | The standard deviation, in volts, of noise added to each conversion's input; 0 for none. |

Behavior, by SNAS531B section:

- *Framing* (THE DIGITAL INTERFACE, steps 1 and 9): CS low starts a
  frame; CS high clears it, whatever it was doing.  The peripheral's QSPI
  select is its /CS, so a conversion that spans several queue entries
  needs CONT on all but the last.
- *Address* (step 3, Tables 2 and 3, Figure 21): DI is clocked on the
  rising edge (SPI modes, below).  Leading zeros are ignored; the
  first one is the start bit, then SGL/DIF, ODD/SIGN, SELECT 1 and
  SELECT 0.  Single-ended, channel 2 x SELECT + ODD against COM;
  differential, the pair SELECT names, its even channel "+" unless
  SIGN.
- *Output* (steps 4 to 8, Figure 21): DO changes on the falling edge.
  It is in TRI-STATE until the last address bit, then drives a leading
  zero for the clock of MUX settling, D7 to D0 MSB first, D1 to D7
  LSB first (D0 is not repeated), then low until CS goes high.  DI is
  ignored from the conversion on.  With /SE high DO holds D0, and the
  LSB-first data follows from the falling edge after /SE goes low.
- *Transfer function* (Reference Considerations; Optional
  Adjustments, Zero Error and Full-Scale): the code is "+" less "-"
  in LSBs, rounded to the nearest: 00 to 01 at 1/2 LSB, FE to FF at
  VREF less 1 1/2 LSB, clamped to 00 and FF.  "+" below "-" is 00
  (Multiplexer Addressing).

Choices where the datasheet is silent, or the board cannot say:

- SPI modes: the QSPI's SPI mode (CPOL and CPHA, QSMRM 4.3.1) decides on
  which edges the master changes MOSI and captures MISO; the peripheral
  declares its own edges and `mc683xx_board::spi` walks the transfer
  (docs/board-files.md, SPI edges).  A capture takes the level before
  its edge, so a side that acts on the edge the other changes its
  output on sees the bit before; MOSI before a transfer's first bit is
  taken as 0.  The ADC0838 samples DI on the rise and changes DO on the
  fall, DO in TRI-STATE until the fall after the address.
  - Modes 0 and 3, the edges it is made for: DI is taken as sent, and
    the master captures DO on the rise, as the fall before drove it.
    In mode 3 a transfer starts with a fall, so with /SE held high
    then let low between transfers the next transfer reads D1 first,
    where mode 0 reads D0 once more.
  - Mode 2: DI is taken as sent, but the master captures DO on the
    fall that changes it: the reply a clock late, a TRI-STATE 1 first.
  - Mode 1: the master changes DI on the rise the peripheral samples it on,
    so the start bit and every bit after come a clock late, and it
    captures DO on the fall after that rise: the reply as in mode 2.
- MISO reads 1 while DO is in TRI-STATE, as the board reads a QSPI no
  peripheral answers.  A board with a pull-down on MISO is not modeled.
- The inputs are sampled at the start of the transfer in which the
  address completes, not at the clock edge, and a change of `se`
  takes effect between transfers, never within one.
- SARS, the conversion status output, is not modeled.
- Noise is a normal deviate approximated as the sum of twelve
  uniforms less six (within six standard deviations), from the peripheral's
  own seed (`Environment::seed_for`, the run's `--seed`), drawn only
  when `noise_volts` is positive.  The sum is exact IEEE arithmetic,
  so the same seed gives the same codes on any host.
- Its calls are never deferred (`Peripheral::qspi_deferrable` is false): a
  conversion samples nets other peripherals drive, at the transfer's time.

The snapshot (format version 3) holds `vref_volts` and `noise_volts`,
the frame's state, /SE and the noise generator's state.  Refused:
other parameters, an address of more than four bits, DO past the
edge after the last.

## AT28C64B

`type = "at28c64b"`: an Atmel AT28C64B, an 8K x 8 parallel EEPROM
(Microchip DS20006432B).  It reads as a static RAM; a write starts a
write cycle that loads up to a 64-byte page and programs it, during
which reads poll; software data protection guards it.  It saves and
restores with the board: the snapshot (format version 2) holds tWC,
the cells, the protection and the write cycle running.  A restore
writes the cells through to the loaded peripheral's `file`, whichever
file the snapshot's peripheral had, and the protection to its sidecar
when the protection changes.  Refused: another
tWC, a cycle that starts with no sequence, loads data within a
sequence, or loads bytes on two pages.  A restore is otherwise made in
memory and never fails on the file: a file or sidecar that cannot be
written drops the file, as a run's failed write does, and
`file_error()` says why.

Its type is public, the one stock type that is
(`mc683xx_peripherals::at28c64b::At28c64b`), so a host reaches it by
type (`Board::peripheral::<At28c64b>`): `cells()`, `protected()`
and `file_error()`, the write-through failure a run cannot report
otherwise.

### Parameters

| Key | Type | Default | Meaning |
| --- | --- | --- | --- |
| `image` | path | -- | The cells' first contents, relative to the board file, at most 8192 bytes, padded with `0xff`; never written.  Without it the peripheral starts erased, all `0xff`. |
| `file` | path | -- | The file the cells live in, relative to the board file.  Made from `image` (or erased) if it does not exist, padded if short; refused if larger than the peripheral.  Every byte the peripheral programs is written to it as its write cycle ends, so a run's writes carry to the next.  The file itself is locked before it is read and held while the board is loaded: a second run refuses the file, and never sees it half made.  It is never renamed over, so nothing but the file (and `<file>.sdp`, below) remains after a run.  A new file is written and synced in a sibling of a unique name, then linked into place, which never replaces a file another run made; a short file is padded erased in place and synced.  A run killed while making a new file may leave that sibling, `<file>.<process>-<count>.partial`, and no file; one killed while padding leaves the file shorter, which the next run pads the same. |
| `write_cycle_us` | integer | `10000` | tWC, the write cycle's programming, in microseconds: 1 to 10000, the datasheet's maximum (Table 6-4). |
| `protected` | boolean | `false` | Software data protection at power-on of a new peripheral: the peripheral ships unprotected (6.6.2).  With a `file`, the protection persists, since power transitions do not disable it (6.6.2): the sidecar `<file>.sdp` holds it (`protected` or `unprotected`, a line), replaced as it changes, and a run with the sidecar starts from it, not from this.  A file without one (a new file, an 8 KiB dump) starts from this; delete the sidecar to start from this again. |

Without `file`, a run given a work directory (`mc683xx-run
--work-dir`, `Environment::work_dir`) keeps the cells in
`<work-dir>/<peripheral name>.eeprom`; a run with neither keeps nothing.
A run sets the file without editing the board file with `--set
eeprom.file=PATH`, a path relative to the board file as there.

### Ports

None: it is on the bus only.  Attach it to a select with its 13
address lines, `lines = 13` (A0 to A12 on ADDR0 to ADDR12; higher
address bits do not reach it).  It is an 8-bit peripheral (above): on an
8-bit port, or on a 16-bit port's lane.  A word cycle with both lanes
reads or writes one byte, at its address, on DATA[15:8].  It has no
reset input.

### Example

```toml
[[peripheral]]
name = "eeprom"
type = "at28c64b"
attach = [{ select = "CS9", lines = 13 }]
reset = []
parameters = { image = "eeprom.bin", file = "eeprom-working.bin", write_cycle_us = 5000 }
```

### Behavior

From the datasheet's section 6:

- **Byte and page writes (6.2, 6.3).**  A write starts a write cycle
  and loads its byte.  Each write within tBLC (150 us, Table 6-4) of
  the one before is loaded too; tBLC after the last, the peripheral programs
  the bytes loaded, and only those, for tWC.  A byte loaded twice keeps
  the second.  Writes while it programs are not taken.
- **Polling (6.4, 6.5).**  From the first write to the end of tWC
  every read, at any address, gives I/O7 the complement of the last
  byte written (DATA polling) and I/O6 toggling from read to read (the
  toggle bit).  Once tWC is over, reads give the cells.
- **Software data protection (6.6.2, 6.18, 6.19).**  `AA` to `1555`,
  `55` to `0AAA`, `A0` to `1555` at the start of a write cycle protect
  the peripheral as the cycle ends; `AA 55 80 AA 55 20` to `1555 0AAA 1555
  1555 0AAA 1555` unprotect it.  The sequences' bytes are not written;
  the bytes after them in the same cycle are.  While protected, a
  cycle without the enable sequence programs nothing, but runs its tWC
  and polls.

Where the datasheet is silent, the model chooses:

- I/O5 to I/O0 of a polling read are the last byte written's.
- A data byte on a page other than the cycle's first data byte's,
  which the datasheet forbids, is dropped; it still restarts tBLC.
- Bytes that start as a sequence and stop short of one are data.
- The first polling read's I/O6 is 1 ("beginning and ending state of
  I/O6 will vary", 6.24).

Not modeled: the extra 64 identification bytes and chip erase, which
take 12 V on A9 or OE (6.7, 6.17), and hardware data protection
(6.6.1).  A host crash may lose the last bytes written through to
`file`, which are not synced one by one, as a power loss during a
write cycle loses its bytes on the peripheral.

## MC144110

`mc144110`: Motorola's MC144110, six 6-bit DACs behind one serial
input (MC144110/D, Rev. 2), on the QSPI.

| Parameter | Default | Meaning |
| --- | --- | --- |
| `vdd` | `5.0` | VDD, volts: the top of the outputs' span (4.3; Figure 4).  5 V is the first supply Tables 2 and 3 characterize. |
| `vss` | `0.0` | VSS, volts: the output at code 0 ("usually ground", 4.3). |

| Port | Kind | Direction | Meaning |
| --- | --- | --- | --- |
| `out0`-`out5` | analog | out | R1 Out to R6 Out, volts: VSS + code x (VDD - VSS) / 64 (Figures 3 and 4). |

- Attach it with `{ qspi = code, mask = pins }`: its select is ENB.
  Bits shift into a 36-bit register while it is selected, most
  significant first (4.1, Din, CLK), and the frame's end, ENB's rise,
  latches the register into the six DACs (4.1, ENB).
- The last six bits shifted are DAC 1's (`out0`), the six before DAC
  2's, and so on (4.1, Din): send DAC 6 first.  Fewer than 36 bits
  set the low DACs and shift the rest up (Table 4).  Any transfer
  length works: three 12-bit entries under CONT are one frame.
- MISO is Dout, the register's last stage (4.2; Figure 1), always
  driven.
- SPI modes: the QSPI's SPI mode (CPOL and CPHA, QSMRM 4.3.1) decides on
  which edges the master changes MOSI and captures MISO; the peripheral
  declares its own edges and `mc683xx_board::spi` walks the transfer
  (docs/board-files.md, SPI edges).  A capture takes the level before
  its edge, so a side that acts on the edge the other changes its
  output on sees the bit before; MOSI before a transfer's first bit is
  taken as 0.  The MC144110 shifts on CLK's fall (4.1, CLK), Dout changing
  as it shifts.
  - Modes 0, 1 and 2: the fall takes each bit as the master sent it,
    and the master captures Dout before the fall that shifts it: what
    went in 36 bits before, a frame reading back the last bit for bit.
  - Mode 3: the fall leads each bit, and the master changes MOSI on
    it, so the register takes each bit one late (the first, 0); the
    master captures Dout on the rise after, one bit early.  A frame
    written in mode 2 and read in mode 3, as smartclock's self-test
    does (z3805a-3543b, 0x187aa), reads each 12-bit word shifted up
    one with the next word's first bit below it; the firmware shifts
    it back down.
- The outputs are driven at power-on and when a frame changes them.
  The datasheet gives no power-on state: here every DAC starts at
  code 0.  It has no reset input, so RESET changes nothing.
- Its QSPI calls are not deferrable: it drives its outputs from them.
- Snapshot: VDD and VSS, the shift register and the latch, format
  version 2.  Refused: other supplies, a bit past the 36.

```toml
[[peripheral]]
name = "trim"
type = "mc144110"
attach = [{ qspi = 0b1110 }]
parameters = { vdd = 12.0 }

[[net]]
name = "trim-1"
type = "analog"
from = "trim.out0"
to = ["external.trim"]
```

## MC68681

`type = "mc68681"`: a Motorola MC68681 DUART (MC68681 Advance
Information, September 1985); `type = "scc68692"`: Philips's SCC68692
(SCC68692 product data, 2004, and the Signetics product
specification of November 1990).  Two full-duplex serial channels with
three-character receive FIFOs, a 16-bit counter/timer, a 6-bit input
port, an 8-bit output port and a vectored interrupt.  One model serves
both; the board file names the peripheral it has, because the SCC68692
differs where firmware sees it:

| | `mc68681` | `scc68692` |
| --- | --- | --- |
| Command register bit 7 | ignored: the command is CR[6:4] (4.2.7.1) | the command is CR[7:4]: `8x` asserts RTSN (sets the channel's OPR bit), `9x` negates it, `Ax` and `Cx` turn timeout mode on and off, `Ex` and `Fx` power down (CRA only) |
| RESET | puts the counter/timer in timer mode, which runs on its preload (2.4, 3.5) | stops the counter/timer until a start command (RESETN; Counter/Timer) |

So `$A0` written to CRA resets the MC68681's receiver and turns on the
SCC68692's timeout mode.

### Parameters

| Key | Type | Default | Meaning |
| --- | --- | --- | --- |
| `x1_hz` | integer | `3686400` | X1/CLK's frequency in hertz: the crystal or clock every rate derives from (1.2).  The baud rate tables are for 3.6864 MHz; another crystal scales every rate, as on the peripheral. |

### Ports

| Port | Kind | Direction | Meaning |
| --- | --- | --- | --- |
| `RxDA`, `RxDB` | serial | in | The receivers' lines. |
| `TxDA`, `TxDB` | serial | out | The transmitters' lines: each frame at its start bit, with its baud and length. |
| `IP0`-`IP5` | logic | in | The input port, high until driven.  IP0 and IP1 are CTSA and CTSB. |
| `OP0`-`OP7` | logic | out | The output port, each pin the level it shows. |
| `IRQ` | logic | out | The interrupt output's level, low while asserted. |

Attach the registers to a select with RS1-RS4 on its address lines
(`lines = [1, 2, 3, 4]` for a peripheral at the odd addresses of a 16-bit
port): the peripheral takes RS4-RS1 from bits 3:0 of the address it sees.
It is an 8-bit peripheral, D7-D0 (above); on a 16-bit port it is one lane,
`lane = "lower"` where it is on DATA7-0.  An
`iack` attachment carries the interrupt acknowledge: it reads the IVR
while IRQ is asserted; otherwise the peripheral ignores IACK (2.11), so the
cycle ends in BERR, the spurious interrupt.  `irq` wires the interrupt
to an IRQ pin; the `IRQ` port is the same output for a net (an
open-drain line shared on a `wired-and` net, a port pin).

A channel's serial nets reach the host through an `external`
endpoint: `mc683xx-run board.toml --bind console=pty` (or `stdio`), or
Python's `Board.send("console", ...)` and `take_received("console")`.

### Example

A DUART whose DSACK reaches the chip as DSACK1 and which the firmware
addresses at odd addresses: the lower byte of a 16-bit port, whatever
the select's port field says (docs/board-files.md, `dsack`).  Its
acknowledge comes through CS10, which the firmware programs for CPU
space at the DUART's level (SIMRM 7.8.1).

```toml
[[peripheral]]
name = "duart"
type = "mc68681"
attach = [
  { select = "CS8", lane = "lower", lines = [1, 2, 3, 4] },  # RS1-RS4 on ADDR1-ADDR4, D7-D0 on DATA7-0
  { select = "CS10", role = "iack", lane = "lower" },        # the IVR on DATA7-0
]
irq = 2
dsack = "DSACK1"
parameters = { x1_hz = 3_686_400 }

[[net]]
name = "console-out"
type = "serial"
from = "duart.TxDB"
to = ["external.console"]

[[net]]
name = "console-in"
type = "serial"
from = "external.console"
to = ["duart.RxDB"]

[[net]]
name = "heartbeat-led"     # OP2: low while OPR[2] is set
type = "logic"
from = "duart.OP2"
to = ["external.heartbeat"]
```

`crates/peripherals/tests/mc68681.rs` runs this wiring on CS1: the
CPU transmits, takes the receiver's interrupt through the IVR and
echoes a character the host sends.

### Behavior

By the MC68681 datasheet's section, the SCC68692's by heading:

- **Transmitter (3.1, 4.2.7.3, 4.2.9.5, 4.2.9.6).**  A character
  written to an enabled transmitter starts at once if the shift
  register is free, else waits in the holding register; each goes out
  as one frame on TxD at its start bit: the data bits, the parity bit
  after them, and MR2's stop bits in sixteenths of a bit, at the rate
  the clock select gives.  TxRDY and TxEMT follow the holding and
  shift registers.  A disabled transmitter takes no character but
  sends what it holds; with MR2[5] it then clears its RTS bit, OPR[0]
  or OPR[1], a bit time after the last stop bit (4.2.2.2).  With
  MR2[4] a character waits for CTS (IP0 or IP1) low (4.2.2.3).  A
  break (command 6) goes out after what the transmitter holds, as
  back-to-back break frames a character time each, until command 7;
  then TxD is high a bit time before the next character (4.2.7.2).
- **Receiver (3.2, 4.2.9).**  A frame on RxD starts at its time, or as
  the frame before it ends, and is assembled at the center of its
  first stop bit at the receiver's own rate, when RxRDY sets: the
  receiver does not check the sender's baud.  An untimed frame (a
  host's) is a character long at the receiver's rate.  Parity is
  checked as MR1 says (PE); in multidrop mode the A/D bit is in PE,
  and a disabled receiver takes address characters only (3.4).  Three
  characters fill the FIFO; a fourth waits in the shift register and
  is lost to the next start bit, which sets OE.  Character and block
  error modes give SR[7:5] as 4.2.1.3 says.  A disabled receiver
  loses the character on the line (4.2.7.4).  With MR1[7] a start bit
  with the FIFO full negates RTS until a position frees (4.2.1.1).
- **Breaks (4.2.9.1).**  A break frame is one all-zero character with
  RB set; back-to-back break frames are one break.  The change in
  break (ISR[2], ISR[6]) sets as it is detected and again at its end.
- **Modes (4.2.2.1).**  Automatic echo sends each received frame on
  TxD as received, at its start bit, and cuts the CPU off the
  transmitter; remote loopback echoes without receiving; local
  loopback turns the transmitter into the receiver, at the transmit
  clock, with TxD held high and RxD ignored.
- **Clocks (Table 4-5, CSRA; Table 4-6).**  Each rate is X1/CLK over a
  16X clock divisor, the one Table 4-6's 16X clock gives at 3.6864
  MHz, in the set ACR[7] selects; clock select `D` is the timer's
  output as a 16X clock while it runs as a timer on X1/CLK.
- **Counter/timer (3.5, 4.2.13.2, 4.2.15.5).**  Counter mode counts
  X1/CLK / 16 or a transmitter's 1X clock down from CTUR:CTLR from the
  start command, sets ISR[3] and drives its output low at terminal
  count, and rolls over and counts on until the stop command, which
  clears ISR[3]; CUR and CLR read the count.  Timer mode runs from its
  first start command on (the MC68681's from RESET, below), inverts
  its output at each terminal count,
  sets ISR[3] at every second, and takes a new preload at the next
  terminal count; stop clears ISR[3] only.  It is counted in closed
  form, so a fast timer costs nothing unless its output is on OP3 or
  ISR[3] is unmasked and clear: then the board advances the peripheral at
  those terminal counts.
- **SCC68692 timeout mode (Timeout Mode).**  Command `Ax` stops the
  counter and makes the channel's receiver restart it at each
  character into the FIFO, as a counter whatever ACR[6] says; ISR[3]
  sets if the line pauses for the preload.  Start and stop commands do
  nothing until `Cx`.
- **Interrupts (1.3, 4.2.14, 4.2.15, 4.2.16).**  IRQ is asserted while
  an ISR bit is set that IMR passes.  ISR[7] sets when IP0-IP3 change,
  as ACR[3:0] enables, and reading IPCR clears it.  A change on
  IP0-IP3 is recognized after two samples of the 38.4 kHz clock
  (X1/CLK / 96) at the new level, 26 to 52 microseconds; a shorter
  pulse is not (1.6).
- **Input port (1.6, Table 4-5; SCC68692 1990, Input Port).**  A
  read of register 13 gives IP5-IP0 unlatched in bits 5:0, one in
  bit 7 and IACK's level in bit 6, on both peripherals: high, since a
  register read is no acknowledge.  The SCC68692's 2004 datasheet says
  bit 6 is IP2's level, which is already bit 2; the same sentence in
  the 1990 SCC68692 specification says IACKN, as do the SCC68681
  (2004), the SCN68681 (1998) and the MC68681.  No firmware in the
  smartclock corpus tests bit 6.
- **Output port (1.7, 4.2.11, 4.2.12).**  Each pin shows the
  complement of its OPR bit, set and reset by writes to registers 14
  and 15.  As OPCR selects, OP3 is the counter/timer output; OP4 and
  OP5 the complement of ISR[1] and ISR[5], OP6 and OP7 of TxRDYA and
  TxRDYB, whatever IMR says; these are open drain, released high and
  pulled low.  OP0 and OP1 are also high while a receiver holds RTS
  negated.  Each pin is its own logic net, driven at power-on and at
  each change, so lamps, LEDs and other peripherals attach to pins and an
  observer sees every change on the nets.
- **RESET (2.4; RESETN).**  IMR, ISR, OPR and OPCR clear, the IVR
  `$0F`, the output port high, both channels inactive with their FIFOs
  empty and mode register pointers at MR1.  The SCC68692's
  counter/timer stops until a start command; the MC68681's is in
  timer mode, running on its preload from RESET.  The modes, clock
  selects, the rest of ACR and the preload stay.  The datasheets
  differ here, and each peripheral follows its own:
  - MC68681 (1985): RESET "places the counter/timer in timer mode"
    (2.4); "In the timer mode, the counter/timer runs continuously and
    cannot be started or stopped by the CPU ... Upon power-up and after
    reset, the counter/timer operates in timer mode" (3.5).  Only its
    appendix, for the MC2681, says RESET "stops the counter/timer"
    (A.2.1).
  - SCC68681 (2004) and SCN68681 (1998): RESETN "stops the
    counter/timer" (pin table), yet "On power up and after reset, the
    timer/counter runs in timer mode and can only be restarted.
    Because it cannot be shut off or stopped ... OP3 should be masked
    off ... until the T/C is programmed" (Counter/Timer).  `mc68681`
    serving a 68681-class peripheral follows the second, the MC68681's.
  - SCC68692 (1990, 2004) and SC68C92 (1990): RESETN "stops the
    counter/timer", and "The C/T will not be running until it receives
    an initial 'Start Counter' command" (Counter/Timer).
  - XR68C681 (2000): RESET "stops the Counter/Timer" (pin
    description); the C/T "begins operation ... upon receipt of the
    address-trigger 'START COUNTER' command" (D.3.1).
- **Snapshot.**  The peripheral and `x1_hz`, every register, both
  channels' FIFOs, shift and holding registers and the frames on their
  lines, the counter/timer and the levels last driven: format version
  2.  Refused: the other peripheral's or another X1/CLK's; the mode register
  pointer past MR2, a FIFO past three, a character held while the
  FIFO has room, a count past a full turn, timeout mode on the MC68681
  or a third channel, input bits past IP5, a change of state pending
  on a pin that has none, levels past IRQ.

Where the datasheets are silent, the model chooses:

- A read of a command address (start counter, stop counter) or a
  reserved one (CRA, CRB) gives `$FF`.
- A read of an empty FIFO gives the last character read again (the
  SCC68692 says its FIFO is never cleared, only its pointers).
- A preload of 0 counts 65536; 1 counts as the minimum, 2.
- The timer's output starts high; the first start command inverts it.
- A transmitter disabled with a character in its holding register
  sends it, as MR2[5]'s description has it, though 3.1 speaks of the
  shift register only.
- Remote loopback cuts the CPU off the transmitter, as automatic echo
  does.
- The input pins are high until a net drives them.

Not modeled: external clocks on the input pins (IP2-IP5 as receiver,
transmitter or counter/timer clocks: a channel or counter/timer
selecting one has no clock, and sends, receives or counts nothing),
the clock outputs on OP2 and OP3 (the pin keeps its level), framing
errors and false start bits (a frame on a serial net carries no bad
stop bit), a break starting mid-character, DTACK's timing (no wait
states), and the SCC68692's power-down mode and BRG test reads (its
extended rates).
