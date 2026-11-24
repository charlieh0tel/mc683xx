"""A board file from Python: Python classes registered as peripheral types
by name, a peripheral on a select driving a net into another, and the run's
trace."""

from collections.abc import Callable
import struct
from pathlib import Path
from types import MappingProxyType

import pytest

import mc683xx

# The SIM's registers with MM set (MC68331UM D.3); CSBAR1 and CSOR1
# (MC68331UM Table D-3).
SIM = 0xFFFA00
CSBAR1, CSOR1 = SIM + 0x50, SIM + 0x52
# CS1 at $200000, 64 Kbytes; both bytes, read and write, fast
# termination (MC68331UM D.3.25, D.3.26).
CS1_BAR = 0x2003
CSOR_FAST = 0x7BB0
# The same with external DSACK.
CSOR_EXTERNAL = 0x7BF0
LATCH = 0x200000
ROM_BYTES = 64 << 10
STACK = 0x10FFFE
START = 0x400
NOP = 0x4E71
MOVE_WORD_IMMEDIATE_TO_ABSOLUTE = 0x33FC
# Nanohertz in a hertz.
NANOHERTZ = 1_000_000_000


def store_word(address: int, value: int) -> list[int]:
    return [MOVE_WORD_IMMEDIATE_TO_ABSOLUTE, value, address >> 16, address & 0xFFFF]


def rom(program: list[int]) -> bytes:
    image = bytearray(struct.pack(">H", NOP) * (ROM_BYTES // 2))
    image[0:8] = struct.pack(">II", STACK, START)
    for index, word in enumerate(program):
        image[START + 2 * index : START + 2 * index + 2] = struct.pack(">H", word)
    return bytes(image)


BOARD = """
version = 1

[chip]
type = "mc68331"
[[memory]]
name = "rom"
image = "rom.bin"
attach = [{ select = "CSBOOT" }]
[[peripheral]]
name = "latch"
type = "latch"
attach = [{ select = "CS1", role = "write" }]
parameters = { width = 1 }
[[peripheral]]
name = "lamp"
type = "lamp"
marking = "DS1, LED"
[[net]]
name = "lamp-on"
type = "logic"
from = "latch.q0"
to = ["lamp.on"]
"""


class Latch:
    """A write-only latch whose bit 0 drives `q0`."""

    PORTS = [("q0", "logic", "out")]

    def __init__(self, width: int) -> None:
        self.width = width
        self.written: list[int] = []
        self.accesses: list[tuple[str | None, mc683xx.Role, mc683xx.Lane]] = []

    def write(self, access: mc683xx.PeripheralAccess, value: int, drive: mc683xx.Drive) -> None:
        self.written.append(value)
        self.accesses.append((access.select, access.role, access.lane))
        drive.drive(0, bool(value & 1))


class Poker:
    """A write-only peripheral that writes the board's memory `rom` from
    a write's value, as bytes, and tries a list too."""

    PORTS: list[tuple[str, str, str]] = []

    def __init__(self) -> None:
        self.refused = False

    def write(self, access: mc683xx.PeripheralAccess, value: int, drive: mc683xx.Drive) -> None:
        drive.write_memory("rom", 0x10, struct.pack(">H", value))
        try:
            drive.write_memory("rom", 0x12, [1, 2])  # type: ignore[arg-type]
        except TypeError:
            self.refused = True


def test_a_peripheral_writes_a_board_memory_as_bytes(tmp_path: Path) -> None:
    program = store_word(CSBAR1, CS1_BAR) + store_word(CSOR1, CSOR_FAST) + store_word(LATCH, 0x1234)
    (tmp_path / "rom.bin").write_bytes(rom(program))
    (tmp_path / "board.toml").write_text(BOARD.split("[[peripheral]]")[0] + POKER)
    board = mc683xx.Board(str(tmp_path / "board.toml"), types={"poker": Poker})
    board.run_for(seconds=0.0001)
    assert board.memory("rom")[0x10:0x14] == b"\x12\x34" + struct.pack(">H", NOP)
    poker = board.peripheral("poker")
    assert isinstance(poker, Poker)
    assert poker.refused


def test_the_host_slices_and_sets_a_board_memory(tmp_path: Path) -> None:
    (tmp_path / "rom.bin").write_bytes(rom([]))
    (tmp_path / "board.toml").write_text(BOARD)
    board = mc683xx.Board(str(tmp_path / "board.toml"), types={"latch": Latch, "lamp": Lamp})
    size = len(board.memory("rom"))
    assert board.memory("rom", 0, 8) == struct.pack(">II", STACK, START)
    assert board.memory("rom", 4) == board.memory("rom")[4:]
    board.set_memory("rom", 0x10, b"\xab\xcd")
    assert board.memory("rom", 0x10, 2) == b"\xab\xcd"
    assert board.chip.peek_bytes(0x10, 2) == b"\xab\xcd"
    with pytest.raises(IndexError):
        board.memory("rom", size, 1)
    with pytest.raises(IndexError):
        board.set_memory("rom", size - 1, b"\x00\x00")
    with pytest.raises(ValueError):
        board.set_memory("nothing", 0, b"\x00")


class Cells:
    """Bytes a debugger peeks and pokes with no cycle."""

    PORTS: list[tuple[str, str, str]] = []

    def __init__(self) -> None:
        self.held = bytearray(8)

    def peek(self, access: mc683xx.PeripheralAccess, length: int, moment: mc683xx.Moment) -> bytes | None:
        at = access.address % 8
        return bytes(self.held[at : at + length])

    def poke(self, access: mc683xx.PeripheralAccess, data: bytes, moment: mc683xx.Moment) -> bool:
        at = access.address % 8
        self.held[at : at + len(data)] = data
        return True


class Raising(Cells):
    """Cells whose peek fails."""

    def peek(self, access: mc683xx.PeripheralAccess, length: int, moment: mc683xx.Moment) -> bytes | None:
        raise KeyError("no peek")

    def poke(self, access: mc683xx.PeripheralAccess, data: bytes, moment: mc683xx.Moment) -> bool:
        raise KeyError("no poke")


def test_a_board_peripheral_s_peek_exception_is_raised_at_once(tmp_path: Path) -> None:
    program = store_word(CSBAR1, CS1_BAR) + store_word(CSOR1, CSOR_FAST)
    (tmp_path / "rom.bin").write_bytes(rom(program))
    (tmp_path / "board.toml").write_text(BOARD.split("[[peripheral]]")[0] + CELLS)
    board = mc683xx.Board(str(tmp_path / "board.toml"), types={"cells": Raising})
    board.run_for(seconds=0.0001)
    with pytest.raises(KeyError):
        board.chip.peek_bytes(LATCH, 2)
    with pytest.raises(KeyError):
        board.chip.poke_bytes(LATCH, b"\x00")
    # each raised already: no stop is left for the next run
    assert board.run_for(seconds=0.0001).stop == mc683xx.Stop.Budget()


CELLS = """[[peripheral]]
name = "cells"
type = "cells"
attach = [{ select = "CS1" }]
"""


def test_a_board_peripheral_is_peeked_and_poked(tmp_path: Path) -> None:
    program = store_word(CSBAR1, CS1_BAR) + store_word(CSOR1, CSOR_FAST)
    (tmp_path / "rom.bin").write_bytes(rom(program))
    (tmp_path / "board.toml").write_text(BOARD.split("[[peripheral]]")[0] + CELLS)
    board = mc683xx.Board(str(tmp_path / "board.toml"), types={"cells": Cells})
    board.run_for(seconds=0.0001)
    board.chip.poke(LATCH, 2, 0xBEEF)
    assert board.chip.peek(LATCH, 2) == 0xBEEF
    cells = board.peripheral("cells")
    assert isinstance(cells, Cells)
    assert cells.held[:2] == b"\xbe\xef"


POKER = """[[peripheral]]
name = "poker"
type = "poker"
attach = [{ select = "CS1", role = "write" }]
"""


class Lamp:
    """A lamp on a logic input."""

    PORTS = [("on", "logic", "in")]

    def __init__(self) -> None:
        self.changes: list[tuple[int, bool]] = []

    def input(self, port: int, value: object, drive: mc683xx.Drive) -> None:
        assert isinstance(value, bool)
        self.changes.append((drive.now, value))


def test_python_peripherals_on_a_board_file(tmp_path: Path) -> None:
    program = store_word(CSBAR1, CS1_BAR) + store_word(CSOR1, CSOR_FAST) + store_word(LATCH, 1)
    (tmp_path / "rom.bin").write_bytes(rom(program))
    (tmp_path / "board.toml").write_text(BOARD)
    board = mc683xx.Board(str(tmp_path / "board.toml"), types={"latch": Latch, "lamp": Lamp})
    board.set_tracing(bus_cycles=True)
    run = board.run_for(seconds=0.0001)
    assert run.stop == mc683xx.Stop.Budget()
    assert 0 < board.instructions < board.chip.now
    latch = board.peripheral("latch")
    assert isinstance(latch, Latch)
    assert latch.width == 1
    assert latch.written == [1]
    assert latch.accesses == [("CS1", mc683xx.Role.WRITE, mc683xx.Lane.BOTH)]
    lamp = board.peripheral("lamp")
    assert isinstance(lamp, Lamp)
    assert [level for _, level in lamp.changes] == [True]
    assert board.marking("lamp") == "DS1, LED"
    assert board.marking("latch") is None
    trace = board.take_trace()
    assert any(
        line.startswith("mc68331: bus-cycle ")
        and line.endswith("access=write address=0x00200000 size=2 value=0x0001")
        for line in trace
    )
    assert board.memory("rom")[:8] == struct.pack(">II", STACK, START)


def test_ports_are_name_kind_direction(tmp_path: Path) -> None:
    class Reversed(Lamp):
        PORTS = [("on", "in", "logic")]

    (tmp_path / "rom.bin").write_bytes(rom([]))
    (tmp_path / "board.toml").write_text(BOARD)
    with pytest.raises(TypeError, match="^on: "):
        mc683xx.Board(str(tmp_path / "board.toml"), types={"latch": Latch, "lamp": Reversed})


def test_a_watched_pc_is_a_hook_the_chip_removes(tmp_path: Path) -> None:
    (tmp_path / "rom.bin").write_bytes(rom([]))
    (tmp_path / "board.toml").write_text(BOARD)

    def traced(watch: bool, remove: bool) -> list[str]:
        board = mc683xx.Board(str(tmp_path / "board.toml"), types={"latch": Latch, "lamp": Lamp})
        if watch:
            hook = board.watch_code(START)
            assert isinstance(hook, mc683xx.HookId)
            if remove:
                assert board.chip.remove_hook(hook)
        board.run_for(seconds=0.0001)
        return board.take_trace()

    assert len(traced(True, False)) > len(traced(False, False))
    assert traced(True, True) == traced(False, False)


def test_a_board_s_chip_reads_and_hooks_as_a_chip_does(tmp_path: Path) -> None:
    program = store_word(CSBAR1, CS1_BAR) + store_word(CSOR1, CSOR_FAST) + store_word(LATCH, 1)
    (tmp_path / "rom.bin").write_bytes(rom(program))
    (tmp_path / "board.toml").write_text(BOARD)
    board = mc683xx.Board(str(tmp_path / "board.toml"), types={"latch": Latch, "lamp": Lamp})
    chip = board.chip
    assert isinstance(chip, mc683xx.BoardChip)
    seen: list[int] = []
    hook = chip.add_code_hook(lambda instruction: seen.append(instruction.pc), START, START)
    board.run_for(seconds=0.0001)
    assert seen == [START]
    assert chip.remove_hook(hook)
    assert chip.read_register("SSP") <= STACK
    assert chip.registers()["PC"] == chip.read_register("PC")
    assert chip.peek(CSBAR1, 2) == CS1_BAR
    assert chip.halted is None
    assert chip.background is None
    assert chip.instructions == board.instructions
    assert chip.system_hz == 8_388_608
    chip.write_register("D0", 7)
    assert board.chip.read_register("D0") == 7
    assert isinstance(board.take_events(), list)


ANALOG_BOARD = """
version = 1

[chip]
type = "mc68331"
[[memory]]
name = "rom"
image = "rom.bin"
attach = [{ select = "CSBOOT" }]
[[peripheral]]
name = "oven"
type = "oven"
[[peripheral]]
name = "probe"
type = "probe"
[[net]]
name = "temperature"
type = "analog"
from = "oven.out"
to = ["probe.in"]
"""


class Oven:
    """A source whose value moves with time, sampled, never driven."""

    PORTS = [("out", "analog", "out")]

    def analog_output(self, port: int, moment: mc683xx.Moment) -> float | None:
        return 20.0 + moment.now / 1e9

    def save(self) -> bytes:
        return b""

    def restore(self, snapshot: bytes) -> None:
        assert snapshot == b""


def test_a_peripheral_s_calls_run_with_no_board() -> None:
    base = mc683xx.TimeBase(8_388_608)
    assert base == mc683xx.TimeBase(8_388_608, cycle=0, time=0)
    assert base.cycle(1_000_000) == 9  # rounded up
    moved = mc683xx.TimeBase(16_000_000, cycle=100, time=5_000_000)
    assert moved.time(100) == 5_000_000
    assert moved.cycle(5_000_000 + 1_000_000) == 116
    moment = mc683xx.Moment(1_000_000, base)
    assert (moment.now, moment.system_clock, moment.cycle()) == (1_000_000, base, 9)
    assert Oven().analog_output(0, moment) == 20.0 + 1_000_000 / 1e9


class Probe:
    """A sink that samples its input every 10 us and can save."""

    PORTS = [("in", "analog", "in")]
    PERIOD = 10_000_000

    def __init__(self) -> None:
        self.next = self.PERIOD
        self.readings: list[float | None] = []

    def next_event(self, moment: mc683xx.Moment) -> int | None:
        return self.next

    def advance(self, drive: mc683xx.Drive) -> None:
        self.readings.append(drive.sample(0))
        self.next = drive.now + self.PERIOD

    def save(self) -> bytes:
        return struct.pack(">QI", self.next, len(self.readings))

    def restore(self, snapshot: bytes) -> None:
        self.next, count = struct.unpack(">QI", snapshot)
        del self.readings[count:]


class Unsaved:
    """A sink with no snapshot."""

    PORTS = [("in", "analog", "in")]


def test_python_peripherals_sample_and_save(tmp_path: Path) -> None:
    (tmp_path / "rom.bin").write_bytes(rom([]))
    (tmp_path / "board.toml").write_text(ANALOG_BOARD)
    board = mc683xx.Board(str(tmp_path / "board.toml"), types={"oven": Oven, "probe": Probe})
    board.run_for(picoseconds=25_000_000)
    probe = board.peripheral("probe")
    assert isinstance(probe, Probe)
    assert probe.readings == [20.01, 20.02]
    without = mc683xx.Board(str(tmp_path / "board.toml"), types={"oven": Oven, "probe": Unsaved})
    try:
        without.save()
        raise AssertionError("a peripheral without save cannot save")
    except mc683xx.StateError as error:
        assert str(error) == "probe: a Python peripheral cannot save its state", error
    snapshot = board.save()
    board.run_for(picoseconds=20_000_000)
    assert len(probe.readings) == 4
    with pytest.raises(mc683xx.StateError):
        board.restore(b"\x01\x02")
    board.restore(bytearray(snapshot))
    assert len(probe.readings) == 2 and probe.next == 30_000_000
    board.run_for(picoseconds=10_000_000)
    assert probe.readings == [20.01, 20.02, 20.03]


class SaveOnlyProbe:
    """A sink with `save` but no `restore`."""

    PORTS = [("in", "analog", "in")]

    def save(self) -> bytes:
        return b""


class InterruptedProbe(Probe):
    """A probe whose restore is interrupted."""

    def restore(self, snapshot: bytes) -> None:
        raise KeyboardInterrupt


def test_a_python_peripheral_with_save_alone_cannot_save(tmp_path: Path) -> None:
    (tmp_path / "rom.bin").write_bytes(rom([]))
    (tmp_path / "board.toml").write_text(ANALOG_BOARD)
    path = str(tmp_path / "board.toml")
    saving = mc683xx.Board(path, types={"oven": Oven, "probe": Probe})
    board = mc683xx.Board(path, types={"oven": Oven, "probe": SaveOnlyProbe})
    with pytest.raises(mc683xx.StateError, match="^probe: a Python peripheral cannot save"):
        board.save()
    with pytest.raises(mc683xx.StateError, match="^probe: a Python peripheral cannot save"):
        board.restore(saving.save())


def test_a_keyboard_interrupt_in_a_restore_is_raised_and_changes_nothing(
    tmp_path: Path,
) -> None:
    (tmp_path / "rom.bin").write_bytes(rom([]))
    (tmp_path / "board.toml").write_text(ANALOG_BOARD)
    board = mc683xx.Board(
        str(tmp_path / "board.toml"), types={"oven": Oven, "probe": InterruptedProbe}
    )
    board.run_for(picoseconds=25_000_000)
    snapshot = board.save()
    board.run_for(picoseconds=20_000_000)
    before = board.save()
    with pytest.raises(KeyboardInterrupt):
        board.restore(snapshot)
    assert board.save() == before



class LookupInterruptedProbe(Probe):
    """A probe whose `save` lookup is interrupted."""

    @property
    def save(self) -> Callable[[], bytes]:
        raise KeyboardInterrupt


def test_a_keyboard_interrupt_looking_up_save_is_raised(tmp_path: Path) -> None:
    (tmp_path / "rom.bin").write_bytes(rom([]))
    (tmp_path / "board.toml").write_text(ANALOG_BOARD)
    board = mc683xx.Board(
        str(tmp_path / "board.toml"), types={"oven": Oven, "probe": LookupInterruptedProbe}
    )
    with pytest.raises(KeyboardInterrupt):
        board.save()


# The QSM's registers with MM set (MC68331UM Table D-1): PORTQS, PQSPAR,
# SPCR0 to SPCR2, transmit and command RAM (QSMRM 4.2).
QSM = 0xFFFC00
PORTQS, PQSPAR = QSM + 0x15, QSM + 0x16
SPCR0, SPCR1, SPCR2 = QSM + 0x18, QSM + 0x1A, QSM + 0x1C
TRANSMIT_RAM, COMMAND_RAM = QSM + 0x120, QSM + 0x140
MOVE_BYTE_IMMEDIATE_TO_ABSOLUTE = 0x13FC
# The queue's one entry: NEWQP and ENDQP 3 (QSMRM 4.2.4).
ENTRY = 3


def store_byte(address: int, value: int) -> list[int]:
    return [MOVE_BYTE_IMMEDIATE_TO_ABSOLUTE, value, address >> 16, address & 0xFFFF]


# PCS[3:0], MOSI and MISO the QSPI's, PCS[3:0], SCK and MOSI outputs, PCS
# high; entry 3 to PCS 0101, 8 bits, at SPBR 2; then
# SPE (QSMRM 4.3).
QSPI_PROGRAM = (
    store_byte(PORTQS, 0x78)
    + store_word(PQSPAR, 0x7b7e)
    + store_word(TRANSMIT_RAM + 2 * ENTRY, 0x1234)
    + store_byte(COMMAND_RAM + ENTRY, 0b0101)
    + store_word(SPCR2, ENTRY << 8 | ENTRY)
    + store_word(SPCR0, 0x8002)
    + store_word(SPCR1, 0x8000)
)

QSPI_BOARD = """
version = 1

[chip]
type = "mc68331"
[[memory]]
name = "rom"
image = "rom.bin"
attach = [{ select = "CSBOOT" }]
[[peripheral]]
name = "flash"
type = "flash"
attach = [{ qspi = 0b0101 }]
"""


class EntryFlash:
    """A QSPI peripheral that keeps each transfer's queue entry."""

    def __init__(self) -> None:
        self.transfers: list[tuple[int, int, int, int, bool]] = []

    def transfer(self, transfer: mc683xx.Transfer, drive: mc683xx.Drive) -> int:
        self.transfers.append(
            (transfer.attachment, transfer.entry, transfer.out, transfer.bits, transfer.starts_frame)
        )
        return 0xA5


def test_a_qspi_peripheral_is_told_the_entry(tmp_path: Path) -> None:
    (tmp_path / "rom.bin").write_bytes(rom(QSPI_PROGRAM))
    (tmp_path / "board.toml").write_text(QSPI_BOARD)
    board = mc683xx.Board(str(tmp_path / "board.toml"), types={"flash": EntryFlash})
    board.run_for(seconds=0.0001)
    flash = board.peripheral("flash")
    assert isinstance(flash, EntryFlash)
    assert flash.transfers == [(0, ENTRY, 0x34, 8, True)]


# SYNCR (MC68331UM Table D-1): $CF00, 16.777216 MHz.
SYNCR, SYNCR_FAST = 0xFFFA04, 0xCF00
# A wait: MOVEQ #100,D3; DBRA D3,* (the queue's entry ends in it).
WAIT = [0x7000 | 3 << 9 | 100, 0x51CB, 0xFFFE]
# The queue again, after the wait, with CPOL and CPHA set and the
# system clock at 16.777216 MHz (QSMRM 4.3.1).
QSPI_PROGRAM_AGAIN = (
    QSPI_PROGRAM
    + WAIT
    + store_word(SYNCR, SYNCR_FAST)
    + store_word(SPCR0, 0x8302)
    + store_word(SPCR1, 0x8000)
)


class ClockedFlash:
    """A QSPI peripheral that keeps the clocks it is told and each transfer's."""

    def __init__(self) -> None:
        self.told: list[tuple[mc683xx.ClockPolarity, mc683xx.ClockPhase, int]] = []
        self.transfers: list[tuple[mc683xx.ClockPolarity, mc683xx.ClockPhase, int]] = []
        self.moments: list[mc683xx.Moment] = []
        self.drive_hertz: list[int] = []

    def set_qspi_clock(self, clock: mc683xx.QspiClock, moment: mc683xx.Moment) -> None:
        self.told.append((clock.polarity, clock.phase, clock.system_clock_nanohertz))
        self.moments.append(moment)

    def transfer(self, transfer: mc683xx.Transfer, drive: mc683xx.Drive) -> int:
        clock = transfer.clock
        self.transfers.append((clock.polarity, clock.phase, clock.system_clock_nanohertz))
        self.drive_hertz.append(drive.system_clock.frequency_hz)
        return 0


def test_a_qspi_peripheral_is_told_the_clock_and_each_transfer_carries_it(tmp_path: Path) -> None:
    (tmp_path / "rom.bin").write_bytes(rom(QSPI_PROGRAM_AGAIN))
    (tmp_path / "board.toml").write_text(QSPI_BOARD)
    board = mc683xx.Board(str(tmp_path / "board.toml"), types={"flash": ClockedFlash})
    board.run_for(seconds=0.001)
    flash = board.peripheral("flash")
    assert isinstance(flash, ClockedFlash)
    expected = [
        (mc683xx.ClockPolarity.IDLE_LOW, mc683xx.ClockPhase.CAPTURE_LEADING, 8_388_608 * NANOHERTZ),
        (mc683xx.ClockPolarity.IDLE_HIGH, mc683xx.ClockPhase.CHANGE_LEADING, 16_777_216 * NANOHERTZ),
    ]
    assert flash.told == expected
    assert flash.transfers == expected
    # each told at its moment, on the system clock of then
    hertz = [moment.system_clock.frequency_hz for moment in flash.moments]
    assert hertz == [8_388_608, 16_777_216]
    assert flash.drive_hertz == hertz
    for moment in flash.moments:
        clock = moment.system_clock
        assert moment.cycle() == clock.cycle(moment.now)
        assert clock.time(moment.cycle()) >= moment.now
        assert clock.cycle(clock.time(moment.cycle())) == moment.cycle()


class TimedFlash:
    """A QSPI peripheral that keeps each transfer's time and says whether its
    calls may come late."""

    DEFERRABLE = False

    def __init__(self) -> None:
        self.transfers: list[tuple[int, int]] = []
        self.asked = 0

    def qspi_deferrable(self) -> bool:
        self.asked += 1
        return self.DEFERRABLE

    def transfer(self, transfer: mc683xx.Transfer, drive: mc683xx.Drive) -> int:
        self.transfers.append((transfer.out, drive.now))
        return 0xA5


class DeferrableFlash(TimedFlash):
    DEFERRABLE = True


def test_a_deferrable_qspi_peripheral_sees_the_same_transfers(tmp_path: Path) -> None:
    (tmp_path / "rom.bin").write_bytes(rom(QSPI_PROGRAM))
    (tmp_path / "board.toml").write_text(QSPI_BOARD)
    seen = []
    for kind in (TimedFlash, DeferrableFlash):
        board = mc683xx.Board(str(tmp_path / "board.toml"), types={"flash": kind})
        board.run_for(seconds=0.0001)
        flash = board.peripheral("flash")
        assert isinstance(flash, kind)
        assert flash.asked == 1, "asked once, as the board is built"
        seen.append(flash.transfers)
    assert seen[0] == seen[1]
    assert [out for out, _ in seen[0]] == [0x34]


SERIAL_BOARD = """
version = 1

[chip]
type = "mc68331"
[[memory]]
name = "rom"
image = "rom.bin"
attach = [{ select = "CSBOOT" }]
[[peripheral]]
name = "modem"
type = "modem"
[[net]]
name = "to-modem"
type = "serial"
from = "external.host"
to = ["modem.rx"]
[[net]]
name = "from-modem"
type = "serial"
from = "modem.tx"
to = ["external.echo"]
"""


class Modem:
    """Echoes each frame, its timing kept, and records what it got."""

    PORTS = [("rx", "serial", "in"), ("tx", "serial", "out")]

    def __init__(self) -> None:
        self.received: list[object] = []

    def input(self, port: int, value: object, drive: mc683xx.Drive) -> None:
        self.received.append(value)
        drive.drive(1, value)


def test_serial_values_carry_their_timing(tmp_path: Path) -> None:
    (tmp_path / "rom.bin").write_bytes(rom([]))
    (tmp_path / "board.toml").write_text(SERIAL_BOARD)
    baud = 9_600 * 10**9
    frame = mc683xx.Serial(mc683xx.Frame.Data(0x41), baud, 10)
    assert (frame.frame, frame.baud_nanohertz, frame.length) == (mc683xx.Frame.Data(0x41), baud, 1_041_666_666)
    assert frame == mc683xx.Serial.timed(mc683xx.Frame.Data(0x41), baud, 1_041_666_666)
    assert frame.end(5) == 5 + frame.length
    assert mc683xx.Serial.untimed(mc683xx.Frame.Break()).length == 0
    board = mc683xx.Board(str(tmp_path / "board.toml"), types={"modem": Modem})
    echo, host = board.externals()
    assert (echo.name, echo.kind, echo.direction) == ("echo", "serial", "in")
    assert (host.name, host.kind, host.direction) == ("host", "serial", "out")
    assert repr(host) == 'External(name="host", kind="serial", direction="out")'
    assert host != "host" and host == board.externals()[1] and host in {host}
    board.check_send("host", frame)
    board.send("host", frame)
    board.send("host", 0x42)
    for send in (board.send, board.check_send):
        with pytest.raises(ValueError, match="external.echo drives no net"):
            send("echo", 0x42)
        with pytest.raises(ValueError, match="drives serial nets only"):
            send("host", True, kind="logic")
    board.run_for(picoseconds=1_000_000)
    modem = board.peripheral("modem")
    assert isinstance(modem, Modem)
    assert modem.received == [frame, mc683xx.Serial.untimed(mc683xx.Frame.Data(0x42))]
    received = board.take_received("echo")
    assert [value for _, value in received] == modem.received
    board.send("host", None)
    board.run_for(picoseconds=1_000_000)
    assert modem.received[-1] == mc683xx.Serial.untimed(mc683xx.Frame.Break())
    with pytest.raises(ValueError, match="no net drives external.host"):
        board.take_received("host")


def test_a_run_sets_parameters_over_the_files(tmp_path: Path) -> None:
    (tmp_path / "rom.bin").write_bytes(rom([]))
    (tmp_path / "board.toml").write_text(BOARD)
    types = {"latch": Latch, "lamp": Lamp}
    path = str(tmp_path / "board.toml")
    board = mc683xx.Board(path, types=types, parameters={"latch": {"width": 3}})
    latch = board.peripheral("latch")
    assert isinstance(latch, Latch)
    assert latch.width == 3
    try:
        mc683xx.Board(path, types=types, parameters={"lump": {"width": 3}})
        raise AssertionError("no peripheral lump")
    except ValueError as error:
        assert "lump" in str(error)


class Failing:
    """A peripheral whose questions, asked with no `Drive`, raise."""

    PORTS: list[tuple[str, str, str]] = []

    def __init__(self, **parameters: object) -> None:
        self.calls = 0

    def interrupt_request(self) -> bool:
        self.calls += 1
        raise KeyError("no interrupt")

    def next_event(self, moment: mc683xx.Moment) -> int | None:
        self.calls += 1
        raise KeyError("no next event")


FAILING_BOARD = """
version = 1

[chip]
type = "mc68331"
[[memory]]
name = "rom"
image = "rom.bin"
attach = [{ select = "CSBOOT" }]
[[peripheral]]
name = "failing"
type = "failing"
"""


def test_a_peripheral_s_failed_question_stops_the_run_at_once(tmp_path: Path) -> None:
    (tmp_path / "rom.bin").write_bytes(rom([]))
    (tmp_path / "board.toml").write_text(FAILING_BOARD)
    board = mc683xx.Board(str(tmp_path / "board.toml"), types={"failing": Failing})
    peripheral = board.peripheral("failing")
    assert isinstance(peripheral, Failing)
    start, calls = board.now, peripheral.calls
    try:
        board.run_for(seconds=1.0)
        raise AssertionError("run should raise")
    except KeyError:
        pass
    # Stopped within a microsecond of board time, a few questions in.
    assert board.now - start < 1_000_000
    assert peripheral.calls - calls < 100


@pytest.mark.parametrize("seconds", [float("nan"), float("inf"), float("-inf"), -1e-6, 1e300, 2e7])
def test_a_run_refuses_seconds_not_finite_negative_or_out_of_range(tmp_path: Path, seconds: float) -> None:
    (tmp_path / "rom.bin").write_bytes(rom([]))
    (tmp_path / "board.toml").write_text(BOARD)
    board = mc683xx.Board(str(tmp_path / "board.toml"), types={"latch": Latch, "lamp": Lamp})
    with pytest.raises(ValueError, match="finite number of seconds"):
        board.run_for(seconds=seconds)
    assert board.now == 0
    assert board.run_for(seconds=0.0).stop == mc683xx.Stop.Budget()


def test_run_until_runs_to_a_time_and_run_for_for_a_span(tmp_path: Path) -> None:
    (tmp_path / "rom.bin").write_bytes(rom([]))
    (tmp_path / "board.toml").write_text(BOARD)
    board = mc683xx.Board(str(tmp_path / "board.toml"), types={"latch": Latch, "lamp": Lamp})
    run = board.run_until(picoseconds=3_000_000)
    assert run.stop == mc683xx.Stop.Budget()
    assert run.now == 3_000_000
    assert board.now == 3_000_000
    board.run_for(picoseconds=2_000_000)
    assert board.now == 5_000_000
    # a time already passed: at once
    cycles = board.chip.now
    assert board.run_until(picoseconds=1_000_000).now == 5_000_000
    assert board.chip.now == cycles
    assert board.now == 5_000_000


def test_the_options_are_any_mappings(tmp_path: Path) -> None:
    (tmp_path / "rom.bin").write_bytes(rom([]))
    (tmp_path / "board.toml").write_text(BOARD)
    board = mc683xx.Board(
        str(tmp_path / "board.toml"),
        types=MappingProxyType({"latch": Latch, "lamp": Lamp}),
        images=MappingProxyType({}),
        parameters=MappingProxyType({"latch": MappingProxyType({"width": 2})}),
    )
    latch = board.peripheral("latch")
    assert isinstance(latch, Latch)
    assert latch.width == 2


def test_enabled_turns_a_file_s_options_on(tmp_path: Path) -> None:
    (tmp_path / "rom.bin").write_bytes(rom([]))
    optional = BOARD.replace("version = 1\n", 'version = 1\n[board]\noptions = ["spare"]\n')
    optional += '[[peripheral]]\nname = "spare"\ntype = "lamp"\noption = "spare"\n'
    (tmp_path / "board.toml").write_text(optional)
    path = str(tmp_path / "board.toml")
    types = {"latch": Latch, "lamp": Lamp}
    with pytest.raises(ValueError, match="spare"):
        mc683xx.Board(path, types=types).peripheral("spare")
    assert isinstance(mc683xx.Board(path, types=types, enabled=["spare"]).peripheral("spare"), Lamp)


def test_a_board_of_another_chip_is_refused(tmp_path: Path) -> None:
    (tmp_path / "rom.bin").write_bytes(rom([]))
    (tmp_path / "board.toml").write_text(BOARD.replace('type = "mc68331"', 'type = "mc68332"'))
    with pytest.raises(mc683xx.LoadError, match="chip type \"mc68332\""):
        mc683xx.Board(str(tmp_path / "board.toml"), types={"latch": Latch, "lamp": Lamp})


def test_a_refused_board_file_raises_load_error(tmp_path: Path) -> None:
    (tmp_path / "board.toml").write_text("version = 2\n")
    with pytest.raises(mc683xx.LoadError, match="board file version 2") as raised:
        mc683xx.Board(str(tmp_path / "board.toml"))
    assert isinstance(raised.value, ValueError)


def test_an_unread_board_file_raises_the_os_error_its_errno_gives(tmp_path: Path) -> None:
    missing = tmp_path / "missing.toml"
    with pytest.raises(FileNotFoundError) as raised:
        mc683xx.Board(str(missing))
    assert raised.value.filename == str(missing)
    assert not isinstance(raised.value, mc683xx.LoadError)


def test_an_unread_image_raises_the_os_error_its_errno_gives(tmp_path: Path) -> None:
    (tmp_path / "board.toml").write_text(BOARD)
    with pytest.raises(FileNotFoundError) as raised:
        mc683xx.Board(str(tmp_path / "board.toml"), types={"latch": Latch, "lamp": Lamp})
    assert raised.value.filename == str(tmp_path / "rom.bin")
    assert not isinstance(raised.value, mc683xx.LoadError)


class Stuck:
    """A peripheral whose next event never passes: it stalls the board."""

    PORTS: list[tuple[str, str, str]] = []

    def next_event(self, moment: mc683xx.Moment) -> int:
        return 1


def test_a_stalled_board_raises_run_error(tmp_path: Path) -> None:
    (tmp_path / "rom.bin").write_bytes(rom([]))
    (tmp_path / "board.toml").write_text(
        BOARD + '[[peripheral]]\nname = "stuck"\ntype = "stuck"\n'
    )
    board = mc683xx.Board(
        str(tmp_path / "board.toml"), types={"latch": Latch, "lamp": Lamp, "stuck": Stuck}
    )
    with pytest.raises(mc683xx.RunError, match="^stuck: advanced to") as raised:
        board.run_for(seconds=0.001)
    assert isinstance(raised.value, RuntimeError)


INTERRUPTED_BOARD = """
version = 1

[chip]
type = "mc68331"
[[memory]]
name = "rom"
image = "rom.bin"
attach = [{ select = "CSBOOT" }]
[[peripheral]]
name = "register"
type = "interrupted"
attach = [{ select = "CS1" }]
"""
MOVE_WORD_ABSOLUTE_TO_D0 = 0x3039
BRA_BACK_8 = 0x60F8


class Interrupted:
    """Reads 0x1234, but its first read is interrupted by Ctrl-C."""

    PORTS: list[tuple[str, str, str]] = []

    def __init__(self) -> None:
        self.reads = 0

    def read(self, access: mc683xx.PeripheralAccess, drive: mc683xx.Drive) -> int:
        self.reads += 1
        if self.reads == 1:
            raise KeyboardInterrupt
        return 0x1234


def test_a_keyboard_interrupt_in_a_read_stops_the_run_without_a_bus_error(tmp_path: Path) -> None:
    # reads the register in a loop; a bus error would find no handler
    # (the vector a NOP pair, odd) and halt the CPU
    program = store_word(CSBAR1, CS1_BAR) + store_word(CSOR1, CSOR_FAST)
    program += [MOVE_WORD_ABSOLUTE_TO_D0, LATCH >> 16, LATCH & 0xFFFF, BRA_BACK_8]
    (tmp_path / "rom.bin").write_bytes(rom(program))
    (tmp_path / "board.toml").write_text(INTERRUPTED_BOARD)
    board = mc683xx.Board(str(tmp_path / "board.toml"), types={"interrupted": Interrupted})
    with pytest.raises(KeyboardInterrupt):
        board.run_for(seconds=1.0)
    # the read answered undriven, 0
    assert board.chip.read_register("D0") & 0xFFFF == 0
    assert board.chip.halted is None
    assert board.run_for(seconds=0.001).stop == mc683xx.Stop.Budget()
    assert board.chip.read_register("D0") & 0xFFFF == 0x1234


class Slow:
    """Reads 0x1234 after more wait states than a byte holds."""

    PORTS: list[tuple[str, str, str]] = []

    def read(self, access: mc683xx.PeripheralAccess, drive: mc683xx.Drive) -> int:
        return 0x1234

    def wait_states(self, access: mc683xx.PeripheralAccess, moment: mc683xx.Moment) -> int:
        return 300


def test_a_python_peripheral_waits_more_than_255_wait_states(tmp_path: Path) -> None:
    # CS1 with external DSACK (MC68331UM D.3.26, DSACK %1111)
    program = store_word(CSBAR1, CS1_BAR) + store_word(CSOR1, CSOR_EXTERNAL)
    program += [MOVE_WORD_ABSOLUTE_TO_D0, LATCH >> 16, LATCH & 0xFFFF, BRA_BACK_8]
    (tmp_path / "rom.bin").write_bytes(rom(program))
    (tmp_path / "board.toml").write_text(INTERRUPTED_BOARD.replace('"interrupted"', '"slow"'))
    board = mc683xx.Board(str(tmp_path / "board.toml"), types={"slow": Slow})
    board.run_for(seconds=0.001)
    assert board.chip.read_register("D0") & 0xFFFF == 0x1234


class Horizoned:
    """Keeps each read's time and horizon; asks for horizons if `wants`."""

    PORTS: list[tuple[str, str, str]] = []

    def __init__(self, wants: bool) -> None:
        self.wants = wants
        self.reads: list[tuple[int, int | None]] = []

    def wants_horizon(self) -> bool:
        return self.wants

    def read(self, access: mc683xx.PeripheralAccess, drive: mc683xx.Drive) -> int:
        self.reads.append((drive.now, access.horizon))
        return 0


@pytest.mark.parametrize("wants", [True, False])
def test_a_python_peripheral_asks_for_its_horizon(tmp_path: Path, wants: bool) -> None:
    program = store_word(CSBAR1, CS1_BAR) + store_word(CSOR1, CSOR_FAST)
    program += [MOVE_WORD_ABSOLUTE_TO_D0, LATCH >> 16, LATCH & 0xFFFF, BRA_BACK_8]
    (tmp_path / "rom.bin").write_bytes(rom(program))
    parameters = f"parameters = {{ wants = {str(wants).lower()} }}"
    board_file = INTERRUPTED_BOARD.replace('type = "interrupted"', 'type = "horizoned"')
    board_file = board_file.replace(
        'attach = [{ select = "CS1" }]', f'attach = [{{ select = "CS1" }}]\n{parameters}'
    )
    (tmp_path / "board.toml").write_text(board_file)
    board = mc683xx.Board(str(tmp_path / "board.toml"), types={"horizoned": Horizoned})
    end = 1_000_000_000
    board.run_for(picoseconds=end)
    peripheral = board.peripheral("register")
    assert isinstance(peripheral, Horizoned)
    assert peripheral.reads
    # nothing else on the board: the run's end
    expected = end if wants else None
    assert all(horizon == expected for _, horizon in peripheral.reads), peripheral.reads[:3]


class LookupInterrupted(Interrupted):
    """Reads 0x1234, but looking up its first read is interrupted."""

    @property
    def read(self) -> Callable[[mc683xx.PeripheralAccess, mc683xx.Drive], int]:
        self.reads += 1
        if self.reads == 1:
            raise KeyboardInterrupt
        return lambda access, drive: 0x1234


def test_a_keyboard_interrupt_looking_up_read_stops_the_run_without_a_bus_error(
    tmp_path: Path,
) -> None:
    program = store_word(CSBAR1, CS1_BAR) + store_word(CSOR1, CSOR_FAST)
    program += [MOVE_WORD_ABSOLUTE_TO_D0, LATCH >> 16, LATCH & 0xFFFF, BRA_BACK_8]
    (tmp_path / "rom.bin").write_bytes(rom(program))
    (tmp_path / "board.toml").write_text(INTERRUPTED_BOARD)
    board = mc683xx.Board(str(tmp_path / "board.toml"), types={"interrupted": LookupInterrupted})
    with pytest.raises(KeyboardInterrupt):
        board.run_for(seconds=1.0)
    assert board.chip.read_register("D0") & 0xFFFF == 0
    assert board.chip.halted is None
    assert board.run_for(seconds=0.001).stop == mc683xx.Stop.Budget()
    assert board.chip.read_register("D0") & 0xFFFF == 0x1234


SWITCHED_BOARD = """
version = 1

[chip]
type = "mc68331"
[[memory]]
name = "ram"
size = 0x800
attach = [{ select = "CSBOOT" }]
[[peripheral]]
name = "failing"
type = "failing"
[[peripheral]]
name = "interrupted"
type = "interrupted"
[[net]]
name = "switch"
type = "logic"
from = "external.switch"
to = ["failing.in", "interrupted.in"]
"""


class FailingInput:
    """Raises `RuntimeError` at its input."""

    PORTS = [("in", "logic", "in")]

    def input(self, port: int, value: object, drive: mc683xx.Drive) -> None:
        raise RuntimeError("failing")


class InterruptedInput:
    """Interrupted by Ctrl-C at its input."""

    PORTS = [("in", "logic", "in")]

    def input(self, port: int, value: object, drive: mc683xx.Drive) -> None:
        raise KeyboardInterrupt


def test_a_keyboard_interrupt_is_raised_before_an_earlier_peripheral_error(tmp_path: Path) -> None:
    (tmp_path / "board.toml").write_text(SWITCHED_BOARD)
    board = mc683xx.Board(
        str(tmp_path / "board.toml"),
        types={"failing": FailingInput, "interrupted": InterruptedInput},
    )
    board.control().send("switch", True, kind="logic")
    with pytest.raises(KeyboardInterrupt) as raised:
        board.run_for(seconds=0.001)
    assert isinstance(raised.value.__context__, RuntimeError)
    assert board.run_for(seconds=0.001).stop == mc683xx.Stop.Budget()


RESET_BOARD = """
version = 1

[chip]
type = "mc68331"
[[memory]]
name = "rom"
image = "rom.bin"
attach = [{ select = "CSBOOT" }]
[[peripheral]]
name = "counter"
type = "counter"
reset = ["watchdog"]
"""
# RSR, $YFFA07 (MC68331UM D.3.4); its SW bit, the watchdog's, and EXT,
# RESET's (SIMRM 8.2.8).
RSR = SIM + 0x07
RSR_SW = 0x20
RSR_EXT = 0x80
# ADDQ.L #1,D1; BRA.S to itself: counts its starts in D1, which a reset
# keeps, the watchdog on as reset leaves it.
COUNTING = [0x5281, 0x60FE]


class Counter:
    """Counts the resets that reach it."""

    PORTS: list[tuple[str, str, str]] = []

    def __init__(self) -> None:
        self.resets = 0

    def reset(self, drive: mc683xx.Drive) -> None:
        self.resets += 1


def test_a_board_resets_for_a_cause_as_its_source_does(tmp_path: Path) -> None:
    (tmp_path / "rom.bin").write_bytes(rom(COUNTING))
    (tmp_path / "board.toml").write_text(RESET_BOARD)
    board = mc683xx.Board(str(tmp_path / "board.toml"), types={"counter": Counter})
    board.run_for(seconds=0.001)
    board.take_events()
    board.reset_for(mc683xx.ResetCause.WATCHDOG)
    events = board.take_events()
    assert [
        event.cause for event in events if isinstance(event, mc683xx.Event.Reset)
    ] == [mc683xx.ResetCause.WATCHDOG]
    assert len(events) == 1
    assert board.chip.peek(RSR, 1) == RSR_SW
    counter = board.peripheral("counter")
    assert isinstance(counter, Counter)
    assert counter.resets == 1, "the watchdog's alone"
    board.run_for(seconds=0.001)
    assert board.chip.read_register("D1") == 2, "started again"
    board.take_events()
    board.reset()
    assert [
        event.cause for event in board.take_events() if isinstance(event, mc683xx.Event.Reset)
    ] == [mc683xx.ResetCause.EXTERNAL] * 2, "RESET asserted and released"
    assert board.chip.peek(RSR, 1) == RSR_EXT


BKPT_BOARD = """
version = 1

[chip]
type = "mc68331"
[[memory]]
name = "rom"
image = "rom.bin"
attach = [{ select = "CSBOOT" }]
[[memory]]
name = "ram"
size = 0x800
writable = true
attach = [{ select = "CS0" }]
[[peripheral]]
name = "counter"
type = "counter"
[[net]]
name = "reset"
type = "wired-and"
from = ["chip.RESET", "external.reset"]
to = ["chip.RESET", "external.observed"]
"""
# CSBAR0 and CSOR0 (MC68331UM Table D-3): CS0 at $100000, 64 Kbytes,
# holding the stack, its 2 Kbytes of RAM repeating.
CSBAR0, CSOR0 = SIM + 0x4C, SIM + 0x4E
STACK_BAR = 0x1003
# BRA.S to itself.
BRANCH_TO_SELF = 0x60FE
# The hardware breakpoint's vector (CPU32RM Table 6-1).
HARDWARE_BREAKPOINT = 12


def bkpt_board(tmp_path: Path, program: list[int]) -> mc683xx.Board:
    (tmp_path / "rom.bin").write_bytes(rom(program))
    (tmp_path / "board.toml").write_text(BKPT_BOARD)
    return mc683xx.Board(str(tmp_path / "board.toml"), types={"counter": Counter})


def test_a_board_reset_with_breakpoint_enters_background_mode(tmp_path: Path) -> None:
    board = bkpt_board(tmp_path, COUNTING)
    board.run_for(seconds=0.001)
    board.take_received("observed")
    counter = board.peripheral("counter")
    assert isinstance(counter, Counter)
    resets = counter.resets
    board.reset_with_breakpoint()
    assert board.chip.background == mc683xx.Entry.Breakpoint()
    assert board.chip.read_register("PC") == START
    assert counter.resets > resets, "the peripherals reset"
    instructions = board.instructions
    run = board.run_for(seconds=0.001)
    assert run.stop == mc683xx.Stop.Background(mc683xx.Entry.Breakpoint())
    assert board.instructions == instructions
    assert board.chip.read_register("D1") == 1, "not started again"
    # the RESET net pulsed by the chip, as for any reset it takes
    assert [level for _, level in board.take_received("observed")] == [False, True]
    board.go()
    board.run_for(seconds=0.001)
    assert board.chip.read_register("D1") == 2, "started again"


def test_a_board_halts_and_goes_on(tmp_path: Path) -> None:
    board = bkpt_board(tmp_path, COUNTING)
    with pytest.raises(mc683xx.TargetError, match="not enabled"):
        board.halt()
    with pytest.raises(mc683xx.TargetError, match="not in background mode"):
        board.go()
    board.reset_with_breakpoint()
    board.go()
    board.run_for(seconds=0.001)
    assert board.halt() == mc683xx.Entry.Breakpoint()
    instructions = board.instructions
    run = board.run_for(seconds=0.001)
    assert run.stop == mc683xx.Stop.Background(mc683xx.Entry.Breakpoint())
    assert board.instructions == instructions
    board.go()
    assert board.run_for(seconds=0.001).stop == mc683xx.Stop.Budget()
    assert board.instructions > instructions


def test_bkpt_on_a_board_without_background_mode_takes_vector_12(tmp_path: Path) -> None:
    program = store_word(CSBAR0, STACK_BAR) + store_word(CSOR0, CSOR_FAST) + [BRANCH_TO_SELF]
    board = bkpt_board(tmp_path, program)
    board.run_for(seconds=0.001)
    board.set_tracing(exceptions=True)
    assert board.assert_breakpoint() is False
    board.run_for(seconds=0.001)
    exceptions = [line for line in board.take_trace() if " exception " in line]
    assert f"vector={HARDWARE_BREAKPOINT} " in exceptions[0]
    board.reset_with_breakpoint()
    board.go()
    assert board.assert_breakpoint() is True
    assert board.chip.background == mc683xx.Entry.Breakpoint()
