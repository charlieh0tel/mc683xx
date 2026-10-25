//! Boards loaded from files and run: the same program as on a chip
//! built by hand, peripherals on split selects and decoders, and nets whose
//! sub-cycle phase survives to a GPT capture.

use std::any::Any;
use std::cell::Cell;
use std::cell::Ref;
use std::env;
use std::fs;
use std::num::NonZeroU64;
use std::ops::ControlFlow;
use std::path::PathBuf;
use std::process;
use std::rc::Rc;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;

use mc683xx_board::adapter::MappedAdapter;
use mc683xx_board::adapter::RELEASED_LEVELS;
use mc683xx_board::adapter::SELECTED_LEVELS;
use mc683xx_board::adapter::SpiAdapter;
use mc683xx_board::board::Board;
use mc683xx_board::board::BoardTracing;
use mc683xx_board::builtin::EdgeCounter;
use mc683xx_board::builtin::Recorder;
use mc683xx_board::description::Description;
use mc683xx_board::description::Options;
use mc683xx_board::error::LoadError;
use mc683xx_board::error::ReceiveError;
use mc683xx_board::error::RunError;
use mc683xx_board::error::SendError;
use mc683xx_board::peripheral::Drive;
use mc683xx_board::peripheral::Lane;
use mc683xx_board::peripheral::Peripheral;
use mc683xx_board::peripheral::PeripheralAccess;
use mc683xx_board::peripheral::Transfer;
use mc683xx_board::registry::Constructed;
use mc683xx_board::registry::Environment;
use mc683xx_board::registry::PortSpec;
use mc683xx_board::registry::Registry;
use mc683xx_board::signal::Direction;
use mc683xx_board::signal::Kind;
use mc683xx_board::signal::Serial;
use mc683xx_board::signal::Signal;
use mc683xx_board::time::PICOSECONDS_PER_SECOND;
use mc683xx_board::time::Time;
use mc683xx_board::time::TimeBase;
use mc683xx_board::trace::Record;
use mc683xx_cpu32::cpu::Stop;
use mc683xx_cpu32::cpu::state::Save;
use mc683xx_cpu32::cpu::state::StateError;
use mc683xx_cpu32::cpu::state::decode;
use mc683xx_cpu32::cpu::state::encode;
use mc683xx_cpu32::cycles::Cycles;
use mc683xx_cpu32::frequency::Frequency;
use mc683xx_cpu32::peripheral::BusCycle;
use mc683xx_cpu32::peripheral::MappedPeripheral;
use mc683xx_cpu32::registers::Registers;
use mc683xx_cpu32::size::Size;
use mc683xx_mc68331::chip::Builder;
use mc683xx_mc68331::chip::Mc68331;
use mc683xx_mc68331::event::DsackFault;
use mc683xx_mc68331::event::Event;
use mc683xx_mc68331::gpt::Gpt;
use mc683xx_modules::chip_select::ChipSelect;
use mc683xx_modules::chip_select::Number;
use mc683xx_modules::module::Module;
use mc683xx_modules::peripheral::ClockPhase;
use mc683xx_modules::peripheral::ClockPolarity;
use mc683xx_modules::peripheral::QspiTransfer;
use mc683xx_modules::peripheral::SpiPeripheral;
use mc683xx_modules::peripheral::Timed;
use mc683xx_modules::qsm::Qsm;
use mc683xx_modules::qsm::sci::Frame;
use mc683xx_modules::sim::ResetCause;
use snapshot_golden::version_of;

/// The initial stack pointer and program counter in the images.
const STACK: u32 = 0x10_fffe;
/// Where the programs start.
const START: u32 = 0x400;
/// The boot ROM's size.
const ROM_BYTES: usize = 0x1_0000;
/// The SIM's registers with MM set (MC68331UM Table D-1).
const SIM: u32 = 0xff_fa00;
/// The GPT's.
const GPT: u32 = 0xff_f900;
/// CSBAR0 and CSOR0 (MC68331UM Table D-3).
const CSBAR0: u32 = SIM + 0x4c;
/// CSOR0.
const CSOR0: u32 = SIM + 0x4e;
/// CS0 at $100000, a 2-Kbyte block (SIMRM 7.10.2, Table 7-6).
const CS0_BAR: u16 = 0x1000;
/// CS0 at $100000, a 64-Kbyte block holding the stack, its 2 Kbytes
/// of RAM repeating.
const STACK_BAR: u16 = 0x1003;
/// CSBAR1.
const CSBAR1: u32 = SIM + 0x50;
/// CSOR1.
const CSOR1: u32 = SIM + 0x52;
/// CSBAR2.
const CSBAR2: u32 = SIM + 0x54;
/// CSOR2.
const CSOR2: u32 = SIM + 0x56;
/// CS1 at $200000, a 64-Kbyte block (SIMRM 7.10.2, Table 7-6).
const CS1_BAR: u16 = 0x2003;
/// Both bytes, read and write, fast termination, supervisor and user
/// space (SIMRM 7.10.3).
const CS_FAST: u16 = 0x7bb0;
/// CS1's window.
const CS1_BASE: u32 = 0x20_0000;
/// CS2 at $300000, a 64-Kbyte block.
const CS2_BAR: u16 = 0x3003;
/// CS2's window.
const CS2_BASE: u32 = 0x30_0000;
/// TCTL1/TCTL2: EDGE1 rising (GPTRM 3.2).
const TCTL: u32 = GPT + 0x1e;
/// TIC1.
const TIC1: u32 = GPT + 0x0e;
/// MOVE.W #imm,(xxx).L (CPU32RM 4, MOVE).
const MOVE_WORD_TO_ABSOLUTE: u16 = 0x33fc;
/// MOVE.W (xxx).L,(xxx).L.
const MOVE_WORD_ABSOLUTE: u16 = 0x33f9;
/// MOVE.W (xxx).L,D0.
const MOVE_WORD_TO_D0: u16 = 0x3039;
/// BRA.S back over the six words before it.
const BRA_BACK_SIX: u16 = 0x60f2;
/// NOP.
const NOP: u16 = 0x4e71;

/// MOVE.W #`value`,`address`.
fn store(address: u32, value: u16) -> [u16; 4] {
    [
        MOVE_WORD_TO_ABSOLUTE,
        value,
        (address >> 16) as u16,
        address as u16,
    ]
}

/// A ROM image with the reset vectors and `program` at `START`.
fn image(program: &[u16]) -> Vec<u8> {
    let mut image: Vec<u8> = NOP.to_be_bytes().repeat(ROM_BYTES / 2);
    image[0..4].copy_from_slice(&STACK.to_be_bytes());
    image[4..8].copy_from_slice(&START.to_be_bytes());
    for (index, word) in program.iter().enumerate() {
        let at = START as usize + 2 * index;
        image[at..at + 2].copy_from_slice(&word.to_be_bytes());
    }
    image
}

/// Scratch directories made, so that tests running at once never share one.
static DIRECTORIES: AtomicUsize = AtomicUsize::new(0);

/// A scratch directory holding `rom.bin` with `program`.
fn directory(name: &str, program: &[u16]) -> PathBuf {
    let unique = DIRECTORIES.fetch_add(1, Ordering::Relaxed);
    let directory =
        env::temp_dir().join(format!("mc683xx-board-{name}-{}-{unique}", process::id()));
    fs::create_dir_all(&directory).unwrap();
    fs::write(directory.join("rom.bin"), image(program)).unwrap();
    directory
}

/// The board `text` says, its ROM `program`.
fn board(name: &str, text: &str, program: &[u16]) -> Board<Mc68331> {
    let directory = directory(name, program);
    let description = Description::parse(text, &directory, &Options::default()).unwrap();
    let board = Board::<Mc68331>::load(
        &description,
        &Registry::with_builtins(),
        Environment::default(),
    )
    .unwrap();
    fs::remove_dir_all(directory).unwrap();
    board
}

/// CS`number`.
fn cs(number: u8) -> ChipSelect {
    ChipSelect::Numbered(Number::new(number).unwrap())
}

/// A program that maps CS1 and writes two words through it, then reads
/// the first back into D0 forever.
fn writes_through_cs1() -> Vec<u16> {
    [
        store(CSBAR1, CS1_BAR),
        store(CSOR1, CS_FAST),
        store(CS1_BASE, 0x1234),
        store(CS1_BASE + 2, 0x5678),
    ]
    .concat()
    .into_iter()
    .chain([
        MOVE_WORD_TO_D0,
        (CS1_BASE >> 16) as u16,
        CS1_BASE as u16,
        NOP,
        NOP,
        NOP,
        BRA_BACK_SIX,
    ])
    .collect()
}

/// A peripheral on a hand-built chip that keeps what is written to it, as
/// the built-in `registers` does: an 8-bit peripheral, a word cycle's byte
/// on DATA\[15:8\] (SIMRM 5.3.3).
#[derive(Debug, Default)]
struct Words([u8; 4]);

impl MappedPeripheral for Words {
    fn read(&mut self, cycle: BusCycle) -> Result<u32, mc683xx_cpu32::bus::BusError> {
        let byte = u32::from(self.0[(cycle.address & 3) as usize]);
        Ok(match cycle.size {
            Size::Byte => byte,
            Size::Word | Size::Long => byte << 8,
        })
    }

    fn write(&mut self, cycle: BusCycle, value: u32) -> Result<(), mc683xx_cpu32::bus::BusError> {
        self.0[(cycle.address & 3) as usize] = match cycle.size {
            Size::Byte => value as u8,
            Size::Word | Size::Long => (value >> 8) as u8,
        };
        Ok(())
    }
}

impl Timed for Words {}

mc683xx_cpu32::unsavable!(Words, "a word latch");

#[test]
fn a_loaded_board_runs_as_the_same_builder_calls() {
    let program = writes_through_cs1();
    let mut board = board(
        "same",
        r#"
version = 1

[chip]
type = "mc68331"
[[memory]]
name = "rom"
image = "rom.bin"
attach = [{ select = "CSBOOT" }]
[[peripheral]]
name = "words"
type = "registers"
attach = [{ select = "CS1" }]
parameters = { bytes = 4 }
"#,
        &program,
    );
    let mut builder = Builder::default();
    builder
        .memory(image(&program), false, &[ChipSelect::Boot])
        .unwrap();
    builder.peripheral(Words::default(), &[cs(1)]).unwrap();
    let mut chip = builder.build();
    let outcome = board.run_until(Time(1_000_000_000)).unwrap();
    assert_eq!(outcome.stop, Stop::Budget);
    chip.run_for(board.chip().now());
    assert_eq!(board.chip().now(), chip.now());
    assert_eq!(board.chip().cpu().registers, chip.cpu().registers);
    assert_eq!(
        board.chip().cpu().registers.data[0] & 0xffff,
        0x1200,
        "a 16-bit port: the upper byte, the lower not driven"
    );
}

#[test]
fn peripherals_split_a_select_by_role_and_lane() {
    let program: Vec<u16> = [
        store(CSBAR1, CS1_BAR),
        store(CSOR1, CS_FAST),
        store(CS1_BASE + 6, 0xabcd),
    ]
    .concat()
    .into_iter()
    .chain([MOVE_WORD_TO_D0, (CS1_BASE >> 16) as u16, 6, NOP])
    .collect();
    let mut board = board(
        "split",
        r#"
version = 1

[chip]
type = "mc68331"
[[memory]]
name = "rom"
image = "rom.bin"
attach = [{ select = "CSBOOT" }]
[[peripheral]]
name = "upper"
type = "recorder"
attach = [{ select = "CS1", role = "write", lane = "upper", lines = [1, 2] }]
[[peripheral]]
name = "lower"
type = "recorder"
attach = [{ select = "CS1", role = "lower", lines = [1, 2], offset = 0x10 }]
parameters = { value = 0x5a }
"#,
        &program,
    );
    board.run_until(Time(100_000_000)).unwrap();
    let upper = board
        .peripheral::<Recorder>("upper")
        .map(|recorder| recorder.log().to_vec())
        .unwrap();
    assert_eq!(upper.len(), 1, "the write only");
    assert_eq!(upper[0].written, Some(0xab));
    assert_eq!(upper[0].access.address, 3, "ADDR1 and ADDR2 of $6");
    assert_eq!(upper[0].access.lane, Lane::Upper);
    let lower = board
        .peripheral::<Recorder>("lower")
        .map(|recorder| recorder.log().to_vec())
        .unwrap();
    assert_eq!(lower[0].written, Some(0xcd));
    assert_eq!(lower[0].access.address, 0x13);
    assert_eq!(lower[1].written, None, "the read");
    assert_eq!(
        board.chip().cpu().registers.data[0] & 0xffff,
        0x005a,
        "the lower lane read, the upper not driven"
    );
    let snapshot = board.save().expect("recorders save");
    board.restore(&snapshot).unwrap();
}

#[test]
fn a_decoder_splits_a_window_by_address() {
    let program: Vec<u16> = [
        store(CSBAR2, CS2_BAR),
        store(CSOR2, CS_FAST),
        store(CS2_BASE, 0x0001),
        store(CS2_BASE + 0x2000, 0x0002),
        store(CS2_BASE + 0x6000, 0x0003),
    ]
    .concat();
    let mut board = board(
        "decoder",
        r#"
version = 1

[chip]
type = "mc68331"
[[memory]]
name = "rom"
image = "rom.bin"
attach = [{ select = "CSBOOT" }]
[[decoder]]
name = "u1"
select = "CS2"
address_bits = [13, 14]
alias = { "3" = 1 }
[[peripheral]]
name = "zero"
type = "recorder"
attach = [{ decoder = "u1", output = 0, lines = 0 }]
[[peripheral]]
name = "one"
type = "recorder"
attach = [{ decoder = "u1", output = 1, lines = 0 }]
"#,
        &program,
    );
    board.run_until(Time(100_000_000)).unwrap();
    let written = |name| {
        board
            .peripheral::<Recorder>(name)
            .map(|recorder| {
                recorder
                    .log()
                    .iter()
                    .filter_map(|logged| logged.written)
                    .collect::<Vec<_>>()
            })
            .unwrap()
    };
    assert_eq!(written("zero"), vec![1]);
    assert_eq!(written("one"), vec![2, 3], "code 3 aliases output 1");
}

/// The board of the phase tests: a 10 MHz clock with a rising edge at
/// `phase` ps, divided by 1000 onto IC1 and an edge counter, and a
/// program that copies TIC1 to RAM forever.
fn phase_board(phase: u64) -> Board<Mc68331> {
    let ram = 0x10_0000_u32;
    let program: Vec<u16> = [
        store(CSBAR0, CS0_BAR),
        store(CSOR0, CS_FAST),
        store(TCTL, 0x0001),
    ]
    .concat()
    .into_iter()
    .chain([
        MOVE_WORD_ABSOLUTE,
        (TIC1 >> 16) as u16,
        TIC1 as u16,
        (ram >> 16) as u16,
        ram as u16,
        NOP,
        BRA_BACK_SIX,
    ])
    .collect();
    let text = format!(
        r#"
version = 1

[chip]
type = "mc68331"
[[memory]]
name = "rom"
image = "rom.bin"
attach = [{{ select = "CSBOOT" }}]
[[memory]]
name = "ram"
size = 0x800
writable = true
attach = [{{ select = "CS0" }}]
[[peripheral]]
name = "reference"
type = "clock"
parameters = {{ hz = 10_000_000, phase_ps = {phase} }}
[[peripheral]]
name = "pps"
type = "divider"
parameters = {{ divide = 1000 }}
[[peripheral]]
name = "counter"
type = "edge-counter"
[[net]]
name = "ten-megahertz"
type = "clock"
from = "reference.out"
to = ["pps.in"]
[[net]]
name = "one-pps"
type = "edge"
from = "pps.out"
to = ["chip.PGP0", "counter.in"]
"#
    );
    board(&format!("phase-{phase}"), &text, &program)
}

/// The rising edge the divider makes first: input edge 1000.
const FIRST_RISE_PS: u64 = 1000 * 100_000;

#[test]
fn an_edge_between_peripherals_keeps_its_picosecond() {
    for phase in [0, 1, 37, 99_999] {
        let mut board = phase_board(phase);
        board.run_until(Time(FIRST_RISE_PS + 200_000)).unwrap();
        let (count, last) = board
            .peripheral::<EdgeCounter>("counter")
            .map(|counter| (counter.count(), counter.last()))
            .unwrap();
        assert_eq!(count, 1);
        assert_eq!(last, Some(Time(phase + FIRST_RISE_PS)), "phase {phase}");
    }
}

#[test]
fn the_format_matches_its_golden() {
    let mut board = phase_board(37);
    board.run_until(Time(FIRST_RISE_PS + 200_000)).unwrap();
    let builtin = board
        .peripheral::<EdgeCounter>("counter")
        .map(|counter| version_of(&counter.save().unwrap()))
        .unwrap();
    snapshot_golden::check(
        env!("CARGO_MANIFEST_DIR"),
        "board",
        &[
            ("board", mc683xx_board::board::VERSION),
            ("chip", mc683xx_mc68331::state::VERSION),
            ("cpu", mc683xx_cpu32::cpu::state::VERSION),
            (
                "qsm",
                version_of(&Qsm::new(Frequency::from_hz(1)).save().unwrap()),
            ),
            ("gpt", version_of(&Gpt::new(Cycles::ZERO).save().unwrap())),
            ("builtin", builtin),
            ("stateless", mc683xx_cpu32::cpu::state::STATELESS_VERSION),
        ],
        &board.save().unwrap(),
    );
}

/// TIC1 as the program copied it, after the first rising edge.
fn captured(phase: u64) -> u16 {
    let mut board = phase_board(phase);
    // Past the edge by more than a pass of the copying loop.
    board
        .run_until(Time(phase + FIRST_RISE_PS + 50_000_000))
        .unwrap();
    let ram = board.memory("ram").unwrap();
    u16::from_be_bytes([ram[0], ram[1]])
}

#[test]
fn a_picosecond_of_phase_moves_the_capture_at_the_clock_it_crosses() {
    // A clock 4 ps wide holds a TCNT tick boundary; find it by halving.
    let (mut early, mut late) = (0_u64, 600_000_u64);
    let first = captured(early);
    assert!(captured(late) > first, "half a microsecond moves TCNT");
    while late - early > 1 {
        let middle = (early + late) / 2;
        if captured(middle) == first {
            early = middle;
        } else {
            late = middle;
        }
    }
    // The edge at `late` ps lies past a system clock that the edge at
    // `early` ps does not: the capture rounds the edge up to the first
    // clock at or after it (PLAN.md, Peripherals).
    let hz = 8_388_608_u128;
    let clock = |phase: u64| (u128::from(phase + FIRST_RISE_PS) * hz).div_ceil(1_000_000_000_000);
    assert_eq!(clock(late), clock(early) + 1, "early {early} late {late}");
}

#[test]
fn a_traced_run_records_peripherals_exceptions_and_code() {
    // TRAP #0 (CPU32RM 4, TRAP), its vector a ROM NOP pair: the handler
    // faults after the trap's frame is recorded.
    let trap = 0x4e40;
    let program: Vec<u16> = [
        store(CSBAR0, STACK_BAR),
        store(CSOR0, CS_FAST),
        store(CSBAR1, CS1_BAR),
        store(CSOR1, CS_FAST),
        store(CS1_BASE, 0x1234),
    ]
    .concat()
    .into_iter()
    .chain([trap])
    .collect();
    let mut board = board(
        "trace",
        r#"
version = 1

[chip]
type = "mc68331"
[[memory]]
name = "rom"
image = "rom.bin"
attach = [{ select = "CSBOOT" }]
[[memory]]
name = "ram"
size = 0x800
writable = true
attach = [{ select = "CS0" }]
[[peripheral]]
name = "words"
type = "registers"
attach = [{ select = "CS1" }]
parameters = { bytes = 4 }
"#,
        &program,
    );
    let mut tracing = BoardTracing::default();
    tracing.bus_cycles = true;
    tracing.exceptions = true;
    board.set_tracing(tracing);
    board.watch_code(START);
    board.run_until(Time(100_000_000)).unwrap();
    let records = board.take_trace().records;
    let Record::Code(code) = records[0] else {
        panic!("{records:?}");
    };
    assert_eq!((code.pc, code.sr, code.address[7]), (START, 0x2700, STACK));
    let peripheral = records
        .iter()
        .find_map(|record| match record {
            Record::BusCycle(peripheral) => Some(*peripheral),
            _ => None,
        })
        .unwrap();
    assert_eq!(
        Record::BusCycle(peripheral).to_string(),
        format!(
            "bus-cycle cycle={} pc=0x00000420 access=write address=0x00200000 size=2 value=0x1234",
            peripheral.cycle.0
        )
    );
    let exception = records
        .iter()
        .find_map(|record| match record {
            Record::Exception(exception) => Some(*exception),
            _ => None,
        })
        .unwrap();
    assert_eq!(
        (
            exception.vector,
            exception.pc,
            exception.sr,
            exception.level
        ),
        (32, START + 42, Some(0x2700), None)
    );
}

#[test]
fn a_traced_pin_takes_its_rounded_clock() {
    let mut board = phase_board(37);
    let mut tracing = BoardTracing::default();
    tracing.pins = true;
    board.set_tracing(tracing);
    board.run_until(Time(FIRST_RISE_PS + 1_000_000)).unwrap();
    let pins: Vec<String> = board
        .take_trace()
        .records
        .iter()
        .map(ToString::to_string)
        .collect();
    let clock = (u128::from(37 + FIRST_RISE_PS) * 8_388_608).div_ceil(1_000_000_000_000);
    assert_eq!(
        pins.last().unwrap(),
        &format!(
            "pin cycle={clock} time={} pin=PGP0 level=1",
            37 + FIRST_RISE_PS
        )
    );
}

#[test]
fn eclk_is_the_system_clock_divided_from_power_on() {
    let mut board = board(
        "eclk",
        r#"
version = 1

[chip]
type = "mc68331"
[[memory]]
name = "rom"
image = "rom.bin"
attach = [{ select = "CSBOOT" }]
[[peripheral]]
name = "halves"
type = "divider"
parameters = { divide = 2, high = 1 }
[[peripheral]]
name = "counter"
type = "edge-counter"
[[net]]
name = "eclk"
type = "clock"
from = "chip.ECLK"
to = ["halves.in"]
[[net]]
name = "half"
type = "edge"
from = "halves.out"
to = ["counter.in"]
"#,
        &[NOP],
    );
    board.run_until(Time(10_000_000)).unwrap();
    // 8.388608 MHz out of reset / 8: an ECLK period of 953674.316 ps;
    // the divider's first rise is at its second edge, rounded up.
    let first = (2 * 8 * 1_000_000_000_000_u128).div_ceil(8_388_608);
    let (count, last) = board
        .peripheral::<EdgeCounter>("counter")
        .map(|counter| (counter.count(), counter.last()))
        .unwrap();
    assert_eq!(count, 5, "edges 2, 4, 6, 8 and 10 by 10 us");
    let period = |edges: u128| (edges * 8 * 1_000_000_000_000).div_ceil(8_388_608);
    assert_eq!(last, Some(Time(u64::try_from(period(10)).unwrap())));
    assert_eq!(period(2), first);
}

/// SYNCR (MC68331UM Table D-1).
const SYNCR: u32 = SIM + 0x04;
/// SYNCR $CF00: 16.777216 MHz from the 32.768 kHz reference (MC68331UM
/// D.3.3).
const SYNCR_FAST: u16 = 0xcf00;

/// A peripheral that keeps the system clocks it is told and the one its
/// drives carry.
#[derive(Debug, Default)]
struct ClockWatcher {
    /// Each `system_clock` call's base.
    told: Vec<TimeBase>,
    /// The base a drive carried at the last advance.
    driven: Option<TimeBase>,
    /// When it next advances.
    next: Option<Time>,
}

impl Peripheral for ClockWatcher {
    fn system_clock(&mut self, clock: TimeBase) {
        self.told.push(clock);
    }

    fn next_event(&self) -> Option<Time> {
        self.next
    }

    fn advance(&mut self, now: Time, drive: &mut Drive<'_>) {
        self.driven = drive.system_clock();
        self.next = Some(now + 1_000_000);
    }
}

#[test]
fn a_peripheral_follows_the_system_clock() {
    let directory = directory("clock", &store(SYNCR, SYNCR_FAST));
    let text = r#"
version = 1

[chip]
type = "mc68331"
[[memory]]
name = "rom"
image = "rom.bin"
attach = [{ select = "CSBOOT" }]
[[peripheral]]
name = "watcher"
type = "watcher"
"#;
    let description = Description::parse(text, &directory, &Options::default()).unwrap();
    let mut registry = Registry::with_builtins();
    registry
        .register("watcher", Vec::new(), |_| {
            Ok(Box::new(ClockWatcher {
                next: Some(Time::ZERO),
                ..ClockWatcher::default()
            }))
        })
        .unwrap();
    let mut board =
        Board::<Mc68331>::load(&description, &registry, Environment::default()).unwrap();
    fs::remove_dir_all(directory).unwrap();
    assert_eq!(board.system_clock().frequency().hz(), 8_388_608);
    board.run_until(Time(100_000_000)).unwrap();
    let base = board.system_clock();
    assert_eq!(base.frequency().hz(), 16_777_216);
    assert_eq!(base.cycles(base.time(Cycles(12_345))), Cycles(12_345));
    let (told, driven) = board
        .peripheral::<ClockWatcher>("watcher")
        .map(|watcher| (watcher.told.clone(), watcher.driven))
        .unwrap();
    let hz: Vec<u64> = told.iter().map(|base| base.frequency().hz()).collect();
    assert_eq!(hz, [8_388_608, 16_777_216], "at load and at the change");
    assert_eq!(driven, Some(base));
}

/// A host's own peripheral, such as a UI's widget: a lamp on a net
/// whose state the host reads back by its type.
#[derive(Debug, Default)]
struct Widget {
    /// The levels it was driven, in order.
    levels: Vec<bool>,
}

impl Peripheral for Widget {
    fn input(&mut self, _port: usize, signal: Signal, _now: Time, _drive: &mut Drive<'_>) {
        if let Signal::Level(level) = signal {
            self.levels.push(level);
        }
    }
}

#[test]
fn a_host_reaches_its_own_peripheral_by_its_type() {
    let text = r#"
version = 1

[chip]
type = "mc68331"
[[memory]]
name = "ram"
size = 0x800
attach = [{ select = "CSBOOT" }]
[[peripheral]]
name = "lamp"
type = "widget"
[[net]]
name = "light"
type = "logic"
from = "external.switch"
to = ["lamp.in"]
"#;
    let description = Description::parse(text, &env::temp_dir(), &Options::default()).unwrap();
    let mut registry = Registry::with_builtins();
    registry
        .register("widget", vec![PortSpec::input("in", Kind::Logic)], |_| {
            Ok(Box::new(Widget::default()))
        })
        .unwrap();
    let mut board =
        Board::<Mc68331>::load(&description, &registry, Environment::default()).unwrap();
    board.send("switch", Signal::Level(true)).unwrap();
    board.run_until(Time(1_000_000)).unwrap();
    let levels = board
        .peripheral::<Widget>("lamp")
        .map(|widget| widget.levels.clone());
    assert_eq!(levels, Some(vec![true]));
    assert_eq!(board.peripheral::<Recorder>("lamp").map(|_| ()), None);
    assert!(board.peripheral::<Widget>("no such").is_none());
    board
        .peripheral_mut::<Widget>("lamp")
        .unwrap()
        .levels
        .clear();
    board.send("switch", Signal::Level(false)).unwrap();
    board.run_until(Time(2_000_000)).unwrap();
    assert_eq!(board.peripheral::<Widget>("lamp").unwrap().levels, [false]);
    // A boxed peripheral upcasts to `Any` and downcasts to its type.
    let boxed: Box<dyn Peripheral> = Box::new(Widget::default());
    let any: Box<dyn Any> = boxed;
    assert!(any.downcast::<Widget>().is_ok());
}

#[test]
fn a_ported_type_takes_its_ports_from_its_parameters() {
    let text = |inputs: u32, net: &str| {
        format!(
            r#"
version = 1

[chip]
type = "mc68331"
[[memory]]
name = "ram"
size = 0x800
attach = [{{ select = "CSBOOT" }}]
[[peripheral]]
name = "lamps"
type = "lamps"
parameters = {{ inputs = {inputs} }}
[[net]]
name = "light"
type = "logic"
from = "external.switch"
to = ["lamps.{net}"]
"#
        )
    };
    let mut registry = Registry::with_builtins();
    // `inputs` lamps, each its own input; 0 declares one port twice
    registry
        .register_ported("lamps", |context| {
            let inputs: u32 = context.parameters.required("inputs")?;
            let ports = (0..inputs.max(2))
                .map(|index| PortSpec::input(format!("in{}", index.min(inputs)), Kind::Logic))
                .collect();
            Ok(Constructed::new(Box::new(Widget::default()), ports))
        })
        .unwrap();
    let load = |inputs, net: &str| {
        let description =
            Description::parse(&text(inputs, net), &env::temp_dir(), &Options::default())?;
        Board::<Mc68331>::load(&description, &registry, Environment::default())
    };
    let mut board = load(3, "in2").unwrap();
    board.send("switch", Signal::Level(true)).unwrap();
    board.run_until(Time(1_000_000)).unwrap();
    let levels = board
        .peripheral::<Widget>("lamps")
        .map(|widget| widget.levels.clone());
    assert_eq!(levels, Some(vec![true]));
    assert!(matches!(load(2, "in3"), Err(LoadError::Endpoint { .. })));
    assert!(matches!(
        load(0, "in0"),
        Err(LoadError::Port { peripheral, port }) if peripheral == "lamps" && port == "in0"
    ));
}

/// A chip-level model with an event every `PERIOD` system clocks, and
/// the clocks it was advanced at.
#[derive(Debug, Default)]
struct Periodic {
    /// Each advance's time.
    advanced: Vec<Cycles>,
}

/// `Periodic`'s period.
const PERIOD: Cycles = Cycles(50_000);

mc683xx_cpu32::unsavable!(Periodic, "a periodic model");

impl MappedPeripheral for Periodic {
    fn read(&mut self, _: BusCycle) -> Result<u32, mc683xx_cpu32::bus::BusError> {
        Ok(0)
    }

    fn write(&mut self, _: BusCycle, _: u32) -> Result<(), mc683xx_cpu32::bus::BusError> {
        Ok(())
    }
}

impl Timed for Periodic {
    fn next_event(&self) -> Option<Cycles> {
        Some(Cycles(PERIOD.0 * (self.advanced.len() as u64 + 1)))
    }

    fn advance(&mut self, now: Cycles) {
        if now >= self.next_event().unwrap() {
            self.advanced.push(now);
        }
    }
}

#[test]
fn a_model_in_clocks_keeps_them_across_a_syncr_write() {
    let directory = directory("clock-model", &store(SYNCR, SYNCR_FAST));
    let text = r#"
version = 1

[chip]
type = "mc68331"
[[memory]]
name = "rom"
image = "rom.bin"
attach = [{ select = "CSBOOT" }]
[[peripheral]]
name = "periodic"
type = "periodic"
"#;
    let description = Description::parse(text, &directory, &Options::default()).unwrap();
    let mut registry = Registry::with_builtins();
    registry
        .register("periodic", Vec::new(), |_| {
            Ok(Box::new(MappedAdapter::new(Periodic::default())))
        })
        .unwrap();
    let mut board =
        Board::<Mc68331>::load(&description, &registry, Environment::default()).unwrap();
    fs::remove_dir_all(directory).unwrap();
    let end = Time(100_000_000_000);
    board.run_until(end).unwrap();
    // the write is the first instruction, after the reset's few hundred
    // clocks at 8.388608 MHz: the rest of the run is at 16.777216 MHz,
    // board time moving at the new rate from the write, not from the
    // end of its quantum (the model's first event, 50000 clocks on)
    let cycles = board.chip().now();
    let expected = 16_777_216 / 10;
    assert!(
        cycles.0 < expected && expected - cycles.0 < 1_000,
        "{cycles:?} clocks in 100 ms, not about {expected}"
    );
    let advanced = board
        .peripheral::<MappedAdapter<Periodic>>("periodic")
        .map(|peripheral| peripheral.peripheral().advanced.clone())
        .unwrap();
    // each advance at its event's clock exactly, before and after
    assert_eq!(advanced.len() as u64, cycles.0 / PERIOD.0);
    for (index, at) in advanced.iter().enumerate() {
        assert_eq!(*at, Cycles(PERIOD.0 * (index as u64 + 1)));
    }
}

/// The QSM's registers with MM set (MC68331UM Table D-1).
const QSM: u32 = 0xff_fc00;

/// A peripheral on the QSPI that keeps its selects and transfers.
#[derive(Debug, Default)]
struct Framed {
    /// Each `select` call's attachment and level.
    selects: Vec<(usize, bool)>,
    /// Each transfer's entry and PCS levels.
    transfers: Vec<(u8, u8)>,
}

impl Peripheral for Framed {
    fn transfer(&mut self, transfer: Transfer, _: &mut Drive<'_>) -> u16 {
        self.transfers.push((transfer.entry, transfer.chip_selects));
        0
    }

    fn select(&mut self, attachment: usize, selected: bool, _: Time, _: &mut Drive<'_>) {
        self.selects.push((attachment, selected));
    }
}

#[test]
fn a_qspi_peripheral_sees_its_own_select_and_its_entries() {
    let mut program: Vec<u16> = Vec::new();
    // PORTQS high, PCS[3:0] the QSPI's outputs; entries to PCS 0101 and
    // 1010 without CONT, ENDQP 1, SPBR 2 (QSMRM 4.3)
    program.extend(store(QSM + 0x14, 0x0078));
    program.extend(store(QSM + 0x16, 0x7878));
    program.extend(store(QSM + 0x140, 0x050a));
    program.extend(store(QSM + 0x1c, 0x0100));
    program.extend(store(QSM + 0x18, 0x8002));
    program.extend(store(QSM + 0x1a, 0x8000));
    let directory = directory("framed", &program);
    let text = r#"
version = 1

[chip]
type = "mc68331"
[[memory]]
name = "rom"
image = "rom.bin"
attach = [{ select = "CSBOOT" }]
[[peripheral]]
name = "first"
type = "framed"
attach = [{ qspi = 0b0101 }]
[[peripheral]]
name = "second"
type = "framed"
attach = [{ qspi = 0b1010 }]
"#;
    let description = Description::parse(text, &directory, &Options::default()).unwrap();
    let mut registry = Registry::with_builtins();
    registry
        .register("framed", Vec::new(), |_| Ok(Box::new(Framed::default())))
        .unwrap();
    let mut board =
        Board::<Mc68331>::load(&description, &registry, Environment::default()).unwrap();
    fs::remove_dir_all(directory).unwrap();
    board.run_until(Time(1_000_000_000)).unwrap();
    let seen = |name: &str| {
        board
            .peripheral::<Framed>(name)
            .map(|peripheral| (peripheral.selects.clone(), peripheral.transfers.clone()))
            .unwrap()
    };
    assert_eq!(
        seen("first"),
        (vec![(0, true), (0, false)], vec![(0, 0b0101)])
    );
    assert_eq!(
        seen("second"),
        (vec![(0, true), (0, false)], vec![(1, 0b1010)])
    );
}

/// A peripheral on the QSPI that keeps each transfer's mode and whether it
/// starts a frame.
#[derive(Debug, Default)]
struct Clocked {
    /// Each transfer's CPOL, CPHA and frame start.
    transfers: Vec<(ClockPolarity, ClockPhase, bool)>,
}

impl Peripheral for Clocked {
    fn transfer(&mut self, transfer: Transfer, _: &mut Drive<'_>) -> u16 {
        let clock = transfer.clock;
        self.transfers
            .push((clock.polarity, clock.phase, transfer.starts_frame));
        0
    }

    fn save(&self) -> Result<Vec<u8>, StateError> {
        Ok(Vec::new())
    }

    fn restore(&mut self, _: &[u8]) -> Result<(), StateError> {
        Ok(())
    }
}

/// A board with a `Clocked` peripheral on PCS 0101 whose program runs three
/// entries to it, the first two with CONT, at CPOL 1 and CPHA 1.
fn clocked_board(name: &str) -> Board<Mc68331> {
    let mut program: Vec<u16> = Vec::new();
    // PORTQS high, PCS[3:0] the QSPI's outputs; entries 0 and 1 with
    // CONT, 2 without; ENDQP 2; MSTR, CPOL, CPHA, SPBR 2 (QSMRM 4.3)
    program.extend(store(QSM + 0x14, 0x0078));
    program.extend(store(QSM + 0x16, 0x7878));
    program.extend(store(QSM + 0x140, 0x8585));
    program.extend(store(QSM + 0x142, 0x0500));
    program.extend(store(QSM + 0x1c, 0x0200));
    program.extend(store(QSM + 0x18, 0x8302));
    program.extend(store(QSM + 0x1a, 0x8000));
    let text = r#"
version = 1

[chip]
type = "mc68331"
[[memory]]
name = "rom"
image = "rom.bin"
attach = [{ select = "CSBOOT" }]
[[peripheral]]
name = "clocked"
type = "clocked"
attach = [{ qspi = 0b0101 }]
"#;
    let directory = directory(name, &program);
    let description = Description::parse(text, &directory, &Options::default()).unwrap();
    let mut registry = Registry::with_builtins();
    registry
        .register("clocked", Vec::new(), |_| Ok(Box::new(Clocked::default())))
        .unwrap();
    let board = Board::<Mc68331>::load(&description, &registry, Environment::default()).unwrap();
    fs::remove_dir_all(directory).unwrap();
    board
}

#[test]
fn a_qspi_peripheral_sees_each_transfers_mode_and_frame() {
    let mut board = clocked_board("clocked");
    board.run_until(Time(1_000_000_000)).unwrap();
    let transfers = board
        .peripheral::<Clocked>("clocked")
        .map(|peripheral| peripheral.transfers.clone())
        .unwrap();
    let (polarity, phase) = (ClockPolarity::IdleHigh, ClockPhase::ChangeLeading);
    assert_eq!(
        transfers,
        [
            (polarity, phase, true),
            (polarity, phase, false),
            (polarity, phase, false)
        ]
    );
}

#[test]
fn the_pcs_decode_is_in_the_boards_snapshot() {
    let mut board = clocked_board("clocked-saved");
    board.run_until(Time(1_000_000_000)).unwrap();
    let snapshot = board.save().unwrap();
    // a fresh board has not seen the clock, the selects or the frame
    let mut other = clocked_board("clocked-restored");
    assert_ne!(other.save().unwrap(), snapshot);
    other.restore(&snapshot).unwrap();
    assert_eq!(other.save().unwrap(), snapshot);
}

/// A latch no reset reaches, whose output powers up high.
#[derive(Debug, Default)]
struct PoweredLatch {
    /// Its resets.
    resets: usize,
}

impl Peripheral for PoweredLatch {
    fn power_on(&mut self, drive: &mut Drive<'_>) {
        drive.drive(0, Signal::Level(true));
    }

    fn reset(&mut self, _: Time, _: &mut Drive<'_>) {
        self.resets += 1;
    }
}

#[test]
fn a_peripheral_drives_its_first_levels_at_power_on() {
    let directory = directory("power", &[NOP]);
    let text = r#"
version = 1

[chip]
type = "mc68331"
[[memory]]
name = "rom"
image = "rom.bin"
attach = [{ select = "CSBOOT" }]
[[peripheral]]
name = "latch"
type = "latch"
reset = []
[[net]]
name = "q"
type = "logic"
from = "latch.q"
to = ["external.q"]
"#;
    let description = Description::parse(text, &directory, &Options::default()).unwrap();
    let mut registry = Registry::with_builtins();
    registry
        .register("latch", vec![PortSpec::output("q", Kind::Logic)], |_| {
            Ok(Box::new(PoweredLatch::default()))
        })
        .unwrap();
    let mut board =
        Board::<Mc68331>::load(&description, &registry, Environment::default()).unwrap();
    fs::remove_dir_all(directory).unwrap();
    board.run_until(Time(1_000)).unwrap();
    assert_eq!(
        board.take_received("q").unwrap(),
        [(Time::ZERO, Signal::Level(true))]
    );
    let resets = board
        .peripheral::<PoweredLatch>("latch")
        .map(|latch| latch.resets);
    assert_eq!(resets, Some(0), "no reset reaches it");
}

/// `Words` with time of its own: an event at `EVENT`, and what it saw.
#[derive(Debug, Default)]
struct TimedWords {
    /// The registers.
    words: Words,
    /// Each cycle's select and start.
    cycles: Vec<(ChipSelect, Cycles)>,
    /// Each advance's time.
    advanced: Vec<Cycles>,
}

/// `TimedWords`' event.
const EVENT: Cycles = Cycles(1_000);

impl MappedPeripheral for TimedWords {
    fn read(&mut self, cycle: BusCycle) -> Result<u32, mc683xx_cpu32::bus::BusError> {
        self.cycles.push((
            cycle.select.and_then(ChipSelect::from_select).unwrap(),
            cycle.now,
        ));
        self.words.read(cycle)
    }

    fn write(&mut self, cycle: BusCycle, value: u32) -> Result<(), mc683xx_cpu32::bus::BusError> {
        self.cycles.push((
            cycle.select.and_then(ChipSelect::from_select).unwrap(),
            cycle.now,
        ));
        self.words.write(cycle, value)
    }
}

mc683xx_cpu32::unsavable!(TimedWords, "a timed word latch");

impl Timed for TimedWords {
    fn next_event(&self) -> Option<Cycles> {
        self.advanced.is_empty().then_some(EVENT)
    }

    fn advance(&mut self, now: Cycles) {
        if self.advanced.is_empty() && now >= EVENT {
            self.advanced.push(now);
        }
    }
}

#[test]
fn a_chip_peripheral_runs_as_a_peripheral_through_the_adapter() {
    let program = writes_through_cs1();
    let directory = directory("adapter", &program);
    let text = r#"
version = 1

[chip]
type = "mc68331"
[[memory]]
name = "rom"
image = "rom.bin"
attach = [{ select = "CSBOOT" }]
[[peripheral]]
name = "words"
type = "timed-words"
attach = [{ select = "CS1" }]
"#;
    let description = Description::parse(text, &directory, &Options::default()).unwrap();
    let mut registry = Registry::with_builtins();
    registry
        .register("timed-words", Vec::new(), |_| {
            Ok(Box::new(MappedAdapter::new(TimedWords::default())))
        })
        .unwrap();
    let mut board =
        Board::<Mc68331>::load(&description, &registry, Environment::default()).unwrap();
    fs::remove_dir_all(directory).unwrap();
    let mut builder = Builder::default();
    builder
        .memory(image(&program), false, &[ChipSelect::Boot])
        .unwrap();
    let id = builder.peripheral(TimedWords::default(), &[cs(1)]).unwrap();
    let mut chip = builder.build();
    board.run_until(Time(1_000_000_000)).unwrap();
    chip.run_for(board.chip().now());
    assert_eq!(board.chip().cpu().registers, chip.cpu().registers);
    assert_eq!(board.chip().cpu().registers.data[0] & 0xffff, 0x1200);
    let (cycles, advanced) = board
        .peripheral::<MappedAdapter<TimedWords>>("words")
        .map(|peripheral| {
            (
                peripheral.peripheral().cycles.clone(),
                peripheral.peripheral().advanced.clone(),
            )
        })
        .unwrap();
    let hand = chip.peripheral::<TimedWords>(id).unwrap();
    assert!(cycles.iter().all(|&(select, _)| select == cs(1)));
    assert_eq!(cycles, hand.cycles, "each cycle at the chip's clock");
    // the board calls advance at the event's time; the chip at the end
    // of the instruction that ran past it
    assert_eq!(advanced, [EVENT], "at its own clock, exactly");
    assert!(hand.advanced[0] >= EVENT);
}

/// A QSPI model that keeps what it is told.
#[derive(Debug, Default)]
struct Dac {
    /// Each transfer's entry and levels.
    transfers: Vec<(u8, u8)>,
    /// Each `chip_selects` call's levels.
    levels: Vec<u8>,
}

mc683xx_cpu32::unsavable!(Dac, "a logging DAC");

impl SpiPeripheral for Dac {
    fn transfer(&mut self, transfer: QspiTransfer) -> u16 {
        self.transfers.push((transfer.entry, transfer.chip_selects));
        0
    }

    fn chip_selects(&mut self, levels: u8, _: Cycles) {
        self.levels.push(levels);
    }
}

#[test]
fn a_qspi_peripheral_runs_as_a_peripheral_through_the_adapter() {
    let mut program: Vec<u16> = Vec::new();
    program.extend(store(QSM + 0x14, 0x0078));
    program.extend(store(QSM + 0x16, 0x7878));
    program.extend(store(QSM + 0x140, 0x050a));
    program.extend(store(QSM + 0x1c, 0x0100));
    program.extend(store(QSM + 0x18, 0x8002));
    program.extend(store(QSM + 0x1a, 0x8000));
    let directory = directory("qspi-adapter", &program);
    let text = r#"
version = 1

[chip]
type = "mc68331"
[[memory]]
name = "rom"
image = "rom.bin"
attach = [{ select = "CSBOOT" }]
[[peripheral]]
name = "dac"
type = "dac"
attach = [{ qspi = 0b1010 }]
"#;
    let description = Description::parse(text, &directory, &Options::default()).unwrap();
    let mut registry = Registry::with_builtins();
    registry
        .register("dac", Vec::new(), |_| {
            Ok(Box::new(SpiAdapter::new(Dac::default())))
        })
        .unwrap();
    let mut board =
        Board::<Mc68331>::load(&description, &registry, Environment::default()).unwrap();
    fs::remove_dir_all(directory).unwrap();
    board.run_until(Time(1_000_000_000)).unwrap();
    let (transfers, levels) = board
        .peripheral::<SpiAdapter<Dac>>("dac")
        .map(|peripheral| {
            (
                peripheral.peripheral().transfers.clone(),
                peripheral.peripheral().levels.clone(),
            )
        })
        .unwrap();
    assert_eq!(transfers, [(1, 0b1010)], "its own entry only");
    assert_eq!(levels, [SELECTED_LEVELS, RELEASED_LEVELS]);
}

/// A model that counts its own periods and reads as the count, and
/// saves.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct Ticker {
    /// The periods past.
    ticks: u32,
    /// The next period's end.
    next: Cycles,
}

/// `Ticker`'s period.
const TICK: Cycles = Cycles(700);
/// `Ticker`'s snapshot format.
const TICKER_VERSION: u32 = 1;

impl MappedPeripheral for Ticker {
    fn read(&mut self, cycle: BusCycle) -> Result<u32, mc683xx_cpu32::bus::BusError> {
        Ok(self.ticks & cycle.size.mask())
    }

    fn write(&mut self, _: BusCycle, _: u32) -> Result<(), mc683xx_cpu32::bus::BusError> {
        Ok(())
    }
}

impl Timed for Ticker {
    fn next_event(&self) -> Option<Cycles> {
        Some(self.next)
    }

    fn advance(&mut self, now: Cycles) {
        while self.next <= now {
            self.ticks += 1;
            self.next += TICK;
        }
    }
}

impl Save for Ticker {
    fn save(&self) -> Result<Vec<u8>, StateError> {
        Ok(encode(TICKER_VERSION, &(self.ticks, self.next.0)))
    }

    fn restore(&mut self, snapshot: &[u8]) -> Result<(), StateError> {
        let (ticks, next): (u32, u64) = decode(TICKER_VERSION, snapshot)?;
        self.ticks = ticks;
        self.next = Cycles(next);
        Ok(())
    }
}

impl SpiPeripheral for Ticker {
    fn transfer(&mut self, _: QspiTransfer) -> u16 {
        self.ticks as u16
    }
}

#[test]
fn an_adapted_model_that_saves_is_in_the_snapshot() {
    let program = writes_through_cs1();
    let directory = directory("adapter-save", &program);
    let text = r#"
version = 1

[chip]
type = "mc68331"
[[memory]]
name = "rom"
image = "rom.bin"
attach = [{ select = "CSBOOT" }]
[[peripheral]]
name = "ticker"
type = "ticker"
attach = [{ select = "CS1" }]
"#;
    let description = Description::parse(text, &directory, &Options::default()).unwrap();
    fs::remove_dir_all(directory).unwrap();
    let load = |saving: bool| {
        let mut registry = Registry::with_builtins();
        registry
            .register("ticker", Vec::new(), move |_| {
                Ok(if saving {
                    Box::new(MappedAdapter::new(Ticker::default()))
                } else {
                    Box::new(MappedAdapter::new(Periodic::default()))
                })
            })
            .unwrap();
        Board::<Mc68331>::load(&description, &registry, Environment::default()).unwrap()
    };
    let ticker = |board: &mut Board<Mc68331>| {
        board
            .peripheral::<MappedAdapter<Ticker>>("ticker")
            .map(|peripheral| peripheral.peripheral().clone())
            .unwrap()
    };
    let mut board = load(true);
    board.run_until(Time(100_000_000)).unwrap();
    let snapshot = board.save().unwrap();
    let saved = ticker(&mut board);
    assert!(saved.ticks > 0);
    board.run_until(Time(200_000_000)).unwrap();
    let after = (ticker(&mut board), board.chip().cpu().registers.clone());
    assert!(after.0.ticks > saved.ticks);
    board.restore(&snapshot).unwrap();
    assert_eq!(ticker(&mut board), saved);
    board.run_until(Time(200_000_000)).unwrap();
    assert_eq!(
        (ticker(&mut board), board.chip().cpu().registers.clone()),
        after
    );
    assert_eq!(after.1.data[0] & 0xffff, after.0.ticks & 0xffff);
    // a model that cannot save cannot restore, and nor can the board,
    // which names it
    let unsupported = StateError::unsupported("a periodic model").of_peripheral("ticker");
    let mut unsaving = load(false);
    assert_eq!(
        unsaving.save().unwrap_err().to_string(),
        unsupported.to_string()
    );
    assert_eq!(
        unsaving.restore(&snapshot).unwrap_err().to_string(),
        unsupported.to_string()
    );
    assert_eq!(
        unsupported.to_string(),
        "ticker: a periodic model cannot save its state"
    );
    // a QSPI model saves as a bus model does; a bad snapshot is refused
    let mut peripheral = SpiAdapter::new(Ticker {
        ticks: 5,
        next: TICK,
    });
    let saved = peripheral.save().unwrap();
    peripheral.peripheral_mut().ticks = 9;
    peripheral.restore(&saved).unwrap();
    assert_eq!(peripheral.peripheral().ticks, 5);
    assert!(peripheral.restore(&[0xff; 4]).is_err());
}

#[test]
fn a_serial_frame_is_stamped_at_its_start_bit() {
    let mut program: Vec<u16> = Vec::new();
    // SCBR 1: 32 clocks a bit; TE; SCSR read, then 'H' to SCDR (QSMRM 5)
    program.extend(store(QSM + 0x08, 1));
    program.extend(store(QSM + 0x0a, 0x0008));
    program.extend([MOVE_WORD_TO_D0, (QSM >> 16) as u16, (QSM + 0x0c) as u16]);
    program.extend(store(QSM + 0x0e, u16::from(b'H')));
    let mut board = board(
        "serial",
        r#"
version = 1

[chip]
type = "mc68331"
[[memory]]
name = "rom"
image = "rom.bin"
attach = [{ select = "CSBOOT" }]
[[net]]
name = "tx"
type = "serial"
from = "chip.TXD"
to = ["external.host"]
"#,
        &program,
    );
    board.run_until(Time(100_000_000)).unwrap();
    let received = board.take_received("host").unwrap();
    let [(at, Signal::Serial(serial))] = received[..] else {
        panic!("one frame: {received:?}");
    };
    assert_eq!(serial.frame, Frame::Data(u16::from(b'H')));
    // 8.388608 MHz out of reset: 262144 baud, ten bits a frame
    assert_eq!(serial.baud, Frequency::from_hz(262_144));
    let base = board.system_clock();
    let start = base.cycles(at);
    assert_eq!(base.time(start), at);
    assert_eq!(base.cycles(serial.end(at)), start + Cycles(320));
    // the preamble's frame went first, as TE set; this one followed it
    assert!(start.0 >= 320, "after the preamble: {start:?}");
}

#[test]
fn externals_say_their_type_and_direction() {
    let board = board(
        "externals",
        r#"
version = 1

[chip]
type = "mc68331"
[[memory]]
name = "rom"
image = "rom.bin"
attach = [{ select = "CSBOOT" }]
[[net]]
name = "tx"
type = "serial"
from = "chip.TXD"
to = ["external.host"]
[[net]]
name = "rx"
type = "serial"
from = "external.host"
to = ["chip.RXD"]
[[net]]
name = "pps"
type = "edge"
from = "external.pps"
to = ["chip.PGP0"]
"#,
        &[],
    );
    let externals: Vec<(String, Kind, Direction)> = board
        .externals()
        .into_iter()
        .map(|external| (external.name, external.kind, external.direction))
        .collect();
    assert_eq!(
        externals,
        [
            ("host".to_owned(), Kind::Serial, Direction::InOut),
            ("pps".to_owned(), Kind::Edge, Direction::Out),
        ]
    );
}

#[test]
fn a_send_or_take_an_endpoint_cannot_do_is_refused() {
    let mut board = board(
        "send",
        r#"
version = 1

[chip]
type = "mc68331"
[[memory]]
name = "rom"
image = "rom.bin"
attach = [{ select = "CSBOOT" }]
[[net]]
name = "tx"
type = "serial"
from = "chip.TXD"
to = ["external.scope"]
[[net]]
name = "pps"
type = "edge"
from = "external.pps"
to = ["chip.PGP0"]
"#,
        &[],
    );
    let frame = Signal::Serial(Serial::untimed(Frame::Data(0x41)));
    assert_eq!(
        board.send("nobody", frame),
        Err(SendError::Endpoint("nobody".to_owned()))
    );
    assert_eq!(
        board.send("scope", frame),
        Err(SendError::Endpoint("scope".to_owned())),
        "it is only driven"
    );
    assert_eq!(
        board.send("pps", frame).unwrap_err().to_string(),
        "external.pps drives edge nets only, which do not carry the value"
    );
    assert_eq!(board.send("pps", Signal::Level(false)), Ok(()));
    assert_eq!(
        board.take_received("nobody"),
        Err(ReceiveError::Endpoint("nobody".to_owned()))
    );
    assert_eq!(
        board.take_received("pps").unwrap_err().to_string(),
        "no net drives external.pps",
        "it only drives"
    );
    assert_eq!(board.take_received("scope"), Ok(Vec::new()));
}

#[test]
fn a_serial_frame_carries_its_length() {
    let frame = Serial::new(Frame::Data(0x41), Frequency::from_hz(9_600), 10);
    // ten bits at 9600 baud: 1.0416666 ms
    assert_eq!(frame.length, 1_041_666_666);
    assert_eq!(frame.end(Time(5)), Time(1_041_666_671));
    assert_eq!(Serial::untimed(Frame::Break).length, 0);
}

/// PEPAR (MC68331UM Table D-1).
const PEPAR: u32 = SIM + 0x16;
/// CS1's option: as `CS_FAST`, but DSACK external (SIMRM 7.10.3).
const CS_EXTERNAL: u16 = 0x7bf0;

#[test]
fn a_dsack_on_a_port_e_pin_ends_no_cycle() {
    let program: Vec<u16> = [
        store(CSBAR1, CS1_BAR),
        store(CSOR1, CS_EXTERNAL),
        store(PEPAR, 0x0000),
    ]
    .concat()
    .into_iter()
    .chain([MOVE_WORD_TO_D0, (CS1_BASE >> 16) as u16, CS1_BASE as u16])
    .collect();
    let mut board = board(
        "dsack",
        r#"
version = 1

[chip]
type = "mc68331"
[[memory]]
name = "rom"
image = "rom.bin"
attach = [{ select = "CSBOOT" }]
[[peripheral]]
name = "duart"
type = "recorder"
attach = [{ select = "CS1" }]
dsack = "DSACK1"
"#,
        &program,
    );
    board.run_until(Time(1_000_000_000)).unwrap();
    let faults: Vec<DsackFault> = board
        .take_events()
        .into_iter()
        .filter_map(|event| match event {
            Event::Dsack(_, fault) => Some(fault),
            _ => None,
        })
        .collect();
    assert_eq!(faults, [DsackFault::Unassigned { select: cs(1) }]);
}

/// CSPAR1 (MC68331UM Table D-1).
const CSPAR1: u32 = SIM + 0x46;
/// CSPAR1 out of reset but CS8 an 8-bit port, %10 in bits 5:4 (SIMRM
/// 7.3.1, Table 7-4).
const CSPAR1_CS8_8_BIT: u16 = 0x03ef;
/// CSBAR8 and CSOR8.
const CSBAR8: u32 = SIM + 0x6c;
/// CSOR8.
const CSOR8: u32 = SIM + 0x6e;
/// The Z3805A's PEPAR: PE1 DSACK1, PE0 a port E pin (SIMRM 9.1).
const PEPAR_DSACK1_ONLY: u16 = 0x0032;
/// MOVE.B (abs).L,D0.
const MOVE_BYTE_TO_D0: u16 = 0x1039;
/// MOVE.W (abs).L,D1.
const MOVE_WORD_TO_D1: u16 = 0x3239;

#[test]
fn a_duart_on_dsack1_is_the_lower_lane_of_a_16_bit_port() {
    // the Z3805A's DUART: CS8's port field 8 bits and its DSACK
    // external, PEPAR assigning DSACK1 alone, so the DUART's DSACK is
    // a 16-bit port (SIMRM Table 5-2) and sizes the cycle (SIMRM
    // 5.3.3); the firmware reads it at odd addresses, DATA7-0
    let program: Vec<u16> = [
        store(CSPAR1, CSPAR1_CS8_8_BIT),
        store(CSBAR8, CS2_BAR),
        store(CSOR8, CS_EXTERNAL),
        store(PEPAR, PEPAR_DSACK1_ONLY),
    ]
    .concat()
    .into_iter()
    .chain([
        MOVE_BYTE_TO_D0,
        (CS2_BASE >> 16) as u16,
        (CS2_BASE + 1) as u16,
        MOVE_WORD_TO_D1,
        (CS2_BASE >> 16) as u16,
        CS2_BASE as u16,
    ])
    .collect();
    let mut board = board(
        "duart-lane",
        r#"
version = 1

[chip]
type = "mc68331"
[[memory]]
name = "rom"
image = "rom.bin"
attach = [{ select = "CSBOOT" }]
[[peripheral]]
name = "duart"
type = "recorder"
attach = [{ select = "CS8", lane = "lower" }]
parameters = { value = 0xa5 }
dsack = "both"
"#,
        &program,
    );
    board.run_until(Time(1_000_000_000)).unwrap();
    assert!(
        !board
            .take_events()
            .iter()
            .any(|event| matches!(event, Event::Dsack(..))),
        "DSACK1 ends every cycle"
    );
    let registers = &board.chip().cpu().registers;
    assert_eq!(registers.data[0] & 0xff, 0xa5);
    assert_eq!(
        registers.data[1] & 0xffff,
        0x00a5,
        "the upper lane undriven"
    );
    let accesses: Vec<(u32, Size)> = board
        .peripheral::<Recorder>("duart")
        .map(|duart| {
            duart
                .log()
                .iter()
                .map(|logged| (logged.access.address, logged.access.size))
                .collect()
        })
        .unwrap();
    // the word is one 16-bit cycle, its lower byte the DUART's; sized
    // from CS8's field it would be two byte cycles, the DUART's at the
    // odd address
    assert_eq!(
        accesses,
        [(CS2_BASE + 1, Size::Byte), (CS2_BASE, Size::Byte)]
    );
}

/// CSBAR2.
const CS2_BAR_OFFSET: u32 = SIM + 0x54;
/// CSOR2.
const CS2_OR_OFFSET: u32 = SIM + 0x56;
/// An Am29F010-style flash's first unlock cycle: $AA at $555.
const UNLOCK_FIRST: (u32, u32) = (0x555, 0xaa);
/// Its second: $55 at $2AA.
const UNLOCK_SECOND: (u32, u32) = (0x2aa, 0x55);
/// Its program command, at $555.
const PROGRAM: u32 = 0xa0;
/// DQ7, data polling: the complement of the bit being programmed.
const DQ7: u32 = 0x80;
/// How long a word takes to program: 1 ms, longer than a real part's
/// microseconds, so that the slow boot ROM's loop polls it.
const PROGRAM_PICOSECONDS: u64 = 1_000_000_000;

/// Where the flash's command logic is in a command sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FlashState {
    /// Reading the array.
    Read,
    /// The first unlock cycle seen.
    Unlocked,
    /// Both seen.
    Commanded,
    /// The program command seen: the next write is the word.
    Program,
    /// Programming the word at `offset` until `until`.
    Busy {
        /// The word's offset in the array.
        offset: u32,
        /// The word.
        data: u32,
        /// When it is done.
        until: Time,
    },
}

/// A minimal flash's command logic on the write select: it programs a
/// word into the array, the board memory `flash`, and answers status
/// reads of the array while it does.
#[derive(Debug)]
struct Flash {
    /// Its state.
    state: FlashState,
    /// The status reads it answered.
    status: Vec<u32>,
}

impl Peripheral for Flash {
    fn write(
        &mut self,
        access: PeripheralAccess,
        value: u32,
        drive: &mut Drive<'_>,
    ) -> Result<(), mc683xx_cpu32::bus::BusError> {
        let command = (access.address, value & 0xff);
        self.state = match (self.state, command) {
            (FlashState::Read, UNLOCK_FIRST) => FlashState::Unlocked,
            (FlashState::Unlocked, UNLOCK_SECOND) => FlashState::Commanded,
            (FlashState::Commanded, (0x555, PROGRAM)) => FlashState::Program,
            (FlashState::Program, _) => {
                let offset = 2 * access.address;
                drive.write_memory(
                    "flash",
                    offset as usize,
                    (value as u16).to_be_bytes().to_vec(),
                );
                drive.take_memory("flash");
                FlashState::Busy {
                    offset,
                    data: value,
                    until: access.now + PROGRAM_PICOSECONDS,
                }
            }
            (busy @ FlashState::Busy { .. }, _) => busy,
            _ => FlashState::Read,
        };
        Ok(())
    }

    fn memory_read(
        &mut self,
        memory: &str,
        _: PeripheralAccess,
        _: &mut Drive<'_>,
    ) -> Result<u32, mc683xx_cpu32::bus::BusError> {
        assert_eq!(memory, "flash");
        let FlashState::Busy { data, .. } = self.state else {
            panic!("a read while not busy reached the peripheral");
        };
        let status = !data & DQ7;
        self.status.push(status);
        Ok(status)
    }

    fn next_event(&self) -> Option<Time> {
        match self.state {
            FlashState::Busy { until, .. } => Some(until),
            _ => None,
        }
    }

    fn advance(&mut self, _: Time, drive: &mut Drive<'_>) {
        drive.release_memory("flash");
        self.state = FlashState::Read;
    }
}

#[test]
fn a_flash_answers_status_reads_while_it_programs() {
    let command = |(address, value): (u32, u32)| store(CS2_BASE + 2 * address, value as u16);
    let program: Vec<u16> = [
        store(CSBAR1, CS1_BAR),
        store(CSOR1, CS_FAST),
        store(CS2_BAR_OFFSET, CS2_BAR),
        store(CS2_OR_OFFSET, CS_FAST),
        command(UNLOCK_FIRST),
        command(UNLOCK_SECOND),
        command((0x555, PROGRAM)),
        store(CS2_BASE + 0x20, 0x1234),
    ]
    .concat()
    .into_iter()
    .chain([
        MOVE_WORD_TO_D0,
        (CS1_BASE >> 16) as u16,
        (CS1_BASE + 0x20) as u16,
        NOP,
        NOP,
        NOP,
        BRA_BACK_SIX,
    ])
    .collect();
    let directory = directory("flash", &program);
    let text = r#"
version = 1

[chip]
type = "mc68331"
[[memory]]
name = "rom"
image = "rom.bin"
attach = [{ select = "CSBOOT" }]
[[memory]]
name = "flash"
size = 0x10000
fill = 0xff
attach = [{ select = "CS1", role = "read" }]
[[peripheral]]
name = "commands"
type = "flash"
attach = [{ select = "CS2", role = "write", lines = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16] }]
"#;
    let description = Description::parse(text, &directory, &Options::default()).unwrap();
    let mut registry = Registry::with_builtins();
    registry
        .register("flash", Vec::new(), |_| {
            Ok(Box::new(Flash {
                state: FlashState::Read,
                status: Vec::new(),
            }))
        })
        .unwrap();
    let mut board =
        Board::<Mc68331>::load(&description, &registry, Environment::default()).unwrap();
    fs::remove_dir_all(directory).unwrap();
    board.run_until(Time(500_000_000)).unwrap();
    // polling during the program: DQ7 the complement of the data's
    assert_eq!(board.chip().cpu().registers.data[0] & 0xffff, DQ7);
    let snapshot = board.save();
    board.run_until(Time(5_000_000_000)).unwrap();
    let status = board
        .peripheral::<Flash>("commands")
        .map(|flash| flash.status.clone())
        .unwrap();
    assert!(!status.is_empty() && status.iter().all(|&status| status == DQ7));
    assert_eq!(&board.memory("flash").unwrap()[0x20..0x22], [0x12, 0x34]);
    assert_eq!(
        board.chip().cpu().registers.data[0] & 0xffff,
        0x1234,
        "the array, once programmed"
    );
    assert!(snapshot.is_err(), "the test peripheral cannot save");
}

/// When `Overwriter` acts on its memory, between the chip's quanta.
const OVERWRITE_AT: Time = Time(MILLISECOND);

/// A peripheral that, at `OVERWRITE_AT`, writes $5678 at the start of the
/// memory `ram`, or takes it and answers its reads $1234.
#[derive(Debug)]
struct Overwriter {
    /// Whether it takes the memory rather than writing it.
    takes: bool,
    /// Whether it has acted.
    done: bool,
}

impl Peripheral for Overwriter {
    fn next_event(&self) -> Option<Time> {
        (!self.done).then_some(OVERWRITE_AT)
    }

    fn advance(&mut self, _: Time, drive: &mut Drive<'_>) {
        self.done = true;
        if self.takes {
            drive.take_memory("ram");
        } else {
            drive.write_memory("ram", 0, vec![0x56, 0x78]);
        }
    }

    fn memory_read(
        &mut self,
        _: &str,
        _: PeripheralAccess,
        _: &mut Drive<'_>,
    ) -> Result<u32, mc683xx_cpu32::bus::BusError> {
        Ok(0x1234)
    }

    fn save(&self) -> Result<Vec<u8>, StateError> {
        Ok(vec![u8::from(self.done)])
    }

    fn restore(&mut self, snapshot: &[u8]) -> Result<(), StateError> {
        self.done = snapshot == [1];
        Ok(())
    }
}

/// A board whose firmware reads the first word of CS0's RAM into D0 for
/// ever, an `Overwriter` acting on that RAM.
fn overwritten(takes: bool) -> Board<Mc68331> {
    let ram = 0x10_0000_u32;
    let program: Vec<u16> = [store(CSBAR0, CS0_BAR), store(CSOR0, CS_FAST)]
        .concat()
        .into_iter()
        // MOVE.W (ram).L,D0; BRA.S back to it
        .chain([MOVE_WORD_TO_D0, (ram >> 16) as u16, ram as u16, 0x60f8])
        .collect();
    let directory = directory("overwritten", &program);
    let text = r#"
version = 1

[chip]
type = "mc68331"
[[memory]]
name = "rom"
image = "rom.bin"
attach = [{ select = "CSBOOT" }]
[[memory]]
name = "ram"
size = 0x800
writable = true
attach = [{ select = "CS0" }]
[[peripheral]]
name = "overwriter"
type = "overwriter"
"#;
    let description = Description::parse(text, &directory, &Options::default()).unwrap();
    fs::remove_dir_all(directory).unwrap();
    let mut registry = Registry::with_builtins();
    registry
        .register("overwriter", Vec::new(), move |_| {
            Ok(Box::new(Overwriter { takes, done: false }))
        })
        .unwrap();
    Board::<Mc68331>::load(&description, &registry, Environment::default()).unwrap()
}

/// D0's low word.
fn d0(board: &Board<Mc68331>) -> u32 {
    board.chip().cpu().registers.data[0] & 0xffff
}

#[test]
fn a_memory_write_between_quanta_reaches_the_next_and_the_snapshot() {
    let soon = OVERWRITE_AT + 10_000_000;
    let mut board = overwritten(false);
    board.run_until(soon).unwrap();
    assert_eq!(d0(&board), 0x5678, "the quantum after the write reads it");
    let mut cut = overwritten(false);
    cut.run_until(OVERWRITE_AT).unwrap();
    let snapshot = cut.save().unwrap();
    assert_eq!(&cut.memory("ram").unwrap()[..2], [0x56, 0x78]);
    let mut restored = overwritten(false);
    restored.restore(&snapshot).unwrap();
    restored.run_until(soon).unwrap();
    assert_eq!(d0(&restored), 0x5678, "the snapshot holds the write");
}

#[test]
fn a_memory_taken_between_quanta_is_in_the_snapshot() {
    let soon = OVERWRITE_AT + 10_000_000;
    let mut board = overwritten(true);
    board.run_until(soon).unwrap();
    assert_eq!(d0(&board), 0x1234, "the peripheral answers");
    let mut cut = overwritten(true);
    cut.run_until(OVERWRITE_AT).unwrap();
    let snapshot = cut.save().unwrap();
    let mut restored = overwritten(true);
    restored.restore(&snapshot).unwrap();
    restored.run_until(soon).unwrap();
    assert_eq!(d0(&restored), 0x1234, "the snapshot holds the diversion");
}

#[test]
fn parameters_set_at_run_time_are_checked_as_the_files() {
    let text = r#"
version = 1

[chip]
type = "mc68331"
[[memory]]
name = "rom"
image = "rom.bin"
attach = [{ select = "CSBOOT" }]
[[peripheral]]
name = "d"
type = "fixed"
parameters = { value = 1 }
"#;
    let directory = directory("run-time-parameters", &[]);
    let load = |setting: &str| {
        let mut options = Options::default();
        options.set(setting).unwrap();
        let description = Description::parse(text, &directory, &options).unwrap();
        Board::<Mc68331>::load(
            &description,
            &Registry::with_builtins(),
            Environment::default(),
        )
    };
    assert!(load("d.value=2").is_ok());
    let refused = |setting: &str| load(setting).unwrap_err().to_string();
    assert_eq!(refused("d.stray=1"), "d: unknown parameter \"stray\"");
    assert_eq!(
        refused("d.value=text"),
        "d: parameter \"value\" is not an integer"
    );
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn a_register_file_is_no_larger_than_the_address_space() {
    let text = r#"
version = 1

[chip]
type = "mc68331"
[[memory]]
name = "rom"
image = "rom.bin"
attach = [{ select = "CSBOOT" }]
[[peripheral]]
name = "r"
type = "registers"
parameters = { bytes = 0x80000000 }
"#;
    let directory = directory("huge-registers", &[]);
    let description = Description::parse(text, &directory, &Options::default()).unwrap();
    fs::remove_dir_all(directory).unwrap();
    let error = Board::<Mc68331>::load(
        &description,
        &Registry::with_builtins(),
        Environment::default(),
    )
    .unwrap_err();
    assert!(
        error.to_string().contains("more than ADDR[23:0] reach"),
        "{error}"
    );
}

/// A peripheral with `events` events at `AT`, one an advance, then, if
/// `moves`, none.
#[derive(Debug)]
struct Events {
    /// The events left.
    events: u32,
    /// Whether its last advance clears its next event.
    moves: bool,
}

/// `Events`' time.
const AT: Time = Time(1_000_000);

impl Peripheral for Events {
    fn next_event(&self) -> Option<Time> {
        (self.events > 0 || !self.moves).then_some(AT)
    }

    fn advance(&mut self, _: Time, _: &mut Drive<'_>) {
        self.events = self.events.saturating_sub(1);
    }
}

#[test]
fn a_peripheral_that_never_moves_its_next_event_stalls_the_run() {
    let directory = directory("stall", &[]);
    let text = r#"
version = 1

[chip]
type = "mc68331"
[[memory]]
name = "rom"
image = "rom.bin"
attach = [{ select = "CSBOOT" }]
[[peripheral]]
name = "events"
type = "events"
"#;
    let description = Description::parse(text, &directory, &Options::default()).unwrap();
    fs::remove_dir_all(directory).unwrap();
    let load = |events: u32, moves: bool| {
        let mut registry = Registry::with_builtins();
        registry
            .register("events", Vec::new(), move |_| {
                Ok(Box::new(Events { events, moves }))
            })
            .unwrap();
        Board::<Mc68331>::load(&description, &registry, Environment::default()).unwrap()
    };
    // several events at one time, each an advance, then none
    let mut board = load(10, true);
    assert_eq!(board.run_until(Time(2_000_000)).unwrap().stop, Stop::Budget);
    assert_eq!(
        board
            .peripheral::<Events>("events")
            .map(|peripheral| peripheral.events)
            .unwrap(),
        0
    );
    // advance never moves it: a typed error naming the peripheral, at its time
    let mut board = load(0, false);
    assert_eq!(
        board.run_until(Time(2_000_000)),
        Err(RunError::Stalled {
            peripheral: "events".to_owned(),
            at: AT,
            repeats: 1_000,
        })
    );
}

/// A peripheral that asks the run to stop: at `AT` from `advance` if
/// `timed`, and at each write through its select.
#[derive(Debug)]
struct Stopper {
    /// Whether its event at `AT` is still to come.
    timed: bool,
}

impl Peripheral for Stopper {
    fn next_event(&self) -> Option<Time> {
        self.timed.then_some(AT)
    }

    fn advance(&mut self, _: Time, drive: &mut Drive<'_>) {
        self.timed = false;
        drive.stop();
    }

    fn write(
        &mut self,
        _: PeripheralAccess,
        _: u32,
        drive: &mut Drive<'_>,
    ) -> Result<(), mc683xx_cpu32::bus::BusError> {
        drive.stop();
        Ok(())
    }
}

/// A board with a `Stopper` on CS1, its event at `AT` if `timed`,
/// running `program`.
fn stopper_board(name: &str, timed: bool, program: &[u16]) -> Board<Mc68331> {
    let directory = directory(name, program);
    let text = r#"
version = 1

[chip]
type = "mc68331"
[[memory]]
name = "rom"
image = "rom.bin"
attach = [{ select = "CSBOOT" }]
[[peripheral]]
name = "stopper"
type = "stopper"
attach = [{ select = "CS1" }]
"#;
    let description = Description::parse(text, &directory, &Options::default()).unwrap();
    fs::remove_dir_all(directory).unwrap();
    let mut registry = Registry::with_builtins();
    registry
        .register("stopper", Vec::new(), move |_| {
            Ok(Box::new(Stopper { timed }))
        })
        .unwrap();
    Board::<Mc68331>::load(&description, &registry, Environment::default()).unwrap()
}

#[test]
fn a_stop_asked_between_quanta_stops_one_run_only() {
    let mut board = stopper_board("stop-advance", true, &[BRA_SELF]);
    let stopped = board.run_for(10_000_000_000).unwrap();
    assert_eq!((stopped.stop, stopped.now), (Stop::Requested, AT));
    let instructions = board.chip().instructions();
    let ran = board.run_for(10_000_000_000).unwrap();
    assert_eq!(ran.stop, Stop::Budget, "the stop is not left pending");
    assert_eq!(ran.now, AT + 10_000_000_000);
    assert!(board.chip().instructions() > instructions + 1);
}

#[test]
fn a_run_stopped_by_an_instruction_past_its_end_ends_at_its_end() {
    let program: Vec<u16> = [
        store(CSBAR1, CS1_BAR),
        store(CSOR1, CS_FAST),
        store(CS1_BASE, 0x1234),
    ]
    .concat()
    .into_iter()
    .chain([BRA_SELF])
    .collect();
    let stopped = stopper_board("stop-write", false, &program)
        .run_for(10_000_000_000)
        .unwrap();
    assert_eq!(stopped.stop, Stop::Requested);
    // the write's instruction starts before `until` and ends after it
    let until = Time(stopped.now.0 - 1);
    let mut board = stopper_board("stop-write", false, &program);
    let outcome = board.run_until(until).unwrap();
    assert_eq!((outcome.stop, outcome.now), (Stop::Requested, until));
    assert_eq!(board.now(), until);
}

/// SPCR2 (QSMRM 4.2.4) for a queue of all 16 entries, NEWQP 0 and
/// ENDQP 15.
const SPCR2_ALL_ENTRIES: u16 = 0x0f00;
/// WREN in SPCR2: the queue wraps to entry 0 for ever.
const SPCR2_WREN: u16 = 0x4000;
/// SPSR, the byte (QSMRM 4.2.5); SPIF is its sign bit.
const SPSR: u32 = QSM + 0x1f;
/// ADDQ.L #1,D1.
const ADDQ_ONE_TO_D1: u16 = 0x5281;
/// TST.B (xxx).L.
const TST_BYTE_ABSOLUTE: u16 = 0x4a39;
/// BPL.S back over the four words before it.
const BPL_BACK_FOUR: u16 = 0x6af6;
/// BRA.S to itself.
const BRA_SELF: u16 = 0x60fe;

/// PCS\[3:0\] the QSPI's; the command RAM's 16 entries to PCS 1010 and
/// 0101 in turn, each 8 bits without CONT; `spcr2`, SPBR 2; then SPE
/// (QSMRM 4.3).
fn queue_program(spcr2: u16) -> Vec<u16> {
    let mut program: Vec<u16> = Vec::new();
    program.extend(store(QSM + 0x14, 0x0078));
    program.extend(store(QSM + 0x16, 0x7878));
    for offset in (0..16).step_by(2) {
        program.extend(store(QSM + 0x140 + offset, 0x0a05));
    }
    program.extend(store(QSM + 0x1c, spcr2));
    program.extend(store(QSM + 0x18, 0x8002));
    program.extend(store(QSM + 0x1a, 0x8000));
    program
}

/// A QSPI model that keeps each call with its time, and says whether
/// its calls may come late.
#[derive(Debug, Default)]
struct Logged {
    /// Each transfer's entry, levels and time.
    transfers: Vec<(u8, u8, Cycles)>,
    /// Each `chip_selects` call's levels and time.
    levels: Vec<(u8, Cycles)>,
    /// What `deferrable` returns.
    deferrable: bool,
}

mc683xx_cpu32::unsavable!(Logged, "a logging QSPI model");

impl SpiPeripheral for Logged {
    fn transfer(&mut self, transfer: QspiTransfer) -> u16 {
        self.transfers
            .push((transfer.entry, transfer.chip_selects, transfer.now));
        !transfer.out
    }

    fn chip_selects(&mut self, levels: u8, now: Cycles) {
        self.levels.push((levels, now));
    }

    fn deferrable(&self) -> bool {
        self.deferrable
    }
}

/// A peripheral with its IRQ wired, masked, that counts the times the chip
/// asks its level.  Each quantum's advance leaves the level stale, so a
/// run's count less that of the same instructions with the QSPI idle
/// is the quanta the queue adds.
#[derive(Debug, Default)]
struct Probe {
    /// The times asked.
    asked: Cell<usize>,
}

impl Peripheral for Probe {
    fn interrupt_request(&self) -> bool {
        self.asked.set(self.asked.get() + 1);
        false
    }
}

/// A `Logged` model's transfers and levels.
type Calls = (Vec<(u8, u8, Cycles)>, Vec<(u8, Cycles)>);

/// What a run of a queue showed.
#[derive(Debug, PartialEq)]
struct QueueRun {
    /// The CPU's registers at the end.
    registers: Registers,
    /// The QSPI records.
    trace: Vec<Record>,
    /// The calls to the model on PCS 1010.
    first: Calls,
    /// The calls to the one on PCS 0101, if a `Logged`.
    second: Option<Calls>,
}

/// Runs `program` to `until` with a `Logged` model on PCS 1010 that
/// says `deferrable`, and on PCS 0101 another that says `second`, or,
/// with `None`, a peripheral that does not say (`Framed`); gives what it
/// showed and the probe's count.
fn run_queue(
    program: &[u16],
    until: Time,
    deferrable: bool,
    second: Option<bool>,
) -> (QueueRun, usize) {
    let directory = directory("deferrable", program);
    let second_type = if second.is_some() { "logged" } else { "framed" };
    let text = format!(
        r#"
version = 1

[chip]
type = "mc68331"
[[memory]]
name = "rom"
image = "rom.bin"
attach = [{{ select = "CSBOOT" }}]
[[peripheral]]
name = "first"
type = "first"
attach = [{{ qspi = 0b1010 }}]
[[peripheral]]
name = "second"
type = "{second_type}"
attach = [{{ qspi = 0b0101 }}]
[[peripheral]]
name = "probe"
type = "probe"
irq = 1
"#
    );
    let description = Description::parse(&text, &directory, &Options::default()).unwrap();
    let mut registry = Registry::with_builtins();
    let logged = |deferrable| -> Box<dyn Peripheral> {
        Box::new(SpiAdapter::new(Logged {
            deferrable,
            ..Logged::default()
        }))
    };
    registry
        .register("first", Vec::new(), move |_| Ok(logged(deferrable)))
        .unwrap();
    let second_deferrable = second.unwrap_or_default();
    registry
        .register("logged", Vec::new(), move |_| Ok(logged(second_deferrable)))
        .unwrap();
    registry
        .register("framed", Vec::new(), |_| Ok(Box::new(Framed::default())))
        .unwrap();
    registry
        .register("probe", Vec::new(), |_| Ok(Box::new(Probe::default())))
        .unwrap();
    let mut board =
        Board::<Mc68331>::load(&description, &registry, Environment::default()).unwrap();
    fs::remove_dir_all(directory).unwrap();
    let mut tracing = BoardTracing::default();
    tracing.qspi = true;
    board.set_tracing(tracing);
    board.run_until(until).unwrap();
    let calls = |peripheral: Ref<'_, SpiAdapter<Logged>>| {
        (
            peripheral.peripheral().transfers.clone(),
            peripheral.peripheral().levels.clone(),
        )
    };
    let shown = QueueRun {
        registers: board.chip().cpu().registers.clone(),
        trace: board.take_trace().records,
        first: board.peripheral("first").map(calls).unwrap(),
        second: board.peripheral("second").map(calls),
    };
    let quanta = board
        .peripheral::<Probe>("probe")
        .map(|probe| probe.asked.get())
        .unwrap();
    (shown, quanta)
}

#[test]
fn a_deferrable_qspi_peripheral_cuts_quanta_and_shows_the_same() {
    // the queue for ever, the CPU on a NOP sled off the board's bus;
    // against the same program with SPE clear, the quanta the queue
    // adds
    let program = queue_program(SPCR2_ALL_ENTRIES | SPCR2_WREN);
    let mut idle = program.clone();
    let spcr1 = idle.len() - 3;
    idle[spcr1] = 0;
    let until = Time(2_000_000_000);
    let (_, idle_quanta) = run_queue(&idle, until, true, Some(true));
    let (watched, watched_quanta) = run_queue(&program, until, false, Some(false));
    let (deferred, deferred_quanta) = run_queue(&program, until, true, Some(true));
    assert_eq!(deferred, watched, "the same calls at the same times");
    let transfers = watched.first.0.len() + watched.second.as_ref().unwrap().0.len();
    assert!(transfers > 100, "{transfers} transfers");
    assert!(
        watched_quanta >= idle_quanta + transfers,
        "a sync a call: {watched_quanta} against {idle_quanta} idle"
    );
    assert!(
        deferred_quanta <= idle_quanta + 1,
        "{deferred_quanta} quanta against {idle_quanta} idle"
    );
    // one pass, the CPU counting its polls of SPIF in D1: SPIF shows at
    // the same instruction
    let mut program = queue_program(SPCR2_ALL_ENTRIES);
    program.extend([
        ADDQ_ONE_TO_D1,
        TST_BYTE_ABSOLUTE,
        (SPSR >> 16) as u16,
        SPSR as u16,
        BPL_BACK_FOUR,
        BRA_SELF,
    ]);
    let until = Time(500_000_000);
    let (watched, _) = run_queue(&program, until, false, Some(false));
    let (deferred, _) = run_queue(&program, until, true, Some(true));
    assert_eq!(deferred, watched);
    assert_eq!(watched.first.0.len(), 8, "the pass's entries on PCS 1010");
    assert!(watched.registers.data[1] > 1, "polled until SPIF");
}

#[test]
fn a_qspi_peripheral_that_does_not_opt_in_keeps_the_queue_synced() {
    let program = queue_program(SPCR2_ALL_ENTRIES | SPCR2_WREN);
    let until = Time(2_000_000_000);
    let (watched, watched_quanta) = run_queue(&program, until, false, Some(false));
    let (mixed, mixed_quanta) = run_queue(&program, until, true, None);
    let (refused, refused_quanta) = run_queue(&program, until, true, Some(false));
    assert_eq!(mixed.first, watched.first);
    assert_eq!(refused, watched);
    assert_eq!(
        mixed_quanta, watched_quanta,
        "a peripheral that does not say"
    );
    assert_eq!(refused_quanta, watched_quanta, "a peripheral that says no");
}

/// The rising edges an edge counter on PCS0 (PQS3) saw in a run of the
/// queue for ever, its one peripheral a `Logged` that says `deferrable`.
fn pcs0_edges(deferrable: bool) -> u64 {
    let directory = directory("pcs-sensed", &queue_program(SPCR2_ALL_ENTRIES | SPCR2_WREN));
    let text = r#"
version = 1

[chip]
type = "mc68331"
[[memory]]
name = "rom"
image = "rom.bin"
attach = [{ select = "CSBOOT" }]
[[peripheral]]
name = "first"
type = "first"
attach = [{ qspi = 0b1010 }]
[[peripheral]]
name = "counter"
type = "edge-counter"
[[net]]
name = "pcs0"
type = "edge"
from = "chip.PQS3"
to = ["counter.in"]
"#;
    let description = Description::parse(text, &directory, &Options::default()).unwrap();
    fs::remove_dir_all(directory).unwrap();
    let mut registry = Registry::with_builtins();
    registry
        .register("first", Vec::new(), move |_| {
            Ok(Box::new(SpiAdapter::new(Logged {
                deferrable,
                ..Logged::default()
            })))
        })
        .unwrap();
    let mut board =
        Board::<Mc68331>::load(&description, &registry, Environment::default()).unwrap();
    board.run_until(Time(2_000_000_000)).unwrap();
    board
        .peripheral::<EdgeCounter>("counter")
        .map(|counter| counter.count())
        .unwrap()
}

#[test]
fn a_net_sensing_pcs_keeps_the_qspi_from_deferring() {
    let watched = pcs0_edges(false);
    assert!(watched > 100, "{watched} edges");
    assert_eq!(pcs0_edges(true), watched, "each PCS0 edge at its time");
}

#[test]
fn the_host_s_stop_request_ends_the_run() {
    let rom = r#"
version = 1

[chip]
type = "mc68331"
[[memory]]
name = "rom"
image = "rom.bin"
attach = [{ select = "CSBOOT" }]
"#;
    let mut board = board("stop-request", rom, &[NOP]);
    let stop = board.stop_request();
    stop.request();
    let until = Time(1_000_000_000);
    assert_eq!(board.run_until(until).unwrap().stop, Stop::Requested);
    assert!(board.now() < until);
    drop(board);
    // Once the board is gone, a request does nothing.
    stop.request();
}

/// A latch on a select: Q0 follows bit 0 of each byte written.
#[derive(Debug, Default)]
struct BitLatch;

impl Peripheral for BitLatch {
    fn write(
        &mut self,
        _: PeripheralAccess,
        value: u32,
        drive: &mut Drive<'_>,
    ) -> Result<(), mc683xx_cpu32::bus::BusError> {
        drive.drive(0, Signal::Level(value & 1 != 0));
        Ok(())
    }
}

/// PORTQS (MC68331UM D.4.8).
const PORTQS: u32 = 0xff_fc15;

/// Firmware that, after `setup`, writes D2 to `output`, whose bit 0
/// reaches PQS0, polls PORTQS until PQS0 shows it, counting the polls
/// in D1, then complements D2 and goes again, for ever.
fn toggling(setup: &[u16], output: u32) -> Vec<u16> {
    setup
        .iter()
        .copied()
        .chain([
            // MOVE.B D2,(output).L
            0x13c2,
            (output >> 16) as u16,
            output as u16,
            // ADDQ.L #1,D1
            0x5281,
            // MOVE.B (PORTQS).L,D0
            0x1039,
            (PORTQS >> 16) as u16,
            PORTQS as u16,
            // EOR.B D2,D0
            0xb500,
            // BTST #0,D0
            0x0800,
            0x0000,
            // BNE.S back to the ADDQ
            0x66f0,
            // NOT.B D2
            0x4602,
            // BRA.S back to the MOVE.B
            0x60e6,
        ])
        .collect()
}

/// What a run gave: its net changes, its instructions and its data
/// registers.
type Ran = (Vec<(Time, Signal)>, u64, [u32; 8]);

/// `board` run for `total` picoseconds in runs of `quantum`, its nets and
/// pins traced: what it gave.
fn quantized(mut board: Board<Mc68331>, total: u64, quantum: u64) -> Ran {
    trace_nets_and_pins(&mut board);
    while board.now() < Time(total) {
        let outcome = board
            .run_until((board.now() + quantum).min(Time(total)))
            .unwrap();
        assert_eq!(outcome.stop, Stop::Budget);
    }
    ran(&mut board)
}

/// Traces `board`'s nets and pins, for `ran`.
fn trace_nets_and_pins(board: &mut Board<Mc68331>) {
    let mut tracing = BoardTracing::default();
    tracing.nets = true;
    tracing.pins = true;
    board.set_tracing(tracing);
}

/// What `board`, its nets and pins traced, gave.
fn ran(board: &mut Board<Mc68331>) -> Ran {
    let trace = board.take_trace();
    let changes = trace
        .nets
        .into_iter()
        .map(|change| (change.at, change.signal))
        .chain(trace.records.iter().filter_map(|record| match record {
            Record::Pin(pin) => Some((pin.time, Signal::Level(pin.level))),
            _ => None,
        }))
        .collect();
    let registers = &board.chip().cpu().registers;
    (changes, board.chip().instructions(), registers.data)
}

/// The toggling board, a `BitLatch` on CS1 driving PQS0, run for
/// `total` picoseconds in runs of `quantum`.
fn toggled(total: u64, quantum: u64) -> Ran {
    quantized(toggling_board(), total, quantum)
}

/// The toggling board, a `BitLatch` on CS1 driving PQS0.
fn toggling_board() -> Board<Mc68331> {
    let setup = [store(CSBAR1, CS1_BAR), store(CSOR1, CS_FAST)].concat();
    let directory = directory("toggling", &toggling(&setup, CS1_BASE));
    let text = r#"
version = 1

[chip]
type = "mc68331"
[[memory]]
name = "rom"
image = "rom.bin"
attach = [{ select = "CSBOOT" }]
[[peripheral]]
name = "latch"
type = "bit-latch"
attach = [{ select = "CS1", role = "write" }]
[[net]]
name = "q0"
type = "logic"
from = "latch.Q0"
to = ["chip.PQS0"]
"#;
    let description = Description::parse(text, &directory, &Options::default()).unwrap();
    fs::remove_dir_all(directory).unwrap();
    let mut registry = Registry::with_builtins();
    registry
        .register(
            "bit-latch",
            vec![PortSpec::output("Q0", Kind::Logic)],
            |_| Ok(Box::new(BitLatch)),
        )
        .unwrap();
    Board::<Mc68331>::load(&description, &registry, Environment::default()).unwrap()
}

/// A millisecond in picoseconds.
const MILLISECOND: u64 = 1_000_000_000;
/// A millisecond, as a quantum.
const MILLISECOND_QUANTUM: NonZeroU64 = NonZeroU64::new(MILLISECOND).expect("not zero");

#[test]
fn a_run_in_quanta_is_one_run() {
    let total = 30 * MILLISECOND;
    let whole = toggled(total, total);
    let mut board = toggling_board();
    trace_nets_and_pins(&mut board);
    let mut quanta = 0;
    let outcome = board
        .run_until_quantized(Time(total), MILLISECOND_QUANTUM, |_| {
            quanta += 1;
            ControlFlow::Continue(())
        })
        .unwrap();
    assert_eq!(outcome.stop, Stop::Budget);
    assert_eq!(outcome.now, Time(total));
    assert_eq!(quanta, 30);
    assert!(ran(&mut board) == whole);
    let mut asked = 0;
    let outcome = board
        .run_until_quantized(Time(2 * total), MILLISECOND_QUANTUM, |_| {
            asked += 1;
            if asked == 3 {
                ControlFlow::Break(())
            } else {
                ControlFlow::Continue(())
            }
        })
        .unwrap();
    assert_eq!(outcome.stop, Stop::Requested, "broken off before the third");
    assert_eq!(outcome.now, Time(total + 2 * MILLISECOND));
}

#[test]
fn a_peripheral_driven_within_a_run_gives_the_same_run_however_it_is_cut() {
    let total = 300 * MILLISECOND;
    let whole = toggled(total, total);
    let (changes, _, data) = &whole;
    // each write reaches PQS0 by the instruction after it: one poll a
    // toggle, the run going on as it would on the board; a net change
    // and a pin record each
    assert!(changes.len() > 10_000, "{} toggles", changes.len());
    assert_eq!(2 * data[1] as usize, changes.len());
    for quantum in [MILLISECOND, 10 * MILLISECOND, 100 * MILLISECOND] {
        assert!(toggled(total, quantum) == whole, "in runs of {quantum} ps");
    }
}

/// A buffer: Y follows A at once.
#[derive(Debug, Default)]
struct Echo;

impl Peripheral for Echo {
    fn input(&mut self, _: usize, signal: Signal, now: Time, drive: &mut Drive<'_>) {
        drive.drive_at(now, 0, signal);
    }
}

/// The words holding SYPCR, DDRF and PFPAR in their low bytes, and
/// PORTF0 (MC68331UM Table D-3, D.3.9 to D.3.12).
const SYPCR_WORD: u32 = SIM + 0x20;
/// DDRF's word.
const DDRF_WORD: u32 = SIM + 0x1c;
/// PFPAR's word.
const PFPAR_WORD: u32 = SIM + 0x1e;
/// PORTF0.
const PORTF0: u32 = SIM + 0x19;

/// The echoing board, PF0 through an `Echo` to PQS0, run for `total`
/// picoseconds in runs of `quantum`.
fn echoed(total: u64, quantum: u64) -> Ran {
    // the watchdog off; port F I/O, PF0 an output
    let setup = [
        store(SYPCR_WORD, 0),
        store(PFPAR_WORD, 0),
        store(DDRF_WORD, 1),
    ]
    .concat();
    let directory = directory("echoing", &toggling(&setup, PORTF0));
    let text = r#"
version = 1

[chip]
type = "mc68331"
[[memory]]
name = "rom"
image = "rom.bin"
attach = [{ select = "CSBOOT" }]
[[peripheral]]
name = "echo"
type = "echo"
[[net]]
name = "pf0"
type = "logic"
from = "chip.PF0"
to = ["echo.A"]
[[net]]
name = "y"
type = "logic"
from = "echo.Y"
to = ["chip.PQS0"]
"#;
    let description = Description::parse(text, &directory, &Options::default()).unwrap();
    fs::remove_dir_all(directory).unwrap();
    let mut registry = Registry::with_builtins();
    registry
        .register(
            "echo",
            vec![
                PortSpec::output("Y", Kind::Logic),
                PortSpec::input("A", Kind::Logic),
            ],
            |_| Ok(Box::new(Echo)),
        )
        .unwrap();
    let board = Board::<Mc68331>::load(&description, &registry, Environment::default()).unwrap();
    quantized(board, total, quantum)
}

#[test]
fn a_chip_output_through_a_peripheral_gives_the_same_run_however_it_is_cut() {
    let total = 300 * MILLISECOND;
    let whole = echoed(total, total);
    let (changes, _, data) = &whole;
    // each toggle reaches PQS0 by the instruction after the write: one
    // poll a toggle; two nets and two pins each
    assert!(changes.len() > 10_000, "{} changes", changes.len());
    // the power-on drives, the DDRF write's toggle and the last
    // toggle, cut by the run's end, aside
    assert!(
        changes.len().abs_diff(4 * data[1] as usize) <= 4,
        "{} changes, {} polls",
        changes.len(),
        data[1]
    );
    for quantum in [MILLISECOND, 10 * MILLISECOND, 100 * MILLISECOND] {
        assert!(echoed(total, quantum) == whole, "in runs of {quantum} ps");
    }
}

/// A board whose firmware, after a wait, drives PF0 low, on a net to a
/// run-time endpoint alone.
fn pf0_board() -> Board<Mc68331> {
    let program: Vec<u16> = [
        store(SYPCR_WORD, 0),
        store(PFPAR_WORD, 0),
        store(PORTF0 - 1, 0),
    ]
    .concat()
    .into_iter()
    // MOVE.L #$1000,D0; SUBQ.L #1,D0; BNE.S back to the SUBQ
    .chain([0x203c, 0x0000, 0x1000, 0x5380, 0x66fc])
    .chain(store(DDRF_WORD, 1))
    .chain([BRA_SELF])
    .collect();
    board(
        "pf0-observed",
        r#"
version = 1

[chip]
type = "mc68331"
[[memory]]
name = "rom"
image = "rom.bin"
attach = [{ select = "CSBOOT" }]
[[net]]
name = "pf0"
type = "logic"
from = "chip.PF0"
to = ["external.pf0"]
"#,
        &program,
    )
}

#[test]
fn an_observer_that_stops_at_a_chip_output_stops_the_run_there() {
    let mut board = pf0_board();
    let stop = board.stop_request();
    let low = Rc::new(Cell::new(None));
    let seen = Rc::clone(&low);
    let id = board
        .observe("pf0", move |_: &str, at: Time, signal: Signal| {
            if signal == Signal::Level(false) {
                seen.set(Some(at));
                stop.request();
            }
        })
        .unwrap();
    let until = Time(100 * MILLISECOND);
    let outcome = board.run_until(until).unwrap();
    let low = low.get().expect("PF0 went low");
    assert_eq!(outcome.stop, Stop::Requested);
    assert!(
        outcome.now >= low && outcome.now < low + 10_000_000,
        "stopped at {:?}, the change at {low:?}",
        outcome.now
    );
    assert!(board.unobserve(id).is_some());
}

/// RESET (CPU32RM 4, RESET).
const RESET: u16 = 0x4e70;

/// Firmware that counts its starts in D1, waits, executes RESET, and
/// spins.
fn resetting() -> Vec<u16> {
    // ADDQ.L #1,D1; MOVE.L #$100,D0; SUBQ.L #1,D0; BNE.S back to the
    // SUBQ; RESET; BRA.S to itself
    vec![
        0x5281, 0x203c, 0x0000, 0x0100, 0x5380, 0x66fc, RESET, BRA_SELF,
    ]
}

#[test]
fn the_chips_own_reset_on_a_wired_reset_net_does_not_reset_it() {
    let mut board = board(
        "reset-wired",
        r#"
version = 1

[chip]
type = "mc68331"
[[memory]]
name = "rom"
image = "rom.bin"
attach = [{ select = "CSBOOT" }]
[[net]]
name = "reset"
type = "wired-and"
from = ["chip.RESET"]
to = ["chip.RESET"]
"#,
        &resetting(),
    );
    board.take_events();
    board.run_until(Time(2_000_000_000)).unwrap();
    assert_eq!(board.chip().cpu().registers.data[1], 1, "one start");
    let resets: Vec<ResetCause> = board
        .take_events()
        .into_iter()
        .filter_map(|event| match event {
            Event::Reset(_, cause) => Some(cause),
            _ => None,
        })
        .collect();
    assert_eq!(resets, [ResetCause::System], "the instruction's alone");
}

/// When `Supervisor` asserts RESET.
const RESET_ASSERTED: Time = Time(1_000_000_000);
/// When it releases it.
const RESET_RELEASED: Time = Time(3_000_000_000);

/// A reset supervisor that holds RESET low from `RESET_ASSERTED` to
/// `RESET_RELEASED`.
#[derive(Debug, Default)]
struct Supervisor;

impl Peripheral for Supervisor {
    fn power_on(&mut self, drive: &mut Drive<'_>) {
        drive.drive_at(RESET_ASSERTED, 0, Signal::Level(false));
        drive.drive_at(RESET_RELEASED, 0, Signal::Level(true));
    }
}

#[test]
fn a_peripheral_holding_reset_low_holds_the_chip_in_reset() {
    let directory = directory("reset-held", &resetting());
    let text = r#"
version = 1

[chip]
type = "mc68331"
[[memory]]
name = "rom"
image = "rom.bin"
attach = [{ select = "CSBOOT" }]
[[peripheral]]
name = "supervisor"
type = "supervisor"
[[net]]
name = "reset"
type = "logic"
from = "supervisor.RESET"
to = ["chip.RESET"]
"#;
    let description = Description::parse(text, &directory, &Options::default()).unwrap();
    fs::remove_dir_all(directory).unwrap();
    let mut registry = Registry::with_builtins();
    registry
        .register(
            "supervisor",
            vec![PortSpec::output("RESET", Kind::Logic)],
            |_| Ok(Box::new(Supervisor)),
        )
        .unwrap();
    let mut board =
        Board::<Mc68331>::load(&description, &registry, Environment::default()).unwrap();
    board.take_events();
    // the external resets since the last call, at their board times
    let resets = |board: &mut Board<Mc68331>| -> Vec<Time> {
        let base = board.system_clock();
        board
            .take_events()
            .into_iter()
            .filter_map(|event| match event {
                Event::Reset(at, ResetCause::External) => Some(base.time(at)),
                _ => None,
            })
            .collect()
    };
    board.run_until(RESET_ASSERTED + 500_000_000).unwrap();
    let asserted = resets(&mut board);
    assert!(
        matches!(asserted[..], [at] if at >= RESET_ASSERTED && at < RESET_ASSERTED + 10_000_000),
        "{asserted:?}"
    );
    let held = board.chip().instructions();
    assert_eq!(
        board.run_until(RESET_RELEASED - 1).unwrap().now,
        RESET_RELEASED - 1
    );
    assert_eq!(
        board.chip().instructions(),
        held,
        "no instruction while held"
    );
    board.run_until(RESET_RELEASED + 1_000_000_000).unwrap();
    let released = resets(&mut board);
    assert!(
        matches!(released[..], [at] if at >= RESET_RELEASED && at < RESET_RELEASED + 10_000_000),
        "{released:?}"
    );
    assert!(board.chip().instructions() > held);
    assert_eq!(board.chip().cpu().registers.data[1], 2, "started again");
}

/// PORTGP (MC68331UM D.2.4): the port GP pins as read.
const PORTGP: u32 = GPT + 0x07;

/// A peripheral that drives, from power-on, each of its levels at its
/// time on its port: port 0 `RESET`, port 1 `PGP0` (high, undriven).
#[derive(Debug)]
struct Scripted(&'static [(Time, usize, bool)]);

impl Peripheral for Scripted {
    fn power_on(&mut self, drive: &mut Drive<'_>) {
        for &(at, port, level) in self.0 {
            drive.drive_at(at, port, Signal::Level(level));
        }
    }
}

/// A board whose firmware, after `setup`, reads PORTGP into D0 for
/// ever, a `Scripted` peripheral driving `script` onto RESET and PGP0.
fn scripted(setup: &[u16], script: &'static [(Time, usize, bool)]) -> Board<Mc68331> {
    let program: Vec<u16> = setup
        .iter()
        .copied()
        // MOVE.B (PORTGP).L,D0; BRA.S back to it
        .chain([
            MOVE_BYTE_TO_D0,
            (PORTGP >> 16) as u16,
            PORTGP as u16,
            0x60f8,
        ])
        .collect();
    let directory = directory("scripted", &program);
    let text = r#"
version = 1

[chip]
type = "mc68331"
[[memory]]
name = "rom"
image = "rom.bin"
attach = [{ select = "CSBOOT" }]
[[peripheral]]
name = "script"
type = "scripted"
[[net]]
name = "reset"
type = "logic"
from = "script.RESET"
to = ["chip.RESET"]
[[net]]
name = "pgp0"
type = "logic"
from = "script.PGP0"
to = ["chip.PGP0"]
"#;
    let description = Description::parse(text, &directory, &Options::default()).unwrap();
    fs::remove_dir_all(directory).unwrap();
    let mut registry = Registry::with_builtins();
    registry
        .register(
            "scripted",
            vec![
                PortSpec::output("RESET", Kind::Logic),
                PortSpec::output("PGP0", Kind::Logic),
            ],
            move |_| Ok(Box::new(Scripted(script))),
        )
        .unwrap();
    Board::<Mc68331>::load(&description, &registry, Environment::default()).unwrap()
}

/// PGP0 as the firmware last read it.
fn pgp0(board: &Board<Mc68331>) -> bool {
    board.chip().cpu().registers.data[0] & 1 != 0
}

#[test]
fn a_gpt_input_edge_during_a_held_reset_shows_as_reset_ends() {
    const SCRIPT: &[(Time, usize, bool)] = &[
        (Time::ZERO, 1, false),
        (RESET_ASSERTED, 0, false),
        (Time(2 * MILLISECOND), 1, true),
        (RESET_RELEASED, 0, true),
    ];
    let mut board = scripted(&[], SCRIPT);
    board.run_until(RESET_RELEASED - 1).unwrap();
    assert!(!pgp0(&board), "read before the reset");
    board.run_until(RESET_RELEASED + 50_000_000).unwrap();
    assert!(pgp0(&board), "high since before the reset ended");
}

#[test]
fn a_gpt_input_edge_driven_ahead_keeps_its_time_across_a_syncr_write() {
    const EDGE: Time = Time(MILLISECOND);
    const SCRIPT: &[(Time, usize, bool)] = &[(Time::ZERO, 1, false), (EDGE, 1, true)];
    let setup = store(SYNCR, SYNCR_FAST);
    let mut board = scripted(&setup, SCRIPT);
    board.run_until(EDGE - 20_000_000).unwrap();
    assert_eq!(board.system_clock().frequency().hz(), 16_777_216);
    assert!(!pgp0(&board), "low until its time");
    board.run_until(EDGE + 20_000_000).unwrap();
    assert!(pgp0(&board), "high from its time");
}

/// PQSPAR and DDRQS, as a word (MC68331UM D.4.9).
const PQSPAR_WORD: u32 = 0xff_fc16;

/// A board whose firmware, the watchdog off, toggles PQS1, a general
/// purpose output, with a wait between, on a net to a run-time endpoint
/// alone.
fn pqs1_toggling_board() -> Board<Mc68331> {
    let program: Vec<u16> = [store(SYPCR_WORD, 0), store(PQSPAR_WORD, 1 << 1)]
        .concat()
        .into_iter()
        // MOVE.B D2,(PORTQS).L; NOT.B D2; MOVE.L #$40,D0; SUBQ.L #1,D0;
        // BNE.S back to the SUBQ; BRA.S back to the MOVE.B
        .chain([
            0x13c2,
            (PORTQS >> 16) as u16,
            PORTQS as u16,
            0x4602,
            0x203c,
            0x0000,
            0x0040,
            0x5380,
            0x66fc,
            0x60ec,
        ])
        .collect();
    board(
        "pqs1-toggling",
        r#"
version = 1

[chip]
type = "mc68331"
[[memory]]
name = "rom"
image = "rom.bin"
attach = [{ select = "CSBOOT" }]
[[net]]
name = "pqs1"
type = "logic"
from = "chip.PQS1"
to = ["external.pqs1"]
"#,
        &program,
    )
}

#[test]
fn port_qs_edges_keep_their_times_however_the_run_is_cut() {
    let total = 3 * MILLISECOND;
    let whole = quantized(pqs1_toggling_board(), total, total);
    let (changes, _, _) = &whole;
    // the net's power-on level, then the DDRQS write's edge and nine
    // toggles on the net and on the pin each
    assert_eq!(changes.len(), 21, "{changes:?}");
    assert!(
        quantized(pqs1_toggling_board(), total, MILLISECOND / 7) == whole,
        "in runs of 1/7 ms"
    );
}

/// SYPCR SWE set, SWP and SWT clear: the watchdog on, its shortest
/// timeout (MC68331UM D.3.12, Table 4-4).
const SYPCR_WATCHDOG_SHORTEST: u16 = 0x0080;
/// System clocks the chip drives RESET low for at an internal reset
/// (MC68331UM 4.6.6).
const RESET_PULSE_CLOCKS: u64 = 512;
/// The system clock from SYNCR's reset value, $3F00, and the 32.768 kHz
/// reference (MC68331UM D.3.3).
const SYNCR_RESET_HZ: u64 = 8_388_608;

#[test]
fn a_reset_that_slows_the_clock_drives_reset_for_512_clocks_of_the_new_one() {
    let program: Vec<u16> = [
        store(SYNCR, SYNCR_FAST),
        store(SYPCR_WORD, SYPCR_WATCHDOG_SHORTEST),
    ]
    .concat()
    .into_iter()
    .chain([BRA_SELF])
    .collect();
    let mut board = board(
        "reset-slows",
        r#"
version = 1

[chip]
type = "mc68331"
[[memory]]
name = "rom"
image = "rom.bin"
attach = [{ select = "CSBOOT" }]
[[net]]
name = "reset"
type = "wired-and"
from = ["chip.RESET"]
to = ["external.reset"]
"#,
        &program,
    );
    trace_nets_and_pins(&mut board);
    board.take_events();
    board.run_until(Time(20 * MILLISECOND)).unwrap();
    let watchdog = board
        .take_events()
        .into_iter()
        .find_map(|event| match event {
            Event::Reset(at, ResetCause::Watchdog) => Some(at),
            _ => None,
        });
    assert!(watchdog.is_some(), "the watchdog reset the chip");
    let edges: Vec<(Time, Signal)> = board
        .take_trace()
        .nets
        .into_iter()
        .map(|change| (change.at, change.signal))
        .collect();
    let [.., (low, Signal::Level(false)), (high, Signal::Level(true))] = edges[..] else {
        panic!("no RESET pulse: {edges:?}");
    };
    assert_eq!(
        u128::from(high.since(low)),
        u128::from(RESET_PULSE_CLOCKS) * u128::from(PICOSECONDS_PER_SECOND)
            / u128::from(SYNCR_RESET_HZ),
        "{edges:?}"
    );
}
