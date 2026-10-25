"""Memory at a fixed address on no chip select, emulator-only: through
the Builder and through a board file."""

import struct
from pathlib import Path

import pytest

import mc683xx

ROM_BYTES = 64 << 10
STACK = 0x10FFFE
START = 0x400
# The fixed code and data, where a hybrid boot puts them.
CODE = 0x600000
DATA = 0x6F0000
PAGE = 2048

# Opcodes (CPU32RM 4).
NOP = 0x4E71
JMP_ABSOLUTE = 0x4EF9
MOVE_WORD_IMMEDIATE_TO_ABSOLUTE = 0x33FC
BRANCH_TO_SELF = 0x60FE


def words(program: list[int], size: int, fill: int = 0) -> bytearray:
    image = bytearray(struct.pack(">H", fill) * (size // 2))
    struct.pack_into(f">{len(program)}H", image, 0, *program)
    return image


def rom() -> bytes:
    """The reset vectors and a jump to CODE, with no select programmed."""
    image = words([], ROM_BYTES, NOP)
    struct.pack_into(">II", image, 0, STACK, START)
    struct.pack_into(">3H", image, START, JMP_ABSOLUTE, CODE >> 16, CODE & 0xFFFF)
    return bytes(image)


def code() -> bytes:
    """A word into DATA, then a branch to itself."""
    store = [MOVE_WORD_IMMEDIATE_TO_ABSOLUTE, 0x1234, DATA >> 16, DATA & 0xFFFF]
    return bytes(words(store + [BRANCH_TO_SELF], PAGE))


def test_the_reset_code_reaches_a_fixed_memory_and_a_reset_reloads_it() -> None:
    builder = mc683xx.Builder()
    builder.memory(rom(), ["CSBOOT"], writable=False)
    builder.fixed_memory(CODE, code(), writable=False)
    data = builder.fixed_memory(DATA, bytes([0x5A]) * PAGE)
    builder.reload_scope(data, [mc683xx.ResetCause.EXTERNAL])
    with pytest.raises(IndexError):
        builder.reload_scope(9, [])
    chip = builder.build()
    chip.run_for(1000)
    assert chip.read_memory(data, 0, 2) == b"\x12\x34"
    assert CODE <= chip.read_register("PC") < CODE + PAGE
    assert chip.peek(DATA, 2) == 0x1234
    snapshot = chip.save()
    chip.reset()
    assert chip.read_memory(data, 0, 2) == b"\x5a\x5a"
    chip.restore(snapshot)
    assert chip.read_memory(data, 0, 2) == b"\x12\x34"


def test_fixed_memory_mistakes() -> None:
    builder = mc683xx.Builder()
    builder.fixed_memory(CODE, bytes(2 * PAGE))
    with pytest.raises(ValueError, match="overlaps"):
        builder.fixed_memory(CODE + PAGE, bytes(PAGE))
    with pytest.raises(ValueError, match="does not fit"):
        builder.fixed_memory(CODE + 2, bytes(PAGE))
    with pytest.raises(ValueError, match="does not fit"):
        builder.fixed_memory(0xFFF800, bytes(2 * PAGE))
    with pytest.raises(ValueError, match="pages"):
        builder.fixed_memory(DATA, bytes(100))


def nops_reached(port: int | None, wait_states: int | None) -> int:
    """How far NOPs from CODE get in a few thousand clocks."""
    builder = mc683xx.Builder()
    builder.memory(rom(), ["CSBOOT"], writable=False)
    code = builder.fixed_memory(CODE, bytes(words([], PAGE, NOP)), writable=False)
    if port is not None:
        builder.fixed_timing(code, port=port, wait_states=wait_states)
    chip = builder.build()
    chip.run_for(4000)
    return chip.read_register("PC") - CODE


def test_a_fixed_memory_takes_a_port_and_wait_states() -> None:
    fast = nops_reached(None, None)
    assert nops_reached(16, None) == fast
    waiting = nops_reached(16, 1)
    assert fast > waiting > nops_reached(8, 1)
    builder = mc683xx.Builder()
    code = builder.fixed_memory(CODE, bytes(PAGE))
    with pytest.raises(ValueError, match="8 or 16"):
        builder.fixed_timing(code, port=32)
    with pytest.raises(ValueError, match="0 to 13 wait states"):
        builder.fixed_timing(code, wait_states=14)
    rom_number = builder.memory(rom(), ["CSBOOT"], writable=False)
    with pytest.raises(ValueError, match="not a fixed memory"):
        builder.fixed_timing(rom_number)


BOARD = """
version = 1

[chip]
type = "mc68331"
[[memory]]
name = "rom"
image = "rom.bin"
attach = [{ select = "CSBOOT" }]
[[memory]]
name = "code"
image = "code.bin"
base = 0x600000
[[memory]]
name = "data"
size = 0x800
fill = 0x5a
writable = true
base = 0x6f0000
reload = ["external"]
"""


def test_a_board_files_fixed_memory(tmp_path: Path) -> None:
    (tmp_path / "rom.bin").write_bytes(rom())
    (tmp_path / "code.bin").write_bytes(code())
    (tmp_path / "board.toml").write_text(BOARD)
    board = mc683xx.Board(str(tmp_path / "board.toml"))
    board.run_for(seconds=0.0001)
    assert board.memory("data")[:2] == b"\x12\x34"
    assert CODE <= board.chip.read_register("PC") < CODE + PAGE
    both = BOARD.replace("base = 0x600000", 'base = 0x600000\nattach = [{ select = "CS1" }]')
    (tmp_path / "both.toml").write_text(both)
    with pytest.raises(ValueError, match="not both"):
        mc683xx.Board(str(tmp_path / "both.toml"))
