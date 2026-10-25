//! Differential runs against Musashi's 68020: random registers and
//! memory, one instruction on each, registers, SR and memory compared.
//! The 68020 is not a CPU32; `differences` lists where they part by
//! design.

use cpu32_oracle::memory::ADDRESS_MASK;
use cpu32_oracle::memory::SparseMemory;
use cpu32_oracle::musashi::Musashi;
use cpu32_oracle::random::SplitMix64;
use cpu32_oracle::state::State;
use mc683xx_cpu32::bus::Bus;
use mc683xx_cpu32::bus::BusAccess;
use mc683xx_cpu32::bus::BusError;
use mc683xx_cpu32::bus::Read;
use mc683xx_cpu32::cpu::Cpu;
use mc683xx_cpu32::cycles::Cycles;
use mc683xx_cpu32::exception::Exception;
use mc683xx_cpu32::memory::Memory;
use mc683xx_cpu32::registers::status;
use mc683xx_cpu32::size::Size;

use crate::differences;
use crate::machine;
use crate::machine::ADDRESS_BITS;
use crate::machine::Ended;

/// The SR bits a random state sets: S, the interrupt mask and the
/// condition codes.  Not the trace bits, which neither side traces yet,
/// nor the 68020's M bit, which the CPU32 lacks (CPU32RM 2.3.1.3).
const RANDOM_SR: u16 = status::S | status::INTERRUPT_MASK | status::CCR;
/// One in this many address registers is left odd; the rest are made
/// even, since an odd operand is an address error on the CPU32 and
/// excluded.
const ODD_ADDRESS_ODDS: u64 = 8;

/// The external bus over a `SparseMemory`: every cycle the core makes,
/// at no extra cost.
#[derive(Debug)]
struct SparseBus(SparseMemory);

impl Bus for SparseBus {
    fn read(&mut self, access: BusAccess) -> Result<Read, BusError> {
        let at = access.address.0;
        let value = match access.size {
            Size::Byte => u32::from(self.0.read_byte(at)),
            Size::Word | Size::Long => u32::from(self.0.read_word(at)),
        };
        Ok(Read {
            value,
            stall: Cycles::ZERO,
        })
    }

    fn write(&mut self, access: BusAccess, value: u32) -> Result<Cycles, BusError> {
        let at = access.address.0;
        match access.size {
            Size::Byte => self.0.write_byte(at, value as u8),
            Size::Word | Size::Long => self.0.write_word(at, value as u16),
        }
        Ok(Cycles::ZERO)
    }
}

/// What one trial came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// Both did the same.
    Agreed,
    /// The core does not implement the opcode, or not with these
    /// extension words.
    Unimplemented,
    /// A known difference, with its reason.
    Excluded(&'static str),
    /// The core took an exception, whose CPU32 frame Musashi's 68020
    /// does not build, so there is nothing to compare.
    Exception(Exception),
    /// They differ: how.
    Disagreed(Vec<String>),
}

/// A core whose every cycle goes to the bus, and Musashi.
#[derive(Debug)]
pub struct Differential {
    /// The core.  Each trial gets a new one, on the same memory, which
    /// has no pages mapped.
    cpu: Cpu,
    /// Musashi, held for the run.
    musashi: Musashi,
}

impl Default for Differential {
    fn default() -> Self {
        Self::new()
    }
}

impl Differential {
    /// A differential runner; it holds Musashi until dropped.
    #[must_use]
    pub fn new() -> Self {
        let memory = Memory::new(ADDRESS_BITS).expect("24 address lines");
        Self {
            cpu: Cpu::new(memory),
            musashi: Musashi::lock(),
        }
    }

    /// Runs `opcode` once from a state and memory drawn from `random`.
    /// The state is part of a disagreement's report.
    pub fn trial(&mut self, opcode: u16, random: &mut SplitMix64) -> (State, Verdict) {
        let state = random_state(random);
        let mut memory = SparseMemory::new(random.next_u64());
        memory.write_word(state.pc, opcode);
        let extension = memory.read_word(state.pc.wrapping_add(2));
        if let Some(reason) = differences::differential(opcode, extension, None) {
            return (state, Verdict::Excluded(reason));
        }
        machine::renew(&mut self.cpu);
        machine::load(&mut self.cpu.registers, &state);
        let mut bus = SparseBus(memory.clone());
        let verdict = match machine::step(&mut self.cpu, &mut bus) {
            Ended::Unimplemented => Verdict::Unimplemented,
            Ended::Exception(exception) => {
                differences::differential(opcode, extension, Some(&exception))
                    .map_or(Verdict::Exception(exception), Verdict::Excluded)
            }
            Ended::Halted(halt) => Verdict::Disagreed(vec![format!("the core halted: {halt:?}")]),
            Ended::Completed => {
                let (expected, expected_memory) = self.musashi.step(&state, memory);
                if let Some(reason) = differences::differential_result(expected.sr) {
                    return (state, Verdict::Excluded(reason));
                }
                let actual = machine::capture(&self.cpu.registers);
                if let Some(reason) = differences::differential_divide(opcode, actual.sr) {
                    return (state, Verdict::Excluded(reason));
                }
                let defined = !differences::undefined_flags(opcode, actual.sr);
                let expected = State {
                    sr: expected.sr & defined,
                    ..expected
                };
                let actual = State {
                    sr: actual.sr & defined,
                    ..actual
                };
                let mut differences = expected.differences(&actual);
                differences.extend(expected_memory.differences(&bus.0).into_iter().map(
                    |(address, expected, actual)| {
                        format!(
                            "memory {address:#08x}: expected {expected:#04x}, got {actual:#04x}"
                        )
                    },
                ));
                if differences.is_empty() {
                    Verdict::Agreed
                } else {
                    Verdict::Disagreed(differences)
                }
            }
        };
        (state, verdict)
    }
}

/// An address register's value: random, usually even.
const fn random_address(random: &mut SplitMix64) -> u32 {
    let address = random.next_u32();
    if random.one_in(ODD_ADDRESS_ODDS) {
        address
    } else {
        address & !1
    }
}

/// Random registers, PC even and inside the 24-bit space.
fn random_state(random: &mut SplitMix64) -> State {
    State {
        data: std::array::from_fn(|_| random.next_u32()),
        address: std::array::from_fn(|_| random_address(random)),
        usp: random_address(random),
        ssp: random_address(random),
        sr: random.next_u32() as u16 & RANDOM_SR,
        pc: random.next_u32() & ADDRESS_MASK & !1,
    }
}
