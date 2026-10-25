//! The host's cycles and peeks, and runs to a target time.

use mc683xx_cpu32::address::Address;
use mc683xx_cpu32::bus::BusError;
use mc683xx_cpu32::cpu::Stop;
use mc683xx_cpu32::cpu::state::StateError;
use mc683xx_cpu32::cycles::Cycles;
use mc683xx_cpu32::function_code::FunctionCode;
use mc683xx_cpu32::interrupt::InterruptLevel;
use mc683xx_cpu32::peripheral::BusCycle;
use mc683xx_cpu32::peripheral::MappedPeripheral;
use mc683xx_cpu32::peripheral::Select;
use mc683xx_cpu32::registers::Registers;
use mc683xx_cpu32::size::Size;
use mc683xx_modules::chip_select::ChipSelect;
use mc683xx_modules::chip_select::Number;
use mc683xx_modules::module;
use mc683xx_modules::module::Block;
use mc683xx_modules::module::Module;
use mc683xx_modules::module::Peek;
use mc683xx_modules::peripheral::Timed;
use mc683xx_modules::qsm::sci::Frame;

use super::AccessError;
use crate::chip::Builder;
use crate::chip::Mc68331;
use crate::chip::MemoryId;
use mc683xx_cpu32::bus::StopRequest;
use mc683xx_cpu32::peripheral::PeripheralId;

/// The SIM's, QSM's and GPT's registers with MM set (MC68331UM Table
/// D-1).
const SIM: u32 = 0xff_fa00;
const QSM: u32 = 0xff_fc00;
const GPT: u32 = 0xff_f900;
/// SYPCR, PICR, PITR (MC68331UM D.3.10 to D.3.12).
const SYPCR: u32 = SIM + 0x20;
const PICR: u32 = SIM + 0x22;
const PITR: u32 = SIM + 0x24;
/// CSBARBT, and CSBAR0 to CSBAR2 with their CSORs after each
/// (MC68331UM Table D-3).
const CSBARBT: u32 = SIM + 0x48;
const CSBAR0: u32 = SIM + 0x4c;
/// The bytes from one select's CSBAR to the next's.
const SELECT_BYTES: u32 = 4;
/// CSOR: both bytes, read and write, supervisor and user space, fast
/// termination; or the peripheral's DSACK (MC68331UM D.3.26).
const CSOR_FAST: u16 = 0x7bb0;
const CSOR_EXTERNAL: u16 = 0x7bf0;
/// CSBARx's 64-Kbyte block size (MC68331UM Table 4-21).
const BLOCK_64K: u16 = 0b011;
/// SCCR0, SCCR1, SCSR, SCDR (QSMRM 5.2); RE; RDRF.
const SCCR0: u32 = QSM + 0x08;
const SCCR1: u32 = QSM + 0x0a;
const SCSR: u32 = QSM + 0x0c;
const SCDR: u32 = QSM + 0x0e;
const SCCR1_RE: u32 = 1 << 2;
const SCSR_RDRF: u32 = 1 << 6;
/// TCNT (GPTRM 3.3).
const TCNT: u32 = GPT + 0x0a;

/// Where RAM, the peripheral and the failing peripheral are.
const RAM_BASE: u32 = 0x10_0000;
const RAM_BYTES: usize = 64 << 10;
const PERIPHERAL_BASE: u32 = 0x20_0000;
const FAILING_BASE: u32 = 0x30_0000;
/// Where programs start, and the PIT's handler.
const START: u32 = 0x400;
const HANDLER: u32 = 0x800;
/// The PIT's vector.
const PIT_VECTOR: u8 = 0x50;

/// NOP, BRA to itself, RTE, MOVE #imm,SR, MOVE.W (xxx).L,D1, ADDQ.W
/// #1,(xxx).L, STOP #imm (CPU32RM 4).
const NOP: u16 = 0x4e71;
const BRANCH_TO_SELF: u16 = 0x60fe;
const RTE: u16 = 0x4e73;
const MOVE_TO_SR: u16 = 0x46fc;
const MOVE_WORD_ABSOLUTE_TO_D1: u16 = 0x3239;
const ADDQ_WORD_1_ABSOLUTE: u16 = 0x5279;
const STOP: u16 = 0x4e72;
/// SR: supervisor, every interrupt level taken.
const UNMASKED: u16 = 0x2000;

const SUPERVISOR: FunctionCode = FunctionCode::SUPERVISOR_DATA;

/// A peripheral that records its cycles and answers `value`.
#[derive(Debug, Default)]
struct Recorder {
    /// Its read cycles.
    reads: Vec<BusCycle>,
    /// Its write cycles and values.
    writes: Vec<(BusCycle, u32)>,
    /// What it reads as.
    value: u32,
}

impl MappedPeripheral for Recorder {
    fn read(&mut self, cycle: BusCycle) -> Result<u32, BusError> {
        self.reads.push(cycle);
        Ok(self.value & cycle.size.mask())
    }

    fn write(&mut self, cycle: BusCycle, value: u32) -> Result<(), BusError> {
        self.writes.push((cycle, value));
        Ok(())
    }
}

mc683xx_cpu32::unsavable!(Recorder, "a recorder");

impl Timed for Recorder {}

/// A peripheral that answers every cycle with BERR.
#[derive(Debug)]
struct Failing;

impl MappedPeripheral for Failing {
    fn read(&mut self, _: BusCycle) -> Result<u32, BusError> {
        Err(BusError)
    }

    fn write(&mut self, _: BusCycle, _: u32) -> Result<(), BusError> {
        Err(BusError)
    }
}

mc683xx_cpu32::unsavable!(Failing, "a failing peripheral");

impl Timed for Failing {}

/// A model with an event every `period`, which only cuts the run's
/// quanta.
#[derive(Debug)]
struct Ticker {
    /// Its period.
    period: Cycles,
    /// Its time.
    now: Cycles,
}

impl MappedPeripheral for Ticker {
    fn read(&mut self, _: BusCycle) -> Result<u32, BusError> {
        Err(BusError)
    }

    fn write(&mut self, _: BusCycle, _: u32) -> Result<(), BusError> {
        Err(BusError)
    }
}

mc683xx_cpu32::unsavable!(Ticker, "a ticker");

impl Timed for Ticker {
    fn next_event(&self) -> Option<Cycles> {
        Some(Cycles((self.now.0 / self.period.0 + 1) * self.period.0))
    }

    fn advance(&mut self, now: Cycles) {
        self.now = now;
    }
}

fn cs(number: u8) -> ChipSelect {
    ChipSelect::Numbered(Number::new(number).unwrap())
}

/// CS`number` as a cycle carries it.
fn on(number: u8) -> Option<Select> {
    Some(cs(number).into())
}

/// A boot ROM of NOPs: the reset vectors, `program` at `START`, and
/// the PIT's handler, which counts in RAM's first word.
fn rom(program: &[u16]) -> Vec<u8> {
    let mut image = NOP.to_be_bytes().repeat(RAM_BYTES / 2);
    let stack = RAM_BASE + RAM_BYTES as u32;
    let mut put = |address: u32, words: &[u16]| {
        for (index, word) in words.iter().enumerate() {
            let at = address as usize + 2 * index;
            image[at..at + 2].copy_from_slice(&word.to_be_bytes());
        }
    };
    put(0, &[(stack >> 16) as u16, stack as u16, 0, START as u16]);
    put(
        u32::from(PIT_VECTOR) * 4,
        &[(HANDLER >> 16) as u16, HANDLER as u16],
    );
    let address = [(RAM_BASE >> 16) as u16, RAM_BASE as u16];
    put(
        HANDLER,
        &[ADDQ_WORD_1_ABSOLUTE, address[0], address[1], RTE],
    );
    put(START, program);
    image
}

/// The board: `program` in the boot ROM, RAM on CS0, a recorder on
/// CS1 and a failing peripheral on CS2, each mapped by the host's own
/// writes, and no watchdog.
struct Board {
    chip: Mc68331,
    ram: MemoryId,
    recorder: PeripheralId,
}

fn board(program: &[u16], ticker: Option<Cycles>) -> Board {
    let mut builder = Builder::default();
    builder
        .memory(rom(program), false, &[ChipSelect::Boot])
        .unwrap();
    let ram = builder.memory(vec![0; RAM_BYTES], true, &[cs(0)]).unwrap();
    let recorder = builder
        .peripheral(
            Recorder {
                value: 0x1234,
                ..Recorder::default()
            },
            &[cs(1)],
        )
        .unwrap();
    builder.peripheral(Failing, &[cs(2)]).unwrap();
    if let Some(period) = ticker {
        builder.unwired_peripheral(Ticker {
            period,
            now: Cycles::ZERO,
        });
    }
    let mut chip = builder.build();
    let selects = [
        (RAM_BASE, CSOR_FAST),
        (PERIPHERAL_BASE, CSOR_FAST),
        (FAILING_BASE, CSOR_EXTERNAL),
    ];
    for (bar, (base, option)) in (CSBAR0..).step_by(SELECT_BYTES as usize).zip(selects) {
        write(
            &mut chip,
            bar,
            Size::Word,
            (base >> 8) | u32::from(BLOCK_64K),
        );
        write(&mut chip, bar + 2, Size::Word, option.into());
    }
    write(&mut chip, SYPCR, Size::Word, 0);
    Board {
        chip,
        ram,
        recorder,
    }
}

fn write(chip: &mut Mc68331, address: u32, size: Size, value: u32) {
    chip.bus_write(Address(address), size, SUPERVISOR, value)
        .unwrap();
}

fn read(chip: &mut Mc68331, address: u32, size: Size) -> Result<u32, AccessError> {
    chip.bus_read(Address(address), size, SUPERVISOR)
}

fn peek(chip: &Mc68331, address: u32, size: Size) -> Result<u32, AccessError> {
    chip.peek(Address(address), size, SUPERVISOR)
}

#[test]
fn the_host_maps_memory_and_reaches_it_through_the_decode() {
    let Board { mut chip, ram, .. } = board(&[BRANCH_TO_SELF], None);
    let now = chip.now();
    write(&mut chip, RAM_BASE, Size::Long, 0x1234_5678);
    write(&mut chip, RAM_BASE + 5, Size::Byte, 0x9a);
    assert_eq!(chip.memory(ram)[..6], [0x12, 0x34, 0x56, 0x78, 0, 0x9a]);
    assert_eq!(read(&mut chip, RAM_BASE, Size::Long), Ok(0x1234_5678));
    assert_eq!(peek(&chip, RAM_BASE + 2, Size::Word), Ok(0x5678));
    assert_eq!(peek(&chip, RAM_BASE + 5, Size::Byte), Ok(0x9a));
    // the 64-Kbyte block on the 24 address lines, from above them
    assert_eq!(peek(&chip, 0xff00_0000 | RAM_BASE, Size::Word), Ok(0x1234));
    // the ROM, from CSBOOT
    assert_eq!(peek(&chip, START, Size::Word), Ok(BRANCH_TO_SELF.into()));
    assert_eq!(chip.now(), now, "the host's cycles take no chip time");
}

#[test]
fn the_cpu_runs_on_the_hosts_map() {
    // MOVE.W (RAM_BASE).L,D1 on RAM the host mapped and wrote
    let program = [
        MOVE_WORD_ABSOLUTE_TO_D1,
        (RAM_BASE >> 16) as u16,
        RAM_BASE as u16,
        BRANCH_TO_SELF,
    ];
    let Board { mut chip, .. } = board(&program, None);
    write(&mut chip, RAM_BASE, Size::Word, 0xbeef);
    chip.run_for(Cycles(200));
    assert_eq!(chip.cpu().registers.data[1] & 0xffff, 0xbeef);
}

#[test]
fn a_peripheral_sees_the_hosts_cycles_and_never_its_peeks() {
    let Board {
        mut chip, recorder, ..
    } = board(&[BRANCH_TO_SELF], None);
    chip.run_for(Cycles(100));
    assert_eq!(read(&mut chip, PERIPHERAL_BASE + 2, Size::Word), Ok(0x1234));
    write(&mut chip, PERIPHERAL_BASE + 1, Size::Byte, 0x56);
    assert_eq!(
        peek(&chip, PERIPHERAL_BASE, Size::Word),
        Err(AccessError::Unpeekable)
    );
    let peripheral = chip.peripheral::<Recorder>(recorder).unwrap();
    assert_eq!(peripheral.reads.len(), 1);
    let cycle = peripheral.reads[0];
    assert_eq!(
        (cycle.select, cycle.address, cycle.size, cycle.now),
        (on(1), PERIPHERAL_BASE + 2, Size::Word, chip.now())
    );
    assert_eq!(cycle.function_code, SUPERVISOR);
    assert_eq!(peripheral.writes.len(), 1);
    assert_eq!(peripheral.writes[0].1, 0x56);
}

#[test]
fn the_host_sees_errors_by_kind() {
    let Board { mut chip, .. } = board(&[BRANCH_TO_SELF], None);
    assert_eq!(
        read(&mut chip, FAILING_BASE, Size::Word),
        Err(AccessError::BusError)
    );
    assert_eq!(
        read(&mut chip, RAM_BASE + 1, Size::Word),
        Err(AccessError::Misaligned)
    );
    assert_eq!(
        peek(&chip, RAM_BASE + 1, Size::Long),
        Err(AccessError::Misaligned)
    );
    assert_eq!(
        chip.bus_write(Address(RAM_BASE), Size::Byte, SUPERVISOR, 0x100),
        Err(AccessError::Width(0x100))
    );
    // CSBOOT down to 64 Kbytes: past it nothing answers
    write(&mut chip, CSBARBT, Size::Word, BLOCK_64K.into());
    assert_eq!(
        read(&mut chip, 0x40_0000, Size::Word),
        Err(AccessError::Unmapped)
    );
    assert_eq!(
        peek(&chip, 0x40_0000, Size::Byte),
        Err(AccessError::Unmapped)
    );
    // a supervisor-only SIM register from user space goes external,
    // where nothing answers now (QSMRM 3.3; PLAN.md)
    let user = FunctionCode::USER_DATA;
    assert_eq!(
        chip.peek(Address(SYPCR), Size::Word, user),
        Err(AccessError::Unmapped)
    );
    assert_eq!(
        chip.bus_read(Address(SYPCR), Size::Word, user),
        Err(AccessError::Unmapped)
    );
}

#[test]
fn a_peek_of_the_sci_leaves_its_flags_and_a_read_clears_them() {
    let Board { mut chip, .. } = board(&[BRANCH_TO_SELF], None);
    // SCBR 1, 32 clocks a bit; the receiver on
    write(&mut chip, SCCR0, Size::Word, 1);
    write(&mut chip, SCCR1, Size::Word, SCCR1_RE);
    chip.sci_receive(Frame::Data(0x41));
    chip.run_until(chip.now() + Cycles(1000));
    assert_ne!(peek(&chip, SCSR, Size::Word).unwrap() & SCSR_RDRF, 0);
    assert_eq!(peek(&chip, SCDR, Size::Word), Ok(0x41));
    assert_eq!(peek(&chip, SCDR + 1, Size::Byte), Ok(0x41));
    // the clear sequence: SCSR read, then SCDR read (QSMRM 5.2.3)
    assert_eq!(read(&mut chip, SCDR, Size::Word), Ok(0x41));
    assert_ne!(peek(&chip, SCSR, Size::Word).unwrap() & SCSR_RDRF, 0);
    assert_ne!(read(&mut chip, SCSR, Size::Word).unwrap() & SCSR_RDRF, 0);
    assert_eq!(read(&mut chip, SCDR, Size::Word), Ok(0x41));
    assert_eq!(peek(&chip, SCSR, Size::Word).unwrap() & SCSR_RDRF, 0);
}

#[test]
fn a_peek_of_a_counter_is_its_read() {
    let Board { mut chip, .. } = board(&[BRANCH_TO_SELF], None);
    chip.run_until(Cycles(4000));
    let peeked = peek(&chip, TCNT, Size::Word).unwrap();
    assert_ne!(peeked, 0);
    assert_eq!(read(&mut chip, TCNT, Size::Word), Ok(peeked));
}

#[test]
fn a_module_that_cannot_peek_says_so() {
    /// A module whose registers only a read can tell.
    #[derive(Debug)]
    struct Opaque;

    impl Timed for Opaque {}

    impl mc683xx_modules::module::sealed::Sealed for Opaque {}

    impl Module for Opaque {
        fn block(&self) -> Block {
            Block {
                offset: 0x700,
                length: 0x40,
            }
        }

        fn read(&mut self, _: module::RegisterAccess) -> Option<u32> {
            Some(0)
        }

        fn peek(&self, _: module::RegisterAccess) -> Peek {
            Peek::Unpeekable
        }

        fn write(&mut self, _: module::RegisterAccess, _: u32) -> bool {
            true
        }

        fn request(&self) -> Option<module::InterruptRequest> {
            None
        }

        fn acknowledge(&mut self, _: InterruptLevel, _: Cycles) -> Option<u8> {
            None
        }

        fn reset(&mut self, _: Cycles) {}

        fn save(&self) -> Result<Vec<u8>, StateError> {
            Ok(Vec::new())
        }

        fn restore(&mut self, _: &[u8]) -> Result<(), StateError> {
            Ok(())
        }
    }

    let Board { mut chip, .. } = board(&[BRANCH_TO_SELF], None);
    chip.board.modules.push(Box::new(Opaque));
    let address = chip.sim().module_base() + 0x700;
    assert_eq!(
        peek(&chip, address, Size::Word),
        Err(AccessError::Unpeekable)
    );
    assert_eq!(read(&mut chip, address, Size::Word), Ok(0));
}

/// A program that takes the PIT's interrupt every 1024 clocks and
/// reads the recorder in a loop.
fn interrupted(ticker: Option<Cycles>) -> Board {
    let program = [
        MOVE_TO_SR,
        UNMASKED,
        MOVE_WORD_ABSOLUTE_TO_D1,
        (PERIPHERAL_BASE >> 16) as u16,
        PERIPHERAL_BASE as u16,
        // BRA back to the MOVE.W
        0x60f8,
    ];
    let mut board = board(&program, ticker);
    // level 6, PITM 1: 4 EXTAL periods of 256 clocks (MC68331UM 4.2.11)
    write(
        &mut board.chip,
        PICR,
        Size::Word,
        0x0600 | u32::from(PIT_VECTOR),
    );
    write(&mut board.chip, PITR, Size::Word, 1);
    board
}

/// What a run leaves: time, instructions, registers, the handler's
/// count and when the recorder was read.
fn outcome(board: &Board) -> (Cycles, u64, Registers, Vec<u8>, Vec<Cycles>) {
    let chip = &board.chip;
    let reads = chip
        .peripheral::<Recorder>(board.recorder)
        .unwrap()
        .reads
        .iter();
    (
        chip.now(),
        chip.instructions(),
        chip.cpu().registers.clone(),
        chip.memory(board.ram)[..2].to_vec(),
        reads.map(|cycle| cycle.now).collect(),
    )
}

#[test]
fn a_run_until_ends_at_the_first_boundary_at_or_after_its_target() {
    let mut board = interrupted(None);
    let chip = &mut board.chip;
    let run = chip.run_until(Cycles(10_000));
    assert_eq!(run.stop, Stop::Budget);
    assert!(chip.now() >= Cycles(10_000) && chip.now() < Cycles(10_100));
    assert_eq!(run.cycles, chip.now());
    let now = chip.now();
    let past = chip.run_until(Cycles(5000));
    assert_eq!((past.cycles, past.stop), (Cycles::ZERO, Stop::Budget));
    assert_eq!(chip.now(), now);
    let count = &chip.memory(board.ram)[..2];
    assert!(
        u16::from_be_bytes([count[0], count[1]]) >= 8,
        "the PIT's handler ran"
    );
}

#[test]
fn a_stopped_cpu_runs_until_its_target_exactly() {
    let Board { mut chip, .. } = board(&[STOP, UNMASKED], None);
    let run = chip.run_until(Cycles(7777));
    assert_eq!(run.stop, Stop::Stopped);
    assert_eq!(chip.now(), Cycles(7777));
}

#[test]
fn runs_until_rising_targets_are_one_long_run() {
    const END: Cycles = Cycles(60_000);
    let mut long = interrupted(None);
    long.chip.run_until(END);
    // targets closer than an instruction, irregular, and synced by a
    // model's events too
    for ticker in [None, Some(Cycles(333))] {
        let mut short = interrupted(ticker);
        let steps = [1, 7, 100, 13, 1500, 3, 999];
        let mut target = Cycles::ZERO;
        for step in steps.iter().cycle() {
            target = (target + Cycles(*step)).min(END);
            short.chip.run_until(target);
            if target == END {
                break;
            }
        }
        assert_eq!(outcome(&short), outcome(&long), "ticker {ticker:?}");
    }
    assert!(outcome(&long).4.len() > 100);
    assert_ne!(outcome(&long).3, [0, 0], "the PIT interrupted");
}

#[test]
fn a_stop_a_peripheral_asks_for_in_a_host_cycle_stops_no_run() {
    /// Asks the run to stop at each write.
    #[derive(Debug)]
    struct Stopper(StopRequest);

    impl MappedPeripheral for Stopper {
        fn read(&mut self, _: BusCycle) -> Result<u32, BusError> {
            Ok(0)
        }

        fn write(&mut self, _: BusCycle, _: u32) -> Result<(), BusError> {
            self.0.request();
            Ok(())
        }
    }

    mc683xx_cpu32::unsavable!(Stopper, "a stopper");

    impl Timed for Stopper {}

    let mut builder = Builder::default();
    builder
        .memory(rom(&[BRANCH_TO_SELF]), false, &[ChipSelect::Boot])
        .unwrap();
    let stop = builder.stop_request();
    builder.peripheral(Stopper(stop.clone()), &[cs(1)]).unwrap();
    let mut chip = builder.build();
    let bar = CSBAR0 + SELECT_BYTES;
    write(
        &mut chip,
        bar,
        Size::Word,
        (PERIPHERAL_BASE >> 8) | u32::from(BLOCK_64K),
    );
    write(&mut chip, bar + 2, Size::Word, CSOR_FAST.into());
    write(&mut chip, PERIPHERAL_BASE, Size::Word, 1);
    assert_eq!(chip.run_for(Cycles(100)).stop, Stop::Budget);
    // one asked for before the cycle stays
    stop.request();
    write(&mut chip, PERIPHERAL_BASE, Size::Word, 1);
    assert_eq!(chip.run_for(Cycles(100)).stop, Stop::Requested);
}
