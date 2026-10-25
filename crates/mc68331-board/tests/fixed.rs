//! A board file's memories at fixed addresses, on no chip select:
//! emulator-only.

use std::env;
use std::fs;
use std::process;

use mc683xx_board::board::Board;

use mc683xx_board::description::Description;
use mc683xx_board::description::Options;
use mc683xx_board::registry::Environment;
use mc683xx_board::registry::Registry;
use mc683xx_board::time::Time;
use mc683xx_mc68331::chip::Mc68331;

/// The initial stack pointer and program counter in the boot ROM.
const STACK: u32 = 0x10_fffe;
/// Where the boot ROM's program starts.
const START: u32 = 0x400;
/// The boot ROM's size.
const ROM_BYTES: usize = 0x1_0000;
/// The fixed code's base.
const CODE: u32 = 0x60_0000;
/// The fixed data's base.
const DATA: u32 = 0x6f_0000;
/// JMP (xxx).L (CPU32RM 4, JMP).
const JUMP_ABSOLUTE: u16 = 0x4ef9;
/// MOVE.W #imm,(xxx).L (CPU32RM 4, MOVE).
const MOVE_WORD_TO_ABSOLUTE: u16 = 0x33fc;
/// BRA.S to itself (CPU32RM 4, Bcc).
const BRANCH_TO_SELF: u16 = 0x60fe;
/// NOP (CPU32RM 4, NOP).
const NOP: u16 = 0x4e71;

/// `words` as big-endian bytes from `at` in `image`.
fn patch(image: &mut [u8], at: usize, words: &[u16]) {
    for (index, word) in words.iter().enumerate() {
        image[at + 2 * index..at + 2 * index + 2].copy_from_slice(&word.to_be_bytes());
    }
}

#[test]
fn a_board_files_fixed_memory_runs_the_boot_codes_jump_and_reloads() {
    let mut rom = NOP.to_be_bytes().repeat(ROM_BYTES / 2);
    rom[0..4].copy_from_slice(&STACK.to_be_bytes());
    rom[4..8].copy_from_slice(&START.to_be_bytes());
    patch(
        &mut rom,
        START as usize,
        &[JUMP_ABSOLUTE, (CODE >> 16) as u16, CODE as u16],
    );
    let mut code = vec![0; 0x800];
    patch(
        &mut code,
        0,
        &[
            MOVE_WORD_TO_ABSOLUTE,
            0x1234,
            (DATA >> 16) as u16,
            DATA as u16,
            BRANCH_TO_SELF,
        ],
    );
    let directory = env::temp_dir().join(format!("mc683xx-board-fixed-{}", process::id()));
    fs::create_dir_all(&directory).unwrap();
    fs::write(directory.join("rom.bin"), rom).unwrap();
    fs::write(directory.join("code.bin"), code).unwrap();
    let text = r#"
version = 1

[chip]
type = "mc68331"
[[memory]]
name = "rom"
image = "rom.bin"
attach = [{ select = "CSBOOT" }]
[[memory]]
name = "code"
note = "emulator-only: no select decodes it"
image = "code.bin"
base = 0x600000
[[memory]]
name = "data"
size = 0x800
fill = 0x5a
writable = true
base = 0x6f0000
reload = ["external"]
"#;
    let description = Description::parse(text, &directory, &Options::default()).unwrap();
    let mut board = Board::<Mc68331>::load(
        &description,
        &Registry::with_builtins(),
        Environment::default(),
    )
    .unwrap();
    fs::remove_dir_all(directory).unwrap();
    board.run_until(Time(100_000_000)).unwrap();
    assert_eq!(board.memory("data").unwrap()[0..2], [0x12, 0x34]);
    let pc = board.chip().cpu().registers.pc;
    assert!((CODE..CODE + 0x800).contains(&pc), "{pc:#x}");
    board.chip_mut().reset();
    assert_eq!(board.memory("data").unwrap()[0..2], [0x5a, 0x5a]);
}
