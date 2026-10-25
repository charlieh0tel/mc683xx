//! Building the instruction and timing tables (docs/performance.md).
//! The core builds each once per process, on first use, so each sample
//! is a fresh process of this benchmark: the child times the first run
//! that builds a table and prints its nanoseconds.

use std::env;
use std::hint::black_box;
use std::process::Command;
use std::time::Duration;
use std::time::Instant;

use criterion::Criterion;
use criterion::criterion_group;
use mc683xx_bench::bare;
use mc683xx_bench::kernel;
use mc683xx_cpu32::cpu::Cpu;
use mc683xx_cpu32::cycles::Cycles;
use mc683xx_cpu32::memory::Memory;

/// The environment variable that makes this process a child, naming
/// the table it times.
const CHILD: &str = "MC683XX_BENCH_TABLE";
/// The instruction table.
const INSTRUCTIONS: &str = "instructions";
/// The timing table.
const TIMING: &str = "timing";
/// The 68331's address lines (MC68331UM 4.4.1.1).
const ADDRESS_LINES: u32 = 24;
/// Samples: each is a process.
const SAMPLES: usize = 20;

/// A first run on an empty bus: the opcode fetch faults, so the run
/// builds the instruction table but not the timing table.
fn first_fetch() {
    let memory = Memory::new(ADDRESS_LINES).expect("24 address lines");
    let mut cpu = Cpu::new(memory);
    cpu.run_for(
        &mut bare::NoBus,
        &mut bare::NoInterrupts,
        Cycles::ZERO,
        Cycles(1),
    );
}

/// A first instruction: it builds the timing table.
fn first_instruction() {
    let mut cpu = bare::machine(&kernel::CORE[0]);
    cpu.run_for(
        &mut bare::NoBus,
        &mut bare::NoInterrupts,
        Cycles::ZERO,
        Cycles(1),
    );
}

/// In a child, times `table`'s build.
fn child(table: &str) -> Duration {
    match table {
        INSTRUCTIONS => {
            let start = Instant::now();
            first_fetch();
            start.elapsed()
        }
        TIMING => {
            first_fetch();
            let start = Instant::now();
            first_instruction();
            start.elapsed()
        }
        _ => panic!("{CHILD}={table}: no such table"),
    }
}

/// The build time of `table` in a fresh process.
fn sample(table: &str) -> Duration {
    let output = Command::new(env::current_exe().expect("this benchmark's path"))
        .env(CHILD, table)
        .output()
        .expect("a child");
    let nanoseconds: u64 = String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse()
        .expect("the child's nanoseconds");
    Duration::from_nanos(nanoseconds)
}

fn tables(criterion: &mut Criterion) {
    let mut group = criterion.benchmark_group("tables");
    group.sample_size(SAMPLES);
    for table in [INSTRUCTIONS, TIMING] {
        group.bench_function(table, |bencher| {
            bencher
                .iter_custom(|iterations| (0..iterations).map(|_| sample(black_box(table))).sum());
        });
    }
    group.finish();
}

criterion_group!(benches, tables);

fn main() {
    if let Ok(table) = env::var(CHILD) {
        println!("{}", child(&table).as_nanos());
        return;
    }
    benches();
    Criterion::default().configure_from_args().final_summary();
}
