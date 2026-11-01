//! `monitor stop`: a board's stop conditions
//! ([docs/gdb.md, Stop conditions](https://github.com/charlieh0tel/mc683xx/blob/main/docs/gdb.md#stop-conditions)).

use gdbstub::target::ext::monitor_cmd::ConsoleOutput;
use gdbstub::target::ext::monitor_cmd::outputln;
use mc683xx_board::condition::Condition;
use mc683xx_board::condition::ConditionId;
use mc683xx_board::condition::Conditions;
use mc683xx_board::condition::NetChange;

/// `monitor stop`'s words after `stop`: none lists the armed
/// conditions, `conditions` what may be armed, `delete N` and `clear`
/// disarm, and a condition's words arm it.
pub(super) fn monitor(
    conditions: &mut dyn Conditions,
    words: &[&str],
    out: &mut ConsoleOutput<'_>,
) {
    match words {
        [] => armed(conditions, out),
        ["conditions"] => offered(conditions, None, out),
        ["conditions", peripheral] => offered(conditions, Some(peripheral), out),
        ["delete", id] => match id.parse() {
            Ok(id) if conditions.disarm(ConditionId(id)) => outputln!(out, "deleted {id}"),
            Ok(id) => outputln!(out, "no condition {id}"),
            Err(_) => outputln!(out, "not a condition's number: {id:?}"),
        },
        ["clear"] => {
            for (id, _) in conditions.armed() {
                conditions.disarm(id);
            }
            outputln!(out, "cleared");
        }
        words => match words.join(" ").parse::<Condition>() {
            Ok(condition) => match conditions.arm(condition.clone()) {
                Ok(id) => outputln!(out, "{id}: {condition}"),
                Err(error) => outputln!(out, "{error}"),
            },
            Err(error) => outputln!(out, "{error}"),
        },
    }
}

/// `monitor help`'s lines for `stop`.
pub(super) fn help(out: &mut ConsoleOutput<'_>) {
    outputln!(
        out,
        "stop: list the armed stop conditions, those that fired marked"
    );
    outputln!(
        out,
        "stop conditions [PERIPHERAL]: list the conditions to arm"
    );
    outputln!(out, "stop net NET CHANGE: arm a net's condition");
    outputln!(
        out,
        "stop peripheral NAME CONDITION [ARGUMENTS]: arm a peripheral's"
    );
    outputln!(out, "stop delete N, stop clear: disarm one, or all");
}

/// The armed conditions, those that fired in the last run marked.
fn armed(conditions: &dyn Conditions, out: &mut ConsoleOutput<'_>) {
    let armed = conditions.armed();
    if armed.is_empty() {
        outputln!(out, "no stop conditions armed");
        return;
    }
    let fired = conditions.fired();
    for (id, condition) in armed {
        let mark = if fired.contains(&id) { " (fired)" } else { "" };
        outputln!(out, "{id}: {condition}{mark}");
    }
}

/// What may be armed: a net's changes, and each peripheral's
/// conditions, or only `peripheral`'s.
fn offered(conditions: &dyn Conditions, peripheral: Option<&str>, out: &mut ConsoleOutput<'_>) {
    if peripheral.is_none() {
        let changes: Vec<String> = NetChange::ALL.iter().map(ToString::to_string).collect();
        outputln!(
            out,
            "net NET {}: rise, fall, high and low on logic and edge nets",
            changes.join("|")
        );
    }
    let mut listed = false;
    for (name, offered) in conditions.offered() {
        if peripheral.is_some_and(|peripheral| peripheral != name) {
            continue;
        }
        for condition in offered {
            let usage = [name.as_str(), condition.name, condition.arguments]
                .into_iter()
                .filter(|word| !word.is_empty())
                .collect::<Vec<_>>()
                .join(" ");
            outputln!(out, "peripheral {usage}: {}", condition.help);
            listed = true;
        }
    }
    if let Some(peripheral) = peripheral
        && !listed
    {
        outputln!(out, "peripheral {peripheral:?} offers no conditions");
    }
}
