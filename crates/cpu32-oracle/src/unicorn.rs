//! Unicorn (github.com/unicorn-engine/unicorn, GPL-2.0; QEMU's m68k
//! inside it) in 68020 mode, as a black-box oracle for firmware calls,
//! through the `unicorn-engine` crate's API alone.  Test-only and never
//! distributed (`third_party/NOTICE`; PLAN.md, Decisions).  Its
//! implementation is not read: what it does is pinned by this module's
//! tests, as smartclock's emulator and harness found it
//! (github.com/charlieh0tel/smartclock,
//! `crates/smartclock-emu/src/cpu/unicorn.rs`,
//! `tools/firmware/unicorn_check.py`; by this project's author), and as
//! `crates/cpu32/tests/unicorn_faults.rs` lists the faults for the
//! core.
//!
//! The faults, and what the oracle does about each, so that what
//! Unicorn answers can be trusted where it can be:
//!
//! 1. A read of SR spoils the lazy condition codes.  SR is read only
//!    between runs: the system byte from a saved context, whose copy
//!    the read spoils instead, the codes with a probe, `MOVE CCR, D0`,
//!    run on a page of the oracle's own.
//! 2. A code hook part way through a block sees stale codes.  The code
//!    hook reads no register and writes none: it stops the run, and the
//!    instruction it stopped at is done, or the run resumed, from
//!    outside.
//! 3. `RTR` is taken for an illegal instruction: the oracle does it.
//! 4. `MOVE (SP)+, SR` that clears S pops the user stack: the oracle
//!    does it.
//! 5. `STOP` runs on rather than waiting: a call that reaches it is not
//!    comparable.
//! 6. No exception is taken by the CPU, each, `RTE` too, handed to the
//!    interrupt hook: the oracle takes the handled traps, format $0, and
//!    pops a format $0 frame; any other ends the call.
//! 7. VBR: the call's is the oracle's own, which it takes its
//!    exceptions through; `MOVEC` to or from VBR is not comparable.
//!
//! Unicorn keeps the code it translated across a write to it, so code
//! run from writable memory is not comparable, and a new image needs a
//! new engine.
//!
//! Unicorn's address space is 32 bits, the 68331's 24 (MC68331UM
//! 4.4.1.1), whose memory repeats above them.  An access above them
//! maps the alias its high byte names, when first made: a read-only
//! region's as a copy, a writable one's or a device's as a device
//! whose accesses go to the region.  Code fetched from the latter is
//! not comparable.

use std::sync::Arc;

use unicorn_engine::Arch;
use unicorn_engine::HookType;
use unicorn_engine::M68kCpuModel;
use unicorn_engine::MemType;
use unicorn_engine::Mode;
use unicorn_engine::Prot;
use unicorn_engine::RegisterM68K;
use unicorn_engine::uc_error;

use crate::call::Call;
use crate::call::CallEnd;
use crate::call::Ending;
use crate::call::FORMAT_SHIFT;
use crate::call::SHORT_FRAME_BYTES;
use crate::call::SUPERVISOR;
use crate::call::TRACE_BITS;
use crate::call::TRAP_VECTOR;
use crate::call::VECTOR_BYTES;
use crate::call::short_frame;
use crate::call::spins;
use crate::call::stop_before;
use crate::memory::ADDRESS_MASK;
use crate::memory::Contents;
use crate::memory::Fault;
use crate::memory::Polled;
use crate::memory::Region;
use crate::memory::RegionMemory;
use crate::state::State;

/// The interrupt numbers Unicorn's m68k hands its interrupt hook, as
/// this module's tests pin them: the vector number for an exception,
/// and `RTE`'s own.
const INTERRUPT_ILLEGAL: u32 = 4;
const INTERRUPT_RTE: u32 = 0x100;
/// The trap vectors, #0 to #15 (CPU32RM 6.1.1, Table 6-1).
const TRAPS: std::ops::RangeInclusive<u32> = TRAP_VECTOR as u32..=TRAP_VECTOR as u32 + 15;

/// The instructions the oracle does or refuses in Unicorn's place
/// (CPU32RM 4.4): RTR, MOVE (SP)+, SR, STOP, and MOVEC.
const RTR: u16 = 0x4e77;
const STATUS_POP: u16 = 0x46df;
const STOP: u16 = 0x4e72;
/// MOVEC: the opcode with the direction bit clear (from the control
/// register), and its mask.
const MOVEC: u16 = 0x4e7a;
const MOVEC_MASK: u16 = 0xfffe;
/// MOVEC's extension word's control register field, and VBR's number
/// (CPU32RM 4.4, MOVEC).
const MOVEC_CONTROL_MASK: u16 = 0x0fff;
const MOVEC_VBR: u16 = 0x0801;
/// Bytes of a word and a long.
const WORD: u32 = 2;
const LONG: u32 = 4;
/// The condition codes, X N Z V C (CPU32RM 2.3.1.2, Figure 2-2).
const CCR: u16 = 0x001f;
/// SR's low byte, which the probe reads the codes in.
const LOW_BYTE: u16 = 0x00ff;

/// The oracle's page, where no call's memory is: the probe, executable
/// only.
const PROBE: u32 = 0x00f0_0000;
const PROBE_BYTES: u32 = 0x1000;
/// The probe: `MOVE CCR, D0` (CPU32RM 4.4, MOVE from CCR).
const MOVE_FROM_CCR_TO_D0: u16 = 0x42c0;
/// Unicorn's pages, which a mapping is in whole.
const PAGE: u32 = 0x1000;

/// The registers by number.
const DATA: [RegisterM68K; 8] = [
    RegisterM68K::D0,
    RegisterM68K::D1,
    RegisterM68K::D2,
    RegisterM68K::D3,
    RegisterM68K::D4,
    RegisterM68K::D5,
    RegisterM68K::D6,
    RegisterM68K::D7,
];
const ADDRESS: [RegisterM68K; 7] = [
    RegisterM68K::A0,
    RegisterM68K::A1,
    RegisterM68K::A2,
    RegisterM68K::A3,
    RegisterM68K::A4,
    RegisterM68K::A5,
    RegisterM68K::A6,
];

/// Why a hook stopped a run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stop {
    /// The interrupt hook: Unicorn's number, at the instruction at `pc`.
    Interrupt {
        /// The number.
        number: u32,
        /// The address.
        pc: u32,
    },
    /// The code hook, at an instruction the oracle does or refuses.
    Instruction {
        /// Which.
        instruction: Hooked,
        /// Its address.
        pc: u32,
    },
    /// The code hook, at the step limit.
    StepLimit,
    /// An access no memory answers.
    Fault(Fault),
    /// The code hook, at code in a writable region.
    WritableCode,
    /// An instruction fetched above the 24 address lines from a
    /// writable region's alias, which Unicorn cannot run as a device's.
    Above24,
}

/// What a mapped region is, for its aliases above the 24 address lines.
#[derive(Debug, Clone)]
enum Kind {
    /// Read only: its contents, which an alias copies.
    ReadOnly(Arc<[u8]>),
    /// Writable memory.
    Writable,
    /// A device, its index in `Shared::devices`.
    Device(usize),
}

/// A mapped region: its start, bytes and kind.
#[derive(Debug, Clone)]
struct Mapped {
    /// Its first address.
    start: u32,
    /// Its bytes.
    length: u32,
    /// What it is.
    kind: Kind,
}

/// An instruction the code hook stops a run at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Hooked {
    /// MOVE (SP)+, SR, which the oracle does (fault 4).
    StatusPop,
    /// STOP (fault 5).
    Stop,
    /// MOVEC to or from VBR (fault 7).
    VectorBase,
    /// A branch to itself, which ends the call.
    Spin,
}

/// What the hooks share with the oracle.
#[derive(Debug, Default)]
struct Shared {
    /// Why the last run stopped, if a hook stopped it.
    stop: Option<Stop>,
    /// Instructions run in this call.
    steps: usize,
    /// The most it may run.
    step_limit: usize,
    /// The probe runs: the code hook leaves it be.
    probing: bool,
    /// The image, read only, from address zero: the code hook's opcodes.
    image: Option<Arc<[u8]>>,
    /// The regions with polled bytes, mapped as devices: each its start
    /// and bytes.
    devices: Vec<(u32, Vec<u8>)>,
    /// The polled bytes.
    polled: Vec<Polled>,
    /// The regions mapped, for their aliases.
    regions: Vec<Mapped>,
}

impl Shared {
    /// A device's byte at `address`, read.
    fn device_read(&self, device: usize, address: u32) -> u8 {
        let address = address & ADDRESS_MASK;
        let (start, bytes) = &self.devices[device];
        let byte = bytes[(address - start) as usize];
        self.polled
            .iter()
            .find(|polled| polled.address == address)
            .map_or(byte, |polled| byte & polled.kept | polled.set)
    }
}

/// The engine.
type Engine<'a> = unicorn_engine::Unicorn<'a, Shared>;

/// Unicorn's 68020, with the faults' workarounds, holding one memory
/// layout at a time.
pub struct Unicorn {
    /// The engine.
    engine: Engine<'static>,
    /// The layout mapped, a region each: its contents as loaded, if
    /// read only.
    mapped: Vec<Option<Arc<[u8]>>>,
}

impl std::fmt::Debug for Unicorn {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("Unicorn").finish_non_exhaustive()
    }
}

/// A register, 32 bits.
fn get(engine: &Engine<'_>, register: RegisterM68K) -> Result<u32, uc_error> {
    // The m68k's registers are 32 bits; the engine widens them.
    engine.reg_read(register).map(|value| value as u32)
}

fn set(engine: &mut Engine<'_>, register: RegisterM68K, value: u32) -> Result<(), uc_error> {
    engine.reg_write(register, u64::from(value))
}

fn read_word(engine: &Engine<'_>, at: u32) -> Result<u16, uc_error> {
    let mut bytes = [0; WORD as usize];
    engine.mem_read(u64::from(at), &mut bytes)?;
    Ok(u16::from_be_bytes(bytes))
}

fn read_long(engine: &Engine<'_>, at: u32) -> Result<u32, uc_error> {
    let mut bytes = [0; LONG as usize];
    engine.mem_read(u64::from(at), &mut bytes)?;
    Ok(u32::from_be_bytes(bytes))
}

/// SR's system byte, from a saved context (fault 1).
fn system_byte(engine: &Engine<'_>) -> Result<u16, uc_error> {
    let copy = engine.context_init()?;
    // SR is 16 bits; the engine widens it.
    copy.reg_read(RegisterM68K::SR)
        .map(|value| value as u16 & !LOW_BYTE)
}

/// The condition codes, by the probe (fault 1), between runs; every
/// register but D0 and PC is untouched by it, and those are put back.
fn condition_codes(engine: &mut Engine<'_>) -> Result<u16, uc_error> {
    let pc = get(engine, RegisterM68K::PC)?;
    let d0 = get(engine, RegisterM68K::D0)?;
    engine.get_data_mut().probing = true;
    let probed = engine.emu_start(u64::from(PROBE), u64::from(PROBE + WORD), 0, 0);
    engine.get_data_mut().probing = false;
    probed?;
    let codes = get(engine, RegisterM68K::D0)? as u16 & LOW_BYTE;
    set(engine, RegisterM68K::D0, d0)?;
    set(engine, RegisterM68K::PC, pc)?;
    Ok(codes)
}

/// SR, between runs.
fn status(engine: &mut Engine<'_>) -> Result<u16, uc_error> {
    Ok(system_byte(engine)? | condition_codes(engine)?)
}

/// The instruction at `pc`, `opcode`, if it is one the code hook stops
/// a run at: one the oracle does or refuses, or a spin, which ends the
/// call.
fn hooked(engine: &Engine<'_>, opcode: u16, pc: u32) -> Option<Hooked> {
    let word = |at: u32| read_word(engine, at).ok();
    match opcode {
        STATUS_POP => Some(Hooked::StatusPop),
        STOP => Some(Hooked::Stop),
        _ if opcode & MOVEC_MASK == MOVEC
            && word(pc + WORD)
                .is_some_and(|extension| extension & MOVEC_CONTROL_MASK == MOVEC_VBR) =>
        {
            Some(Hooked::VectorBase)
        }
        _ if spins(word, pc) => Some(Hooked::Spin),
        _ => None,
    }
}

/// The code hook: counts the instruction, or stops the run at the step
/// limit or at an instruction the oracle does or refuses (fault 2: it
/// reads and writes no register).
fn code_hook(engine: &mut Engine<'_>, address: u64, _size: u32) {
    let shared = engine.get_data();
    if shared.probing {
        return;
    }
    // A 24-bit address space.
    let pc = address as u32;
    let opcode = shared
        .image
        .as_ref()
        .and_then(|image| image.get(pc as usize..pc as usize + WORD as usize))
        .map(|bytes| u16::from_be_bytes([bytes[0], bytes[1]]))
        .or_else(|| read_word(engine, pc).ok());
    let in_image = shared
        .image
        .as_ref()
        .is_some_and(|image| (pc as usize) < image.len());
    let writable_code = !in_image
        && shared.regions.iter().any(|region| {
            !matches!(region.kind, Kind::ReadOnly(_))
                && (region.start..region.start + region.length).contains(&(pc & ADDRESS_MASK))
        });
    let stop = if shared.steps == shared.step_limit {
        Some(Stop::StepLimit)
    } else if writable_code {
        Some(Stop::WritableCode)
    } else {
        opcode
            .and_then(|opcode| hooked(engine, opcode, pc))
            .map(|instruction| Stop::Instruction { instruction, pc })
    };
    let shared = engine.get_data_mut();
    match stop {
        Some(stop) => {
            shared.stop = Some(stop);
            let _ = engine.emu_stop();
        }
        None => shared.steps += 1,
    }
}

/// The byte at `address`, inside 24 bits, as an alias reads it.
fn alias_read(engine: &Engine<'_>, address: u32) -> u8 {
    let shared = engine.get_data();
    let device = shared.regions.iter().find_map(|region| match region.kind {
        Kind::Device(device) if (region.start..region.start + region.length).contains(&address) => {
            Some(device)
        }
        _ => None,
    });
    match device {
        Some(device) => shared.device_read(device, address),
        None => {
            let mut byte = [0];
            let _ = engine.mem_read(u64::from(address), &mut byte);
            byte[0]
        }
    }
}

/// Writes the byte at `address`, inside 24 bits, in `region`, as an
/// alias writes it.
fn alias_write(engine: &mut Engine<'_>, region: &Mapped, address: u32, value: u8) {
    match region.kind {
        Kind::Device(device) => {
            engine.get_data_mut().devices[device].1[(address - region.start) as usize] = value;
        }
        Kind::Writable => {
            let _ = engine.mem_write(u64::from(address), &[value]);
        }
        Kind::ReadOnly(_) => {}
    }
}

/// Maps the alias of the region holding `address & ADDRESS_MASK` at
/// `address`'s high byte, the 68331's 24 address lines wrapping
/// (MC68331UM 4.4.1.1): a copy of a read-only region, which runs as
/// code; a device mapping of a writable one, whose accesses go to the
/// region.  `false` if no region holds the address.
fn map_alias(engine: &mut Engine<'_>, address: u64) -> bool {
    // An address above 24 bits, of the 32 the engine has.
    let high = address as u32 & !ADDRESS_MASK;
    let low = address as u32 & ADDRESS_MASK;
    let Some(region) = engine
        .get_data()
        .regions
        .iter()
        .find(|region| (region.start..region.start + region.length).contains(&low))
        .cloned()
    else {
        return false;
    };
    let alias = u64::from(high | region.start);
    let length = u64::from(region.length);
    let mapped = match &region.kind {
        Kind::ReadOnly(bytes) => engine
            .mem_map(alias, length, Prot::READ | Prot::EXEC)
            .and_then(|()| engine.mem_write(alias, bytes)),
        _ => {
            let writes = region.clone();
            engine.mmio_map(
                alias,
                length,
                Some(move |engine: &mut Engine<'_>, offset: u64, size: usize| {
                    // Offsets are within the region.
                    let at = region.start + offset as u32;
                    (0..size as u32).fold(0, |value, index| {
                        value << 8 | u64::from(alias_read(engine, at + index))
                    })
                }),
                Some(
                    move |engine: &mut Engine<'_>, offset: u64, size: usize, value: u64| {
                        let at = writes.start + offset as u32;
                        for index in 0..size {
                            let byte = (value >> (8 * (size - 1 - index))) as u8;
                            alias_write(engine, &writes, at + index as u32, byte);
                        }
                    },
                ),
            )
        }
    };
    mapped.is_ok()
}

/// The invalid access hook: an alias above the 24 address lines mapped
/// and the access retried, or the call's fault.
fn invalid_access(
    engine: &mut Engine<'_>,
    access: MemType,
    address: u64,
    _size: usize,
    _value: i64,
) -> bool {
    let write = matches!(access, MemType::WRITE_UNMAPPED | MemType::WRITE_PROT);
    let fetch = matches!(access, MemType::FETCH_UNMAPPED | MemType::FETCH_PROT);
    let above = address > u64::from(ADDRESS_MASK);
    let unmapped = matches!(
        access,
        MemType::READ_UNMAPPED | MemType::WRITE_UNMAPPED | MemType::FETCH_UNMAPPED
    );
    if above && unmapped && map_alias(engine, address) {
        return true;
    }
    let stop = if above && fetch {
        Stop::Above24
    } else {
        Stop::Fault(Fault {
            // A 24-bit address space.
            address: address as u32 & ADDRESS_MASK,
            write,
        })
    };
    engine.get_data_mut().stop.get_or_insert(stop);
    false
}

impl Unicorn {
    /// A 68020 with the probe's page, and nothing else mapped yet.
    pub fn new() -> Result<Self, uc_error> {
        let mut engine = Engine::new_with_data(Arch::M68K, Mode::BIG_ENDIAN, Shared::default())?;
        engine.ctl_set_cpu_model(M68kCpuModel::M68020 as i32)?;
        engine.mem_map(u64::from(PROBE), u64::from(PROBE_BYTES), Prot::EXEC)?;
        engine.mem_write(u64::from(PROBE), &MOVE_FROM_CCR_TO_D0.to_be_bytes())?;
        engine.add_code_hook(1, 0, code_hook)?;
        engine.add_intr_hook(|engine: &mut Engine<'_>, number| {
            let pc = get(engine, RegisterM68K::PC).unwrap_or_default();
            engine.get_data_mut().stop = Some(Stop::Interrupt { number, pc });
            let _ = engine.emu_stop();
        })?;
        engine.add_mem_hook(HookType::MEM_INVALID, 1, 0, invalid_access)?;
        Ok(Self {
            engine,
            mapped: Vec::new(),
        })
    }

    /// Maps `memory`'s layout the first time, and loads its contents:
    /// a read-only region only the first time.  Unicorn keeps the code
    /// it translated across a write to it, as a run of a second image
    /// shows, so read-only contents that change, a new image, need a
    /// new engine.
    fn load(&mut self, memory: &RegionMemory) -> Result<(), uc_error> {
        let changed = self
            .mapped
            .iter()
            .zip(memory.regions())
            .any(|(loaded, region)| match (loaded, &region.contents) {
                (Some(loaded), Contents::ReadOnly(bytes)) => {
                    !Arc::ptr_eq(loaded, bytes) && **loaded != **bytes
                }
                _ => false,
            });
        if changed {
            *self = Self::new()?;
        }
        if self.mapped.is_empty() {
            self.map(memory)?;
        }
        assert_eq!(
            self.mapped.len(),
            memory.regions().len(),
            "one layout per oracle"
        );
        for (index, region) in memory.regions().iter().enumerate() {
            match &region.contents {
                Contents::ReadOnly(bytes) => {
                    let loaded = &mut self.mapped[index];
                    if loaded.is_none() {
                        self.engine.mem_write(u64::from(region.start), bytes)?;
                        *loaded = Some(Arc::clone(bytes));
                    }
                }
                Contents::Writable(bytes) => {
                    let shared = self.engine.get_data_mut();
                    match shared
                        .devices
                        .iter_mut()
                        .find(|(start, _)| *start == region.start)
                    {
                        Some((_, device)) => device.clone_from(bytes),
                        None => self.engine.mem_write(u64::from(region.start), bytes)?,
                    }
                }
            }
        }
        let image = memory
            .regions()
            .iter()
            .find(|region| region.start == 0)
            .and_then(|region| match &region.contents {
                Contents::ReadOnly(bytes) => Some(Arc::clone(bytes)),
                Contents::Writable(_) => None,
            });
        let shared = self.engine.get_data_mut();
        shared.image = image;
        shared.polled = memory.polled().to_vec();
        Ok(())
    }

    /// Maps `memory`'s regions: read only, writable, or, holding polled
    /// bytes, a device whose reads the oracle answers.
    fn map(&mut self, memory: &RegionMemory) -> Result<(), uc_error> {
        for region in memory.regions() {
            let start = region.start;
            let length = region.bytes().len();
            assert!(
                start.is_multiple_of(PAGE) && length.is_multiple_of(PAGE as usize),
                "a region in whole pages"
            );
            let polled = memory
                .polled()
                .iter()
                .any(|polled| (start..start + length as u32).contains(&polled.address));
            let writable = matches!(region.contents, Contents::Writable(_));
            let kind = match &region.contents {
                _ if polled => Kind::Device(self.engine.get_data().devices.len()),
                Contents::ReadOnly(bytes) => Kind::ReadOnly(Arc::clone(bytes)),
                Contents::Writable(_) => Kind::Writable,
            };
            self.engine.get_data_mut().regions.push(Mapped {
                start,
                length: length as u32,
                kind,
            });
            if polled {
                let device = self.engine.get_data().devices.len();
                self.engine
                    .get_data_mut()
                    .devices
                    .push((start, vec![0; length]));
                self.engine.mmio_map(
                    u64::from(start),
                    length as u64,
                    Some(move |engine: &mut Engine<'_>, offset: u64, size: usize| {
                        // Offsets are within the region.
                        let at = start + offset as u32;
                        (0..size as u32).fold(0, |value, index| {
                            value << 8
                                | u64::from(engine.get_data().device_read(device, at + index))
                        })
                    }),
                    Some(
                        move |engine: &mut Engine<'_>, offset: u64, size: usize, value: u64| {
                            let at = (offset as u32) as usize;
                            let bytes = &mut engine.get_data_mut().devices[device].1;
                            for index in 0..size {
                                bytes[at + index] = (value >> (8 * (size - 1 - index))) as u8;
                            }
                        },
                    ),
                )?;
            } else {
                let protection = if writable {
                    Prot::ALL
                } else {
                    Prot::READ | Prot::EXEC
                };
                self.engine
                    .mem_map(u64::from(start), length as u64, protection)?;
            }
            self.mapped.push(None);
        }
        Ok(())
    }

    /// Sets the registers to `state`, SR first so that A7 is the stack
    /// S says.
    fn set_state(&mut self, state: &State) -> Result<(), uc_error> {
        let engine = &mut self.engine;
        set(engine, RegisterM68K::SR, u32::from(state.sr))?;
        for (register, &value) in DATA.iter().zip(&state.data) {
            set(engine, *register, value)?;
        }
        for (register, &value) in ADDRESS.iter().zip(&state.address) {
            set(engine, *register, value)?;
        }
        let supervisor = state.sr & SUPERVISOR != 0;
        set(
            engine,
            RegisterM68K::A7,
            if supervisor { state.ssp } else { state.usp },
        )?;
        set(engine, RegisterM68K::PC, state.pc)
    }

    /// The registers, between runs.  Unicorn has no other stack
    /// pointer than A7 to read: the other is the call's start's.
    fn state(&mut self, started: &State) -> Result<State, uc_error> {
        let sr = status(&mut self.engine)?;
        let engine = &self.engine;
        let a7 = get(engine, RegisterM68K::A7)?;
        let (usp, ssp) = if sr & SUPERVISOR != 0 {
            (started.usp, a7)
        } else {
            (a7, started.ssp)
        };
        let mut data = [0; 8];
        for (value, register) in data.iter_mut().zip(DATA) {
            *value = get(engine, register)?;
        }
        let mut address = [0; 7];
        for (value, register) in address.iter_mut().zip(ADDRESS) {
            *value = get(engine, register)?;
        }
        Ok(State {
            data,
            address,
            usp,
            ssp,
            sr,
            pc: get(engine, RegisterM68K::PC)?,
        })
    }

    /// Memory as the call left it, in `layout`'s regions.
    fn memory(&self, layout: &RegionMemory) -> Result<RegionMemory, uc_error> {
        let shared = self.engine.get_data();
        let regions = layout
            .regions()
            .iter()
            .map(|region| {
                let contents = match &region.contents {
                    Contents::ReadOnly(bytes) => Contents::ReadOnly(Arc::clone(bytes)),
                    Contents::Writable(bytes) => Contents::Writable(
                        match shared
                            .devices
                            .iter()
                            .find(|(start, _)| *start == region.start)
                        {
                            Some((_, device)) => device.clone(),
                            None => self
                                .engine
                                .mem_read_as_vec(u64::from(region.start), bytes.len())?,
                        },
                    ),
                };
                Ok(Region {
                    start: region.start,
                    contents,
                })
            })
            .collect::<Result<Vec<_>, uc_error>>()?;
        Ok(RegionMemory::new(regions).with_polled(layout.polled().to_vec()))
    }

    /// Takes the exception through `vector` from SR `sr`, returning to
    /// `return_to`: a format $0 frame on the supervisor stack, S set and
    /// the trace bits clear, the handler from the call's table (CPU32RM
    /// 6.1.3, 6.4.1).
    fn take(&mut self, vbr: u32, vector: u8, return_to: u32) -> Result<(), uc_error> {
        let sr = status(&mut self.engine)?;
        let engine = &mut self.engine;
        set(
            engine,
            RegisterM68K::SR,
            u32::from((sr | SUPERVISOR) & !TRACE_BITS),
        )?;
        let stack = get(engine, RegisterM68K::A7)? - SHORT_FRAME_BYTES;
        engine.mem_write(u64::from(stack), &short_frame(sr, return_to, vector))?;
        set(engine, RegisterM68K::A7, stack)?;
        let handler = read_long(engine, vbr + u32::from(vector) * VECTOR_BYTES)?;
        set(engine, RegisterM68K::PC, handler)
    }

    /// RTE of a format $0 frame (CPU32RM 4.4, RTE; 6.4.1): the stack
    /// pointer first, so that SR switches the stacks after.  `false`
    /// for another format.
    fn return_from_exception(&mut self) -> Result<bool, uc_error> {
        let engine = &mut self.engine;
        let stack = get(engine, RegisterM68K::A7)?;
        let sr = read_word(engine, stack)?;
        let return_to = read_long(engine, stack + WORD)?;
        let format = read_word(engine, stack + crate::call::FORMAT_WORD_OFFSET)? >> FORMAT_SHIFT;
        if format != 0 {
            return Ok(false);
        }
        set(engine, RegisterM68K::A7, stack + SHORT_FRAME_BYTES)?;
        set(engine, RegisterM68K::SR, u32::from(sr))?;
        set(engine, RegisterM68K::PC, return_to)?;
        Ok(true)
    }

    /// RTR (CPU32RM 4.4, RTR): the condition codes, then PC, popped
    /// (fault 3).
    fn return_and_restore(&mut self) -> Result<(), uc_error> {
        let system = system_byte(&self.engine)?;
        let engine = &mut self.engine;
        let stack = get(engine, RegisterM68K::A7)?;
        let codes = read_word(engine, stack)? & CCR;
        let return_to = read_long(engine, stack + WORD)?;
        set(engine, RegisterM68K::A7, stack + WORD + LONG)?;
        set(engine, RegisterM68K::SR, u32::from(system | codes))?;
        set(engine, RegisterM68K::PC, return_to)
    }

    /// MOVE (SP)+, SR at `pc`, in supervisor state (CPU32RM 4.4, MOVE
    /// to SR): the word popped from the supervisor stack before SR
    /// switches stacks (fault 4).
    fn status_pop(&mut self, pc: u32) -> Result<(), uc_error> {
        let engine = &mut self.engine;
        let stack = get(engine, RegisterM68K::A7)?;
        let sr = read_word(engine, stack)?;
        set(engine, RegisterM68K::A7, stack + WORD)?;
        set(engine, RegisterM68K::SR, u32::from(sr))?;
        set(engine, RegisterM68K::PC, pc + WORD)
    }

    /// Runs `call` on `memory` until it ends.
    pub fn call(&mut self, call: &Call<'_>, memory: RegionMemory) -> CallEnd {
        let run = self.run(call, &memory);
        let (ending, state) = match run {
            Ok(ending) => (ending, self.state(&call.state).unwrap_or(call.state)),
            Err(_) => (Ending::Unsupported("Unicorn refused a request"), call.state),
        };
        let steps = self.engine.get_data().steps;
        let memory = self.memory(&memory).unwrap_or(memory);
        CallEnd {
            ending,
            state,
            steps,
            memory,
        }
    }

    /// `call`'s run: how it ended.
    fn run(&mut self, call: &Call<'_>, memory: &RegionMemory) -> Result<Ending, uc_error> {
        self.load(memory)?;
        self.set_state(&call.state)?;
        {
            let shared = self.engine.get_data_mut();
            shared.steps = 0;
            shared.step_limit = call.step_limit;
            shared.stop = None;
        }
        let word = |engine: &Engine<'_>, at: u32| read_word(engine, at).ok();
        loop {
            let pc = get(&self.engine, RegisterM68K::PC)?;
            if pc == call.until {
                return Ok(Ending::Returned);
            }
            if let Some(ending) = stop_before(|at| word(&self.engine, at), pc, call.handled_traps) {
                return Ok(ending);
            }
            if self.engine.get_data().steps == call.step_limit {
                return Ok(Ending::StepLimit);
            }
            let started = self
                .engine
                .emu_start(u64::from(pc), u64::from(call.until), 0, 0);
            let stop = self.engine.get_data_mut().stop.take();
            match stop {
                None => {
                    started?;
                }
                Some(Stop::StepLimit) => return Ok(Ending::StepLimit),
                Some(Stop::Fault(fault)) => return Ok(Ending::Fault(fault)),
                Some(Stop::WritableCode) => {
                    return Ok(Ending::Unsupported(
                        "code run from writable memory, which Unicorn does not translate \
                         again after a write",
                    ));
                }
                Some(Stop::Above24) => {
                    return Ok(Ending::Unsupported(
                        "a fetch above ADDR[23:0] from a writable region, whose alias \
                         Unicorn maps as a device",
                    ));
                }
                Some(Stop::Instruction { instruction, pc }) => {
                    set(&mut self.engine, RegisterM68K::PC, pc)?;
                    match instruction {
                        Hooked::Spin => continue,
                        Hooked::StatusPop if system_byte(&self.engine)? & SUPERVISOR != 0 => {
                            self.status_pop(pc)?;
                        }
                        Hooked::StatusPop => {
                            return Ok(Ending::Unsupported(
                                "MOVE (SP)+, SR in user state, a privilege violation",
                            ));
                        }
                        Hooked::Stop => {
                            return Ok(Ending::Unsupported(
                                "STOP: Unicorn runs on past it (fault 5)",
                            ));
                        }
                        Hooked::VectorBase => {
                            return Ok(Ending::Unsupported(
                                "MOVEC of VBR: Unicorn's is not the call's (fault 7)",
                            ));
                        }
                    }
                    self.engine.get_data_mut().steps += 1;
                }
                Some(Stop::Interrupt { number, pc }) => {
                    set(&mut self.engine, RegisterM68K::PC, pc)?;
                    // The code hook counted the instruction.
                    if let Some(ending) = self.interrupt(call, number, pc)? {
                        return Ok(ending);
                    }
                }
            }
        }
    }

    /// An exception Unicorn handed the interrupt hook, number `number`
    /// at the instruction at `pc`, done as the CPU would: how the call
    /// ends, if it does.
    fn interrupt(
        &mut self,
        call: &Call<'_>,
        number: u32,
        pc: u32,
    ) -> Result<Option<Ending>, uc_error> {
        match number {
            _ if TRAPS.contains(&number) => {
                // The trap vectors fit a byte.
                let trap = (number - TRAPS.start()) as u8;
                if !call.handled_traps.contains(&trap) {
                    return Ok(Some(Ending::Trap(trap)));
                }
                self.take(call.vbr, number as u8, pc + WORD)?;
                Ok(None)
            }
            INTERRUPT_RTE => Ok((!self.return_from_exception()?)
                .then_some(Ending::Unsupported("RTE of a frame not format $0"))),
            INTERRUPT_ILLEGAL if read_word(&self.engine, pc)? == RTR => {
                self.return_and_restore()?;
                Ok(None)
            }
            _ => Ok(Some(Ending::Exception {
                // Exception numbers fit a byte.
                vector: number as u8,
                pc,
            })),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use unicorn_engine::RegisterM68K;

    use super::MOVEC;
    use super::MOVEC_VBR;
    use super::RTR;
    use super::STATUS_POP;
    use super::STOP;
    use super::Unicorn;
    use super::get;
    use super::status;
    use crate::call::Call;
    use crate::call::Ending;
    use crate::call::sample;
    use crate::memory::Contents;
    use crate::memory::Fault;
    use crate::memory::RegionMemory;

    #[test]
    fn a_call_runs_its_handled_trap_and_returns() {
        let mut unicorn = Unicorn::new().unwrap();
        let end = unicorn.call(&sample::call(&[1]), sample::memory(sample::STORE));
        assert_eq!(end.ending, Ending::Returned);
        assert_eq!(end.state.data[0], 6);
        assert_eq!(end.state.ssp, sample::STACK + 4);
        assert_eq!(end.state.sr, 0x2700);
        assert_eq!(end.memory.peek_word(sample::RAM), Some(6));
        assert_eq!(end.steps, 6);
    }

    #[test]
    fn a_call_ends_at_an_unhandled_trap_and_a_fault() {
        let mut unicorn = Unicorn::new().unwrap();
        let end = unicorn.call(&sample::call(&[]), sample::memory(sample::STORE));
        assert_eq!(end.ending, Ending::Trap(1));
        assert_eq!(end.state.pc, sample::CODE + 2);
        assert_eq!(end.state.data[0], 5);
        let end = unicorn.call(&sample::call(&[1]), sample::memory(sample::STORE_TO_ROM));
        assert_eq!(
            end.ending,
            Ending::Fault(Fault {
                address: 0x100,
                write: true
            })
        );
    }

    /// Where the tests' own code goes, in the sample's ROM.
    const CODE: u32 = 0x1800;

    /// The sample's memory, `code` at `CODE`.
    fn with_code(code: &[u16]) -> RegionMemory {
        let memory = sample::memory(sample::STORE);
        let mut regions = memory.regions().to_vec();
        let mut rom = regions[0].bytes().to_vec();
        for (at, &word) in (CODE as usize..).step_by(2).zip(code) {
            rom[at..at + 2].copy_from_slice(&word.to_be_bytes());
        }
        regions[0].contents = Contents::ReadOnly(Arc::from(rom));
        RegionMemory::new(regions)
    }

    /// Code in writable memory: not comparable.
    #[test]
    fn code_in_writable_memory_is_unsupported() {
        let mut memory = sample::memory(sample::STORE);
        // MOVEQ #1, D0 in RAM.
        memory.write_word(0x3800, 0x7001);
        let mut call = sample::call(&[]);
        call.state.pc = 0x3800;
        let end = Unicorn::new().unwrap().call(&call, memory);
        assert!(
            matches!(end.ending, Ending::Unsupported(_)),
            "{:?}",
            end.ending
        );
    }

    /// The sample call, from `CODE`.
    fn from_code() -> Call<'static> {
        let mut call = sample::call(&[]);
        call.state.pc = CODE;
        call
    }

    /// Fault 1, as it shows: a read of SR after a compare does not give
    /// the codes the compare left, N and C, and the probe does.  If the
    /// first assertion fails, Unicorn is fixed: revisit the probe.
    #[test]
    fn fault_1_a_read_of_sr_after_a_compare_spoils_it_and_the_probe_does_not() {
        // MOVE #0, CCR; MOVEQ #1, D0; CMPI.L #2, D0 (CPU32RM 4.4).
        let compare = [0x44fc, 0x0000, 0x7001, 0x0c80, 0x0000, 0x0002];
        let end = u64::from(CODE) + 2 * compare.len() as u64;
        for probed in [false, true] {
            let mut unicorn = Unicorn::new().unwrap();
            unicorn.load(&with_code(&compare)).unwrap();
            unicorn.set_state(&from_code().state).unwrap();
            unicorn.engine.get_data_mut().step_limit = usize::MAX;
            unicorn
                .engine
                .emu_start(u64::from(CODE), end, 0, 0)
                .unwrap();
            let sr = if probed {
                status(&mut unicorn.engine).unwrap()
            } else {
                get(&unicorn.engine, RegisterM68K::SR).unwrap() as u16
            };
            assert_eq!(sr == 0x2709, probed, "{sr:#06x}");
        }
    }

    /// Fault 3: RTR, which the oracle does, pops the codes and returns.
    #[test]
    fn fault_3_rtr_returns_with_the_codes() {
        // MOVE.W #$001F, -(SP); RTR (CPU32RM 4.4).
        let end = Unicorn::new()
            .unwrap()
            .call(&from_code(), with_code(&[0x3f3c, 0x001f, RTR]));
        assert_eq!(end.ending, Ending::Returned);
        assert_eq!(end.state.sr, 0x271f);
        assert_eq!(end.state.ssp, sample::STACK + 4);
    }

    /// Fault 4: MOVE (SP)+, SR into user state, which the oracle does,
    /// pops the supervisor stack.
    #[test]
    fn fault_4_status_pop_pops_the_supervisor_stack() {
        // MOVE.W #0, -(SP); MOVE (SP)+, SR; TRAP #3 (CPU32RM 4.4).
        let end = Unicorn::new().unwrap().call(
            &from_code(),
            with_code(&[0x3f3c, 0x0000, STATUS_POP, 0x4e43]),
        );
        assert_eq!(end.ending, Ending::Trap(3));
        assert_eq!(end.state.sr, 0);
        assert_eq!(end.state.ssp, sample::STACK);
        assert_eq!(end.state.pc, CODE + 6);
    }

    /// Faults 5 and 7: not comparable.
    #[test]
    fn stop_and_movec_of_vbr_are_unsupported() {
        // STOP #$2700; MOVEC VBR, D0 (CPU32RM 4.4).
        for code in [[STOP, 0x2700, 0x4e71], [MOVEC, MOVEC_VBR, 0x4e71]] {
            let end = Unicorn::new().unwrap().call(&from_code(), with_code(&code));
            assert!(
                matches!(end.ending, Ending::Unsupported(_)),
                "{code:04x?}: {:?}",
                end.ending
            );
        }
    }

    /// Above the 24 address lines, memory repeats (MC68331UM 4.4.1.1).
    #[test]
    fn accesses_above_24_bits_reach_their_aliases() {
        // MOVE.L ($01001000).L, D0; MOVE.L D0, ($FF002000).L; RTS: the
        // sample's first long, MOVEQ #5, D0; TRAP #1, to RAM.
        let code = [0x2039, 0x0100, 0x1000, 0x23c0, 0xff00, 0x2000, 0x4e75];
        let end = Unicorn::new().unwrap().call(&from_code(), with_code(&code));
        assert_eq!(end.ending, Ending::Returned);
        assert_eq!(end.state.data[0], 0x7005_4e41);
        assert_eq!(end.memory.peek_word(sample::RAM), Some(0x7005));
        // MOVE.L D0, ($01000100).L: a write to ROM's alias faults.
        let end = Unicorn::new()
            .unwrap()
            .call(&from_code(), with_code(&[0x23c0, 0x0100, 0x0100]));
        assert_eq!(
            end.ending,
            Ending::Fault(Fault {
                address: 0x100,
                write: true
            })
        );
    }

    #[test]
    fn a_spin_ends_a_call() {
        // MOVEQ #1, D0; BRA.S *.
        let end = Unicorn::new()
            .unwrap()
            .call(&from_code(), with_code(&[0x7001, 0x60fe]));
        assert_eq!(end.ending, Ending::Spin);
        assert_eq!(end.state.pc, CODE + 2);
        assert_eq!(end.state.data[0], 1);
    }
}
