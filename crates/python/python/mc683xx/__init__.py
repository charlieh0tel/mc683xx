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
    int)`, `peek(cycle) -> int | None` and `poke(cycle, value) -> bool`
    (a debugger's byte with no cycle's side effects; `None` and `False`
    decline, as missing ones do), and `save() -> bytes` with
    `restore(snapshot: bytes)`, both or neither."""

    def read(self, cycle: BusCycle, /) -> int: ...
    def write(self, cycle: BusCycle, value: int, /) -> object: ...


class SpiPeripheral(Protocol):
    """The QSPI's peripheral, as `Builder.set_qspi` takes it (Rust's
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
    direction)` each, and is told serial frames as `Serial`s; every
    method is optional, a missing one taking the
    trait's default: `read(access, drive) -> int`, `write(access,
    value, drive)`, `wait_states(access, moment) -> int | None`,
    `dsack_port(access, moment) -> int | None`, `wants_horizon() -> bool`,
    `interrupt_request() -> bool`, `power_on(drive)`,
    `reset(drive)`, `transfer(transfer, drive) -> int`,
    `set_qspi_clock(clock, moment)`, `peek(access, moment) -> int |
    None`, `poke(access, value, moment) -> bool`, `set_selected(attachment, selected,
    drive)`, `input(port, value, drive)`, `analog_output(port, moment) ->
    float | None`, `next_event(moment) -> int | None`, `advance(drive)`,
    `qspi_deferrable() -> bool`, `memory_read(memory, access, drive) ->
    int`, `memory_write(memory, access, value, drive)`, `inspect() ->
    Mapping[str, object]`, and `save() -> bytes` with
    `restore(snapshot: bytes)`, both or neither (docs/python.md, Board
    files)."""
