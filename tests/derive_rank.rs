mod common;

use common::{build_note_event_take, mid};
use serde_json::{json, Value};
use std::path::Path;

#[test]
fn reordering_different_pitches_at_the_same_tick_derives_no_edits() {
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("a.mid");
    let b = dir.path().join("b.mid");
    let edits = dir.path().join("derived.json");
    let notes = [
        (0, 0x90, 60, 60),
        (0, 0x90, 64, 64),
        (10, 0x80, 60, 0),
        (10, 0x80, 64, 0),
    ];
    build_note_event_take(&a, &[notes.to_vec()]);
    build_note_event_take(&b, &[vec![notes[1], notes[0], notes[2], notes[3]]]);
    derive(&a, &b, &edits).success();
    assert_eq!(read_edits(&edits), json!({"edits": []}));
}

fn derive(a: &Path, b: &Path, edits: &Path) -> assert_cmd::assert::Assert {
    mid()
        .arg("derive")
        .arg(a)
        .arg(b)
        .arg("-o")
        .arg(edits)
        .arg("--json")
        .assert()
}

fn read_edits(path: &Path) -> Value {
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

#[test]
fn correcting_program_order_restates_the_program() {
    use common::{build_state_tracks, event_stream, program_change, release, strike};

    let dir = tempfile::tempdir().unwrap();
    let strike = strike(0, 60);
    let program = program_change(0, 40);
    let release = release(10, 60);
    let a = build_state_tracks(&dir.path().join("a.mid"), &[vec![strike, program, release]]);
    let b = build_state_tracks(&dir.path().join("b.mid"), &[vec![program, strike, release]]);
    let edits = dir.path().join("derived.json");
    let replay = dir.path().join("replay.mid");
    derive(&a, &b, &edits).success();
    assert_eq!(
        read_edits(&edits),
        json!({"edits": [
            {"kind": "set_program", "track": 0, "channel": 0, "tick": 0, "program": 40}
        ]}),
    );
    mid()
        .arg("apply")
        .arg(&a)
        .arg(&edits)
        .arg("-o")
        .arg(&replay)
        .assert()
        .success();
    assert_eq!(event_stream(&replay), event_stream(&b));

    derive(&b, &a, &edits)
        .failure()
        .stdout(predicates::str::contains("event_order"));
    derive(&a, &a, &edits).success();
    assert_eq!(read_edits(&edits), json!({"edits": []}));
}

#[test]
fn refuse_a_new_counter_rule_damper_order_even_if_the_edit_could_replay_it() {
    use common::{build_state_tracks, control_change, event_stream, release, strike};

    let dir = tempfile::tempdir().unwrap();
    let a = build_state_tracks(
        &dir.path().join("a.mid"),
        &[vec![
            strike(0, 60),
            strike(10, 64),
            release(10, 60),
            release(20, 64),
        ]],
    );
    let b = build_state_tracks(
        &dir.path().join("b.mid"),
        &[vec![
            strike(0, 60),
            control_change(10, 64, 127),
            strike(10, 64),
            release(10, 60),
            release(20, 64),
        ]],
    );
    let authored = dir.path().join("authored.json");
    std::fs::write(&authored, br#"{"edits":[{"kind":"set_controller","track":0,"tick":10,"channel":0,"controller":64,"value":127}]}"#).unwrap();
    let replay = dir.path().join("replay.mid");
    mid()
        .arg("apply")
        .arg(&a)
        .arg(&authored)
        .arg("-o")
        .arg(&replay)
        .assert()
        .success();
    assert_eq!(event_stream(&replay), event_stream(&b));

    let edits = dir.path().join("derived.json");
    let refusal = derive(&a, &b, &edits).failure();
    let report: Value = serde_json::from_slice(&refusal.get_output().stdout).unwrap();
    assert_eq!(
        report["refusal"],
        json!([{"kind": "event_order", "track": 0, "tick": 10}])
    );
    assert!(!edits.exists());
}

#[test]
fn stacked_strikes_keep_their_note_identities() {
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("a.mid");
    let b = dir.path().join("b.mid");
    let edits = dir.path().join("derived.json");
    let notes = [
        (0, 0x90, 60, 40),
        (0, 0x90, 60, 60),
        (10, 0x80, 60, 0),
        (20, 0x80, 60, 0),
    ];
    build_note_event_take(&a, &[notes.to_vec()]);
    build_note_event_take(&b, &[vec![notes[1], notes[0], notes[2], notes[3]]]);
    derive(&a, &b, &edits).success();
    assert_eq!(
        read_edits(&edits),
        json!({"edits": [
            {"kind": "set_velocity", "id": "t0:c0:p60:s0:n0", "velocity": 60},
            {"kind": "set_velocity", "id": "t0:c0:p60:s0:n1", "velocity": 40}
        ]}),
    );
}

#[test]
fn swapping_releases_of_overlapping_notes_changes_release_velocities() {
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("a.mid");
    let b = dir.path().join("b.mid");
    let edits = dir.path().join("derived.json");
    let notes = [
        (0, 0x90, 60, 40),
        (5, 0x90, 60, 60),
        (10, 0x80, 60, 20),
        (10, 0x80, 60, 30),
    ];
    build_note_event_take(&a, &[notes.to_vec()]);
    build_note_event_take(&b, &[vec![notes[0], notes[1], notes[3], notes[2]]]);
    let refusal = derive(&a, &b, &edits).failure();
    let report: Value = serde_json::from_slice(&refusal.get_output().stdout).unwrap();
    assert_eq!(
        report["refusal"],
        json!([{"kind": "release_velocity", "track": 0, "tick": 10}])
    );
}

#[test]
fn swapping_tempo_statements_changes_the_final_tempo() {
    use common::{build_state_tracks, tempo};

    let dir = tempfile::tempdir().unwrap();
    let slower = tempo(0, 600_000);
    let faster = tempo(0, 500_000);
    let a = build_state_tracks(&dir.path().join("a.mid"), &[vec![slower, faster]]);
    let b = build_state_tracks(&dir.path().join("b.mid"), &[vec![faster, slower]]);
    let edits = dir.path().join("derived.json");
    let refusal = derive(&a, &b, &edits).failure();
    let report: Value = serde_json::from_slice(&refusal.get_output().stdout).unwrap();
    assert_eq!(
        report["refusal"],
        json!([{"kind": "tempo", "track": 0, "tick": 0}])
    );
}

#[test]
fn controller_reset_order_changes_the_value_in_force() {
    use common::{build_state_tracks, control_change, event_stream};

    let dir = tempfile::tempdir().unwrap();
    let reset = control_change(0, 121, 0);
    let expression = control_change(0, 11, 60);
    let a = build_state_tracks(&dir.path().join("a.mid"), &[vec![expression, reset]]);
    let b = build_state_tracks(&dir.path().join("b.mid"), &[vec![reset, expression]]);
    let edits = dir.path().join("derived.json");

    let refusal = derive(&b, &a, &edits).failure();
    let report: Value = serde_json::from_slice(&refusal.get_output().stdout).unwrap();
    assert_eq!(
        report["refusal"],
        json!([{"kind": "controller", "track": 0, "tick": 0}])
    );
    assert!(!edits.exists());

    let replay = dir.path().join("replay.mid");
    derive(&a, &b, &edits).success();
    assert_eq!(
        read_edits(&edits),
        json!({"edits": [
            {"kind": "set_controller", "track": 0, "tick": 0, "channel": 0,
             "controller": 11, "value": 60}
        ]})
    );
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
fn changing_velocity_keeps_a_carried_program_order() {
    use common::{build_state_tracks, event_stream, program_change, release};
    use midly::{
        num::{u4, u7},
        MidiMessage::NoteOn,
        TrackEventKind,
    };

    let dir = tempfile::tempdir().unwrap();
    let with_velocity = |velocity| {
        vec![
            (
                0,
                TrackEventKind::Midi {
                    channel: u4::new(0),
                    message: NoteOn {
                        key: u7::new(60),
                        vel: u7::new(velocity),
                    },
                },
            ),
            program_change(0, 40),
            release(10, 60),
        ]
    };
    let a = build_state_tracks(&dir.path().join("a.mid"), &[with_velocity(40)]);
    let b = build_state_tracks(&dir.path().join("b.mid"), &[with_velocity(60)]);
    let edits = dir.path().join("derived.json");
    let replay = dir.path().join("replay.mid");

    derive(&a, &b, &edits).success();
    assert_eq!(
        read_edits(&edits),
        json!({"edits": [
            {"kind": "set_velocity", "id": "t0:c0:p60:s0:n0", "velocity": 60}
        ]}),
    );
    mid()
        .arg("apply")
        .arg(&a)
        .arg(&edits)
        .arg("-o")
        .arg(&replay)
        .assert()
        .success();
    assert_eq!(event_stream(&replay), event_stream(&b));
}
