//! The MC68331 on the Z3805A's map: sleds from flash with data from
//! RAM or a peripheral, with the periodic timer off, syncing the run, and
//! interrupting it, and with a QSPI queue in wraparound syncing it; and
//! many short runs (docs/performance.md).

use std::hint::black_box;

use criterion::Criterion;
use criterion::Throughput;
use criterion::criterion_group;
use criterion::criterion_main;
use mc683xx_bench::board::BUDGET;
use mc683xx_bench::board::Board;
use mc683xx_bench::board::KERNELS;
use mc683xx_bench::board::Load;
use mc683xx_bench::kernel::Kernel;
use mc683xx_cpu32::cycles::Cycles;

/// Runs of one instruction each per iteration of `short_runs`.
const SHORT_RUNS: u64 = 1000;

/// A benchmark of `kernel` with `load`, named `name`.
fn bench(criterion: &mut Criterion, name: &str, kernel: &Kernel, load: Load) {
    let mut group = criterion.benchmark_group("chip");
    let Ok(calibration) = Board::calibrate(kernel, load, BUDGET) else {
        eprintln!("{name}: skipped, not implemented yet");
        return;
    };
    let mut board = Board::new(kernel, load);
    let before = board.quanta();
    board.iteration(BUDGET);
    eprintln!(
        "{}; {} quanta",
        calibration.describe(name),
        board.quanta() - before
    );
    group.throughput(Throughput::Elements(calibration.instructions));
    group.bench_function(name, |bencher| {
        bencher.iter(|| board.iteration(black_box(BUDGET)));
    });
    group.finish();
}

fn kernels(criterion: &mut Criterion) {
    for kernel in KERNELS {
        bench(criterion, kernel.name, kernel, Load::Idle);
    }
    let nop = &KERNELS[0];
    bench(criterion, "flash_nop_pit_masked", nop, Load::PitMasked);
    bench(
        criterion,
        "flash_nop_pit_interrupting",
        nop,
        Load::PitInterrupting,
    );
    bench(criterion, "flash_nop_qspi_words", nop, Load::QspiWords);
    bench(criterion, "flash_nop_qspi_bytes", nop, Load::QspiBytes);
    bench(
        criterion,
        "flash_nop_qspi_bytes_deferred",
        nop,
        Load::QspiBytesDeferred,
    );
}

/// The chip's run loop and syncing per call: a budget of one clock
/// runs one instruction.
fn short_runs(criterion: &mut Criterion) {
    let mut group = criterion.benchmark_group("chip");
    group.throughput(Throughput::Elements(SHORT_RUNS));
    let mut board = Board::new(&KERNELS[0], Load::Idle);
    group.bench_function("short_runs", |bencher| {
        bencher.iter(|| {
            board.prepare();
            for _ in 0..SHORT_RUNS {
                board.chip.run_for(black_box(Cycles(1)));
            }
        });
    });
    group.finish();
}

criterion_group!(benches, kernels, short_runs);
criterion_main!(benches);
