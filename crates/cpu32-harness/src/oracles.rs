//! The firmware function corpus against oracles: each case the core
//! ends where a comparison means something (a return, a trap or spin it
//! stops at, a zero divide, a bus error at a write to ROM or beyond the
//! memory and peripherals mapped) is run again on each oracle
//! from the same registers and memory, and the end compared: how it
//! ended, the registers, SR but the bits the CPU32 and the 68020 differ
//! in or the manuals leave undefined (`differences`), and memory and
//! the peripherals.  A case that runs an instruction `differences` says the
//! 68020 runs otherwise, or that an oracle cannot be trusted with, is
//! not comparable.

use cpu32_oracle::call::Call as OracleCall;
use cpu32_oracle::call::CallEnd;
use cpu32_oracle::call::Ending;
use cpu32_oracle::memory::ADDRESS_MASK;
use cpu32_oracle::memory::Fault;
use cpu32_oracle::memory::RegionMemory;
#[cfg(feature = "musashi")]
use cpu32_oracle::musashi::Musashi;
use cpu32_oracle::state::State;
#[cfg(feature = "unicorn")]
use cpu32_oracle::unicorn::Unicorn;
use mc683xx_cpu32::exception::CycleKind;
use mc683xx_cpu32::exception::Exception;
use mc683xx_cpu32::exception::Vector;

use crate::differences;
use crate::firmware::Call;
use crate::firmware::Executed;
use crate::firmware::Firmware;
use crate::firmware::HANDLED_TRAPS;
use crate::firmware::RETURN;
use crate::firmware::STEP_LIMIT;
use crate::firmware::Stopped;
use crate::firmware::VECTORS;
use crate::functions::CaseObserver;
use crate::functions::Corpus;
use crate::functions::Function;
use crate::functions::Outcome;
use crate::functions::Verdict;
use crate::functions::observed_verdict;
use crate::machine;

/// An oracle a call can run on.
pub trait Oracle {
    /// Its name, for a report.
    fn name(&self) -> &'static str;
    /// Runs `call` on `memory` until it ends.
    fn call(&mut self, call: &OracleCall<'_>, memory: RegionMemory) -> CallEnd;
}

#[cfg(feature = "musashi")]
impl Oracle for Musashi {
    fn name(&self) -> &'static str {
        "Musashi"
    }

    fn call(&mut self, call: &OracleCall<'_>, memory: RegionMemory) -> CallEnd {
        Self::call(self, call, memory)
    }
}

#[cfg(feature = "unicorn")]
impl Oracle for Unicorn {
    fn name(&self) -> &'static str {
        "Unicorn"
    }

    fn call(&mut self, call: &OracleCall<'_>, memory: RegionMemory) -> CallEnd {
        Self::call(self, call, memory)
    }
}

/// How a case, or a function, came out against one oracle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Comparison {
    /// The oracle ended it as the core did.
    Matched,
    /// It did not: the first difference.
    Mismatched(String),
    /// There is nothing to compare: why.
    NotComparable(String),
}

/// A function against the oracles.
#[derive(Debug, Clone)]
pub struct Compared {
    /// Its verdict on the core alone, the calling convention's.
    pub verdict: Verdict,
    /// Against each oracle, in their order: the first case that
    /// mismatched; else matched, if one did; else why none compared.
    pub comparisons: Vec<Comparison>,
    /// The first case every oracle, two or more, ended alike and the
    /// core otherwise: a probable core bug, with the first difference.
    pub agreed_against_core: Option<String>,
}

/// Runs every case of `function` on the core and on each of `oracles`.
pub fn compare(
    corpus: &Corpus,
    firmware: &mut Firmware,
    caller_name: u32,
    function: &Function,
    oracles: &mut [Box<dyn Oracle>],
) -> Compared {
    let mut watcher = Watcher {
        oracles,
        start: None,
        notes: Notes::default(),
        cases: Vec::new(),
    };
    let verdict = observed_verdict(corpus, firmware, caller_name, function, Some(&mut watcher));
    let comparisons = (0..watcher.oracles.len())
        .map(|index| {
            let per_case = || watcher.cases.iter().map(|case| &case.comparisons[index]);
            per_case()
                .find(|comparison| matches!(comparison, Comparison::Mismatched(_)))
                .or_else(|| per_case().find(|comparison| **comparison == Comparison::Matched))
                .or_else(|| per_case().next())
                .cloned()
                .unwrap_or_else(|| Comparison::NotComparable("no case run".to_owned()))
        })
        .collect();
    let agreed_against_core = watcher
        .cases
        .iter()
        .find_map(|case| case.agreed_against_core.clone());
    Compared {
        verdict,
        comparisons,
        agreed_against_core,
    }
}

/// What a case's instructions mean for a comparison.
#[derive(Debug, Default)]
struct Notes {
    /// Why the 68020 runs one of them otherwise, if it does.
    differs: Option<&'static str>,
    /// The condition codes one of them leaves undefined.
    undefined: u16,
}

/// One case against each oracle.
#[derive(Debug)]
struct Case {
    /// Against each oracle.
    comparisons: Vec<Comparison>,
    /// Every oracle's first difference from the core, when they all
    /// ended alike and the core otherwise.
    agreed_against_core: Option<String>,
}

/// Watches a function's cases, running each on the oracles.
struct Watcher<'a> {
    /// The oracles.
    oracles: &'a mut [Box<dyn Oracle>],
    /// Memory as the case started.
    start: Option<RegionMemory>,
    /// The case's instructions, so far.
    notes: Notes,
    /// The cases, so far.
    cases: Vec<Case>,
}

impl CaseObserver for Watcher<'_> {
    fn before(&mut self, firmware: &Firmware) {
        self.start = Some(firmware.snapshot());
        self.notes = Notes::default();
    }

    fn executed(&mut self, executed: Executed) {
        let Executed {
            opcode,
            extension,
            sr,
            ..
        } = executed;
        self.notes.differs = self.notes.differs.or_else(|| {
            differences::differential(opcode, extension, None)
                .or_else(|| differences::differential_divide(opcode, sr))
        });
        self.notes.undefined |= differences::undefined_flags(opcode, sr);
    }

    fn after(&mut self, firmware: &Firmware, call: &Call, outcome: &Outcome) {
        let start = self.start.take().expect("before ran");
        let not_comparable = |why: String| Case {
            comparisons: vec![Comparison::NotComparable(why); self.oracles.len()],
            agreed_against_core: None,
        };
        let case = match (core_ending(call, outcome), self.notes.differs) {
            (Err(why), _) => not_comparable(why),
            (Ok(_), Some(why)) => not_comparable(why.to_owned()),
            (Ok(ending), None) => {
                let core = End {
                    ending,
                    state: machine::capture(&call.registers),
                    memory: firmware.snapshot(),
                };
                self.against(&core, &call.started, &start)
            }
        };
        self.cases.push(case);
    }
}

impl Watcher<'_> {
    /// The core's end of a case, `core`, against each oracle's, from
    /// `started` on `start`.
    fn against(&mut self, core: &End, started: &State, start: &RegionMemory) -> Case {
        let call = OracleCall {
            state: *started,
            vbr: VECTORS,
            until: RETURN,
            handled_traps: &HANDLED_TRAPS,
            step_limit: STEP_LIMIT,
        };
        let undefined = self.notes.undefined;
        let ends: Vec<CallEnd> = self
            .oracles
            .iter_mut()
            .map(|oracle| oracle.call(&call, start.clone()))
            .collect();
        let comparisons: Vec<Comparison> = self
            .oracles
            .iter()
            .zip(&ends)
            .map(|(oracle, end)| compare_ends(oracle.name(), core, end, undefined))
            .collect();
        let all_mismatched = comparisons
            .iter()
            .all(|comparison| matches!(comparison, Comparison::Mismatched(_)));
        let alike = ends
            .windows(2)
            .all(|pair| same_end(&pair[0], &pair[1], undefined));
        let agreed_against_core =
            (ends.len() > 1 && all_mismatched && alike).then(|| match &comparisons[0] {
                Comparison::Mismatched(how) => how.clone(),
                _ => unreachable!("all mismatched"),
            });
        Case {
            comparisons,
            agreed_against_core,
        }
    }
}

/// A case's end on the core.
#[derive(Debug)]
struct End {
    /// How it ended.
    ending: Ending,
    /// The registers then.
    state: State,
    /// Memory then.
    memory: RegionMemory,
}

/// The core's end of a case as an oracle would report it, if the end
/// is one an oracle's means something against: why not, if not.
fn core_ending(call: &Call, outcome: &Outcome) -> Result<Ending, String> {
    match (call.stopped, outcome) {
        (Stopped::Returned, _) => Ok(Ending::Returned),
        (Stopped::Trap(number), _) => Ok(Ending::Trap(number)),
        (Stopped::Spin, _) => Ok(Ending::Spin),
        // A bus error, at a write to ROM or beyond the map, which an
        // oracle reports as a fault; the CPU32's frame is not compared.
        (
            Stopped::Exception {
                exception: Exception::Fault(fault),
                ..
            },
            Outcome::Ended(_) | Outcome::Skipped(_),
        ) if fault.vector == Vector::BUS_ERROR => Ok(Ending::Fault(Fault {
            address: fault.address.0 & ADDRESS_MASK,
            write: fault.cycle == CycleKind::Write,
        })),
        (Stopped::Exception { pc, .. }, Outcome::Ended(_)) => Ok(Ending::Exception {
            vector: Vector::ZERO_DIVIDE.0,
            pc,
        }),
        (_, outcome) => Err(format!("the core did not end it: {}", kind(outcome))),
    }
}

/// An outcome's kind, for a report.
const fn kind(outcome: &Outcome) -> &'static str {
    match outcome {
        Outcome::Returned | Outcome::Ended(_) | Outcome::Broke(_) => "ended",
        Outcome::Unimplemented(_) => "unimplemented",
        Outcome::Skipped(_) => "skipped",
        Outcome::Misaligned(_) => "misaligned",
        Outcome::Exception(..) => "exception",
        Outcome::Stuck(_) => "stuck",
    }
}

/// The SR bits compared: not the 68020's M bit, nor the condition codes
/// some instruction of the case left undefined.
const fn compared_sr(undefined: u16) -> u16 {
    !(differences::MASTER | undefined)
}

/// `state` with only its compared SR bits.
const fn masked(state: &State, undefined: u16) -> State {
    State {
        sr: state.sr & compared_sr(undefined),
        ..*state
    }
}

/// Whether an end's registers and memory are compared: a call that
/// took an exception or faulted has the core's CPU32 frame, or none.
const fn compares_state(ending: Ending) -> bool {
    matches!(ending, Ending::Returned | Ending::Trap(_) | Ending::Spin)
}

/// The core's end, `core`, against oracle `name`'s.
fn compare_ends(name: &str, core: &End, oracle: &CallEnd, undefined: u16) -> Comparison {
    if let Ending::Unsupported(why) = oracle.ending {
        return Comparison::NotComparable(why.to_owned());
    }
    if oracle.ending != core.ending {
        return Comparison::Mismatched(format!(
            "ended: {name} {:?} at {:#x}, the core {:?} at {:#x}",
            oracle.ending, oracle.state.pc, core.ending, core.state.pc
        ));
    }
    if !compares_state(core.ending) {
        return Comparison::Matched;
    }
    let registers = masked(&oracle.state, undefined).differences(&masked(&core.state, undefined));
    if let Some(first) = registers.first() {
        return Comparison::Mismatched(format!("{first} ({name}'s expected, the core's got)"));
    }
    match oracle.memory.first_difference(&core.memory) {
        Some((address, expected, actual)) => Comparison::Mismatched(format!(
            "memory {address:#08x}: {name} {expected:#04x}, the core {actual:#04x}"
        )),
        None => Comparison::Matched,
    }
}

/// Whether two oracles ended a case alike.
fn same_end(first: &CallEnd, second: &CallEnd, undefined: u16) -> bool {
    first.ending == second.ending
        && (!compares_state(first.ending)
            || masked(&first.state, undefined) == masked(&second.state, undefined)
                && first.memory.first_difference(&second.memory).is_none())
}

#[cfg(test)]
mod tests {
    use cpu32_oracle::call::Call as OracleCall;
    use cpu32_oracle::call::CallEnd;
    use cpu32_oracle::call::Ending;
    use cpu32_oracle::memory::RegionMemory;

    use super::Comparison;
    use super::Oracle;
    use super::compare;
    use crate::firmware::Firmware;
    use crate::firmware::ROM_BYTES;
    use crate::functions::Corpus;

    /// An oracle that returns at once with D0 `d0`, popping the return
    /// address, memory untouched.
    struct Fixed {
        d0: u32,
    }

    impl Oracle for Fixed {
        fn name(&self) -> &'static str {
            "fixed"
        }

        fn call(&mut self, call: &OracleCall<'_>, memory: RegionMemory) -> CallEnd {
            let mut state = call.state;
            state.data[0] = self.d0;
            state.ssp += 4;
            state.pc = call.until;
            CallEnd {
                ending: Ending::Returned,
                state,
                steps: 1,
                memory,
            }
        }
    }

    /// `f(int)` at $400, which returns 5 in D0: MOVEQ #5, D0; RTS.
    fn returns_five() -> (Corpus, Firmware) {
        let c = "int f(int param_1);\n";
        let sidecar = r#"{"address":"00000400","name":"f","convention":"unknown","stack":[{"offset":4,"size":4}]}"#;
        let mut image = vec![0; ROM_BYTES as usize];
        image[0x400..0x404].copy_from_slice(&[0x70, 0x05, 0x4e, 0x75]);
        (
            Corpus::new(c, sidecar).unwrap(),
            Firmware::new(&image).unwrap(),
        )
    }

    #[test]
    fn oracles_that_agree_with_the_core_match() {
        let (corpus, mut firmware) = returns_five();
        let mut oracles: Vec<Box<dyn Oracle>> = vec![Box::new(Fixed { d0: 5 })];
        let compared = compare(
            &corpus,
            &mut firmware,
            0,
            &corpus.functions[0],
            &mut oracles,
        );
        assert_eq!(compared.comparisons, [Comparison::Matched]);
        assert_eq!(compared.agreed_against_core, None);
    }

    #[test]
    fn oracles_that_agree_against_the_core_are_a_probable_bug() {
        let (corpus, mut firmware) = returns_five();
        let mut oracles: Vec<Box<dyn Oracle>> =
            vec![Box::new(Fixed { d0: 6 }), Box::new(Fixed { d0: 6 })];
        let compared = compare(
            &corpus,
            &mut firmware,
            0,
            &corpus.functions[0],
            &mut oracles,
        );
        let how = "D0: expected 0x00000006, got 0x00000005 (fixed's expected, the core's got)";
        assert_eq!(
            compared.comparisons,
            [
                Comparison::Mismatched(how.to_owned()),
                Comparison::Mismatched(how.to_owned())
            ]
        );
        assert_eq!(compared.agreed_against_core.as_deref(), Some(how));
        let mut oracles: Vec<Box<dyn Oracle>> =
            vec![Box::new(Fixed { d0: 6 }), Box::new(Fixed { d0: 7 })];
        let compared = compare(
            &corpus,
            &mut firmware,
            0,
            &corpus.functions[0],
            &mut oracles,
        );
        assert_eq!(compared.agreed_against_core, None);
    }
}
