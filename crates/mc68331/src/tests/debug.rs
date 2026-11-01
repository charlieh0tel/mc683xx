//! The chip under a debugger's control: `Cpu32Target`.

use mc683xx_cpu32::cycles::Cycles;
use mc683xx_cpu32::exception::Vector;
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
use crate::debug::TargetError;

/// BGND (CPU32RM 4.4).
const BGND: u16 = 0x4afa;
/// MOVEQ #1,D0.
const MOVEQ_1_D0: u16 = 0x7001;
/// RAM at a fixed address, on no select.
const RAM: u32 = 0x20_0000;
/// Bytes of RAM: one page.
const RAM_BYTES: usize = 2048;

/// A chip booting `program` from CSBOOT, with RAM at `RAM`.
fn chip(program: &[u16]) -> Mc68331 {
    let mut builder = Builder::default();
    builder
        .memory(image(program, ROM_BYTES), false, &[ChipSelect::Boot])
        .unwrap();
    builder
        .fixed_memory(Address(RAM), vec![0; RAM_BYTES], true)
        .unwrap();
    builder.build()
}

#[test]
fn a_reset_with_bkpt_low_halts_before_the_first_instruction() {
    let mut chip = chip(&[MOVEQ_1_D0, BRANCH_TO_SELF]);
    assert_eq!(chip.reset_halted(), Ok(Entry::Breakpoint));
    assert_eq!(chip.read_system_register(SystemRegister::Rpc), Ok(START));
    assert_eq!(chip.read_system_register(SystemRegister::Pcc), Ok(START));
    assert_eq!(chip.read_system_register(SystemRegister::Atemp), Ok(0));
    chip.run_for(Cycles(1000));
    assert_eq!(chip.read_register(Register::D0), Ok(0), "nothing ran");
    assert_eq!(Cpu32Target::halted(&mut chip), Ok(Some(Entry::Breakpoint)));
}

#[test]
fn registers_and_memory_through_the_target() {
    let mut chip = chip(&[BRANCH_TO_SELF]);
    chip.reset_halted().unwrap();
    chip.write_register(Register::D3, 0x1234_5678).unwrap();
    chip.write_register(Register::A2, 0x0000_abcd).unwrap();
    assert_eq!(chip.read_register(Register::D3), Ok(0x1234_5678));
    assert_eq!(chip.read_register(Register::A2), Ok(0x0000_abcd));
    chip.write_system_register(SystemRegister::Vbr, RAM)
        .unwrap();
    assert_eq!(chip.read_system_register(SystemRegister::Vbr), Ok(RAM));
    chip.write_system_register(SystemRegister::Far, 7).unwrap();
    assert_eq!(
        chip.read_system_register(SystemRegister::Far),
        Ok(0),
        "read only"
    );
    let data = [1, 2, 3, 4, 5, 6, 7, 8];
    for size in [Size::Byte, Size::Word, Size::Long] {
        chip.write_memory(Address(RAM), FunctionCode::SUPERVISOR_DATA, size, &data)
            .unwrap();
        let mut back = [0; 8];
        chip.read_memory(Address(RAM), FunctionCode::SUPERVISOR_DATA, size, &mut back)
            .unwrap();
        assert_eq!(back, data, "{size:?}");
    }
    let mut word = [0; 2];
    chip.read_memory(
        Address(START),
        FunctionCode::SUPERVISOR_PROGRAM,
        Size::Word,
        &mut word,
    )
    .unwrap();
    assert_eq!(word, BRANCH_TO_SELF.to_be_bytes());
    assert!(matches!(
        chip.read_memory(
            Address(RAM),
            FunctionCode::SUPERVISOR_DATA,
            Size::Long,
            &mut [0; 3]
        ),
        Err(TargetError::Length { .. })
    ));
    assert!(matches!(
        chip.read_memory(
            Address(RAM + 1),
            FunctionCode::SUPERVISOR_DATA,
            Size::Word,
            &mut word
        ),
        Err(TargetError::Access { .. })
    ));
}

#[test]
fn go_runs_to_a_bgnd_and_halt_stops_a_running_cpu() {
    let mut chip = chip(&[MOVEQ_1_D0, BGND, BRANCH_TO_SELF]);
    chip.reset_halted().unwrap();
    assert_eq!(chip.go(), Ok(()));
    assert_eq!(Cpu32Target::halted(&mut chip), Ok(None));
    chip.run_for(Cycles(1000));
    assert_eq!(Cpu32Target::halted(&mut chip), Ok(Some(Entry::Background)));
    assert_eq!(chip.read_register(Register::D0), Ok(1));
    assert_eq!(
        chip.read_system_register(SystemRegister::Pcc),
        Ok(START + 2)
    );
    assert_eq!(
        chip.read_system_register(SystemRegister::Rpc),
        Ok(START + 4)
    );
    chip.go().unwrap();
    chip.run_for(Cycles(1000));
    assert_eq!(
        Cpu32Target::halted(&mut chip),
        Ok(None),
        "spinning on the branch"
    );
    assert_eq!(chip.halt(), Ok(Entry::Breakpoint));
    assert_eq!(
        chip.read_system_register(SystemRegister::Rpc),
        Ok(START + 4)
    );
    assert_eq!(chip.halt(), Ok(Entry::Breakpoint), "a halted CPU stays so");
}

#[test]
fn a_fault_on_go_s_first_fetch_is_its_own_exception() {
    // the reset's vector fetches are behind it: an odd RPC is an
    // address error, not a double bus fault (CPU32RM 7.2.8.12, 6.2.3)
    let handler = RAM + 0x100;
    let mut chip = chip(&[BRANCH_TO_SELF]);
    chip.reset_halted().unwrap();
    let space = FunctionCode::SUPERVISOR_DATA;
    chip.write_system_register(SystemRegister::Vbr, RAM)
        .unwrap();
    chip.write_memory(
        Address(RAM + Vector::ADDRESS_ERROR.offset()),
        space,
        Size::Long,
        &handler.to_be_bytes(),
    )
    .unwrap();
    chip.write_memory(Address(handler), space, Size::Word, &BGND.to_be_bytes())
        .unwrap();
    chip.write_system_register(SystemRegister::Ssp, RAM + 0x400)
        .unwrap();
    chip.write_system_register(SystemRegister::Rpc, START + 1)
        .unwrap();
    chip.go().unwrap();
    chip.run_for(Cycles(1000));
    assert_eq!(Cpu32Target::halted(&mut chip), Ok(Some(Entry::Background)));
    assert_eq!(chip.read_system_register(SystemRegister::Pcc), Ok(handler));
}

#[test]
fn without_a_bkpt_reset_the_cpu_cannot_be_halted() {
    let mut chip = chip(&[BRANCH_TO_SELF]);
    assert_eq!(chip.halt(), Err(TargetError::NotEnabled));
    assert_eq!(chip.go(), Err(TargetError::NotHalted));
    assert_eq!(
        chip.read_register(Register::D0),
        Err(TargetError::NotHalted)
    );
    chip.reset_halted().unwrap();
    chip.reset();
    assert_eq!(
        chip.halt(),
        Err(TargetError::NotEnabled),
        "BKPT high at reset"
    );
}
