//! The CPU32 as gdb's m68k sees it: the registers of the
//! `org.gnu.gdb.m68k.core` feature, in its order, and the CPU32's own
//! supervisor registers in a feature of ours ([docs/gdb.md,
//! Registers](https://github.com/charlieh0tel/mc683xx/blob/main/docs/gdb.md#registers)).
//! The CPU32 has no FPU (CPU32RM 1.1), so the description has no
//! floating-point feature and gdb shows no FP registers.

use std::array;

use gdbstub::arch::Arch;
use mc683xx_cpu32::function_code::FunctionCode;
use mc683xx_cpu32::registers::Registers;

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
    /// The CPU's registers as gdb sees them.
    pub(crate) fn of(registers: &Registers) -> Self {
        let mut core = [0; CORE_REGISTERS];
        core[..A0].copy_from_slice(&registers.data);
        for (number, value) in core[A0..=SP].iter_mut().enumerate() {
            *value = registers.address(number);
        }
        core[PS] = registers.sr().into();
        core[PC] = registers.pc;
        let system = [
            registers.usp,
            registers.ssp,
            registers.vbr,
            registers.sfc.bits().into(),
            registers.dfc.bits().into(),
        ];
        Self {
            core,
            given: CORE_REGISTERS,
            ragged: false,
            system: Some(system),
        }
    }

    /// Whether they came from a `G` packet that ended within a
    /// register, which the stub refuses rather than guess at.
    pub(crate) const fn is_ragged(&self) -> bool {
        self.ragged
    }

    /// Writes them to the CPU's registers: those given, the rest as
    /// they are.  SR goes first, so SP is the stack pointer the new S
    /// selects; SP, USP and SSP are written only if gdb changed them, so
    /// a change to one is not undone by the other's old value.  SR's
    /// unimplemented bits stay clear.
    pub(crate) fn store(&self, registers: &mut Registers) {
        let before = Self::of(registers);
        let core: [u32; CORE_REGISTERS] = array::from_fn(|index| {
            if index < self.given {
                self.core[index]
            } else {
                before.core[index]
            }
        });
        registers.data.copy_from_slice(&core[..A0]);
        registers.set_sr(core[PS] as u16);
        for (number, &value) in core[A0..SP].iter().enumerate() {
            registers.set_address(number, value);
        }
        registers.pc = core[PC];
        if let (Some(system), Some(old)) = (self.system, before.system) {
            if system[USP] != old[USP] {
                registers.usp = system[USP];
            }
            if system[SSP] != old[SSP] {
                registers.ssp = system[SSP];
            }
            registers.vbr = system[VBR];
            registers.sfc = FunctionCode::new(system[SFC]);
            registers.dfc = FunctionCode::new(system[DFC]);
        }
        if core[SP] != before.core[SP] {
            registers.set_address(SP - A0, core[SP]);
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
