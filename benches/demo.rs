//! The example demo board's C firmware: its Mandelbrot command, typed
//! at the prompt and run to the next, from a snapshot
//! (docs/performance.md).

use criterion::Criterion;
use criterion::Throughput;
use criterion::criterion_group;
use criterion::criterion_main;
use mc683xx_bench::demo::Demo;
use mc683xx_bench::demo::MANDELBROT;

fn mandelbrot(criterion: &mut Criterion) {
    let mut group = criterion.benchmark_group("demo");
    let mut demo = Demo::new(MANDELBROT);
    eprintln!(
        "mandelbrot: {} instructions, {} ps of board time a run",
        demo.instructions, demo.span
    );
    group.throughput(Throughput::Elements(demo.instructions));
    group.bench_function("mandelbrot", |bencher| bencher.iter(|| demo.iteration()));
    group.finish();
}

criterion_group!(benches, mandelbrot);
criterion_main!(benches);
