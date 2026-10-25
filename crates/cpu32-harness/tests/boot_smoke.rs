//! A smoke test of a whole smartclock image booted on the MC68331 from
//! its power-on reset, to find the core's and the chip's faults before
//! the board's real peripherals are modeled (PLAN.md, Open).  The wiring is
//! the Z3805A's (github.com/charlieh0tel/smartclock,
//! `docs/firmware/restart.md`, "The chip selects"): the flash read on
//! CSBOOT and CS1 and written on CS6 and CS7, the RAM read on CS0 and
//! written a byte lane each on CS2 and CS3, and every other select on a
//! recording stub, no peripheral's logic.  It prints a trace and checks
//! nothing the stubs cannot give.  The images are HP's and not in this
//! repository: `SMARTCLOCK_FIRMWARE` names smartclock's
//! `third_party/firmware`; without it the test is skipped.
//! `make boot-smoke` runs it.

use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::PathBuf;

use mc683xx_cpu32::bus::BusError;
use mc683xx_cpu32::cpu::Stop;
use mc683xx_cpu32::cycles::Cycles;
use mc683xx_cpu32::peripheral::BusCycle;
use mc683xx_cpu32::peripheral::MappedPeripheral;
use mc683xx_mc68331::chip::Mc68331;
use mc683xx_mc68331::event::Event;
use mc683xx_modules::chip_select::ChipSelect;
use mc683xx_modules::peripheral::Timed;
use mc683xx_modules::qsm::sci::Frame;

use common::FIRMWARE;
use common::board;

mod common;

/// The variable giving the emulated seconds each image runs.
const SECONDS: &str = "BOOT_SMOKE_SECONDS";
/// Emulated seconds an image runs unless `SECONDS` says otherwise.
const DEFAULT_SECONDS: f64 = 3.0;
/// What the stubs read where nothing was written: smartclock-emu's
/// unmodeled byte (`board/mod.rs`, `NOTHING`).
const STUB_DEFAULT: u8 = 0;
/// Exceptions traced one by one before only being counted.
const EXCEPTIONS_SHOWN: usize = 40;
/// Instructions at the end whose PCs are tallied, to show a loop.
const TAIL_INSTRUCTIONS: usize = 100_000;
/// PCs shown from the tally, and addresses from the stub's reads.
const TAIL_SHOWN: usize = 12;
/// Addresses shown from the stub's writes.
const WRITES_SHOWN: usize = 32;
/// Bytes kept of what is written at an address.
const LOG_BYTES: usize = 2048;
/// Writes at an address that make its log worth showing.
const LOG_SHOWN: usize = 8;
/// Clocks run at a time while the CPU is stopped and waits.
const STOPPED_QUANTUM: Cycles = Cycles(256);
/// The select registers traced: CSBOOT and CS0 to CS10.
const SELECTS: usize = ChipSelect::COUNT;

/// The primary's vector table (smartclock `docs/firmware/README.md`),
/// whose reset PC the boot code jumps to.
const PRIMARY_VECTORS: usize = 0x1_0000;

/// Functions on smartclock's boot path, by the image's name and its
/// address there (smartclock `third_party/firmware/<image>/names.tsv`;
/// `docs/firmware/restart.md`), or where it fails: each traced the
/// first time the CPU starts an instruction there.
struct Image {
    /// Its file's stem.
    name: &'static str,
    /// Its boot path's functions.
    milestones: &'static [(&'static str, u32)],
}

/// The images booted.
const IMAGES: [Image; 2] = [
    Image {
        name: "z3805a-3543b",
        milestones: &[
            ("boot_reset", 0x550),
            ("kernel_init", 0x15cd8),
            ("root_task", 0x1304a),
            ("duart_init", 0x1d93e),
            ("console_task", 0x1aedc),
            ("scpi_task", 0x2a04a),
            ("selftest_cpu", 0x17a96),
            ("selftest_ram", 0x179ac),
            ("selftest_eeprom", 0x17b16),
            ("selftest_qspi", 0x17b96),
            ("selftest_fpga", 0x17c3e),
            ("selftest_uart", 0x17cbe),
            ("powerup", 0x45e8e),
            ("primary_default_exception", 0x11924),
            ("fatal_error", 0x1f176),
        ],
    },
    Image {
        name: "58503a-3704",
        milestones: &[
            ("boot_reset", 0x550),
            ("kernel_init", 0x15d32),
            ("root_task", 0x1321e),
            ("duart_init", 0x1d790),
            ("console_task", 0x1b040),
            ("selftest_cpu", 0x17af0),
            ("selftest_ram", 0x17a06),
            ("selftest_uart", 0x17d18),
            ("powerup", 0x46072),
            ("fatal_error", 0x1efea),
        ],
    },
];

/// Addresses the stub reads as a fixed byte, whatever was written:
/// the DUART's SRA and SRB, which share their addresses with the
/// write-only CSRA and CSRB, as `TxRDY` and `TxEMT` with no character
/// received, as smartclock's harness has them (smartclock
/// `tools/firmware/harness.py`, `STATUS_REGISTERS`; MC68681, "Channel
/// A status register (SRA) and channel B status register (SRB)").
const FIXED_READS: [(u32, u8); 2] = [(0x20_0003, 0x0c), (0x20_0013, 0x0c)];

/// A select's name, as the manuals give it.
/// The select that carried `cycle`.
fn select_of(cycle: BusCycle) -> ChipSelect {
    cycle
        .select
        .and_then(ChipSelect::from_select)
        .expect("a chip's cycle carries its select")
}

fn select_name(select: ChipSelect) -> String {
    match select {
        ChipSelect::Boot => "CSBOOT".to_owned(),
        ChipSelect::Numbered(number) => format!("CS{}", number.get()),
    }
}

/// A peripheral that stands for whatever the board has on its selects: a
/// read gives `FIXED_READS`' byte, or the byte last written at its
/// address, or `STUB_DEFAULT`; each select's first cycle and every
/// write are recorded.
#[derive(Debug, Default)]
struct Stub {
    /// Bytes written, by space and address.
    bytes: BTreeMap<(u8, u32), u8>,
    /// Each select's first cycle and whether it wrote.
    first: BTreeMap<ChipSelect, (BusCycle, bool)>,
    /// Cycles by select: reads, writes.
    counts: BTreeMap<ChipSelect, (u64, u64)>,
    /// Writes by select and address: how many, the last value.
    writes: BTreeMap<(ChipSelect, u32), (u64, u32)>,
    /// Reads by select and address.
    reads: BTreeMap<(ChipSelect, u32), u64>,
    /// The bytes written at each address in order, the first
    /// `LOG_BYTES`: a peripheral's output, a console's text among them.
    log: BTreeMap<(ChipSelect, u32), Vec<u8>>,
}

impl Stub {
    fn record(&mut self, cycle: BusCycle, write: bool) {
        self.first.entry(select_of(cycle)).or_insert((cycle, write));
        let counts = self.counts.entry(select_of(cycle)).or_default();
        if write {
            counts.1 += 1;
        } else {
            counts.0 += 1;
        }
    }
}

impl MappedPeripheral for Stub {
    fn read(&mut self, cycle: BusCycle) -> Result<u32, BusError> {
        self.record(cycle, false);
        *self
            .reads
            .entry((select_of(cycle), cycle.address))
            .or_default() += 1;
        let space = cycle.function_code.bits();
        Ok((0..cycle.size.bytes()).fold(0, |value, byte| {
            let at = cycle.address.wrapping_add(byte);
            let fixed = FIXED_READS
                .iter()
                .find(|&&(address, _)| address == at && !cycle.function_code.is_cpu_space());
            let byte = fixed.map_or_else(
                || {
                    self.bytes
                        .get(&(space, at))
                        .copied()
                        .unwrap_or(STUB_DEFAULT)
                },
                |&(_, byte)| byte,
            );
            value << 8 | u32::from(byte)
        }))
    }

    fn write(&mut self, cycle: BusCycle, value: u32) -> Result<(), BusError> {
        self.record(cycle, true);
        let entry = self
            .writes
            .entry((select_of(cycle), cycle.address))
            .or_default();
        *entry = (entry.0 + 1, value);
        let log = self
            .log
            .entry((select_of(cycle), cycle.address))
            .or_default();
        if log.len() < LOG_BYTES {
            log.push(value as u8);
        }
        let space = cycle.function_code.bits();
        let bytes = cycle.size.bytes();
        for byte in 0..bytes {
            let shift = 8 * (bytes - 1 - byte);
            self.bytes.insert(
                (space, cycle.address.wrapping_add(byte)),
                (value >> shift) as u8,
            );
        }
        Ok(())
    }
}

impl Timed for Stub {}

// The run is never snapshotted: its records are the test's.
mc683xx_cpu32::unsavable!(Stub, "a stub");

/// The chip-select registers as the trace compares them: CSPAR0 and
/// CSPAR1, then each select's CSBAR and CSOR.
fn select_registers(chip: &Mc68331) -> ([u16; 2], [(u16, u16); SELECTS]) {
    let selects = chip.sim().chip_selects();
    let mut registers = [(0, 0); SELECTS];
    for (index, entry) in registers.iter_mut().enumerate() {
        let select = ChipSelect::from_index(index).expect("a select");
        *entry = (selects.base(select), selects.option(select));
    }
    (
        [selects.pin_assignment(0), selects.pin_assignment(1)],
        registers,
    )
}

/// What one image's boot did.
#[derive(Debug, Default)]
struct Boot {
    /// Instructions run.
    instructions: u64,
    /// Exceptions taken, by vector.
    exceptions: BTreeMap<u8, u64>,
    /// The PCs of the last `TAIL_INSTRUCTIONS`, tallied.
    tail: BTreeMap<u32, u64>,
    /// Milestones reached, with the time.
    reached: Vec<(&'static str, Cycles)>,
}

/// Boots `image` for `seconds` of emulated time, tracing as it goes.
fn boot(image: &Image, bytes: Vec<u8>, seconds: f64) -> Boot {
    let primary_reset = bytes
        .get(PRIMARY_VECTORS + 4..PRIMARY_VECTORS + 8)
        .map(|long| u32::from_be_bytes(long.try_into().expect("a long")));
    let mut milestones: BTreeMap<u32, &'static str> = image
        .milestones
        .iter()
        .map(|&(name, pc)| (pc, name))
        .chain(primary_reset.map(|pc| (pc, "primary_reset")))
        .collect();
    let (builder, stub, _) = board(bytes, Stub::default());
    let mut chip = builder.build();
    let hz = chip.sim().clock().frequency().hz();
    let mut trace = Boot::default();
    let mut registers = select_registers(&chip);
    let mut vbr = chip.cpu().registers.vbr;
    let mut in_upper_flash = false;
    let mut shown = 0;
    // resets seen, the power-on's among them: the selects' programming
    // is traced until the second
    let mut resets = 0;
    let mut tail_start = None;
    let name = image.name;
    println!(
        "{name}: reset: SSP {:#x} PC {:#x}",
        chip.cpu().registers.ssp,
        chip.cpu().registers.pc
    );
    let mut budget_end = None;
    loop {
        let hz_now = chip.sim().clock().frequency().hz();
        if budget_end.is_none() || hz_now != hz {
            // the end is a time, at the clock the firmware sets
            let elapsed = chip.now().0;
            #[expect(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                clippy::cast_precision_loss,
                reason = "seconds of a few clocks' worth"
            )]
            let end = (seconds * hz_now as f64) as u64;
            budget_end = Some(end.max(elapsed));
        }
        let end = budget_end.expect("set");
        if chip.now().0 >= end {
            break;
        }
        let pc = chip.cpu().registers.pc;
        let run = chip.run_for(Cycles(1));
        let run = if run.stop == Stop::Stopped {
            chip.run_for(STOPPED_QUANTUM)
        } else {
            run
        };
        trace.instructions += 1;
        if let Some(milestone) = milestones.remove(&pc) {
            println!(
                "{name}: milestone {milestone} ({pc:#x}) at clock {}",
                chip.now().0
            );
            trace.reached.push((milestone, chip.now()));
        }
        if !in_upper_flash && (0x4_0000..0x8_0000).contains(&pc) {
            in_upper_flash = true;
            println!(
                "{name}: first instruction in CS1's upper flash at {pc:#x}, clock {}",
                chip.now().0
            );
        }
        if let Some(exception) = chip.cpu().exception_taken() {
            let vector = exception.vector().0;
            let count = trace.exceptions.entry(vector).or_default();
            *count += 1;
            if shown < EXCEPTIONS_SHOWN || *count == 1 {
                shown += 1;
                println!(
                    "{name}: exception vector {vector} at PC {pc:#x}, clock {}: {exception:?} \
                     -> {:#x}",
                    chip.now().0,
                    chip.cpu().registers.pc
                );
            }
        }
        let now_registers = select_registers(&chip);
        if now_registers != registers && resets <= 1 {
            for register in 0..2 {
                if now_registers.0[register] != registers.0[register] {
                    println!(
                        "{name}: CSPAR{register} = {:#06x} at PC {pc:#x}",
                        now_registers.0[register]
                    );
                }
            }
            for index in 0..SELECTS {
                let (base, option) = now_registers.1[index];
                if (base, option) != registers.1[index] {
                    let select = ChipSelect::from_index(index).expect("a select");
                    println!(
                        "{name}: {} CSBAR {base:#06x} CSOR {option:#06x} at PC {pc:#x}",
                        select_name(select)
                    );
                }
            }
        }
        registers = now_registers;
        if chip.cpu().registers.vbr != vbr {
            vbr = chip.cpu().registers.vbr;
            println!(
                "{name}: VBR = {vbr:#x} at PC {pc:#x}, clock {}",
                chip.now().0
            );
        }
        for event in chip.take_events() {
            println!("{name}: {event:?} at PC {pc:#x}");
            if let Event::Reset(..) = event {
                tail_start = None;
                resets += 1;
            }
        }
        if let Stop::Halted(halt) = run.stop {
            println!(
                "{name}: halted, {halt:?}, at PC {pc:#x}, clock {}",
                chip.now().0
            );
            break;
        }
        if end.saturating_sub(chip.now().0) < end / 10 {
            let start = tail_start.get_or_insert(trace.instructions);
            if trace.instructions - *start < TAIL_INSTRUCTIONS as u64 {
                *trace.tail.entry(chip.cpu().registers.pc).or_default() += 1;
            }
        }
    }
    let registers = &chip.cpu().registers;
    println!(
        "{name}: end at clock {} ({hz} Hz at reset, {} Hz now), {} instructions, PC {:#x}, \
         SR {:#06x}, SSP {:#x}, USP {:#x}, VBR {:#x}",
        chip.now().0,
        chip.sim().clock().frequency().hz(),
        trace.instructions,
        registers.pc,
        registers.sr(),
        registers.ssp,
        registers.usp,
        registers.vbr
    );
    println!("{name}: D {:x?}", registers.data);
    println!(
        "{name}: A {:x?}",
        (0..8).map(|n| registers.address(n)).collect::<Vec<_>>()
    );
    println!("{name}: exceptions by vector: {:?}", trace.exceptions);
    let mut tail: Vec<_> = trace.tail.iter().collect();
    tail.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
    println!(
        "{name}: hottest PCs near the end: {:x?}",
        tail.iter().take(TAIL_SHOWN).collect::<Vec<_>>()
    );
    if let Some(stub) = chip.peripheral::<Stub>(stub) {
        for (select, (cycle, write)) in &stub.first {
            println!(
                "{name}: {} first {} at {:#x}, {:?}, FC {}, clock {}",
                select_name(*select),
                if *write { "write" } else { "read" },
                cycle.address,
                cycle.size,
                cycle.function_code.bits(),
                cycle.now.0
            );
        }
        for (select, (reads, writes)) in &stub.counts {
            println!(
                "{name}: {}: {reads} reads, {writes} writes",
                select_name(*select)
            );
        }
        let mut reads: Vec<_> = stub.reads.iter().collect();
        reads.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
        for ((select, address), count) in reads.iter().take(TAIL_SHOWN) {
            println!(
                "{name}: read {count} times: {} {address:#x}",
                select_name(*select)
            );
        }
        let writes: Vec<_> = stub
            .writes
            .iter()
            .map(|((select, address), (count, value))| {
                format!(
                    "{} {address:#x} x{count} last {value:#x}",
                    select_name(*select)
                )
            })
            .collect();
        println!(
            "{name}: writes, the first {WRITES_SHOWN} addresses: {:?}",
            &writes[..writes.len().min(WRITES_SHOWN)]
        );
        for ((select, address), log) in &stub.log {
            if log.len() >= LOG_SHOWN {
                println!(
                    "{name}: written at {} {address:#x}: \"{}\"",
                    select_name(*select),
                    log.escape_ascii()
                );
            }
        }
    }
    let sci: Vec<u8> = chip
        .take_sci_transmitted()
        .into_iter()
        .filter_map(|sent| match sent.frame {
            Frame::Data(data) => Some(data as u8),
            Frame::Break => None,
        })
        .collect();
    println!("{name}: SCI sent: \"{}\"", sci.escape_ascii());
    println!(
        "{name}: milestones not reached: {:?}",
        milestones.values().collect::<Vec<_>>()
    );
    trace
}

#[test]
fn images_boot_from_power_on() {
    let Some(directory) = env::var_os(FIRMWARE).map(PathBuf::from) else {
        eprintln!("skipped: {FIRMWARE} is unset; `make boot-smoke` runs it");
        return;
    };
    let seconds = env::var(SECONDS)
        .ok()
        .and_then(|seconds| seconds.parse().ok())
        .unwrap_or(DEFAULT_SECONDS);
    for image in &IMAGES {
        let Ok(bytes) = fs::read(directory.join(format!("{}.bin", image.name))) else {
            println!("{}: no image, skipped", image.name);
            continue;
        };
        let trace = boot(image, bytes, seconds);
        let reached: Vec<_> = trace.reached.iter().map(|&(name, _)| name).collect();
        println!("{}: milestones reached: {reached:?}", image.name);
    }
}
