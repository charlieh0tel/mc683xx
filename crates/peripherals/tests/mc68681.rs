//! An MC68681 on a board: the CPU transmits through channel A to the
//! run-time endpoint, and a character the host sends interrupts it, the
//! DUART's IVR its vector, and is echoed back.  The DUART is wired as
//! docs/peripherals.md shows: the lower byte of a 16-bit port through
//! DSACK1, RS1-RS4 on ADDR1-ADDR4, its acknowledge through CS10.

use std::env;
use std::fs;
use std::path::PathBuf;
use std::process;

use mc683xx_board::board::Board;

use mc683xx_board::description::Description;
use mc683xx_board::description::Options;
use mc683xx_board::registry::Environment;
use mc683xx_board::registry::Registry;
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
/// The vector the DUART's IVR gives: the first user vector (CPU32RM
/// Table 6-1).
const VECTOR: u8 = 0x40;
/// The SIM's registers with MM set (MC68331UM Table D-1).
const SIM: u32 = 0xff_fa00;
/// PEPAR (MC68331UM Table D-1).
const PEPAR: u32 = SIM + 0x16;
/// PE1 DSACK1, PE0 a port pin, PE4 AVEC, PE5 AS (SIMRM 9.1).
const PEPAR_DSACK1_ONLY: u16 = 0x0032;
/// CSBAR1, CSOR1, CSBAR2, CSOR2, CSBAR10, CSOR10 (MC68331UM Table D-3).
const CSBAR1: u32 = SIM + 0x50;
/// CSOR1.
const CSOR1: u32 = SIM + 0x52;
/// CSBAR2.
const CSBAR2: u32 = SIM + 0x54;
/// CSOR2.
const CSOR2: u32 = SIM + 0x56;
/// CSBAR10.
const CSBAR10: u32 = SIM + 0x74;
/// CSOR10.
const CSOR10: u32 = SIM + 0x76;
/// CS1 at $200000, a 64-Kbyte block (SIMRM 7.10.2, Table 7-6).
const DUART_BAR: u16 = 0x2003;
/// Both bytes, read and write, DSACK external, supervisor and user
/// space (SIMRM 7.10.3).
const DUART_OR: u16 = 0x7bf0;
/// CS2 at $100000, a 64-Kbyte block: the RAM.
const RAM_BAR: u16 = 0x1003;
/// Both bytes, read and write, fast termination.
const RAM_OR: u16 = 0x7bb0;
/// CS10 at $FFF800, 2 Kbytes: CPU space, where acknowledges are.
const IACK_BAR: u16 = 0xfff8;
/// The lower byte, reads, DSACK external, CPU space, IPL 2 (SIMRM
/// 7.10.3, 7.8.1).
const IACK_OR: u16 = 0x2bc4;
/// The DUART's window.
const DUART: u32 = 0x20_0000;
/// The DUART's register `n`: the odd byte of word `n`, RS1-RS4 on
/// ADDR1-ADDR4.
const fn register(n: u32) -> u32 {
    DUART + 2 * n + 1
}
/// CRA, MR1A/MR2A, CSRA, the receive and transmit buffers, ACR, IMR, IVR
/// (MC68681 Table 4-1).
const CRA: u32 = register(2);
/// MR1A, MR2A.
const MRA: u32 = register(0);
/// CSRA.
const CSRA: u32 = register(1);
/// RBA and TBA.
const RBA: u32 = register(3);
/// ACR.
const ACR: u32 = register(4);
/// IMR.
const IMR: u32 = register(5);
/// IVR.
const IVR: u32 = register(12);
/// MOVE.W #imm,(xxx).L (CPU32RM 4, MOVE).
const MOVE_WORD: u16 = 0x33fc;
/// MOVE.B #imm,(xxx).L.
const MOVE_BYTE: u16 = 0x13fc;
/// MOVE.B (xxx).L,D0.
const MOVE_BYTE_TO_D0: u16 = 0x1039;
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

/// The ROM: the reset vectors, the DUART's vector at the handler, the
/// program and the handler.
fn image() -> Vec<u8> {
    let program: Vec<u16> = [
        store_word(CSBAR2, RAM_BAR),
        store_word(CSOR2, RAM_OR),
        store_word(PEPAR, PEPAR_DSACK1_ONLY),
        store_word(CSBAR1, DUART_BAR),
        store_word(CSOR1, DUART_OR),
        store_word(CSBAR10, IACK_BAR),
        store_word(CSOR10, IACK_OR),
        // reset the mode register pointer; 8N1; 38.4k both ways in set 1
        store_byte(CRA, 0x10),
        store_byte(MRA, 0x13),
        store_byte(MRA, 0x07),
        store_byte(CSRA, 0xcc),
        store_byte(ACR, 0x00),
        // a word: the lower lane carries its low byte to the IVR
        store_word(IVR - 1, u16::from(VECTOR)),
        // RxRDYA's interrupt; both enabled; a character out
        store_byte(IMR, 0x02),
        store_byte(CRA, 0x05),
        store_byte(RBA, b'H'),
        vec![MOVE_TO_SR, SR_UNMASKED, BRA_SELF],
    ]
    .concat();
    let handler: Vec<u16> = [
        vec![MOVE_BYTE_TO_D0],
        words(RBA).to_vec(),
        vec![MOVE_D0_TO_BYTE],
        words(RBA).to_vec(),
        vec![RTE],
    ]
    .concat();
    let mut image: Vec<u8> = NOP.to_be_bytes().repeat(ROM_BYTES / 2);
    image[0..4].copy_from_slice(&STACK.to_be_bytes());
    image[4..8].copy_from_slice(&START.to_be_bytes());
    let vector = 4 * usize::from(VECTOR);
    image[vector..vector + 4].copy_from_slice(&HANDLER.to_be_bytes());
    for (at, code) in [(START, program), (HANDLER, handler)] {
        for (index, word) in code.iter().enumerate() {
            let at = at as usize + 2 * index;
            image[at..at + 2].copy_from_slice(&word.to_be_bytes());
        }
    }
    image
}

/// The board, as docs/peripherals.md's example wires the DUART.
const BOARD: &str = r#"
version = 1

[chip]
type = "mc68331"

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
name = "duart"
type = "mc68681"
attach = [
  { select = "CS1", lane = "lower", lines = [1, 2, 3, 4] },
  { select = "CS10", role = "iack", lane = "lower" },
]
irq = 2
dsack = "DSACK1"

[[net]]
name = "console-out"
type = "serial"
from = "duart.TxDA"
to = ["external.console"]

[[net]]
name = "console-in"
type = "serial"
from = "external.console"
to = ["duart.RxDA"]
"#;

/// The board, its ROM in a scratch directory.
fn board() -> Board<Mc68331> {
    let directory: PathBuf =
        env::temp_dir().join(format!("mc683xx-peripherals-mc68681-{}", process::id()));
    fs::create_dir_all(&directory).unwrap();
    fs::write(directory.join("rom.bin"), image()).unwrap();
    let description = Description::parse(BOARD, &directory, &Options::default()).unwrap();
    let mut registry = Registry::with_builtins();
    mc683xx_peripherals::register(&mut registry).unwrap();
    let board = Board::<Mc68331>::load(&description, &registry, Environment::default()).unwrap();
    fs::remove_dir_all(directory).unwrap();
    board
}

/// The frames that reached the console, with their times.
fn received(board: &mut Board<Mc68331>) -> Vec<(Time, Serial)> {
    board
        .take_received("console")
        .unwrap()
        .into_iter()
        .filter_map(|(at, signal)| match signal {
            Signal::Serial(serial) => Some((at, serial)),
            _ => None,
        })
        .collect()
}

#[test]
fn the_cpu_transmits_receives_and_takes_the_duarts_vectored_interrupt() {
    let mut board = board();
    // 2 ms: the program sets up and sends 'H', 260 microseconds at
    // 38.4k
    board.run_until(Time(2_000_000_000)).unwrap();
    let sent = received(&mut board);
    let [(_, serial)] = sent[..] else {
        panic!("one frame: {sent:?}");
    };
    assert_eq!(serial.frame, Frame::Data(u16::from(b'H')));
    assert_eq!(serial.baud, Frequency::from_hz(38_400));
    // the host's character interrupts at level 2; the acknowledge reads
    // the IVR through CS10, and the handler echoes what it reads
    let sent_at = board.now();
    board
        .send(
            "console",
            Signal::Serial(Serial::untimed(Frame::Data(u16::from(b'x')))),
        )
        .unwrap();
    board.run_until(sent_at + 2_000_000_000).unwrap();
    let echoed = received(&mut board);
    let [(at, serial)] = echoed[..] else {
        panic!("one frame: {echoed:?}");
    };
    assert_eq!(serial.frame, Frame::Data(u16::from(b'x')));
    // after a character time of reception and the handler's start
    assert!(at.since(sent_at) > 250_000_000, "{at:?} from {sent_at:?}");
    assert_eq!(board.chip().cpu().registers.data[0] & 0xff, u32::from(b'x'));
}
