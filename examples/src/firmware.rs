//! Firmware for the examples: a boot ROM image and the handful of
//! CPU32 instructions (CPU32RM 4) their programs are made of, written
//! as words.
//!
//! Real hosts load real images; these keep each example
//! self-contained.

use mc683xx_mc68331::chip::Builder;
use mc683xx_mc68331::chip::MemoryId;
use mc683xx_modules::chip_select::ChipSelect;
use mc683xx_modules::chip_select::Number;

/// The SIM's registers with MM set (MC68331UM Table D-1).
pub const SIM: u32 = 0xff_fa00;
/// The QSM's registers with MM set (MC68331UM Table D-1).
pub const QSM: u32 = 0xff_fc00;
/// The GPT's registers with MM set (MC68331UM Table D-1).
pub const GPT: u32 = 0xff_f900;
/// SYPCR's word: zero stops the watchdog (MC68331UM 4.2.10, D.3.12).
pub const SYPCR: u32 = SIM + 0x20;

/// The boot ROM's size.
pub const ROM_BYTES: usize = 64 << 10;
/// Where `booting` puts the RAM on CS0.
pub const RAM_BASE: u32 = 0x10_0000;
/// The RAM's size.
pub const RAM_BYTES: usize = 64 << 10;
/// The initial stack pointer: the RAM's end.
pub const STACK: u32 = RAM_BASE + RAM_BYTES as u32;
/// Where the programs start.
pub const START: u32 = 0x400;
/// Where the examples put an exception handler.
pub const HANDLER: u32 = 0x800;

/// CSBARx's base field holds ADDR[23:11] from bit 3 (MC68331UM D.3.25).
const BASE_SHIFT: u32 = 8;
/// CSBARx's BLKSZ for 64 Kbytes (MC68331UM Table 4-21).
pub const BLOCK_64K: u16 = 0b011;
/// CSOR: both bytes, read and write, fast termination, supervisor and
/// user space (MC68331UM D.3.26).
pub const CSOR_FAST: u16 = 0x7bb0;
/// CSOR: as `CSOR_FAST`, but the peripheral asserts DSACK itself (DSACK
/// %1111, SIMRM 7.10.3).
pub const CSOR_EXTERNAL: u16 = 0x7bf0;
/// CSBAR for an interrupt acknowledge select: CPU space type $F is at
/// `$FFFFFx` (SIMRM 7.8.1), in a 2-Kbyte block from `$FFF800`.
pub const CSBAR_ACKNOWLEDGE: u16 = 0xfff8;
/// CSOR for an interrupt acknowledge select: the lower byte, read,
/// external DSACK, CPU space; the level goes in IPL, bits 3:1 (SIMRM
/// 7.8.1, 7.10.3).
const CSOR_ACKNOWLEDGE: u16 = 0x2bc0;
/// CSOR's IPL field's shift.
const IPL_SHIFT: u16 = 1;

/// NOP.
pub const NOP: u16 = 0x4e71;
/// MOVE.W #imm,(xxx).L.
const MOVE_WORD_TO_ABSOLUTE: u16 = 0x33fc;
/// MOVE.B #imm,(xxx).L.
const MOVE_BYTE_TO_ABSOLUTE: u16 = 0x13fc;
/// MOVE.W (xxx).L,Dn; the register is in bits 11:9.
const MOVE_WORD_FROM_ABSOLUTE: u16 = 0x3039;
/// MOVE.B D0,(xxx).L.
const MOVE_BYTE_D0_TO_ABSOLUTE: u16 = 0x13c0;
/// MOVE #imm,SR.
const MOVE_TO_STATUS_REGISTER: u16 = 0x46fc;
/// MOVE.W #imm,D3.
const MOVE_WORD_IMMEDIATE_TO_D3: u16 = 0x363c;
/// DBRA D3, with its displacement word to follow.
const DBRA_D3: u16 = 0x51cb;
/// DBRA's displacement back to itself.
const TO_SELF: u16 = 0xfffe;
/// STOP #imm.
const STOP: u16 = 0x4e72;
/// ANDI.W #imm,D1.
pub const ANDI_WORD_D1: u16 = 0x0241;
/// BEQ.S, its displacement in the low byte.
pub const BEQ_SHORT: u16 = 0x6700;
/// BRA.S, its displacement in the low byte.
pub const BRA_SHORT: u16 = 0x6000;
/// BRA.S to itself: a spin.
pub const BRANCH_TO_SELF: u16 = 0x60fe;
/// The supervisor state with the interrupt mask at 0.
pub const UNMASKED: u16 = 0x2000;

/// CS`number`.
///
/// # Panics
///
/// If there is no such select.
#[must_use]
pub fn cs(number: u8) -> ChipSelect {
    ChipSelect::Numbered(Number::new(number).expect("CS0 to CS10"))
}

/// A ROM of NOPs: the reset vectors (STACK, START) and `program` at
/// `START`.
#[must_use]
pub fn rom(program: &[u16]) -> Vec<u8> {
    let mut image = NOP.to_be_bytes().repeat(ROM_BYTES / 2);
    image[0..4].copy_from_slice(&STACK.to_be_bytes());
    image[4..8].copy_from_slice(&START.to_be_bytes());
    patch(&mut image, START, program);
    image
}

/// Copies `words` into `image` from `address`.
pub fn patch(image: &mut [u8], address: u32, words: &[u16]) {
    for (index, word) in words.iter().enumerate() {
        let at = address as usize + 2 * index;
        image[at..at + 2].copy_from_slice(&word.to_be_bytes());
    }
}

/// Points `vector`'s table entry at `handler` (CPU32RM 6.1.3).
pub fn set_vector(image: &mut [u8], vector: u8, handler: u32) {
    let at = 4 * usize::from(vector);
    image[at..at + 4].copy_from_slice(&handler.to_be_bytes());
}

/// MOVE.W #`value`,`address`.
#[must_use]
pub const fn store_word(address: u32, value: u16) -> [u16; 4] {
    [
        MOVE_WORD_TO_ABSOLUTE,
        value,
        (address >> 16) as u16,
        address as u16,
    ]
}

/// MOVE.B #`value`,`address`.
#[must_use]
pub const fn store_byte(address: u32, value: u8) -> [u16; 4] {
    [
        MOVE_BYTE_TO_ABSOLUTE,
        value as u16,
        (address >> 16) as u16,
        address as u16,
    ]
}

/// MOVE.B D0,`address`.
#[must_use]
pub const fn store_d0_byte(address: u32) -> [u16; 3] {
    [
        MOVE_BYTE_D0_TO_ABSOLUTE,
        (address >> 16) as u16,
        address as u16,
    ]
}

/// MOVE.W `address`,D`register`.
#[must_use]
pub const fn load_word(address: u32, register: u16) -> [u16; 3] {
    [
        MOVE_WORD_FROM_ABSOLUTE | register << 9,
        (address >> 16) as u16,
        address as u16,
    ]
}

/// MOVE #`sr`,SR.
#[must_use]
pub const fn set_sr(sr: u16) -> [u16; 2] {
    [MOVE_TO_STATUS_REGISTER, sr]
}

/// STOP #`sr`.
#[must_use]
pub const fn stop(sr: u16) -> [u16; 2] {
    [STOP, sr]
}

/// A delay: DBRA D3 to itself `count` + 1 times.
#[must_use]
pub const fn delay(count: u16) -> [u16; 4] {
    [MOVE_WORD_IMMEDIATE_TO_D3, count, DBRA_D3, TO_SELF]
}

/// A short branch `opcode` to `words_back` words before the word after
/// it.
#[must_use]
pub const fn short_branch(opcode: u16, words_back: u16) -> u16 {
    opcode | (words_back * 2).wrapping_neg() & 0xff
}

/// CS`number` at `base`, a 64-Kbyte block, with option register
/// `option`: CSBARx and CSORx (MC68331UM Table D-3).
#[must_use]
pub fn select(number: u8, base: u32, option: u16) -> Vec<u16> {
    let bar = SIM + 0x4c + 4 * u32::from(number);
    [
        store_word(bar, (base >> BASE_SHIFT) as u16 | BLOCK_64K),
        store_word(bar + 2, option),
    ]
    .concat()
}

/// CS`number` for the interrupt acknowledge of `level` (SIMRM 7.8.1).
#[must_use]
pub fn acknowledge_select(number: u8, level: u8) -> Vec<u16> {
    let bar = SIM + 0x4c + 4 * u32::from(number);
    [
        store_word(bar, CSBAR_ACKNOWLEDGE),
        store_word(bar + 2, CSOR_ACKNOWLEDGE | u16::from(level) << IPL_SHIFT),
    ]
    .concat()
}

/// A board with `image` as its boot ROM on CSBOOT and RAM on CS0: the
/// builder and the RAM.  The image's program should begin with
/// `prologue()`, which maps the RAM and stops the watchdog.
///
/// # Panics
///
/// If the image is not a whole number of pages.
#[must_use]
pub fn booting(image: Vec<u8>) -> (Builder, MemoryId) {
    let mut builder = Builder::default();
    builder
        .memory(image, false, &[ChipSelect::Boot])
        .expect("a ROM of whole pages");
    let ram = builder
        .memory(vec![0; RAM_BYTES], true, &[cs(0)])
        .expect("CS0 is free");
    (builder, ram)
}

/// What a program on `booting`'s board does first: CS0 at `RAM_BASE`,
/// the watchdog stopped.
#[must_use]
pub fn prologue() -> Vec<u16> {
    [
        select(0, RAM_BASE, CSOR_FAST),
        store_word(SYPCR, 0).to_vec(),
    ]
    .concat()
}
