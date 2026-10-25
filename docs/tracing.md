# Tracing

What a board's run traces, as structured records (`mc683xx_board::trace`)
and as the text `mc683xx-run --trace` writes.  The text is a contract:
its first line names its version, a change that would make a line
mean something else is a new version, and the same run writes the
same bytes, so two runs' traces, or a trace and one another tool
writes in this format, diff line for line.  The records are the API;
the text is one output of them: `trace::Line`'s `Display`, the chip's
name and the record's `Display`.  `Board::set_tracing` chooses what is
recorded, `Board::watch_code` adds a code watch, and
`Board::take_trace` gives the records in cycle order.

## The text

One record a line, ASCII, on standard error:

```
CHIP: KIND key=value key=value ...
```

The chip's name (its `Chip::TYPE`, the board file's `[chip] type`:
`mc68331`) and `: `, the record's kind, then its fields as `key=value`,
separated by single spaces, in the order below.  The examples here
are an MC68331's.  Lines that start with `#` are comments: a trace
starts with

```
# mc683xx trace 3 chip=mc68331
```

the format, its version (`trace::VERSION`, `trace::header`) and the
chip, and may carry other comments (the board, the image).  A reader
skips them.

A serial frame's time, wherever one is reported (a `net` line, what
reaches a run-time endpoint), is its start bit's leading edge
(`docs/board-files.md`, nets).

Numbers: `cycle` is the chip's system clock count since power-on,
decimal.  `time` is board time in picoseconds, decimal.  Addresses,
registers and the PC are hexadecimal with `0x`, eight digits; SR four;
a value as wide as its size (`size=1`: two digits, `size=2`: four).

## Records

### `bus-cycle`: a peripheral answered a bus cycle

```
mc68331: bus-cycle cycle=42 pc=0x00015f88 access=read address=0x00200003 size=1 value=0x04
```

| Key | Meaning |
| --- | --- |
| `cycle` | When the cycle's instruction started. |
| `pc` | That instruction's address. |
| `access` | `read` or `write`. |
| `address` | The address on ADDR, as the CPU drove it. |
| `size` | Bytes: 1 or 2, after dynamic bus sizing. |
| `value` | The value read or written; `berr` for a read that ended in a bus error. |

Its PC comes from a code hook on every instruction, so tracing
peripherals about doubles a run's time.  Only cycles a board peripheral answers: RAM and ROM the core serves, and
the modules' registers, are not peripheral cycles.  A cycle two peripherals
answer (an upper and a lower peripheral) is one record.

### `exception`: the CPU entered an exception

```
mc68331: exception cycle=7 vector=66 pc=0x00000100 sr=0x2000 level=5
```

| Key | Meaning |
| --- | --- |
| `cycle` | When its frame was stacked: the run stops there for the record and goes on. |
| `vector` | Its vector number, decimal. |
| `pc` | The PC its frame stacked. |
| `sr` | The SR its frame stacked: the SR before it; `------` if the stack is not memory the core serves. |
| `level` | An interrupt's level; absent for any other exception. |

### `pin`: a chip pin on a net changed level

```
mc68331: pin cycle=1677 time=100000037 pin=PGP0 level=1
```

| Key | Meaning |
| --- | --- |
| `cycle` | The system clock the change takes effect at: a change into the chip rounds up to the first clock at or after its time; one out of the chip is the clock it changed at. |
| `time` | Its board time. |
| `pin` | The pin, by its endpoint name after `chip.`: `PGP0`-`PGP7` (IC1 to IC3 on PGP0 to PGP2, OC1 to OC4 on PGP3 to PGP6, IC4/OC5 on PGP7), `PAI`, `PCLK`, `PE0`-`PE7`, `PF0`-`PF7`, `PQS0`-`PQS7`, `PC0`-`PC6`, `PWMA`, `PWMB`, `IRQ1`-`IRQ7`, `RESET`. |
| `level` | `0` or `1`, electrical. |

Only pins on nets, and only changes of level.

### `qspi`: a QSPI transfer

```
mc68331: qspi cycle=90210 pcs=0x7 bits=16 out=0x8000 in=0xffff cpol=1 cpha=1
```

| Key | Meaning |
| --- | --- |
| `cycle` | When it started. |
| `pcs` | PCS[3:0]'s levels, one hex digit. |
| `bits` | Bits shifted, decimal. |
| `out` | What the QSPI shifted out, four digits. |
| `in` | What came back, four digits: all ones where no peripheral is selected. |
| `cpol` | SPCR0's CPOL for the transfer: `1` when SCK idles high (QSMRM 4.3.1). |
| `cpha` | SPCR0's CPHA: `1` when data changes on SCK's leading edge (QSMRM 4.3.1). |

### `code`: a watched instruction is about to run

```
mc68331: code cycle=5000 pc=0x00001234 sr=0x2700 d=00000000,...,00000000 a=00000000,...,0010fffc stack=00000000,...,00000000
```

| Key | Meaning |
| --- | --- |
| `cycle` | Now: the run stops before the instruction for the record and goes on. |
| `pc` | Its address. |
| `sr` | SR. |
| `d` | D0 to D7, eight digits each, comma-separated. |
| `a` | A0 to A7, the active stack pointer as A7. |
| `stack` | The eight longs from A7 up, as `d`; a long the core cannot read without a bus cycle is `--------`. |

### `position`: where the CPU is

```
mc68331: position cycle=838860 pc=0x00004e22 sr=0x2000 vbr=0x00000000 a7=0x0010fff0 instructions=212345
```

`cycle`, `pc`, `sr`, `vbr`, `a7`, and `instructions`, the instructions
run since power-on.  The runner writes one every `--position SECONDS`
of board time.

### `stopped`: the run ended

```
mc68331: stopped cycle=1677721 instructions=420001 reason=budget
```

`cycle`, `instructions`, and `reason`: `budget`, `halted`, `hook` or
`requested` (`other` for a stop a later core adds).

## Net lines

`Trace::nets`, apart from the records and in board-time order, each a
`trace::NetRecord` whose `Display` is the line:

```
mc68331: net time=1000000 name=pps level=1
```

`time` and `name`, then the value's fields, `signal::Signal`'s
`Display`:

| Value | Fields |
| --- | --- |
| A level | `level=0` or `level=1`. |
| A clock | `clock_at` (picoseconds), `frequency` (nanohertz), `phase` (phase units, `time::PHASE_UNITS` a cycle), decimal. |
| A serial frame | `data=0x41`, two digits, or `data=break`; `baud` (nanohertz) and `length` (picoseconds), 0 for an untimed frame. |
| An analog value | `analog=1.5`. |
| A bus word | `bus=0x0000abcd`. |

The runner writes what reaches a binding that takes no serial data the
same way: `mc68331: external name=NAME time=PS` and the value's fields.
