//! SingleStepTests `m68000` on the core.  The tests are read from the
//! directory `SINGLESTEP_M68000` names (their `v1` directory, or the
//! repository holding it); `make singlestep` fetches and runs them.
//! Without it the test is skipped.  `SINGLESTEP_FILTER`, a comma-separated
//! list of file names such as `MOVE.w,NOP`, runs only those.

use std::env;
use std::fs;
use std::path::PathBuf;

use cpu32_harness::decode::decode;
use cpu32_harness::single_step::Runner;
use cpu32_harness::single_step::Tally;

/// The variable naming the tests' directory.
const DATA: &str = "SINGLESTEP_M68000";
/// The variable naming the files to run.
const FILTER: &str = "SINGLESTEP_FILTER";
/// The tests' file suffix.
const SUFFIX: &str = ".json.bin";
/// Failed cases shown per file.
const FAILURES_SHOWN: usize = 5;
/// Differences shown per failed case.
const DIFFERENCES_SHOWN: usize = 8;

#[test]
fn m68000() {
    let Some(root) = env::var_os(DATA).map(PathBuf::from) else {
        eprintln!("skipped: {DATA} is unset; `make singlestep` fetches and runs the tests");
        return;
    };
    let directory = if root.join("v1").is_dir() {
        root.join("v1")
    } else {
        root
    };
    let filter: Option<Vec<String>> = env::var(FILTER)
        .ok()
        .map(|names| names.split(',').map(str::to_owned).collect());
    let mut files: Vec<(String, PathBuf)> = fs::read_dir(&directory)
        .unwrap_or_else(|error| panic!("{}: {error}", directory.display()))
        .filter_map(|entry| {
            let path = entry.ok()?.path();
            let name = path.file_name()?.to_str()?.strip_suffix(SUFFIX)?.to_owned();
            Some((name, path))
        })
        .filter(|(name, _)| filter.as_ref().is_none_or(|names| names.contains(name)))
        .collect();
    files.sort();
    assert!(
        !files.is_empty(),
        "no {SUFFIX} files in {}",
        directory.display()
    );

    let mut runner = Runner::new();
    let mut total = Tally::default();
    println!(
        "{:<14} {:>6} {:>6} {:>6} {:>6} {:>6}",
        "file", "passed", "failed", "except", "unimpl", "known"
    );
    for (name, path) in files {
        let bytes = fs::read(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
        let cases = decode(&bytes).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
        let mut tally = Tally::default();
        for case in &cases {
            tally.count(&case.name, runner.run(&name, case));
        }
        println!(
            "{name:<14} {:>6} {:>6} {:>6} {:>6} {:>6}",
            tally.passed,
            tally.failed.len(),
            tally.exception,
            tally.unimplemented,
            tally.excluded
        );
        for (case, differences) in tally.failed.iter().take(FAILURES_SHOWN) {
            println!("  failed: {case}");
            for difference in differences.iter().take(DIFFERENCES_SHOWN) {
                println!("    {difference}");
            }
        }
        total.add(tally);
    }
    println!(
        "{:<14} {:>6} {:>6} {:>6} {:>6} {:>6}",
        "total",
        total.passed,
        total.failed.len(),
        total.exception,
        total.unimplemented,
        total.excluded
    );
    for reason in &total.reasons {
        println!("known difference: {reason}");
    }
    assert!(
        total.failed.is_empty(),
        "{} cases failed",
        total.failed.len()
    );
}
