//! The Python peripheral adapter, on objects defined in Python.

use std::ffi::CStr;

use mc683xx_cpu32::bus::BusError;
use mc683xx_cpu32::cycles::Cycles;
use mc683xx_cpu32::frequency::Frequency;
use mc683xx_cpu32::function_code::FunctionCode;
use mc683xx_cpu32::peripheral::BusCycle;
use mc683xx_cpu32::peripheral::MappedPeripheral;
use mc683xx_cpu32::size::Size;
use mc683xx_modules::chip_select::ChipSelect;
use mc683xx_modules::peripheral::ClockPhase;
use mc683xx_modules::peripheral::ClockPolarity;
use mc683xx_modules::peripheral::QspiClock;
use mc683xx_modules::peripheral::QspiTransfer;
use mc683xx_modules::peripheral::SpiPeripheral;
use mc683xx_modules::peripheral::Timed;
use pyo3::Bound;
use pyo3::PyAny;
use pyo3::Python;
use pyo3::exceptions::PyKeyboardInterrupt;
use pyo3::exceptions::PyRuntimeError;
use pyo3::exceptions::PyTypeError;
use pyo3::exceptions::PyValueError;
use pyo3::exceptions::PyZeroDivisionError;
use pyo3::types::PyAnyMethods;
use pyo3::types::PyModule;
use pyo3::types::PyModuleMethods;

use crate::exceptions;
use crate::mapped::Pending;
use crate::mapped::PythonMapped;
use crate::mapped::UNDRIVEN;
use crate::qspi::PythonSpiPeripheral;

/// Python peripherals for the tests.  `BusError` is the module's, which
/// the test puts in the code's globals.
const PERIPHERALS: &CStr = cr#"
class Echo:
    def __init__(self):
        self.writes = []
        self.resets = []
        self.advanced = []

    def read(self, cycle):
        return cycle.address & 0xff

    def write(self, cycle, value):
        self.writes.append((cycle.select, cycle.address, cycle.size, value))

    def wait_states(self, cycle):
        return None if cycle.address == 0 else 3

    def interrupt_request(self):
        return 1

    def reset(self, now):
        self.resets.append(now)

    def next_event(self):
        return 1000

    def advance(self, now):
        self.advanced.append(now)

class Wide:
    def read(self, cycle):
        return 0x1234

    def write(self, cycle, value):
        raise BusError()

class Broken:
    def read(self, cycle):
        return 1 // 0

    def write(self, cycle, value):
        pass

    def interrupt_request(self):
        raise ValueError("no")

class Bare:
    def read(self, cycle):
        return 0

    def write(self, cycle, value):
        pass

class Timer:
    pass

class Peripheral:
    def __init__(self):
        self.levels = []

    def transfer(self, transfer):
        return transfer.out >> 8 if transfer.chip_selects else 0x1ff

    def chip_selects(self, levels, now):
        self.levels.append((levels, now))
"#;

/// A word cycle on CS3 at `address`.
fn cycle(address: u32, size: Size) -> BusCycle {
    BusCycle::new(address, size, FunctionCode::SUPERVISOR_DATA, Cycles(7))
        .through(ChipSelect::from_index(4).unwrap().into())
}

/// Runs `test` with the test peripherals' module.
fn with_peripherals(test: impl FnOnce(&Bound<'_, PyModule>)) {
    Python::initialize();
    Python::attach(|py| {
        let module = PyModule::new(py, "peripherals").unwrap();
        module
            .add("BusError", py.get_type::<exceptions::BusError>())
            .unwrap();
        py.run(PERIPHERALS, Some(&module.dict()), None).unwrap();
        test(&module);
    });
}

/// A new `class` from `module`.
fn object<'py>(module: &Bound<'py, PyModule>, class: &str) -> Bound<'py, PyAny> {
    module.getattr(class).unwrap().call0().unwrap()
}

#[test]
fn a_peripheral_answers_its_cycles() {
    with_peripherals(|module| {
        let echo = object(module, "Echo");
        let pending = Pending::default();
        let mut peripheral = PythonMapped::mapped(&echo, pending.clone()).unwrap();
        assert_eq!(peripheral.read(cycle(0x20_0012, Size::Word)), Ok(0x12));
        assert_eq!(peripheral.write(cycle(0x20_0014, Size::Byte), 0xab), Ok(()));
        assert_eq!(peripheral.wait_states(cycle(0, Size::Word)), None);
        assert_eq!(peripheral.wait_states(cycle(2, Size::Word)), Some(3));
        assert!(peripheral.interrupt_request());
        peripheral.reset(Cycles(5));
        assert_eq!(peripheral.next_event(), Some(Cycles(1000)));
        peripheral.advance(Cycles(1000));
        assert!(pending.check().is_ok());
        let writes: Vec<(String, u32, u32, u32)> =
            echo.getattr("writes").unwrap().extract().unwrap();
        assert_eq!(writes, [("CS3".to_owned(), 0x20_0014, 1, 0xab)]);
        let resets: Vec<u64> = echo.getattr("resets").unwrap().extract().unwrap();
        assert_eq!(resets, [5]);
        let advanced: Vec<u64> = echo.getattr("advanced").unwrap().extract().unwrap();
        assert_eq!(advanced, [1000]);
    });
}

#[test]
fn bus_error_ends_the_cycle_and_nothing_waits() {
    with_peripherals(|module| {
        let pending = Pending::default();
        let mut peripheral =
            PythonMapped::mapped(&object(module, "Wide"), pending.clone()).unwrap();
        assert_eq!(peripheral.write(cycle(0, Size::Word), 1), Err(BusError));
        assert!(pending.check().is_ok());
    });
}

#[test]
fn a_value_too_wide_for_the_cycle_waits_as_value_error() {
    with_peripherals(|module| {
        let pending = Pending::default();
        let mut peripheral =
            PythonMapped::mapped(&object(module, "Wide"), pending.clone()).unwrap();
        assert_eq!(peripheral.read(cycle(0, Size::Word)), Ok(0x1234));
        assert_eq!(peripheral.read(cycle(0, Size::Byte)), Ok(UNDRIVEN));
        Python::attach(|py| {
            assert!(
                pending
                    .check()
                    .unwrap_err()
                    .is_instance_of::<PyValueError>(py)
            );
        });
    });
}

#[test]
fn the_first_exception_waits() {
    with_peripherals(|module| {
        let pending = Pending::default();
        let mut peripheral =
            PythonMapped::mapped(&object(module, "Broken"), pending.clone()).unwrap();
        assert_eq!(peripheral.read(cycle(0, Size::Word)), Ok(UNDRIVEN));
        assert!(!peripheral.interrupt_request());
        Python::attach(|py| {
            let error = pending.check().unwrap_err();
            assert!(error.is_instance_of::<PyZeroDivisionError>(py));
        });
        assert!(pending.check().is_ok(), "taken once");
    });
}

#[test]
fn missing_methods_take_the_defaults() {
    with_peripherals(|module| {
        let pending = Pending::default();
        let mut peripheral =
            PythonMapped::mapped(&object(module, "Bare"), pending.clone()).unwrap();
        assert_eq!(peripheral.wait_states(cycle(0, Size::Word)), Some(0));
        assert!(!peripheral.interrupt_request());
        assert_eq!(peripheral.next_event(), None);
        peripheral.reset(Cycles(1));
        peripheral.advance(Cycles(1));
        assert!(pending.check().is_ok());
    });
}

#[test]
fn a_model_needs_no_read_or_write() {
    with_peripherals(|module| {
        let timer = object(module, "Timer");
        let mut model = PythonMapped::unwired(&timer, Pending::default()).unwrap();
        assert_eq!(model.read(cycle(0, Size::Word)), Err(BusError));
        let error = PythonMapped::mapped(&timer, Pending::default()).unwrap_err();
        Python::attach(|py| assert!(error.is_instance_of::<PyTypeError>(py)));
    });
}

/// Entry 0's transfer of `bits` of `out` with `chip_selects` on PCS.
fn qspi_transfer(chip_selects: u8, out: u16, bits: u8) -> QspiTransfer {
    let clock = QspiClock {
        polarity: ClockPolarity::IdleLow,
        phase: ClockPhase::CaptureLeading,
        period: Cycles(8),
        system_clock: Frequency::from_hz(16_777_216),
    };
    QspiTransfer::new(0, chip_selects, out, bits, clock, Cycles(3))
}

#[test]
fn a_qspi_peripheral_shifts_back() {
    with_peripherals(|module| {
        let python = object(module, "Peripheral");
        let pending = Pending::default();
        let mut peripheral = PythonSpiPeripheral::new(&python, pending.clone()).unwrap();
        assert_eq!(peripheral.transfer(qspi_transfer(0b0101, 0x1234, 16)), 0x12);
        peripheral.chip_selects(0b1111, Cycles(9));
        assert!(pending.check().is_ok());
        let levels: Vec<(u8, u64)> = python.getattr("levels").unwrap().extract().unwrap();
        assert_eq!(levels, [(0b1111, 9)]);
        // too wide for the transfer: zeros shift in, the error waits
        assert_eq!(peripheral.transfer(qspi_transfer(0, 0, 8)), 0);
        Python::attach(|py| {
            assert!(
                pending
                    .check()
                    .unwrap_err()
                    .is_instance_of::<PyValueError>(py)
            );
        });
        let error = PythonSpiPeripheral::new(&object(module, "Timer"), pending).unwrap_err();
        Python::attach(|py| assert!(error.is_instance_of::<PyTypeError>(py)));
    });
}

#[test]
fn a_keyboard_interrupt_goes_before_a_waiting_error() {
    Python::initialize();
    Python::attach(|py| {
        let pending = Pending::default();
        pending.hold(PyRuntimeError::new_err("waiting"));
        let raised = pending.after(PyKeyboardInterrupt::new_err(()));
        assert!(raised.is_instance_of::<PyKeyboardInterrupt>(py));
        let context = raised.value(py).getattr("__context__").unwrap();
        assert!(context.is_instance_of::<PyRuntimeError>());
        assert!(pending.check().is_ok(), "taken");
        // held later, it takes the waiting one's place
        pending.hold(PyRuntimeError::new_err("waiting"));
        pending.hold(PyKeyboardInterrupt::new_err(()));
        assert!(
            pending
                .check()
                .unwrap_err()
                .is_instance_of::<PyKeyboardInterrupt>(py)
        );
        // any other error waits behind the first
        pending.hold(PyRuntimeError::new_err("waiting"));
        let raised = pending.after(PyValueError::new_err("later"));
        assert!(raised.is_instance_of::<PyRuntimeError>(py));
    });
}
