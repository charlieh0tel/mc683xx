//! Loads and init scripts on the emulated chip, reset halted in
//! background mode as a BDM pod holds silicon.

use mc683xx_debug::image::Image;
use mc683xx_debug::load::LoadError;
use mc683xx_debug::load::LoadOptions;
use mc683xx_debug::load::OddEdges;
use mc683xx_debug::load::Verification;
use mc683xx_debug::script::CommandError;
use mc683xx_debug::script::Script;
use mc683xx_mc68331::chip::Builder;
use mc683xx_mc68331::chip::Mc68331;
use mc683xx_mc68331::debug::TargetError;
use mc683xx_modules::chip_select::ChipSelect;
use mc683xx_target::Cpu32Target;
use mc683xx_target::address::Address;
use mc683xx_target::function_code::FunctionCode;
use mc683xx_target::register::Register;
use mc683xx_target::register::SystemRegister;
use mc683xx_target::size::Size;

/// The example demo board's firmware and its ELF (examples/boards).
const DEMO_BIN: &[u8] = include_bytes!("../../../examples/boards/demo.bin");
/// The same, linked, with symbols.
const DEMO_ELF: &[u8] = include_bytes!("../../../examples/boards/demo.elf");
/// The demo's entry point, its ELF header's `e_entry`.
const DEMO_ENTRY: u32 = 0x400;
/// Bytes of the boot memory, on CSBOOT.
const BOOT_BYTES: usize = 0x1_0000;
/// RAM, on no select.
const RAM: u32 = 0x20_0000;
/// Bytes of RAM.
const RAM_BYTES: usize = 0x1000;
/// The initial SSP, at the top of RAM.
const STACK: u32 = RAM + RAM_BYTES as u32;
/// The SIM's CSPAR0 (MC68331UM D.3.23), with the module map at
/// $FFF000 (SIMCR MM set, as out of reset).
const CSPAR0: u32 = 0xff_fa44;
/// CSPAR0's bit 1, which always reads one.
const CSPAR0_ONE: u32 = 0x0002;

/// The chip reset halted: boot memory on CSBOOT, writable or not,
/// its reset vectors pointing into RAM.
fn chip(writable_boot: bool) -> Mc68331 {
    let mut boot = vec![0xff; BOOT_BYTES];
    boot[..4].copy_from_slice(&STACK.to_be_bytes());
    boot[4..8].copy_from_slice(&RAM.to_be_bytes());
    let mut builder = Builder::default();
    builder
        .memory(boot, writable_boot, &[ChipSelect::Boot])
        .unwrap();
    builder
        .fixed_memory(Address(RAM), vec![0; RAM_BYTES], true)
        .unwrap();
    let mut chip = builder.build();
    chip.reset_halted().unwrap();
    chip
}

/// `length` bytes at `address` in supervisor data space.
fn read(chip: &mut Mc68331, address: u32, length: usize) -> Vec<u8> {
    let mut data = vec![0; length];
    chip.read_memory(
        Address(address),
        FunctionCode::SUPERVISOR_DATA,
        Size::Byte,
        &mut data,
    )
    .unwrap();
    data
}

#[test]
fn the_demo_elf_is_its_binary_entering_at_its_entry() {
    let image = Image::from_elf(DEMO_ELF).unwrap();
    assert_eq!(image.segments().len(), 1, ".bss has no file bytes");
    assert_eq!(image.segments()[0].address(), Address(0));
    assert_eq!(image.segments()[0].data(), DEMO_BIN);
    assert_eq!(image.entry(), Some(Address(DEMO_ENTRY)));
}

#[test]
fn the_demo_loads_over_bdm_and_runs_from_its_entry() {
    let mut chip = chip(true);
    let image = Image::from_elf(DEMO_ELF).unwrap();
    let report = image.load(&mut chip, &LoadOptions::default()).unwrap();
    assert_eq!(report.bytes, DEMO_BIN.len());
    assert_eq!(report.verification, Verification::Passed);
    assert_eq!(read(&mut chip, 0, DEMO_BIN.len()), DEMO_BIN);
    assert_eq!(
        chip.read_system_register(SystemRegister::Rpc),
        Ok(DEMO_ENTRY)
    );
    chip.step().unwrap();
    assert_ne!(
        chip.read_system_register(SystemRegister::Rpc),
        Ok(DEMO_ENTRY),
        "it ran from its entry"
    );
}

#[test]
fn srecords_load_into_ram_with_odd_edges_in_bytes() {
    let mut chip = chip(false);
    // S3: four bytes at RAM + 1; S7: the entry, RAM + 0x100.
    let image = Image::from_srecords("S30900200001DEADBEEF9D\nS70500200100D9\n").unwrap();
    assert!(matches!(
        image.load(&mut chip, &LoadOptions::default()),
        Err(LoadError::OddEdge { .. })
    ));
    let mut options = LoadOptions::default();
    options.odd_edges = OddEdges::Bytes;
    image.load(&mut chip, &options).unwrap();
    assert_eq!(read(&mut chip, RAM, 6), [0, 0xde, 0xad, 0xbe, 0xef, 0]);
    assert_eq!(
        chip.read_system_register(SystemRegister::Rpc),
        Ok(RAM + 0x100)
    );
}

#[test]
fn a_load_into_rom_fails_verification() {
    let mut chip = chip(false);
    let error = Image::from_binary(Address(0x100), vec![0x4e, 0x71])
        .unwrap()
        .load(&mut chip, &LoadOptions::default())
        .unwrap_err();
    assert!(
        matches!(
            error,
            LoadError::Verify {
                address: Address(0x100),
                wrote: 0x4e,
                read: 0xff,
            }
        ),
        "{error}"
    );
}

#[test]
fn a_load_where_nothing_answers_fails_its_write() {
    let mut chip = chip(false);
    let error = Image::from_binary(Address(0x50_0000), vec![0; 4])
        .unwrap()
        .load(&mut chip, &LoadOptions::default())
        .unwrap_err();
    assert!(
        matches!(
            error,
            LoadError::Write {
                address: Address(0x50_0000),
                bytes: 4,
                error: TargetError::Access { .. },
            }
        ),
        "{error}"
    );
}

#[test]
fn an_init_script_programs_the_sim_and_registers() {
    let mut chip = chip(false);
    let script: Script = format!(
        "# CS0 to CS5 as 16-bit selects\n\
         write_memory.w {CSPAR0:#x} 0x3fff\n\
         read_memory.w {CSPAR0:#x} expect 0x3fff\n\
         write_memory.w {CSPAR0:#x} 0\n\
         read_memory.w {CSPAR0:#x} expect {CSPAR0_ONE:#x}  # bit 1 stays\n\
         poll_memory.w {CSPAR0:#x} mask {CSPAR0_ONE:#x} expect {CSPAR0_ONE:#x} timeout_ms 0\n\
         write_memory.l {RAM:#x} 0xcafef00d\n\
         read_memory.b {:#x} expect 0xfe\n\
         write_register d3 0x1234\n\
         write_system_register vbr {RAM:#x}\n",
        RAM + 1
    )
    .parse()
    .unwrap();
    script.run(&mut chip).unwrap();
    assert_eq!(chip.read_register(Register::D3), Ok(0x1234));
    assert_eq!(chip.read_system_register(SystemRegister::Vbr), Ok(RAM));
    assert_eq!(read(&mut chip, RAM, 4), [0xca, 0xfe, 0xf0, 0x0d]);

    let error = format!("read_memory.w {CSPAR0:#x} expect 0x3fff")
        .parse::<Script>()
        .unwrap()
        .run(&mut chip)
        .unwrap_err();
    assert!(
        matches!(error.error, CommandError::Mismatch { read: CSPAR0_ONE }),
        "{error}"
    );
}
