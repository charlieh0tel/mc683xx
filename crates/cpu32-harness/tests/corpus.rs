//! The smartclock firmware corpus on the core: the Z3805A 3543B's C
//! library on known inputs (`libc`), and every C function of each image
//! the harness can call, on generated arguments (`functions`).  The
//! images are HP's and not in this repository: `SMARTCLOCK_FIRMWARE`
//! names the directory holding `<image>.bin` and
//! `<image>/decompiled/<image>.c` and `.conventions.jsonl`
//! (smartclock's `third_party/firmware`).  Without it the tests are
//! skipped.  `make corpus` runs them.
//!
//! A function that stops at an instruction the core does not implement
//! is counted by the first such instruction, which says which to
//! implement next.  A libc routine that returns a wrong answer fails
//! the test.  Once no function of an image stops at an unimplemented
//! instruction, one that breaks its convention or takes an exception
//! neither smartclock's harness would end or skip it at nor
//! `functions::EXPLAINED` explains fails it too.
//!
//! With the `musashi` or `unicorn` feature, `oracles_compared` runs
//! every function against those oracles too (`oracles`) and reports
//! where they and the core part.

use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::Path;
use std::path::PathBuf;

use cpu32_harness::families::family;
use cpu32_harness::firmware::Firmware;
use cpu32_harness::firmware::Stopped;
use cpu32_harness::functions::Corpus;
use cpu32_harness::functions::IMAGES;
use cpu32_harness::functions::Verdict;
use cpu32_harness::functions::verdict;
use cpu32_harness::libc;
use cpu32_harness::libc::Outcome;
use cpu32_harness::oracles::Comparison;
use cpu32_harness::oracles::Oracle;
use cpu32_harness::oracles::compare;
#[cfg(feature = "musashi")]
use cpu32_oracle::musashi::Musashi;
#[cfg(feature = "unicorn")]
use cpu32_oracle::unicorn::Unicorn;
use mc683xx_cpu32::exception::Exception;
use mc683xx_cpu32::exception::Vector;

/// The variable naming the firmware directory.
const FIRMWARE: &str = "SMARTCLOCK_FIRMWARE";
/// The variable naming the functions to run, comma separated.
const FILTER: &str = "CORPUS_FILTER";
/// Instructions shown in the report's breakdown.
const FAMILIES_SHOWN: usize = 25;
/// Opcodes shown per instruction.
const OPCODES_SHOWN: usize = 4;
/// Functions shown per other kind of stop.
const FUNCTIONS_SHOWN: usize = 10;

/// The firmware directory, or `None` when the tests are skipped.
fn firmware_directory() -> Option<PathBuf> {
    let directory = env::var_os(FIRMWARE).map(PathBuf::from);
    if directory.is_none() {
        eprintln!("skipped: {FIRMWARE} is unset; `make corpus` runs the corpus");
    }
    directory
}

fn image(directory: &Path, name: &str) -> Option<Vec<u8>> {
    fs::read(directory.join(format!("{name}.bin"))).ok()
}

/// The functions `CORPUS_FILTER` names, if it is set.
fn filter() -> Option<Vec<String>> {
    env::var(FILTER)
        .ok()
        .map(|names| names.split(',').map(str::to_owned).collect())
}

/// Whether `filter` runs `function`.
fn chosen(filter: Option<&[String]>, function: &str) -> bool {
    filter.is_none_or(|names| names.iter().any(|name| name == function))
}

/// Image `name`'s functions and the image on the core, if the
/// directory has the image.
fn load(directory: &Path, name: &str) -> Option<(Corpus, Firmware)> {
    let Some(image) = image(directory, name) else {
        println!("{name}: no image, skipped");
        return None;
    };
    let decompiled = directory.join(name).join("decompiled");
    let read = |suffix: &str| {
        let path = decompiled.join(format!("{name}{suffix}"));
        fs::read_to_string(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
    };
    let corpus = Corpus::new(&read(".c"), &read(".conventions.jsonl"))
        .unwrap_or_else(|error| panic!("{name}: {error}"));
    Some((corpus, Firmware::new(&image).expect("an image")))
}

/// The first unimplemented opcode of a call, as the report counts it.
fn stopped_at(stopped: &Stopped) -> Option<u16> {
    match stopped {
        Stopped::Unimplemented { opcode, .. } => Some(*opcode),
        _ => None,
    }
}

/// One instruction's line of the report.
struct Row {
    /// The instruction.
    name: &'static str,
    /// The stops at it.
    count: usize,
    /// Its opcodes, each with its stops, most first.
    opcodes: Vec<(u16, usize)>,
}

/// Opcodes by instruction, most first.
fn breakdown(opcodes: &BTreeMap<u16, usize>) -> Vec<Row> {
    let mut families: BTreeMap<&'static str, Vec<(u16, usize)>> = BTreeMap::new();
    for (&opcode, &count) in opcodes {
        families
            .entry(family(opcode))
            .or_default()
            .push((opcode, count));
    }
    let mut rows: Vec<Row> = families
        .into_iter()
        .map(|(name, mut opcodes)| {
            opcodes.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
            Row {
                name,
                count: opcodes.iter().map(|(_, count)| count).sum(),
                opcodes,
            }
        })
        .collect();
    rows.sort_by(|a, b| b.count.cmp(&a.count).then(a.name.cmp(b.name)));
    rows
}

fn print_breakdown(opcodes: &BTreeMap<u16, usize>) {
    for Row {
        name,
        count,
        opcodes,
    } in breakdown(opcodes).iter().take(FAMILIES_SHOWN)
    {
        let shown: Vec<String> = opcodes
            .iter()
            .take(OPCODES_SHOWN)
            .map(|(opcode, count)| format!("{opcode:04x} x{count}"))
            .collect();
        println!("  {name:<16} {count:>5}  {}", shown.join(", "));
    }
}

#[test]
fn libc() {
    let Some(directory) = firmware_directory() else {
        return;
    };
    let image = image(&directory, libc::IMAGE)
        .unwrap_or_else(|| panic!("no {}.bin in {}", libc::IMAGE, directory.display()));
    let checks = libc::run(Firmware::new(&image).expect("an image"));
    let mut opcodes: BTreeMap<u16, usize> = BTreeMap::new();
    let mut failed = Vec::new();
    let (mut passed, mut stopped) = (0, 0);
    for check in &checks {
        match &check.outcome {
            Outcome::Passed => passed += 1,
            Outcome::Failed => failed.push(format!("{} {}", check.routine, check.description)),
            Outcome::Stopped(how) => {
                stopped += 1;
                if let Some(opcode) = stopped_at(how) {
                    *opcodes.entry(opcode).or_default() += 1;
                } else {
                    println!("  {} {}: {how:?}", check.routine, check.description);
                }
            }
        }
    }
    println!(
        "libc {}: {} checks, {passed} passed, {} failed, {stopped} stopped",
        libc::IMAGE,
        checks.len(),
        failed.len()
    );
    println!("stopped at the first unimplemented instruction:");
    print_breakdown(&opcodes);
    for failure in &failed {
        println!("FAIL {failure}");
    }
    assert!(failed.is_empty(), "{} libc checks failed", failed.len());
}

/// An image's verdicts, tallied.
#[derive(Debug, Default)]
struct Tally {
    /// Functions that passed.
    passed: usize,
    /// Functions a case of which smartclock's harness would skip.
    skipped: usize,
    /// Functions a case of which made a misaligned access.
    misaligned: usize,
    /// Functions that took an exception `EXPLAINED` explains, with it.
    explained: Vec<(String, Vector, &'static str)>,
    /// Functions stopped at an unimplemented opcode, by the opcode.
    unimplemented: BTreeMap<u16, usize>,
    /// Functions that took an exception nothing explains, by vector,
    /// with their names and where.
    exceptions: BTreeMap<u8, Vec<String>>,
    /// Functions stuck, with how.
    stuck: Vec<(String, Stopped)>,
    /// Functions that broke their convention, with how.
    broke: Vec<(String, Vec<String>)>,
}

#[test]
fn functions() {
    let Some(directory) = firmware_directory() else {
        return;
    };
    let filter = filter();
    let mut images = 0;
    let mut failing = Vec::new();
    for &(name, caller_name) in IMAGES {
        let Some((corpus, mut firmware)) = load(&directory, name) else {
            continue;
        };
        let mut tally = Tally::default();
        let mut run = 0;
        for function in &corpus.functions {
            if !chosen(filter.as_deref(), &function.name) {
                continue;
            }
            run += 1;
            match verdict(&corpus, &mut firmware, caller_name, function) {
                Verdict::Passed => tally.passed += 1,
                Verdict::Skipped(_) => tally.skipped += 1,
                Verdict::Misaligned(_) => tally.misaligned += 1,
                Verdict::Explained(vector, why) => {
                    tally.explained.push((function.name.clone(), vector, why));
                }
                Verdict::Unimplemented(opcode) => {
                    *tally.unimplemented.entry(opcode).or_default() += 1
                }
                Verdict::Exception(exception, pc) => {
                    let named = match exception {
                        Exception::Fault(fault) => {
                            format!("{} at {pc:#x} ({:#x})", function.name, fault.address.0)
                        }
                        _ => format!("{} at {pc:#x}", function.name),
                    };
                    tally
                        .exceptions
                        .entry(exception.vector().0)
                        .or_default()
                        .push(named);
                }
                Verdict::Stuck(how) => tally.stuck.push((function.name.clone(), how)),
                Verdict::Broke(how) => tally.broke.push((function.name.clone(), how)),
            }
        }
        images += 1;
        let unimplemented: usize = tally.unimplemented.values().sum();
        let exceptions: usize = tally.exceptions.values().map(Vec::len).sum();
        println!(
            "{name}: {run} functions run ({} not runnable), {} passed, {} skipped, \
             {} misaligned, {} explained, {unimplemented} unimplemented, {exceptions} exception, {} stuck, \
             {} broke their convention",
            corpus.unrunnable,
            tally.passed,
            tally.skipped,
            tally.misaligned,
            tally.explained.len(),
            tally.stuck.len(),
            tally.broke.len()
        );
        println!("stopped at the first unimplemented instruction:");
        print_breakdown(&tally.unimplemented);
        for (vector, names) in &tally.exceptions {
            let shown: Vec<&str> = names
                .iter()
                .take(FUNCTIONS_SHOWN)
                .map(String::as_str)
                .collect();
            println!("  vector {vector}: {} ({})", names.len(), shown.join(", "));
        }
        for (function, how) in tally.stuck.iter().take(FUNCTIONS_SHOWN) {
            println!("  stuck: {function}: {how:?}");
        }
        for (function, how) in tally.broke.iter().take(FUNCTIONS_SHOWN) {
            println!("  broke: {function}: {}", how.join("; "));
        }
        for (function, vector, why) in &tally.explained {
            println!("  explained: {function}: vector {}: {why}", vector.0);
        }
        if unimplemented == 0 && exceptions + tally.broke.len() > 0 {
            failing.push(format!(
                "{name}: {exceptions} unexplained exceptions, {} broken conventions",
                tally.broke.len()
            ));
        }
    }
    assert!(failing.is_empty(), "{}", failing.join("; "));
    assert!(
        images > 0,
        "no image of {IMAGES:?} in {}",
        directory.display()
    );
}

/// The oracles the features build, in the report's order.
#[cfg_attr(
    any(feature = "musashi", feature = "unicorn"),
    expect(clippy::vec_init_then_push, reason = "the features choose the oracles")
)]
fn oracles() -> Vec<Box<dyn Oracle>> {
    #[cfg_attr(
        not(any(feature = "musashi", feature = "unicorn")),
        expect(unused_mut, reason = "no oracle feature")
    )]
    let mut oracles: Vec<Box<dyn Oracle>> = Vec::new();
    #[cfg(feature = "musashi")]
    oracles.push(Box::new(Musashi::lock()));
    #[cfg(feature = "unicorn")]
    oracles.push(Box::new(Unicorn::new().expect("Unicorn")));
    oracles
}

/// One oracle's results over an image's functions.
#[derive(Debug, Default)]
struct OracleTally {
    /// Functions that matched.
    matched: usize,
    /// Functions that mismatched, with the first difference.
    mismatched: Vec<(String, String)>,
    /// Functions not comparable, by why.
    not_comparable: BTreeMap<String, usize>,
}

/// A verdict's kind, for the core's convention-only line.
const fn verdict_kind(verdict: &Verdict) -> &'static str {
    match verdict {
        Verdict::Passed => "passed",
        Verdict::Skipped(_) => "skipped",
        Verdict::Misaligned(_) => "misaligned",
        Verdict::Explained(..) => "explained",
        Verdict::Broke(_) => "broke",
        Verdict::Unimplemented(_) => "unimplemented",
        Verdict::Exception(..) => "exception",
        Verdict::Stuck(_) => "stuck",
    }
}

/// Every function of each image against the oracles the features build
/// (`make corpus-musashi`, `corpus-unicorn`, `corpus-oracles`).
/// Reported, not asserted: a mismatch is for triage.  Mismatches every
/// oracle agrees on against the core come first.
#[test]
fn oracles_compared() {
    let Some(directory) = firmware_directory() else {
        return;
    };
    let mut oracles = oracles();
    if oracles.is_empty() {
        eprintln!("skipped: no oracle; `make corpus-musashi` or `corpus-unicorn` runs them");
        return;
    }
    let names: Vec<&'static str> = oracles.iter().map(|oracle| oracle.name()).collect();
    let filter = filter();
    for &(name, caller_name) in IMAGES {
        let Some((corpus, mut firmware)) = load(&directory, name) else {
            continue;
        };
        let mut tallies: Vec<OracleTally> = names.iter().map(|_| OracleTally::default()).collect();
        let mut verdicts: BTreeMap<&'static str, usize> = BTreeMap::new();
        let mut agreed = Vec::new();
        for function in &corpus.functions {
            if !chosen(filter.as_deref(), &function.name) {
                continue;
            }
            let compared = compare(&corpus, &mut firmware, caller_name, function, &mut oracles);
            *verdicts.entry(verdict_kind(&compared.verdict)).or_default() += 1;
            if let Some(how) = compared.agreed_against_core {
                agreed.push((function.name.clone(), how));
            }
            for (tally, comparison) in tallies.iter_mut().zip(compared.comparisons) {
                match comparison {
                    Comparison::Matched => tally.matched += 1,
                    Comparison::Mismatched(how) => {
                        tally.mismatched.push((function.name.clone(), how));
                    }
                    Comparison::NotComparable(why) => {
                        *tally.not_comparable.entry(why).or_default() += 1;
                    }
                }
            }
        }
        let verdicts: Vec<String> = verdicts
            .iter()
            .map(|(kind, count)| format!("{count} {kind}"))
            .collect();
        println!(
            "{name}: {} functions; the core alone, by convention: {}",
            corpus.functions.len(),
            verdicts.join(", ")
        );
        if names.len() > 1 {
            println!(
                "{name}: {} probable core bugs, every oracle agreeing against the core:",
                agreed.len()
            );
            for (function, how) in &agreed {
                println!("  BUG {function}: {how}");
            }
        }
        for (oracle, tally) in names.iter().zip(&tallies) {
            let not_comparable: usize = tally.not_comparable.values().sum();
            println!(
                "{name} against {oracle}: {} matched, {} mismatched, {not_comparable} not comparable",
                tally.matched,
                tally.mismatched.len()
            );
            for (function, how) in &tally.mismatched {
                println!("  MISMATCH {function}: {how}");
            }
            let mut reasons: Vec<(&String, &usize)> = tally.not_comparable.iter().collect();
            reasons.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
            for (why, count) in reasons {
                println!("  not comparable x{count}: {why}");
            }
        }
    }
}
