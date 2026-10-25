use gdbstub::arch::Registers as _;
use mc683xx_cpu32::registers::Registers;
use mc683xx_cpu32::registers::status;

use super::GdbRegisters;
use super::PS;
use super::SP;
use super::SSP;
use super::USP;

/// User and supervisor stack pointers apart.
fn supervisor() -> Registers {
    let mut registers = Registers::default();
    registers.set_sr(status::S);
    registers.usp = 0x1000;
    registers.ssp = 0x2000;
    registers
}

#[test]
fn sp_is_the_active_stack_pointer() {
    let registers = supervisor();
    let gdb = GdbRegisters::of(&registers);
    assert_eq!(gdb.core[SP], 0x2000);
    assert_eq!(gdb.system.unwrap()[USP], 0x1000);
}

#[test]
fn a_changed_sp_is_not_undone_by_ssp() {
    let mut registers = supervisor();
    let mut gdb = GdbRegisters::of(&registers);
    gdb.core[SP] = 0x3000;
    gdb.store(&mut registers);
    assert_eq!(registers.ssp, 0x3000);
    assert_eq!(registers.usp, 0x1000);
}

#[test]
fn a_changed_ssp_is_not_undone_by_sp() {
    let mut registers = supervisor();
    let mut gdb = GdbRegisters::of(&registers);
    if let Some(system) = gdb.system.as_mut() {
        system[SSP] = 0x4000;
    }
    gdb.store(&mut registers);
    assert_eq!(registers.ssp, 0x4000);
}

#[test]
fn clearing_s_makes_sp_the_user_stack_pointer() {
    let mut registers = supervisor();
    let mut gdb = GdbRegisters::of(&registers);
    gdb.core[PS] = 0;
    gdb.store(&mut registers);
    assert!(!registers.is_supervisor());
    assert_eq!(registers.address(7), 0x1000);
    assert_eq!(registers.ssp, 0x2000);
}

#[test]
fn a_g_packet_without_the_description_sets_only_the_core() {
    let mut gdb = GdbRegisters::default();
    // The core's 18 registers, then gdb's default FP registers.
    let bytes = vec![0x11; 18 * 4 + 8 * 12 + 3 * 4];
    gdb.gdb_deserialize(&bytes).unwrap();
    assert_eq!(gdb.core[0], 0x1111_1111);
    assert_eq!(gdb.system, None);
}

#[test]
fn a_short_g_packet_sets_only_the_registers_it_gives() {
    let mut registers = supervisor();
    registers.data[2] = 0x2222_2222;
    registers.pc = 0x400;
    let mut gdb = GdbRegisters::default();
    gdb.gdb_deserialize(&[0x11; 2 * 4]).unwrap();
    assert!(!gdb.is_ragged());
    gdb.store(&mut registers);
    assert_eq!(registers.data[..3], [0x1111_1111, 0x1111_1111, 0x2222_2222]);
    assert_eq!(registers.pc, 0x400);
    assert_eq!(registers.ssp, 0x2000);
    assert!(registers.is_supervisor());
    gdb.gdb_deserialize(&[0]).unwrap();
    assert!(gdb.is_ragged());
}
