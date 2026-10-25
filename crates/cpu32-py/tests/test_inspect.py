"""What a board's peripherals show a front end (`Board.inspect`): a
stock peripheral's fields and a Python peripheral's own `inspect()`, as
dicts."""

from pathlib import Path

import pytest

import mc683xx

BOARD = """
version = 1

[chip]
type = "mc68331"
[[memory]]
name = "ram"
size = 0x800
attach = [{ select = "CSBOOT" }]
[[peripheral]]
name = "switches"
type = "74x244"
attach = [{ select = "CS1" }]
parameters = { levels = 0xa5 }
[[peripheral]]
name = "panel"
type = "panel"
[[peripheral]]
name = "broken"
type = "broken"
[[peripheral]]
name = "plain"
type = "plain"
[[peripheral]]
name = "huge"
type = "huge"
"""


class Panel:
    """A front end's own peripheral, showing its state."""

    def inspect(self) -> dict[str, object]:
        return {"lamp": True, "count": 3, "volts": 1.5, "name": "panel", "raw": b"\x01", "keys": {"up": False}}


class Broken:
    """An `inspect()` that raises."""

    def inspect(self) -> dict[str, object]:
        raise RuntimeError("cannot show")


class Plain:
    """No `inspect()`."""


class Huge:
    """An `inspect()` with an int too wide for 64 bits."""

    def inspect(self) -> dict[str, object]:
        return {"wide": 1 << 64}


def board(directory: Path) -> mc683xx.Board:
    (directory / "board.toml").write_text(BOARD)
    return mc683xx.Board(
        str(directory / "board.toml"),
        types={"panel": Panel, "broken": Broken, "plain": Plain, "huge": Huge},
    )


def test_a_stock_peripheral_shows_its_fields(tmp_path: Path) -> None:
    assert board(tmp_path).inspect("switches") == {"inputs": 0xA5}


def test_a_python_peripheral_shows_its_own(tmp_path: Path) -> None:
    shown = board(tmp_path)
    assert shown.inspect("panel") == {
        "lamp": True,
        "count": 3,
        "volts": 1.5,
        "name": "panel",
        "raw": b"\x01",
        "keys": {"up": False},
    }
    assert shown.inspect("plain") == {}
    assert shown.inspect("nowhere") is None


def test_an_inspect_s_exception_is_raised(tmp_path: Path) -> None:
    with pytest.raises(RuntimeError, match="cannot show"):
        board(tmp_path).inspect("broken")


def test_an_int_too_wide_for_64_bits_raises(tmp_path: Path) -> None:
    with pytest.raises(OverflowError, match="wide"):
        board(tmp_path).inspect("huge")
