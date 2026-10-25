//! A smartclock firmware image on the core, as smartclock's harness
//! calls into it (github.com/charlieh0tel/smartclock,
//! `tools/firmware/libc_check.py` and `harness.py`): the image in ROM,
//! RAM, a stack, a return address that ends a call, and a call run
//! instruction by instruction until it returns or stops.  The images
//! are HP's and never committed here; tests read them from a path
//! (PLAN.md, "Firmware images by path, not committed").

use std::sync::Arc;

use cpu32_oracle::call::Ending;
use cpu32_oracle::call::stop_before;
use cpu32_oracle::memory::Contents;
use cpu32_oracle::memory::Polled;
use cpu32_oracle::memory::Region;
use cpu32_oracle::memory::RegionMemory;
use cpu32_oracle::state::State;
use mc683xx_cpu32::address::Address;
use mc683xx_cpu32::bus::Bus;
use mc683xx_cpu32::bus::BusAccess;
use mc683xx_cpu32::bus::BusError;
use mc683xx_cpu32::bus::PortSize;
use mc683xx_cpu32::bus::Read;
use mc683xx_cpu32::cpu::Cpu;
use mc683xx_cpu32::cycles::Cycles;
use mc683xx_cpu32::exception::Exception;
use mc683xx_cpu32::exception::Vector;
use mc683xx_cpu32::memory::Backing;
use mc683xx_cpu32::memory::Memory;
use mc683xx_cpu32::memory::Page;
use mc683xx_cpu32::memory::RegionId;
use mc683xx_cpu32::memory::Space;
use mc683xx_cpu32::registers::Registers;
use mc683xx_cpu32::registers::status;

use crate::machine;
use crate::machine::ADDRESS_BITS;
use crate::machine::Ended;

/// The image's ROM: boot chip select, 512K from zero
/// (smartclock `docs/firmware/README.md`).
pub const ROM: u32 = 0;
/// Bytes of ROM: every image is this long.
pub const ROM_BYTES: u32 = 0x8_0000;
/// RAM, as the harness maps it.
pub const RAM: u32 = 0x10_0000;
/// Bytes of RAM.
pub const RAM_BYTES: u32 = 0x1_0000;
/// The EEPROM, chip select 9's 8K, erased at a call's start.
pub const EEPROM: u32 = 0x40_0000;
/// Bytes of EEPROM.
pub const EEPROM_BYTES: u32 = 0x2000;
/// What erased EEPROM holds.
const ERASED: u8 = 0xff;
/// The harness's call stack, clear of ROM, RAM and the peripherals.
pub const STACK: u32 = 0x70_0000;
/// Bytes of stack.
pub const STACK_BYTES: u32 = 0x1_0000;
/// The stack's top, less a caller's frame.
pub const STACK_TOP: u32 = STACK + STACK_BYTES - 0x10;
/// What the stack holds at a call's start.
pub const CLEAN_STACK: u8 = 0xa5;
/// Where a call returns to: mapped, never executed, the call's end.
pub const RETURN: u32 = 0x80_0000;
/// Bytes mapped at `RETURN`: the return page and the harness's stubs.
const RETURN_BYTES: u32 = 0x1000;
/// Routines the harness passes for a parameter of routine type: an RTS
/// each, `STUB_SPACING` apart, after the return address.
pub const STUBS: u32 = RETURN + 0x100;
/// Bytes between two stubs.
pub const STUB_SPACING: u32 = 0x20;
/// The arguments' buffers, in RAM.
pub const BUFFER: u32 = RAM + 0x8000;
/// Bytes between two buffers, the most one holds.
pub const BUFFER_SPACING: u32 = 0x100;
/// What a buffer holds past its contents.
const BUFFER_FILL: u8 = 0xa5;
/// The primary's vector table (smartclock `docs/firmware/README.md`),
/// which a call takes its exceptions through.
pub const VECTORS: u32 = 0x1_0000;
/// SR at a call's start: supervisor state, interrupts masked.
pub const SUPERVISOR: u16 = status::S | status::INTERRUPT_MASK;
/// Instructions a call may run before it is taken to be stuck.
pub const STEP_LIMIT: usize = 2_000_000;
/// RTS (CPU32RM 4.4, RTS).
pub const RTS: u16 = 0x4e75;
/// The traps whose handlers a call runs, the image's own: #1, which
/// saves or restores SR for setjmp and longjmp, and #7, which hands back
/// the caller's SR (smartclock `harness.py`, HANDLED).  A call ends
/// at any other: the kernel's, answered by the harness's models, and
/// those that never return.
pub const HANDLED_TRAPS: [u8; 2] = [1, 7];
/// Clocks of a memory cycle: the fastest (CPU32RM 8.1.5).
const CLOCKS: u8 = 2;

/// The peripherals' chip selects and the MC68331's own modules, as
/// smartclock's harness maps them (`harness.py`, `PERIPHERALS`; smartclock
/// `docs/firmware/restart.md`, "The chip selects"): each its start and
/// bytes, memory zeroed at a call's start, so that a read finds zero or
/// what the call last wrote there, but for `STATUS_REGISTERS`.  CPU
/// space is not among them: a call takes no interrupt.
const PERIPHERALS: [(u32, u32); 4] = [
    // The DUART.
    (0x20_0000, 0x1000),
    // The latch, the switch byte and the registers at $304000 and
    // $306000.
    (0x30_0000, 0x1_0000),
    (0x50_0000, 0x1000),
    // The GPT, SIM and QSM.
    (0xff_f000, 0x1000),
];

/// A peripheral register a routine polls (`harness.py`, Status), which
/// reads as the bits its peripheral shows once it is done, whatever was
/// written there: ready at once, nothing waiting, as smartclock's first
/// peripheral case has it (`harness.py`, `PERIPHERAL_CASES`).
#[derive(Debug, Clone, Copy)]
struct Status {
    /// Its address.
    address: u32,
    /// Its bytes.
    size: u32,
    /// The bits a read gives as last written.
    kept: u32,
    /// The bits the peripheral sets once ready.
    ready: u32,
}

/// The registers the routines poll (`harness.py`, `STATUS_REGISTERS`).
const STATUS_REGISTERS: [Status; 5] = [
    // The DUART's SRA and SRB: TxRDY and TxEMT (MC68681, "Channel A
    // status register (SRA) and channel B status register (SRB)").
    Status {
        address: 0x20_0003,
        size: 1,
        kept: 0,
        ready: 0x0c,
    },
    Status {
        address: 0x20_0013,
        size: 1,
        kept: 0,
        ready: 0x0c,
    },
    // The SCI's SCSR: TDRE and TC (MC68331UM D.4.6).
    Status {
        address: 0xff_fc0c,
        size: 2,
        kept: 0,
        ready: 0x0180,
    },
    // The QSPI's SPCR1, whose SPE the QSPI clears at the end of its
    // queue (MC68331UM 6.3.5.1), and SPSR, whose SPIF it then sets
    // (D.4.13).
    Status {
        address: 0xff_fc1a,
        size: 2,
        kept: 0x7fff,
        ready: 0,
    },
    Status {
        address: 0xff_fc1f,
        size: 1,
        kept: 0,
        ready: 0x80,
    },
];

/// The addresses the 24 address lines reach.
const ADDRESS_MASK: u32 = (1 << ADDRESS_BITS) - 1;

/// The peripherals on the bus, as `PERIPHERALS` and `STATUS_REGISTERS` model
/// them.  Any other cycle the core does not serve, a write to ROM
/// among them, ends in a bus error.
#[derive(Debug)]
struct Peripherals {
    /// Each of `PERIPHERALS`' bytes, in its order.
    windows: Vec<Vec<u8>>,
}

impl Peripherals {
    fn new() -> Self {
        Self {
            windows: PERIPHERALS
                .iter()
                .map(|&(_, length)| vec![0; length as usize])
                .collect(),
        }
    }

    /// Every peripheral's bytes zero, as a call starts them.
    fn fresh(&mut self) {
        for window in &mut self.windows {
            window.fill(0);
        }
    }

    /// The byte at `address`, if a peripheral has one there.
    fn byte(&mut self, address: u32) -> Option<&mut u8> {
        PERIPHERALS
            .iter()
            .zip(&mut self.windows)
            .find(|&(&(start, length), _)| (start..start + length).contains(&address))
            .map(|(&(start, _), window)| &mut window[(address - start) as usize])
    }

    /// The addresses of a cycle's bytes, if a peripheral has them all; CPU
    /// space has none.
    fn addresses(&mut self, access: BusAccess) -> Result<Vec<u32>, BusError> {
        if access.function_code.is_cpu_space() {
            return Err(BusError);
        }
        let addresses: Vec<u32> = (0..access.size.bytes())
            .map(|index| (access.address.0 + index) & ADDRESS_MASK)
            .collect();
        if addresses.iter().any(|&at| self.byte(at).is_none()) {
            return Err(BusError);
        }
        Ok(addresses)
    }
}

/// The bits of `bits`, a status register's, that lie in its byte at
/// `address`.
const fn status_byte(status: &Status, bits: u32, address: u32) -> u8 {
    (bits >> (8 * (status.address + status.size - 1 - address))) as u8
}

impl Bus for Peripherals {
    fn read(&mut self, access: BusAccess) -> Result<Read, BusError> {
        let mut value = 0;
        for at in self.addresses(access)? {
            let byte = *self.byte(at).expect("a peripheral's");
            let byte = STATUS_REGISTERS
                .iter()
                .find(|status| (status.address..status.address + status.size).contains(&at))
                .map_or(byte, |status| {
                    byte & status_byte(status, status.kept, at)
                        | status_byte(status, status.ready, at)
                });
            value = value << 8 | u32::from(byte);
        }
        Ok(Read {
            value,
            stall: Cycles::ZERO,
        })
    }

    fn write(&mut self, access: BusAccess, value: u32) -> Result<Cycles, BusError> {
        let addresses = self.addresses(access)?;
        let last = addresses.len() - 1;
        for (index, &at) in addresses.iter().enumerate() {
            *self.byte(at).expect("a peripheral's") = (value >> (8 * (last - index))) as u8;
        }
        Ok(Cycles::ZERO)
    }
}

/// What is wrong with an image.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FirmwareError {
    /// It is not the ROM's size.
    #[error("image is {0} bytes, not {ROM_BYTES}")]
    Size(usize),
}

/// How a call ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stopped {
    /// It returned to `RETURN`.
    Returned,
    /// It reached a trap whose handler it does not run: the number.
    Trap(u8),
    /// It reached a branch to itself.
    Spin,
    /// The core does not implement the opcode at `pc`.
    Unimplemented {
        /// The opcode.
        opcode: u16,
        /// Its address.
        pc: u32,
    },
    /// The core took an exception the call does not handle itself.
    Exception {
        /// It.
        exception: Exception,
        /// The address of the instruction that raised it.
        pc: u32,
    },
    /// The CPU halted.
    Halted,
    /// It ran `STEP_LIMIT` instructions.
    StepLimit,
}

/// A call's end: how, the registers then, and the instructions it ran.
#[derive(Debug, Clone)]
pub struct Call {
    /// How it ended.
    pub stopped: Stopped,
    /// The registers it started with.
    pub started: State,
    /// The registers.
    pub registers: Registers,
    /// Instructions run.
    pub steps: usize,
}

impl Call {
    /// D0.
    #[must_use]
    pub const fn d0(&self) -> u32 {
        self.registers.data[0]
    }

    /// D1.
    #[must_use]
    pub const fn d1(&self) -> u32 {
        self.registers.data[1]
    }
}

/// An instruction a call ran, for an observer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Executed {
    /// Its address.
    pub pc: u32,
    /// Its opcode.
    pub opcode: u16,
    /// The word after the opcode.
    pub extension: u16,
    /// SR after it.
    pub sr: u16,
}

/// The image on the core.
#[derive(Debug)]
pub struct Firmware {
    /// The image, which every snapshot shares.
    image: Arc<[u8]>,
    /// The core.
    cpu: Cpu,
    /// The peripherals.
    peripherals: Peripherals,
    /// The whole 24-bit address space, each address at its own offset,
    /// mapped only where the harness has memory.
    space: RegionId,
    /// Where the next buffer goes.
    next_buffer: u32,
}

impl Firmware {
    /// `image` in ROM, everything else as a call starts it.
    pub fn new(image: &[u8]) -> Result<Self, FirmwareError> {
        if image.len() != ROM_BYTES as usize {
            return Err(FirmwareError::Size(image.len()));
        }
        let mut memory = Memory::new(ADDRESS_BITS).expect("24 address lines");
        let mut bytes = vec![0; 1 << ADDRESS_BITS];
        bytes[ROM as usize..(ROM + ROM_BYTES) as usize].copy_from_slice(image);
        let space = memory.add_region(bytes);
        let backing = |start: u32| {
            Some(Backing {
                region: space,
                offset: start,
            })
        };
        let rom = Page {
            read: backing(ROM),
            write: None,
            port: PortSize::Bits16,
            clocks: CLOCKS,
        };
        let page = |start| Page {
            read: backing(start),
            write: backing(start),
            ..rom
        };
        // The return page is read only: a call writes no code.
        let regions = [
            (ROM, ROM_BYTES, rom),
            (RAM, RAM_BYTES, page(RAM)),
            (EEPROM, EEPROM_BYTES, page(EEPROM)),
            (STACK, STACK_BYTES, page(STACK)),
            (
                RETURN,
                RETURN_BYTES,
                Page {
                    read: backing(RETURN),
                    ..rom
                },
            ),
        ];
        for (start, length, page) in regions {
            for space in [Space::User, Space::Supervisor] {
                memory
                    .map(space, Address(start), length, page)
                    .expect("a region inside 24 bits");
            }
        }
        let mut firmware = Self {
            image: Arc::from(image),
            cpu: Cpu::new(memory),
            peripherals: Peripherals::new(),
            space,
            next_buffer: BUFFER,
        };
        firmware.fresh();
        Ok(firmware)
    }

    /// Memory as a call starts with it: RAM and the peripherals zero, the
    /// stack clean, the EEPROM erased, the stubs in place, no buffers.
    pub fn fresh(&mut self) {
        self.peripherals.fresh();
        let bytes = self.cpu.memory.region_mut(self.space);
        let mut fill = |start: u32, length: u32, value: u8| {
            bytes[start as usize..(start + length) as usize].fill(value);
        };
        fill(RAM, RAM_BYTES, 0);
        fill(EEPROM, EEPROM_BYTES, ERASED);
        fill(STACK, STACK_BYTES, CLEAN_STACK);
        fill(RETURN, RETURN_BYTES, 0);
        for at in (STUBS..RETURN + RETURN_BYTES).step_by(STUB_SPACING as usize) {
            bytes[at as usize..at as usize + 2].copy_from_slice(&RTS.to_be_bytes());
        }
        self.next_buffer = BUFFER;
    }

    /// The stub for a routine parameter at `position`.
    #[must_use]
    pub const fn stub(position: usize) -> u32 {
        STUBS + position as u32 * STUB_SPACING
    }

    /// A new buffer holding `contents`, `BUFFER_FILL` after them: its
    /// address.
    ///
    /// # Panics
    ///
    /// If `contents` is longer than a buffer, or RAM is full.
    pub fn buffer(&mut self, contents: &[u8]) -> u32 {
        assert!(contents.len() <= BUFFER_SPACING as usize, "a long buffer");
        let at = self.next_buffer;
        assert!(at + BUFFER_SPACING <= RAM + RAM_BYTES, "RAM is full");
        self.next_buffer += BUFFER_SPACING;
        let mut filled = contents.to_vec();
        filled.resize(BUFFER_SPACING as usize, BUFFER_FILL);
        self.write(at, &filled);
        at
    }

    /// `length` bytes at `at`.
    #[must_use]
    pub fn read(&self, at: u32, length: usize) -> &[u8] {
        &self.cpu.memory.region(self.space)[at as usize..at as usize + length]
    }

    /// The text at `at`, to its NUL or the buffer's end.
    #[must_use]
    pub fn string(&self, at: u32) -> &[u8] {
        let bytes = self.read(at, BUFFER_SPACING as usize);
        let end = bytes
            .iter()
            .position(|&byte| byte == 0)
            .unwrap_or(bytes.len());
        &bytes[..end]
    }

    /// The long word at `at`.
    #[must_use]
    pub fn long(&self, at: u32) -> u32 {
        let bytes = self.read(at, 4);
        u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
    }

    /// Memory and the peripherals as they are, for an oracle: the image
    /// shared, the rest copied, the polled status registers' bits.
    #[must_use]
    pub fn snapshot(&self) -> RegionMemory {
        let memory = MEMORY.iter().map(|&(start, length)| {
            let bytes = self.read(start, length as usize);
            let contents = match start {
                ROM => Contents::ReadOnly(Arc::clone(&self.image)),
                RETURN => Contents::ReadOnly(Arc::from(bytes)),
                _ => Contents::Writable(bytes.to_vec()),
            };
            Region { start, contents }
        });
        let peripherals =
            PERIPHERALS
                .iter()
                .zip(&self.peripherals.windows)
                .map(|(&(start, _), window)| Region {
                    start,
                    contents: Contents::Writable(window.clone()),
                });
        let polled = STATUS_REGISTERS.iter().flat_map(|status| {
            (status.address..status.address + status.size).map(|address| Polled {
                address,
                kept: status_byte(status, status.kept, address),
                set: status_byte(status, status.ready, address),
            })
        });
        RegionMemory::new(memory.chain(peripherals).collect()).with_polled(polled.collect())
    }

    /// Writes `bytes` at `at`, ROM too.
    pub fn write(&mut self, at: u32, bytes: &[u8]) {
        self.cpu.memory.region_mut(self.space)[at as usize..at as usize + bytes.len()]
            .copy_from_slice(bytes);
    }

    /// Lays out a call's stack below `top`: `arguments`, then `RETURN`
    /// below them.  The stack pointer the call starts with.
    pub fn push_call(&mut self, top: u32, arguments: &[u8]) -> u32 {
        let arguments_at = top - arguments.len() as u32;
        self.write(arguments_at, arguments);
        let sp = arguments_at - 4;
        self.write(sp, &RETURN.to_be_bytes());
        sp
    }

    /// Calls `entry` with SP `sp`, in supervisor state with interrupts
    /// masked and VBR at the image's vectors, the registers as `set`
    /// leaves them, until the call returns or stops.
    pub fn call(&mut self, entry: u32, sp: u32, set: impl FnOnce(&mut Registers)) -> Call {
        self.call_observed(entry, sp, set, None)
    }

    /// `call`, telling `observe` each instruction the call runs.
    pub fn call_observed(
        &mut self,
        entry: u32,
        sp: u32,
        set: impl FnOnce(&mut Registers),
        mut observe: Option<&mut dyn FnMut(Executed)>,
    ) -> Call {
        machine::renew(&mut self.cpu);
        let registers = &mut self.cpu.registers;
        registers.set_sr(SUPERVISOR);
        registers.vbr = VECTORS;
        registers.ssp = sp;
        registers.pc = entry;
        set(registers);
        let started = machine::capture(registers);
        let mut steps = 0;
        let stopped = loop {
            if steps == STEP_LIMIT {
                break Stopped::StepLimit;
            }
            let pc = self.cpu.registers.pc;
            if pc == RETURN {
                break Stopped::Returned;
            }
            if let Some(stopped) = self.stops_at(pc) {
                break stopped;
            }
            steps += 1;
            let words = observe
                .is_some()
                .then(|| (self.word(pc), self.word(pc.wrapping_add(2))));
            let ended = machine::step(&mut self.cpu, &mut self.peripherals);
            if let (Some(observe), Some((Some(opcode), extension))) = (&mut observe, words) {
                observe(Executed {
                    pc,
                    opcode,
                    extension: extension.unwrap_or_default(),
                    sr: self.cpu.registers.sr(),
                });
            }
            match ended {
                Ended::Completed => {}
                Ended::Unimplemented => {
                    break Stopped::Unimplemented {
                        opcode: self.word(pc).unwrap_or_default(),
                        pc,
                    };
                }
                // The image's own handlers take the handled traps.
                Ended::Exception(Exception::Short { vector, .. })
                    if (Vector::TRAP.0..Vector::TRAP.0 + 16).contains(&vector.0) => {}
                Ended::Exception(exception) => break Stopped::Exception { exception, pc },
                Ended::Halted(_) => break Stopped::Halted,
            }
        };
        Call {
            stopped,
            started,
            registers: self.cpu.registers.clone(),
            steps,
        }
    }

    /// The word at `at`, if memory holds it.
    fn word(&self, at: u32) -> Option<u16> {
        let bytes = self.cpu.memory.region(self.space);
        (in_memory(at) && in_memory(at + 1) && at.is_multiple_of(2))
            .then(|| u16::from_be_bytes([bytes[at as usize], bytes[at as usize + 1]]))
    }

    /// Whether the instruction at `pc` ends the call before it runs: a
    /// trap it does not handle or a spin.
    fn stops_at(&self, pc: u32) -> Option<Stopped> {
        match stop_before(|at| self.word(at), pc, &HANDLED_TRAPS)? {
            Ending::Trap(number) => Some(Stopped::Trap(number)),
            _ => Some(Stopped::Spin),
        }
    }
}

/// The memory the harness maps, each its start and bytes.
const MEMORY: [(u32, u32); 5] = [
    (ROM, ROM_BYTES),
    (RAM, RAM_BYTES),
    (EEPROM, EEPROM_BYTES),
    (STACK, STACK_BYTES),
    (RETURN, RETURN_BYTES),
];

/// Whether memory holds the byte at `at`.
fn in_memory(at: u32) -> bool {
    MEMORY
        .iter()
        .any(|&(start, length)| (start..start + length).contains(&at))
}

/// Whether the harness maps the byte at `address`, memory or a
/// peripheral's, on the 24 address lines.
#[must_use]
pub fn maps(address: u32) -> bool {
    let at = address & ADDRESS_MASK;
    in_memory(at)
        || PERIPHERALS
            .iter()
            .any(|&(start, length)| (start..start + length).contains(&at))
}

#[cfg(test)]
mod tests {
    use mc683xx_cpu32::exception::Exception;

    use super::Firmware;
    use super::ROM_BYTES;
    use super::RTS;
    use super::STACK_TOP;
    use super::Stopped;
    use super::maps;

    /// MOVEQ #5, D0 (CPU32RM 4.4, MOVEQ).
    const MOVEQ_5: u16 = 0x7005;
    /// TRAP #3 (CPU32RM 4.4, TRAP).
    const TRAP_3: u16 = 0x4e43;
    /// Where the tests' code is.
    const CODE: u32 = 0x400;

    fn image(code: &[u16]) -> Vec<u8> {
        let mut image = vec![0; ROM_BYTES as usize];
        for (index, word) in code.iter().enumerate() {
            let at = CODE as usize + 2 * index;
            image[at..at + 2].copy_from_slice(&word.to_be_bytes());
        }
        image
    }

    #[test]
    fn a_call_returns_with_its_result() {
        let mut firmware = Firmware::new(&image(&[MOVEQ_5, RTS])).unwrap();
        let sp = firmware.push_call(STACK_TOP, &[]);
        let call = firmware.call(CODE, sp, |_| {});
        assert_eq!(call.stopped, Stopped::Returned);
        assert_eq!(call.d0(), 5);
        assert_eq!(call.registers.ssp, sp + 4);
    }

    #[test]
    fn a_call_ends_at_an_unhandled_trap_and_a_spin() {
        let mut firmware = Firmware::new(&image(&[TRAP_3, 0x60fe])).unwrap();
        let sp = firmware.push_call(STACK_TOP, &[]);
        assert_eq!(firmware.call(CODE, sp, |_| {}).stopped, Stopped::Trap(3));
        assert_eq!(firmware.call(CODE + 2, sp, |_| {}).stopped, Stopped::Spin);
    }

    #[test]
    fn an_unimplemented_opcode_ends_a_call() {
        // ADD.L D1, D0: not in the core yet.
        const ADD: u16 = 0xd081;
        let mut firmware = Firmware::new(&image(&[ADD, RTS])).unwrap();
        let sp = firmware.push_call(STACK_TOP, &[]);
        let stopped = firmware.call(CODE, sp, |_| {}).stopped;
        assert!(
            matches!(
                stopped,
                Stopped::Unimplemented {
                    opcode: ADD,
                    pc: CODE
                }
            ) || stopped == Stopped::Returned,
            "{stopped:?}"
        );
    }

    #[test]
    fn peripherals_keep_what_is_written_and_polled_registers_read_ready() {
        // MOVE.B #$5A, $200000; MOVE.B $200000, D0; MOVE.B $200003, D1
        // (SRA); RTS
        let code = [
            0x13fc, 0x005a, 0x0020, 0x0000, 0x1039, 0x0020, 0x0000, 0x1239, 0x0020, 0x0003, RTS,
        ];
        let mut firmware = Firmware::new(&image(&code)).unwrap();
        let sp = firmware.push_call(STACK_TOP, &[]);
        let call = firmware.call(CODE, sp, |_| {});
        assert_eq!(call.stopped, Stopped::Returned);
        assert_eq!(call.d0(), 0x5a);
        assert_eq!(call.d1(), 0x0c);
        // Fresh, they are zero again.
        firmware.fresh();
        let sp = firmware.push_call(STACK_TOP, &[]);
        let call = firmware.call(CODE + 8, sp, |_| {});
        assert_eq!(call.stopped, Stopped::Returned);
        assert_eq!(call.d0(), 0);
    }

    #[test]
    fn a_write_to_rom_or_beyond_the_map_is_a_bus_error() {
        // MOVE.B D0, $400.W
        let mut firmware = Firmware::new(&image(&[0x11c0, 0x0400])).unwrap();
        let sp = firmware.push_call(STACK_TOP, &[]);
        assert!(matches!(
            firmware.call(CODE, sp, |_| {}).stopped,
            Stopped::Exception {
                exception: Exception::Fault(_),
                pc: CODE
            }
        ));
        assert!(maps(0xff_fc1f) && maps(0x40_0000) && !maps(0x11_0000));
    }

    #[test]
    fn a_snapshot_holds_memory_and_the_peripherals_as_the_core_sees_them() {
        let mut firmware = Firmware::new(&image(&[MOVEQ_5, RTS])).unwrap();
        let at = firmware.buffer(b"ab");
        let mut memory = firmware.snapshot();
        assert_eq!(memory.read_word(CODE), MOVEQ_5);
        assert_eq!(memory.read_word(at), u16::from_be_bytes(*b"ab"));
        // SRA reads TxRDY and TxEMT, whatever was written.
        memory.write_byte(0x20_0003, 0);
        assert_eq!(memory.read_byte(0x20_0003), 0x0c);
        memory.write_byte(CODE, 0);
        assert!(memory.fault().is_some_and(|fault| fault.write));
    }

    #[test]
    fn buffers_are_filled_past_their_contents() {
        let mut firmware = Firmware::new(&image(&[])).unwrap();
        let at = firmware.buffer(b"ab\0");
        assert_eq!(firmware.string(at), b"ab");
        assert_eq!(firmware.read(at, 4), b"ab\0\xa5");
        assert_eq!(firmware.buffer(b""), at + super::BUFFER_SPACING);
    }
}
