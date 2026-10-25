//! A whole smartclock image's boot on the MC68331 compared with a
//! reference trace of the same boot from smartclock-emu's Unicorn
//! backend (github.com/charlieh0tel/smartclock, `docs/emulator.md`,
//! "Tracing"): the peripheral cycles both make, in order and by value, and
//! the exceptions.  The
//! trace's header names the image and the EEPROM dump; the Z3805A 3543B
//! and the 58503A 3704 have been compared.  It runs twice: once with
//! the boot smoke test's stubs on the board's selects, to find the
//! first place the stubs answer differently from smartclock's models;
//! then replaying, every board peripheral read but the EEPROM's answered
//! with the value the reference read there, in its order, the QSPI's
//! transfers and the modules' input pins likewise, and the DUART's
//! interrupt and IC1's edge made where the reference took them, so
//! that the chip follows the reference past its peripherals.  The EEPROM
//! is a model in both runs, from the dump.  The 58503A's front panel's
//! display changes, formed from the latch and the QSPI as smartclock
//! forms them, are compared with the reference's `display` records.
//! It reports the divergences
//! and checks nothing: Unicorn's time is an estimate, so whatever
//! depends on time (the chip's modules' polls, their interrupts) is
//! expected to differ.  The images and the traces are not in this
//! repository: `SMARTCLOCK_FIRMWARE` names smartclock's
//! `third_party/firmware`, `BOOT_DIFF_REFERENCE` the trace; without
//! either the test is skipped.  `make boot-diff` runs it.

use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::collections::VecDeque;
use std::env;
use std::fmt;
use std::fs;
use std::path::Path;
use std::path::PathBuf;

use mc683xx_cpu32::bus::BusError;
use mc683xx_cpu32::cpu::Stop;
use mc683xx_cpu32::cycles::Cycles;
use mc683xx_cpu32::peripheral::BusCycle;
use mc683xx_cpu32::peripheral::MappedPeripheral;
use mc683xx_cpu32::peripheral::PeripheralId;
use mc683xx_mc68331::chip::Mc68331;
use mc683xx_mc68331::chip::MemoryId;
use mc683xx_mc68331::event::Event;
use mc683xx_modules::peripheral::QspiTransfer;
use mc683xx_modules::peripheral::SpiPeripheral;
use mc683xx_modules::peripheral::Timed;

use common::FIRMWARE;
use common::board;

mod common;

/// The variable naming the reference trace; its port output is beside
/// it, `.port.out` for `.txt`.
const REFERENCE: &str = "BOOT_DIFF_REFERENCE";
/// The variable giving the emulated seconds each run lasts.
const SECONDS: &str = "BOOT_DIFF_SECONDS";
/// The variable naming a directory the chip's traces are written to,
/// in the reference's format, if set.
const OUTPUT: &str = "BOOT_DIFF_OUTPUT";
/// Emulated seconds a run lasts past the reference's unless `SECONDS`
/// says otherwise: the chip's instructions take longer than Unicorn's
/// estimates.
const SECONDS_PAST_REFERENCE: f64 = 1.0;
/// Where the reference's header names its image and EEPROM dump.
const FIRMWARE_PATH: &str = "third_party/firmware/";
/// The EEPROM dumps' directory under it.
const EEPROM_DIRECTORY: &str = "eeprom/";
/// smartclock-emu's board clock (the reference's header).
const BOARD_HZ: f64 = 16_777_216.0;
/// The record kinds' prefix in the reference.
const PREFIX: &str = "smartclock-emu: ";
/// The reference's kind for a bus cycle a board peripheral answered:
/// smartclock-emu's own, which `docs/tracing.md` calls `bus-cycle`.  The
/// chip's traces this test writes keep the reference's dialect, so the
/// two diff line for line.
const BUS_CYCLE: &str = "device";
/// Clocks run at a time while the CPU is stopped and waits.
const STOPPED_QUANTUM: Cycles = Cycles(256);

/// The board's peripheral regions as the reference traces them (its
/// header): the DUART on CS8, CS5's latch, switch, FPGA port and EFC
/// strobe, the EEPROM on CS9 and CS4 (restart.md, "The chip selects").
const BOARD_PERIPHERAL: std::ops::Range<u32> = 0x20_0000..0x60_0000;
/// The MC68331's module registers (MC68331UM 1.4, the module map).
const MODULES: u32 = 0xff_f000;
/// The EEPROM's window on CS9 (restart.md; AT28C64B, 8 K x 8).
const EEPROM: std::ops::Range<u32> = 0x40_0000..0x40_2000;
/// The EEPROM's byte load cycle time, tBLC, 150 us (AT28C64B, AC Write
/// Characteristics), at the Z3805A's 16.777216 MHz.
const EEPROM_LOAD: Cycles = Cycles(2517);
/// Its write cycle time, tWC, 5 ms.
const EEPROM_WRITE: Cycles = Cycles(83_886);
/// DATA polling's bit, I/O7.
const IO7: u8 = 1 << 7;
/// Toggle bit's, I/O6.
const IO6: u8 = 1 << 6;
/// An erased byte, beyond the dump.
const ERASED: u8 = 0xff;
/// The DUART's SRA and SRB, which read as `TxRDY` and `TxEMT` with no
/// character received where the reference has nothing to replay, as
/// the boot smoke test has them (MC68681, SRA and SRB).
const STATUS_REGISTERS: [(u32, u8); 2] = [(0x20_0003, 0x0c), (0x20_0013, 0x0c)];
/// The DUART's IVR, whose value answers its interrupt acknowledge
/// (MC68681, "Interrupt vector register (IVR)").
const DUART_IVR: u32 = 0x20_0019;
/// The DUART's TBB, channel B's transmit holding register: the host
/// port (MC68681, register map).
const DUART_TBB: u32 = 0x20_0017;
/// The DUART's interrupt level on the Z3805A and the 58503A: CS10 is
/// programmed for its acknowledge at IPL 2 (the boot code's CSOR10,
/// $2BC4; SIMRM 7.10.3).
const DUART_LEVEL: u8 = 2;
/// The receive RAM, REC.RAM (QSMRM 4.3.5, $FFFD00 to $FFFD1F).
const RECEIVE_RAM: std::ops::Range<u32> = 0xff_fd00..0xff_fd20;
/// The command RAM, COMD.RAM (QSMRM 4.3.5, $FFFD40 to $FFFD4F).
const COMMAND_RAM: std::ops::Range<u32> = 0xff_fd40..0xff_fd50;
/// The queue's entries (QSMRM 4.3.5).
const QUEUE_ENTRIES: u8 = 16;
/// SPCR1, with SPE (QSMRM 4.3.2, $FFFC1A).
const SPCR1: u32 = 0xff_fc1a;
/// SPCR1's SPE.
const SPE: u32 = 1 << 15;
/// SPCR2, with NEWQP (QSMRM 4.3.3, $FFFC1C).
const SPCR2: u32 = 0xff_fc1c;
/// SPCR2's NEWQP.
const NEWQP: u32 = 0x0f;
/// SR's interrupt masks inside the board's interrupt handlers: the
/// DUART's 2, the QSPI's 4, the PIT's 5, the GPT's 6, each handler at
/// its own level (CPU32RM 6.3.2).
const HANDLER_MASKS: std::ops::RangeInclusive<u16> = 2..=6;
/// The registers whose input pins the replay drives as the reference
/// read them: PORTE0, PORTF0 (MC68331UM D.2.12, D.2.15), PORTGP (GPTRM
/// 2.3, $YFF907) and PACTL, its PAIS and PCLKS the PAI and PCLK pins
/// (GPTRM 4.1.1, $YFF90C).
const PORTS: [u32; 4] = [0xff_fa11, 0xff_fa19, 0xff_f907, 0xff_f90c];
/// PORTGP's index in `PORTS`.
const GP: usize = 2;
/// PORTGP's IC1 pin (GPTRM 2.3).
const IC1: u8 = 1 << 0;
/// The GPT's interrupt level on the Z3805A and 58503A: ICR's IRL, 6
/// (the boot code's ICR, $0650; GPTRM 7.1.1).
const GPT_LEVEL: u8 = 6;
/// PACTL's PAIS and PCLKS (GPTRM 4.1.1).
const PACTL_PAIS: u8 = 1 << 7;
/// PACTL's PCLKS.
const PACTL_PCLKS: u8 = 1 << 3;
/// The 58503A's latch, whose bits 6 and 7 frame the front panel's
/// commands (smartclock `docs/firmware/58503a-3704.md`, "The front
/// panel").
const LATCH: u32 = 0x30_0000;
/// The latch's bit that frames a command while low.
const LATCH_FRAME: u8 = 1 << 6;
/// The latch's bit that holds the panel in reset while low.
const LATCH_RUN: u8 = 1 << 7;
/// PCS3 to PCS0 in a command RAM entry and on the pins (QSMRM 4.3.5).
const PCS_MASK: u8 = 0x0f;
/// The front panel's PCS code: all four low (58503a-3704.md).
const FRONT_PANEL_SELECTS: u8 = 0;
/// The panel's commands that change the display, by their first byte
/// (58503a-3704.md): the key lamps then the text, the lamps, the text.
const COMMAND_LAMPS_AND_TEXT: u8 = 0x01;
/// The key lamps alone.
const COMMAND_LAMPS: u8 = 0x02;
/// The text alone.
const COMMAND_TEXT: u8 = 0x04;
/// The reference's display records' kind.
const DISPLAY: &str = "display cycle=";
/// Display records shown, side by side.
const DISPLAYS_SHOWN: usize = 40;
/// Records that must agree for the alignment to take up again.
const RESYNC: usize = 4;
/// How far ahead on either side the alignment looks to take up again.
const LOOKAHEAD: usize = 300;
/// Addresses shown whose writes differ.
const WRITES_SHOWN: usize = 12;
/// Reference lines shown of those the replay did not use.
const UNUSED_SHOWN: usize = 24;
/// Divergences either side of one whose records it may have moved to.
const MOVED_WITHIN: usize = 8;
/// Divergences shown in full of a kind but `Kind::Board`.
const KINDS_SHOWN: usize = 2;
/// Divergences shown in full of `Kind::Board`.
const DIVERGENCES_SHOWN: usize = 12;
/// Records shown before a divergence, and of each side's part of it.
const CONTEXT: usize = 6;

/// One record of a trace, the reference's or the chip's, as compared.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum Record {
    /// A peripheral or module register cycle.
    Access {
        /// Whether it wrote.
        write: bool,
        /// Its address.
        address: u32,
        /// Bytes: 1 or 2.  A long of the reference is two words.
        size: u8,
        /// The value read or written.
        value: u32,
    },
    /// An exception entered.
    Exception {
        /// Its vector.
        vector: u8,
        /// The instruction's PC for a trap or fault, the next one's for
        /// an interrupt.
        pc: u32,
        /// SR before it.
        sr: u16,
        /// The level, for an interrupt.
        level: Option<u8>,
    },
}

impl Record {
    /// Whether it is an interrupt, whose place depends on time.
    const fn is_interrupt(&self) -> bool {
        matches!(self, Self::Exception { level: Some(_), .. })
    }

    /// Whether it is an exception taken in an interrupt's handler: SR's
    /// mask at one of the board's interrupt levels, not 0 or 7.
    fn in_handler(&self) -> bool {
        matches!(self, Self::Exception { sr, .. } if HANDLER_MASKS.contains(&(*sr >> 8 & 7)))
    }

    /// Whether it is a cycle on a board peripheral, not a module.
    const fn is_board(&self) -> bool {
        matches!(self, Self::Access { address, .. } if *address < MODULES)
    }
}

/// A record with where it came from.
#[derive(Debug, Clone, Copy)]
struct Traced {
    /// What happened.
    record: Record,
    /// The PC of the instruction that made it.
    pc: u32,
    /// The reference's line, or the chip's instruction count.
    origin: u64,
    /// When: the reference's board cycle, or the chip's system clock's.
    cycle: u64,
    /// Whether it is half of a reference long, which Unicorn reads
    /// aligned to four bytes: two longs for a long at an address of 2
    /// mod 4, a word of each not the CPU32's.
    split: bool,
}

impl fmt::Display for Traced {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.record {
            Record::Access {
                write,
                address,
                size,
                value,
            } => write!(
                formatter,
                "#{} {BUS_CYCLE} cycle={} pc={:#010x} access={} address={address:#010x} size={size} value={value:#0width$x}",
                self.origin,
                self.cycle,
                self.pc,
                if write { "write" } else { "read" },
                width = 2 + 2 * usize::from(size),
            ),
            Record::Exception {
                vector,
                pc,
                sr,
                level,
            } => {
                write!(
                    formatter,
                    "#{} exception cycle={} vector={vector} pc={pc:#010x} sr={sr:#06x}",
                    self.origin, self.cycle
                )?;
                if let Some(level) = level {
                    write!(formatter, " level={level}")?;
                }
                Ok(())
            }
        }
    }
}

/// What the reference's header and end say of its run.
#[derive(Debug)]
struct ReferenceRun {
    /// The image, under `FIRMWARE`.
    image: String,
    /// The EEPROM dump, likewise.
    eeprom: String,
    /// The board seconds it lasted.
    seconds: f64,
    /// The board seconds it lasted past its last write to TBB.
    after_port: f64,
}

impl ReferenceRun {
    /// The run `text`'s header names: its image the first file under
    /// `FIRMWARE_PATH`, its EEPROM the first under its `eeprom/`.
    fn of(text: &str) -> Self {
        let files: Vec<&str> = text
            .lines()
            .take_while(|line| line.starts_with('#'))
            .flat_map(|line| line.split(FIRMWARE_PATH).skip(1))
            .filter_map(|rest| {
                rest.split(|c: char| c.is_whitespace() || c == ')' || c == '>')
                    .next()
            })
            .collect();
        let image = files
            .iter()
            .find(|file| !file.starts_with(EEPROM_DIRECTORY))
            .expect("the header names the image");
        let eeprom = files
            .iter()
            .find(|file| file.starts_with(EEPROM_DIRECTORY))
            .expect("the header names the EEPROM's dump");
        let stopped = text
            .lines()
            .rev()
            .find_map(|line| line.strip_prefix(PREFIX)?.strip_prefix("stopped cycle="))
            .and_then(|cycle| cycle.trim().parse::<u64>().ok())
            .unwrap_or_default();
        let tbb = format!("access=write address={DUART_TBB:#010x}");
        let last_port = text
            .lines()
            .rev()
            .find(|line| line.contains(&tbb))
            .and_then(|line| {
                line.split_whitespace()
                    .find_map(|word| word.strip_prefix("cycle="))
            })
            .and_then(|cycle| cycle.parse::<u64>().ok())
            .unwrap_or(stopped);
        #[expect(clippy::cast_precision_loss, reason = "cycles of seconds")]
        let (seconds, after_port) = (
            stopped as f64 / BOARD_HZ,
            stopped.saturating_sub(last_port) as f64 / BOARD_HZ,
        );
        Self {
            image: (*image).to_owned(),
            eeprom: (*eeprom).to_owned(),
            seconds,
            after_port,
        }
    }
}

/// The value of `key=` in a record's fields, as a number.
fn field(fields: &BTreeMap<&str, &str>, key: &str) -> Option<u64> {
    let text = fields.get(key)?;
    text.strip_prefix("0x").map_or_else(
        || text.parse().ok(),
        |hex| u64::from_str_radix(hex, 16).ok(),
    )
}

/// The reference's records, a long split into its words.
fn parse_reference(text: &str) -> Vec<Traced> {
    let mut records = Vec::new();
    for (number, line) in text.lines().enumerate() {
        let Some(rest) = line.strip_prefix(PREFIX) else {
            continue;
        };
        let mut words = rest.split_whitespace();
        let kind = words.next().unwrap_or_default();
        let fields: BTreeMap<&str, &str> = words.filter_map(|word| word.split_once('=')).collect();
        let origin = number as u64 + 1;
        let pc = field(&fields, "pc").unwrap_or_default() as u32;
        let cycle = field(&fields, "cycle").unwrap_or_default();
        match kind {
            BUS_CYCLE => {
                let (Some(address), Some(size), Some(value)) = (
                    field(&fields, "address"),
                    field(&fields, "size"),
                    field(&fields, "value"),
                ) else {
                    panic!("line {origin}: a bus-cycle record without its fields");
                };
                let write = fields.get("access") == Some(&"write");
                let address = address as u32;
                let value = value as u32;
                let words: Vec<(u32, u8, u32)> = if size == 4 {
                    vec![(address, 2, value >> 16), (address + 2, 2, value & 0xffff)]
                } else {
                    vec![(address, size as u8, value)]
                };
                for (address, size, value) in words {
                    records.push(Traced {
                        record: Record::Access {
                            write,
                            address,
                            size,
                            value,
                        },
                        pc,
                        origin,
                        cycle,
                        split: size == 2 && fields.get("size") == Some(&"4"),
                    });
                }
            }
            "exception" => {
                let (Some(vector), Some(sr)) = (field(&fields, "vector"), field(&fields, "sr"))
                else {
                    panic!("line {origin}: an exception record without its fields");
                };
                records.push(Traced {
                    record: Record::Exception {
                        vector: vector as u8,
                        pc,
                        sr: sr as u16,
                        level: field(&fields, "level").map(|level| level as u8),
                    },
                    pc,
                    origin,
                    cycle,
                    split: false,
                });
            }
            _ => {}
        }
    }
    records
}

/// The interrupts at `level` the replay requests, each at the
/// reference's line of the last board peripheral cycle before it.
fn interrupts_at(reference: &[Traced], level: u8) -> VecDeque<u64> {
    let mut last_board = 0;
    let mut interrupts = VecDeque::new();
    for traced in reference {
        match traced.record {
            Record::Exception {
                level: Some(at), ..
            } if at == level => interrupts.push_back(last_board),
            record if record.is_board() => last_board = traced.origin,
            _ => {}
        }
    }
    interrupts
}

/// What the replay did that the reference did not let it do exactly.
#[derive(Debug, Default)]
struct ReplayCounts {
    /// Bytes read with no reference read left to replay.
    unreplayed: u64,
    /// Bytes written with no reference write left at their address.
    unmatched_writes: u64,
    /// Bytes written other than the reference wrote there.
    differing_writes: u64,
    /// DUART interrupts requested.
    interrupts: u64,
    /// The newest reference line used.
    newest: u64,
    /// The oldest reference lines of board peripheral cycles not used.
    first_unused: Vec<u64>,
    /// The DUART interrupts not requested, at their lines.
    interrupts_left: Vec<u64>,
}

/// The board's peripherals on every select but the flash's and the RAM's.
/// A read takes the oldest byte the reference read at its address but
/// in a split long's words, which Unicorn may have read where the
/// CPU32 does not; failing that, a status register's ready, the last
/// byte read or written there, or zero, as the boot smoke test's stub.
/// The EEPROM is never replayed: its polls count its write cycle's
/// time, so it is `Eeprom`, in either mode.  With no reference it is
/// the stub.  The DUART's
/// interrupt is requested once the chip has made the reference's
/// cycles before it, until it is acknowledged.
#[derive(Debug, Default)]
struct Replay {
    /// The reference's bytes read, by address: its line, the byte.
    reads: BTreeMap<u32, VecDeque<(u64, u8)>>,
    /// The reference's bytes written, likewise.
    writes: BTreeMap<u32, VecDeque<(u64, u8)>>,
    /// The last byte read or written at each address.
    last: BTreeMap<u32, u8>,
    /// The EEPROM.
    eeprom: Eeprom,
    /// The newest reference line the chip has used.
    newest: u64,
    /// The reference lines the chip has used.
    used: BTreeSet<u64>,
    /// The DUART's interrupts still to request, at their lines.
    interrupts: VecDeque<u64>,
    /// What did not replay exactly.
    counts: ReplayCounts,
    /// The bytes written to TBB, the host port's output.
    port: Vec<u8>,
}

impl Replay {
    /// The stub, with the EEPROM's dump.
    fn stub(eeprom: Vec<u8>) -> Self {
        Self {
            eeprom: Eeprom {
                memory: eeprom,
                ..Eeprom::default()
            },
            ..Self::default()
        }
    }

    /// The replay of `reference`.
    fn of(reference: &[Traced], eeprom: Vec<u8>) -> Self {
        let mut replay = Self::stub(eeprom);
        for traced in reference
            .iter()
            .filter(|traced| traced.record.is_board() && !traced.split)
            .filter(|traced| {
                !matches!(traced.record, Record::Access { write: false, address, .. } if EEPROM.contains(&address))
            })
        {
            let Record::Access {
                write,
                address,
                size,
                value,
            } = traced.record
            else {
                continue;
            };
            let queues = if write {
                &mut replay.writes
            } else {
                &mut replay.reads
            };
            for byte in 0..u32::from(size) {
                let shift = 8 * (u32::from(size) - 1 - byte);
                queues
                    .entry(address + byte)
                    .or_default()
                    .push_back((traced.origin, (value >> shift) as u8));
            }
        }
        replay.interrupts = interrupts_at(reference, DUART_LEVEL);
        replay
    }

    /// The oldest byte of `queue`, if any, and its line now used.
    fn take(&mut self, write: bool, address: u32) -> Option<u8> {
        let queues = if write {
            &mut self.writes
        } else {
            &mut self.reads
        };
        let (line, byte) = queues.get_mut(&address)?.pop_front()?;
        self.newest = self.newest.max(line);
        self.used.insert(line);
        Some(byte)
    }

    /// The byte read at `address`.
    fn read_byte(&mut self, address: u32, now: Cycles) -> u8 {
        if EEPROM.contains(&address) {
            return self.eeprom.read(address - EEPROM.start, now);
        }
        if let Some(byte) = self.take(false, address) {
            self.last.insert(address, byte);
            return byte;
        }
        if !self.reads.is_empty() {
            self.counts.unreplayed += 1;
        }
        let fixed = STATUS_REGISTERS
            .iter()
            .find(|&&(at, _)| at == address)
            .map(|&(_, byte)| byte);
        fixed
            .or_else(|| self.last.get(&address).copied())
            .unwrap_or_default()
    }

    /// `byte` written at `address`.
    fn write_byte(&mut self, address: u32, byte: u8, now: Cycles) {
        if EEPROM.contains(&address) {
            self.eeprom.write(address - EEPROM.start, byte, now);
        }
        self.last.insert(address, byte);
        if address == DUART_TBB {
            self.port.push(byte);
        }
        if self.reads.is_empty() {
            return;
        }
        match self.take(true, address) {
            Some(expected) => {
                if expected != byte {
                    self.counts.differing_writes += 1;
                }
            }
            None => self.counts.unmatched_writes += 1,
        }
    }
}

/// The AT28C64B, as its data sheet gives it (smartclock
/// `third_party/microchip/AT28C64B.pdf`): a byte or page write loads
/// for up to tBLC after its last byte, then the write cycle takes tWC;
/// until it ends a read gives DATA polling's complement of the last
/// byte's I/O7 and toggle bit's I/O6, toggling each read.  Software
/// data protection is off, as shipped.
#[derive(Debug, Default)]
struct Eeprom {
    /// Its bytes.
    memory: Vec<u8>,
    /// When the write cycle under way ends.
    busy_until: Option<Cycles>,
    /// The last byte written.
    last_written: u8,
    /// I/O6 as the next read in a write cycle gives it.
    toggle: bool,
}

impl Eeprom {
    /// The byte at `offset` read at `now`.
    fn read(&mut self, offset: u32, now: Cycles) -> u8 {
        if self.busy_until.is_some_and(|end| now < end) {
            let byte = (!self.last_written & IO7) | if self.toggle { IO6 } else { 0 };
            self.toggle = !self.toggle;
            return byte;
        }
        self.busy_until = None;
        self.memory.get(offset as usize).copied().unwrap_or(ERASED)
    }

    /// `byte` written at `offset` at `now`.
    fn write(&mut self, offset: u32, byte: u8, now: Cycles) {
        if let Some(cell) = self.memory.get_mut(offset as usize) {
            *cell = byte;
        }
        self.last_written = byte;
        self.busy_until = Some(now + EEPROM_LOAD + EEPROM_WRITE);
    }
}

impl MappedPeripheral for Replay {
    fn read(&mut self, cycle: BusCycle) -> Result<u32, BusError> {
        if cycle.function_code.is_cpu_space() {
            // the DUART's interrupt acknowledge, through CS10
            if self.interrupt_request() {
                self.interrupts.pop_front();
                self.counts.interrupts += 1;
            }
            return Ok(u32::from(
                self.last.get(&DUART_IVR).copied().unwrap_or_default(),
            ));
        }
        Ok((0..cycle.size.bytes()).fold(0, |value, byte| {
            value << 8 | u32::from(self.read_byte(cycle.address + byte, cycle.now))
        }))
    }

    fn write(&mut self, cycle: BusCycle, value: u32) -> Result<(), BusError> {
        if cycle.function_code.is_cpu_space() {
            return Ok(());
        }
        let bytes = cycle.size.bytes();
        for byte in 0..bytes {
            let shift = 8 * (bytes - 1 - byte);
            self.write_byte(cycle.address + byte, (value >> shift) as u8, cycle.now);
        }
        Ok(())
    }

    fn interrupt_request(&self) -> bool {
        self.interrupts
            .front()
            .is_some_and(|&line| self.used.contains(&line))
    }
}

impl Timed for Replay {}

// The run is never snapshotted: its records are the test's.
mc683xx_cpu32::unsavable!(Replay, "a replay");

/// What the QSPI's registers hold of a queue run, as the cycles that
/// wrote them show: the command RAM and NEWQP (QSMRM 4.3.3, 4.3.5).
#[derive(Debug, Default)]
struct QspiShadow {
    /// COMD.RAM.
    command: [u8; QUEUE_ENTRIES as usize],
    /// SPCR2's NEWQP.
    newqp: u8,
}

impl QspiShadow {
    /// Follows `record`; at a write of SPCR1 with SPE, the run it
    /// starts: its first entry's command, the run's key, and NEWQP.
    fn observe(&mut self, record: Record) -> Option<(u8, u8)> {
        let Record::Access {
            write: true,
            address,
            size,
            value,
        } = record
        else {
            return None;
        };
        match address {
            SPCR2 => self.newqp = (value & NEWQP) as u8,
            SPCR1 if value & SPE != 0 => {
                return Some((self.command[usize::from(self.newqp)], self.newqp));
            }
            _ if COMMAND_RAM.contains(&address) => {
                for byte in 0..u32::from(size) {
                    let entry = (address + byte - COMMAND_RAM.start) as usize;
                    let shift = 8 * (u32::from(size) - 1 - byte);
                    if let Some(command) = self.command.get_mut(entry) {
                        *command = (value >> shift) as u8;
                    }
                }
            }
            _ => {}
        }
        None
    }
}

/// The QSPI's peripherals as the replay has them: each transfer
/// answered with the word the reference read from its entry of the
/// receive RAM in its run of the same first command, the runs of each
/// taken in order; zero where the reference read none (QSMRM 4.3.5).
#[derive(Debug, Default)]
struct ReplayQspi {
    /// The reference's runs by their first entry's command, in order:
    /// the first word it read at each receive RAM entry.
    runs: BTreeMap<u8, VecDeque<BTreeMap<u8, u16>>>,
    /// The run under way and its next entry.
    current: Option<(BTreeMap<u8, u16>, u8)>,
    /// Runs started with none of their command left to replay.
    unreplayed: u64,
    /// The bytes shifted to the front panel since last taken, with
    /// when.
    panel: Vec<(Cycles, u8)>,
}

impl ReplayQspi {
    /// The runs of `reference`.
    fn of(reference: &[Traced]) -> Self {
        let mut shadow = QspiShadow::default();
        let mut runs: BTreeMap<u8, VecDeque<BTreeMap<u8, u16>>> = BTreeMap::new();
        let mut last: Option<u8> = None;
        for traced in reference {
            if let Some((command, _)) = shadow.observe(traced.record) {
                runs.entry(command).or_default().push_back(BTreeMap::new());
                last = Some(command);
            }
            if let Record::Access {
                write: false,
                address,
                size: 2,
                value,
            } = traced.record
                && RECEIVE_RAM.contains(&address)
                && let Some(run) = last.and_then(|command| runs.get_mut(&command)?.back_mut())
            {
                let entry = ((address - RECEIVE_RAM.start) / 2) as u8;
                run.entry(entry).or_insert(value as u16);
            }
        }
        Self {
            runs,
            ..Self::default()
        }
    }

    /// The chip set SPE with `command` the first entry's, from
    /// `newqp`.
    fn start(&mut self, command: u8, newqp: u8) {
        let run = self.runs.get_mut(&command).and_then(VecDeque::pop_front);
        if run.is_none() {
            self.unreplayed += 1;
        }
        self.current = Some((run.unwrap_or_default(), newqp));
    }
}

mc683xx_cpu32::unsavable!(ReplayQspi, "a QSPI replay");

impl SpiPeripheral for ReplayQspi {
    fn transfer(&mut self, transfer: QspiTransfer) -> u16 {
        if transfer.chip_selects & PCS_MASK == FRONT_PANEL_SELECTS {
            self.panel.push((transfer.now, transfer.out as u8));
        }
        let Some((run, entry)) = &mut self.current else {
            return 0;
        };
        let word = run.get(entry).copied().unwrap_or_default();
        *entry = (*entry + 1) % QUEUE_ENTRIES;
        word
    }
}

/// What the 58503A's front panel shows: the key lamps and the text, as
/// last written.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Display {
    /// The key lamps, key n's on bit n - 1.
    lamps: u8,
    /// The text, as sent.
    text: Vec<u8>,
}

impl fmt::Display for Display {
    /// As the reference's record has it: the text with a space, a
    /// backslash and anything not printable ASCII as `\xHH`.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "lamps={:#04x} text=", self.lamps)?;
        for byte in &self.text {
            if byte.is_ascii_graphic() && *byte != b'\\' {
                write!(formatter, "{}", char::from(*byte))?;
            } else {
                write!(formatter, "\\x{byte:02x}")?;
            }
        }
        Ok(())
    }
}

/// A change of the display: when, and what it shows, as a record's
/// `lamps=` and `text=`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Shown {
    /// The reference's board cycle, or the chip's system clock's.
    cycle: u64,
    /// What.
    display: String,
}

/// The front panel's display as smartclock-emu forms it from the latch
/// and the QSPI (smartclock `crates/smartclock-emu/src/board/
/// front_panel.rs`, ported): bit 6 low frames a command, from its first
/// byte to its last; bit 7 low holds the panel in reset; a frame closed
/// with its last byte still on the QSPI ends when the byte comes.  The
/// panel's replies are the replay's, so only the display is kept.
#[derive(Debug, Default)]
struct FrontPanel {
    /// A command framed: bit 6 low, out of reset.
    framed: bool,
    /// The frame closed with its last byte still on the QSPI.
    closing: bool,
    /// The framed command's first byte, once it has come.
    command: Option<u8>,
    /// Its data so far.
    data: Vec<u8>,
    /// What it shows.
    display: Display,
    /// Each change of the display.
    shown: Vec<Shown>,
}

impl FrontPanel {
    /// The latch written `value` at `now`, with `in_flight` whether a
    /// transfer to the panel is on the QSPI.
    fn control(&mut self, value: u8, in_flight: bool, now: u64) {
        let running = value & LATCH_RUN != 0;
        let framed = running && value & LATCH_FRAME == 0;
        if !running {
            self.framed = false;
            self.closing = false;
            self.command = None;
            self.data.clear();
            return;
        }
        if framed && (!self.framed || self.closing) {
            if self.closing {
                self.end(now);
            }
            self.command = None;
            self.data.clear();
            self.framed = true;
        } else if !framed && self.framed && !self.closing {
            if in_flight {
                self.closing = true;
            } else {
                self.end(now);
            }
        }
    }

    /// The panel takes `byte` from the QSPI at `now`.
    fn exchange(&mut self, byte: u8, now: u64) {
        if !self.framed {
            return;
        }
        if self.command.is_some() {
            self.data.push(byte);
        } else {
            self.command = Some(byte);
        }
        if self.closing {
            self.end(now);
        }
    }

    /// The frame ends at `now`: what its command writes is shown.
    fn end(&mut self, now: u64) {
        self.framed = false;
        self.closing = false;
        let display = match (self.command, self.data.split_first()) {
            (Some(COMMAND_LAMPS_AND_TEXT), Some((lamps, text))) => Display {
                lamps: *lamps,
                text: text.to_vec(),
            },
            (Some(COMMAND_LAMPS), Some((lamps, _))) => Display {
                lamps: *lamps,
                text: self.display.text.clone(),
            },
            (Some(COMMAND_TEXT), _) => Display {
                lamps: self.display.lamps,
                text: self.data.clone(),
            },
            _ => return,
        };
        if display != self.display {
            self.shown.push(Shown {
                cycle: now,
                display: display.to_string(),
            });
            self.display = display;
        }
    }
}

/// The reference's display records.
fn reference_displays(text: &str) -> Vec<Shown> {
    text.lines()
        .filter_map(|line| line.strip_prefix(PREFIX)?.strip_prefix(DISPLAY))
        .filter_map(|rest| {
            let (cycle, display) = rest.split_once(' ')?;
            Some(Shown {
                cycle: cycle.parse().ok()?,
                display: display.to_owned(),
            })
        })
        .collect()
}

/// Whether the chip's cycle at `address` is one the reference traces.
const fn traced_address(address: u32) -> bool {
    (address >= BOARD_PERIPHERAL.start && address < BOARD_PERIPHERAL.end) || address >= MODULES
}

/// One run of the chip and its trace.
struct ChipRun {
    /// The records.
    trace: Vec<Traced>,
    /// The host port's output.
    port: Vec<u8>,
    /// What the replay could not replay.
    counts: ReplayCounts,
    /// The end, as a line.
    end: String,
    /// QSPI runs the replay had nothing for.
    qspi_unreplayed: u64,
    /// IC1 edges not made.
    captures_left: usize,
    /// The front panel's display changes.
    displays: Vec<Shown>,
}

/// What a run gives the chip, from the reference or not.
struct Inputs {
    /// The board's peripherals.
    peripheral: Replay,
    /// The QSPI's peripherals.
    qspi: Option<ReplayQspi>,
    /// The levels `PORTS`' input pins take, a read at a time.
    pins: [VecDeque<u8>; PORTS.len()],
    /// IC1's rising edges, each at the line the reference made the
    /// board cycle before it.
    captures: VecDeque<u64>,
}

impl Inputs {
    /// The stubs, with the EEPROM's dump.
    fn stub(eeprom: Vec<u8>) -> Self {
        Self {
            peripheral: Replay::stub(eeprom),
            qspi: None,
            pins: Default::default(),
            captures: VecDeque::new(),
        }
    }

    /// The replay of `reference`.
    fn replay(reference: &[Traced], eeprom: Vec<u8>) -> Self {
        Self {
            peripheral: Replay::of(reference, eeprom),
            qspi: Some(ReplayQspi::of(reference)),
            pins: port_reads(reference),
            captures: interrupts_at(reference, GPT_LEVEL),
        }
    }
}

/// Runs `image` on the board with `inputs` for `run`'s time, or until
/// `run.after_port` past TBB written as `expected_port` has it.
fn run_chip(
    image: Vec<u8>,
    inputs: Inputs,
    run: &ReferenceRun,
    seconds: f64,
    expected_port: &Port,
) -> ChipRun {
    let Inputs {
        peripheral,
        qspi,
        mut pins,
        mut captures,
    } = inputs;
    let after_port = run.after_port;
    let (mut builder, id, ram) = board(image, peripheral);
    if let Some(qspi) = qspi {
        builder.qspi(qspi);
    }
    builder.interrupt(
        id,
        mc683xx_cpu32::interrupt::IrqPin::new(DUART_LEVEL).expect("a level"),
    );
    let mut chip = builder.build();
    let mut tracing = mc683xx_mc68331::event::ChipTracing::default();
    tracing.accesses = true;
    tracing.exceptions = true;
    chip.set_tracing(tracing);
    let mut trace = Vec::new();
    let mut prompt_at = None;
    let mut stop_at = None;
    let mut driven: [Option<u8>; PORTS.len()] = [None; PORTS.len()];
    let mut shadow = QspiShadow::default();
    let mut panel = FrontPanel::default();
    let mut panel_in_flight = false;
    let end = loop {
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            clippy::cast_precision_loss,
            reason = "seconds of a few clocks' worth"
        )]
        let budget = (seconds * chip.sim().clock().frequency().hz() as f64) as u64;
        if chip.now().0 >= budget {
            break format!("the time, {seconds} s, at clock {}", chip.now().0);
        }
        if stop_at.is_some_and(|at| chip.now().0 >= at) {
            break format!(
                "{} s past TBB written as the reference's, as the reference ran on, at clock {}",
                after_port,
                chip.now().0
            );
        }
        drive_pins(&mut chip, &pins, &mut driven);
        let capture = captures
            .front()
            .is_some_and(|line| peripheral_of(&chip, id).used.contains(line));
        if capture {
            // IC1's rising edge, the FPGA's 1 PPS as TCTL2's EDGE1
            // takes it (GPTRM 6.1)
            captures.pop_front();
            let level = driven[GP].unwrap_or_default();
            chip.set_port_gp_pins(level | IC1);
            driven[GP] = Some(level);
        }
        let pc = chip.cpu().registers.pc;
        let run = chip.run_for(Cycles(1));
        let run = if run.stop == Stop::Stopped {
            chip.run_for(STOPPED_QUANTUM)
        } else {
            run
        };
        if capture {
            chip.set_port_gp_pins(driven[GP].unwrap_or_default());
        }
        let origin = chip.instructions();
        for event in chip.take_events() {
            if let Some(record) = chip_record(&chip, ram, pc, event) {
                if let Record::Access {
                    write: false,
                    address,
                    ..
                } = record
                    && let Some(port) = PORTS.iter().position(|&at| at == address)
                {
                    pins[port].pop_front();
                }
                if let Record::Access {
                    write: true,
                    address: LATCH,
                    value,
                    ..
                } = record
                {
                    panel.control(value as u8, panel_in_flight, chip.now().0);
                }
                if let Some((command, newqp)) = shadow.observe(record)
                    && let Some(qspi) = chip.qspi_peripheral_mut::<ReplayQspi>()
                {
                    panel_in_flight |= command & PCS_MASK == FRONT_PANEL_SELECTS;
                    qspi.start(command, newqp);
                }
                trace.push(Traced {
                    record,
                    pc,
                    origin,
                    cycle: chip.now().0,
                    split: false,
                });
            }
        }
        if let Some(qspi) = chip.qspi_peripheral_mut::<ReplayQspi>() {
            for (at, byte) in std::mem::take(&mut qspi.panel) {
                panel_in_flight = false;
                panel.exchange(byte, at.0);
            }
        }
        if prompt_at.is_none()
            && !expected_port.written.is_empty()
            && peripheral_of(&chip, id).port == expected_port.written
        {
            prompt_at = Some(chip.now().0);
            #[expect(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                clippy::cast_precision_loss,
                reason = "seconds of a few clocks' worth"
            )]
            let after = (after_port * chip.sim().clock().frequency().hz() as f64) as u64;
            stop_at = Some(chip.now().0 + after);
            println!(
                "boot-diff: TBB written as the reference's at clock {}",
                chip.now().0
            );
        }
        if let Stop::Halted(halt) = run.stop {
            break format!("a halt, {halt:?}, at PC {pc:#x}, clock {}", chip.now().0);
        }
    };
    let qspi_unreplayed = chip
        .qspi_peripheral::<ReplayQspi>()
        .map_or(0, |qspi| qspi.unreplayed);
    let peripheral = chip.peripheral_mut::<Replay>(id).expect("the replay");
    ChipRun {
        trace,
        port: std::mem::take(&mut peripheral.port),
        counts: ReplayCounts {
            newest: peripheral.newest,
            interrupts_left: peripheral
                .interrupts
                .iter()
                .take(UNUSED_SHOWN)
                .copied()
                .collect(),
            first_unused: peripheral
                .reads
                .values()
                .chain(peripheral.writes.values())
                .flat_map(|queue| queue.iter().map(|&(line, _)| line))
                .collect::<BTreeSet<u64>>()
                .into_iter()
                .take(UNUSED_SHOWN)
                .collect(),
            ..std::mem::take(&mut peripheral.counts)
        },
        end,
        qspi_unreplayed,
        captures_left: captures.len(),
        displays: panel.shown,
    }
}

/// The reference's reads of each of `PORTS`, in order.
fn port_reads(reference: &[Traced]) -> [VecDeque<u8>; PORTS.len()] {
    let mut reads: [VecDeque<u8>; PORTS.len()] = Default::default();
    for traced in reference {
        if let Record::Access {
            write: false,
            address,
            value,
            ..
        } = traced.record
            && let Some(port) = PORTS.iter().position(|&at| at == address)
        {
            reads[port].push_back(value as u8);
        }
    }
    reads
}

/// Drives each port's pins to the levels of the reference's next read
/// of it, where they changed from `driven`.
fn drive_pins(
    chip: &mut Mc68331,
    pins: &[VecDeque<u8>; PORTS.len()],
    driven: &mut [Option<u8>; PORTS.len()],
) {
    for (port, levels) in pins.iter().enumerate() {
        let Some(&level) = levels.front() else {
            continue;
        };
        if driven[port] == Some(level) {
            continue;
        }
        driven[port] = Some(level);
        match port {
            0 => chip.set_port_e_pins(level),
            1 => chip.set_port_f_pins(level),
            GP => chip.set_port_gp_pins(level),
            _ => {
                chip.set_pai(level & PACTL_PAIS != 0);
                chip.set_pclk(level & PACTL_PCLKS != 0);
            }
        }
    }
}

/// The board's peripheral.
fn peripheral_of(chip: &Mc68331, id: PeripheralId) -> &Replay {
    chip.peripheral::<Replay>(id).expect("the replay")
}

/// `event`'s record, if the reference would trace it.
fn chip_record(chip: &Mc68331, ram: MemoryId, pc: u32, event: Event) -> Option<Record> {
    match event {
        Event::Access(access)
            if traced_address(access.address.0) && !access.function_code.is_cpu_space() =>
        {
            Some(Record::Access {
                write: access.write,
                address: access.address.0,
                size: access.size.bytes() as u8,
                value: access.value.unwrap_or_default(),
            })
        }
        Event::Exception(_, entry) => {
            let memory = chip.memory(ram);
            let offset = entry.stack as usize & (memory.len() - 1);
            let sr = memory
                .get(offset..offset + 2)
                .map_or(0, |word| u16::from_be_bytes([word[0], word[1]]));
            let level = entry
                .level
                .map(mc683xx_cpu32::interrupt::InterruptLevel::get);
            Some(Record::Exception {
                vector: entry.vector.0,
                pc: if level.is_some() { entry.pc.0 } else { pc },
                sr,
                level,
            })
        }
        _ => None,
    }
}

/// A run of records the two sides do not share, at an index of each.
#[derive(Debug)]
struct Divergence {
    /// Where it starts in the reference's compared records.
    reference: usize,
    /// How many of the reference's.
    reference_length: usize,
    /// Where it starts in the chip's.
    chip: usize,
    /// How many of the chip's.
    chip_length: usize,
}

/// The records compared in order: neither interrupts nor the traps
/// their handlers make, whose place depends on time, nor a repeat of
/// the record before, or of the one before that between reads of its
/// address (a toggling bit), a poll's count depending on time too.
fn compared(trace: &[Traced]) -> Vec<Traced> {
    let mut kept: Vec<Traced> = Vec::new();
    for traced in trace
        .iter()
        .filter(|traced| !traced.record.is_interrupt() && !traced.record.in_handler())
    {
        let repeat =
            |back: usize| kept.len() >= back && kept[kept.len() - back].record == traced.record;
        let polled = |other: &Traced| match (other.record, traced.record) {
            (
                Record::Access {
                    write: false,
                    address: a,
                    ..
                },
                Record::Access {
                    write: false,
                    address: b,
                    ..
                },
            ) => a == b,
            _ => false,
        };
        if repeat(1) || repeat(2) && kept.last().is_some_and(polled) {
            continue;
        }
        kept.push(*traced);
    }
    kept
}

/// The divergences of `chip` from `reference`, aligned greedily: past
/// each, the nearest place `RESYNC` records agree again; with the
/// records that agree and the reference's split words the chip did not
/// read, passed over.
fn align(reference: &[Traced], chip: &[Traced]) -> (Vec<Divergence>, usize, usize) {
    let agree = |i: usize, j: usize| {
        (0..RESYNC).all(|k| match (reference.get(i + k), chip.get(j + k)) {
            (Some(a), Some(b)) => a.record == b.record,
            (None, None) => true,
            _ => false,
        })
    };
    let mut divergences = Vec::new();
    let (mut i, mut j) = (0, 0);
    let mut matched = 0;
    let mut split = 0;
    while i < reference.len() && j < chip.len() {
        if reference[i].record == chip[j].record {
            i += 1;
            j += 1;
            matched += 1;
            continue;
        }
        if reference[i].split {
            i += 1;
            split += 1;
            continue;
        }
        let resync = (1..=2 * LOOKAHEAD).find_map(|distance| {
            (0..=distance)
                .map(|di| (di, distance - di))
                .filter(|&(di, dj)| di <= LOOKAHEAD && dj <= LOOKAHEAD)
                .find(|&(di, dj)| agree(i + di, j + dj))
        });
        let (di, dj) = resync.unwrap_or((reference.len() - i, chip.len() - j));
        divergences.push(Divergence {
            reference: i,
            reference_length: di,
            chip: j,
            chip_length: dj,
        });
        i += di;
        j += dj;
    }
    if i < reference.len() || j < chip.len() {
        divergences.push(Divergence {
            reference: i,
            reference_length: reference.len() - i,
            chip: j,
            chip_length: chip.len() - j,
        });
    }
    (divergences, matched, split)
}

/// What a divergence is, as far as its records tell.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Kind {
    /// The same records on both sides in another order: an interrupt
    /// handler's or another task's cycles at another time.
    Moved,
    /// Only the modules' registers and traps: the chip's modules'
    /// timing against smartclock's.
    Modules,
    /// A board peripheral's cycle differs.
    Board,
}

/// What `divergences[index]` is: `Kind::Moved` if each side's
/// records are among the other side's of the divergences within
/// `MOVED_WITHIN` of it.
fn kind(reference: &[Traced], chip: &[Traced], divergences: &[Divergence], index: usize) -> Kind {
    let theirs = |divergence: &Divergence| {
        &reference[divergence.reference..divergence.reference + divergence.reference_length]
    };
    let ours =
        |divergence: &Divergence| &chip[divergence.chip..divergence.chip + divergence.chip_length];
    let nearby = &divergences
        [index.saturating_sub(MOVED_WITHIN)..(index + MOVED_WITHIN + 1).min(divergences.len())];
    let among = |records: &[Traced], others: &mut BTreeMap<Record, usize>| {
        records
            .iter()
            .all(|traced| match others.get_mut(&traced.record) {
                Some(count) if *count > 0 => {
                    *count -= 1;
                    true
                }
                _ => false,
            })
    };
    let counted = |records: &mut dyn Iterator<Item = &Traced>| {
        let mut counts: BTreeMap<Record, usize> = BTreeMap::new();
        for traced in records {
            *counts.entry(traced.record).or_default() += 1;
        }
        counts
    };
    let divergence = &divergences[index];
    let moved = among(
        theirs(divergence),
        &mut counted(&mut nearby.iter().flat_map(ours)),
    ) && among(
        ours(divergence),
        &mut counted(&mut nearby.iter().flat_map(theirs)),
    );
    if moved {
        Kind::Moved
    } else if theirs(divergence)
        .iter()
        .chain(ours(divergence))
        .all(|traced| !traced.record.is_board())
    {
        Kind::Modules
    } else {
        Kind::Board
    }
}

/// Prints `records[start..start + length]`, at most `CONTEXT` of them.
fn show(label: &str, records: &[Traced], start: usize, length: usize) {
    let end = (start + length).min(records.len());
    for traced in &records[start..end.min(start + CONTEXT)] {
        println!("boot-diff:   {label} {traced}");
    }
    if end - start > CONTEXT {
        println!("boot-diff:   {label} ... {} more", end - start - CONTEXT);
    }
}

/// Exceptions by vector and level.
fn exception_counts(trace: &[Traced]) -> BTreeMap<(u8, Option<u8>), u64> {
    let mut counts = BTreeMap::new();
    for traced in trace {
        if let Record::Exception { vector, level, .. } = traced.record {
            *counts.entry((vector, level)).or_default() += 1;
        }
    }
    counts
}

/// The bytes each side wrote at each board peripheral address, compared:
/// the first address, by the reference's line, where they differ.
fn write_differences(reference: &[Traced], chip: &[Traced]) -> Vec<String> {
    let writes = |trace: &[Traced]| {
        let mut writes: BTreeMap<u32, Vec<(u64, u32)>> = BTreeMap::new();
        for traced in trace {
            if let Record::Access {
                write: true,
                address,
                value,
                ..
            } = traced.record
            {
                writes
                    .entry(address)
                    .or_default()
                    .push((traced.origin, value));
            }
        }
        writes
    };
    let (ours, theirs) = (writes(chip), writes(reference));
    let mut differences: Vec<(u64, String)> = Vec::new();
    let addresses: BTreeSet<u32> = theirs.keys().chain(ours.keys()).copied().collect();
    for address in addresses {
        let expected = theirs.get(&address).map_or(&[][..], Vec::as_slice);
        let got = ours.get(&address).map_or(&[][..], Vec::as_slice);
        let index = expected
            .iter()
            .zip(got)
            .position(|(a, b)| a.1 != b.1)
            .or_else(|| (got.len() != expected.len()).then_some(got.len().min(expected.len())));
        if let Some(index) = index {
            let line = expected.get(index).map_or(u64::MAX, |write| write.0);
            {
                differences.push((
                    line,
                    format!(
                        "{address:#010x} write {index}: reference {}, chip {}; \
                         {} writes there in the reference, {} in the chip",
                        expected.get(index).map_or_else(
                            || "none".to_owned(),
                            |(at, value)| format!("{value:#x} at line {at}")
                        ),
                        got.get(index).map_or_else(
                            || "none".to_owned(),
                            |(at, value)| format!("{value:#x} at instruction {at}")
                        ),
                        expected.len(),
                        got.len()
                    ),
                ));
            }
        }
    }
    differences.sort();
    differences.into_iter().map(|(_, text)| text).collect()
}

/// Aligns `ours` with `theirs` and prints the divergences by kind, the
/// first of each.
fn compare(label: &str, theirs: &[Traced], ours: &[Traced]) {
    let (divergences, matched, split) = align(theirs, ours);
    let mut kinds: BTreeMap<Kind, Vec<&Divergence>> = BTreeMap::new();
    for (index, divergence) in divergences.iter().enumerate() {
        kinds
            .entry(kind(theirs, ours, &divergences, index))
            .or_default()
            .push(divergence);
    }
    println!(
        "boot-diff: {label}: {} reference records compared, {} of the chip's; {matched} agree; \
         {split} split words of Unicorn's passed over; {} divergences: {:?}",
        theirs.len(),
        ours.len(),
        divergences.len(),
        kinds
            .iter()
            .map(|(kind, divergences)| (*kind, divergences.len()))
            .collect::<Vec<_>>()
    );
    let shown = kinds.iter().flat_map(|(kind, divergences)| {
        let shown = if *kind == Kind::Board {
            DIVERGENCES_SHOWN
        } else {
            KINDS_SHOWN
        };
        divergences
            .iter()
            .take(shown)
            .map(move |divergence| (*kind, *divergence))
    });
    for (kind, divergence) in shown {
        println!(
            "boot-diff: {label}: {kind:?} divergence at reference line {}, chip record {}:",
            theirs
                .get(divergence.reference)
                .map_or(0, |traced| traced.origin),
            divergence.chip
        );
        let before = divergence.reference.saturating_sub(CONTEXT);
        show("both     ", theirs, before, divergence.reference - before);
        show(
            "reference",
            theirs,
            divergence.reference,
            divergence.reference_length,
        );
        show("chip     ", ours, divergence.chip, divergence.chip_length);
    }
}

/// The front panel's display changes compared in order, side by side.
fn compare_displays(name: &str, theirs: &[Shown], ours: &[Shown]) {
    let agree = theirs
        .iter()
        .zip(ours)
        .take_while(|(a, b)| a.display == b.display)
        .count();
    println!(
        "boot-diff: {name}: display: {} changes in the reference, {} in the chip; the first {agree} \
         the same, in order",
        theirs.len(),
        ours.len()
    );
    for index in 0..theirs.len().max(ours.len()).min(DISPLAYS_SHOWN) {
        let side = |shown: Option<&Shown>| {
            shown.map_or_else(
                || "none".to_owned(),
                |shown| format!("cycle={} {}", shown.cycle, shown.display),
            )
        };
        let (a, b) = (theirs.get(index), ours.get(index));
        let same = a.zip(b).is_some_and(|(a, b)| a.display == b.display);
        println!(
            "boot-diff:   {index} {}: reference {}; chip {}",
            if same { "same" } else { "differs" },
            side(a),
            side(b)
        );
    }
}

/// Compares `run` with `reference` and prints what differs.
fn report(
    name: &str,
    reference: &[Traced],
    displays: &[Shown],
    run: &ChipRun,
    expected_port: &Port,
    output: Option<&Path>,
) {
    println!("boot-diff: {name}: ended at {}", run.end);
    if let Some(directory) = output {
        let path = directory.join(format!("chip-{name}.txt"));
        let text: String = run
            .trace
            .iter()
            .map(|traced| format!("{PREFIX}{traced}\n"))
            .collect();
        fs::write(&path, text).expect("the chip's trace written");
        println!("boot-diff: {name}: the chip's trace in {}", path.display());
    }
    let (theirs, ours) = (compared(reference), compared(&run.trace));
    compare(&format!("{name}: all"), &theirs, &ours);
    // the board's peripherals alone, which the modules' timing does not
    // reorder
    let board = |records: &[Traced]| -> Vec<Traced> {
        records
            .iter()
            .filter(|traced| traced.record.is_board())
            .copied()
            .collect()
    };
    compare(&format!("{name}: board"), &board(&theirs), &board(&ours));
    compare_displays(name, displays, &run.displays);
    let differences = write_differences(reference, &run.trace);
    println!(
        "boot-diff: {name}: {} addresses where the writes differ, the first {WRITES_SHOWN} by \
         the reference's line:",
        differences.len()
    );
    for difference in differences.iter().take(WRITES_SHOWN) {
        println!("boot-diff:   {difference}");
    }
    println!(
        "boot-diff: {name}: exceptions by (vector, level): reference {:?}",
        exception_counts(reference)
    );
    println!(
        "boot-diff: {name}: exceptions by (vector, level): chip      {:?}",
        exception_counts(&run.trace)
    );
    let counts = &run.counts;
    println!(
        "boot-diff: {name}: replay: {} bytes read with nothing to replay, {} written with no \
         reference write left there, {} written otherwise than the reference, {} DUART \
         interrupts; the newest reference line used {}, the oldest not used {:?}; DUART interrupts left at {:?}",
        counts.unreplayed,
        counts.unmatched_writes,
        counts.differing_writes,
        counts.interrupts,
        counts.newest,
        counts.first_unused,
        counts.interrupts_left
    );
    println!(
        "boot-diff: {name}: replay: {} QSPI runs with none of their first command left, {} IC1 \
         edges not made",
        run.qspi_unreplayed, run.captures_left
    );
    println!(
        "boot-diff: {name}: TBB written: \"{}\", {} the reference's; the port's output in \
         the reference, \"{}\", {}",
        run.port.escape_ascii(),
        if run.port == expected_port.written {
            "as"
        } else {
            "not as"
        },
        expected_port.shown.escape_ascii(),
        if in_order(&expected_port.shown, &run.port) {
            "among them in order"
        } else {
            "not among them"
        }
    );
}

/// Whether `part`'s bytes are among `whole`'s, in order.
fn in_order(part: &[u8], whole: &[u8]) -> bool {
    let mut rest = whole.iter();
    part.iter().all(|byte| rest.any(|other| other == byte))
}

/// The host port as the reference has it.
#[derive(Debug)]
struct Port {
    /// The bytes written to TBB, loopback self-tests' among them.
    written: Vec<u8>,
    /// What the port showed (`.port.out`).
    shown: Vec<u8>,
}

impl Port {
    /// `reference`'s, with `shown` its port's output.
    fn of(reference: &[Traced], shown: Vec<u8>) -> Self {
        let written = reference
            .iter()
            .filter_map(|traced| match traced.record {
                Record::Access {
                    write: true,
                    address: DUART_TBB,
                    value,
                    ..
                } => Some(value as u8),
                _ => None,
            })
            .collect();
        Self { written, shown }
    }
}

#[test]
fn boot_follows_the_reference() {
    let (Some(firmware), Some(reference_path)) = (
        env::var_os(FIRMWARE).map(PathBuf::from),
        env::var_os(REFERENCE).map(PathBuf::from),
    ) else {
        eprintln!("skipped: {FIRMWARE} or {REFERENCE} is unset; `make boot-diff` runs it");
        return;
    };
    let text = fs::read_to_string(&reference_path).expect("the reference");
    let run = ReferenceRun::of(&text);
    let seconds = env::var(SECONDS)
        .ok()
        .and_then(|seconds| seconds.parse().ok())
        .unwrap_or(run.seconds + SECONDS_PAST_REFERENCE);
    let output = env::var_os(OUTPUT).map(PathBuf::from);
    println!(
        "boot-diff: {} with {}, for {seconds} s",
        run.image, run.eeprom
    );
    let image = fs::read(firmware.join(&run.image)).expect("the image");
    let eeprom = fs::read(firmware.join(&run.eeprom)).expect("the EEPROM's dump");
    let reference = parse_reference(&text);
    let displays = reference_displays(&text);
    let expected_port = Port::of(
        &reference,
        fs::read(reference_path.with_extension("port.out")).unwrap_or_default(),
    );
    println!(
        "boot-diff: {} reference records from {}",
        reference.len(),
        reference_path.display()
    );
    let stub = run_chip(
        image.clone(),
        Inputs::stub(eeprom.clone()),
        &run,
        seconds,
        &expected_port,
    );
    report(
        "stub",
        &reference,
        &displays,
        &stub,
        &expected_port,
        output.as_deref(),
    );
    let replay = run_chip(
        image,
        Inputs::replay(&reference, eeprom),
        &run,
        seconds,
        &expected_port,
    );
    report(
        "replay",
        &reference,
        &displays,
        &replay,
        &expected_port,
        output.as_deref(),
    );
}
