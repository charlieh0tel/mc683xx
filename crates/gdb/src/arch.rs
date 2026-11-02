//! The CPU32 as gdb's m68k sees it: the registers of the
//! `org.gnu.gdb.m68k.core` feature, in its order, and the CPU32's own
//! supervisor registers in a feature of ours ([docs/gdb.md,
//! Registers](https://github.com/charlieh0tel/mc683xx/blob/main/docs/gdb.md#registers)).
//! The CPU32 has no FPU (CPU32RM 1.1), so the description has no
//! floating-point feature and gdb shows no FP registers.

use std::array;
use std::convert::Infallible;

use gdbstub::arch::Arch;
use mc683xx_cpu32::registers::Registers;
use mc683xx_target::function_code::FunctionCode;
use mc683xx_target::register::Register;
use mc683xx_target::register::SystemRegister;

#[cfg(test)]
mod tests;

/// The target description gdb reads (`qXfer:features:read`): gdb's m68k
/// core registers, D0-D7, A0-A5, A6 as `fp`, the active stack pointer
/// as `sp`, SR as `ps` and PC, then USP, SSP, VBR, SFC and DFC.
const TARGET_XML: &str = r#"<?xml version="1.0"?>
<!DOCTYPE target SYSTEM "gdb-target.dtd">
<target version="1.0">
  <architecture>m68k:cpu32</architecture>
  <feature name="org.gnu.gdb.m68k.core">
    <reg name="d0" bitsize="32" type="int32"/>
    <reg name="d1" bitsize="32" type="int32"/>
    <reg name="d2" bitsize="32" type="int32"/>
    <reg name="d3" bitsize="32" type="int32"/>
    <reg name="d4" bitsize="32" type="int32"/>
    <reg name="d5" bitsize="32" type="int32"/>
    <reg name="d6" bitsize="32" type="int32"/>
    <reg name="d7" bitsize="32" type="int32"/>
    <reg name="a0" bitsize="32" type="data_ptr"/>
    <reg name="a1" bitsize="32" type="data_ptr"/>
    <reg name="a2" bitsize="32" type="data_ptr"/>
    <reg name="a3" bitsize="32" type="data_ptr"/>
    <reg name="a4" bitsize="32" type="data_ptr"/>
    <reg name="a5" bitsize="32" type="data_ptr"/>
    <reg name="fp" bitsize="32" type="data_ptr"/>
    <reg name="sp" bitsize="32" type="data_ptr"/>
    <reg name="ps" bitsize="32" type="int32"/>
    <reg name="pc" bitsize="32" type="code_ptr"/>
  </feature>
  <feature name="org.mc683xx.cpu32.system">
    <reg name="usp" bitsize="32" type="data_ptr" group="system"/>
    <reg name="ssp" bitsize="32" type="data_ptr" group="system"/>
    <reg name="vbr" bitsize="32" type="data_ptr" group="system"/>
    <reg name="sfc" bitsize="32" type="int32" group="system"/>
    <reg name="dfc" bitsize="32" type="int32" group="system"/>
  </feature>
</target>
"#;

/// Bytes in each register in a `g` packet.
const REGISTER_BYTES: usize = 4;
/// Registers in the core feature: D0-D7, A0-A7, PS, PC.
const CORE_REGISTERS: usize = 18;
/// Registers in our system feature: USP, SSP, VBR, SFC, DFC.
const SYSTEM_REGISTERS: usize = 5;
/// The core feature's A0.
const A0: usize = 8;
/// The core feature's SP, A7.
const SP: usize = 15;
/// The core feature's PS, SR.
const PS: usize = 16;
/// The core feature's PC.
const PC: usize = 17;
/// The system feature's USP.
const USP: usize = 0;
/// The system feature's SSP.
const SSP: usize = 1;
/// The system feature's VBR.
const VBR: usize = 2;
/// The system feature's SFC.
const SFC: usize = 3;
/// The system feature's DFC.
const DFC: usize = 4;

/// Where a register of the description lives: a data or address
/// register, or a system register, as BDM names them (CPU32RM 7.2.8).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Location {
    /// D0-D7, A0-A7; A7 the active stack pointer.
    Register(Register),
    /// SR, the PC (RPC), USP, SSP, VBR, SFC, DFC.
    System(SystemRegister),
}

/// The core feature's registers' locations, in its order.
const CORE_LOCATIONS: [Location; CORE_REGISTERS] = [
    Location::Register(Register::D0),
    Location::Register(Register::D1),
    Location::Register(Register::D2),
    Location::Register(Register::D3),
    Location::Register(Register::D4),
    Location::Register(Register::D5),
    Location::Register(Register::D6),
    Location::Register(Register::D7),
    Location::Register(Register::A0),
    Location::Register(Register::A1),
    Location::Register(Register::A2),
    Location::Register(Register::A3),
    Location::Register(Register::A4),
    Location::Register(Register::A5),
    Location::Register(Register::A6),
    Location::Register(Register::A7),
    Location::System(SystemRegister::Sr),
    Location::System(SystemRegister::Rpc),
];

/// The system feature's registers' locations, in its order.
const SYSTEM_LOCATIONS: [Location; SYSTEM_REGISTERS] = [
    Location::System(SystemRegister::Usp),
    Location::System(SystemRegister::Ssp),
    Location::System(SystemRegister::Vbr),
    Location::System(SystemRegister::Sfc),
    Location::System(SystemRegister::Dfc),
];

/// The register at `location` in `registers`; PCC, ATEMP and FAR,
/// which the description lacks, read zero.
fn get(registers: &Registers, location: Location) -> u32 {
    match location {
        Location::Register(register) => {
            let number = usize::from(register.code());
            match number {
                0..8 => registers.data[number],
                _ => registers.address(number - A0),
            }
        }
        Location::System(register) => match register {
            SystemRegister::Sr => registers.sr().into(),
            SystemRegister::Rpc => registers.pc,
            SystemRegister::Usp => registers.usp,
            SystemRegister::Ssp => registers.ssp,
            SystemRegister::Vbr => registers.vbr,
            SystemRegister::Sfc => registers.sfc.bits().into(),
            SystemRegister::Dfc => registers.dfc.bits().into(),
            SystemRegister::Pcc | SystemRegister::Atemp | SystemRegister::Far => 0,
        },
    }
}

/// Writes `value` to the register at `location` in `registers`: SR's
/// low word, SFC's and DFC's low three bits; PCC, ATEMP and FAR not at
/// all.
fn set(registers: &mut Registers, location: Location, value: u32) {
    match location {
        Location::Register(register) => {
            let number = usize::from(register.code());
            match number {
                0..8 => registers.data[number] = value,
                _ => registers.set_address(number - A0, value),
            }
        }
        Location::System(register) => match register {
            SystemRegister::Sr => registers.set_sr(value as u16),
            SystemRegister::Rpc => registers.pc = value,
            SystemRegister::Usp => registers.usp = value,
            SystemRegister::Ssp => registers.ssp = value,
            SystemRegister::Vbr => registers.vbr = value,
            SystemRegister::Sfc => registers.sfc = FunctionCode::new(value),
            SystemRegister::Dfc => registers.dfc = FunctionCode::new(value),
            SystemRegister::Pcc | SystemRegister::Atemp | SystemRegister::Far => {}
        },
    }
}

/// The CPU32 for gdbstub.
#[derive(Debug)]
pub(crate) enum Cpu32 {}

impl Arch for Cpu32 {
    type Usize = u32;
    type Registers = GdbRegisters;
    type BreakpointKind = usize;
    type RegId = ();

    fn target_description_xml() -> Option<&'static str> {
        Some(TARGET_XML)
    }
}

/// The registers in the description's order, as a `g` packet carries
/// them, big-endian.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct GdbRegisters {
    /// The core feature's.
    core: [u32; CORE_REGISTERS],
    /// How many of the core feature's, from the first, are given: all
    /// but from a short `G` packet.
    given: usize,
    /// Whether a short `G` packet ended within a register.
    ragged: bool,
    /// The system feature's; `None` from a `G` packet without them (a
    /// gdb that did not read the description).
    system: Option<[u32; SYSTEM_REGISTERS]>,
}

impl GdbRegisters {
    /// The registers as gdb sees them, each read from its location by
    /// `read`, in the description's order; the first error ends it.
    pub(crate) fn read<E>(mut read: impl FnMut(Location) -> Result<u32, E>) -> Result<Self, E> {
        let mut core = [0; CORE_REGISTERS];
        for (value, &location) in core.iter_mut().zip(&CORE_LOCATIONS) {
            *value = read(location)?;
        }
        let mut system = [0; SYSTEM_REGISTERS];
        for (value, &location) in system.iter_mut().zip(&SYSTEM_LOCATIONS) {
            *value = read(location)?;
        }
        Ok(Self {
            core,
            given: CORE_REGISTERS,
            ragged: false,
            system: Some(system),
        })
    }

    /// The CPU's registers as gdb sees them.
    pub(crate) fn of(registers: &Registers) -> Self {
        let read: Result<Self, Infallible> = Self::read(|location| Ok(get(registers, location)));
        match read {
            Ok(read) => read,
            Err(never) => match never {},
        }
    }

    /// Whether they came from a `G` packet that ended within a
    /// register, which the stub refuses rather than guess at.
    pub(crate) const fn is_ragged(&self) -> bool {
        self.ragged
    }

    /// The writes that make registers `before` these, in order: those
    /// given that gdb changed, the rest as they are.  SR goes first of
    /// all but the data registers, so SP is the stack pointer the new S
    /// selects; as SP, USP and SSP are written only if gdb changed them,
    /// a change to one is not undone by the other's old value.
    pub(crate) fn writes(&self, before: &Self) -> Vec<(Location, u32)> {
        let core: [u32; CORE_REGISTERS] = array::from_fn(|index| {
            if index < self.given {
                self.core[index]
            } else {
                before.core[index]
            }
        });
        let mut writes: Vec<(Location, u32)> = (0..A0)
            .chain([PS])
            .chain(A0..SP)
            .chain([PC])
            .filter(|&index| core[index] != before.core[index])
            .map(|index| (CORE_LOCATIONS[index], core[index]))
            .collect();
        if let (Some(system), Some(old)) = (self.system, before.system) {
            for index in [USP, SSP, VBR, SFC, DFC] {
                if system[index] != old[index] {
                    writes.push((SYSTEM_LOCATIONS[index], system[index]));
                }
            }
        }
        if core[SP] != before.core[SP] {
            writes.push((CORE_LOCATIONS[SP], core[SP]));
        }
        writes
    }

    /// Writes them to the CPU's registers, as [`writes`](Self::writes)
    /// orders them.  SR's unimplemented bits stay clear.
    pub(crate) fn store(&self, registers: &mut Registers) {
        for (location, value) in self.writes(&Self::of(registers)) {
            set(registers, location, value);
        }
    }
}

/// `bytes` as big-endian words.
fn words(bytes: &[u8]) -> Vec<u32> {
    bytes
        .as_chunks::<REGISTER_BYTES>()
        .0
        .iter()
        .map(|&bytes| u32::from_be_bytes(bytes))
        .collect()
}

impl gdbstub::arch::Registers for GdbRegisters {
    type ProgramCounter = u32;

    fn pc(&self) -> u32 {
        self.core[PC]
    }

    fn gdb_serialize(&self, mut write_byte: impl FnMut(Option<u8>)) {
        let system = self.system.iter().flatten();
        for byte in self
            .core
            .iter()
            .chain(system)
            .flat_map(|word| word.to_be_bytes())
        {
            write_byte(Some(byte));
        }
    }

    /// A `G` packet's registers: the core's, then the system feature's
    /// if the packet is exactly as long as the description.  A gdb
    /// without the description sends FP registers after the core's,
    /// which the CPU32 lacks; they are ignored.  The gdb manual
    /// (Packets, `G`) defines only the whole block: a shorter packet
    /// gives the registers it holds whole, from D0, and leaves the rest
    /// as they are.  Nothing fails here, where gdbstub would end the
    /// session; a packet that ends within a register is refused when
    /// stored, with an error reply.
    fn gdb_deserialize(&mut self, bytes: &[u8]) -> Result<(), ()> {
        let core_bytes = CORE_REGISTERS * REGISTER_BYTES;
        let core = &bytes[..bytes.len().min(core_bytes)];
        let given = words(core);
        self.core[..given.len()].copy_from_slice(&given);
        self.given = given.len();
        self.ragged = !core.len().is_multiple_of(REGISTER_BYTES);
        self.system = if bytes.len() == core_bytes + SYSTEM_REGISTERS * REGISTER_BYTES {
            words(&bytes[core_bytes..]).try_into().ok()
        } else {
            None
        };
        Ok(())
    }
}
