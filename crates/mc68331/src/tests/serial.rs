//! BDM's serial interface: `Mc68331::bdm_exchange` (CPU32RM 7.2.7,
//! 7.2.8).

use mc683xx_cpu32::cycles::Cycles;
use mc683xx_modules::chip_select::ChipSelect;
use mc683xx_target::Cpu32Target;
use mc683xx_target::address::Address;
use mc683xx_target::entry::Entry;
use mc683xx_target::function_code::FunctionCode;
use mc683xx_target::register::Register;
use mc683xx_target::register::SystemRegister;
use mc683xx_target::size::Size;

use super::BRANCH_TO_SELF;
use super::ROM_BYTES;
use super::START;
use super::image;
use crate::chip::Builder;
use crate::chip::Mc68331;
use crate::debug::serial::BUS_ERROR;
use crate::debug::serial::COMPLETE;
use crate::debug::serial::ILLEGAL;
use crate::debug::serial::NOT_IN_BACKGROUND;
use crate::debug::serial::NOT_READY;
use crate::debug::serial::STATUS;

/// RDREG D0 (CPU32RM 7.2.8.4); the register code is added.
const RDREG: u16 = 0x2180;
/// WDREG D0 (7.2.8.5).
const WDREG: u16 = 0x2080;
/// RSREG RPC (7.2.8.6); the select code is added.
const RSREG: u16 = 0x2580;
/// WSREG RPC (7.2.8.7).
const WSREG: u16 = 0x2480;
/// READ.W (7.2.8.8).
const READ_WORD: u16 = 0x1940;
/// READ.L.
const READ_LONG: u16 = 0x1980;
/// READ.B.
const READ_BYTE: u16 = 0x1900;
/// WRITE.W (7.2.8.9).
const WRITE_WORD: u16 = 0x1840;
/// WRITE.B.
const WRITE_BYTE: u16 = 0x1800;
/// DUMP.W (7.2.8.10).
const DUMP_WORD: u16 = 0x1D40;
/// FILL.W (7.2.8.11).
const FILL_WORD: u16 = 0x1C40;
/// GO (7.2.8.12).
const GO: u16 = 0x0C00;
/// CALL (7.2.8.13).
const CALL: u16 = 0x0800;
/// RST (7.2.8.14).
const RST: u16 = 0x0400;
/// NOP (7.2.8.15).
const NOP: u16 = 0x0000;
/// MOVEQ #1,D0.
const MOVEQ_1_D0: u16 = 0x7001;
/// RAM at a fixed address, on no select.
const RAM: u32 = 0x20_0000;
/// Bytes of RAM: one page.
const RAM_BYTES: usize = 2048;
/// RSREG and WSREG's select code for ATEMP.
const ATEMP: u16 = 0x8;

/// A chip booting `program` from CSBOOT, with RAM at `RAM`, in
/// background mode out of a reset with BKPT, SFC and DFC supervisor
/// data.
fn chip(program: &[u16]) -> Mc68331 {
    let mut builder = Builder::default();
    builder
        .memory(image(program, ROM_BYTES), false, &[ChipSelect::Boot])
        .unwrap();
    builder
        .fixed_memory(Address(RAM), vec![0; RAM_BYTES], true)
        .unwrap();
    let mut chip = builder.build();
    chip.reset_halted().unwrap();
    let data = u32::from(FunctionCode::SUPERVISOR_DATA.bits());
    chip.write_system_register(SystemRegister::Sfc, data)
        .unwrap();
    chip.write_system_register(SystemRegister::Dfc, data)
        .unwrap();
    chip
}

/// What each of `words` shifts out.
fn exchange(chip: &mut Mc68331, words: &[u16]) -> Vec<u32> {
    words
        .iter()
        .map(|&word| chip.bdm_exchange(u32::from(word)))
        .collect()
}

/// `address`'s high and low words.
const fn halves(address: u32) -> [u16; 2] {
    [(address >> 16) as u16, address as u16]
}

#[test]
fn rdreg_returns_a_long_high_word_first_then_the_next_command_runs() {
    let mut chip = chip(&[BRANCH_TO_SELF]);
    chip.write_register(Register::D0, 0x1234_5678).unwrap();
    chip.write_register(Register::A7, 0x0002_0004).unwrap();
    assert_eq!(
        exchange(&mut chip, &[RDREG, NOP, NOP, NOP]),
        [NOT_READY, 0x1234, 0x5678, COMPLETE],
        "the word sent with the high word is not a command"
    );
    assert_eq!(
        exchange(&mut chip, &[RDREG | 0xF, NOP, NOP]),
        [COMPLETE, 2, 4],
        "RAREG A7"
    );
    // the control bit is ignored (Table 7-3)
    assert_eq!(
        chip.bdm_exchange(STATUS | u32::from(RDREG)),
        COMPLETE,
        "the last NOP's"
    );
    assert_eq!(exchange(&mut chip, &[NOP, NOP]), [0x1234, 0x5678]);
}

#[test]
fn wdreg_and_wsreg_answer_not_ready_for_each_operand() {
    let mut chip = chip(&[BRANCH_TO_SELF]);
    assert_eq!(
        exchange(&mut chip, &[WDREG | 0xA, 0xdead, 0xbeef, NOP]),
        [NOT_READY, NOT_READY, NOT_READY, COMPLETE]
    );
    assert_eq!(chip.read_register(Register::A2), Ok(0xdead_beef));
    assert_eq!(
        exchange(&mut chip, &[WSREG | ATEMP, 0, 7, RSREG | ATEMP, NOP, NOP]),
        [NOT_READY, NOT_READY, NOT_READY, COMPLETE, 0, 7]
    );
    // RSREG RPC
    assert_eq!(
        exchange(&mut chip, &[RSREG, NOP, NOP]),
        [COMPLETE, 0, START]
    );
}

#[test]
fn an_illegal_command_answers_illegal_then_not_ready() {
    let mut chip = chip(&[BRANCH_TO_SELF]);
    chip.write_register(Register::D0, 0x1234_5678).unwrap();
    // the RDREG sent with ILLEGAL is not received (Figure 7-10)
    assert_eq!(
        exchange(&mut chip, &[0xFFFF, RDREG, NOP, NOP]),
        [NOT_READY, ILLEGAL, NOT_READY, COMPLETE]
    );
    for illegal in [
        RSREG | 2,        // no select code 2
        READ_BYTE | 0xC0, // size 11
        NOP | 1,          // a bit the format gives as 0
        RDREG | 0x0040,   // RDREG with size 11
        GO | 0x0080,      // GO with a size
    ] {
        assert_eq!(
            exchange(&mut chip, &[illegal, NOP, NOP]),
            [COMPLETE, ILLEGAL, NOT_READY],
            "{illegal:#06x}"
        );
        assert_eq!(chip.bdm_exchange(u32::from(NOP)), COMPLETE);
    }
}

#[test]
fn memory_through_read_write_dump_and_fill() {
    let mut chip = chip(&[BRANCH_TO_SELF]);
    let [high, low] = halves(RAM);
    assert_eq!(
        exchange(
            &mut chip,
            &[WRITE_WORD, high, low, 0xabcd, FILL_WORD, 0x1234]
        ),
        [
            NOT_READY, NOT_READY, NOT_READY, NOT_READY, COMPLETE, NOT_READY
        ]
    );
    assert_eq!(
        exchange(&mut chip, &[READ_WORD, high, low, DUMP_WORD, NOP, NOP]),
        [COMPLETE, NOT_READY, NOT_READY, 0xabcd, 0x1234, COMPLETE]
    );
    assert_eq!(
        exchange(&mut chip, &[READ_LONG, high, low, NOP, NOP]),
        [COMPLETE, NOT_READY, NOT_READY, 0xabcd, 0x1234]
    );
    let [_, byte] = halves(RAM + 4);
    assert_eq!(
        exchange(
            &mut chip,
            &[WRITE_BYTE, high, byte, 0xff77, READ_BYTE, high, byte, NOP]
        ),
        [
            COMPLETE, NOT_READY, NOT_READY, NOT_READY, COMPLETE, NOT_READY, NOT_READY, 0x0077
        ],
        "a byte is the low byte of a word, the high one clear"
    );
    let mut back = [0; 6];
    chip.read_memory(
        Address(RAM),
        FunctionCode::SUPERVISOR_DATA,
        Size::Byte,
        &mut back,
    )
    .unwrap();
    assert_eq!(back, [0xab, 0xcd, 0x12, 0x34, 0x77, 0]);
}

#[test]
fn a_bus_or_address_error_answers_berr_then_not_ready() {
    let mut chip = chip(&[BRANCH_TO_SELF]);
    let [high, low] = halves(RAM + 1);
    assert_eq!(
        exchange(&mut chip, &[READ_WORD, high, low, RDREG, NOP, NOP]),
        [
            NOT_READY, NOT_READY, NOT_READY, BUS_ERROR, NOT_READY, COMPLETE
        ]
    );
    assert_eq!(
        exchange(&mut chip, &[WRITE_WORD, high, low, 0, NOP, NOP]),
        [
            COMPLETE, NOT_READY, NOT_READY, NOT_READY, BUS_ERROR, NOT_READY
        ]
    );
}

#[test]
fn go_leaves_background_mode_and_a_transfer_then_is_a_breakpoint() {
    let mut chip = chip(&[MOVEQ_1_D0, BRANCH_TO_SELF]);
    assert_eq!(chip.bdm_exchange(u32::from(GO)), NOT_READY);
    assert_eq!(Cpu32Target::halted(&mut chip), Ok(None));
    chip.run_for(Cycles(1000));
    assert_eq!(chip.bdm_exchange(u32::from(RDREG)), NOT_IN_BACKGROUND);
    assert_eq!(Cpu32Target::halted(&mut chip), Ok(Some(Entry::Breakpoint)));
    assert_eq!(
        exchange(&mut chip, &[RDREG, NOP, NOP]),
        [NOT_READY, 0, 1],
        "the RDREG clocked while running was not received"
    );
}

#[test]
fn call_stacks_rpc_or_answers_berr_and_stays() {
    let mut chip = chip(&[BRANCH_TO_SELF]);
    let [high, low] = halves(START);
    chip.write_register(Register::A7, RAM + 1).unwrap();
    assert_eq!(
        exchange(&mut chip, &[CALL, high, low, NOP, NOP]),
        [NOT_READY, NOT_READY, NOT_READY, BUS_ERROR, NOT_READY]
    );
    assert!(Cpu32Target::halted(&mut chip).unwrap().is_some());
    assert_eq!(
        chip.read_register(Register::A7),
        Ok(RAM + 1),
        "SP as it was"
    );
    chip.write_register(Register::A7, RAM + 0x100).unwrap();
    chip.write_system_register(SystemRegister::Rpc, START + 2)
        .unwrap();
    assert_eq!(
        exchange(&mut chip, &[CALL, high, low]),
        [NOT_READY, NOT_READY, NOT_READY]
    );
    assert_eq!(Cpu32Target::halted(&mut chip), Ok(None));
    assert_eq!(
        chip.bus_read(
            Address(RAM + 0xFC),
            Size::Long,
            FunctionCode::SUPERVISOR_DATA
        ),
        Ok(START + 2)
    );
}

#[test]
fn rst_completes_with_the_cpu_and_the_modules_frozen() {
    let mut chip = chip(&[BRANCH_TO_SELF]);
    chip.write_register(Register::D0, 5).unwrap();
    assert_eq!(exchange(&mut chip, &[RST, NOP]), [NOT_READY, COMPLETE]);
    assert!(Cpu32Target::halted(&mut chip).unwrap().is_some());
    assert!(chip.board.frozen);
    assert_eq!(chip.read_register(Register::D0), Ok(5), "the CPU not reset");
}

#[test]
fn a_debugger_s_own_command_drops_one_in_progress() {
    let mut chip = chip(&[BRANCH_TO_SELF]);
    assert_eq!(exchange(&mut chip, &[WDREG | 1, 0x1111]), [NOT_READY; 2]);
    chip.read_register(Register::D0).unwrap();
    assert_eq!(
        exchange(&mut chip, &[NOP, NOP]),
        [NOT_READY, COMPLETE],
        "the NOP is a command, not WDREG's low word"
    );
    assert_eq!(chip.read_register(Register::D1), Ok(0));
}

#[test]
fn a_snapshot_keeps_a_command_in_progress() {
    let mut chip = chip(&[BRANCH_TO_SELF]);
    exchange(&mut chip, &[WDREG | 1, 0x1111]);
    let snapshot = chip.save().unwrap();
    chip.read_register(Register::D0).unwrap();
    chip.restore(&snapshot).unwrap();
    assert_eq!(exchange(&mut chip, &[0x2222, NOP]), [NOT_READY, COMPLETE]);
    assert_eq!(chip.read_register(Register::D1), Ok(0x1111_2222));
}
