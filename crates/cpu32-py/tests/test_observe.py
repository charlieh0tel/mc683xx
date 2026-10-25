"""Observers on a board's nets from Python (`Board.observe`): what they
see is what the net's peripherals see, an exception one raises stops
the run as a peripheral's does, one stops it without raising through
`Board.stop_request`, and a restore calls each `on_restore`."""

from pathlib import Path

import pytest

import mc683xx

# A 10 MHz clock divided by 1000 onto `pps`, a rise every 100 us from
# the 1000th input edge; `switch` from the host.  A probe on both.
BOARD = """
version = 1

[chip]
type = "mc68331"
[[memory]]
name = "ram"
size = 0x800
attach = [{ select = "CSBOOT" }]
[[peripheral]]
name = "reference"
type = "clock"
parameters = { hz = 10_000_000 }
[[peripheral]]
name = "divider"
type = "divider"
parameters = { divide = 1000 }
[[peripheral]]
name = "probe"
type = "probe"
[[net]]
name = "ten-megahertz"
type = "clock"
from = "reference.out"
to = ["divider.in"]
[[net]]
name = "pps"
type = "edge"
from = "divider.out"
to = ["chip.PGP0", "probe.pps"]
[[net]]
name = "switch"
type = "logic"
from = "external.switch"
to = ["probe.switch"]
"""
# `BOARD` without the probe, which cannot save.
SAVING_BOARD = (
    BOARD.replace('[[peripheral]]\nname = "probe"\ntype = "probe"\n', "")
    .replace(', "probe.pps"', "")
    .replace('["probe.switch"]', '["external.lamp"]')
)
FIRST_RISE_PS = 100_000_000
NETS = ["pps", "switch"]


class Probe:
    """Keeps each change on its inputs."""

    PORTS = [("pps", "edge", "in"), ("switch", "logic", "in")]

    def __init__(self) -> None:
        self.seen: list[tuple[str, int, object]] = []

    def input(self, port: int, value: object, now: int, drive: mc683xx.Drive) -> None:
        self.seen.append((NETS[port], now, value))


def board(directory: Path) -> mc683xx.Board:
    (directory / "board.toml").write_text(BOARD)
    return mc683xx.Board(str(directory / "board.toml"), types={"probe": Probe})


def test_an_observer_sees_what_a_sink_sees(tmp_path: Path) -> None:
    observed = board(tmp_path)
    seen: list[tuple[str, int, object]] = []
    for net in NETS:
        observed.observe(net, lambda net, at, value: seen.append((net, at, value)))
    observed.run_for(seconds=0.0005)
    observed.send("switch", True, kind="logic")
    observed.run_for(seconds=0.0005)
    probe = observed.peripheral("probe")
    assert isinstance(probe, Probe)
    assert len(seen) > 10
    assert seen == probe.seen
    assert ("pps", FIRST_RISE_PS, True) in seen
    assert observed.net_value("switch") is True
    assert observed.nets() == [("ten-megahertz", "clock"), ("pps", "edge"), ("switch", "logic")]


def test_an_observer_s_exception_stops_the_run_at_its_change(tmp_path: Path) -> None:
    observed = board(tmp_path)

    def failing(net: str, at: int, value: object) -> None:
        if value is True:
            raise RuntimeError("seen")

    observed.observe("pps", failing)
    with pytest.raises(RuntimeError, match="seen"):
        observed.run_for(seconds=1.0)
    assert observed.now == FIRST_RISE_PS


def test_an_observer_stops_the_run_at_its_change_without_raising(tmp_path: Path) -> None:
    observed = board(tmp_path)
    stop = observed.stop_request()

    def stopping(net: str, at: int, value: object) -> None:
        if value is True:
            stop.request()

    observed.observe("pps", stopping)
    assert observed.run_for(seconds=1.0).stop == mc683xx.Stop.Requested()
    assert observed.now == FIRST_RISE_PS
    assert observed.run_for(picoseconds=1).stop == mc683xx.Stop.Budget(), "the stop is taken"


def test_a_restore_calls_each_on_restore_with_the_net_s_value(tmp_path: Path) -> None:
    (tmp_path / "board.toml").write_text(SAVING_BOARD)
    observed = mc683xx.Board(str(tmp_path / "board.toml"))
    restored: list[tuple[str, int, object]] = []
    changes: list[str] = []
    for net in ["switch", "pps"]:
        observed.observe(
            net,
            lambda net, at, value: changes.append(net),
            on_restore=lambda net, at, value: restored.append((net, at, value)),
        )
    observed.observe("pps", lambda net, at, value: None)
    observed.run_for(seconds=0.00015)
    snapshot = observed.save()
    at, pps = observed.now, observed.net_value("pps")
    assert pps is not None
    observed.send("switch", True, kind="logic")
    observed.run_for(seconds=0.001)
    changed = len(changes)
    with pytest.raises(mc683xx.StateError):
        observed.restore(b"not a snapshot")
    assert restored == [], "a failed restore calls nothing"
    observed.restore(snapshot)
    assert restored == [("pps", at, pps), ("switch", at, None)], "nets in file order"
    assert len(changes) == changed

    def failing(net: str, at: int, value: object) -> None:
        raise RuntimeError("resync")

    observed.observe("pps", lambda net, at, value: None, on_restore=failing)
    with pytest.raises(RuntimeError, match="resync"):
        observed.restore(snapshot)
    assert observed.now == at, "restored all the same"
    assert observed.run_for(picoseconds=1).stop == mc683xx.Stop.Budget(), "and nothing stops"


def test_an_observer_is_detached(tmp_path: Path) -> None:
    observed = board(tmp_path)
    seen: list[str] = []
    observer = observed.observe("pps", lambda net, at, value: seen.append(net))
    assert observed.unobserve(observer)
    assert not observed.unobserve(observer)
    observed.run_for(seconds=0.001)
    assert seen == []
    with pytest.raises(ValueError, match="no net"):
        observed.observe("nowhere", print)
    with pytest.raises(ValueError, match="no net"):
        observed.net_value("nowhere")
