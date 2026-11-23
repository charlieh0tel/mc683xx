"""The enum classes, each member's name and str (python.md, Enums), and
the value classes' equality and hash."""

import pytest

import mc683xx

# Each enum class, and whether its str is Rust's Display (else the name).
ENUMS = [
    (mc683xx.Halt, True),
    (mc683xx.Ending, False),
    (mc683xx.ResetCause, True),
    (mc683xx.FunctionCode, True),
    (mc683xx.Space, False),
    (mc683xx.ClockPolarity, False),
    (mc683xx.ClockPhase, False),
    (mc683xx.BusCycleKind, True),
    (mc683xx.Invalid, False),
    (mc683xx.Channel, True),
    (mc683xx.Port, True),
    (mc683xx.Input, True),
    (mc683xx.Role, True),
    (mc683xx.Lane, True),
    (mc683xx.FaultCause, True),
    (mc683xx.Dsack, False),
]


def members(enum: type) -> list[object]:
    return [value for value in vars(enum).values() if isinstance(value, enum)]


@pytest.mark.parametrize(("enum", "displayed"), ENUMS, ids=lambda value: getattr(value, "__name__", ""))
def test_a_member_has_its_name_and_str(enum: type, displayed: bool) -> None:
    assert members(enum)
    for member in members(enum):
        name = member.name  # type: ignore[attr-defined]
        assert getattr(enum, name) == member
        assert repr(member) == f"{enum.__name__}.{name}"
        if not displayed:
            assert str(member) == name
        with pytest.raises(TypeError):
            int(member)  # type: ignore[call-overload]
        assert member != 0


def test_str_is_rust_s_display() -> None:
    assert str(mc683xx.ResetCause.WATCHDOG) == "watchdog"
    assert str(mc683xx.FunctionCode.SUPERVISOR_DATA) == "supervisor-data"
    assert str(mc683xx.BusCycleKind.FETCH) == "fetch"
    assert str(mc683xx.Ending.HUNG_UP) == "HUNG_UP"
    assert mc683xx.Port.GP.name == "GP"


def test_value_classes_compare_and_hash_by_value() -> None:
    reset = mc683xx.Event.Reset(0, mc683xx.ResetCause.POWER_ON)
    assert reset == mc683xx.Event.Reset(0, mc683xx.ResetCause.POWER_ON)
    assert reset != mc683xx.Event.Reset(1, mc683xx.ResetCause.POWER_ON)
    assert len({reset, mc683xx.Event.Reset(0, mc683xx.ResetCause.POWER_ON)}) == 1
    until = mc683xx.End.Until()
    assert hash(until) == hash(mc683xx.End.Until())
    assert len({until, mc683xx.End.Instructions()}) == 2
