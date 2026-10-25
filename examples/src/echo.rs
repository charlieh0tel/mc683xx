//! `boards/echo.bin`, the example board's firmware: it maps RAM, stops
//! the watchdog, starts the SCI and echoes each frame it receives.
//!
//! `chatter_image` is the same board's firmware that sends for ever,
//! for a console nobody reads.
//! `cargo run -p mc683xx-examples --bin echo-image` (`make echo-image`)
//! writes it, and a test checks the committed file is what `image`
//! builds.

use crate::firmware::BEQ_SHORT;
use crate::firmware::BRA_SHORT;
use crate::firmware::QSM;
use crate::firmware::load_word;
use crate::firmware::prologue;
use crate::firmware::rom;
use crate::firmware::short_branch;
use crate::firmware::store_byte;
use crate::firmware::store_d0_byte;
use crate::firmware::store_word;

/// SCCR0 (QSMRM 5.2.1, MC68331UM D.4.4).
const SCCR0: u32 = QSM + 0x08;
/// SCCR1 (QSMRM 5.2.2, MC68331UM D.4.5).
const SCCR1: u32 = QSM + 0x0a;
/// SCSR (QSMRM 5.2.3, MC68331UM D.4.6).
const SCSR: u32 = QSM + 0x0c;
/// SCDR (QSMRM 5.2.4, MC68331UM D.4.7); its low byte holds the frame.
const SCDR: u32 = QSM + 0x0e;
/// SCCR0's SCBR: the fastest rate.
const SCBR: u16 = 1;
/// SCCR1's TE and RE (QSMRM 5.2.2).
const TE_RE: u16 = 0x000c;
/// SCSR's RDRF bit (QSMRM 5.2.3).
const RDRF: u16 = 6;
/// SCSR's TDRE bit (QSMRM 5.2.3).
const TDRE: u16 = 8;
/// What `chatter_program` sends.
const CHATTER: u8 = b'A';
/// BTST #imm,D0 (CPU32RM 4, BTST), the bit number to follow.
const BTST_IMMEDIATE_D0: u16 = 0x0800;

/// The setup: the prologue, then the SCI at its fastest rate,
/// transmitter and receiver on.
fn setup() -> Vec<u16> {
    [
        prologue(),
        store_word(SCCR0, SCBR).to_vec(),
        store_word(SCCR1, TE_RE).to_vec(),
    ]
    .concat()
}

/// A wait for SCSR bit `bit` to set, before its BEQ.
fn wait_for(bit: u16) -> Vec<u16> {
    [load_word(SCSR, 0).as_slice(), &[BTST_IMMEDIATE_D0, bit]].concat()
}

/// The program: the setup, then a loop that waits for RDRF and writes
/// the frame back.
#[must_use]
pub fn program() -> Vec<u16> {
    let setup = setup();
    let wait = wait_for(RDRF);
    // Back over `wait` and the BEQ itself.
    let wait_back = wait.len() as u16 + 1;
    let echo = [load_word(SCDR, 0).as_slice(), &store_d0_byte(SCDR + 1)].concat();
    let loop_back = wait_back + echo.len() as u16 + 1;
    [
        setup,
        wait,
        vec![short_branch(BEQ_SHORT, wait_back)],
        echo,
        vec![short_branch(BRA_SHORT, loop_back)],
    ]
    .concat()
}

/// The ROM image: `program` in `firmware::rom`.
#[must_use]
pub fn image() -> Vec<u8> {
    rom(&program())
}

/// A program that sends `CHATTER` each time TDRE sets, for ever: the
/// setup, then a loop that waits for TDRE and writes the frame.
#[must_use]
pub fn chatter_program() -> Vec<u16> {
    let wait = wait_for(TDRE);
    let wait_back = wait.len() as u16 + 1;
    let send = store_byte(SCDR + 1, CHATTER);
    let loop_back = wait_back + send.len() as u16 + 1;
    [
        setup(),
        wait,
        vec![short_branch(BEQ_SHORT, wait_back)],
        send.to_vec(),
        vec![short_branch(BRA_SHORT, loop_back)],
    ]
    .concat()
}

/// `chatter_program`'s ROM image.
#[must_use]
pub fn chatter_image() -> Vec<u8> {
    rom(&chatter_program())
}
