//! Save and restore the chip ([PLAN.md], State): the core's snapshot,
//! the SIM, the modules', the host's peripherals' and the QSPI peripheral's
//! snapshots, board time and the pins the host drives.
//!
//! The wiring is the builder's, so a snapshot restores into a chip
//! built the same way.
//!
//! [PLAN.md]: https://github.com/charlieh0tel/mc683xx/blob/main/PLAN.md

use mc683xx_cpu32::cpu::Halt;
use mc683xx_cpu32::cpu::state::StateError;
use mc683xx_cpu32::cpu::state::decode;
use mc683xx_cpu32::cpu::state::encode;
use mc683xx_cpu32::cpu::state::restore_each;
use mc683xx_cpu32::cycles::Cycles;
use mc683xx_cpu32::memory::RegionId;
use mc683xx_modules::qsm::Qsm;
use mc683xx_modules::sim::Sim;
use serde::Deserialize;
use serde::Serialize;

use crate::board::Board;
use crate::board::QSM;
use crate::chip::Mc68331;

/// The format of [`Mc68331::save`]'s bytes.  The core's, the modules',
/// the peripherals' and the QSPI peripheral's snapshots inside carry their
/// own.
pub const VERSION: u32 = 10;

/// What the QSPI's peripheral's save and restore errors name.
const QSPI_PERIPHERAL: &str = "the QSPI peripheral";

/// What a snapshot of the chip holds.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ChipState {
    /// The core's snapshot.
    cpu: Vec<u8>,
    /// The SIM.
    sim: Sim,
    /// Each module's snapshot, in the board's order.
    modules: Vec<Vec<u8>>,
    /// Each peripheral's snapshot, in the order the builder added them.
    peripherals: Vec<Vec<u8>>,
    /// The QSPI's peripheral's snapshot, if the builder attached one.
    qspi: Option<Vec<u8>>,
    /// Now, on the system clock.
    now: Cycles,
    /// Why the CPU halted, while it stays halted.
    halt: Option<Halt>,
    /// The IRQ pins the host asserts.
    interrupt_pins: u8,
    /// Whether a decode change awaits a page table rebuild.
    decode_changed: bool,
    /// The memories diverted, each to its peripheral.
    diverted: Vec<(RegionId, usize)>,
    /// Each fixed memory's base and length, in the order added: the
    /// wiring a snapshot restores into.
    fixed: Vec<(u32, u32)>,
    /// The `DsackFault`s told of each peripheral, so a restored chip does
    /// not tell them again.
    dsack_told: Vec<u8>,
}

impl Board {
    /// Each peripheral's snapshot, in the order added; an error names the
    /// peripheral.
    fn save_peripherals(&self) -> Result<Vec<Vec<u8>>, StateError> {
        self.peripherals
            .iter()
            .enumerate()
            .map(|(index, peripheral)| peripheral.save().map_err(|error| named(index, error)))
            .collect()
    }

    /// Restores peripheral `index` from `snapshot`; an error names it.
    fn restore_peripheral(&mut self, index: usize, snapshot: &[u8]) -> Result<(), StateError> {
        self.peripherals[index]
            .restore(snapshot)
            .map_err(|error| named(index, error))
    }

    /// The QSPI's peripheral's snapshot, if there is one; an error names
    /// it.
    fn save_qspi(&self) -> Result<Option<Vec<u8>>, StateError> {
        self.module::<Qsm>(QSM)
            .attached()
            .map(|peripheral| {
                peripheral
                    .save()
                    .map_err(|error| error.of_peripheral(QSPI_PERIPHERAL))
            })
            .transpose()
    }

    /// Restores the QSPI's peripheral from `snapshot`, which the caller
    /// has checked is there if the peripheral is; an error names it.
    fn restore_qspi(&mut self, snapshot: &[u8]) -> Result<(), StateError> {
        self.module_mut::<Qsm>(QSM)
            .attached_mut()
            .map_or(Ok(()), |peripheral| {
                peripheral
                    .restore(snapshot)
                    .map_err(|error| error.of_peripheral(QSPI_PERIPHERAL))
            })
    }
}

/// `error`, of peripheral `index`: its [`PeripheralId`](mc683xx_cpu32::peripheral::PeripheralId)'s
/// place, in the order the builder added the peripherals.
fn named(index: usize, error: StateError) -> StateError {
    error.of_peripheral(&format!("peripheral {index}"))
}

impl Mc68331 {
    /// The chip's whole state as a snapshot: the core (registers,
    /// memory, run-loop timing), the SIM, the modules, every peripheral
    /// and the QSPI's peripheral
    /// ([`Save`](mc683xx_cpu32::cpu::state::Save)), board time and the
    /// IRQ pins.  Not the host's hooks, tracing or events not yet taken.
    ///
    /// # Errors
    ///
    /// [`StateError::Unsupported`] if a module cannot save; a peripheral
    /// that cannot, named by its place in the order the builder added
    /// the peripherals (`peripheral 0` first; [`StateError::Peripheral`]), or
    /// the QSPI's peripheral, named `the QSPI peripheral`.
    pub fn save(&self) -> Result<Vec<u8>, StateError> {
        let board = &self.board;
        let state = ChipState {
            cpu: self.cpu.save(),
            sim: board.sim.clone(),
            modules: board
                .modules
                .iter()
                .map(|module| module.save())
                .collect::<Result<_, _>>()?,
            peripherals: board.save_peripherals()?,
            qspi: board.save_qspi()?,
            now: self.now,
            halt: self.halt,
            interrupt_pins: board.interrupt_pins,
            decode_changed: board.decode_changed,
            diverted: board.diverted.clone(),
            fixed: board.fixed_layout(),
            dsack_told: board.dsack_told.clone(),
        };
        Ok(encode(VERSION, &state))
    }

    /// Restores a snapshot `save` made of a chip built the same way.
    /// Everything is checked before anything changes: the SIM against
    /// this chip's ([`Sim::check_snapshot`]), the count of modules and
    /// of peripherals, whether a QSPI peripheral is attached, each fixed
    /// memory's base and length, each
    /// diversion's peripheral, then each peripheral's snapshot, the QSPI
    /// peripheral's, each module's and the core's.  A peripheral's or module's snapshot can
    /// be checked only by restoring it, so each is saved first and the
    /// ones restored put back if a later one, or the core, refuses
    /// ([`restore_each`]).  On an error the chip is unchanged.  The page
    /// tables are then rebuilt from this chip's wiring, not taken from
    /// the snapshot.
    ///
    /// # Errors
    ///
    /// A [`StateError`] if `snapshot` is not a chip's snapshot of this
    /// format, or does not fit this chip: other modules, another count
    /// of peripherals, a QSPI peripheral where this chip has none or none
    /// where it has one (`StateError::mismatch("the QSPI peripheral")`),
    /// fixed memories at other bases or of other lengths
    /// (`StateError::mismatch("fixed memories")`),
    /// another reset configuration, a diversion to a peripheral
    /// this chip lacks, or a module's or the core's state it refuses; a
    /// peripheral or the QSPI's peripheral that cannot save or refuses its
    /// snapshot, named ([`StateError::Peripheral`]).
    pub fn restore(&mut self, snapshot: &[u8]) -> Result<(), StateError> {
        let state: ChipState = decode(VERSION, snapshot)?;
        if state.modules.len() != self.board.modules.len() {
            return Err(StateError::mismatch("modules"));
        }
        if state.peripherals.len() != self.board.peripherals.len() {
            return Err(StateError::mismatch("peripherals"));
        }
        if state.qspi.is_some() != self.qsm().attached().is_some() {
            return Err(StateError::mismatch(QSPI_PERIPHERAL));
        }
        if state.fixed != self.board.fixed_layout() {
            return Err(StateError::mismatch("fixed memories"));
        }
        state.sim.check_snapshot(&self.board.sim)?;
        self.board.check_diverted(&state.diverted)?;
        self.board.check_dsack_told(&state.dsack_told)?;
        let board = &mut self.board;
        let module_backups = board
            .modules
            .iter()
            .map(|module| module.save())
            .collect::<Result<Vec<_>, _>>()?;
        // The parts in the order restored: the peripherals, the QSPI's
        // peripheral if there is one, the modules, the core.
        let backups: Vec<Vec<u8>> = board
            .save_peripherals()?
            .into_iter()
            .chain(board.save_qspi()?)
            .chain(module_backups)
            .collect();
        let peripherals = board.peripherals.len();
        let qspi = usize::from(state.qspi.is_some());
        let modules = board.modules.len();
        let parts: Vec<&[u8]> = state
            .peripherals
            .iter()
            .map(Vec::as_slice)
            .chain(state.qspi.as_deref())
            .chain(state.modules.iter().map(Vec::as_slice))
            .chain([state.cpu.as_slice()])
            .collect();
        let cpu = &mut self.cpu;
        restore_each(&parts, &backups, |part, bytes| {
            if part < peripherals {
                board.restore_peripheral(part, bytes)
            } else if part < peripherals + qspi {
                board.restore_qspi(bytes)
            } else if part < peripherals + qspi + modules {
                board.modules[part - peripherals - qspi].restore(bytes)
            } else {
                cpu.restore(bytes)
            }
        })?;
        board.sim = state.sim;
        board.interrupt_pins = state.interrupt_pins;
        board.modules_changed();
        board.diversion.take();
        board.diverted = state.diverted;
        board.dsack_told = state.dsack_told;
        // The page tables are this chip's wiring's, not the snapshot's.
        board.rebuild(&mut self.cpu.memory);
        board.decode_changed = state.decode_changed;
        board.stop_request.withdraw();
        board.stop_taken = false;
        board.output_seen = board.seen();
        self.now = state.now;
        self.halt = state.halt;
        Ok(())
    }
}

/// `snapshot` with `change` made to its diversions.
#[cfg(test)]
pub(crate) fn with_diverted(
    snapshot: &[u8],
    change: impl FnOnce(&mut Vec<(RegionId, usize)>),
) -> Vec<u8> {
    let mut state: ChipState = decode(VERSION, snapshot).expect("a chip's snapshot");
    change(&mut state.diverted);
    encode(VERSION, &state)
}

/// `snapshot` with the core's snapshot `cpu`.
#[cfg(test)]
pub(crate) fn with_cpu(snapshot: &[u8], cpu: Vec<u8>) -> Vec<u8> {
    let state: ChipState = decode(VERSION, snapshot).expect("a chip's snapshot");
    encode(VERSION, &ChipState { cpu, ..state })
}

/// `snapshot` with module `index`'s snapshot `module`.
#[cfg(test)]
pub(crate) fn with_module(snapshot: &[u8], index: usize, module: Vec<u8>) -> Vec<u8> {
    let mut state: ChipState = decode(VERSION, snapshot).expect("a chip's snapshot");
    state.modules[index] = module;
    encode(VERSION, &state)
}

/// `snapshot` with peripheral `index`'s snapshot `peripheral`.
#[cfg(test)]
pub(crate) fn with_peripheral(snapshot: &[u8], index: usize, peripheral: Vec<u8>) -> Vec<u8> {
    let mut state: ChipState = decode(VERSION, snapshot).expect("a chip's snapshot");
    state.peripherals[index] = peripheral;
    encode(VERSION, &state)
}

/// `snapshot` with the QSPI's peripheral's snapshot `qspi`.
#[cfg(test)]
pub(crate) fn with_qspi(snapshot: &[u8], qspi: Option<Vec<u8>>) -> Vec<u8> {
    let state: ChipState = decode(VERSION, snapshot).expect("a chip's snapshot");
    encode(VERSION, &ChipState { qspi, ..state })
}

/// `snapshot` with the `DsackFault`s told `dsack_told`.
#[cfg(test)]
pub(crate) fn with_dsack_told(snapshot: &[u8], dsack_told: Vec<u8>) -> Vec<u8> {
    let state: ChipState = decode(VERSION, snapshot).expect("a chip's snapshot");
    encode(
        VERSION,
        &ChipState {
            dsack_told,
            ..state
        },
    )
}
