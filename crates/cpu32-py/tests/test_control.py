"""A board's control handle (`Board.control`) and paced runs from
Python: another thread stops a paced run and queues input, taken at a
sync point, and pacing changes nothing the board does."""

import threading
import time
from pathlib import Path

import pytest

import mc683xx

# A 10 MHz clock divided by 1000 onto `tick`, to PGP0, and `switch` from
# the host to PGP1.
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
[[net]]
name = "ten-megahertz"
type = "clock"
from = "reference.out"
to = ["divider.in"]
[[net]]
name = "tick"
type = "edge"
from = "divider.out"
to = ["chip.PGP0"]
[[net]]
name = "switch"
type = "logic"
from = "external.switch"
to = ["chip.PGP1"]
"""
QUANTUM_PS = 5_000_000_000


def board(directory: Path) -> mc683xx.Board:
    (directory / "board.toml").write_text(BOARD)
    return mc683xx.Board(str(directory / "board.toml"))


def observed(loaded: mc683xx.Board) -> list[tuple[str, int, object]]:
    seen: list[tuple[str, int, object]] = []
    for net in ["tick", "switch"]:
        loaded.observe(net, lambda net, at, value: seen.append((net, at, value)))
    return seen


def test_pacing_names_and_factors() -> None:
    assert mc683xx.Pacing.UNPACED.factor is None
    assert mc683xx.Pacing.REAL_TIME.factor == 1.0
    assert mc683xx.Pacing.scaled(2.0).factor == 2.0
    assert mc683xx.Pacing("realtime") == mc683xx.Pacing.REAL_TIME
    assert mc683xx.Pacing("0.5") == mc683xx.Pacing.scaled(0.5)
    assert str(mc683xx.Pacing.REAL_TIME) == "realtime"
    for factor in [0.0, -1.0, float("inf"), float("nan")]:
        with pytest.raises(ValueError, match="pacing factor"):
            mc683xx.Pacing.scaled(factor)
    with pytest.raises(ValueError, match="no pacing"):
        mc683xx.Pacing("fast")


def test_a_pacing_hashes_as_it_compares() -> None:
    assert hash(mc683xx.Pacing("realtime")) == hash(mc683xx.Pacing.REAL_TIME)
    assert hash(mc683xx.Pacing("0.5")) == hash(mc683xx.Pacing.scaled(0.5))
    pacings = {mc683xx.Pacing.UNPACED, mc683xx.Pacing.REAL_TIME, mc683xx.Pacing.scaled(1.0), mc683xx.Pacing.scaled(0.5)}
    assert len(pacings) == 4
    assert mc683xx.Pacing("0.5") in pacings


def test_inputs_queued_between_runs_are_driven_at_the_next_runs_start(tmp_path: Path) -> None:
    loaded = board(tmp_path)
    seen = observed(loaded)
    control = loaded.control()
    control.send("switch", False, kind="logic")
    control.send("switch", True, kind="logic")
    with pytest.raises(ValueError, match="drives no net"):
        control.send("lamp", True, kind="logic")
    loaded.run_for(seconds=0.001)
    assert [change for change in seen if change[0] == "switch"] == [
        ("switch", 0, False),
        ("switch", 0, True),
    ]


def test_a_direct_send_lands_after_inputs_queued_before_it(tmp_path: Path) -> None:
    loaded = board(tmp_path)
    seen = observed(loaded)
    loaded.control().send("switch", True, kind="logic")
    loaded.send("switch", False, kind="logic")
    loaded.run_for(seconds=0.001)
    assert [value for net, _, value in seen if net == "switch"] == [True, False]
    assert loaded.net_value("switch") is False


def test_a_second_thread_stops_a_paced_run_and_queues_a_key(tmp_path: Path) -> None:
    loaded = board(tmp_path)
    seen = observed(loaded)
    control = loaded.control()

    def front_end() -> None:
        time.sleep(0.05)
        control.send("switch", True, kind="logic")
        time.sleep(0.05)
        control.stop()

    thread = threading.Thread(target=front_end)
    thread.start()
    # slow enough that the run is still going when the thread acts; the
    # thread runs while the board sleeps, the GIL released
    run = loaded.run_for(seconds=10.0, pacing=mc683xx.Pacing.scaled(0.1))
    thread.join()
    assert run.stop == mc683xx.Stop.Requested()
    assert loaded.now < 10 * 10**12
    assert control.pacing == mc683xx.Pacing.UNPACED, "this call's pacing put back"
    assert not control.stopping
    switched = [at for net, at, _ in seen if net == "switch"]
    assert len(switched) == 1
    (at,) = switched
    assert 0 < at <= loaded.now
    ticks = {at for net, at, _ in seen if net == "tick"}
    assert at % QUANTUM_PS == 0 or at in ticks, "a sync point"


def test_a_paced_run_is_the_unpaced_run(tmp_path: Path) -> None:
    def ran(pacing: mc683xx.Pacing) -> tuple[list[tuple[str, int, object]], bytes]:
        loaded = board(tmp_path)
        seen = observed(loaded)
        loaded.control().send("switch", False, kind="logic")
        loaded.run_for(seconds=0.05, pacing=pacing)
        return seen, loaded.save()

    unpaced = ran(mc683xx.Pacing.UNPACED)
    assert len(unpaced[0]) > 100
    assert ran(mc683xx.Pacing.scaled(4.0)) == unpaced


def test_real_time_pacing_keeps_board_time_with_wall_time(tmp_path: Path) -> None:
    loaded = board(tmp_path)
    started = time.monotonic()
    loaded.run_for(seconds=0.2, pacing=mc683xx.Pacing.REAL_TIME)
    paced = time.monotonic() - started
    # never ahead of wall time, its last quantum waited out too;
    # generous above, for a loaded host
    assert 0.2 <= paced < 5.0


def test_a_runs_pacing_is_put_back_even_when_it_raises(tmp_path: Path) -> None:
    loaded = board(tmp_path)
    control = loaded.control()
    control.set_pacing(mc683xx.Pacing.scaled(3.0))
    loaded.run_for(seconds=0.001, pacing=mc683xx.Pacing.scaled(100.0))
    assert control.pacing == mc683xx.Pacing.scaled(3.0)

    def failing(net: str, at: int, value: object) -> None:
        raise RuntimeError("seen")

    loaded.observe("tick", failing)
    with pytest.raises(RuntimeError, match="seen"):
        loaded.run_for(seconds=1.0, pacing=mc683xx.Pacing.scaled(100.0))
    assert control.pacing == mc683xx.Pacing.scaled(3.0)


def test_a_pacing_another_thread_sets_during_the_run_stays(tmp_path: Path) -> None:
    loaded = board(tmp_path)
    control = loaded.control()

    def front_end() -> None:
        time.sleep(0.05)
        control.set_pacing(mc683xx.Pacing.REAL_TIME)
        time.sleep(0.05)
        control.stop()

    thread = threading.Thread(target=front_end)
    thread.start()
    run = loaded.run_for(seconds=10.0, pacing=mc683xx.Pacing.scaled(0.1))
    thread.join()
    assert run.stop == mc683xx.Stop.Requested()
    assert control.pacing == mc683xx.Pacing.REAL_TIME


def test_a_second_thread_stops_an_unpaced_run(tmp_path: Path) -> None:
    loaded = board(tmp_path)
    control = loaded.control()

    def front_end() -> None:
        time.sleep(0.05)
        control.stop()

    thread = threading.Thread(target=front_end)
    thread.start()
    # far more board time than any host runs unpaced before the stop:
    # the thread gets the GIL between quanta
    run = loaded.run_for(seconds=1000.0)
    thread.join()
    assert run.stop == mc683xx.Stop.Requested()
    assert loaded.now < 1000 * 10**12


def test_short_paced_runs_in_a_loop_are_paced(tmp_path: Path) -> None:
    loaded = board(tmp_path)
    started = time.monotonic()
    for _ in range(100):
        loaded.run_for(seconds=0.005, pacing=mc683xx.Pacing.scaled(0.1))
    # 0.5 s of board time at a tenth of real time
    assert time.monotonic() - started >= 5.0
