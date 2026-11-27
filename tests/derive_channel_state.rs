//! #87's channel statements, verified through derive and apply.

mod common;

use common::{
    build_state_tracks, control_change, event_stream, mid, program_change, release, strike,
};
use serde_json::{json, Value};

#[test]
fn channel_statements_and_notes_replay_as_their_own_edits() {
    let dir = tempfile::tempdir().unwrap();
    let edits = dir.path().join("derived.json");
    let replay = dir.path().join("replay.mid");

    let a = build_state_tracks(
        &dir.path().join("a.mid"),
        &[vec![
            program_change(0, 40),
            strike(0, 60),
            control_change(5, 11, 10),
            control_change(5, 11, 20),
            control_change(8, 11, 45),
            control_change(8, 11, 40),
            release(20, 60),
        ]],
    );
    let b = build_state_tracks(
        &dir.path().join("b.mid"),
        &[vec![
            program_change(0, 41),
            strike(0, 60),
            control_change(5, 11, 10),
            control_change(5, 11, 30),
            control_change(8, 11, 45),
            control_change(10, 11, 45),
            program_change(15, 42),
            release(25, 60),
        ]],
    );

    mid()
        .arg("derive")
        .args([&a, &b])
        .arg("-o")
        .arg(&edits)
        .assert()
        .success();
    let set: Value = serde_json::from_slice(&std::fs::read(&edits).unwrap()).unwrap();
    let actual = set["edits"].as_array().unwrap();
    let expected = [
        json!({"kind":"resize_note","id":"t0:c0:p60:s0:n0","delta_ticks":5}),
        json!({"kind":"set_program","track":0,"channel":0,"tick":0,"program":41}),
        json!({"kind":"set_controller","track":0,"channel":0,"controller":11,"tick":5,"value":30}),
        json!({"kind":"delete_controller","track":0,"channel":0,"controller":11,"tick":8}),
        json!({"kind":"set_controller","track":0,"channel":0,"controller":11,"tick":10,"value":45}),
        json!({"kind":"set_program","track":0,"channel":0,"tick":15,"program":42}),
    ];
    assert_eq!(actual.len(), expected.len());
    assert!(expected.iter().all(|edit| actual.contains(edit)));

    mid()
        .arg("apply")
        .args([&a, &edits])
        .arg("-o")
        .arg(&replay)
        .assert()
        .success();
    assert_eq!(event_stream(&replay), event_stream(&b));
}

#[test]
fn shadowed_controller_program_deletion_and_channel_mode_are_named_on_refusal() {
    let dir = tempfile::tempdir().unwrap();
    let output = dir.path().join("derived.json");
    let a = build_state_tracks(
        &dir.path().join("a.mid"),
        &[vec![
            program_change(0, 40),
            control_change(5, 11, 10),
            control_change(5, 11, 20),
        ]],
    );
    let b = build_state_tracks(
        &dir.path().join("b.mid"),
        &[vec![
            control_change(5, 11, 20),
            control_change(5, 11, 10),
            control_change(10, 123, 0),
        ]],
    );

    let command = mid()
        .arg("derive")
        .args([&a, &b])
        .arg("--json")
        .arg("-o")
        .arg(&output)
        .assert()
        .failure();
    let report: Value = serde_json::from_slice(&command.get_output().stdout).unwrap();
    assert_eq!(report["output"], Value::Null);
    assert_eq!(
        report["refusal"],
        json!([
            {"kind":"controller", "track":0, "tick":5},
            {"kind":"program", "track":0, "tick":0},
            {"kind":"channel_mode", "track":0, "tick":10}
        ])
    );
    assert!(!output.exists());
}

#[test]
fn a_new_unranked_site_does_not_blame_an_independent_controller_edit() {
    let dir = tempfile::tempdir().unwrap();
    let output = dir.path().join("derived.json");
    let notes = vec![strike(0, 60), release(5, 60)];
    let before = build_state_tracks(
        &dir.path().join("before.mid"),
        &[
            vec![control_change(10, 11, 10), control_change(10, 11, 20)],
            notes.clone(),
        ],
    );
    let after = build_state_tracks(
        &dir.path().join("after.mid"),
        &[
            vec![
                program_change(0, 40),
                control_change(10, 11, 15),
                control_change(10, 11, 20),
                control_change(10, 7, 80),
            ],
            notes,
        ],
    );

    let command = mid()
        .arg("derive")
        .args([&before, &after])
        .arg("--json")
        .arg("-o")
        .arg(&output)
        .assert()
        .failure();
    let report: Value = serde_json::from_slice(&command.get_output().stdout).unwrap();
    assert_eq!(
        report["refusal"],
        json!([
            {"kind": "controller", "track": 0, "tick": 10},
            {"kind": "unranked", "track": 0, "tick": 0}
        ])
    );
    assert!(!output.exists());
}

#[test]
fn derive_reports_each_conflicting_controller_at_the_same_tick_track_and_channel() {
    let statements = |volume, expression| {
        vec![
            control_change(35, 7, volume),
            control_change(35, 11, expression),
        ]
    };
    let other = statements(40, 50);
    assert_two_controller_conflicts(&[vec![], other.clone()], &[statements(60, 70), other]);
}

#[test]
fn derive_reports_every_controller_against_a_strike_once() {
    let statements = vec![control_change(35, 7, 60), control_change(35, 11, 70)];
    let target = [statements.clone(), vec![strike(35, 60), release(65, 60)]];
    for before in [[statements, vec![]], [vec![], vec![]]] {
        assert_two_controller_conflicts(&before, &target);
    }
}

fn assert_two_controller_conflicts(
    before: &[Vec<(u32, midly::TrackEventKind<'static>)>],
    after: &[Vec<(u32, midly::TrackEventKind<'static>)>],
) {
    let dir = tempfile::tempdir().unwrap();
    let before = build_state_tracks(&dir.path().join("before.mid"), before);
    let after = build_state_tracks(&dir.path().join("after.mid"), after);
    let output = dir.path().join("refused.json");
    let refusal = mid()
        .arg("derive")
        .args([before, after])
        .arg("-o")
        .arg(&output)
        .arg("--json")
        .assert()
        .code(1);
    let report: Value = serde_json::from_slice(&refusal.get_output().stdout).unwrap();
    assert_eq!(
        report["refusal"],
        json!([
            {"kind": "unranked", "track": 0, "tick": 35},
            {"kind": "unranked", "track": 0, "tick": 35},
        ])
    );
    assert!(!output.exists());
}
