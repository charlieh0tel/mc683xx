//! A debugger's access with no bus cycle: a bare machine's `read` and
//! `write` of 512 Kbytes, through a peripheral that peeks and pokes
//! them and into RAM, as a host loads an image into a peripheral's
//! array (docs/performance.md).

use std::hint::black_box;

use criterion::Criterion;
use criterion::Throughput;
use criterion::criterion_group;
use criterion::criterion_main;
use mc683xx_cpu32::BusError;
use mc683xx_cpu32::flat::ADDRESS_BITS;
use mc683xx_cpu32::flat::Machine;
use mc683xx_cpu32::peripheral::BusCycle;
use mc683xx_cpu32::peripheral::MappedPeripheral;
use mc683xx_cpu32::state::Stateless;
use mc683xx_target::Address;

/// The bytes each access moves: an image of a 4-Mbit part.
const BYTES: u32 = 512 << 10;
/// Where the RAM is.
const RAM: Address = Address(0);
/// Where the peripheral is, past the RAM.
const PERIPHERAL: Address = Address(0x0100_0000);

/// A peripheral that holds bytes, as a flash's array: the debugger's
/// access reads and writes them.
struct Array(Vec<u8>);

impl Array {
    /// The offset in it of `address`.
    const fn offset(address: Address) -> usize {
        (address.0 - PERIPHERAL.0) as usize
    }
}

impl MappedPeripheral for Array {
    fn read(&mut self, _: BusCycle) -> Result<u32, BusError> {
        Err(BusError)
    }

    fn write(&mut self, _: BusCycle, _: u32) -> Result<(), BusError> {
        Err(BusError)
    }

    fn peek(&self, cycle: BusCycle) -> Option<u32> {
        Some(u32::from(self.0[Self::offset(cycle.address)]))
    }

    fn poke(&mut self, cycle: BusCycle, value: u32) -> bool {
        self.0[Self::offset(cycle.address)] = value as u8;
        true
    }
}

impl Stateless for Array {}

/// A machine with [`BYTES`] of RAM at [`RAM`] and of [`Array`] at
/// [`PERIPHERAL`].
fn machine() -> Machine {
    let mut machine = Machine::new(ADDRESS_BITS).expect("32 address lines");
    machine.map(RAM, BYTES, &[]).expect("RAM");
    machine
        .add_peripheral(
            PERIPHERAL..=PERIPHERAL.offset(BYTES - 1),
            Box::new(Array(vec![0; BYTES as usize])),
        )
        .expect("the array");
    machine
}

fn access(criterion: &mut Criterion) {
    let mut group = criterion.benchmark_group("access");
    group.throughput(Throughput::Bytes(u64::from(BYTES)));
    let mut machine = machine();
    let image: Vec<u8> = (0..BYTES).map(|byte| byte as u8).collect();
    let mut buffer = vec![0; BYTES as usize];
    for (name, at) in [("ram", RAM), ("peripheral", PERIPHERAL)] {
        group.bench_function(format!("{name}_write"), |bencher| {
            bencher.iter(|| machine.write(black_box(at), &image).expect("served"));
        });
        group.bench_function(format!("{name}_read"), |bencher| {
            bencher.iter(|| machine.read(black_box(at), &mut buffer).expect("served"));
        });
    }
    group.finish();
}

criterion_group!(benches, access);
criterion_main!(benches);
