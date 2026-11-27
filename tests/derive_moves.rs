//! #89 exercises pairing through derivation and a separate replay process.

mod common;

use common::{build_note_event_take, event_stream, mid};
use serde_json::{json, Value};
use std::path::Path;

fn read_json(path: &Path) -> Value {
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

#[test]
fn a_move_is_an_explicit_judgement_with_a_stated_bound() {
    let dir = tempfile::tempdir().unwrap();
    let before = build_note_event_take(
        &dir.path().join("before.mid"),
        &[vec![(0, 0x90, 60, 72), (240, 0x80, 60, 0)]],
    );
    let after = build_note_event_take(
        &dir.path().join("after.mid"),
        &[vec![(60, 0x90, 60, 72), (300, 0x80, 60, 0)]],
    );
    let edits = dir.path().join("derived.json");
    mid()
        .arg("derive")
        .args([&before, &after])
        .arg("-o")
        .arg(&edits)
        .assert()
        .success();
    assert_eq!(
        read_json(&edits),
        json!({"edits":[
            {"kind":"delete_note","id":"t0:c0:p60:s0:n0"},
            {"kind":"add_note","track":0,"channel":0,"pitch":60,
                "start":60,"duration":240,"velocity":72}
        ]})
    );

    let output = mid()
        .arg("derive")
        .args([&before, &after])
        .args(["--tolerance", "120", "--json", "-o"])
        .arg(&edits)
        .assert()
        .success()
        .stderr("tolerance: 120 ticks\n")
        .get_output()
        .stdout
        .clone();
    assert_eq!(
        serde_json::from_slice::<Value>(&output).unwrap()["tolerance_ticks"],
        120
    );
    assert_eq!(
        read_json(&edits),
        json!({"edits":[{"kind":"move_note","id":"t0:c0:p60:s0:n0","delta_ticks":60}]})
    );

    let replay = dir.path().join("replay.mid");
    mid()
        .arg("apply")
        .args([&before, &edits])
        .arg("-o")
        .arg(&replay)
        .assert()
        .success();
    assert_eq!(event_stream(&replay), event_stream(&after));
}

#[test]
fn the_same_bounded_matching_drives_diff_and_derivation() {
    let dir = tempfile::tempdir().unwrap();
    let before = build_note_event_take(
        &dir.path().join("before.mid"),
        &[vec![
            (0, 0x90, 60, 72),
            (10, 0x90, 67, 52),
            (210, 0x80, 67, 0),
            (240, 0x80, 60, 0),
        ]],
    );
    let after = build_note_event_take(
        &dir.path().join("after.mid"),
        &[vec![
            (60, 0x90, 62, 82),
            (60, 0x90, 60, 72),
            (280, 0x80, 62, 0),
            (300, 0x80, 60, 0),
        ]],
    );
    let edits = dir.path().join("derived.json");
    mid()
        .arg("derive")
        .args([&before, &after])
        .args(["--tolerance", "120", "-o"])
        .arg(&edits)
        .assert()
        .success();

    let diff = mid()
        .arg("diff")
        .args([&before, &after])
        .args(["--tolerance", "120", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let diff: Value = serde_json::from_slice(&diff).unwrap();
    let matched: Vec<_> = diff["changed"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| (&row["before"]["id"], &row["after"]["id"]))
        .collect();
    assert_eq!(
        matched,
        [
            (&json!("t0:c0:p60:s0:n0"), &json!("t0:c0:p60:s60:n0")),
            (&json!("t0:c0:p67:s10:n0"), &json!("t0:c0:p62:s60:n0")),
        ]
    );
    assert_eq!(
        read_json(&edits),
        json!({"edits":[
            {"kind":"move_note","id":"t0:c0:p67:s10:n0","delta_ticks":50},
            {"kind":"transpose_note","id":"t0:c0:p67:s10:n0","semitones":-5},
            {"kind":"set_velocity","id":"t0:c0:p67:s10:n0","velocity":82},
            {"kind":"resize_note","id":"t0:c0:p67:s10:n0","delta_ticks":20},
            {"kind":"move_note","id":"t0:c0:p60:s0:n0","delta_ticks":60}
        ]})
    );

    let replay = dir.path().join("replay.mid");
    mid()
        .arg("apply")
        .args([&before, &edits])
        .arg("-o")
        .arg(&replay)
        .assert()
        .success();
    assert_eq!(event_stream(&replay), event_stream(&after));
}
