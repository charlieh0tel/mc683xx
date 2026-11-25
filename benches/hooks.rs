//! What the hooks cost the core (docs/performance.md, Hooks): kernels
//! with a hook that never fires, one called on every instruction, an
//! opcode hook elsewhere, over the sled matching nothing and matching
//! everything, a coverage elsewhere and over the sled, and installing
//! and removing a hook.

use std::hint::black_box;
use std::ops::ControlFlow;
use std::ops::RangeInclusive;

use criterion::Criterion;
use criterion::Throughput;
use criterion::criterion_group;
use criterion::criterion_main;
use mc683xx_bench::bare;
use mc683xx_bench::kernel;
use mc683xx_cpu32::Cpu;
use mc683xx_target::Address;

/// A range on the RAM's page tables that no kernel's sled reaches.
const ELSEWHERE: RangeInclusive<Address> = Address(0x3_8000)..=Address(0x3_8001);
/// The sleds' addresses.
const SLED: RangeInclusive<Address> = Address(bare::CODE)..=Address(0x1_ffff);
/// Every address.
const EVERYWHERE: RangeInclusive<Address> = Address(0)..=Address(u32::MAX);

/// The kernels measured: one that touches no memory but its fetches,
/// one that reads and one that writes.
const KERNELS: [&str; 3] = ["nop", "move_indirect", "clr_indirect"];

/// The hooks a case installs.
type Install = fn(&mut Cpu);

/// An opcode no kernel runs: the F-line's last.
const UNRUN: u16 = 0xffff;

/// The cases: a name and the hooks it installs.
const CASES: [(&str, Install); 9] = [
    ("unhooked", |_| {}),
    ("code_elsewhere", |cpu| {
        cpu.add_code_hook(ELSEWHERE, |_| ControlFlow::Continue(()));
    }),
    ("write_elsewhere", |cpu| {
        cpu.add_write_hook(ELSEWHERE, |_| ControlFlow::Continue(()));
    }),
    ("code_everywhere", |cpu| {
        cpu.add_code_hook(EVERYWHERE, |_| ControlFlow::Continue(()));
    }),
    ("opcode_elsewhere", |cpu| {
        cpu.add_opcode_hook(ELSEWHERE, u16::MAX, UNRUN, |_| ControlFlow::Continue(()));
    }),
    ("opcode_unmatched", |cpu| {
        cpu.add_opcode_hook(SLED, u16::MAX, UNRUN, |_| ControlFlow::Continue(()));
    }),
    ("opcode_everywhere", |cpu| {
        cpu.add_opcode_hook(EVERYWHERE, 0, 0, |_| ControlFlow::Continue(()));
    }),
    ("coverage_elsewhere", |cpu| {
        cpu.add_coverage(ELSEWHERE);
    }),
    ("coverage_sled", |cpu| {
        cpu.add_coverage(SLED);
    }),
];

fn hooked(criterion: &mut Criterion) {
    let mut group = criterion.benchmark_group("hooks");
    for name in KERNELS {
        let kernel = kernel::CORE
            .iter()
            .find(|kernel| kernel.name == name)
            .expect("a kernel");
        let calibration = bare::calibrate(kernel, bare::BUDGET).expect("implemented");
        group.throughput(Throughput::Elements(calibration.instructions));
        for (case, install) in CASES {
            let mut cpu = bare::machine(kernel);
            install(&mut cpu);
            group.bench_function(format!("{name}_{case}"), |bencher| {
                bencher.iter(|| bare::iteration(&mut cpu, black_box(bare::BUDGET)));
            });
        }
    }
    group.finish();
}

/// Adding and removing a code hook, on the RAM's 256 Kbytes.
fn install(criterion: &mut Criterion) {
    let mut group = criterion.benchmark_group("hooks");
    let mut cpu = bare::machine(&kernel::CORE[0]);
    group.bench_function("add_remove", |bencher| {
        bencher.iter(|| {
            let id = cpu.add_code_hook(black_box(ELSEWHERE), |_| ControlFlow::Continue(()));
            cpu.remove_hook(id)
        });
    });
    group.finish();
}

criterion_group!(benches, hooked, install);
criterion_main!(benches);
