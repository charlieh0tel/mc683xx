//! An MC6850 on a board, wired as the HP 58503B wires its ACIA: an
//! 8-bit port on CS8, synchronous to the E clock, RS on ADDR1 (control
//! and status at even address 0, the data registers at 2), its IRQ on
//! level 2 and acknowledged by the autovector.  The CPU master resets
//! it, sets it to divide by 16 and 8N1 with the receive interrupt, and
//! transmits; a character the host sends interrupts it and is echoed
//! back.

use std::env;
use std::fs;
use std::path::PathBuf;
use std::process;

use mc683xx_board::board::Board;
use mc683xx_board::description::Description;
use mc683xx_board::description::Options;
use mc683xx_board::registry::Environment;
use mc683xx_board::signal::Serial;
use mc683xx_board::signal::Signal;
use mc683xx_board::time::Time;
use mc683xx_cpu32::frequency::Frequency;
use mc683xx_mc68331::chip::Mc68331;
use mc683xx_modules::qsm::sci::Frame;

/// The initial stack pointer: the top of the RAM on CS2.
const STACK: u32 = 0x10_fffe;
/// Where the program starts.
const START: u32 = 0x400;
/// The interrupt handler.
const HANDLER: u32 = 0x600;
/// The boot ROM's size.
const ROM_BYTES: usize = 0x1_0000;
/// The level 2 autovector: vector 26 (CPU32RM Table 6-1).
const AUTOVECTOR_2: usize = 26;
/// The SIM's registers with MM set (MC68331UM Table D-1).
const SIM: u32 = 0xff_fa00;
/// CSPAR1 (MC68331UM Table D-1).
const CSPAR1: u32 = SIM + 0x46;
/// CS8 an 8-bit port (%10, MC68331UM D.3.24), CS6, CS7, CS9 and CS10
/// 16-bit selects as out of reset.
const CSPAR1_CS8_8_BIT: u16 = 0x03ef;
/// CSBAR2, CSOR2, CSBAR8, CSOR8 (MC68331UM Table D-1).
const CSBAR2: u32 = SIM + 0x54;
/// CSOR2.
const CSOR2: u32 = SIM + 0x56;
/// CSBAR8.
const CSBAR8: u32 = SIM + 0x6c;
/// CSOR8.
const CSOR8: u32 = SIM + 0x6e;
/// CS8 at $200000, a 64-Kbyte block (SIMRM 7.10.2, Table 7-6).
const ACIA_BAR: u16 = 0x2003;
/// The 58503B's CSOR8: synchronous to the E clock, both bytes, read and
/// write, supervisor and user space (SIMRM 7.10.3).
const ACIA_OR: u16 = 0xb830;
/// CS2 at $100000, a 64-Kbyte block: the RAM.
const RAM_BAR: u16 = 0x1003;
/// Both bytes, read and write, fast termination.
const RAM_OR: u16 = 0x7bb0;
/// The ACIA's control and status registers: RS low.
const CONTROL: u32 = 0x20_0000;
/// Its data registers: RS high, on ADDR1.
const DATA: u32 = CONTROL + 2;
/// Master reset (MC6850, Counter Divide Select Bits).
const MASTER_RESET: u8 = 0x03;
/// Divide by 16, 8N1, RTS low, the receive interrupt enabled.
const RUN_RIE: u8 = 0x95;
/// MOVE.W #imm,(xxx).L (CPU32RM 4, MOVE).
const MOVE_WORD: u16 = 0x33fc;
/// MOVE.B #imm,(xxx).L.
const MOVE_BYTE: u16 = 0x13fc;
/// MOVE.B (xxx).L,D0.
const MOVE_BYTE_TO_D0: u16 = 0x1039;
/// MOVE.B (xxx).L,D1.
const MOVE_BYTE_TO_D1: u16 = 0x1239;
/// MOVE.B D0,(xxx).L.
const MOVE_D0_TO_BYTE: u16 = 0x13c0;
/// MOVE #imm,SR.
const MOVE_TO_SR: u16 = 0x46fc;
/// Supervisor, interrupt mask 0.
const SR_UNMASKED: u16 = 0x2000;
/// BRA.S to itself.
const BRA_SELF: u16 = 0x60fe;
/// RTE.
const RTE: u16 = 0x4e73;
/// NOP.
const NOP: u16 = 0x4e71;

/// The high and low words of `address`.
const fn words(address: u32) -> [u16; 2] {
    [(address >> 16) as u16, address as u16]
}

/// MOVE.W #`value`,`address`.
fn store_word(address: u32, value: u16) -> Vec<u16> {
    [vec![MOVE_WORD, value], words(address).to_vec()].concat()
}

/// MOVE.B #`value`,`address`.
fn store_byte(address: u32, value: u8) -> Vec<u16> {
    [vec![MOVE_BYTE, u16::from(value)], words(address).to_vec()].concat()
}

/// The ROM: the reset vectors, the autovector at the handler, the
/// program and the handler.
fn image() -> Vec<u8> {
    let program: Vec<u16> = [
        store_word(CSBAR2, RAM_BAR),
        store_word(CSOR2, RAM_OR),
        store_word(CSPAR1, CSPAR1_CS8_8_BIT),
        store_word(CSBAR8, ACIA_BAR),
        store_word(CSOR8, ACIA_OR),
        store_byte(CONTROL, MASTER_RESET),
        store_byte(CONTROL, RUN_RIE),
        store_byte(DATA, b'H'),
        vec![MOVE_TO_SR, SR_UNMASKED, BRA_SELF],
    ]
    .concat();
    // the status into D1, the character into D0 and back out
    let handler: Vec<u16> = [
        vec![MOVE_BYTE_TO_D1],
        words(CONTROL).to_vec(),
        vec![MOVE_BYTE_TO_D0],
        words(DATA).to_vec(),
        vec![MOVE_D0_TO_BYTE],
        words(DATA).to_vec(),
        vec![RTE],
    ]
    .concat();
    let mut image: Vec<u8> = NOP.to_be_bytes().repeat(ROM_BYTES / 2);
    image[0..4].copy_from_slice(&STACK.to_be_bytes());
    image[4..8].copy_from_slice(&START.to_be_bytes());
    let vector = 4 * AUTOVECTOR_2;
    image[vector..vector + 4].copy_from_slice(&HANDLER.to_be_bytes());
    for (at, code) in [(START, program), (HANDLER, handler)] {
        for (index, word) in code.iter().enumerate() {
            let at = at as usize + 2 * index;
            image[at..at + 2].copy_from_slice(&word.to_be_bytes());
        }
    }
    image
}

/// The board, as docs/peripherals.md's example wires the ACIA.
const BOARD: &str = r#"
version = 1

[chip]
type = "mc68331"
autovector = [2]

[[memory]]
name = "rom"
image = "rom.bin"
attach = [{ select = "CSBOOT" }]

[[memory]]
name = "ram"
size = 0x10000
writable = true
attach = [{ select = "CS2" }]

[[peripheral]]
name = "acia"
type = "mc6850"
attach = [{ select = "CS8", lines = [1] }]
irq = 2
reset = []
parameters = { tx_clk_hz = 153_600, rx_clk_hz = 153_600 }

[[net]]
name = "gps-out"
type = "serial"
from = "acia.TxData"
to = ["external.gps"]

[[net]]
name = "gps-in"
type = "serial"
from = "external.gps"
to = ["acia.RxData"]
"#;

/// The board, its ROM in a scratch directory.
fn board() -> Board<Mc68331> {
    let directory: PathBuf =
        env::temp_dir().join(format!("mc683xx-peripherals-mc6850-{}", process::id()));
    fs::create_dir_all(&directory).unwrap();
    fs::write(directory.join("rom.bin"), image()).unwrap();
    let description = Description::parse(BOARD, &directory, &Options::default()).unwrap();
    let registry = mc683xx_peripherals::registry();
    let board = Board::<Mc68331>::load(&description, &registry, Environment::default()).unwrap();
    fs::remove_dir_all(directory).unwrap();
    board
}

/// The frames that reached the host, with their times.
fn received(board: &mut Board<Mc68331>) -> Vec<(Time, Serial)> {
    board
        .take_received("gps")
        .unwrap()
        .into_iter()
        .filter_map(|(at, signal)| match signal {
            Signal::Serial(serial) => Some((at, serial)),
            _ => None,
        })
        .collect()
}

#[test]
fn the_cpu_transmits_and_echoes_through_the_autovectored_interrupt() {
    let mut board = board();
    // 3 ms: the program sets up and sends 'H', 1.04 ms at 9600 baud
    board.run_until(Time(3_000_000_000)).unwrap();
    let sent = received(&mut board);
    let [(_, serial)] = sent[..] else {
        panic!("one frame: {sent:?}");
    };
    assert_eq!(serial.frame, Frame::Data(u16::from(b'H')));
    assert_eq!(serial.baud, Frequency::from_hz(9600));
    // the host's character interrupts at level 2 through the
    // autovector; the handler reads the status and echoes the character
    let sent_at = board.now();
    board
        .send(
            "gps",
            Signal::Serial(Serial::new(
                Frame::Data(u16::from(b'x')),
                Frequency::from_hz(9600),
                10,
            )),
        )
        .unwrap();
    board.run_until(sent_at + 3_000_000_000).unwrap();
    let echoed = received(&mut board);
    let [(at, serial)] = echoed[..] else {
        panic!("one frame: {echoed:?}");
    };
    assert_eq!(serial.frame, Frame::Data(u16::from(b'x')));
    // after the receiver's nine and a half bits
    assert!(at.since(sent_at) > 989_000_000, "{at:?} from {sent_at:?}");
    let data = &board.chip().cpu().registers.data;
    assert_eq!(data[0] & 0xff, u32::from(b'x'));
    // RDRF, TDRE and IRQ when the handler read the status
    assert_eq!(data[1] & 0xff, 0x83);
}
