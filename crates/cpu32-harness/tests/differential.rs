//! The core against Musashi's 68020, over every opcode the core
//! implements.  Needs the `musashi` feature; `make oracle` fetches
//! Musashi and runs it.  `CPU32_FUZZ_SEED` and `CPU32_FUZZ_TRIALS` (per
//! opcode) set the run; the same seed is the same run.
#![cfg(feature = "musashi")]

use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::env;

use cpu32_harness::differential::Differential;
use cpu32_harness::differential::Verdict;
use cpu32_oracle::random::SplitMix64;

/// The variable that sets the seed.
const SEED: &str = "CPU32_FUZZ_SEED";
/// The variable that sets the trials per opcode.
const TRIALS: &str = "CPU32_FUZZ_TRIALS";
/// The seed when none is set.
const DEFAULT_SEED: u64 = 0x6833_1000;
/// Trials per opcode when none is set.
const DEFAULT_TRIALS: usize = 8;
/// Disagreements shown.
const DISAGREEMENTS_SHOWN: usize = 10;
/// Where an opcode's line, bits 15:12, starts.
const LINE_SHIFT: u16 = 12;

/// One line's tallies.
#[derive(Debug, Default)]
struct Tally {
    /// Opcodes with a trial the core completed.
    implemented: usize,
    /// Trials that agreed.
    agreed: usize,
    /// Trials the core does not implement.
    unimplemented: usize,
    /// Trials excluded as known differences.
    excluded: usize,
    /// Trials where the core raised an exception.
    exception: usize,
    /// Trials that disagreed.
    disagreed: usize,
}

fn setting<T: std::str::FromStr>(name: &str, default: T) -> T {
    env::var(name).ok().map_or(default, |value| {
        value
            .parse()
            .unwrap_or_else(|_| panic!("{name}={value}: not a number"))
    })
}

#[test]
fn against_musashi() {
    let seed = setting(SEED, DEFAULT_SEED);
    let trials = setting(TRIALS, DEFAULT_TRIALS);
    let mut random = SplitMix64::new(seed);
    let mut differential = Differential::new();
    let mut lines: BTreeMap<u16, Tally> = BTreeMap::new();
    let mut reasons = BTreeSet::new();
    let mut disagreements = Vec::new();
    for opcode in 0..=u16::MAX {
        let tally = lines.entry(opcode >> LINE_SHIFT).or_default();
        let mut implemented = false;
        for _ in 0..trials {
            let (state, verdict) = differential.trial(opcode, &mut random);
            implemented |= matches!(verdict, Verdict::Agreed | Verdict::Disagreed(_));
            match verdict {
                Verdict::Agreed => tally.agreed += 1,
                Verdict::Unimplemented => tally.unimplemented += 1,
                Verdict::Excluded(reason) => {
                    tally.excluded += 1;
                    reasons.insert(reason);
                }
                Verdict::Exception(_) => tally.exception += 1,
                Verdict::Disagreed(differences) => {
                    tally.disagreed += 1;
                    disagreements.push((opcode, state, differences));
                }
            }
        }
        tally.implemented += usize::from(implemented);
    }
    println!("seed {seed:#x}, {trials} trials per opcode");
    println!(
        "{:<5} {:>8} {:>8} {:>8} {:>8} {:>8} {:>8}",
        "line", "opcodes", "agreed", "unimpl", "known", "except", "differ"
    );
    for (line, tally) in &lines {
        println!(
            "{line:<5X} {:>8} {:>8} {:>8} {:>8} {:>8} {:>8}",
            tally.implemented,
            tally.agreed,
            tally.unimplemented,
            tally.excluded,
            tally.exception,
            tally.disagreed
        );
    }
    for reason in &reasons {
        println!("known difference: {reason}");
    }
    for (opcode, state, differences) in disagreements.iter().take(DISAGREEMENTS_SHOWN) {
        println!("opcode {opcode:#06x} from {state:x?}");
        for difference in differences {
            println!("    {difference}");
        }
    }
    assert!(
        disagreements.is_empty(),
        "{} trials disagreed",
        disagreements.len()
    );
}
