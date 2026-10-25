//! Runs SingleStepTests' `m68000` cases (github.com/SingleStepTests/
//! m68000, MIT) on the core: one instruction from the initial state,
//! then the registers, SR and RAM compared with the final state.  Cycle
//! counts are not: the 68000's timing is not the CPU32's.
//!
//! Cases where the 68000 takes an exception are skipped, since its
//! frames differ from the CPU32's (CPU32RM 6.4), as are the known
//! differences in `differences`.

use std::collections::BTreeMap;
use std::collections::BTreeSet;

use cpu32_oracle::state::State;
use mc683xx_cpu32::address::Address;
use mc683xx_cpu32::bus::Bus;
use mc683xx_cpu32::bus::BusAccess;
use mc683xx_cpu32::bus::BusError;
use mc683xx_cpu32::bus::PortSize;
use mc683xx_cpu32::bus::Read;
use mc683xx_cpu32::cpu::Cpu;
use mc683xx_cpu32::cycles::Cycles;
use mc683xx_cpu32::memory::Backing;
use mc683xx_cpu32::memory::Memory;
use mc683xx_cpu32::memory::PAGE_BYTES;
use mc683xx_cpu32::memory::Page;
use mc683xx_cpu32::memory::RegionId;
use mc683xx_cpu32::memory::Space;

use crate::decode::Case;
use crate::differences;
use crate::extension;
use crate::machine;
use crate::machine::ADDRESS_BITS;
use crate::machine::Ended;

/// The 68000's PC in the tests is its next prefetch address, two words
/// past the instruction (the tests' README).
pub(crate) const PREFETCH_BYTES: u32 = 4;
/// STOP's file.
const STOP: &str = "STOP";
/// SR's S bit (CPU32RM 2.3.1.3, Figure 2-3).
const SUPERVISOR: u16 = 1 << 13;
/// The vectors whose entry a 68000 exception could have fetched: all
/// but the reset vectors (CPU32RM 6.1.1, Table 6-1).
const VECTORS: std::ops::Range<u32> = 2..256;
/// Bytes in a vector.
const VECTOR_BYTES: u32 = 4;
/// What a case's name shows of an indexed mode, `(d8, An, Xn)` or
/// `(d8, PC, Xn)`.
const INDEXED: &str = "Xn)";
/// The clocks of a RAM cycle: the fastest (CPU32RM 8.1.5).
const RAM_CLOCKS: u8 = 2;

/// A bus with nothing on it: an access outside the case's RAM is a bus
/// error, so a stray access shows.
#[derive(Debug)]
struct NoBus;

impl Bus for NoBus {
    fn read(&mut self, _access: BusAccess) -> Result<Read, BusError> {
        Err(BusError)
    }

    fn write(&mut self, _access: BusAccess, _value: u32) -> Result<Cycles, BusError> {
        Err(BusError)
    }
}

/// What one case came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// The core did as the 68000 did.
    Passed,
    /// The 68000 took an exception: skipped.
    Exception,
    /// The core does not implement the instruction: skipped.
    Unimplemented,
    /// A known difference, with its reason: skipped.
    Excluded(&'static str),
    /// The core did otherwise: what differed.
    Failed(Vec<String>),
}

/// The tallies of one file's outcomes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Tally {
    /// Cases passed.
    pub passed: usize,
    /// Cases where the 68000 took an exception.
    pub exception: usize,
    /// Cases the core does not implement.
    pub unimplemented: usize,
    /// Cases excluded as known differences.
    pub excluded: usize,
    /// The reasons for the exclusions.
    pub reasons: BTreeSet<&'static str>,
    /// Cases failed, with what differed.
    pub failed: Vec<(String, Vec<String>)>,
}

impl Tally {
    /// Counts `outcome` of the case named `name`.
    pub fn count(&mut self, name: &str, outcome: Outcome) {
        match outcome {
            Outcome::Passed => self.passed += 1,
            Outcome::Exception => self.exception += 1,
            Outcome::Unimplemented => self.unimplemented += 1,
            Outcome::Excluded(reason) => {
                self.excluded += 1;
                self.reasons.insert(reason);
            }
            Outcome::Failed(differences) => self.failed.push((name.to_owned(), differences)),
        }
    }

    /// Adds `other`'s tallies to these.
    pub fn add(&mut self, other: Self) {
        self.passed += other.passed;
        self.exception += other.exception;
        self.unimplemented += other.unimplemented;
        self.excluded += other.excluded;
        self.reasons.extend(other.reasons);
        self.failed.extend(other.failed);
    }
}

/// A core on 24 bits of RAM, which each case maps only where it has
/// RAM.
#[derive(Debug)]
pub struct Runner {
    /// The core.  Each case gets a new one, on the same memory, so
    /// nothing carries over.
    cpu: Cpu,
    /// RAM: the whole address space, each address at its own offset.
    region: RegionId,
}

impl Default for Runner {
    fn default() -> Self {
        Self::new()
    }
}

impl Runner {
    /// A runner, its RAM all zero and unmapped.
    #[must_use]
    pub fn new() -> Self {
        let mut memory = Memory::new(ADDRESS_BITS).expect("24 address lines");
        let region = memory.add_region(vec![0; 1 << ADDRESS_BITS]);
        Self {
            cpu: Cpu::new(memory),
            region,
        }
    }

    /// Runs `case`, from the file named `file`.
    pub fn run(&mut self, file: &str, case: &Case) -> Outcome {
        if took_exception(case) {
            return Outcome::Exception;
        }
        if let Some(reason) = differences::single_step(file, case) {
            return Outcome::Excluded(reason);
        }
        let (initial, expected) = normalized_ram(case);
        let pages: BTreeSet<u32> = initial
            .keys()
            .chain(expected.keys())
            .map(|address| address / PAGE_BYTES * PAGE_BYTES)
            .collect();
        self.map(&pages, &initial);
        // The cases end before the trace exception T1 calls for, which
        // the core takes as part of the instruction (CPU32RM 6.2.10): it
        // runs with T1 clear, and T1 is put back unless the instruction
        // replaced SR.
        let traced = case.initial.registers.sr & differences::TRACE != 0;
        let state = State {
            pc: case.initial.registers.pc.wrapping_sub(PREFETCH_BYTES),
            sr: case.initial.registers.sr & !differences::TRACE,
            ..case.initial.registers
        };
        machine::renew(&mut self.cpu);
        machine::load(&mut self.cpu.registers, &state);
        let ended = machine::step(&mut self.cpu, &mut NoBus);
        let outcome = match ended {
            Ended::Completed => {
                // STOP prefetches nothing more: the case's PC is the
                // next instruction's.
                let prefetch = if file == STOP { 0 } else { PREFETCH_BYTES };
                let mut actual = machine::capture(&self.cpu.registers);
                if traced && !differences::REPLACES_SR.contains(&file) {
                    actual.sr |= differences::TRACE;
                }
                // The 68000 has no T0: what sets it sets nothing there.
                actual.sr &= !differences::TRACE_ON_CHANGE_OF_FLOW;
                let defined = !differences::undefined_flags(differences::opcode(case), actual.sr);
                actual.sr &= defined;
                let expected_state = State {
                    pc: case.expected.registers.pc.wrapping_sub(prefetch),
                    sr: case.expected.registers.sr & defined,
                    ..case.expected.registers
                };
                let mut differences = expected_state.differences(&actual);
                differences.extend(self.ram_differences(&pages, &expected));
                if differences.is_empty() {
                    Outcome::Passed
                } else {
                    Outcome::Failed(differences)
                }
            }
            Ended::Unimplemented => Outcome::Unimplemented,
            Ended::Exception(exception) => {
                Outcome::Failed(vec![format!("the core raised {exception:?}")])
            }
            Ended::Halted(halt) => Outcome::Failed(vec![format!("the core halted: {halt:?}")]),
        };
        self.unmap(&pages);
        outcome
    }

    /// A new core whose memory maps `pages` as RAM, in both spaces,
    /// holding `bytes`.
    fn map(&mut self, pages: &BTreeSet<u32>, bytes: &BTreeMap<u32, u8>) {
        machine::renew(&mut self.cpu);
        let memory = &mut self.cpu.memory;
        let ram = memory.region_mut(self.region);
        for (&address, &byte) in bytes {
            ram[address as usize] = byte;
        }
        for &start in pages {
            let backing = Some(Backing {
                region: self.region,
                offset: start,
            });
            let page = Page {
                read: backing,
                write: backing,
                port: PortSize::Bits16,
                clocks: RAM_CLOCKS,
            };
            for space in [Space::User, Space::Supervisor] {
                memory
                    .map(space, Address(start), PAGE_BYTES, page)
                    .expect("a page inside 24 bits");
            }
        }
    }

    /// Clears and unmaps `pages`.
    fn unmap(&mut self, pages: &BTreeSet<u32>) {
        let ram = self.cpu.memory.region_mut(self.region);
        for &start in pages {
            ram[start as usize..(start + PAGE_BYTES) as usize].fill(0);
        }
        self.cpu.memory.clear();
    }

    /// Each byte of `pages` that is not as `expected` says (zero where
    /// it says nothing), as a line for a report.
    fn ram_differences(&self, pages: &BTreeSet<u32>, expected: &BTreeMap<u32, u8>) -> Vec<String> {
        let ram = self.cpu.memory.region(self.region);
        pages
            .iter()
            .flat_map(|&start| start..start + PAGE_BYTES)
            .filter_map(|address| {
                let want = expected.get(&address).copied().unwrap_or(0);
                let got = ram[address as usize];
                (want != got)
                    .then(|| format!("RAM {address:#08x}: expected {want:#04x}, got {got:#04x}"))
            })
            .collect()
    }
}

/// Whether the 68000 took an exception: it ended in supervisor state
/// with PC two words past an entry of the vector table it was given.
fn took_exception(case: &Case) -> bool {
    let words: BTreeMap<u32, u16> = case.initial.ram.iter().copied().collect();
    let expected = &case.expected.registers;
    let next_pc = expected.pc.wrapping_sub(PREFETCH_BYTES);
    expected.sr & SUPERVISOR != 0
        && VECTORS.into_iter().any(|vector| {
            let at = vector * VECTOR_BYTES;
            match (words.get(&at), words.get(&(at + 2))) {
                (Some(&high), Some(&low)) => u32::from(high) << 16 | u32::from(low) == next_pc,
                _ => false,
            }
        })
}

/// The case's initial and final RAM as bytes, its brief extension words
/// as the 68000 read them (see `extension`).
fn normalized_ram(case: &Case) -> (BTreeMap<u32, u8>, BTreeMap<u32, u8>) {
    let mut initial: BTreeMap<u32, u16> = case.initial.ram.iter().copied().collect();
    let mut expected: BTreeMap<u32, u16> = case.expected.ram.iter().copied().collect();
    let start = case.initial.registers.pc.wrapping_sub(PREFETCH_BYTES);
    // Only a case with an indexed mode, which its name shows, has a brief
    // extension word to look for.
    if case.name.contains(INDEXED)
        && let Some(&opcode) = initial.get(&start)
    {
        for offset in extension::brief_extension_words(opcode) {
            let at = start.wrapping_add(2 * offset as u32);
            if let Some(word) = initial.get_mut(&at) {
                let original = *word;
                *word = extension::as_68000_reads(original);
                // The instruction may have overwritten it.
                if let Some(after) = expected.get_mut(&at)
                    && *after == original
                {
                    *after = *word;
                }
            }
        }
    }
    let bytes = |words: BTreeMap<u32, u16>| {
        words
            .into_iter()
            .flat_map(|(at, word)| {
                let [high, low] = word.to_be_bytes();
                [(at, high), (at + 1, low)]
            })
            .collect()
    };
    (bytes(initial), bytes(expected))
}

#[cfg(test)]
mod tests {
    use super::Outcome;
    use super::Runner;
    use crate::decode::Case;
    use crate::decode::Snapshot;
    use cpu32_oracle::state::State;

    /// Where the cases' instructions start.
    const CODE: u32 = 0x1000;
    /// The 68000's PC at the start: two words on.
    const PREFETCH_PC: u32 = CODE + 4;

    /// A case of `words` at `CODE` and `data`, from `initial` to
    /// `expected`; PCs are the tests' prefetch addresses.
    fn case(
        name: &str,
        words: &[u16],
        data: &[(u32, u16)],
        initial: State,
        expected: State,
        written: &[(u32, u16)],
    ) -> Case {
        let code: Vec<(u32, u16)> = (CODE..).step_by(2).zip(words.iter().copied()).collect();
        let ram = [code.as_slice(), data].concat();
        Case {
            name: name.to_owned(),
            initial: Snapshot {
                registers: initial,
                ram: ram.clone(),
            },
            expected: Snapshot {
                registers: expected,
                ram: [ram.as_slice(), written].concat(),
            },
        }
    }

    fn supervisor() -> State {
        State {
            sr: 0x2700,
            pc: PREFETCH_PC,
            ssp: 0x4000,
            ..State::default()
        }
    }

    #[test]
    fn nop_passes() {
        let initial = supervisor();
        let expected = State {
            pc: PREFETCH_PC + 2,
            ..initial
        };
        let nop = case(
            "000 NOP 4e71",
            &[0x4e71, 0x4e71],
            &[],
            initial,
            expected,
            &[],
        );
        assert_eq!(Runner::new().run("NOP", &nop), Outcome::Passed);
    }

    /// MOVE.W (4, A0, D1.W), D2 with SCALE 11 and bit 8 set in the
    /// extension word, which the 68000 ignored.
    fn indexed_move(result: u32) -> Case {
        let initial = State {
            data: [0, 0x10, 0, 0, 0, 0, 0, 0],
            address: [0x2000, 0, 0, 0, 0, 0, 0],
            ..supervisor()
        };
        let mut expected = State {
            pc: PREFETCH_PC + 4,
            sr: 0x2708,
            ..initial
        };
        expected.data[2] = result;
        case(
            "001 MOVE.w (d8, A0, Xn), D2 3430",
            &[0x3430, 0x1704, 0x4e71],
            &[(0x2014, 0xbeef)],
            initial,
            expected,
            &[],
        )
    }

    #[test]
    fn brief_extension_words_are_read_as_the_68000_read_them() {
        let mut runner = Runner::new();
        assert_eq!(runner.run("MOVE.w", &indexed_move(0xbeef)), Outcome::Passed);
        let Outcome::Failed(differences) = runner.run("MOVE.w", &indexed_move(0xbeee)) else {
            panic!("a wrong result passed");
        };
        assert_eq!(differences, ["D2: expected 0x0000beee, got 0x0000beef"]);
    }

    #[test]
    fn exceptions_and_unimplemented_instructions_are_skipped() {
        let initial = supervisor();
        // ILLEGAL, through vector 4 to $3000.
        let illegal = case(
            "002 ILLEGAL 4afc",
            &[0x4afc, 0x4e71],
            &[(0x10, 0), (0x12, 0x3000)],
            initial,
            State {
                pc: 0x3004,
                ssp: 0x3ffa,
                ..initial
            },
            &[],
        );
        let mut runner = Runner::new();
        assert_eq!(runner.run("ILLEGAL", &illegal), Outcome::Exception);
        // A line A opcode: no instruction the core implements.
        let line_a = case(
            "003 LINEA a000",
            &[0xa000, 0x4e71],
            &[],
            initial,
            State {
                pc: PREFETCH_PC + 2,
                ..initial
            },
            &[],
        );
        assert_eq!(runner.run("LINEA", &line_a), Outcome::Unimplemented);
    }

    #[test]
    fn a_stray_write_fails() {
        // MOVE.W D0, (A0) with A0 outside the case's RAM.
        let initial = State {
            address: [0x8000, 0, 0, 0, 0, 0, 0],
            ..supervisor()
        };
        let stray = case(
            "004 MOVE.w D0, (A0) 3080",
            &[0x3080, 0x4e71],
            &[],
            initial,
            State {
                pc: PREFETCH_PC + 2,
                sr: 0x2704,
                ..initial
            },
            &[],
        );
        assert!(matches!(
            Runner::new().run("MOVE.w", &stray),
            Outcome::Failed(_)
        ));
    }
}
