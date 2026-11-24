use super::Layer;
use super::ParseLayerError;
use super::StateError;
use super::restore_each;

#[test]
fn a_refused_restore_puts_back_only_the_parts_restored_before() {
    // Part 2 of four refuses: parts 0 and 1 get their backups, part 2
    // is asked once, and part 3 never.
    let snapshots = [[10_u8], [11], [12], [13]];
    let backups = [[0_u8], [1], [2], [3]];
    let mut calls = Vec::new();
    let refused = restore_each(&snapshots, &backups, |part, bytes| {
        calls.push((part, bytes[0]));
        if part == 2 {
            Err(StateError::invalid("part 2"))
        } else {
            Ok(())
        }
    });
    assert!(matches!(refused, Err(StateError::Invalid(reason)) if reason == "part 2"));
    assert_eq!(calls, [(0, 10), (1, 11), (2, 12), (0, 0), (1, 1)]);
}

#[test]
fn a_part_refusing_its_backup_leaves_the_rest_put_back_and_the_first_error() {
    let snapshots = [[10_u8], [11], [12]];
    let backups = [[0_u8], [1], [2]];
    let mut calls = Vec::new();
    let refused = restore_each(&snapshots, &backups, |part, bytes| {
        calls.push((part, bytes[0]));
        match (part, bytes[0]) {
            (0, 0) => Err(StateError::invalid("part 0's backup")),
            (2, _) => Err(StateError::invalid("part 2")),
            _ => Ok(()),
        }
    });
    assert!(matches!(refused, Err(StateError::Invalid(reason)) if reason == "part 2"));
    assert_eq!(calls, [(0, 10), (1, 11), (2, 12), (0, 0), (1, 1)]);
}

#[test]
fn a_reason_may_be_built_at_run_time() {
    let register = 3;
    let error = StateError::invalid(format!("register {register}"));
    assert_eq!(error.to_string(), "snapshot inconsistent: register 3");
    assert!(matches!(error, StateError::Invalid(reason) if reason == "register 3"));
    assert_eq!(
        StateError::mismatch("a fixed reason").to_string(),
        "snapshot of other hardware: a fixed reason"
    );
}

#[test]
fn a_layer_parses_from_its_name_in_any_case() {
    for layer in Layer::ALL {
        assert_eq!(layer.to_string().parse(), Ok(layer));
        assert_eq!(layer.to_string().to_uppercase().parse(), Ok(layer));
    }
    assert_eq!("cpu".parse(), Ok(Layer::Cpu));
    assert_eq!(
        "flat".parse::<Layer>(),
        Err(ParseLayerError("flat".to_owned()))
    );
}
