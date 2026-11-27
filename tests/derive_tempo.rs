//! #90 through the CLI. The target events are stated as a literal Take.

mod common;

use common::{build_state_tracks, event_stream, mid, tempo};
use serde_json::{json, Value};
use std::path::Path;

fn derive_and_replay(before: &Path, after: &Path) -> Vec<Value> {
    let dir = tempfile::tempdir().unwrap();
    let edits = dir.path().join("tempo.json");
    let replay = dir.path().join("replay.mid");
    mid()
        .arg("derive")
        .args([before, after])
        .arg("-o")
        .arg(&edits)
        .assert()
        .success();
    mid()
        .arg("apply")
        .args([before, &edits])
        .arg("-o")
        .arg(&replay)
        .assert()
        .success();
    assert_eq!(event_stream(&replay), event_stream(after));
    let set: Value = serde_json::from_slice(&std::fs::read(edits).unwrap()).unwrap();
    set["edits"].as_array().unwrap().clone()
}

#[test]
fn a_broadened_phrase_has_exactly_two_new_tempo_statements() {
    let dir = tempfile::tempdir().unwrap();
    let before = build_state_tracks(
        &dir.path().join("before.mid"),
        &[vec![], vec![tempo(0, 500_000)]],
    );
    let after = build_state_tracks(
        &dir.path().join("after.mid"),
        &[
            vec![],
            vec![tempo(0, 500_000), tempo(480, 600_000), tempo(960, 500_000)],
        ],
    );
    assert_eq!(
        derive_and_replay(&before, &after),
        [
            json!({"kind":"set_tempo", "track":1, "tick":480, "micros_per_quarter":600000}),
            json!({"kind":"set_tempo", "track":1, "tick":960, "micros_per_quarter":500000}),
        ],
    );
}

#[test]
fn a_change_of_tempo_track_uses_delete_and_set() {
    use common::{release, strike};

    let dir = tempfile::tempdir().unwrap();
    let before = build_state_tracks(
        &dir.path().join("before.mid"),
        &[
            vec![strike(0, 60), release(120, 60), tempo(360, 450_000)],
            vec![tempo(0, 500_000)],
        ],
    );
    let after = build_state_tracks(
        &dir.path().join("after.mid"),
        &[
            vec![strike(0, 60), release(120, 60)],
            vec![tempo(0, 510_000), tempo(360, 450_000)],
        ],
    );
    assert_eq!(
        derive_and_replay(&before, &after),
        [
            json!({"kind":"delete_tempo", "track":0, "tick":360}),
            json!({"kind":"set_tempo", "track":1, "tick":0, "micros_per_quarter":510000}),
            json!({"kind":"set_tempo", "track":1, "tick":360, "micros_per_quarter":450000}),
        ],
    );
}

#[test]
fn deleting_a_tempo_exposes_the_previous_statement_at_its_tick() {
    let dir = tempfile::tempdir().unwrap();
    let before = build_state_tracks(
        &dir.path().join("before.mid"),
        &[vec![tempo(480, 500_000), tempo(480, 600_000)]],
    );
    let after = build_state_tracks(&dir.path().join("after.mid"), &[vec![tempo(480, 500_000)]]);
    assert_eq!(
        derive_and_replay(&before, &after),
        [json!({"kind":"delete_tempo", "track":0, "tick":480})],
    );
}

#[test]
fn moving_the_only_tempo_between_otherwise_identical_tracks_is_reachable() {
    let dir = tempfile::tempdir().unwrap();
    let before = build_state_tracks(
        &dir.path().join("before.mid"),
        &[vec![tempo(0, 500_000)], vec![]],
    );
    let after = build_state_tracks(
        &dir.path().join("after.mid"),
        &[vec![], vec![tempo(0, 500_000)]],
    );
    assert_eq!(
        derive_and_replay(&before, &after),
        [
            json!({"kind":"delete_tempo", "track":0, "tick":0}),
            json!({"kind":"set_tempo", "track":1, "tick":0, "micros_per_quarter":500000}),
        ],
    );
}
