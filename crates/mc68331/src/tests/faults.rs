//! Fault stops on the chip ([`CpuControl::set_stop_on_fault`]).
//!
//! [`CpuControl::set_stop_on_fault`]: mc683xx_cpu32::cpu::control::CpuControl::set_stop_on_fault

use mc683xx_cpu32::cpu::Stop;
use mc683xx_cpu32::cpu::bare::FaultCause;
use mc683xx_cpu32::cycles::Cycles;
use mc683xx_cpu32::exception::CycleKind;
use mc683xx_modules::chip_select::ChipSelect;
use mc683xx_target::Cpu32Target;
use mc683xx_target::address::Address;
use mc683xx_target::register::SystemRegister;

use super::LPSTOP_MASK_0;
use super::MOVE_WORD_FROM_ABSOLUTE;
use super::NOP;
use super::ROM_BYTES;
use super::SIM;
use super::STACK;
use super::START;
use super::booting;
use super::image;
use super::store_word;
use super::write_register;
use crate::chip::Builder;
use crate::chip::Mc68331;
use crate::debug::TargetError;

/// An odd address, which a word access faults at (CPU32RM 6.2.3).
const ODD: u32 = 0x0401;

#[test]
fn a_fault_stop_ends_the_run_with_the_instruction_undone() {
    let (mut chip, _) = booting(&[MOVE_WORD_FROM_ABSOLUTE, 0, ODD as u16, NOP]);
    chip.cpu_control().set_stop_on_fault(true);
    let run = chip.run_for(Cycles(1000));
    assert_eq!(run.stop, Stop::Fault);
    let fault = chip.cpu().fault_stop().expect("a fault stop");
    assert_eq!(fault.cycle, CycleKind::Read);
    assert_eq!(fault.cause, FaultCause::AddressError);
    assert_eq!(fault.address, Address(ODD));
    assert_eq!(fault.registers.pc, START);
    assert_eq!(chip.cpu().registers.pc, START);
    assert_eq!(chip.cpu().registers.ssp, STACK, "nothing stacked");
    // taken as an exception with stop on fault off
    chip.cpu_control().set_stop_on_fault(false);
    let run = chip.run_for(Cycles(1000));
    assert_ne!(run.stop, Stop::Fault);
    assert_eq!(chip.cpu().fault_stop(), None);
}

#[test]
fn a_bus_monitor_timeout_stops_the_run_rather_than_halting() {
    let (mut chip, _) = booting(&[NOP]);
    chip.cpu_control().set_stop_on_fault(true);
    // BME, BMT 8 clocks: CSBOOT's 16-clock cycles time out
    write_register(&mut chip, SIM + 0x20, 0x0007);
    let run = chip.run_for(Cycles(1));
    assert_eq!(run.stop, Stop::Fault);
    let fault = chip.cpu().fault_stop().expect("a fault stop");
    assert_eq!(fault.cycle, CycleKind::Fetch);
    assert_eq!(fault.cause, FaultCause::BusError);
    assert_eq!(chip.halted(), None, "no double bus fault");
}

/// MOVEA.L #1,A7: SSP odd, so stacking faults (CPU32RM 4, MOVEA).
const ODD_STACK: [u16; 3] = [0x2e7c, 0, 1];
/// The periodic timer's vector.
const PIT_VECTOR: u32 = 0x50;
/// Its handler, NOPs in the boot ROM.
const HANDLER: u32 = 0x2000;
/// RAM, for a stack.
const RAM: u32 = 0x20_0000;
/// Its bytes.
const RAM_BYTES: usize = 2048;

/// A chip with RAM whose periodic timer wakes it from LPSTOP, SSP odd.
fn sleeping() -> Mc68331 {
    let mut program = Vec::new();
    // the watchdog off; the PIT at level 6, vector $50, PITM 1
    program.extend(store_word(SIM + 0x20, 0));
    program.extend(store_word(SIM + 0x22, 0x0600 | PIT_VECTOR as u16));
    program.extend(store_word(SIM + 0x24, 0x0001));
    program.extend(ODD_STACK);
    program.extend(LPSTOP_MASK_0);
    let mut rom = image(&program, ROM_BYTES);
    let entry = 4 * PIT_VECTOR as usize;
    rom[entry..entry + 4].copy_from_slice(&HANDLER.to_be_bytes());
    let mut builder = Builder::default();
    builder.memory(rom, false, &[ChipSelect::Boot]).unwrap();
    builder
        .fixed_memory(Address(RAM), vec![0; RAM_BYTES], true)
        .unwrap();
    builder.build()
}

#[test]
fn a_fault_stacking_the_interrupt_that_ends_lpstop_leaves_the_chip_asleep() {
    let mut chip = sleeping();
    chip.cpu_control().set_stop_on_fault(true);
    let run = loop {
        let run = chip.run_for(Cycles(1000));
        if run.stop != Stop::Budget && run.stop != Stop::Stopped {
            break run;
        }
        assert!(chip.now() < Cycles(200_000), "never woke");
    };
    assert_eq!(run.stop, Stop::Fault);
    assert_eq!(
        chip.cpu().fault_stop().map(|fault| fault.cycle),
        Some(CycleKind::Write)
    );
    assert!(chip.cpu().stopped(), "LPSTOP not ended (CPU32RM 4.4)");
    assert_eq!(chip.sim().low_power(), Some(0), "the modules asleep");
    // SSP fixed: the PIT's request stands, so the next run takes it at
    // once, and the chip wakes
    chip.cpu_control().registers_mut().ssp = RAM + RAM_BYTES as u32;
    let faulted = chip.now();
    assert_ne!(chip.run_for(Cycles(64)).stop, Stop::Fault);
    assert!(
        !chip.cpu().stopped(),
        "taken at {faulted:?}, not the next count"
    );
    assert_eq!(chip.sim().low_power(), None);
    assert!(
        (HANDLER..HANDLER + 0x100).contains(&chip.cpu().registers.pc),
        "{:#x}",
        chip.cpu().registers.pc
    );
}

#[test]
fn a_fault_stop_fails_a_bdm_step_in_background_mode_at_the_instruction() {
    let (mut chip, _) = booting(&[MOVE_WORD_FROM_ABSOLUTE, 0, ODD as u16, NOP]);
    chip.reset_halted().unwrap();
    chip.cpu_control().set_stop_on_fault(true);
    assert_eq!(chip.step(), Err(TargetError::Stopped));
    assert!(chip.cpu().background().is_some());
    assert_eq!(chip.read_system_register(SystemRegister::Rpc), Ok(START));
    assert!(chip.cpu().fault_stop().is_some());
}

#[test]
fn stop_on_fault_is_the_host_s_setting_not_the_snapshot_s() {
    let (mut chip, _) = booting(&[NOP]);
    let snapshot = chip.save().unwrap();
    chip.cpu_control().set_stop_on_fault(true);
    chip.restore(&snapshot).unwrap();
    assert!(chip.cpu().stops_on_fault());
}
