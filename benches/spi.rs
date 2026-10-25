//! A QSPI peripheral's transfer walked edge by edge (`mc683xx_board::spi`),
//! against the same register shifted as a word (docs/performance.md).

use std::hint::black_box;

use criterion::Criterion;
use criterion::Throughput;
use criterion::criterion_group;
use criterion::criterion_main;
use mc683xx_board::peripheral::Transfer;
use mc683xx_board::spi::Edge;
use mc683xx_board::spi::Edges;
use mc683xx_board::spi::Mode;
use mc683xx_board::spi::Slave;
use mc683xx_board::spi::exchange;
use mc683xx_board::time::Time;
use mc683xx_cpu32::cycles::Cycles;
use mc683xx_cpu32::frequency::Frequency;
use mc683xx_modules::peripheral::QspiClock;

/// A transfer's bits.
const BITS: u8 = 16;
/// The register's stages: an MC144110's.
const STAGES: u32 = 36;

/// A shift register of `STAGES` stages, MISO its last.
#[derive(Debug, Default)]
struct Register(u64);

impl Register {
    /// Shifts `count` bits of `input` in as a word and returns those
    /// that leave.
    fn shift_word(&mut self, input: u16, count: u8) -> u16 {
        let all = u128::from(self.0) << count | u128::from(input);
        self.0 = all as u64 & ((1 << STAGES) - 1);
        (all >> STAGES & ((1 << count) - 1)) as u16
    }
}

impl Slave for Register {
    fn sample(&mut self, mosi: bool) {
        self.0 = (self.0 << 1 | u64::from(mosi)) & ((1 << STAGES) - 1);
    }

    fn miso(&self) -> Option<bool> {
        Some(self.0 >> (STAGES - 1) & 1 != 0)
    }
}

/// A transfer in SPI mode `number`.
fn transfer(number: u8) -> Transfer {
    let mode = Mode::numbered(number);
    let clock = QspiClock {
        polarity: mode.polarity,
        phase: mode.phase,
        period: Cycles(8),
        system_clock: Frequency::from_hz(16_777_216),
    };
    Transfer::new(0, 0, 0, 0, BITS, clock, Time::ZERO)
}

fn transfers(criterion: &mut Criterion) {
    let mut group = criterion.benchmark_group("spi");
    group.throughput(Throughput::Elements(1));
    let edges = Edges::new(Edge::Falling, Edge::Falling, true);
    for number in [2, 3] {
        let transfer = transfer(number);
        let mut register = Register::default();
        group.bench_function(format!("exchange_mode_{number}"), |bencher| {
            bencher.iter(|| {
                exchange(
                    black_box(transfer.shifting(0x5a5a, BITS)),
                    edges,
                    &mut register,
                )
            });
        });
    }
    let mut register = Register::default();
    group.bench_function("word", |bencher| {
        bencher.iter(|| register.shift_word(black_box(0x5a5a), BITS));
    });
    group.finish();
}

criterion_group!(benches, transfers);
criterion_main!(benches);
