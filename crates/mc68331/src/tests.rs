//! The chip's decode, timing, interrupts and run loop, on boards
//! wired with a map of every kind of select and as other hardware
//! might be.

use mc683xx_modules::pins::PortLevels;
use std::cell::Cell;
use std::ops::ControlFlow;
use std::rc::Rc;

use mc683xx_cpu32::address::Address;
use mc683xx_cpu32::bus::BusError;
use mc683xx_cpu32::cpu;
use mc683xx_cpu32::cpu::Halt;
use mc683xx_cpu32::cpu::Stop;
use mc683xx_cpu32::cpu::state::Save;
use mc683xx_cpu32::cpu::state::StateError;
use mc683xx_cpu32::cpu::state::Stateless;
use mc683xx_cpu32::cpu::state::decode;
use mc683xx_cpu32::cpu::state::encode;
use mc683xx_cpu32::cycles::Cycles;
use mc683xx_cpu32::exception::Vector;
use mc683xx_cpu32::frequency::Frequency;
use mc683xx_cpu32::function_code::FunctionCode;
use mc683xx_cpu32::interrupt::Acknowledge;
use mc683xx_cpu32::interrupt::InterruptLevel;
use mc683xx_cpu32::interrupt::IrqPin;
use mc683xx_cpu32::memory::Space;
use mc683xx_cpu32::peripheral::BusCycle;
use mc683xx_cpu32::peripheral::MappedPeripheral;
use mc683xx_cpu32::peripheral::Select;
use mc683xx_cpu32::size::Size;
use mc683xx_modules::chip_select::ChipSelect;
use mc683xx_modules::chip_select::Number;
use mc683xx_modules::module;
use mc683xx_modules::module::Block;
use mc683xx_modules::module::Module;
use mc683xx_modules::peripheral::QspiClock;
use mc683xx_modules::peripheral::QspiTransfer;
use mc683xx_modules::peripheral::SpiPeripheral;
use mc683xx_modules::peripheral::Timed;
use mc683xx_modules::qsm::sci::Frame;
use mc683xx_modules::sim::Configuration;
use mc683xx_modules::sim::ResetCause;
use mc683xx_modules::sim::clock::ClockSource;
use mc683xx_modules::sim::ports::Change;
use mc683xx_modules::sim::ports::PortName;
use snapshot_golden::version_of;

use crate::board;
use crate::board::stall;
use crate::chip::Builder;
use crate::chip::Diversion;
use crate::chip::Mc68331;
use crate::chip::MemoryId;
use crate::chip::OutputStop;
use crate::chip::SyncRequest;
use crate::event::ChipTracing;
use crate::event::DsackFault;
use crate::event::Event;
use crate::gpt::Input;
use crate::gpt::OutputChange;
use crate::gpt::PortGpPin;
use crate::gpt::pwm::Channel;
use crate::host::Unserved;
use crate::state;
use mc683xx_cpu32::bus::StopRequest;
use mc683xx_cpu32::peripheral::PeripheralId;
use mc683xx_modules::sim::chip_selects::Dsack;
use mc683xx_modules::sim::reset::ResetScope;

mod bus;
mod fixed;

/// The NOP instruction (CPU32RM 4, NOP).
const NOP: u16 = 0x4e71;
/// MOVE.W #imm,(xxx).L, MOVE.B #imm,(xxx).L and MOVE.W (xxx).L,Dn
/// (CPU32RM 4, MOVE).
const MOVE_WORD_TO_ABSOLUTE: u16 = 0x33fc;
/// MOVE.B #imm,(xxx).L.
const MOVE_BYTE_TO_ABSOLUTE: u16 = 0x13fc;
/// MOVE.W (xxx).L,D0; the register is in bits 11:9.
const MOVE_WORD_FROM_ABSOLUTE: u16 = 0x3039;
/// The initial stack pointer and program counter in the test images.
const STACK: u32 = 0x10_fffe;
/// Where the test programs start.
const START: u32 = 0x400;
/// The SIM's registers with MM set.
const SIM: u32 = 0xff_fa00;
/// The QSM's registers with MM set (MC68331UM Table D-1).
const QSM: u32 = 0xff_fc00;
/// The GPT's.
const GPT: u32 = 0xff_f900;
/// The boot ROM's size in most tests.
const ROM_BYTES: usize = 64 << 10;

fn cs(number: u8) -> ChipSelect {
    ChipSelect::Numbered(Number::new(number).unwrap())
}

/// CS`number` as a cycle carries it.
fn on(number: u8) -> Option<Select> {
    Some(cs(number).into())
}

/// A ROM image of `bytes` NOPs with the reset vectors and `program`
/// at `START`.
fn image(program: &[u16], bytes: usize) -> Vec<u8> {
    let mut image: Vec<u8> = NOP.to_be_bytes().repeat(bytes / 2);
    image[0..4].copy_from_slice(&STACK.to_be_bytes());
    image[4..8].copy_from_slice(&START.to_be_bytes());
    for (index, word) in program.iter().enumerate() {
        let at = START as usize + 2 * index;
        image[at..at + 2].copy_from_slice(&word.to_be_bytes());
    }
    image
}

/// MOVE.W #`value`,`address`.
fn store_word(address: u32, value: u16) -> [u16; 4] {
    [
        MOVE_WORD_TO_ABSOLUTE,
        value,
        (address >> 16) as u16,
        address as u16,
    ]
}

/// MOVE.B #`value`,`address`.
fn store_byte(address: u32, value: u8) -> [u16; 4] {
    [
        MOVE_BYTE_TO_ABSOLUTE,
        value.into(),
        (address >> 16) as u16,
        address as u16,
    ]
}

/// MOVE.W `address`,D`register`.
fn load_word(address: u32, register: u16) -> [u16; 3] {
    [
        MOVE_WORD_FROM_ABSOLUTE | register << 9,
        (address >> 16) as u16,
        address as u16,
    ]
}

/// A peripheral that records its cycles and answers `value`.  Its records
/// are the test's, as the chip's tracing is, not the peripheral's state.
#[derive(Debug, Default)]
struct Recorder {
    /// Its read cycles.
    reads: Vec<BusCycle>,
    /// Its write cycles and values.
    writes: Vec<(BusCycle, u32)>,
    /// What it reads as.
    value: u32,
    /// Its wait states on an externally terminated select.
    wait: Option<u8>,
    /// Whether it asks for an interrupt.
    interrupt: bool,
    /// The resets it saw.
    resets: usize,
}

impl Recorder {
    fn answering(value: u32) -> Self {
        Self {
            value,
            wait: Some(0),
            ..Self::default()
        }
    }
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

    fn wait_states(&mut self, _: BusCycle) -> Option<u8> {
        self.wait
    }

    fn interrupt_request(&self) -> bool {
        self.interrupt
    }

    fn reset(&mut self, _: Cycles) {
        self.resets += 1;
    }
}

impl Stateless for Recorder {}

impl Timed for Recorder {}

/// A model with an event at a fixed time, recording when it is
/// brought forward.
#[derive(Debug)]
struct Alarm {
    /// Its event.
    at: Cycles,
    /// The times it was advanced to.
    advanced: Rc<Cell<Option<Cycles>>>,
}

impl MappedPeripheral for Alarm {
    fn read(&mut self, _: BusCycle) -> Result<u32, BusError> {
        Err(BusError)
    }

    fn write(&mut self, _: BusCycle, _: u32) -> Result<(), BusError> {
        Err(BusError)
    }
}

mc683xx_cpu32::unsavable!(Alarm, "an alarm");

impl Timed for Alarm {
    fn next_event(&self) -> Option<Cycles> {
        (self.advanced.get().is_none()).then_some(self.at)
    }

    fn advance(&mut self, now: Cycles) {
        if now >= self.at && self.advanced.get().is_none() {
            self.advanced.set(Some(now));
        }
    }
}

/// A peripheral that asserts BERR on every cycle.
#[derive(Debug)]
struct Faulting;

impl MappedPeripheral for Faulting {
    fn read(&mut self, _: BusCycle) -> Result<u32, BusError> {
        Err(BusError)
    }

    fn write(&mut self, _: BusCycle, _: u32) -> Result<(), BusError> {
        Err(BusError)
    }
}

impl Stateless for Faulting {}

impl Timed for Faulting {}

/// A chip with a 64-Kbyte boot ROM on CSBOOT running `program`.
fn booting(program: &[u16]) -> (Mc68331, MemoryId) {
    let mut builder = Builder::default();
    let rom = builder
        .memory(image(program, ROM_BYTES), false, &[ChipSelect::Boot])
        .unwrap();
    (builder.build(), rom)
}

/// A cycle in `function_code`'s space straight to the board.
fn cycle_in(
    chip: &mut Mc68331,
    function_code: FunctionCode,
    address: u32,
    size: Size,
    write: Option<u32>,
) -> Result<(u32, u16), BusError> {
    let Mc68331 {
        cpu, board, now, ..
    } = chip;
    board.cycle(
        Some(&mut cpu.memory),
        address,
        size,
        function_code,
        *now,
        write,
    )
}

/// A supervisor data cycle straight to the board.
fn cycle(
    chip: &mut Mc68331,
    address: u32,
    size: Size,
    write: Option<u32>,
) -> Result<(u32, u16), BusError> {
    cycle_in(chip, FunctionCode::SUPERVISOR_DATA, address, size, write)
}

fn write_register(chip: &mut Mc68331, address: u32, value: u16) {
    cycle(chip, address, Size::Word, Some(value.into())).unwrap();
}

#[test]
fn csboot_serves_the_reset_vectors_with_13_wait_states() {
    let (mut chip, _) = booting(&[NOP]);
    assert_eq!(chip.halted(), None);
    assert_eq!(chip.cpu().registers.ssp, STACK);
    assert_eq!(chip.cpu().registers.pc, START);
    // one NOP: a two-clock fetch from a 16-clock select (MC68331UM
    // Table 4-24; SIMRM 7.5.1)
    let run = chip.run_for(Cycles(1));
    assert_eq!(run.cycles, Cycles(16));
    assert_eq!(run.stop, Stop::Budget);
    assert_eq!(
        chip.take_events(),
        [Event::Reset(Cycles::ZERO, ResetCause::PowerOn)]
    );
    // the 1-Mbyte block repeats the 64-Kbyte ROM
    assert_eq!(cycle(&mut chip, 0x1_0004, Size::Word, None), Ok((0, 16)));
    assert_eq!(
        cycle(&mut chip, 0x1_0406, Size::Word, None),
        Ok((u32::from(NOP), 16))
    );
}

#[test]
fn nothing_answers_outside_the_selects() {
    let (mut chip, _) = booting(&[NOP]);
    assert_eq!(cycle(&mut chip, 0x10_0000, Size::Word, None), Err(BusError));
    // nor in CPU space, but for the LPSTOP broadcast the SIM takes
    let Mc68331 { board, .. } = &mut chip;
    let breakpoint = board.cycle(
        None,
        0,
        Size::Word,
        FunctionCode::CPU_SPACE,
        Cycles::ZERO,
        None,
    );
    assert_eq!(breakpoint, Err(BusError));
    let lpstop = board.cycle(
        None,
        0x3_fffe,
        Size::Word,
        FunctionCode::CPU_SPACE,
        Cycles::ZERO,
        Some(5),
    );
    assert_eq!(lpstop, Ok((0, 2)));
    assert_eq!(board.sim.low_power(), Some(5));
}

#[test]
fn a_chip_select_write_remaps_at_once() {
    // CSBARBT to 64 Kbytes, then a read past it
    let program: Vec<u16> = store_word(SIM + 0x48, 0x0003)
        .into_iter()
        .chain(load_word(0x1_0000, 0))
        .collect();
    let (mut chip, _) = booting(&program);
    assert_eq!(
        cycle(&mut chip, 0x1_0000, Size::Word, None),
        Ok((0x0010, 16))
    );
    let run = chip.run_for(Cycles(200));
    // the read faulted: until exception processing lands the core
    // halts on it
    assert!(matches!(run.stop, Stop::Halted(_)));
    assert_eq!(cycle(&mut chip, 0x1_0000, Size::Word, None), Err(BusError));
    assert_eq!(
        cycle(&mut chip, 0xfffe, Size::Word, None),
        Ok((u32::from(NOP), 16))
    );
}

#[test]
fn mm_moves_the_module_space() {
    let (mut chip, _) = booting(&[NOP]);
    write_register(&mut chip, SIM, 0x008f);
    assert_eq!(chip.sim().module_base(), 0x7f_f000);
    assert_eq!(
        cycle(&mut chip, 0x7f_fa04, Size::Word, None),
        Ok((0x3f08, 2))
    );
    // its old place is external, where no select answers
    assert_eq!(cycle(&mut chip, SIM + 4, Size::Word, None), Err(BusError));
}

#[test]
fn user_mode_sim_cycles_go_external() {
    let (mut chip, _) = booting(&[NOP]);
    // CSBOOT on the module space too, user and supervisor
    write_register(&mut chip, SIM + 0x48, 0xfff8 | 0b111);
    let Mc68331 { cpu, board, .. } = &mut chip;
    let user = board.cycle(
        Some(&mut cpu.memory),
        SIM + 0x04,
        Size::Word,
        FunctionCode::USER_DATA,
        Cycles::ZERO,
        None,
    );
    // CSBOOT's 1-Mbyte block from $F00000 repeats the ROM
    assert_eq!(user, Ok((u32::from(NOP), 16)));
}

/// A synthetic map with every kind of select the decode handles
/// (SIMRM 7.10; MC68331UM D.3.23 to D.3.26): CSPAR0 and CSPAR1, then
/// each select's CSBAR and CSOR.  ROM in two 128-Kbyte banks, CSBOOT's
/// and CS1's, read only with 2 wait states, CS6 and CS7 taking their
/// writes; 128 Kbytes of supervisor RAM, read on CS0 and written a
/// byte lane each on CS2 (upper) and CS3 (lower); an 8-bit peripheral on
/// CS4 with 10 wait states; a DUART on CS8, an 8-bit port that ends
/// its own cycles, and its interrupt acknowledge at level 1 on CS10;
/// CS9, on a 16-bit port with 4 wait states, with nothing wired.  CS5
/// is a discrete output.
const WIRED_SELECTS: [(u32, u16); 24] = [
    // CSBOOT and CS0 to CS3 16-bit, CS4 8-bit, CS5 a discrete output
    (0x44, 0x0bff),
    // CS6, CS7 and CS10 16-bit, CS8 8-bit, CS9 16-bit
    (0x46, 0x03ef),
    // CSBOOT: $000000, 128 Kbytes; both bytes, read, 2 wait states,
    // supervisor and user
    (0x48, 0x0004),
    (0x4a, 0x68b0),
    // CS0: $100000, 128 Kbytes; both bytes, read, no wait states,
    // supervisor
    (0x4c, 0x1004),
    (0x4e, 0x6820),
    // CS1: $020000, 128 Kbytes; as CSBOOT
    (0x50, 0x0204),
    (0x52, 0x68b0),
    // CS2: CS0's block; the upper byte, write
    (0x54, 0x1004),
    (0x56, 0x5020),
    // CS3: CS0's block; the lower byte, write
    (0x58, 0x1004),
    (0x5a, 0x3020),
    // CS4: $0C0000, 16 Kbytes; both bytes, read and write, 10 wait
    // states, supervisor and user
    (0x5c, 0x0c02),
    (0x5e, 0x7ab0),
    // CS6: CSBOOT's block; both bytes, write, 2 wait states
    (0x64, 0x0004),
    (0x66, 0x70b0),
    // CS7: CS1's block; as CS6
    (0x68, 0x0204),
    (0x6a, 0x70b0),
    // CS8: $0E0000, 2 Kbytes; the lower byte, read and write, external
    // DSACK, supervisor
    (0x6c, 0x0e00),
    (0x6e, 0x3be0),
    // CS9: $0F0000, 8 Kbytes; both bytes, read and write, 4 wait
    // states, supervisor and user
    (0x70, 0x0f01),
    (0x72, 0x7930),
    // CS10: $FFF800 in CPU space, interrupt acknowledges at level 1;
    // the lower byte, read, external DSACK (SIMRM 7.8.1)
    (0x74, 0xfff8),
    (0x76, 0x2bc2),
];

/// The lower ROM bank's size, CSBOOT's block; CS1's upper bank is the
/// same size.
const ROM_BANK_BYTES: usize = 128 << 10;

/// The wired board, its map programmed, then `program`.
struct Wired {
    chip: Mc68331,
    ram: MemoryId,
    flash_writes: PeripheralId,
    shared: PeripheralId,
    duart: PeripheralId,
}

fn wired(program: &[u16]) -> Wired {
    wired_patched(program, &[])
}

/// The wired board running `program`, its ROM's words at each
/// address in `patches` replaced: vectors and handlers.
fn wired_patched(program: &[u16], patches: &[(u32, &[u16])]) -> Wired {
    let mut code: Vec<u16> = WIRED_SELECTS
        .iter()
        .flat_map(|&(offset, value)| store_word(SIM + offset, value))
        .collect();
    code.extend_from_slice(program);
    let mut lower = image(&code, ROM_BANK_BYTES);
    lower[ROM_BANK_BYTES - 2..].copy_from_slice(&0x1111_u16.to_be_bytes());
    for &(address, words) in patches {
        for (index, word) in words.iter().enumerate() {
            let at = address as usize + 2 * index;
            lower[at..at + 2].copy_from_slice(&word.to_be_bytes());
        }
    }
    let upper = 0x2222_u16.to_be_bytes().repeat(ROM_BANK_BYTES / 2);
    let mut builder = Builder::default();
    builder.memory(lower, false, &[ChipSelect::Boot]).unwrap();
    builder.memory(upper, false, &[cs(1)]).unwrap();
    let flash_writes = builder
        .peripheral(Recorder::default(), &[cs(6), cs(7)])
        .unwrap();
    let ram = builder
        .memory(vec![0; 128 << 10], true, &[cs(0), cs(2), cs(3)])
        .unwrap();
    let shared = builder
        .peripheral(Recorder::answering(0xab), &[cs(4)])
        .unwrap();
    let duart = builder
        .peripheral(Recorder::answering(0x40), &[cs(8), cs(10)])
        .unwrap();
    builder.interrupt(duart, IrqPin::new(1).unwrap());
    let mut chip = builder.build();
    chip.run_for(Cycles(40 * WIRED_SELECTS.len() as u64));
    assert_eq!(chip.halted(), None);
    Wired {
        chip,
        ram,
        flash_writes,
        shared,
        duart,
    }
}

#[test]
fn the_wired_map() {
    let program: Vec<u16> = [
        store_word(0x10_0000, 0x1234),
        store_byte(0x10_0003, 0x56),
        store_word(0xaaa, 0xaaaa),
    ]
    .concat()
    .into_iter()
    .chain(load_word(0x10_0000, 0))
    .chain(load_word(0x2_0000, 1))
    .chain(load_word(0x1_fffe, 3))
    .chain(load_word(0x0c_2000, 2))
    .collect();
    let mut board = wired(&program);
    let chip = &mut board.chip;
    chip.run_for(Cycles(400));
    assert_eq!(chip.halted(), None);
    let registers = &chip.cpu().registers;
    assert_eq!(registers.data[0] & 0xffff, 0x1234);
    assert_eq!(registers.data[1] & 0xffff, 0x2222, "the upper bank on CS1");
    assert_eq!(registers.data[3] & 0xffff, 0x1111, "the lower bank's end");
    assert_eq!(registers.data[2] & 0xffff, 0xabab);
    assert_eq!(chip.memory(board.ram)[..4], [0x12, 0x34, 0x00, 0x56]);
    let writes = &chip
        .peripheral::<Recorder>(board.flash_writes)
        .unwrap()
        .writes;
    assert_eq!(writes.len(), 1);
    assert_eq!(writes[0].0.select, on(6));
    assert_eq!(writes[0].0.address, 0xaaa);
    assert_eq!(writes[0].1, 0xaaaa);
    // the 8-bit port: a word is two byte cycles on CS4
    let reads = &chip.peripheral::<Recorder>(board.shared).unwrap().reads;
    let addresses: Vec<_> = reads
        .iter()
        .map(|read| (read.address, read.size, read.select))
        .collect();
    assert_eq!(
        addresses,
        [
            (0x0c_2000, Size::Byte, on(4)),
            (0x0c_2001, Size::Byte, on(4))
        ]
    );
}

#[test]
fn wired_timing() {
    let mut board = wired(&[]);
    let chip = &mut board.chip;
    // RAM: 0 wait states, 3 clocks; ROM: 2 wait states; CS4: 10 wait
    // states on an 8-bit port, a word two 13-clock cycles; the DUART's
    // own DSACK
    assert_eq!(cycle(chip, 0x10_0000, Size::Word, None), Ok((0, 3)));
    assert_eq!(cycle(chip, 0x2_0000, Size::Word, None), Ok((0x2222, 5)));
    assert_eq!(cycle(chip, 0x0c_0000, Size::Word, None), Ok((0xabab, 26)));
    assert_eq!(cycle(chip, 0x0c_0000, Size::Byte, None), Ok((0xab, 13)));
    assert_eq!(cycle(chip, 0x0e_0001, Size::Byte, None), Ok((0x40, 3)));
    // CS9 has nothing wired: the select still ends the cycle, a word
    // in one on its 16-bit port
    assert_eq!(cycle(chip, 0x0f_0000, Size::Byte, None), Ok((0, 7)));
    assert_eq!(cycle(chip, 0x0f_0000, Size::Word, None), Ok((0, 7)));
    // RAM is supervisor space only
    assert_eq!(
        cycle_in(chip, FunctionCode::USER_DATA, 0x10_0000, Size::Word, None),
        Err(BusError)
    );
    // ROM is read only on CSBOOT, so a write there goes to CS6
    assert_eq!(cycle(chip, 0x100, Size::Word, Some(0x5555)), Ok((0, 5)));
    // a NOP from ROM: two wait states
    let before = chip.now();
    let run = chip.run_for(Cycles(1));
    assert_eq!(run.cycles, Cycles(5));
    assert_eq!(chip.now() - before, Cycles(5));
}

#[test]
fn the_duart_is_vectored_through_cs10() {
    let mut board = wired(&[]);
    let chip = &mut board.chip;
    assert_eq!(chip.board.level(), InterruptLevel::NONE);
    chip.peripheral_mut::<Recorder>(board.duart)
        .unwrap()
        .interrupt = true;
    let level = InterruptLevel::new(1).unwrap();
    assert_eq!(chip.board.level(), level);
    assert_eq!(
        chip.board.acknowledge(level, Cycles::ZERO).answer,
        Acknowledge::Vector(0x40)
    );
    let reads = &chip.peripheral::<Recorder>(board.duart).unwrap().reads;
    let acknowledge = reads.last().unwrap();
    assert_eq!(acknowledge.select, on(10));
    assert_eq!(acknowledge.address, 0xff_fff3);
    assert_eq!(acknowledge.size, Size::Byte);
    assert_eq!(acknowledge.function_code, FunctionCode::CPU_SPACE);
    // PFPAR clear makes IRQ1 a port pin
    write_register(chip, SIM + 0x1e, 0x00fd);
    assert_eq!(chip.board.level(), InterruptLevel::NONE);
}

/// A board unlike the wired one: an 8-bit boot ROM (DATA0 low), RAM in
/// user space only on CS1 with fast termination, and an autovectoring
/// select.
#[test]
fn another_board() {
    let configuration = Configuration {
        data: !1,
        ..Configuration::default()
    };
    let mut builder = Builder::new(configuration);
    builder
        .memory(image(&[NOP], ROM_BYTES), false, &[ChipSelect::Boot])
        .unwrap();
    let ram = builder.memory(vec![0; 32 << 10], true, &[cs(1)]).unwrap();
    builder.autovector(IrqPin::new(3).unwrap());
    let mut chip = builder.build();
    // a word fetch is two 16-clock byte cycles
    assert_eq!(chip.run_for(Cycles(1)).cycles, Cycles(32));
    // CS1: a 64-Kbyte block at $200000 holding 32 Kbytes, user only,
    // fast termination
    write_register(&mut chip, SIM + 0x50, 0x2003);
    write_register(&mut chip, SIM + 0x52, 0x7b90);
    let user = |chip: &mut Mc68331, write| {
        cycle_in(chip, FunctionCode::USER_DATA, 0x20_8002, Size::Word, write)
    };
    assert_eq!(user(&mut chip, Some(0xbeef)), Ok((0, 2)));
    assert_eq!(user(&mut chip, None), Ok((0xbeef, 2)));
    assert_eq!(
        chip.memory(ram)[2..4],
        [0xbe, 0xef],
        "32 Kbytes repeat in 64"
    );
    assert_eq!(
        cycle(&mut chip, 0x20_0000, Size::Word, None),
        Err(BusError),
        "supervisor"
    );
    // IRQ3 with AVEC (PE2) tied for level 3; IRQ4 answered by nothing
    let three = IrqPin::new(3).unwrap();
    chip.set_interrupt_pin(three, true);
    assert_eq!(
        chip.board.acknowledge(three.level(), Cycles::ZERO).answer,
        Acknowledge::Autovector
    );
    let four = IrqPin::new(4).unwrap();
    chip.set_interrupt_pin(four, true);
    assert_eq!(chip.board.level(), four.level());
    assert_eq!(
        chip.board.acknowledge(four.level(), Cycles::ZERO).answer,
        Acknowledge::Spurious
    );
}

#[test]
fn overlapping_selects_end_at_the_first_dsack() {
    let mut builder = Builder::default();
    builder
        .memory(image(&[NOP], ROM_BYTES), false, &[ChipSelect::Boot])
        .unwrap();
    let slow = builder
        .peripheral(Recorder::answering(0x1200), &[cs(0)])
        .unwrap();
    let fast = builder
        .peripheral(Recorder::answering(0x0034), &[cs(1)])
        .unwrap();
    let mut chip = builder.build();
    // CS0 the upper byte, 5 wait states; CS1 the lower byte, fast
    write_register(&mut chip, SIM + 0x4c, 0x5000);
    write_register(&mut chip, SIM + 0x4e, 0x5970);
    write_register(&mut chip, SIM + 0x50, 0x5000);
    write_register(&mut chip, SIM + 0x52, 0x3bb0);
    assert_eq!(
        cycle(&mut chip, 0x50_0000, Size::Word, None),
        Ok((0x1234, 2))
    );
    assert_eq!(cycle(&mut chip, 0x50_0000, Size::Byte, None), Ok((0x00, 8)));
    assert_eq!(chip.peripheral::<Recorder>(slow).unwrap().reads.len(), 2);
    assert_eq!(chip.peripheral::<Recorder>(fast).unwrap().reads.len(), 1);
}

#[test]
fn an_external_dsack_waits_on_the_peripheral_and_the_bus_monitor() {
    let mut builder = Builder::default();
    builder
        .memory(image(&[NOP], ROM_BYTES), false, &[ChipSelect::Boot])
        .unwrap();
    let peripheral = builder
        .peripheral(
            Recorder {
                wait: Some(20),
                ..Recorder::default()
            },
            &[cs(2)],
        )
        .unwrap();
    let mut chip = builder.build();
    write_register(&mut chip, SIM + 0x54, 0x6000);
    write_register(&mut chip, SIM + 0x56, 0x7bf0);
    assert_eq!(cycle(&mut chip, 0x60_0000, Size::Word, None), Ok((0, 23)));
    // BME with BMT 16 clocks (SIMRM 3.2)
    write_register(&mut chip, SIM + 0x20, 0x0006);
    assert_eq!(cycle(&mut chip, 0x60_0000, Size::Word, None), Err(BusError));
    // CSBOOT's 16 clocks still end inside it, but 13 wait states are
    // too many for 8 (and the core's ROM pages follow)
    assert_eq!(
        cycle(&mut chip, 0x0, Size::Word, None),
        Ok((STACK >> 16, 16))
    );
    chip.peripheral_mut::<Recorder>(peripheral).unwrap().wait = None;
    assert_eq!(cycle(&mut chip, 0x60_0000, Size::Word, None), Err(BusError));
}

/// CSPAR0 (MC68331UM Table D-1).
const CSPAR0: u32 = SIM + 0x44;
/// CSPAR0 out of reset but CS2 an 8-bit port, %10 in bits 7:6 (SIMRM
/// 7.3.1, Table 7-4).
const CSPAR0_CS2_8_BIT: u16 = 0x3fbf;
/// CS2's option as [`externally_terminated`]'s but DSACK zero wait states
/// (SIMRM 7.10.3).
const CS2_INTERNAL: u16 = 0x7830;

/// A chip with a peripheral on CS2 at $600000, externally terminated, that
/// drives `pins`, and the peripheral.
fn externally_terminated(pins: Dsack) -> (Mc68331, PeripheralId) {
    let mut builder = Builder::default();
    builder
        .memory(image(&[NOP], ROM_BYTES), false, &[ChipSelect::Boot])
        .unwrap();
    let peripheral = builder
        .peripheral(Recorder::answering(0x1234), &[cs(2)])
        .unwrap();
    builder.dsack(peripheral, pins);
    let mut chip = builder.build();
    write_register(&mut chip, SIM + 0x54, 0x6000);
    write_register(&mut chip, SIM + 0x56, 0x7bf0);
    chip.take_events();
    (chip, peripheral)
}

#[test]
fn a_peripherals_dsack_is_checked_against_pepar() {
    let (mut chip, _) = externally_terminated(Dsack::Dsack1);
    assert_eq!(
        cycle(&mut chip, 0x60_0000, Size::Word, None),
        Ok((0x1234, 3))
    );
    assert_eq!(chip.take_events(), []);
    // PEPAR 0: PE0 and PE1 are port E pins, so no DSACK ends the cycle
    // (SIMRM 9.1) and, with no bus monitor, it never ends: BERR
    let (mut chip, _) = externally_terminated(Dsack::Both);
    write_register(&mut chip, SIM + 0x16, 0x0000);
    assert_eq!(cycle(&mut chip, 0x60_0000, Size::Word, None), Err(BusError));
    assert_eq!(
        chip.take_events(),
        [Event::Dsack(
            Cycles::ZERO,
            DsackFault::Unassigned { select: cs(2) }
        )]
    );
    // PE1 alone assigned: DSACK1 ends it
    write_register(&mut chip, SIM + 0x16, 0x0002);
    assert_eq!(
        cycle(&mut chip, 0x60_0000, Size::Word, None),
        Ok((0x1234, 3))
    );
}

#[test]
fn a_dsack_fault_told_stays_told_across_a_restore() {
    let unassigned = |chip: &mut Mc68331| {
        // PEPAR 0: no DSACK pin assigned (SIMRM 9.1)
        write_register(chip, SIM + 0x16, 0x0000);
        assert_eq!(cycle(chip, 0x60_0000, Size::Word, None), Err(BusError));
        chip.take_events()
    };
    let (mut chip, _) = externally_terminated(Dsack::Both);
    let fresh = chip.save().unwrap();
    assert_eq!(unassigned(&mut chip).len(), 1);
    let told = chip.save().unwrap();
    let (mut other, _) = externally_terminated(Dsack::Both);
    other.restore(&told).unwrap();
    assert_eq!(unassigned(&mut other), [], "told once, before the save");
    other.restore(&fresh).unwrap();
    assert_eq!(unassigned(&mut other).len(), 1, "not yet told");
    for bad in [vec![], vec![0x80]] {
        assert!(matches!(
            other.restore(&state::with_dsack_told(&told, bad)),
            Err(StateError::Invalid(reason)) if reason == "the DSACK faults told"
        ));
    }
}

#[test]
fn an_external_dsack_sizes_the_cycle() {
    // DSACK0 alone is an 8-bit port (SIMRM Table 5-2) though CS2's
    // field is 16 bits: the peripheral's DSACK sizes the cycle (SIMRM
    // 5.3.3), so a word is two byte cycles (MC68331UM 4.4.2)
    let (mut chip, peripheral) = externally_terminated(Dsack::Dsack0);
    assert_eq!(
        cycle(&mut chip, 0x60_0000, Size::Word, None),
        Ok((0x3434, 6))
    );
    assert_eq!(
        cycle(&mut chip, 0x60_0000, Size::Word, Some(0xabcd)),
        Ok((0, 6))
    );
    let recorder = chip.peripheral::<Recorder>(peripheral).unwrap();
    let reads: Vec<(u32, Size)> = recorder
        .reads
        .iter()
        .map(|read| (read.address, read.size))
        .collect();
    assert_eq!(reads, [(0x60_0000, Size::Byte), (0x60_0001, Size::Byte)]);
    let writes: Vec<(u32, u32)> = recorder
        .writes
        .iter()
        .map(|(write, value)| (write.address, *value))
        .collect();
    assert_eq!(writes, [(0x60_0000, 0xab), (0x60_0001, 0xcd)]);
    assert_eq!(chip.take_events(), []);
    // DSACK1 on an 8-bit select: one word cycle
    let (mut chip, _) = externally_terminated(Dsack::Dsack1);
    write_register(&mut chip, CSPAR0, CSPAR0_CS2_8_BIT);
    assert_eq!(
        cycle(&mut chip, 0x60_0000, Size::Word, None),
        Ok((0x1234, 3))
    );
    // with the select's own DSACK its field governs (SIMRM 7.6): a word
    // on the 8-bit select is two byte cycles, whatever the peripheral's pins
    write_register(&mut chip, SIM + 0x56, CS2_INTERNAL);
    assert_eq!(
        cycle(&mut chip, 0x60_0000, Size::Word, None),
        Ok((0x3434, 6))
    );
    // and on the 16-bit field one word cycle, the peripheral's DSACK0
    // notwithstanding
    let (mut chip, _) = externally_terminated(Dsack::Dsack0);
    write_register(&mut chip, SIM + 0x56, CS2_INTERNAL);
    assert_eq!(
        cycle(&mut chip, 0x60_0000, Size::Word, None),
        Ok((0x1234, 3))
    );
}

#[test]
fn a_split_word_cycle_counts_both_bytes_clocks() {
    // 200 wait states a byte cycle: two of 203 clocks pass a byte
    let (mut chip, peripheral) = externally_terminated(Dsack::Dsack0);
    chip.peripheral_mut::<Recorder>(peripheral).unwrap().wait = Some(200);
    assert_eq!(
        cycle(&mut chip, 0x60_0000, Size::Word, None),
        Ok((0x3434, 2 * 203))
    );
    assert_eq!(stall(2 * 203), Cycles(2 * 203 - 2));
}

/// A peripheral whose writes divert a memory to another peripheral and back:
/// a flash's command logic.
#[derive(Debug)]
struct Switch {
    /// The chip's handle.
    diversion: Diversion,
    /// The memory.
    memory: MemoryId,
    /// The peripheral that takes its cycles while diverted.
    peripheral: PeripheralId,
}

impl MappedPeripheral for Switch {
    fn read(&mut self, _: BusCycle) -> Result<u32, BusError> {
        Ok(0)
    }

    fn write(&mut self, _: BusCycle, value: u32) -> Result<(), BusError> {
        if value == 0 {
            self.diversion.release(self.memory);
        } else {
            self.diversion.divert(self.memory, self.peripheral);
        }
        Ok(())
    }
}

impl Stateless for Switch {}

impl Timed for Switch {}

#[test]
fn a_peripheral_diverts_a_memory_off_the_fast_path_within_a_quantum() {
    let mut program: Vec<u16> = Vec::new();
    // CS1 at $200000 and CS2 at $300000, 2 Kbytes each, fast
    program.extend(store_word(SIM + 0x50, 0x2000));
    program.extend(store_word(SIM + 0x52, 0x7bb0));
    program.extend(store_word(SIM + 0x54, 0x3000));
    program.extend(store_word(SIM + 0x56, 0x7bb0));
    program.extend(load_word(0x20_0000, 0));
    program.extend(store_word(0x30_0000, 1));
    program.extend(load_word(0x20_0000, 1));
    program.extend(store_word(0x30_0000, 0));
    program.extend(load_word(0x20_0000, 2));
    let mut builder = Builder::default();
    builder
        .memory(image(&program, ROM_BYTES), false, &[ChipSelect::Boot])
        .unwrap();
    let memory = builder
        .memory([0x11; 2048].to_vec(), false, &[cs(1)])
        .unwrap();
    let peripheral = builder.unwired_peripheral(Recorder::answering(0xbeef));
    let diversion = builder.diversion();
    builder
        .peripheral(
            Switch {
                diversion,
                memory,
                peripheral,
            },
            &[cs(2)],
        )
        .unwrap();
    let mut chip = builder.build();
    chip.run_for(Cycles(2000));
    let data = chip.cpu().registers.data;
    assert_eq!(
        [data[0] & 0xffff, data[1] & 0xffff, data[2] & 0xffff],
        [0x1111, 0xbeef, 0x1111]
    );
    let read = chip.peripheral::<Recorder>(peripheral).unwrap().reads[0];
    assert_eq!((read.select, read.address), (on(1), 0x20_0000));
    // from the host, between runs
    chip.divert_memory(memory, Some(peripheral));
    assert_eq!(
        cycle(&mut chip, 0x20_0000, Size::Word, None),
        Ok((0xbeef, 2))
    );
    let snapshot = chip.save().unwrap();
    chip.divert_memory(memory, None);
    assert_eq!(
        cycle(&mut chip, 0x20_0000, Size::Word, None),
        Ok((0x1111, 2))
    );
    chip.restore(&snapshot).unwrap();
    assert_eq!(
        cycle(&mut chip, 0x20_0000, Size::Word, None),
        Ok((0xbeef, 2))
    );
}

#[test]
fn a_diversion_to_no_peripheral_is_refused() {
    let mut builder = Builder::default();
    builder
        .memory(image(&[NOP], ROM_BYTES), false, &[ChipSelect::Boot])
        .unwrap();
    let memory = builder
        .memory([0x11; 2048].to_vec(), false, &[cs(1)])
        .unwrap();
    let peripheral = builder.unwired_peripheral(Recorder::answering(0xbeef));
    let mut chip = builder.build();
    // CS1 at $200000, 2 Kbytes, fast
    write_register(&mut chip, SIM + 0x50, 0x2000);
    write_register(&mut chip, SIM + 0x52, 0x7bb0);
    chip.divert_memory(memory, Some(peripheral));
    let snapshot = chip.save().unwrap();
    let to_none = state::with_diverted(&snapshot, |diverted| diverted[0].1 = 999);
    assert!(matches!(
        chip.restore(&to_none),
        Err(StateError::Invalid(reason)) if reason == "a diversion to no peripheral"
    ));
    let twice = state::with_diverted(&snapshot, |diverted| diverted.push(diverted[0]));
    assert!(matches!(
        chip.restore(&twice),
        Err(StateError::Invalid(reason)) if reason == "a memory diverted twice"
    ));
    assert_eq!(chip.save().unwrap(), snapshot, "unchanged by the refusals");
    assert_eq!(
        cycle(&mut chip, 0x20_0000, Size::Word, None),
        Ok((0xbeef, 2))
    );
}

#[test]
fn a_refused_snapshot_leaves_the_chip_as_it_was() {
    let (mut chip, _) = booting(&[NOP; 64]);
    let snapshot = chip.save().unwrap();
    // QILR and QIVR, so the QSM differs from the snapshot's, and the
    // core moves on
    write_register(&mut chip, QSM + 0x04, 0x1240);
    chip.run_for(Cycles(100));
    let before = chip.save().unwrap();
    // the QSM's snapshot taken, then the core's refused
    let bad_core = state::with_cpu(&snapshot, Vec::new());
    assert!(matches!(
        chip.restore(&bad_core),
        Err(StateError::Truncated)
    ));
    assert_eq!(chip.save().unwrap(), before);
    // the QSM's taken, then the GPT's refused
    let bad_gpt = state::with_module(&snapshot, 1, Vec::new());
    assert!(matches!(chip.restore(&bad_gpt), Err(StateError::Truncated)));
    assert_eq!(chip.save().unwrap(), before);
}

#[test]
fn a_tight_bus_monitor_faults_the_boot_rom() {
    let (mut chip, _) = booting(&[NOP]);
    // BME, BMT 8 clocks: CSBOOT's 16-clock cycles time out
    write_register(&mut chip, SIM + 0x20, 0x0007);
    assert_eq!(cycle(&mut chip, 0x0, Size::Word, None), Err(BusError));
    let run = chip.run_for(Cycles(1));
    assert!(matches!(run.stop, Stop::Halted(_)), "the fetch faulted");
}

#[test]
fn the_periodic_timer_interrupts_on_time() {
    let (mut chip, _) = booting(&[NOP]);
    // level 6, vector $50, PITM 1: 4 EXTAL periods of 256 clocks
    write_register(&mut chip, SIM + 0x22, 0x0650);
    write_register(&mut chip, SIM + 0x24, 0x0001);
    let period = Cycles(4 * 256);
    assert_eq!(chip.board.next_event(), Some(period));
    chip.run_for(Cycles(period.0 - 32));
    assert_eq!(chip.board.level(), InterruptLevel::NONE);
    let run = chip.run_for(Cycles(32));
    assert!(chip.now() >= period);
    assert_eq!(run.stop, Stop::Budget);
    let six = InterruptLevel::new(6).unwrap();
    assert_eq!(chip.board.level(), six);
    assert_eq!(
        chip.board.acknowledge(six, chip.now()).answer,
        Acknowledge::Vector(0x50)
    );
    assert_eq!(chip.board.level(), InterruptLevel::NONE);
}

#[test]
fn the_format_matches_its_golden() {
    let mut builder = Builder::default();
    builder
        .memory(image(&[NOP], ROM_BYTES), false, &[ChipSelect::Boot])
        .unwrap();
    builder.unwired_peripheral(Fixed);
    builder.qspi(Selected::default());
    let mut chip = builder.build();
    write_register(&mut chip, SIM + 0x22, 0x0650);
    write_register(&mut chip, SIM + 0x24, 0x0001);
    chip.run_for(Cycles(4 * 256 + 32));
    let module = |index: usize| version_of(&chip.board.modules[index].save().unwrap());
    snapshot_golden::check(
        env!("CARGO_MANIFEST_DIR"),
        "chip",
        &[
            ("chip", state::VERSION),
            ("cpu", cpu::state::VERSION),
            ("qsm", module(board::QSM)),
            ("gpt", module(board::GPT)),
            ("stateless", cpu::state::STATELESS_VERSION),
        ],
        &chip.save().unwrap(),
    );
}

#[test]
fn a_run_syncs_at_the_next_event() {
    let mut builder = Builder::default();
    builder
        .memory(image(&[NOP], ROM_BYTES), false, &[ChipSelect::Boot])
        .unwrap();
    let advanced = Rc::new(Cell::new(None));
    let at = Cycles(1000);
    builder.unwired_peripheral(Alarm {
        at,
        advanced: Rc::clone(&advanced),
    });
    let mut chip = builder.build();
    chip.run_for(Cycles(5000));
    // a quantum ends at the alarm, within one 16-clock NOP
    let seen = advanced.get().unwrap();
    assert!(seen >= at && seen < at + Cycles(16), "{seen:?}");
}

#[test]
fn the_watchdog_resets_the_chip_unless_serviced() {
    let (mut chip, _) = booting(&[NOP]);
    // 2^9 EXTAL periods at 256 clocks
    let timeout = Cycles(512 * 256);
    chip.run_for(Cycles(timeout.0 - 100));
    write_register(&mut chip, SIM + 0x26, 0x0055);
    write_register(&mut chip, SIM + 0x26, 0x00aa);
    chip.take_events();
    // the time-out restarts at the EXTAL period the service fell in
    chip.run_for(Cycles(timeout.0 - 512));
    assert!(chip.take_events().is_empty());
    chip.run_for(Cycles(timeout.0));
    let events = chip.take_events();
    assert!(
        matches!(events[..], [Event::Reset(_, ResetCause::Watchdog)]),
        "{events:?}"
    );
    assert_eq!(chip.sim().reset_status(), 0x20);
    assert!(chip.cpu().registers.pc >= START);
}

#[test]
fn a_fault_on_the_reset_vectors_halts() {
    // a boot peripheral that asserts BERR (CPU32RM 6.2.1)
    let mut builder = Builder::default();
    builder.peripheral(Faulting, &[ChipSelect::Boot]).unwrap();
    let mut chip = builder.build();
    assert!(chip.halted().is_some());
    // time passes while it is halted
    let run = chip.run_for(Cycles(100));
    assert!(matches!(run.stop, Stop::Halted(_)));
    assert_eq!(run.cycles, Cycles(100));
}

#[test]
fn the_watchdog_resets_a_halted_chip() {
    // ILLEGAL with no stack: the exception's stacking faults twice, a
    // double bus fault (CPU32RM 6.2.1); the halt monitor off (SYPCR
    // HME clear) leaves it halted, SWE set out of reset (SIMRM Table
    // 8-4) times it out after 2^9 EXTAL periods (MC68331UM 4.2.10)
    let mut program: Vec<u16> = store_byte(SIM + 0x21, 0x80).to_vec();
    program.push(ILLEGAL);
    let (mut chip, _) = booting(&program);
    let run = chip.run_for(Cycles(10_000));
    assert_eq!(run.stop, Stop::Halted(Halt::DoubleBusFault));
    let halted = chip.now();
    assert!(halted < Cycles(10_000), "the run ends at the halt");
    chip.take_events();
    let timeout = Cycles(512 * 256);
    let run = chip.run_for(timeout);
    let events = chip.take_events();
    assert!(
        matches!(events[..], [Event::Reset(at, ResetCause::Watchdog), ..] if at <= timeout),
        "{events:?}"
    );
    assert_eq!(chip.halted(), Some(Halt::DoubleBusFault), "and halts again");
    assert_eq!(run.stop, Stop::Halted(Halt::DoubleBusFault));
}

#[test]
fn a_halt_monitor_reset_that_slows_the_clock_ends_the_run() {
    // SYNCR $CF00 (16.78 MHz); SYPCR $08, HME set, the watchdog off
    // (MC68331UM D.3.12); then ILLEGAL with no stack, a double bus
    // fault the halt monitor resets (MC68331UM 4.6.1), which puts
    // SYNCR back to its reset 8.39 MHz
    let mut program: Vec<u16> = store_word(SIM + 0x04, 0xcf00).to_vec();
    program.extend(store_byte(SIM + 0x21, 0x08));
    program.push(ILLEGAL);
    let (mut chip, _) = booting(&program);
    chip.set_clock_stop(true);
    // the SYNCR write ends the first run
    assert_eq!(chip.run_for(Cycles(100_000)).stop, Stop::Budget);
    assert_eq!(chip.sim().clock().frequency().hz(), 16_777_216);
    chip.take_events();
    let run = chip.run_for(Cycles(100_000));
    let events = chip.take_events();
    assert!(
        events
            .iter()
            .any(|event| matches!(event, Event::Reset(_, ResetCause::HaltMonitor))),
        "{events:?}"
    );
    assert_eq!(chip.sim().clock().frequency().hz(), 8_388_608);
    assert!(
        run.cycles < Cycles(1_000),
        "ends at the reset: {:?}",
        run.cycles
    );
}

#[test]
fn iarb_arbitrates_the_sim_qsm_and_gpt_at_one_level() {
    let (mut chip, _) = booting(&[NOP]);
    let five = InterruptLevel::new(5).unwrap();
    // the PIT at level 5, vector $42, on the SIM's IARB $F
    write_register(&mut chip, SIM + 0x22, 0x0542);
    write_register(&mut chip, SIM + 0x24, 0x0001);
    // the QSM's SCI at level 5, vectors $60 and $61, IARB $F: TDRE
    // requests once TIE and TE are set
    write_register(&mut chip, QSM, 0x008f);
    write_register(&mut chip, QSM + 0x04, 0x0560);
    write_register(&mut chip, QSM + 0x08, 0x0001);
    write_register(&mut chip, QSM + 0x0a, 0x0088);
    // the GPT's OC1 at level 5, vectors from $70, IARB $F
    write_register(&mut chip, GPT, 0x008f);
    write_register(&mut chip, GPT + 0x04, 0x0570);
    write_register(&mut chip, GPT + 0x14, 0x0010);
    write_register(&mut chip, GPT + 0x20, 0x0800);
    chip.run_for(Cycles(2000));
    chip.take_events();
    let tie = Event::ArbitrationTie {
        level: five,
        arbitration: 0xf,
    };
    // a tie: the SIM, then the QSM, then the GPT (PLAN.md)
    assert_eq!(
        chip.board.acknowledge(five, chip.now()).answer,
        Acknowledge::Vector(0x42)
    );
    assert_eq!(chip.take_events(), [tie]);
    assert_eq!(
        chip.board.acknowledge(five, chip.now()).answer,
        Acknowledge::Vector(0x60)
    );
    assert_eq!(chip.take_events(), [tie]);
    // a higher IARB wins: the QSM 3, the GPT 4
    write_register(&mut chip, QSM, 0x0083);
    write_register(&mut chip, GPT, 0x0084);
    assert_eq!(
        chip.board.acknowledge(five, chip.now()).answer,
        Acknowledge::Vector(0x74)
    );
    assert!(chip.take_events().is_empty());
    // IARB 0 does not contend; with no contender, a spurious interrupt
    // (SIMRM 6.3)
    write_register(&mut chip, GPT, 0x0080);
    assert_eq!(
        chip.board.acknowledge(five, chip.now()).answer,
        Acknowledge::Vector(0x60)
    );
    write_register(&mut chip, QSM, 0x0080);
    assert_eq!(chip.board.level(), five);
    assert_eq!(
        chip.board.acknowledge(five, chip.now()).answer,
        Acknowledge::Spurious
    );
    // the GPT's register block wins over the external bus
    assert_eq!(
        cycle(&mut chip, GPT + 0x04, Size::Word, None),
        Ok((0x0570, 2))
    );
}

#[test]
fn the_reset_instruction_resets_the_peripherals_not_the_sim() {
    let mut board = wired(&[]);
    let chip = &mut board.chip;
    chip.board.reset_instruction(Cycles(7));
    assert_eq!(chip.sim().reset_status(), 0x02);
    assert_eq!(chip.sim().chip_selects().base(cs(0)), 0x1004);
    assert_eq!(chip.peripheral::<Recorder>(board.duart).unwrap().resets, 2);
    assert_eq!(
        chip.take_events().last(),
        Some(&Event::Reset(Cycles(7), ResetCause::System))
    );
}

#[test]
fn a_faster_clock_keeps_extal_time() {
    let (mut chip, _) = booting(&[NOP]);
    // SYNCR $CF00: 512 clocks an EXTAL period
    write_register(&mut chip, SIM + 0x04, 0xcf00);
    write_register(&mut chip, SIM + 0x22, 0x0650);
    write_register(&mut chip, SIM + 0x24, 0x0008);
    assert_eq!(chip.sim().clock().frequency().hz(), 16_777_216);
    assert_eq!(chip.board.next_event(), Some(Cycles(8 * 4 * 512)));
}

#[test]
fn a_clock_change_can_end_the_run() {
    let program = store_word(SIM + 0x04, 0xcf00);
    let budget = Cycles(100_000);
    let (mut chip, _) = booting(&program);
    let run = chip.run_for(budget);
    assert!(
        run.stop == Stop::Budget && run.cycles >= budget,
        "off by default"
    );
    let (mut chip, _) = booting(&program);
    chip.set_clock_stop(true);
    let run = chip.run_for(budget);
    assert_eq!(run.stop, Stop::Budget);
    assert!(
        run.cycles < Cycles(1_000),
        "after the write: {:?}",
        run.cycles
    );
    assert_eq!(chip.sim().clock().frequency().hz(), 16_777_216);
    assert!(
        chip.run_for(budget).cycles >= budget,
        "and on, with no change"
    );
}

#[test]
fn a_watched_output_change_ends_the_run() {
    // PFPAR (MC68331UM D.3.11) 0: port F is I/O; DDRF bit 0 an output;
    // then PORTF0 bit 0 low
    let program = [
        store_byte(SIM + 0x1f, 0),
        store_byte(SIM + 0x1d, 1),
        store_byte(SIM + 0x19, 0),
    ]
    .concat();
    let budget = Cycles(100_000);
    let watching = |port_f: u8| {
        let (mut chip, _) = booting(&program);
        chip.set_output_stop(OutputStop {
            port_f,
            ..OutputStop::default()
        });
        chip
    };
    let mut chip = watching(0);
    assert!(chip.run_for(budget).cycles >= budget, "off by default");
    let mut chip = watching(1 << 1);
    assert!(chip.run_for(budget).cycles >= budget, "another pin");
    let mut chip = watching(1);
    // PF0 driven low ends the run after the instruction
    let run = chip.run_for(budget);
    assert_eq!(run.stop, Stop::Budget);
    assert!(
        run.cycles < Cycles(1_000),
        "after the write: {:?}",
        run.cycles
    );
    assert_eq!(
        chip.port_f_outputs(),
        PortLevels {
            driven: 1,
            levels: 0
        }
    );
    assert!(
        chip.run_for(budget).cycles >= budget,
        "and on, with no change"
    );
}

#[test]
fn the_reset_instruction_ends_a_run_watching_reset_from_fast_memory() {
    // the boot ROM's pages are the core's: no cycle reaches the chip
    let (mut chip, _) = booting(&[RESET]);
    chip.set_output_stop(OutputStop {
        reset: true,
        ..OutputStop::default()
    });
    let run = chip.run_for(Cycles(100_000));
    assert_eq!(run.stop, Stop::Budget);
    // RESET asserts it for 512 clocks (CPU32RM 4, RESET)
    assert!(run.cycles < Cycles(1_000), "after it: {:?}", run.cycles);
    assert_eq!(
        chip.take_events().last(),
        Some(&Event::Reset(Cycles::ZERO, ResetCause::System))
    );
}

/// A QSPI peripheral that answers each word with the PCS levels it
/// saw.  Its records, and the other QSPI peripherals' here, are the
/// test's, not state.
#[derive(Debug, Default)]
struct Selected(Vec<u8>);

impl Stateless for Selected {}

impl SpiPeripheral for Selected {
    fn transfer(&mut self, transfer: QspiTransfer) -> u16 {
        self.0.push(transfer.chip_selects);
        transfer.chip_selects.into()
    }
}

/// A QSPI peripheral that keeps what it is told: each transfer's entry
/// and levels, each change of the levels, and its resets.
#[derive(Debug, Default)]
struct Framed {
    /// Each transfer's entry and PCS levels.
    transfers: Vec<(u8, u8)>,
    /// Each change of the PCS levels.
    levels: Vec<u8>,
    /// Its resets.
    resets: usize,
}

impl Stateless for Framed {}

impl SpiPeripheral for Framed {
    fn transfer(&mut self, transfer: QspiTransfer) -> u16 {
        self.transfers.push((transfer.entry, transfer.chip_selects));
        0
    }

    fn chip_selects(&mut self, levels: u8, _: Cycles) {
        self.levels.push(levels);
    }

    fn reset(&mut self, _: Cycles) {
        self.resets += 1;
    }
}

/// A chip whose firmware runs a two-entry queue, PCS 0101 then 1010,
/// neither with CONT, its QSPI peripheral seeing `scope`'s resets.
fn two_entries(scope: ResetScope) -> Mc68331 {
    let mut program: Vec<u16> = Vec::new();
    program.extend(store_byte(QSM + 0x15, 0x78));
    program.extend(store_word(QSM + 0x16, 0x7878));
    program.extend(store_byte(QSM + 0x140, 0x05));
    program.extend(store_byte(QSM + 0x141, 0x0a));
    // ENDQP 1 (QSMRM 4.3.3)
    program.extend(store_word(QSM + 0x1c, 0x0100));
    program.extend(store_word(QSM + 0x18, 0x8002));
    program.extend(store_word(QSM + 0x1a, 0x8000));
    let mut builder = Builder::default();
    builder
        .memory(image(&program, ROM_BYTES), false, &[ChipSelect::Boot])
        .unwrap();
    builder.qspi(Framed::default());
    builder.qspi_reset_scope(scope);
    let mut chip = builder.build();
    chip.run_for(Cycles(4000));
    chip
}

#[test]
fn the_qspi_peripheral_sees_entries_every_level_and_its_resets() {
    let mut chip = two_entries(ResetScope::ALL);
    let framed = chip.qspi_peripheral::<Framed>().unwrap();
    assert_eq!(framed.transfers, [(0, 0b0101), (1, 0b1010)]);
    // PORTQS high between entries: each entry's frame starts and ends
    assert_eq!(framed.levels, [0b0101, 0b1111, 0b1010, 0b1111]);
    assert_eq!(framed.resets, 1, "power-on");
    chip.reset();
    assert_eq!(chip.qspi_peripheral::<Framed>().unwrap().resets, 2);
    let mut chip = two_entries(ResetScope::ALL.without(ResetCause::External));
    chip.reset();
    assert_eq!(chip.qspi_peripheral::<Framed>().unwrap().resets, 1);
}

#[test]
fn the_firmware_talks_on_the_sci_and_the_qspi() {
    let mut program: Vec<u16> = Vec::new();
    // SCBR 1: 32 clocks a bit; TE and RE; SCSR read, then 'H' to SCDR
    program.extend(store_word(QSM + 0x08, 1));
    program.extend(store_word(QSM + 0x0a, 0x000c));
    program.extend(load_word(QSM + 0x0c, 0));
    program.extend(store_byte(QSM + 0x0f, b'H'));
    // PCS[3:0] outputs with PORTQS high, the QSPI's; one entry to
    // PCS 0101 at SPBR 2
    program.extend(store_byte(QSM + 0x15, 0x78));
    program.extend(store_word(QSM + 0x16, 0x7878));
    program.extend(store_byte(QSM + 0x140, 0x05));
    program.extend(store_word(QSM + 0x18, 0x8002));
    program.extend(store_word(QSM + 0x1a, 0x8000));
    let mut builder = Builder::default();
    builder
        .memory(image(&program, ROM_BYTES), false, &[ChipSelect::Boot])
        .unwrap();
    builder.qspi(Selected::default());
    let mut chip = builder.build();
    chip.run_for(Cycles(2000));
    let sent = chip.take_sci_transmitted();
    assert!(
        matches!(sent[..], [sent] if sent.frame == Frame::Data(0x48)),
        "{sent:?}"
    );
    assert_eq!(chip.qspi_peripheral::<Selected>().unwrap().0, [0b0101]);
    assert_eq!(
        cycle(&mut chip, QSM + 0x100, Size::Word, None),
        Ok((0b0101, 2))
    );
    // a frame from the host: RDRF a frame time later
    chip.sci_receive(Frame::Data(0x21));
    chip.run_for(Cycles(400));
    assert_eq!(
        cycle(&mut chip, QSM + 0x0c, Size::Word, None).unwrap().0 & 0x40,
        0x40
    );
    assert_eq!(
        cycle(&mut chip, QSM + 0x0e, Size::Word, None),
        Ok((0x21, 2))
    );
    assert_eq!(chip.qsm().arbitration(), 0);
}

/// A QSPI peripheral that records the serial clocks it is told and
/// each transfer's.
#[derive(Debug, Default)]
struct Clocked {
    /// The clocks told.
    told: Vec<QspiClock>,
    /// Each transfer's clock.
    transfers: Vec<QspiClock>,
}

impl Stateless for Clocked {}

impl SpiPeripheral for Clocked {
    fn transfer(&mut self, transfer: QspiTransfer) -> u16 {
        self.transfers.push(transfer.clock);
        0
    }

    fn clock(&mut self, clock: QspiClock, _: Cycles) {
        self.told.push(clock);
    }
}

#[test]
fn the_qspi_peripheral_sees_syncr_in_the_baud() {
    let mut builder = Builder::default();
    builder
        .memory(image(&[NOP], ROM_BYTES), false, &[ChipSelect::Boot])
        .unwrap();
    builder.qspi(Clocked::default());
    let mut chip = builder.build();
    // SYNCR $CF00: 16.78 MHz; SPBR 84: 100 kHz (QSMRM Table 4-3)
    write_register(&mut chip, SIM + 0x04, 0xcf00);
    write_register(&mut chip, QSM + 0x18, 0x8054);
    write_register(&mut chip, QSM + 0x1a, 0x8000);
    let clocks = &chip.qspi_peripheral::<Clocked>().unwrap().told;
    assert_eq!(clocks.len(), 1);
    assert_eq!(clocks[0].system_clock.hz(), 16_777_216);
    assert_eq!(clocks[0].baud().hz(), 99_864);
    // the entry's 8 bits at 168 clocks each, and the delays
    chip.run_for(Cycles(4_000));
    let clocked = chip.qspi_peripheral::<Clocked>().unwrap();
    assert_eq!(clocked.transfers, clocked.told, "the transfer carries it");
}

#[test]
fn the_host_masters_the_qspi_as_a_slave() {
    let (mut chip, _) = booting(&[NOP]);
    // MISO an output, MOSI and PCS0/SS inputs; BITS 8, slave; ENDQP 1
    write_register(&mut chip, QSM + 0x16, 0x0b01);
    write_register(&mut chip, QSM + 0x120, 0x00c3);
    write_register(&mut chip, QSM + 0x122, 0x0081);
    write_register(&mut chip, QSM + 0x18, 0x2000);
    write_register(&mut chip, QSM + 0x1c, 0x0100);
    write_register(&mut chip, QSM + 0x1a, 0x8000);
    chip.set_qspi_slave_select(true);
    let start = chip.now();
    let shifted = chip.qspi_slave_shift(0x5a, 8, Cycles(10));
    assert_eq!((shifted.miso, shifted.driven), (0xc3, 0xff));
    // a period a bit, the last taken at the end of an instruction
    let end = start + Cycles(80);
    assert!(
        (end..end + Cycles(32)).contains(&chip.now()),
        "{:?}",
        chip.now()
    );
    assert_eq!(
        cycle(&mut chip, QSM + 0x100, Size::Word, None),
        Ok((0x5a, 2))
    );
    let shifted = chip.qspi_slave_shift(0xa5, 8, Cycles(10));
    assert_eq!(shifted.miso, 0x81);
    assert_eq!(
        cycle(&mut chip, QSM + 0x1e, Size::Word, None),
        Ok((0x81, 2)),
        "SPIF, CPTQP 1"
    );
    chip.set_qspi_slave_select(false);
    assert_eq!(chip.qspi_slave_shift(0xff, 8, Cycles(10)).driven, 0);
}

/// Where the interrupt tests' handlers are.
const HANDLER: u32 = 0x2000;
/// The wired board's RAM, CS0's block (`WIRED_SELECTS`).
const RAM: u32 = 0x10_0000;

/// The vector table entry for `vector` pointing at `HANDLER`.
fn vector_to_handler(vector: u8) -> (u32, [u16; 2]) {
    (
        u32::from(vector) * 4,
        [(HANDLER >> 16) as u16, HANDLER as u16],
    )
}

/// Runs `program` on the wired board with the interrupt mask at 0 until
/// `vector`'s handler, which writes $BEEF to RAM, has run; asserts the
/// format 0 frame it was taken with (CPU32RM 6.2.11, 6.4).
fn takes_vector(program: &[u16], vector: u8, level: u8) -> Mc68331 {
    let (entry, address) = vector_to_handler(vector);
    let handler = store_word(RAM, 0xbeef);
    let mut board = wired_patched(program, &[(entry, &address), (HANDLER, &handler)]);
    let chip = &mut board.chip;
    chip.registers_mut().set_sr(0x2000);
    chip.run_for(Cycles(20_000));
    assert_eq!(chip.memory(board.ram)[..2], [0xbe, 0xef], "the handler ran");
    assert_eq!(chip.cpu().registers.interrupt_mask(), level);
    let frame = (chip.cpu().registers.ssp - RAM) as usize;
    let format_vector = &chip.memory(board.ram)[frame + 6..frame + 8];
    assert_eq!(
        u16::from_be_bytes([format_vector[0], format_vector[1]]),
        u16::from(vector) * 4
    );
    board.chip
}

#[test]
fn an_sci_interrupt_reaches_its_handler() {
    let mut program: Vec<u16> = Vec::new();
    // IARB 5; the SCI at level 3, vectors $40 and $41; TIE and TE: TDRE
    // requests at once
    program.extend(store_word(QSM, 0x0085));
    program.extend(store_word(QSM + 0x04, 0x0340));
    program.extend(store_word(QSM + 0x08, 1));
    program.extend(store_word(QSM + 0x0a, 0x0088));
    takes_vector(&program, 0x40, 3);
}

#[test]
fn a_gpt_interrupt_reaches_its_handler() {
    let mut program: Vec<u16> = Vec::new();
    // IARB 6; level 4, vectors from $60; OC1 at TCNT $1000 with OC1I
    program.extend(store_word(GPT, 0x0006));
    program.extend(store_word(GPT + 0x04, 0x0460));
    program.extend(store_word(GPT + 0x14, 0x1000));
    program.extend(store_word(GPT + 0x20, 0x0800));
    let chip = takes_vector(&program, 0x64, 4);
    // taken at the compare: TCNT at the system clock / 4
    assert!(chip.now() >= Cycles(4 * 0x1000));
}

/// LPSTOP #$2000 (CPU32RM 4, LPSTOP): low-power stop at mask 0.
const LPSTOP_MASK_0: [u16; 3] = [0xf800, 0x01c0, 0x2000];

/// TCNT now, by a peek.
fn tcnt(chip: &Mc68331) -> u32 {
    chip.peek(
        Address(GPT + 0x0a),
        Size::Word,
        FunctionCode::SUPERVISOR_DATA,
    )
    .unwrap()
}

#[test]
fn in_lpstop_only_the_pit_wakes_and_the_gpt_and_qsm_stop() {
    let mut program: Vec<u16> = Vec::new();
    // the watchdog off; the SCI at level 3, vector $40, TIE and TE:
    // TDRE requests at once; the GPT at IARB 6; the PIT at level 6,
    // vector $50, PITR 64: 64 x 4 EXTAL periods (MC68331UM 4.2.11)
    program.extend(store_word(SIM + 0x20, 0));
    program.extend(store_word(QSM, 0x0085));
    program.extend(store_word(QSM + 0x04, 0x0340));
    program.extend(store_word(QSM + 0x08, 1));
    program.extend(store_word(QSM + 0x0a, 0x0088));
    program.extend(store_word(GPT, 0x0006));
    program.extend(store_word(SIM + 0x22, 0x0650));
    program.extend(store_word(SIM + 0x24, 0x0040));
    program.extend(LPSTOP_MASK_0);
    let (entry, address) = vector_to_handler(0x50);
    let handler = store_word(RAM, 0xbeef);
    let mut board = wired_patched(&program, &[(entry, &address), (HANDLER, &handler)]);
    let chip = &mut board.chip;
    chip.set_tracing(ChipTracing {
        exceptions: true,
        ..ChipTracing::default()
    });
    while chip.sim().low_power().is_none() {
        chip.run_for(Cycles(16));
        assert!(chip.now() < Cycles(20_000), "never stopped");
        assert_eq!(chip.halted(), None);
    }
    let (stopped, counted) = (chip.now(), tcnt(chip));
    // the SCI's request at level 3 waits; the PIT's at 6 wakes it
    // (MC68331UM 4.2.12)
    let taken = loop {
        chip.run_for(Cycles(16));
        if let Some(taken) = chip.take_events().iter().find_map(|event| match event {
            Event::Exception(now, entry) => Some((*now, entry.vector, entry.level)),
            _ => None,
        }) {
            break taken;
        }
        assert!(chip.now() < stopped + Cycles(200_000), "never woke");
        assert_eq!(chip.halted(), None);
    };
    let (woke, vector, level) = taken;
    assert_eq!((vector, level), (Vector(0x50), InterruptLevel::new(6)));
    assert!(woke > stopped + Cycles(60_000), "{woke:?}");
    assert_eq!(chip.sim().low_power(), None);
    // TCNT, at the system clock / 4, stood still from the stop to the
    // wake (MC68331UM 4.3.4)
    let since = chip.now().0 - woke.0;
    assert!(
        u64::from(tcnt(chip) - counted) <= since / 4 + 1,
        "TCNT {} then {}",
        counted,
        tcnt(chip)
    );
}

/// When the first interrupt of `program`, at mask 0, is taken, the chip
/// run `budget` clocks at a time.
fn first_interrupt(program: &[u16], vector: u8, budget: Cycles) -> Cycles {
    let (entry, address) = vector_to_handler(vector);
    let handler = store_word(RAM, 0xbeef);
    let mut board = wired_patched(program, &[(entry, &address), (HANDLER, &handler)]);
    let chip = &mut board.chip;
    chip.registers_mut().set_sr(0x2000);
    chip.set_tracing(ChipTracing {
        exceptions: true,
        ..ChipTracing::default()
    });
    let end = chip.now() + Cycles(20_000);
    while chip.now() < end {
        chip.run_for(budget);
        if let Some(now) = chip.take_events().iter().find_map(|event| match event {
            Event::Exception(now, _) => Some(*now),
            _ => None,
        }) {
            return now;
        }
    }
    panic!("no interrupt");
}

#[test]
fn an_event_a_write_makes_ends_the_quantum() {
    let mut program: Vec<u16> = Vec::new();
    // the watchdog off; NOPs, past the setup's run; IARB 5; the QSPI
    // at level 5, vector $41; one 8-bit entry at SPBR 2 with SPIFIE,
    // SPE written last
    program.extend(store_word(SIM + 0x20, 0));
    program.extend([NOP; 256]);
    program.extend(store_word(QSM, 0x0085));
    program.extend(store_word(QSM + 0x04, 0x2840));
    program.extend(store_word(QSM + 0x18, 0x8002));
    program.extend(store_word(QSM + 0x1c, 0x8000));
    program.extend(store_word(QSM + 0x1a, 0x8000));
    let one_long_run = first_interrupt(&program, 0x41, Cycles(20_000));
    let instruction_by_instruction = first_interrupt(&program, 0x41, Cycles(1));
    assert_eq!(one_long_run, instruction_by_instruction);
}

/// A board unlike the wired one: a 16-MHz external clock (MODCLK low),
/// the SCI at SCBR 52 (9615 baud), TCNT at the system clock / 16, and
/// a 1 PPS on IC2.
#[test]
fn an_external_clock_board_uses_the_sci_and_the_gpt() {
    let configuration = Configuration {
        clock: ClockSource::external(Frequency::from_hz(16_000_000)).unwrap(),
        ..Configuration::default()
    };
    let mut builder = Builder::new(configuration);
    builder
        .memory(image(&[NOP], ROM_BYTES), false, &[ChipSelect::Boot])
        .unwrap();
    let mut chip = builder.build();
    assert_eq!(chip.sim().clock().frequency().hz(), 16_000_000);
    write_register(&mut chip, QSM + 0x08, 52);
    write_register(&mut chip, QSM + 0x0a, 0x0004);
    chip.sci_receive(Frame::Data(0x55));
    let frame = Cycles(10 * 32 * 52);
    let start = chip.now();
    // a run syncs at the frame's end
    assert_eq!(chip.board.next_event(), Some(start + frame));
    chip.run_for(frame);
    assert_eq!(
        cycle(&mut chip, QSM + 0x0e, Size::Word, None),
        Ok((0x55, 2))
    );
    // CPR 2: / 16; EDGE2 rising
    write_register(&mut chip, GPT + 0x20, 0x0002);
    write_register(&mut chip, GPT + 0x1e, 0x0004);
    let tcnt = |chip: &mut Mc68331| cycle(chip, GPT + 0x0a, Size::Word, None).unwrap().0;
    let before = tcnt(&mut chip);
    chip.run_for(Cycles(16 * 100));
    chip.set_port_gp_pins(0xfd);
    chip.set_port_gp_pins(0xff);
    let captured = cycle(&mut chip, GPT + 0x10, Size::Word, None).unwrap().0;
    assert!(
        captured >= before + 100 && captured <= before + 102,
        "{captured} {before}"
    );
    assert_eq!(
        cycle(&mut chip, GPT + 0x22, Size::Word, None).unwrap().0,
        0x0200
    );
    assert_eq!(
        chip.port_gp_outputs(),
        PortLevels {
            driven: 0,
            levels: 0
        }
    );
    assert_eq!(chip.gpt().arbitration(), 0);
}

/// MOVE #imm,SR (CPU32RM 4, MOVE to SR).
const MOVE_TO_STATUS_REGISTER: u16 = 0x46fc;
/// RESET (CPU32RM 4, RESET).
const RESET: u16 = 0x4e70;
/// RTE (CPU32RM 4, RTE).
const RTE: u16 = 0x4e73;
/// BRA.S to itself (CPU32RM 4, Bcc): the end of a test program.
const BRANCH_TO_SELF: u16 = 0x60fe;
/// The spurious interrupt vector (CPU32RM 6.2.11).
const SPURIOUS_VECTOR: u8 = 24;

/// The 68681 DUART registers the fake models, by number: IMR
/// (written) and IVR.
const DUART_IMR: u32 = 5;
/// IVR.
const DUART_IVR: u32 = 12;
/// IVR out of reset, the 68681's "uninitialized" vector.
const DUART_IVR_RESET: u8 = 0x0f;

/// A fake 68681 DUART on the lower byte of a 16-bit port, its
/// registers on odd addresses: its interrupt output asserted while IMR
/// is nonzero, and IVR its answer to an interrupt acknowledge, which
/// it ends after `wait` wait states (its registers after none).
#[derive(Debug)]
struct Duart {
    /// IMR.
    mask: u8,
    /// IVR.
    vector: u8,
    /// Its wait states before DSACK at an acknowledge.
    wait: u8,
    /// The interrupt acknowledge cycles it answered.
    acknowledges: Vec<BusCycle>,
}

impl Duart {
    fn new(wait: u8) -> Self {
        Self {
            mask: 0,
            vector: DUART_IVR_RESET,
            wait,
            acknowledges: Vec::new(),
        }
    }
}

impl MappedPeripheral for Duart {
    fn read(&mut self, cycle: BusCycle) -> Result<u32, BusError> {
        if cycle.function_code.is_cpu_space() {
            self.acknowledges.push(cycle);
            return Ok(self.vector.into());
        }
        Ok(0)
    }

    fn write(&mut self, cycle: BusCycle, value: u32) -> Result<(), BusError> {
        match cycle.address >> 1 & 0xf {
            DUART_IMR => self.mask = value as u8,
            DUART_IVR => self.vector = value as u8,
            _ => {}
        }
        Ok(())
    }

    fn wait_states(&mut self, cycle: BusCycle) -> Option<u8> {
        Some(if cycle.function_code.is_cpu_space() {
            self.wait
        } else {
            0
        })
    }

    fn interrupt_request(&self) -> bool {
        self.mask != 0
    }

    fn reset(&mut self, _: Cycles) {
        *self = Self::new(self.wait);
    }
}

mc683xx_cpu32::unsavable!(Duart, "a DUART stub");

impl Timed for Duart {}

/// The DUART's registers, CS8's block.
const DUART: u32 = 0x0e_0000;
/// The vector the firmware gives the DUART.
const DUART_VECTOR: u8 = 0x45;

/// Copies `words` into `rom` from `address`.
fn patch(rom: &mut [u8], address: u32, words: &[u16]) {
    for (index, word) in words.iter().enumerate() {
        let at = address as usize + 2 * index;
        rom[at..at + 2].copy_from_slice(&word.to_be_bytes());
    }
}

/// A board with RAM on CS0, the DUART's registers on CS8 and its
/// interrupt acknowledge on CS10 (CPU space, IPL 2, the lower byte,
/// external DSACK; SIMRM 7.8.1), its output on IRQ2.  The firmware
/// programs the selects, writes SYPCR if `protection`, sets IVR and
/// IMR, lowers the mask and waits; vector `DUART_VECTOR`'s handler and
/// the spurious one write $BEEF to RAM.
fn duart_board(wait: u8, protection: Option<u16>) -> (Mc68331, MemoryId, PeripheralId) {
    let selects = [
        // CS0: $100000, 64 Kbytes; both bytes, read and write, no wait
        // states, supervisor
        (0x4c, 0x1003),
        (0x4e, 0x7820),
        // CS8: `DUART`, 2 Kbytes; the lower byte, read and write,
        // external DSACK, supervisor
        (0x6c, 0x0e00),
        (0x6e, 0x3be0),
        // CS10: $FFF800 in CPU space, interrupt acknowledges at level
        // 2; the lower byte, read, external DSACK
        (0x74, 0xfff8),
        (0x76, 0x2bc4),
    ];
    let mut program: Vec<u16> = selects
        .iter()
        .flat_map(|&(offset, value)| store_word(SIM + offset, value))
        .collect();
    if let Some(value) = protection {
        program.extend(store_word(SIM + 0x20, value));
    }
    program.extend(store_byte(DUART + (DUART_IVR << 1 | 1), DUART_VECTOR));
    program.extend(store_byte(DUART + (DUART_IMR << 1 | 1), 0x01));
    program.extend([MOVE_TO_STATUS_REGISTER, 0x2000, BRANCH_TO_SELF]);
    let mut rom = image(&program, ROM_BYTES);
    for vector in [DUART_VECTOR, SPURIOUS_VECTOR] {
        let (entry, address) = vector_to_handler(vector);
        patch(&mut rom, entry, &address);
    }
    let handler: Vec<u16> = store_word(RAM, 0xbeef)
        .into_iter()
        .chain([BRANCH_TO_SELF])
        .collect();
    patch(&mut rom, HANDLER, &handler);
    let mut builder = Builder::default();
    builder.memory(rom, false, &[ChipSelect::Boot]).unwrap();
    let ram = builder.memory(vec![0; 64 << 10], true, &[cs(0)]).unwrap();
    let duart = builder
        .peripheral(Duart::new(wait), &[cs(8), cs(10)])
        .unwrap();
    builder.interrupt(duart, IrqPin::new(2).unwrap());
    let mut chip = builder.build();
    chip.run_for(Cycles(5000));
    (chip, ram, duart)
}

/// The vector the format 0 frame on top of the stack records (CPU32RM
/// 6.4), and the interrupt mask.
fn taken_vector(chip: &Mc68331, ram: MemoryId) -> (u16, u8) {
    let frame = (chip.cpu().registers.ssp - RAM) as usize;
    let format_vector = &chip.memory(ram)[frame + 6..frame + 8];
    (
        u16::from_be_bytes([format_vector[0], format_vector[1]]) / 4,
        chip.cpu().registers.interrupt_mask(),
    )
}

#[test]
fn a_duart_vectors_its_interrupt_through_a_chip_select() {
    let (chip, ram, duart) = duart_board(4, None);
    assert_eq!(chip.memory(ram)[..2], [0xbe, 0xef], "the handler ran");
    assert_eq!(taken_vector(&chip, ram), (DUART_VECTOR.into(), 2));
    let acknowledges = &chip.peripheral::<Duart>(duart).unwrap().acknowledges;
    assert_eq!(acknowledges.len(), 1);
    let acknowledge = acknowledges[0];
    assert_eq!(acknowledge.select, on(10));
    // CPU space type $F, level 2 on ADDR[3:1] (MC68331UM 4.5.4)
    assert_eq!(acknowledge.address, 0xff_fff5);
    assert_eq!(acknowledge.size, Size::Byte);
    assert_eq!(acknowledge.function_code, FunctionCode::CPU_SPACE);
}

#[test]
fn a_duarts_wait_states_at_its_acknowledge_cost_time() {
    let two = InterruptLevel::new(2).unwrap();
    let stall = |wait| {
        // the handler leaves the mask at 2 and the DUART requesting
        let (mut chip, _, _) = duart_board(wait, None);
        let now = chip.now();
        let acknowledged = chip.board.acknowledge(two, now);
        assert_eq!(acknowledged.answer, Acknowledge::Vector(DUART_VECTOR));
        acknowledged.stall
    };
    // an external DSACK after no wait states: 3 clocks, one beyond the
    // tables' 2 (SIMRM 7.5.1)
    assert_eq!(stall(0), Cycles(1));
    assert_eq!(stall(4), Cycles(5));
}

#[test]
fn a_spurious_acknowledge_costs_the_bus_monitors_period() {
    let two = InterruptLevel::new(2).unwrap();
    // the DUART too slow to answer: BME with BMT 16 and 32 clocks
    // (SIMRM 3.2, Table 3-1), less the tables' 2
    for (wait, protection, stall) in [(20, 0x0006, 14), (40, 0x0005, 30)] {
        let (mut chip, _, _) = duart_board(wait, Some(protection));
        let now = chip.now();
        let acknowledged = chip.board.acknowledge(two, now);
        assert_eq!(acknowledged.answer, Acknowledge::Spurious);
        assert_eq!(acknowledged.stall, Cycles(stall));
    }
}

#[test]
fn a_duart_too_slow_for_the_bus_monitor_is_spurious() {
    // BME, BMT 16 clocks (SIMRM 3.2), the watchdog off: a bus error
    // ends the acknowledge, so the spurious vector (CPU32RM 6.2.11)
    let (chip, ram, duart) = duart_board(20, Some(0x0006));
    assert_eq!(chip.memory(ram)[..2], [0xbe, 0xef], "the handler ran");
    assert_eq!(taken_vector(&chip, ram), (SPURIOUS_VECTOR.into(), 2));
    assert!(
        chip.peripheral::<Duart>(duart)
            .unwrap()
            .acknowledges
            .is_empty()
    );
}

#[test]
fn a_reset_reaches_only_the_peripherals_in_its_scope() {
    let mut builder = Builder::default();
    builder
        .memory(image(&[NOP], ROM_BYTES), false, &[ChipSelect::Boot])
        .unwrap();
    let everything = builder.unwired_peripheral(Recorder::default());
    // a board whose DUART sees no watchdog reset, as smartclock's
    let not_watchdog = builder.unwired_peripheral(Recorder::default());
    builder.reset_scope(not_watchdog, ResetScope::ALL.without(ResetCause::Watchdog));
    let power_on = builder.unwired_peripheral(Recorder::default());
    builder.reset_scope(power_on, ResetScope::of(&[ResetCause::PowerOn]));
    let none = builder.unwired_peripheral(Recorder::default());
    builder.reset_scope(none, ResetScope::NONE);
    let mut chip = builder.build();
    let resets = |chip: &Mc68331| {
        [everything, not_watchdog, power_on, none]
            .map(|peripheral| chip.peripheral::<Recorder>(peripheral).unwrap().resets)
    };
    assert_eq!(resets(&chip), [1, 1, 1, 0]);
    chip.reset();
    assert_eq!(resets(&chip), [2, 2, 1, 0]);
    chip.board.reset_instruction(chip.now());
    assert_eq!(resets(&chip), [3, 3, 1, 0]);
    chip.take_events();
    // the watchdog's 2^9 EXTAL periods at 256 clocks
    chip.run_for(Cycles(512 * 256 + 100));
    let events = chip.take_events();
    assert!(
        matches!(events[..], [Event::Reset(_, ResetCause::Watchdog)]),
        "{events:?}"
    );
    assert_eq!(resets(&chip), [4, 3, 1, 0]);
}

#[test]
fn the_host_sees_each_port_edge_and_drives_the_inputs() {
    let mut program: Vec<u16> = Vec::new();
    // PE0 a port output; a pulse on it
    program.extend(store_byte(SIM + 0x17, 0xfe));
    program.extend(store_byte(SIM + 0x15, 0x01));
    program.extend(store_byte(SIM + 0x11, 0x01));
    program.extend(store_byte(SIM + 0x11, 0x00));
    // CS3 to CS5 discrete outputs; PC1 low
    program.extend(store_word(SIM + 0x44, 0x00ff));
    program.extend(store_byte(SIM + 0x41, 0x7d));
    // PORTE read into D0: PE1 an input
    program.extend(load_word(SIM + 0x10, 0));
    program.push(BRANCH_TO_SELF);
    let (mut chip, _) = booting(&program);
    chip.set_port_e_pins(0x02);
    chip.run_for(Cycles(2000));
    assert_eq!(chip.cpu().registers.data[0] & 0xff, 0x02);
    assert_eq!(
        chip.port_e_outputs(),
        PortLevels {
            driven: 0x01,
            levels: 0x00
        }
    );
    assert_eq!(
        chip.port_c_outputs(),
        PortLevels {
            driven: 0x07,
            levels: 0x05
        }
    );
    let changes = chip.take_port_changes();
    let seen: Vec<Change> = changes.iter().map(|&(_, change)| change).collect();
    let change = |port, driven, levels| Change {
        port,
        outputs: PortLevels { driven, levels },
    };
    assert_eq!(
        seen,
        [
            change(PortName::E, 0x01, 0x00),
            change(PortName::E, 0x01, 0x01),
            change(PortName::E, 0x01, 0x00),
            change(PortName::C, 0x07, 0x07),
            change(PortName::C, 0x07, 0x05),
        ]
    );
    // each at the time of its write, within one quantum
    assert!(changes.windows(2).all(|pair| pair[0].0 < pair[1].0));
    assert!(chip.take_port_changes().is_empty());
}

#[test]
fn gpt_output_edges_keep_their_times_across_quanta() {
    let (mut chip, _) = booting(&[NOP]);
    // OC2 toggles PGP4 at TCNT $10, OC3 sets PGP5 at $30, OC1 clears it
    // at $50 (GPTRM 3.3)
    write_register(&mut chip, GPT + 0x1e, 0x0d00);
    write_register(&mut chip, GPT + 0x14, 0x0050);
    write_register(&mut chip, GPT + 0x16, 0x0010);
    write_register(&mut chip, GPT + 0x18, 0x0030);
    write_register(&mut chip, GPT + 0x08, 0x2000);
    chip.set_report_pwm_edges(Channel::B, true);
    // PWMB $01: high for 2 of every 512 clocks from 512
    write_register(&mut chip, GPT + 0x26, 0x0001);
    let mut changes = Vec::new();
    while chip.now() < Cycles(1100) {
        chip.run_for(Cycles(37));
        changes.extend(chip.take_gpt_changes());
    }
    let gp = |driven, levels| OutputChange::PortGp(PortLevels { driven, levels });
    let b = |level| OutputChange::Pwm {
        channel: Channel::B,
        level,
    };
    assert_eq!(
        changes,
        [
            (Cycles(0), gp(0x30, 0x00)),
            (Cycles(4 * 0x10), gp(0x30, 0x10)),
            (Cycles(4 * 0x30), gp(0x30, 0x30)),
            (Cycles(4 * 0x50), gp(0x30, 0x10)),
            (Cycles(512), b(true)),
            (Cycles(514), b(false)),
            (Cycles(1024), b(true)),
            (Cycles(1026), b(false)),
        ]
    );
}

#[test]
fn a_scheduled_gpt_input_edge_is_captured_inside_a_run() {
    let (mut chip, _) = booting(&[NOP]);
    // EDGE1 rising (GPTRM 3.2)
    write_register(&mut chip, GPT + 0x1e, 0x0001);
    let pgp0 = Input::PortGp(PortGpPin::new(0).unwrap());
    chip.schedule_gpt_input(Cycles(1001), pgp0, false);
    chip.schedule_gpt_input(Cycles(2001), pgp0, true);
    assert_eq!(chip.board.next_event(), Some(Cycles(1001)));
    chip.run_for(Cycles(5000));
    assert_eq!(
        cycle(&mut chip, GPT + 0x0e, Size::Word, None).unwrap().0,
        500
    );
}

/// CS1's base address in the tests that wire a peripheral to it.
const CS1_BASE: u32 = 0x20_0000;
/// CSBAR1 for it: ADDR\[23:11\] from bit 3, BLKSZ 011, 64 Kbytes (SIMRM
/// 7.10.2, Table 7-6).
const CS1_BAR: u16 = 0x2003;
/// CSOR1: both bytes, read and write, fast termination, supervisor
/// and user space (SIMRM 7.10.3).
const CS1_FAST: u16 = 0x7bb0;

/// A chip running `program` from its boot ROM, `peripheral` on CS1 at
/// `CS1_BASE`; `peripheral` is given the chip's stop request.
fn with_peripheral<D: MappedPeripheral + Timed + Save + 'static>(
    program: &[u16],
    peripheral: impl FnOnce(StopRequest) -> D,
) -> (Mc68331, MemoryId, PeripheralId) {
    let mut builder = Builder::default();
    let rom = builder
        .memory(image(program, ROM_BYTES), false, &[ChipSelect::Boot])
        .unwrap();
    let peripheral = peripheral(builder.stop_request());
    let id = builder.peripheral(peripheral, &[cs(1)]).unwrap();
    let mut chip = builder.build();
    write_register(&mut chip, SIM + 0x50, CS1_BAR);
    write_register(&mut chip, SIM + 0x52, CS1_FAST);
    (chip, rom, id)
}

/// A peripheral that asks the run to stop at each read.
#[derive(Debug)]
struct Stopper(StopRequest);

impl MappedPeripheral for Stopper {
    fn read(&mut self, _: BusCycle) -> Result<u32, BusError> {
        self.0.request();
        Ok(0)
    }

    fn write(&mut self, _: BusCycle, _: u32) -> Result<(), BusError> {
        Ok(())
    }
}

mc683xx_cpu32::unsavable!(Stopper, "a stopper");

impl Timed for Stopper {}

#[test]
fn a_poke_writes_rom_and_refuses_what_the_core_does_not_serve() {
    let (mut chip, _) = booting(&[NOP, NOP]);
    chip.poke(Address(START), &[0x4e, 0x75]).unwrap();
    assert_eq!(
        chip.peek(Address(START), Size::Word, FunctionCode::SUPERVISOR_DATA),
        Ok(0x4e75)
    );
    // the SIM's registers are the chip's, not memory the core serves
    let simcr = Address(0x00ff_fa00);
    assert_eq!(chip.poke(simcr, &[0]), Err(Unserved(simcr)));
}

#[test]
fn a_hook_stops_the_chip_run() {
    let (mut chip, _) = booting(&[NOP, NOP]);
    let at = Address(START + 2);
    let id = chip
        .cpu_control()
        .add_code_hook(at..=at, |_| ControlFlow::Break(()));
    let run = chip.run_for(Cycles(10_000));
    assert_eq!(run.stop, Stop::Hook(id));
    assert_eq!(chip.cpu().registers.pc, START + 2);
    assert!(chip.now() < Cycles(10_000));
    // On past it.
    assert_eq!(chip.run_for(Cycles(100)).stop, Stop::Budget);
}

#[test]
fn a_peripheral_stops_the_run_after_its_instruction() {
    let (mut chip, _, _) = with_peripheral(&load_word(CS1_BASE, 0), Stopper);
    let run = chip.run_for(Cycles(10_000));
    assert_eq!(run.stop, Stop::Requested);
    assert_eq!(chip.cpu().registers.pc, START + 6);
    // Asked once: the next run goes on.
    assert_eq!(chip.run_for(Cycles(100)).stop, Stop::Budget);
}

/// A peripheral whose interrupt output the host holds, shared; it takes
/// writes and reads zero.
#[derive(Debug)]
struct Line(Rc<Cell<bool>>);

impl MappedPeripheral for Line {
    fn read(&mut self, _: BusCycle) -> Result<u32, BusError> {
        Ok(0)
    }

    fn write(&mut self, _: BusCycle, _: u32) -> Result<(), BusError> {
        Ok(())
    }

    fn interrupt_request(&self) -> bool {
        self.0.get()
    }
}

mc683xx_cpu32::unsavable!(Line, "a line");

impl Timed for Line {}

#[test]
fn a_request_the_host_moves_between_runs_is_seen() {
    let line = Rc::new(Cell::new(false));
    let mut builder = Builder::default();
    builder
        .memory(image(&[NOP], ROM_BYTES), false, &[ChipSelect::Boot])
        .unwrap();
    let peripheral = builder
        .peripheral(Line(Rc::clone(&line)), &[cs(1)])
        .unwrap();
    let seven = IrqPin::IRQ7;
    builder.interrupt(peripheral, seven);
    builder.autovector(seven);
    let mut chip = builder.build();
    write_register(&mut chip, SIM + 0x50, CS1_BAR);
    write_register(&mut chip, SIM + 0x52, CS1_FAST);
    // a stack the peripheral takes the frame's writes on
    chip.registers_mut().ssp = CS1_BASE + 0x1000;
    chip.set_tracing(ChipTracing {
        accesses: false,
        exceptions: true,
    });
    chip.run_for(Cycles(100));
    line.set(true);
    chip.run_for(Cycles(100));
    assert!(
        chip.take_events().iter().any(|event| matches!(
            event,
            Event::Exception(_, entry) if entry.level == Some(seven.level())
        )),
        "level 7 taken"
    );
}

/// A peripheral that asks the run to end early at each read.
#[derive(Debug)]
struct Syncer(SyncRequest);

impl MappedPeripheral for Syncer {
    fn read(&mut self, _: BusCycle) -> Result<u32, BusError> {
        self.0.request();
        Ok(0)
    }

    fn write(&mut self, _: BusCycle, _: u32) -> Result<(), BusError> {
        Ok(())
    }
}

mc683xx_cpu32::unsavable!(Syncer, "a syncer");

impl Timed for Syncer {}

#[test]
fn a_peripheral_ends_the_run_early_after_its_instruction() {
    let mut builder = Builder::default();
    builder
        .memory(
            image(&load_word(CS1_BASE, 0), ROM_BYTES),
            false,
            &[ChipSelect::Boot],
        )
        .unwrap();
    let syncer = Syncer(builder.sync_request());
    builder.peripheral(syncer, &[cs(1)]).unwrap();
    let mut chip = builder.build();
    write_register(&mut chip, SIM + 0x50, CS1_BAR);
    write_register(&mut chip, SIM + 0x52, CS1_FAST);
    let start = chip.now();
    let run = chip.run_for(Cycles(10_000));
    assert_eq!(run.stop, Stop::Budget, "not a stop");
    assert_eq!(chip.cpu().registers.pc, START + 6);
    assert_eq!(run.cycles, chip.now() - start);
    assert!(run.cycles < Cycles(10_000));
    // Asked once: the next run spends its budget.
    let run = chip.run_for(Cycles(100));
    assert_eq!(run.stop, Stop::Budget);
    assert!(run.cycles >= Cycles(100));
}

/// ILLEGAL (CPU32RM 4, ILLEGAL).
const ILLEGAL: u16 = 0x4afc;

#[test]
fn tracing_reports_peripheral_cycles_and_exceptions() {
    let mut program = load_word(CS1_BASE + 2, 0).to_vec();
    program.push(ILLEGAL);
    let (mut chip, _, _) = with_peripheral(&program, |_| Recorder::answering(0x1234));
    // A stack the peripheral takes the frame's writes on.
    chip.registers_mut().ssp = CS1_BASE + 0x1000;
    chip.take_events();
    chip.set_tracing(ChipTracing {
        accesses: true,
        exceptions: true,
    });
    let before = chip.instructions();
    chip.run_for(Cycles(2000));
    let events = chip.take_events();
    let Some(Event::Access(access)) = events.first() else {
        panic!("an access first: {events:?}");
    };
    assert_eq!(access.address, Address(CS1_BASE + 2));
    assert_eq!((access.write, access.value), (false, Some(0x1234)));
    assert!(events.iter().any(|event| matches!(
        event,
        Event::Exception(_, entry)
            if entry.vector == Vector::ILLEGAL_INSTRUCTION && entry.pc == Address(START + 6)
    )));
    assert!(chip.instructions() >= before + 2);
    // Off again: nothing.
    chip.set_tracing(ChipTracing::default());
    chip.run_for(Cycles(200));
    assert!(chip.take_events().is_empty());
}

/// A model that asks the run to stop when time reaches `at`.
#[derive(Debug)]
struct StopAt {
    /// When, until it has.
    at: Option<Cycles>,
    /// The chip's stop request.
    stop: StopRequest,
}

impl MappedPeripheral for StopAt {
    fn read(&mut self, _: BusCycle) -> Result<u32, BusError> {
        Err(BusError)
    }

    fn write(&mut self, _: BusCycle, _: u32) -> Result<(), BusError> {
        Err(BusError)
    }
}

mc683xx_cpu32::unsavable!(StopAt, "a stopper");

impl Timed for StopAt {
    fn next_event(&self) -> Option<Cycles> {
        self.at
    }

    fn advance(&mut self, now: Cycles) {
        if self.at.is_some_and(|at| now >= at) {
            self.at = None;
            self.stop.request();
        }
    }
}

#[test]
fn a_model_stops_the_run_between_quanta() {
    let mut builder = Builder::default();
    builder
        .memory(image(&[NOP], ROM_BYTES), false, &[ChipSelect::Boot])
        .unwrap();
    let stop = builder.stop_request();
    builder.unwired_peripheral(StopAt {
        at: Some(Cycles(1000)),
        stop,
    });
    let mut chip = builder.build();
    let run = chip.run_for(Cycles(10_000));
    assert_eq!(run.stop, Stop::Requested);
    assert!(chip.now() >= Cycles(1000) && chip.now() < Cycles(1100));
}

/// A peripheral that reads as a count of its reads, which it saves.
#[derive(Debug, Default)]
struct Counter(u16);

/// The format of a [`Counter`]'s snapshot.
const COUNTER_VERSION: u32 = 1;

impl Save for Counter {
    fn save(&self) -> Result<Vec<u8>, StateError> {
        Ok(encode(COUNTER_VERSION, &self.0))
    }

    fn restore(&mut self, snapshot: &[u8]) -> Result<(), StateError> {
        self.0 = decode(COUNTER_VERSION, snapshot)?;
        Ok(())
    }
}

impl MappedPeripheral for Counter {
    fn read(&mut self, _: BusCycle) -> Result<u32, BusError> {
        self.0 = self.0.wrapping_add(1);
        Ok(self.0.into())
    }

    fn write(&mut self, _: BusCycle, _: u32) -> Result<(), BusError> {
        Ok(())
    }
}

impl Timed for Counter {}

/// RAM's base on CS0 in the snapshot tests.
const CS0_BASE: u32 = 0x10_0000;
/// CSBAR0 for it: a 64-Kbyte block.
const CS0_BAR: u16 = 0x1003;

/// A board whose program copies the counter on CS1 into RAM on CS0,
/// in a loop of branches; the chip, its RAM and the counter.
fn copying() -> (Mc68331, MemoryId, PeripheralId) {
    copying_with(|_| {})
}

/// [`copying`]'s board, with what `more` adds after the counter.
fn copying_with(more: impl FnOnce(&mut Builder)) -> (Mc68331, MemoryId, PeripheralId) {
    let program = [
        // MOVE.W (CS1_BASE).L, D1
        MOVE_WORD_FROM_ABSOLUTE | 1 << 9,
        (CS1_BASE >> 16) as u16,
        CS1_BASE as u16,
        // MOVE.W D1, (A0)+
        0x30c1,
        // DBRA D0, START
        0x51c8,
        0xfff6,
        // BRA START
        0x60f2,
    ];
    let mut builder = Builder::default();
    builder
        .memory(image(&program, ROM_BYTES), false, &[ChipSelect::Boot])
        .unwrap();
    let ram = builder.memory(vec![0; 64 << 10], true, &[cs(0)]).unwrap();
    let counter = builder.peripheral(Counter::default(), &[cs(1)]).unwrap();
    more(&mut builder);
    let mut chip = builder.build();
    write_register(&mut chip, SIM + 0x4c, CS0_BAR);
    write_register(&mut chip, SIM + 0x4e, CS1_FAST);
    write_register(&mut chip, SIM + 0x50, CS1_BAR);
    write_register(&mut chip, SIM + 0x52, CS1_FAST);
    chip.registers_mut().set_address(0, CS0_BASE);
    chip.registers_mut().data[0] = 9;
    (chip, ram, counter)
}

/// The trace of `budget` clocks, and what the run leaves.
fn traced(chip: &mut Mc68331, ram: MemoryId, budget: u64) -> (Vec<Event>, Vec<u8>, Vec<u8>) {
    chip.set_tracing(ChipTracing {
        accesses: true,
        exceptions: true,
    });
    chip.take_events();
    chip.run_for(Cycles(budget));
    (
        chip.take_events(),
        chip.memory(ram).to_vec(),
        chip.save().unwrap(),
    )
}

#[test]
fn a_chip_snapshot_mid_run_reproduces_the_run() {
    let (mut chip, ram, counter) = copying();
    chip.run_for(Cycles(3_000));
    let snapshot = chip.save().unwrap();
    let count = chip.peripheral::<Counter>(counter).unwrap().0;
    assert!(count > 0);
    let first = traced(&mut chip, ram, 5_000);
    assert!(first.0.len() > 20, "{:?}", first.0);
    // Another chip built the same way, its peripheral restored with it.
    let (mut other, other_ram, other_counter) = copying();
    other.run_for(Cycles(1_234));
    other.restore(&snapshot).unwrap();
    assert_eq!(other.peripheral::<Counter>(other_counter).unwrap().0, count);
    assert_eq!(traced(&mut other, other_ram, 5_000), first);
    // And the first chip, back.
    chip.restore(&snapshot).unwrap();
    assert_eq!(chip.peripheral::<Counter>(counter).unwrap().0, count);
    assert_eq!(traced(&mut chip, ram, 5_000), first);
}

/// A peripheral with no state: it reads zero and ignores writes.
#[derive(Debug)]
struct Fixed;

impl Stateless for Fixed {}

impl MappedPeripheral for Fixed {
    fn read(&mut self, _: BusCycle) -> Result<u32, BusError> {
        Ok(0)
    }

    fn write(&mut self, _: BusCycle, _: u32) -> Result<(), BusError> {
        Ok(())
    }
}

impl Timed for Fixed {}

#[test]
fn a_peripheral_that_cannot_save_fails_the_save_and_restore_naming_it() {
    let (saving, _, _) = copying_with(|builder| {
        builder.unwired_peripheral(Fixed);
    });
    let snapshot = saving.save().unwrap();
    /// A model that cannot save.
    struct Unsaving;

    impl MappedPeripheral for Unsaving {
        fn read(&mut self, _: BusCycle) -> Result<u32, BusError> {
            Ok(0)
        }

        fn write(&mut self, _: BusCycle, _: u32) -> Result<(), BusError> {
            Ok(())
        }
    }

    mc683xx_cpu32::unsavable!(Unsaving, "an unsaving model");

    impl Timed for Unsaving {}

    let (mut unsaving, _, _) = copying_with(|builder| {
        builder.unwired_peripheral(Unsaving);
    });
    let unsupported = StateError::unsupported("an unsaving model").of_peripheral("peripheral 1");
    assert_eq!(
        unsaving.save().unwrap_err().to_string(),
        unsupported.to_string()
    );
    assert_eq!(
        unsaving.restore(&snapshot).unwrap_err().to_string(),
        unsupported.to_string()
    );
}

#[test]
fn a_restore_into_another_count_of_peripherals_is_refused_and_changes_nothing() {
    let (mut chip, _, _) = copying();
    chip.run_for(Cycles(1_000));
    let snapshot = chip.save().unwrap();
    let (mut other, ram, _) = copying_with(|builder| {
        builder.unwired_peripheral(Fixed);
    });
    other.run_for(Cycles(300));
    let before = other.save().unwrap();
    let memory = other.memory(ram).to_vec();
    assert!(matches!(
        other.restore(&snapshot),
        Err(StateError::Mismatch(reason)) if reason == "peripherals"
    ));
    assert_eq!(other.save().unwrap(), before);
    assert_eq!(other.memory(ram), memory);
}

#[test]
fn a_snapshot_of_fixed_memories_elsewhere_is_refused_and_changes_nothing() {
    let at = |base: u32| {
        copying_with(|builder| {
            builder
                .fixed_memory(Address(base), vec![0; 64 << 10], true)
                .unwrap();
        })
        .0
    };
    let mut chip = at(0x10_0000);
    chip.run_for(Cycles(1_000));
    let snapshot = chip.save().unwrap();
    for mut other in [at(0x20_0000), copying().0] {
        other.run_for(Cycles(300));
        let before = other.save().unwrap();
        assert!(matches!(
            other.restore(&snapshot),
            Err(StateError::Mismatch(reason)) if reason == "fixed memories"
        ));
        assert_eq!(other.save().unwrap(), before);
    }
}

#[test]
fn a_restore_rebuilds_the_page_tables_from_the_chips_own_wiring() {
    // The RAM on CS0 and the counter on CS1, or the other way round:
    // the same regions and peripherals, wired apart.
    let wired = |ram: u8, counter: u8| {
        let mut builder = Builder::default();
        builder
            .memory(image(&[0x60fe], ROM_BYTES), false, &[ChipSelect::Boot])
            .unwrap();
        builder.memory(vec![0; 64 << 10], true, &[cs(ram)]).unwrap();
        builder
            .peripheral(Counter::default(), &[cs(counter)])
            .unwrap();
        let mut chip = builder.build();
        write_register(&mut chip, SIM + 0x4c, CS0_BAR);
        write_register(&mut chip, SIM + 0x4e, CS1_FAST);
        write_register(&mut chip, SIM + 0x50, CS1_BAR);
        write_register(&mut chip, SIM + 0x52, CS1_FAST);
        chip.run_for(Cycles(300));
        chip
    };
    let pages = |chip: &Mc68331| {
        [CS0_BASE, CS1_BASE].map(|base| chip.cpu().memory.entry(Space::Supervisor, Address(base)))
    };
    let chip = wired(0, 1);
    let snapshot = chip.save().unwrap();
    let mut other = wired(1, 0);
    let own = pages(&other);
    assert_ne!(pages(&chip), own, "wired apart");
    other.restore(&snapshot).unwrap();
    assert_eq!(pages(&other), own);
}

#[test]
fn a_peripheral_that_refuses_its_snapshot_leaves_the_chip_as_it_was() {
    let two = || {
        copying_with(|builder| {
            builder.unwired_peripheral(Counter(7));
        })
    };
    let (mut chip, _, first) = two();
    chip.run_for(Cycles(1_000));
    let snapshot = chip.save().unwrap();
    // QILR and QIVR, so the QSM differs from the snapshot's, and the
    // core and the first counter move on
    write_register(&mut chip, QSM + 0x04, 0x1240);
    chip.run_for(Cycles(1_000));
    let before = chip.save().unwrap();
    let count = chip.peripheral::<Counter>(first).unwrap().0;
    // the first peripheral's taken, then the second's refused
    let later = COUNTER_VERSION + 1;
    let bad_peripheral = state::with_peripheral(&snapshot, 1, encode(later, &0_u16));
    assert_eq!(
        chip.restore(&bad_peripheral).unwrap_err().to_string(),
        StateError::Version {
            found: later,
            expected: COUNTER_VERSION
        }
        .of_peripheral("peripheral 1")
        .to_string()
    );
    assert_eq!(chip.save().unwrap(), before);
    assert_eq!(chip.peripheral::<Counter>(first).unwrap().0, count);
    // every peripheral's and module's taken, then the core's refused
    let bad_core = state::with_cpu(&snapshot, Vec::new());
    assert!(matches!(
        chip.restore(&bad_core),
        Err(StateError::Truncated)
    ));
    assert_eq!(chip.save().unwrap(), before);
    let (mut other, _, _) = two();
    other.restore(&snapshot).unwrap();
    assert_eq!(other.save().unwrap(), snapshot);
}

/// A peripheral that saves but refuses every snapshot, its own too, which
/// breaks [`Save::restore`]'s contract; it counts the restores it is
/// asked for.
#[derive(Debug, Default)]
struct Refuser(Rc<Cell<u32>>);

impl Save for Refuser {
    fn save(&self) -> Result<Vec<u8>, StateError> {
        Ok(encode(COUNTER_VERSION, &()))
    }

    fn restore(&mut self, _: &[u8]) -> Result<(), StateError> {
        self.0.set(self.0.get() + 1);
        Err(StateError::invalid("a refuser"))
    }
}

impl MappedPeripheral for Refuser {
    fn read(&mut self, _: BusCycle) -> Result<u32, BusError> {
        Ok(0)
    }

    fn write(&mut self, _: BusCycle, _: u32) -> Result<(), BusError> {
        Ok(())
    }
}

impl Timed for Refuser {}

#[test]
fn a_refusing_peripheral_is_not_rolled_back_and_the_peripherals_before_it_are() {
    let restores = Rc::new(Cell::new(0));
    let (mut chip, _, counter) = copying_with(|builder| {
        builder.unwired_peripheral(Refuser(Rc::clone(&restores)));
    });
    chip.run_for(Cycles(1_000));
    let snapshot = chip.save().unwrap();
    chip.run_for(Cycles(1_000));
    let before = chip.save().unwrap();
    let count = chip.peripheral::<Counter>(counter).unwrap().0;
    assert_eq!(
        chip.restore(&snapshot).unwrap_err().to_string(),
        StateError::invalid("a refuser")
            .of_peripheral("peripheral 1")
            .to_string()
    );
    assert_eq!(restores.get(), 1, "asked once, not to take its backup");
    assert_eq!(chip.peripheral::<Counter>(counter).unwrap().0, count);
    assert_eq!(chip.save().unwrap(), before);
}

/// A QSPI peripheral that answers each transfer with a count of them,
/// which it saves.
#[derive(Debug, Default)]
struct Tally(u16);

/// The format of a [`Tally`]'s snapshot.
const TALLY_VERSION: u32 = 1;

impl Save for Tally {
    fn save(&self) -> Result<Vec<u8>, StateError> {
        Ok(encode(TALLY_VERSION, &self.0))
    }

    fn restore(&mut self, snapshot: &[u8]) -> Result<(), StateError> {
        self.0 = decode(TALLY_VERSION, snapshot)?;
        Ok(())
    }
}

impl SpiPeripheral for Tally {
    fn transfer(&mut self, _: QspiTransfer) -> u16 {
        self.0 = self.0.wrapping_add(1);
        self.0
    }
}

/// [`copying`]'s board with a [`Tally`] of `count` on the QSPI.
fn tallying(count: u16) -> (Mc68331, MemoryId, PeripheralId) {
    copying_with(|builder| builder.qspi(Tally(count)))
}

/// The count of the [`Tally`] on `chip`'s QSPI.
fn tally(chip: &Mc68331) -> u16 {
    chip.qspi_peripheral::<Tally>().unwrap().0
}

#[test]
fn the_qspi_peripheral_is_in_the_chip_snapshot() {
    let (mut chip, ram, counter) = tallying(3);
    chip.run_for(Cycles(1_000));
    let snapshot = chip.save().unwrap();
    let count = chip.peripheral::<Counter>(counter).unwrap().0;
    chip.qspi_peripheral_mut::<Tally>().unwrap().0 = 9;
    chip.run_for(Cycles(1_000));
    chip.restore(&snapshot).unwrap();
    assert_eq!(tally(&chip), 3);
    assert_eq!(chip.peripheral::<Counter>(counter).unwrap().0, count);
    let first = traced(&mut chip, ram, 2_000);
    let (mut other, other_ram, _) = tallying(5);
    other.restore(&snapshot).unwrap();
    assert_eq!(tally(&other), 3);
    assert_eq!(traced(&mut other, other_ram, 2_000), first);
}

#[test]
fn a_qspi_peripheral_that_cannot_save_fails_the_save_and_restore_naming_it() {
    /// A QSPI peripheral that cannot save.
    struct Unsaving;

    impl SpiPeripheral for Unsaving {
        fn transfer(&mut self, _: QspiTransfer) -> u16 {
            0
        }
    }

    mc683xx_cpu32::unsavable!(Unsaving, "an unsaving model");

    let (saving, _, _) = tallying(0);
    let snapshot = saving.save().unwrap();
    let (mut unsaving, _, _) = copying_with(|builder| builder.qspi(Unsaving));
    let unsupported =
        StateError::unsupported("an unsaving model").of_peripheral("the QSPI peripheral");
    assert_eq!(
        unsaving.save().unwrap_err().to_string(),
        unsupported.to_string()
    );
    assert_eq!(
        unsaving.restore(&snapshot).unwrap_err().to_string(),
        unsupported.to_string()
    );
}

#[test]
fn a_snapshot_with_another_qspi_attachment_is_refused_and_changes_nothing() {
    let (with, _, _) = tallying(1);
    let (without, _, _) = copying();
    for (snapshot, mut chip) in [
        (with.save().unwrap(), copying().0),
        (without.save().unwrap(), tallying(2).0),
    ] {
        chip.run_for(Cycles(300));
        let before = chip.save().unwrap();
        assert!(matches!(
            chip.restore(&snapshot),
            Err(StateError::Mismatch(reason)) if reason == "the QSPI peripheral"
        ));
        assert_eq!(chip.save().unwrap(), before);
    }
}

#[test]
fn a_qspi_peripheral_that_refuses_its_snapshot_leaves_the_chip_as_it_was() {
    let (mut chip, _, counter) = tallying(4);
    chip.run_for(Cycles(1_000));
    let snapshot = chip.save().unwrap();
    chip.qspi_peripheral_mut::<Tally>().unwrap().0 = 8;
    chip.run_for(Cycles(1_000));
    let before = chip.save().unwrap();
    let count = chip.peripheral::<Counter>(counter).unwrap().0;
    // the peripheral's taken, then the QSPI peripheral's refused
    let later = TALLY_VERSION + 1;
    let bad_qspi = state::with_qspi(&snapshot, Some(encode(later, &0_u16)));
    assert_eq!(
        chip.restore(&bad_qspi).unwrap_err().to_string(),
        StateError::Version {
            found: later,
            expected: TALLY_VERSION
        }
        .of_peripheral("the QSPI peripheral")
        .to_string()
    );
    assert_eq!(chip.save().unwrap(), before);
    assert_eq!(chip.peripheral::<Counter>(counter).unwrap().0, count);
    // the QSPI peripheral's taken, then the core's refused
    let bad_core = state::with_cpu(&snapshot, Vec::new());
    assert!(matches!(
        chip.restore(&bad_core),
        Err(StateError::Truncated)
    ));
    assert_eq!(tally(&chip), 8);
    assert_eq!(chip.save().unwrap(), before);
}

#[test]
fn a_module_that_cannot_save_refuses_the_snapshot() {
    /// A module that cannot save.
    #[derive(Debug)]
    struct Unsaved;

    impl Timed for Unsaved {}

    impl mc683xx_modules::module::sealed::Sealed for Unsaved {}

    impl Module for Unsaved {
        fn block(&self) -> Block {
            Block {
                offset: 0x900,
                length: 0x40,
            }
        }

        fn read(&mut self, _: module::RegisterAccess) -> Option<u32> {
            None
        }

        fn peek(&self, _: module::RegisterAccess) -> module::Peek {
            module::Peek::Unacknowledged
        }

        fn write(&mut self, _: module::RegisterAccess, _: u32) -> bool {
            false
        }

        fn request(&self) -> Option<module::InterruptRequest> {
            None
        }

        fn acknowledge(&mut self, _: InterruptLevel, _: Cycles) -> Option<u8> {
            None
        }

        fn reset(&mut self, _: Cycles) {}

        fn save(&self) -> Result<Vec<u8>, StateError> {
            Err(StateError::unsupported("a module"))
        }

        fn restore(&mut self, _: &[u8]) -> Result<(), StateError> {
            Err(StateError::unsupported("a module"))
        }
    }

    let (mut chip, _) = booting(&[NOP]);
    let snapshot = chip.save().unwrap();
    chip.board.modules.push(Box::new(Unsaved));
    assert!(matches!(chip.save(), Err(StateError::Unsupported(reason)) if reason == "a module"));
    assert!(matches!(
        chip.restore(&snapshot),
        Err(StateError::Mismatch(reason)) if reason == "modules"
    ));
    let mut snapshot = snapshot;
    snapshot[0] ^= 1;
    assert!(matches!(
        booting(&[NOP]).0.restore(&snapshot),
        Err(StateError::Version { .. })
    ));
}

/// Where [`interrupt_trace`] logs D0 at each interrupt it takes.
const TRACE_LOG: u32 = RAM + 0x1000;
/// The PIT's handler in [`interrupt_trace`]; the SCI's is at `HANDLER`.
const TIMER_HANDLER: u32 = 0x2100;

/// The wired board counting in D0 with the PIT interrupting every 1024
/// clocks at level 6 and the SCI's TDRE at level 3, enabled by the
/// loop's own write every 64 counts and disabled by its handler; each
/// handler logs D0 at `TRACE_LOG`.  The log is where every interrupt
/// was taken, instruction by instruction.
fn interrupt_trace() -> Wired {
    const MOVE_D0_TO_POSTINCREMENT_A0: u16 = 0x20c0;
    let mut program: Vec<u16> = Vec::new();
    program.extend(store_word(SIM + 0x20, 0));
    program.extend(store_word(SIM + 0x22, 0x0650));
    program.extend(store_word(SIM + 0x24, 0x0001));
    program.extend(store_word(QSM, 0x0085));
    program.extend(store_word(QSM + 0x04, 0x0340));
    program.extend(store_word(QSM + 0x08, 1));
    // LEA (TRACE_LOG).L,A0; MOVEQ #0,D0
    program.extend([0x41f9, (TRACE_LOG >> 16) as u16, TRACE_LOG as u16, 0x7000]);
    // loop: ADDQ.L #1,D0; MOVE.L D0,D1; ANDI.W #$3F,D1; BNE.S skip;
    // TIE and TE; skip: BRA.S loop
    program.extend([0x5280, 0x2200, 0x0241, 0x003f, 0x6608]);
    program.extend(store_word(QSM + 0x0a, 0x0088));
    program.push(0x60ec);
    let sci_handler: Vec<u16> = [MOVE_D0_TO_POSTINCREMENT_A0]
        .into_iter()
        .chain(store_word(QSM + 0x0a, 0x0008))
        .chain([RTE])
        .collect();
    let (sci_entry, sci_address) = vector_to_handler(0x40);
    let timer_entry = (
        0x50 * 4,
        [(TIMER_HANDLER >> 16) as u16, TIMER_HANDLER as u16],
    );
    let mut board = wired_patched(
        &program,
        &[
            (sci_entry, &sci_address),
            (HANDLER, &sci_handler),
            (timer_entry.0, &timer_entry.1),
            (TIMER_HANDLER, &[MOVE_D0_TO_POSTINCREMENT_A0, RTE]),
        ],
    );
    board.chip.registers_mut().set_sr(0x2000);
    board
}

/// The D0 values [`interrupt_trace`] logged.
fn trace_log(board: &Wired) -> Vec<u32> {
    let end = (board.chip.cpu().registers.address(0) - RAM) as usize;
    board.chip.memory(board.ram)[(TRACE_LOG - RAM) as usize..end]
        .chunks(4)
        .map(|long| u32::from_be_bytes([long[0], long[1], long[2], long[3]]))
        .collect()
}

#[test]
fn interrupts_are_taken_at_the_same_boundary_however_the_run_syncs() {
    let budget = Cycles(100_000);
    let mut long = interrupt_trace();
    long.chip.run_for(budget);
    let mut stepped = interrupt_trace();
    let end = stepped.chip.now() + budget;
    while stepped.chip.now() < end {
        stepped.chip.run_for(Cycles(1));
    }
    let log = trace_log(&long);
    assert_eq!(log, trace_log(&stepped));
    assert_eq!(long.chip.now(), stepped.chip.now());
    assert_eq!(long.chip.cpu().registers, stepped.chip.cpu().registers);
    // the log as the board gave it before its interrupt level was
    // cached: both kinds of source, at once and in turn
    assert_eq!(
        log[..16],
        [
            16, 38, 60, 64, 79, 101, 123, 128, 142, 164, 186, 192, 205, 227, 249, 256
        ]
    );
    assert_eq!(log.len(), 130);
    assert_eq!(
        log.iter().map(|&value| u64::from(value)).sum::<u64>(),
        135_120
    );
}

/// MOVEQ #`value`,D0 (CPU32RM 4, MOVEQ).
const fn moveq_to_d0(value: u8) -> u16 {
    0x7000 | value as u16
}

#[test]
fn new_code_runs_however_the_host_writes_it() {
    // writable memory on CSBOOT running MOVEQ #0,D0; BRA.S back to it
    let mut builder = Builder::default();
    let code = builder
        .memory(
            image(&[moveq_to_d0(0), 0x60fc], ROM_BYTES),
            true,
            &[ChipSelect::Boot],
        )
        .unwrap();
    let mut chip = builder.build();
    let runs = |chip: &mut Mc68331, value: u8| {
        chip.run_for(Cycles(200));
        assert_eq!(chip.cpu().registers.data[0], value.into());
    };
    runs(&mut chip, 0);
    let at = START as usize;
    let write = |chip: &mut Mc68331, value: u8| {
        chip.memory_mut(code)[at..at + 2].copy_from_slice(&moveq_to_d0(value).to_be_bytes());
    };
    write(&mut chip, 1);
    runs(&mut chip, 1);
    chip.bus_write(
        Address(START),
        Size::Word,
        FunctionCode::SUPERVISOR_DATA,
        moveq_to_d0(2).into(),
    )
    .unwrap();
    runs(&mut chip, 2);
    write(&mut chip, 3);
    let snapshot = chip.save().unwrap();
    write(&mut chip, 4);
    runs(&mut chip, 4);
    chip.restore(&snapshot).unwrap();
    runs(&mut chip, 3);
}

#[test]
fn code_the_cpu_writes_runs_at_once() {
    // MOVE.W #MOVEQ #7,D0 over the MOVEQ #6,D0 that follows it
    let next = START + 8;
    let program: Vec<u16> = store_word(next, moveq_to_d0(7))
        .into_iter()
        .chain([moveq_to_d0(6), BRANCH_TO_SELF])
        .collect();
    let mut builder = Builder::default();
    builder
        .memory(image(&program, ROM_BYTES), true, &[ChipSelect::Boot])
        .unwrap();
    let mut chip = builder.build();
    chip.run_for(Cycles(500));
    assert_eq!(chip.cpu().registers.data[0], 7);
}

#[test]
fn a_pin_wired_twice_is_named_as_the_manual_names_it() {
    let mut builder = Builder::default();
    builder
        .memory(vec![0; 2048], false, &[ChipSelect::Boot])
        .unwrap();
    let error = builder
        .memory(vec![0; 2048], false, &[ChipSelect::Boot])
        .unwrap_err();
    assert_eq!(error.to_string(), "CSBOOT is already wired");
}

/// When the GPT's OC1 interrupt, armed before LPSTOP and due after the
/// PIT wakes the chip, is taken, the chip run `budget` clocks at a time.
fn oc1_after_wake(budget: Cycles) -> Cycles {
    let mut program: Vec<u16> = Vec::new();
    // the watchdog off; the GPT at IARB 6, level 4, vectors from $60;
    // OC1 at TCNT + $80 with OC1I (GPTRM 3.2, 4.3); the PIT at level 6,
    // vector $50, PITR 1 (MC68331UM 4.2.11)
    program.extend(store_word(SIM + 0x20, 0));
    program.extend(store_word(GPT, 0x0006));
    program.extend(store_word(GPT + 0x04, 0x0460));
    // MOVE.W (TCNT).L,D0; ADDI.W #$80,D0; MOVE.W D0,(TOC1).L
    program.extend(load_word(GPT + 0x0a, 0));
    program.extend([0x0640, 0x0080]);
    program.extend([0x33c0, (GPT >> 16) as u16, (GPT + 0x14) as u16]);
    program.extend(store_word(GPT + 0x20, 0x0800));
    program.extend(store_word(SIM + 0x22, 0x0650));
    program.extend(store_word(SIM + 0x24, 0x0001));
    program.extend(LPSTOP_MASK_0);
    program.push(BRANCH_TO_SELF);
    let (pit_entry, pit_address) = vector_to_handler(0x50);
    let oc1_entry = 0x64 * 4;
    let oc1_handler_at = HANDLER + 0x100;
    let oc1_address = [(oc1_handler_at >> 16) as u16, oc1_handler_at as u16];
    let rte = [RTE];
    let oc1_handler: Vec<u16> = store_word(RAM, 0xbeef)
        .into_iter()
        .chain([BRANCH_TO_SELF])
        .collect();
    let mut board = wired_patched(
        &program,
        &[
            (pit_entry, &pit_address),
            (HANDLER, &rte),
            (oc1_entry, &oc1_address),
            (oc1_handler_at, &oc1_handler),
        ],
    );
    let chip = &mut board.chip;
    chip.set_tracing(ChipTracing {
        exceptions: true,
        ..ChipTracing::default()
    });
    let end = chip.now() + Cycles(50_000);
    let mut woke = false;
    while chip.now() < end {
        chip.run_for(budget);
        for event in chip.take_events() {
            if let Event::Exception(now, entry) = event {
                woke |= entry.vector == Vector(0x50);
                if entry.vector == Vector(0x64) {
                    assert!(woke, "OC1 before the wake");
                    return now;
                }
            }
        }
    }
    panic!("no OC1 interrupt");
}

#[test]
fn an_interrupt_after_a_wake_is_the_same_however_the_run_is_cut() {
    assert_eq!(oc1_after_wake(Cycles(50_000)), oc1_after_wake(Cycles(1)));
}

/// A peripheral that asks the run to stop at each read, and reads as
/// all ones in its low byte.
#[derive(Debug)]
struct OnesStopper(StopRequest);

impl MappedPeripheral for OnesStopper {
    fn read(&mut self, _: BusCycle) -> Result<u32, BusError> {
        self.0.request();
        Ok(0x00ff)
    }

    fn write(&mut self, _: BusCycle, _: u32) -> Result<(), BusError> {
        Ok(())
    }
}

mc683xx_cpu32::unsavable!(OnesStopper, "a stopper");

impl Timed for OnesStopper {}

#[test]
fn a_stop_asked_in_an_instruction_that_also_syncs_is_kept() {
    // MOVE.W (CS1_BASE).L,(PORTQS word).L: the read asks the stop, the
    // write moves a watched output, which syncs
    let program = [
        0x33f9,
        (CS1_BASE >> 16) as u16,
        CS1_BASE as u16,
        0x00ff,
        0xfc14,
    ];
    let (mut chip, _, _) = with_peripheral(&program, OnesStopper);
    // PQSPAR: all general-purpose; DDRQS: all outputs (QSMRM 3.3.2, 3.3.3)
    write_register(&mut chip, QSM + 0x16, 0x00ff);
    chip.set_output_stop(OutputStop {
        port_qs: 0xff,
        ..OutputStop::default()
    });
    let run = chip.run_for(Cycles(10_000));
    assert_eq!(run.stop, Stop::Requested);
}

/// What port QS drives, change by change, while firmware toggles PQS1
/// with a delay loop between, the watchdog off, run to 20,000 clocks
/// `step` at a time.
fn port_qs_toggles(step: u64) -> Vec<(Cycles, PortLevels)> {
    let mut program: Vec<u16> = Vec::new();
    // the watchdog off; PQSPAR all general-purpose, DDRQS PQS1 an
    // output (QSMRM 3.3.2, 3.3.3)
    program.extend(store_word(SIM + 0x20, 0));
    program.extend(store_word(QSM + 0x16, 0x0002));
    program.extend([
        // MOVE.B D2,(PORTQS).L; NOT.B D2; MOVE.L #$40,D0
        0x13c2,
        (QSM >> 16) as u16,
        (QSM + 0x15) as u16,
        0x4602,
        0x203c,
        0x0000,
        0x0040,
        // SUBQ.L #1,D0; BNE.S to it; BRA.S to the MOVE.B
        0x5380,
        0x66fc,
        0x60ec,
    ]);
    let (mut chip, _) = booting(&program);
    let mut changes = Vec::new();
    for target in (step..=20_000).step_by(step as usize) {
        chip.run_until(Cycles(target));
        changes.extend(chip.take_qsm_changes());
    }
    changes
}

#[test]
fn port_qs_changes_keep_their_times_however_the_run_is_cut() {
    let whole = port_qs_toggles(20_000);
    assert!(whole.len() > 4, "{whole:?}");
    let low = PortLevels {
        driven: 0x02,
        levels: 0x00,
    };
    let high = PortLevels {
        driven: 0x02,
        levels: 0x02,
    };
    // DDRQS drives the latch's 0, then the loop toggles it
    assert!(
        whole
            .iter()
            .map(|&(_, outputs)| outputs)
            .eq([low, high].into_iter().cycle().take(whole.len()))
    );
    assert!(whole.windows(2).all(|pair| pair[0].0 < pair[1].0));
    assert_eq!(port_qs_toggles(7), whole);
}
