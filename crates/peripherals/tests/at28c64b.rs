//! An AT28C64B on a board loaded from a file: the CPU writes a byte,
//! polls it until its write cycle ends, and the byte reaches the peripheral's
//! file.

use std::env;
use std::fs;
use std::path::PathBuf;
use std::process;

use mc683xx_board::board::Board;

use mc683xx_board::description::Description;
use mc683xx_board::description::Options;
use mc683xx_board::registry::Environment;
use mc683xx_board::registry::Registry;
use mc683xx_board::time::Time;
use mc683xx_mc68331::chip::Mc68331;

/// The initial stack pointer and program counter in the image.
const STACK: u32 = 0x10_fffe;
/// Where the program starts.
const START: u32 = 0x400;
/// The boot ROM's size.
const ROM_BYTES: usize = 0x1_0000;
/// The SIM's registers with MM set (MC68331UM Table D-1).
const SIM: u32 = 0xff_fa00;
/// SYPCR, the low byte of its word (MC68331UM D.3.12): written 0, SWE
/// clear, the software watchdog off for the run.
const SYPCR: u32 = SIM + 0x20;
/// CSBAR1 and CSOR1 (MC68331UM Table D-3).
const CSBAR1: u32 = SIM + 0x50;
/// CSOR1.
const CSOR1: u32 = SIM + 0x52;
/// CS1 at $200000, a 64-Kbyte block (SIMRM 7.10.2, Table 7-6).
const CS1_BAR: u16 = 0x2003;
/// Both bytes, read and write, fast termination, supervisor and user
/// space (SIMRM 7.10.3).
const CS_FAST: u16 = 0x7bb0;
/// The cell the program writes, in CS1's window.
const CELL: u32 = 0x20_0123;
/// What it writes there.
const VALUE: u16 = 0x5a;
/// The peripheral's tWC at most, and the board's (AT28C64B datasheet, Table
/// 6-4).
const WRITE_CYCLE_PS: u64 = 10_000_000_000;

/// MOVE.W #imm,(xxx).L (CPU32RM 4, MOVE).
const MOVE_WORD_TO_ABSOLUTE: u16 = 0x33fc;
/// MOVE.B #imm,(xxx).L.
const MOVE_BYTE_TO_ABSOLUTE: u16 = 0x13fc;
/// MOVE.B (xxx).L,D0.
const MOVE_BYTE_TO_D0: u16 = 0x1039;
/// ADDQ.L #1,D1 (CPU32RM 4, ADDQ).
const ADDQ_ONE_D1: u16 = 0x5281;
/// CMPI.B #imm,D0 (CPU32RM 4, CMPI).
const CMPI_BYTE_D0: u16 = 0x0c00;
/// BNE.S back over the six words before it (CPU32RM 4, Bcc).
const BNE_BACK_SIX: u16 = 0x66f2;
/// BRA.S to itself.
const BRA_SELF: u16 = 0x60fe;
/// NOP.
const NOP: u16 = 0x4e71;

/// The high and low words of `address`.
const fn words(address: u32) -> [u16; 2] {
    [(address >> 16) as u16, address as u16]
}

/// The program: stop the watchdog, map CS1, write `VALUE` to `CELL`, then read it back
/// into D0, counting reads in D1, until it reads `VALUE`.
fn program() -> Vec<u16> {
    [
        &[MOVE_WORD_TO_ABSOLUTE, 0][..],
        &words(SYPCR),
        &[MOVE_WORD_TO_ABSOLUTE, CS1_BAR],
        &words(CSBAR1),
        &[MOVE_WORD_TO_ABSOLUTE, CS_FAST],
        &words(CSOR1),
        &[MOVE_BYTE_TO_ABSOLUTE, VALUE],
        &words(CELL),
        &[ADDQ_ONE_D1, MOVE_BYTE_TO_D0],
        &words(CELL),
        &[CMPI_BYTE_D0, VALUE, BNE_BACK_SIX, BRA_SELF],
    ]
    .concat()
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

/// The board: the ROM on CSBOOT and the EEPROM on CS1, its cells in
/// `eeprom.bin`.
const BOARD: &str = r#"
version = 1

[chip]
type = "mc68331"
[[memory]]
name = "rom"
image = "rom.bin"
attach = [{ select = "CSBOOT" }]
[[peripheral]]
name = "eeprom"
type = "at28c64b"
attach = [{ select = "CS1", lines = 13 }]
reset = []
parameters = { file = "eeprom.bin" }
"#;

#[test]
fn the_cpu_writes_a_byte_and_polls_it_to_the_end_of_its_write_cycle() {
    let directory: PathBuf =
        env::temp_dir().join(format!("mc683xx-at28c64b-board-{}", process::id()));
    fs::create_dir_all(&directory).unwrap();
    fs::write(directory.join("rom.bin"), image(&program())).unwrap();
    let description = Description::parse(BOARD, &directory, &Options::default()).unwrap();
    let mut registry = Registry::with_builtins();
    mc683xx_peripherals::register(&mut registry).unwrap();
    let mut board =
        Board::<Mc68331>::load(&description, &registry, Environment::default()).unwrap();

    board.run_until(Time(WRITE_CYCLE_PS / 2)).unwrap();
    let data = board.chip().cpu().registers.data;
    assert_ne!(data[0] & 0xff, u32::from(VALUE), "polling: {:#x}", data[0]);
    let held = fs::read(directory.join("eeprom.bin")).unwrap();
    assert_eq!(held[(CELL & 0x1fff) as usize], 0xff, "not yet programmed");

    board.run_until(Time(2 * WRITE_CYCLE_PS)).unwrap();
    let data = board.chip().cpu().registers.data;
    assert_eq!(data[0] & 0xff, u32::from(VALUE), "read back");
    assert!(data[1] > 100, "polled through tWC: {} reads", data[1]);
    let held = fs::read(directory.join("eeprom.bin")).unwrap();
    assert_eq!(
        u16::from(held[(CELL & 0x1fff) as usize]),
        VALUE,
        "written through"
    );
    drop(board);
    fs::remove_dir_all(directory).unwrap();
}
