//! The core alone: a CPU32 on 256 Kbytes of zero-wait RAM in both
//! spaces, nothing on the bus and no interrupts.

use mc683xx_cpu32::address::Address;
use mc683xx_cpu32::bus::Bus;
use mc683xx_cpu32::bus::BusAccess;
use mc683xx_cpu32::bus::BusError;
use mc683xx_cpu32::bus::PortSize;
use mc683xx_cpu32::bus::Read;
use mc683xx_cpu32::cpu::Cpu;
use mc683xx_cpu32::cycles::Cycles;
use mc683xx_cpu32::interrupt::Acknowledge;
use mc683xx_cpu32::interrupt::Acknowledged;
use mc683xx_cpu32::interrupt::InterruptLevel;
use mc683xx_cpu32::interrupt::Interrupts;
use mc683xx_cpu32::memory::Backing;
use mc683xx_cpu32::memory::Memory;
use mc683xx_cpu32::memory::Page;
use mc683xx_cpu32::memory::Space;
use mc683xx_cpu32::registers::status;

use crate::kernel::Calibration;
use crate::kernel::Kernel;
use crate::kernel::Skip;
use crate::kernel::sled;

/// The 68331's address lines (MC68331UM 4.4.1.1).
const ADDRESS_LINES: u32 = 24;
/// Bytes of RAM from zero.
const RAM_BYTES: u32 = 256 << 10;
/// Clocks in a zero-wait bus cycle (CPU32RM 8.1.5).
const FASTEST_CLOCKS: u8 = 2;
/// Where the sled starts.
pub const CODE: u32 = 0x1000;
/// Where the sled ends and A0's buffer starts.
const DATA: u32 = 0x2_0000;
/// A1's buffer.
const OUTPUT: u32 = 0x3_0000;
/// The supervisor stack, at the top of RAM.
const STACK: u32 = RAM_BYTES - 2;
/// The address register numbers `prepare` sets.
const A0: usize = 0;
/// A1.
const A1: usize = 1;
/// D7, the kernels' loop counter.
const D7: usize = 7;
/// A loop counter for DBcc: 65536 times round.
const LOOP_COUNT: u32 = 0xffff;
/// Clocks one iteration runs: well inside the sled for every kernel.
pub const BUDGET: Cycles = Cycles(20_000);

/// A bus with nothing on it.
#[derive(Debug)]
pub struct NoBus;

impl Bus for NoBus {
    fn read(&mut self, _access: BusAccess) -> Result<Read, BusError> {
        Err(BusError)
    }

    fn write(&mut self, _access: BusAccess, _value: u32) -> Result<Cycles, BusError> {
        Err(BusError)
    }
}

/// No interrupt sources.
#[derive(Debug)]
pub struct NoInterrupts;

impl Interrupts for NoInterrupts {
    fn level(&self) -> InterruptLevel {
        InterruptLevel::NONE
    }

    fn acknowledge(&mut self, _level: InterruptLevel, _now: Cycles) -> Acknowledged {
        Acknowledge::Spurious.into()
    }
}

/// A CPU on RAM holding `kernel`'s sled, prepared to run it.
#[must_use]
pub fn machine(kernel: &Kernel) -> Cpu {
    let mut memory = Memory::new(ADDRESS_LINES).expect("24 address lines");
    let region = memory.add_region(vec![0; RAM_BYTES as usize]);
    let backing = Some(Backing { region, offset: 0 });
    let page = Page {
        read: backing,
        write: backing,
        port: PortSize::Bits16,
        clocks: FASTEST_CLOCKS,
    };
    for space in [Space::User, Space::Supervisor] {
        memory
            .map(space, Address(0), RAM_BYTES, page)
            .expect("RAM fits");
    }
    memory.region_mut(region)[CODE as usize..DATA as usize]
        .copy_from_slice(&sled(kernel, (DATA - CODE) as usize));
    let mut cpu = Cpu::new(memory);
    prepare(&mut cpu);
    cpu
}

/// Back to the sled's start, supervisor state with interrupts
/// masked, A0 and A1 at their buffers and D7 a loop count.
pub fn prepare(cpu: &mut Cpu) {
    let registers = &mut cpu.registers;
    registers.set_sr(status::S | status::INTERRUPT_MASK);
    registers.pc = CODE;
    registers.ssp = STACK;
    registers.data = [0; 8];
    registers.data[D7] = LOOP_COUNT;
    registers.set_address(A0, DATA);
    registers.set_address(A1, OUTPUT);
}

/// One iteration: the sled from its start for `budget` clocks.
pub fn iteration(cpu: &mut Cpu, budget: Cycles) -> Cycles {
    prepare(cpu);
    cpu.run_for(&mut NoBus, &mut NoInterrupts, Cycles::ZERO, budget)
        .cycles
}

/// What an iteration of `kernel` does, or why it cannot run.
///
/// # Panics
///
/// If the kernel takes an exception, runs off its sled, or the
/// whole-budget run disagrees with the count.
pub fn calibrate(kernel: &Kernel, budget: Cycles) -> Result<Calibration, Skip> {
    let mut cpu = machine(kernel);
    let calibration = Calibration::measure(budget, || {
        let run = cpu.run_for(&mut NoBus, &mut NoInterrupts, Cycles::ZERO, Cycles(1));
        let taken = cpu.exception_taken();
        assert!(
            taken.is_none_or(crate::kernel::unimplemented),
            "{}: {taken:?}",
            kernel.name
        );
        (run.cycles, taken)
    })?;
    assert!(cpu.registers.pc < DATA, "{}: ran off its sled", kernel.name);
    let mut whole = machine(kernel);
    assert_eq!(
        iteration(&mut whole, budget),
        calibration.clocks,
        "{}: one run and single steps disagree",
        kernel.name
    );
    Ok(calibration)
}
