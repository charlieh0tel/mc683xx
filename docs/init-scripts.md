# Init scripts

An init script readies a CPU32 for a load: the writes a board's reset
code would make (chip selects, the watchdog, the clock) and checks
that they took, one command a line, run on a halted target in
background mode.  `mc683xx_debug::script` parses and runs one over any
`mc683xx_target::Cpu32Target`: a BDM pod's on silicon, or the emulated
chip's.  The language is a format under semver (`docs/releasing.md`):
a script that parses in one release parses, to the same commands, in
every later release that is not a breaking one (`docs/releasing.md`,
Versions and tags).

```text
# Chip selects for RAM at 0x200000 (SIMRM 7), then check them.
write_memory.w 0xfffa44 0x3fff       # CSPAR0
write_memory.w 0xfffa4c 0x2003       # CSBAR0: 64 Kbytes
read_memory.w 0xfffa44 expect 0x3fff
write_register a7 0x20fffc
write_system_register vbr 0x200000
poll_memory.b 0x300001 mask 0x80 expect 0x80 timeout_ms 100 fc supervisor-data
```

## Lines

One command a line.  `#` starts a comment, to the end of the line;
a blank line, or one that is only a comment, is skipped.  Words are
separated by spaces or tabs.  Lines are numbered from 1, blank and
comment lines included, and an error names its line.

## Commands

| Command | What it does |
| --- | --- |
| `write_memory.SIZE ADDRESS VALUE [fc FC]` | Writes VALUE to memory in one access. |
| `write_register REGISTER VALUE` | Writes a data or address register: `d0`-`d7`, `a0`-`a7`. |
| `write_system_register REGISTER VALUE` | Writes a system register as BDM names them (CPU32RM 7.2.8): `rpc`, `pcc`, `atemp`, `far`, `vbr`, `sr`, `usp`, `ssp`, `sfc`, `dfc`.  The PC is `rpc`. |
| `read_memory.SIZE ADDRESS expect VALUE [mask M] [fc FC]` | Reads memory once, and fails unless the value read, masked with M, is VALUE. |
| `poll_memory.SIZE ADDRESS mask M expect VALUE timeout_ms N [fc FC]` | Reads memory until the value read, masked with M, is VALUE, and fails if it is not by N milliseconds of host time after the first read, which is always made. |

- A command's name is in lower case.  A register's name is in any
  case (`A7`, `a7`, `VBR`).
- SIZE is `b` (byte), `w` (word) or `l` (long), after a `.`: every
  memory command has one, a register command none.
- The clauses after the address (`expect`, `mask`, `timeout_ms`, `fc`)
  come in any order, each once, a keyword and the word after it.
  `mask` is every bit of the size unless given.
- `fc` names the address space of the access ([Function codes](#function-codes)):
  supervisor data unless given.

## Numbers

Every number (an address, a value, a mask, a timeout) is decimal,
hexadecimal after `0x` or binary after `0b`, the prefix in lower case
and the hexadecimal digits in either, with `_` allowed between two
digits: `1000`, `0xfffa44`, `0xff_fa44`, `0b1010_0000`.  There is no
sign, no `$`, and a decimal number has no leading zero but `0`
itself, so `010` is refused rather than read as octal.  It is the one
number syntax a person types to the workspace's commands
(`mc683xx_target::parse_integer`): gdb's `monitor` commands
and the runner's options take the same.

## Function codes

`fc` takes a function code (CPU32RM 5.3, Table 5-1) by name, in any
case, or by number, 0 to 7, in the number syntax above
(`mc683xx_target::FunctionCode`'s `FromStr`):

| Code | Name |
| --- | --- |
| 0 | `reserved0` |
| 1 | `user-data` |
| 2 | `user-program` |
| 3 | `reserved3` |
| 4 | `reserved4` |
| 5 | `supervisor-data` |
| 6 | `supervisor-program` |
| 7 | `cpu-space` |

A script prints a function code by its name, and leaves `fc` out
where it is `supervisor-data`.

## What parsing refuses

A script is checked whole before anything runs, and parsing refuses
what could not work:

- a command or a clause it does not know, a word where none is
  expected, a clause given twice, an operand or a clause's value
  missing;
- a word that is not a number, or one too large (past 32 bits);
- a value, a mask or an expected value wider than its access;
- an expected value with bits outside its mask, which no read could
  match;
- a word or long access at an odd address, an address error (CPU32RM
  6.2.3);
- a word that names no register, or no function code and is no number
  from 0 to 7.

A parse error names its line and why (`ParseScriptError`).

## Running

`Script::run(target)` runs the commands in order on a halted target,
up to the first that fails, and names its line, the command as a
script prints it, and why: the target's error, the value a read gave,
or the last value a poll read before its timeout (`ScriptError`).
`poll_memory` is the one use of host time in the workspace outside a
runner's pacing: a peripheral on silicon settles in real time.  The
emulated chip, halted, does not move at all, so a poll on it holds at
once or times out.

A script prints as its commands, a line each, which parse back to the
same commands; comments and blank lines are not kept, and a number
prints in hexadecimal.

```rust
use mc683xx_debug::script::Script;

let script: Script = "
    write_memory.w 0xfffa44 0x3fff   # CSPAR0
    poll_memory.b 0x300001 timeout_ms 100 mask 0x80 expect 0x80 fc 5
"
.parse()?;
assert_eq!(script.lines()[1].number, 3);
assert_eq!(
    script.to_string(),
    "write_memory.w 0x00fffa44 0x3fff\n\
     poll_memory.b 0x00300001 mask 0x80 expect 0x80 timeout_ms 100\n"
);
assert_eq!(script.to_string().parse::<Script>()?.lines().len(), 2);
# Ok::<(), mc683xx_debug::script::ParseScriptError>(())
```

The runner does not take a script yet; a host reads the file and
parses its text (the crate opens no files).
