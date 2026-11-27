//! Chip-level peripheral models as board peripherals.
//!
//! A [`MappedPeripheral`] and [`Timed`] model (a DUART, an EEPROM written against
//! [`mc683xx_modules::peripheral`]), or a [`SpiPeripheral`], registers as a
//! peripheral without hand-mapping, its cycles and times converted
//! through the board's system clock ([`TimeBase`](crate::time::TimeBase)).
//!
//! What the model sees: a [`BusCycle`] with the attachment's select, the
//! address the attachment gives (`offset` plus ADDR on its `lines`, so
//! ADDR itself by default) and its start in system clocks; `now` in
//! system clocks for its time, reset and QSPI calls.  Its `advance`
//! runs at the time its `next_event` gave, as a peripheral's does, where the
//! chip calls a peripheral's at every sync point.  It drives nets only
//! if it implements [`Driver`] and its adapter was made `driving`: its
//! [`Driver::drive`] reads the model and drives the adapter's ports
//! after each call that may change the model; a model that needs more
//! of the board is a [`Peripheral`].  It is a `Driver`, not a `Drive`,
//! as [`Drive`] is what it drives through.
//! The model implements [`Save`], so it is in the board's snapshot: one that cannot
//! save is [`unsavable!`](mc683xx_cpu32::unsavable), and the board's save
//! then fails naming it.  A model that implements
//! [`Inspect`], its adapter made `inspecting`, shows its state to a front
//! end ([`Peripheral::inspect`]); otherwise the adapter shows nothing.  A
//! model both on the bus and on the QSPI is two peripherals, or a
//! [`Peripheral`].

use mc683xx_cpu32::BusError;
use mc683xx_cpu32::Cycle;
use mc683xx_cpu32::Cycles;
use mc683xx_cpu32::PortSize;
use mc683xx_cpu32::peripheral::BusCycle;
use mc683xx_cpu32::peripheral::MappedPeripheral;
use mc683xx_cpu32::state::Save;
use mc683xx_cpu32::state::StateError;
use mc683xx_cpu32::state::decode;
use mc683xx_cpu32::state::encode;
use mc683xx_modules::peripheral::QspiClock;
use mc683xx_modules::peripheral::QspiTransfer;
use mc683xx_modules::peripheral::SpiPeripheral;
use mc683xx_modules::peripheral::Timed;
use mc683xx_target::Address;

use crate::inspect::Inspection;
use crate::peripheral::Drive;
use crate::peripheral::Peripheral;
use crate::peripheral::PeripheralAccess;
use crate::peripheral::Transfer;
use crate::time::Moment;
use crate::time::Time;

/// The PCS levels a [`SpiAdapter`]'s model is told when the board's decode
/// selects it: all low.
///
/// The board decodes PCS for its peripherals, so the
/// model sees its own select as levels it can test against zero.
pub(crate) const SELECTED_LEVELS: u8 = 0b0000;
/// The levels it is told when the decode releases it: all high, PCS's
/// idle.
pub(crate) const RELEASED_LEVELS: u8 = 0b1111;

/// An adapter's snapshot format: the model's bytes, which version
/// themselves.
const VERSION: u32 = 1;

/// `peripheral`'s snapshot: its own bytes in the adapter's.
fn save<D: Save>(peripheral: &D) -> Result<Vec<u8>, StateError> {
    Ok(encode(VERSION, &peripheral.save()?))
}

/// `peripheral` restored from `snapshot`.
fn restore<D: Save>(peripheral: &mut D, snapshot: &[u8]) -> Result<(), StateError> {
    let bytes: Vec<u8> = decode(VERSION, snapshot)?;
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

/// A model's outputs on the board, through its adapter's ports: a model
/// of [`mc683xx_modules::peripheral`], which knows nothing of the board,
/// drives nets through this, its adapter made `driving`.
pub trait Driver {
    /// Drives the adapter's ports from the model ([`Drive::drive`]), or
    /// stops the run ([`Drive::stop`]); called after each adapter call
    /// that may change the model.  By default nothing.
    fn drive(&mut self, drive: &mut Drive<'_>) {
        let _ = drive;
    }
}

/// A model's [`Driver::drive`], if its adapter was made `driving`.
type Driving<D> = fn(&mut D, &mut Drive<'_>);

/// `driving` run on `peripheral` and `drive`, if the adapter has one.
fn drive<D>(driving: Option<Driving<D>>, peripheral: &mut D, drive: &mut Drive<'_>) {
    if let Some(driving) = driving {
        driving(peripheral, drive);
    }
}

/// A [`MappedPeripheral`] and [`Timed`] model on the bus as a peripheral.
#[derive(Debug, Default)]
pub struct MappedAdapter<D> {
    /// The model.
    peripheral: D,
    /// Its inspection, if it shows one.
    inspector: Option<Inspector<D>>,
    /// What drives the adapter's nets from the model, if anything.
    driving: Option<Driving<D>>,
}

impl<D> MappedAdapter<D> {
    /// `peripheral` as a peripheral.
    pub const fn new(peripheral: D) -> Self {
        Self {
            peripheral,
            inspector: None,
            driving: None,
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

    /// It, driving its nets from the model: the model's
    /// [`Driver::drive`] runs with the call's [`Drive`] after each call
    /// that may change the model (power-on, `read`, `write`, `reset`,
    /// `advance`) and drives the ports the adapter was registered with.
    #[must_use]
    pub fn driving(self) -> Self
    where
        D: Driver,
    {
        Self {
            driving: Some(D::drive),
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

    /// The model's driver run on `drive`, if it has one.
    fn drive(&mut self, drive: &mut Drive<'_>) {
        self::drive(self.driving, &mut self.peripheral, drive);
    }
}

/// The time of `drive`'s call in the chip's system clocks.
fn cycle(drive: &Drive<'_>) -> Cycle {
    drive.moment().cycle()
}

/// The cycle a model sees for `access` at `moment`, its time in system
/// clocks, through its select if one carried it.
fn carried(access: PeripheralAccess, moment: Moment) -> BusCycle {
    let cycle = BusCycle::new(
        Address(access.address),
        access.size,
        access.function_code,
        moment.cycle(),
    )
    .writing(access.write);
    match access.select {
        Some(select) => cycle.through(select.into()),
        None => cycle,
    }
}

impl<D: MappedPeripheral + Timed + Save + 'static> Peripheral for MappedAdapter<D> {
    fn read(&mut self, access: PeripheralAccess, drive: &mut Drive<'_>) -> Result<u32, BusError> {
        let read = self.peripheral.read(carried(access, drive.moment()));
        self.drive(drive);
        read
    }

    fn write(
        &mut self,
        access: PeripheralAccess,
        value: u32,
        drive: &mut Drive<'_>,
    ) -> Result<(), BusError> {
        let written = self
            .peripheral
            .write(carried(access, drive.moment()), value);
        self.drive(drive);
        written
    }

    fn peek(&self, access: PeripheralAccess, data: &mut [u8], moment: Moment) -> bool {
        self.peripheral.peek(carried(access, moment), data)
    }

    fn poke(&mut self, access: PeripheralAccess, data: &[u8], moment: Moment) -> bool {
        self.peripheral.poke(carried(access, moment), data)
    }

    fn wait_states(&mut self, access: PeripheralAccess, moment: Moment) -> Option<Cycles> {
        self.peripheral.wait_states(carried(access, moment))
    }

    fn dsack_port(&mut self, access: PeripheralAccess, moment: Moment) -> Option<PortSize> {
        self.peripheral.dsack_port(carried(access, moment))
    }

    fn interrupt_request(&self) -> bool {
        self.peripheral.interrupt_request()
    }

    fn power_on(&mut self, drive: &mut Drive<'_>) {
        self.drive(drive);
    }

    fn reset(&mut self, drive: &mut Drive<'_>) {
        self.peripheral.reset(cycle(drive));
        self.drive(drive);
    }

    fn next_event(&self, moment: Moment) -> Option<Time> {
        self.peripheral
            .next_event()
            .map(|at| moment.system_clock().time(at))
    }

    fn advance(&mut self, drive: &mut Drive<'_>) {
        self.peripheral.advance(cycle(drive));
        self.drive(drive);
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
/// levels 0b0000 as its attachment's decode selects it and 0b1111, PCS's
/// idle, as it releases it ([`Peripheral::set_selected`], the board's
/// framing), and each transfer with the levels on the pins.  Its
/// `qspi_deferrable` is the model's `deferrable`.
#[derive(Debug, Default)]
pub struct SpiAdapter<D> {
    /// The model.
    peripheral: D,
    /// Its inspection, if it shows one.
    inspector: Option<Inspector<D>>,
    /// What drives the adapter's nets from the model, if anything.
    driving: Option<Driving<D>>,
}

impl<D> SpiAdapter<D> {
    /// `peripheral` as a peripheral.
    pub const fn new(peripheral: D) -> Self {
        Self {
            peripheral,
            inspector: None,
            driving: None,
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

    /// It, driving its nets from the model: the model's
    /// [`Driver::drive`] runs with the call's [`Drive`] after each call
    /// that may change the model (power-on, `reset`, `transfer`, a
    /// select or release) and drives the ports the adapter was
    /// registered with.
    #[must_use]
    pub fn driving(self) -> Self
    where
        D: Driver,
    {
        Self {
            driving: Some(D::drive),
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

    /// The model's driver run on `drive`, if it has one.
    fn drive(&mut self, drive: &mut Drive<'_>) {
        self::drive(self.driving, &mut self.peripheral, drive);
    }
}

impl<D: SpiPeripheral + Save + 'static> Peripheral for SpiAdapter<D> {
    fn power_on(&mut self, drive: &mut Drive<'_>) {
        self.drive(drive);
    }

    fn reset(&mut self, drive: &mut Drive<'_>) {
        self.peripheral.reset(cycle(drive));
        self.drive(drive);
    }

    fn transfer(&mut self, transfer: Transfer, drive: &mut Drive<'_>) -> u16 {
        let shifted = self.peripheral.transfer(
            QspiTransfer::new(transfer.out, transfer.bits, transfer.clock, cycle(drive))
                .at_entry(transfer.entry)
                .selecting(transfer.chip_selects),
        );
        self.drive(drive);
        shifted
    }

    fn set_selected(&mut self, _: usize, selected: bool, drive: &mut Drive<'_>) {
        let levels = if selected {
            SELECTED_LEVELS
        } else {
            RELEASED_LEVELS
        };
        self.peripheral.set_chip_selects(levels, cycle(drive));
        self.drive(drive);
    }

    fn set_qspi_clock(&mut self, clock: QspiClock, moment: Moment) {
        self.peripheral.set_clock(clock, moment.cycle());
    }

    fn qspi_deferrable(&self) -> bool {
        self.peripheral.deferrable()
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
    use mc683xx_cpu32::BusError;
    use mc683xx_cpu32::peripheral::BusCycle;
    use mc683xx_cpu32::peripheral::MappedPeripheral;
    use mc683xx_cpu32::state::Save;
    use mc683xx_cpu32::state::StateError;
    use mc683xx_cpu32::state::Stateless;
    use mc683xx_modules::peripheral::QspiTransfer;
    use mc683xx_modules::peripheral::SpiPeripheral;
    use mc683xx_modules::peripheral::Timed;

    use super::Inspect;
    use super::MappedAdapter;
    use super::SpiAdapter;
    use super::VERSION;
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
            &[("adapter", VERSION)],
            &save(&Fixed).unwrap(),
        );
    }
}
