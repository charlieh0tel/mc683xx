//! A fixed count of a kernel's iterations, for `perf stat`, whose
//! instruction counts are exact where criterion's times move with the
//! machine's load (docs/performance.md, Reproducing):
//!
//! ```text
//! cargo build --profile bench -p mc683xx-bench --example iterations
//! taskset -c 2 perf stat -e instructions:u,cycles:u \
//!     target/release/examples/iterations core nop 5000
//! taskset -c 2 perf stat -e instructions:u,cycles:u \
//!     target/release/examples/iterations chip ram_move idle 5000
//! taskset -c 2 perf stat -e instructions:u,cycles:u \
//!     target/release/examples/iterations demo mandelbrot 20
//! taskset -c 2 perf stat -e instructions:u,cycles:u \
//!     target/release/examples/iterations short chip 5000
//! ```
//!
//! `core NAME COUNT` runs a `benches/core.rs` kernel's iteration,
//! `chip NAME LOAD COUNT` a `benches/chip.rs` one's under a load:
//! `idle`, `pit_masked`, `pit_interrupting`, `qspi_words`,
//! `qspi_bytes` or `qspi_bytes_deferred`; `demo mandelbrot COUNT`
//! `benches/demo.rs`'s, the demo board's firmware; `short core|chip
//! COUNT` the `short_runs` benchmarks' iteration, 1000 runs of one
//! clock, the cost of a run call.  It prints the
//! clocks run (the bytes printed, for the demo), so the work cannot be
//! optimized away.

use std::hint::black_box;
use std::process::ExitCode;

use mc683xx_bench::bare;
use mc683xx_bench::board;
use mc683xx_bench::board::Board;
use mc683xx_bench::board::Load;
use mc683xx_bench::demo::Demo;
use mc683xx_bench::demo::MANDELBROT;
use mc683xx_bench::kernel;
use mc683xx_bench::kernel::Kernel;
use mc683xx_cpu32::Cycle;
use mc683xx_cpu32::Cycles;

/// How to call it.
const USAGE: &str = "usage: iterations core NAME COUNT | chip NAME LOAD COUNT | \
                     demo mandelbrot COUNT | short core|chip COUNT";

/// Runs of one clock each in a `short` iteration, as `short_runs`'.
const SHORT_RUNS: u64 = 1000;

/// The load named `name`.
fn load(name: &str) -> Option<Load> {
    Some(match name {
        "idle" => Load::Idle,
        "pit_masked" => Load::PitMasked,
        "pit_interrupting" => Load::PitInterrupting,
        "qspi_words" => Load::QspiWords,
        "qspi_bytes" => Load::QspiBytes,
        "qspi_bytes_deferred" => Load::QspiBytesDeferred,
        _ => return None,
    })
}

/// The kernel of `kernels` named `name`.
fn kernel<'a>(kernels: &'a [Kernel], name: &str) -> Option<&'a Kernel> {
    kernels.iter().find(|kernel| kernel.name == name)
}

/// The clocks `count` iterations ran, from the arguments after the
/// program's name.
fn run(arguments: &[String]) -> Option<u64> {
    let (count, rest) = arguments.split_last()?;
    let count: u64 = count.parse().ok()?;
    let clocks = match rest {
        [kind, name] if kind == "core" => {
            let mut cpu = bare::machine(kernel(kernel::CORE, name)?);
            (0..count)
                .map(|_| bare::iteration(&mut cpu, black_box(bare::BUDGET)).clocks())
                .sum()
        }
        [kind, name, load_name] if kind == "chip" => {
            let mut chip = Board::new(kernel(board::KERNELS, name)?, load(load_name)?);
            (0..count)
                .map(|_| chip.iteration(black_box(board::BUDGET)).clocks())
                .sum()
        }
        [kind, layer] if kind == "short" && layer == "core" => {
            let mut cpu = bare::machine(&kernel::CORE[0]);
            let clock = Cycles::from_clocks(1);
            (0..count)
                .map(|_| {
                    bare::prepare(&mut cpu);
                    (0..SHORT_RUNS)
                        .map(|_| {
                            cpu.run_for(
                                &mut bare::NoBus,
                                &mut bare::NoInterrupts,
                                Cycle::ZERO,
                                black_box(clock),
                            )
                            .cycles
                            .clocks()
                        })
                        .sum::<u64>()
                })
                .sum()
        }
        [kind, layer] if kind == "short" && layer == "chip" => {
            let mut chip = Board::new(&board::KERNELS[0], Load::Idle);
            let clock = Cycles::from_clocks(1);
            (0..count)
                .map(|_| {
                    chip.prepare();
                    (0..SHORT_RUNS)
                        .map(|_| chip.chip.run_for(black_box(clock)).cycles.clocks())
                        .sum::<u64>()
                })
                .sum()
        }
        [kind, name] if kind == "demo" && name == "mandelbrot" => {
            let mut demo = Demo::new(MANDELBROT);
            (0..count).map(|_| demo.iteration().len() as u64).sum()
        }
        _ => return None,
    };
    Some(clocks)
}

fn main() -> ExitCode {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    match run(&arguments) {
        Some(clocks) => {
            println!("{clocks} clocks");
            ExitCode::SUCCESS
        }
        None => {
            eprintln!("{USAGE}");
            ExitCode::FAILURE
        }
    }
}
