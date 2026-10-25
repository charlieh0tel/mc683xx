//! A QSPI peripheral's SPI edges: what a slave shifts in and what the master
//! captures, bit by bit, from the SCK edges each acts on.
//!
//! A peripheral declares its datasheet's edges ([`Edges`]) and its shift logic
//! ([`Slave`]); `exchange` runs a transfer in the master's mode
//! ([`Mode`], from the transfer's [`QspiClock`]) and returns the bits the
//! master captures.  Nothing here is per mode: each mode is only which
//! edge comes first and which edges the master changes MOSI and
//! captures MISO on (QSMRM 4.3.1; MC68331UM Figures A-16 and A-17).
//!
//! At each edge, everything that edge captures is the level before it,
//! and every level it changes changes after: the master's MISO capture
//! and the slave's MOSI sample first, then the slave's MISO change and
//! the master's MOSI change.  A slave that samples MOSI on the edge the
//! master changes it so takes the level before the change (with CPHA
//! 0 the bit meant for it, with CPHA 1, whose leading edge puts each
//! bit, the bit before), and a master that captures MISO on the edge
//! the slave changes it the bit before; each side's output changes
//! after its clock edge (MC68331UM Table A-9, data valid and hold
//! after SCK).
//!
//! A peripheral whose MISO is undriven, or that ignores MOSI, need not use
//! this: a transfer's word in or out is enough for it.

use mc683xx_modules::peripheral::ClockPhase;
use mc683xx_modules::peripheral::ClockPolarity;
use mc683xx_modules::peripheral::QspiClock;

use crate::peripheral::Transfer;

/// MOSI's level before a transfer's first bit and after its last: the
/// QSPI drives PORTQS's MOSI bit there ("PORT DATA", MC68331UM Figures
/// A-16 and A-17).  A peripheral does not see PORTQS, so this is its value
/// out of reset, 0 (QSMRM 3.3.1).
const MOSI_BETWEEN_TRANSFERS: bool = false;

/// MISO's level when no slave drives it: all ones, as a QSPI no peripheral
/// answers reads ([`Peripheral::transfer`]).
///
/// [`Peripheral::transfer`]: crate::peripheral::Peripheral::transfer
pub const UNDRIVEN: bool = true;

/// An SCK edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Edge {
    /// Low to high.
    Rising,
    /// High to low.
    Falling,
}

impl Edge {
    /// The other edge.
    #[must_use]
    pub const fn opposite(self) -> Self {
        match self {
            Self::Rising => Self::Falling,
            Self::Falling => Self::Rising,
        }
    }
}

/// The master's SPI mode: SPCR0's CPOL and CPHA (QSMRM 4.3.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Mode {
    /// CPOL: SCK's level between transfers.
    pub polarity: ClockPolarity,
    /// CPHA: whether data is captured or changed on the leading edge.
    pub phase: ClockPhase,
}

impl Mode {
    /// The mode of `polarity` and `phase`.
    #[must_use]
    pub const fn new(polarity: ClockPolarity, phase: ClockPhase) -> Self {
        Self { polarity, phase }
    }

    /// SPI mode `number`, 0 to 3, by the common numbering the QSMRM
    /// does not use: CPOL its bit 1, CPHA its bit 0.  Higher bits are
    /// ignored.
    #[must_use]
    pub const fn numbered(number: u8) -> Self {
        Self {
            polarity: if number & 0b10 == 0 {
                ClockPolarity::IdleLow
            } else {
                ClockPolarity::IdleHigh
            },
            phase: if number & 0b01 == 0 {
                ClockPhase::CaptureLeading
            } else {
                ClockPhase::ChangeLeading
            },
        }
    }

    /// The mode `clock` shifts in.
    #[must_use]
    pub const fn of(clock: QspiClock) -> Self {
        Self::new(clock.polarity, clock.phase)
    }

    /// Its number, as `numbered` takes it.
    #[must_use]
    pub const fn number(self) -> u8 {
        let polarity = match self.polarity {
            ClockPolarity::IdleLow => 0,
            ClockPolarity::IdleHigh => 0b10,
        };
        let phase = match self.phase {
            ClockPhase::CaptureLeading => 0,
            ClockPhase::ChangeLeading => 0b01,
        };
        polarity | phase
    }

    /// Each bit's first SCK edge, away from the idle level.
    #[must_use]
    pub const fn leading(self) -> Edge {
        match self.polarity {
            ClockPolarity::IdleLow => Edge::Rising,
            ClockPolarity::IdleHigh => Edge::Falling,
        }
    }

    /// Each bit's second SCK edge, back to the idle level.
    #[must_use]
    pub const fn trailing(self) -> Edge {
        self.leading().opposite()
    }

    /// The edge the master captures MISO on.
    #[must_use]
    pub const fn capture(self) -> Edge {
        match self.phase {
            ClockPhase::CaptureLeading => self.leading(),
            ClockPhase::ChangeLeading => self.trailing(),
        }
    }

    /// The edge the master changes MOSI on.  With CPHA 0 the first bit
    /// is on MOSI before the first edge, and each trailing edge puts
    /// the next; with CPHA 1 each leading edge puts its bit.
    #[must_use]
    pub const fn change(self) -> Edge {
        self.capture().opposite()
    }
}

/// A slave's edges, as its datasheet gives them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Edges {
    /// The edge it samples MOSI on.
    pub sample: Edge,
    /// The edge it changes MISO on.
    pub change: Edge,
    /// Whether its first MISO bit is valid when its select asserts,
    /// before any edge: a register's last stage, which is always on
    /// its output.  If not, MISO is undriven until the frame's first
    /// change edge, which puts the first bit rather than the next.
    pub first_bit_at_select: bool,
}

impl Edges {
    /// A slave that samples on `sample`, changes on `change`, and has
    /// its first bit out at its select if `first_bit_at_select`.
    #[must_use]
    pub const fn new(sample: Edge, change: Edge, first_bit_at_select: bool) -> Self {
        Self {
            sample,
            change,
            first_bit_at_select,
        }
    }
}

/// A slave bit by bit: its shift logic, which `exchange` clocks at its
/// edges.
pub trait Slave {
    /// Its sample edge, with MOSI's level before it.
    fn sample(&mut self, mosi: bool);

    /// Its change edge: MISO moves to its next bit.  By default
    /// nothing changes, for a slave whose output changes as it samples
    /// (a shift register's last stage).
    fn change(&mut self) {}

    /// MISO's level now, or [`None`] if it does not drive it.
    fn miso(&self) -> Option<bool>;
}

/// Runs `transfer` against `slave` with `edges`, in the transfer's mode,
/// and returns the bits the master captures, right-aligned.
///
/// It shifts the transfer's `bits` of `out`, at most 16, most
/// significant first, as the QSPI shifts them (QSMRM 4.3.6).
pub fn exchange<S: Slave + ?Sized>(transfer: Transfer, edges: Edges, slave: &mut S) -> u16 {
    let mode = Mode::of(transfer.clock);
    let out = transfer.out;
    let bits = transfer.bits.min(u16::BITS as u8);
    let bit = |index: u8| {
        if index < bits {
            out >> (bits - 1 - index) & 1 != 0
        } else {
            MOSI_BETWEEN_TRANSFERS
        }
    };
    let change_leading = mode.phase == ClockPhase::ChangeLeading;
    let mut mosi = if change_leading {
        MOSI_BETWEEN_TRANSFERS
    } else {
        bit(0)
    };
    let mut driving = edges.first_bit_at_select || !transfer.starts_frame;
    let mut captured: u16 = 0;
    for index in 0..bits {
        for edge in [mode.leading(), mode.trailing()] {
            if edge == mode.capture() {
                let level = if driving {
                    slave.miso().unwrap_or(UNDRIVEN)
                } else {
                    UNDRIVEN
                };
                captured = captured << 1 | u16::from(level);
            }
            if edge == edges.sample {
                slave.sample(mosi);
            }
            if edge == edges.change {
                if driving {
                    slave.change();
                }
                driving = true;
            }
            if edge == mode.change() {
                mosi = bit(if change_leading { index } else { index + 1 });
            }
        }
    }
    captured
}

#[cfg(test)]
mod tests {
    use mc683xx_cpu32::cycles::Cycles;
    use mc683xx_cpu32::frequency::Frequency;
    use mc683xx_modules::peripheral::ClockPhase;
    use mc683xx_modules::peripheral::ClockPolarity;
    use mc683xx_modules::peripheral::QspiClock;

    use super::Edge;
    use super::Edges;
    use super::Mode;
    use super::Slave;
    use super::exchange;
    use crate::peripheral::Transfer;
    use crate::time::Time;

    /// A transfer in SPI mode `number`, a frame's first.
    fn transfer(number: u8) -> Transfer {
        let mode = Mode::numbered(number);
        let clock = QspiClock {
            polarity: mode.polarity,
            phase: mode.phase,
            period: Cycles(8),
            system_clock: Frequency::from_hz(16_777_216),
        };
        Transfer::new(0, 0, 0, 0, 0, clock, Time::ZERO)
    }

    /// An 8-stage shift register, MISO its last stage.
    #[derive(Debug, Default)]
    struct Register(u8);

    impl Slave for Register {
        fn sample(&mut self, mosi: bool) {
            self.0 = self.0 << 1 | u8::from(mosi);
        }

        fn miso(&self) -> Option<bool> {
            Some(self.0 & 0x80 != 0)
        }
    }

    /// A slave that sends `0` bits from its select, one each change
    /// edge, and keeps what it samples.
    #[derive(Debug, Default)]
    struct Sender {
        /// What it sends, the next bit at bit 7.
        out: u8,
        /// What it sampled.
        sampled: u8,
    }

    impl Slave for Sender {
        fn sample(&mut self, mosi: bool) {
            self.sampled = self.sampled << 1 | u8::from(mosi);
        }

        fn change(&mut self) {
            self.out <<= 1;
        }

        fn miso(&self) -> Option<bool> {
            Some(self.out & 0x80 != 0)
        }
    }

    #[test]
    fn each_mode_captures_and_changes_on_the_qsmrms_edges() {
        // CPHA 0 captures on the leading edge, CPHA 1 on the trailing
        // (QSMRM 4.3.1); CPOL 1 leads with the fall
        let edges = [
            (Edge::Rising, Edge::Rising, Edge::Falling),
            (Edge::Rising, Edge::Falling, Edge::Rising),
            (Edge::Falling, Edge::Falling, Edge::Rising),
            (Edge::Falling, Edge::Rising, Edge::Falling),
        ];
        for (number, (leading, capture, change)) in (0..4).zip(edges) {
            let mode = Mode::numbered(number);
            assert_eq!(
                (mode.leading(), mode.capture(), mode.change()),
                (leading, capture, change),
                "mode {number}"
            );
            assert_eq!(mode.number(), number);
        }
        assert_eq!(
            Mode::numbered(3),
            Mode::new(ClockPolarity::IdleHigh, ClockPhase::ChangeLeading)
        );
    }

    #[test]
    fn a_register_on_the_falling_edge_reads_early_in_mode_3() {
        // samples and shifts on the fall: in modes 0 to 2 the master
        // captures the stage before each shift; in mode 3 the fall
        // leads, so it captures after, and the fall samples MOSI as the
        // master changes it, the bit before
        let edges = Edges::new(Edge::Falling, Edge::Falling, true);
        for number in 0..3 {
            let mut register = Register(0xa5);
            let back = exchange(transfer(number).shifting(0x3c, 8), edges, &mut register);
            assert_eq!((back, register.0), (0xa5, 0x3c), "mode {number}");
        }
        let mut register = Register(0xa5);
        let back = exchange(transfer(3).shifting(0x3c, 8), edges, &mut register);
        // one bit early, the last the first bit shifted in, MOSI's level
        // before the transfer, 0
        assert_eq!(back, 0x4a);
        assert_eq!(register.0, 0x1e, "one bit late");
    }

    #[test]
    fn a_register_on_the_rising_edge_is_skewed_in_mode_1() {
        let edges = Edges::new(Edge::Rising, Edge::Rising, true);
        for number in [0, 2, 3] {
            let mut register = Register(0xa5);
            let back = exchange(transfer(number).shifting(0x3c, 8), edges, &mut register);
            assert_eq!((back, register.0), (0xa5, 0x3c), "mode {number}");
        }
        let mut register = Register(0xa5);
        assert_eq!(
            exchange(transfer(1).shifting(0x3c, 8), edges, &mut register),
            0x4a
        );
        assert_eq!(register.0, 0x1e);
    }

    #[test]
    fn a_slave_that_drives_at_its_first_change_edge_reads_late_in_mode_0() {
        // sample on the rise, change on the fall, the first bit put by
        // the first fall: mode 3's edges
        let edges = Edges::new(Edge::Rising, Edge::Falling, false);
        let mut sender = Sender {
            out: 0xc3,
            sampled: 0,
        };
        assert_eq!(
            exchange(transfer(3).shifting(0x96, 8), edges, &mut sender),
            0xc3
        );
        assert_eq!(sender.sampled, 0x96);
        // in mode 0 the first rise captures it undriven, each bit after
        // one late
        let mut sender = Sender {
            out: 0xc3,
            sampled: 0,
        };
        assert_eq!(
            exchange(transfer(0).shifting(0x96, 8), edges, &mut sender),
            0x80 | 0xc3 >> 1
        );
        assert_eq!(sender.sampled, 0x96);
        // a frame's later transfer is driven from its start
        let mut sender = Sender {
            out: 0xc3,
            sampled: 0,
        };
        let later = transfer(3).continuing();
        assert_eq!(exchange(later.shifting(0, 8), edges, &mut sender), 0x86);
    }

    #[test]
    fn an_undriven_miso_reads_ones() {
        /// A slave that only listens.
        struct Listener;

        impl Slave for Listener {
            fn sample(&mut self, _: bool) {}

            fn miso(&self) -> Option<bool> {
                None
            }
        }

        let edges = Edges::new(Edge::Rising, Edge::Falling, true);
        assert_eq!(
            exchange(transfer(0).shifting(0, 12), edges, &mut Listener),
            0xfff
        );
        assert_eq!(
            exchange(transfer(0).shifting(0, 16), edges, &mut Listener),
            0xffff
        );
    }
}
