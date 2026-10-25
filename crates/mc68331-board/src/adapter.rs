//! Chip-level peripheral models as board peripherals.
//!
//! A [`MappedPeripheral`] and [`Timed`] model (a DUART, an EEPROM written against
//! [`mc683xx_modules::peripheral`]), or a [`SpiPeripheral`], registers as a
//! peripheral without hand-mapping, its cycles and times converted
//! through the board's system clock ([`TimeBase`]).
//!
//! What the model sees: a [`BusCycle`] with the attachment's select, the
//! address the attachment gives (`offset` plus ADDR on its `lines`, so
//! ADDR itself by default) and its start in system clocks; `now` in
//! system clocks for its time, reset and QSPI calls.  Its `advance`
//! runs at the time its `next_event` gave, as a peripheral's does, where the
//! chip calls a peripheral's at every sync point.  It drives no nets and
//! writes no memories: a model that does is a [`Peripheral`].  The model
//! implements [`Save`], so it is in the board's snapshot: one that cannot
//! save is [`unsavable!`](mc683xx_cpu32::unsavable), and the board's save
//! then fails naming it.  A model that implements
//! [`Inspect`], its adapter made `inspecting`, shows its state to a front
//! end ([`Peripheral::inspect`]); otherwise the adapter shows nothing.  A
//! model both on the bus and on the QSPI is two peripherals, or a
//! [`Peripheral`].

use mc683xx_cpu32::bus::BusError;
use mc683xx_cpu32::bus::PortSize;
use mc683xx_cpu32::cpu::state::Save;
use mc683xx_cpu32::cpu::state::StateError;
use mc683xx_cpu32::cpu::state::decode;
use mc683xx_cpu32::cpu::state::encode;
use mc683xx_cpu32::cycles::Cycles;
use mc683xx_cpu32::peripheral::BusCycle;
use mc683xx_cpu32::peripheral::MappedPeripheral;
use mc683xx_modules::peripheral::QspiClock;
use mc683xx_modules::peripheral::QspiTransfer;
use mc683xx_modules::peripheral::SpiPeripheral;
use mc683xx_modules::peripheral::Timed;

use crate::inspect::Inspection;
use crate::peripheral::Drive;
use crate::peripheral::Peripheral;
use crate::peripheral::PeripheralAccess;
use crate::peripheral::Transfer;
use crate::time::Time;
use crate::time::TimeBase;

/// The PCS levels a [`SpiAdapter`]'s model is told when the board's decode
/// selects it: all low.
///
/// The board decodes PCS for its peripherals, so the
/// model sees its own select as levels it can test against zero.
pub const SELECTED_LEVELS: u8 = 0b0000;
/// The levels it is told when the decode releases it: all high, PCS's
/// idle.
pub const RELEASED_LEVELS: u8 = 0b1111;

/// An adapter's snapshot format: the model's bytes, which version
/// themselves.
const SNAPSHOT_VERSION: u32 = 1;

/// `peripheral`'s snapshot: its own bytes in the adapter's.
fn save<D: Save>(peripheral: &D) -> Result<Vec<u8>, StateError> {
    Ok(encode(SNAPSHOT_VERSION, &peripheral.save()?))
}

/// `peripheral` restored from `snapshot`.
fn restore<D: Save>(peripheral: &mut D, snapshot: &[u8]) -> Result<(), StateError> {
    let bytes: Vec<u8> = decode(SNAPSHOT_VERSION, snapshot)?;
    peripheral.restore(&bytes)
}

/// A model's state as a front end shows it, through its adapter's
/// [`Peripheral::inspect`]: a model of [`mc683xx_modules::peripheral`], which
/// knows nothing of the board, shows itself through this.
pub trait Inspect {
    /// Its state, read-only, as [`Peripheral::inspect`].  By default
    /// empty.
    fn inspect(&self) -> Inspection {
        Inspection::default()
    }
}

/// A model's [`Inspect::inspect`], if its adapter was made `inspecting`.
type Inspector<D> = fn(&D) -> Inspection;

/// `peripheral`'s state, if `inspector` shows it.
fn inspect<D>(inspector: Option<Inspector<D>>, peripheral: &D) -> Inspection {
    inspector.map_or_else(Inspection::default, |inspect| inspect(peripheral))
}

/// The system clock a call gives, kept for the calls that have none.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Clock(Option<TimeBase>);

impl Clock {
    /// The base, from `drive` if it carries one.
    fn update(&mut self, drive: &Drive<'_>) -> TimeBase {
        if let Some(base) = drive.system_clock() {
            self.0 = Some(base);
        }
        self.base()
    }

    /// The base last given.
    fn base(&self) -> TimeBase {
        self.0
            .expect("the board gives a peripheral the system clock before any call")
    }
}

/// A [`MappedPeripheral`] and [`Timed`] model on the bus as a peripheral.
#[derive(Debug, Default)]
pub struct MappedAdapter<D> {
    /// The model.
    peripheral: D,
    /// The board's system clock.
    clock: Clock,
    /// Its inspection, if it shows one.
    inspector: Option<Inspector<D>>,
}

impl<D> MappedAdapter<D> {
    /// `peripheral` as a peripheral.
    pub const fn new(peripheral: D) -> Self {
        Self {
            peripheral,
            clock: Clock(None),
            inspector: None,
        }
    }

    /// It, showing the model's [`Inspect::inspect`] as its own.
    #[must_use]
    pub fn inspecting(self) -> Self
    where
        D: Inspect,
    {
        Self {
            inspector: Some(D::inspect),
            ..self
        }
    }

    /// The model.
    pub const fn peripheral(&self) -> &D {
        &self.peripheral
    }

    /// The model, to change.
    pub const fn peripheral_mut(&mut self) -> &mut D {
        &mut self.peripheral
    }

    /// The cycle the model sees for `access`.
    fn cycle(&mut self, access: PeripheralAccess, drive: &Drive<'_>) -> BusCycle {
        let base = self.clock.update(drive);
        carried(access, base.cycles(access.now))
    }
}

/// The cycle a model sees for `access` at `now`, through its select if
/// one carried it.
fn carried(access: PeripheralAccess, now: Cycles) -> BusCycle {
    let cycle = BusCycle::new(access.address, access.size, access.function_code, now);
    match access.select {
        Some(select) => cycle.through(select.into()),
        None => cycle,
    }
}

impl<D: MappedPeripheral + Timed + Save + 'static> Peripheral for MappedAdapter<D> {
    fn read(&mut self, access: PeripheralAccess, drive: &mut Drive<'_>) -> Result<u32, BusError> {
        let cycle = self.cycle(access, drive);
        self.peripheral.read(cycle)
    }

    fn write(
        &mut self,
        access: PeripheralAccess,
        value: u32,
        drive: &mut Drive<'_>,
    ) -> Result<(), BusError> {
        let cycle = self.cycle(access, drive);
        self.peripheral.write(cycle, value)
    }

    fn wait_states(&mut self, access: PeripheralAccess) -> Option<u8> {
        let cycle = carried(access, self.clock.base().cycles(access.now));
        self.peripheral.wait_states(cycle)
    }

    fn dsack_port(&mut self, access: PeripheralAccess) -> Option<PortSize> {
        let cycle = carried(access, self.clock.base().cycles(access.now));
        self.peripheral.dsack_port(cycle)
    }

    fn interrupt_request(&self) -> bool {
        self.peripheral.interrupt_request()
    }

    fn reset(&mut self, now: Time, drive: &mut Drive<'_>) {
        let base = self.clock.update(drive);
        self.peripheral.reset(base.cycles(now));
    }

    fn system_clock(&mut self, clock: TimeBase) {
        self.clock.0 = Some(clock);
    }

    fn next_event(&self) -> Option<Time> {
        let base = self.clock.0?;
        self.peripheral.next_event().map(|at| base.time(at))
    }

    fn advance(&mut self, now: Time, drive: &mut Drive<'_>) {
        let base = self.clock.update(drive);
        self.peripheral.advance(base.cycles(now));
    }

    fn inspect(&self) -> Inspection {
        inspect(self.inspector, &self.peripheral)
    }

    fn save(&self) -> Result<Vec<u8>, StateError> {
        save(&self.peripheral)
    }

    fn restore(&mut self, snapshot: &[u8]) -> Result<(), StateError> {
        restore(&mut self.peripheral, snapshot)
    }
}

/// A [`SpiPeripheral`] model on the QSPI as a peripheral.
///
/// The board decodes PCS\[3:0\]: the model's `chip_selects` hears
/// `SELECTED_LEVELS` as its attachment's decode selects it and
/// `RELEASED_LEVELS` as it releases it ([`Peripheral::select`], the board's
/// framing), and each transfer with the levels on the pins.  Its
/// `qspi_deferrable` is the model's `deferrable`.
#[derive(Debug, Default)]
pub struct SpiAdapter<D> {
    /// The model.
    peripheral: D,
    /// The board's system clock.
    clock: Clock,
    /// Its inspection, if it shows one.
    inspector: Option<Inspector<D>>,
}

impl<D> SpiAdapter<D> {
    /// `peripheral` as a peripheral.
    pub const fn new(peripheral: D) -> Self {
        Self {
            peripheral,
            clock: Clock(None),
            inspector: None,
        }
    }

    /// It, showing the model's [`Inspect::inspect`] as its own.
    #[must_use]
    pub fn inspecting(self) -> Self
    where
        D: Inspect,
    {
        Self {
            inspector: Some(D::inspect),
            ..self
        }
    }

    /// The model.
    pub const fn peripheral(&self) -> &D {
        &self.peripheral
    }

    /// The model, to change.
    pub const fn peripheral_mut(&mut self) -> &mut D {
        &mut self.peripheral
    }

    /// `now` in system clocks.
    fn cycles(&mut self, now: Time, drive: &Drive<'_>) -> Cycles {
        self.clock.update(drive).cycles(now)
    }
}

impl<D: SpiPeripheral + Save + 'static> Peripheral for SpiAdapter<D> {
    fn reset(&mut self, now: Time, drive: &mut Drive<'_>) {
        let now = self.cycles(now, drive);
        self.peripheral.reset(now);
    }

    fn transfer(&mut self, transfer: Transfer, drive: &mut Drive<'_>) -> u16 {
        let now = self.cycles(transfer.now, drive);
        self.peripheral.transfer(QspiTransfer::new(
            transfer.entry,
            transfer.chip_selects,
            transfer.out,
            transfer.bits,
            transfer.clock,
            now,
        ))
    }

    fn select(&mut self, _: usize, selected: bool, now: Time, drive: &mut Drive<'_>) {
        let now = self.cycles(now, drive);
        let levels = if selected {
            SELECTED_LEVELS
        } else {
            RELEASED_LEVELS
        };
        self.peripheral.chip_selects(levels, now);
    }

    fn qspi_clock(&mut self, clock: QspiClock, now: Time) {
        let now = self.clock.base().cycles(now);
        self.peripheral.clock(clock, now);
    }

    fn qspi_deferrable(&self) -> bool {
        self.peripheral.deferrable()
    }

    fn system_clock(&mut self, clock: TimeBase) {
        self.clock.0 = Some(clock);
    }

    fn inspect(&self) -> Inspection {
        inspect(self.inspector, &self.peripheral)
    }

    fn save(&self) -> Result<Vec<u8>, StateError> {
        save(&self.peripheral)
    }

    fn restore(&mut self, snapshot: &[u8]) -> Result<(), StateError> {
        restore(&mut self.peripheral, snapshot)
    }
}

#[cfg(test)]
mod tests {
    use mc683xx_cpu32::bus::BusError;
    use mc683xx_cpu32::cpu::state::Save;
    use mc683xx_cpu32::cpu::state::StateError;
    use mc683xx_cpu32::cpu::state::Stateless;
    use mc683xx_cpu32::peripheral::BusCycle;
    use mc683xx_cpu32::peripheral::MappedPeripheral;
    use mc683xx_modules::peripheral::QspiTransfer;
    use mc683xx_modules::peripheral::SpiPeripheral;
    use mc683xx_modules::peripheral::Timed;

    use super::Inspect;
    use super::MappedAdapter;
    use super::SNAPSHOT_VERSION;
    use super::SpiAdapter;
    use super::save;
    use crate::inspect::Inspection;
    use crate::inspect::Value;
    use crate::peripheral::Peripheral;

    /// A model on the bus and the QSPI that shows a count.
    #[derive(Debug, Default)]
    struct Counted(u8);

    mc683xx_cpu32::unsavable!(Counted, "a counting model");

    impl MappedPeripheral for Counted {
        fn read(&mut self, _: BusCycle) -> Result<u32, BusError> {
            Ok(0)
        }

        fn write(&mut self, _: BusCycle, _: u32) -> Result<(), BusError> {
            Ok(())
        }
    }

    impl Timed for Counted {}

    impl SpiPeripheral for Counted {
        fn transfer(&mut self, _: QspiTransfer) -> u16 {
            0
        }
    }

    impl Inspect for Counted {
        fn inspect(&self) -> Inspection {
            Inspection::default().integer("count", self.0)
        }
    }

    /// A model that shows nothing of its own.
    #[derive(Debug, Default)]
    struct Plain;

    impl SpiPeripheral for Plain {
        fn transfer(&mut self, _: QspiTransfer) -> u16 {
            0
        }
    }

    impl Inspect for Plain {}

    impl Stateless for Plain {}

    /// A model whose snapshot is three fixed bytes.
    struct Fixed;

    impl Save for Fixed {
        fn save(&self) -> Result<Vec<u8>, StateError> {
            Ok(vec![1, 2, 3])
        }

        fn restore(&mut self, _: &[u8]) -> Result<(), StateError> {
            Ok(())
        }
    }

    #[test]
    fn a_peripheral_adapter_shows_its_model_only_inspecting() {
        let shown = MappedAdapter::new(Counted(3)).inspecting().inspect();
        assert_eq!(shown.get("count"), Some(&Value::Integer(3)));
        assert!(MappedAdapter::new(Counted(3)).inspect().is_empty());
    }

    #[test]
    fn a_qspi_adapter_shows_its_model_only_inspecting() {
        let shown = SpiAdapter::new(Counted(5)).inspecting().inspect();
        assert_eq!(shown.get("count"), Some(&Value::Integer(5)));
        assert!(SpiAdapter::new(Counted(5)).inspect().is_empty());
        assert!(SpiAdapter::new(Plain).inspecting().inspect().is_empty());
    }

    #[test]
    fn the_format_matches_its_golden() {
        snapshot_golden::check(
            env!("CARGO_MANIFEST_DIR"),
            "adapter",
            &[("adapter", SNAPSHOT_VERSION)],
            &save(&Fixed).unwrap(),
        );
    }
}
