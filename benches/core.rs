//! The core alone: each kernel's sled from zero-wait RAM, and many
//! short runs (docs/performance.md).

use std::hint::black_box;

use criterion::Criterion;
use criterion::Throughput;
use criterion::criterion_group;
use criterion::criterion_main;
use mc683xx_bench::bare;
use mc683xx_bench::kernel;
use mc683xx_cpu32::cycles::Cycles;

/// Runs of one instruction each per iteration of `short_runs`.
const SHORT_RUNS: u64 = 1000;

fn kernels(criterion: &mut Criterion) {
    let mut group = criterion.benchmark_group("core");
    for kernel in kernel::CORE {
        let Ok(calibration) = bare::calibrate(kernel, bare::BUDGET) else {
            eprintln!("{}: skipped, not implemented yet", kernel.name);
            continue;
        };
        eprintln!("{}", calibration.describe(kernel.name));
        group.throughput(Throughput::Elements(calibration.instructions));
        let mut cpu = bare::machine(kernel);
        group.bench_function(kernel.name, |bencher| {
            bencher.iter(|| bare::iteration(&mut cpu, black_box(bare::BUDGET)));
        });
    }
    group.finish();
}

/// The run loop's cost per call: a budget of one clock runs one
/// instruction.
fn short_runs(criterion: &mut Criterion) {
    let mut group = criterion.benchmark_group("core");
    group.throughput(Throughput::Elements(SHORT_RUNS));
    let mut cpu = bare::machine(&kernel::CORE[0]);
    group.bench_function("short_runs", |bencher| {
        bencher.iter(|| {
            bare::prepare(&mut cpu);
            for _ in 0..SHORT_RUNS {
                cpu.run_for(
                    &mut bare::NoBus,
                    &mut bare::NoInterrupts,
                    Cycles::ZERO,
                    black_box(Cycles(1)),
                );
            }
        });
    });
    group.finish();
}

criterion_group!(benches, kernels, short_runs);
criterion_main!(benches);
