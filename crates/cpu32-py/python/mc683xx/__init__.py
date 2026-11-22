"""The package around the compiled module, `mc683xx.mc683xx`, which
maturin puts here: its names are the package's (`__init__.pyi` has
them), and the protocols of the host objects the module calls, which
are Python's alone."""

from typing import Protocol

from .mc683xx import *  # noqa: F403
from .mc683xx import BusCycle, QspiTransfer
from .mc683xx import __all__ as _compiled
from .mc683xx import __doc__, __version__  # noqa: F401

__all__ = [*_compiled, "MappedPeripheral", "SpiPeripheral", "Peripheral"]


class MappedPeripheral(Protocol):
    """A bus peripheral, as `Builder.peripheral`, `Builder.bus_peripheral`
    and `Machine.add_peripheral` take it (Rust's `MappedPeripheral` and
    `Timed`): `read` and `write` answer its cycles.  The module also
    calls these, each optional, a missing one taking the trait's
    default: `wait_states(cycle) -> int | None`, `dsack_port(cycle) ->
    int | None` (8, 16 or None), `interrupt_request() -> bool`,
    `reset(now: int)`, `next_event() -> int | None`, `advance(now:
    int)`, and `save() -> bytes` with `restore(snapshot: bytes)`, both
    or neither."""

    def read(self, cycle: BusCycle, /) -> int: ...
    def write(self, cycle: BusCycle, value: int, /) -> object: ...


class SpiPeripheral(Protocol):
    """The QSPI's peripheral, as `Builder.qspi` takes it (Rust's
    `SpiPeripheral`): `transfer` shifts a queue entry's bits.  The
    module also calls these, each optional: `set_chip_selects(levels:
    int, now: int)`, `set_clock(clock: QspiClock, now: int)`,
    `reset(now: int)`, `deferrable() -> bool`, and `save() -> bytes`
    with `restore(snapshot: bytes)`, both or neither."""

    def transfer(self, transfer: QspiTransfer, /) -> int: ...


class Peripheral(Protocol):
    """A board peripheral, as the class `Board`'s `types` names makes it
    (Rust's `Peripheral`), called with its parameters as keyword
    arguments.  Its class declares its ports in `PORTS`, `(name, kind,
    direction)` each, and sets `TIMED_SERIAL` to be told serial frames
    as `Serial`s; every method is optional, a missing one taking the
    trait's default: `read(access, drive) -> int`, `write(access,
    value, drive)`, `wait_states(access) -> int | None`,
    `dsack_port(access) -> int | None`, `wants_horizon() -> bool`,
    `interrupt_request() -> bool`, `power_on(drive)`,
    `reset(drive)`, `transfer(transfer, drive) -> int`,
    `set_qspi_clock(clock, now)`, `select(attachment, selected, drive)`,
    `input(port, value, drive)`, `sample(port, now) -> float | None`,
    `next_event() -> int | None`, `advance(drive)`,
    `qspi_deferrable() -> bool`, `memory_read(memory, access, drive) ->
    int`, `memory_write(memory, access, value, drive)`, `inspect() ->
    Mapping[str, object]`, and `save() -> bytes` with
    `restore(snapshot: bytes)`, both or neither (docs/python.md, Board
    files)."""
