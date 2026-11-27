//! #83 refuses an observable permutation of the tracks.

mod common;

use common::{build_note_event_take, mid};
use serde_json::{json, Value};

#[test]
fn exchanging_whole_tracks_refuses_without_an_edit_set() {
    let dir = tempfile::tempdir().unwrap();
    let first = vec![(0, 0x90, 60, 64), (60, 0x80, 60, 0)];
    let second = vec![(0, 0x90, 65, 64), (60, 0x80, 65, 0)];
    let source = build_note_event_take(
        &dir.path().join("source.mid"),
        &[first.clone(), second.clone()],
    );
    let target = build_note_event_take(&dir.path().join("target.mid"), &[second, first]);
    let edits = dir.path().join("derived.json");
    let report = mid()
        .arg("derive")
        .arg(&source)
        .arg(&target)
        .arg("-o")
        .arg(&edits)
        .arg("--json")
        .assert()
        .failure();
    let report: Value = serde_json::from_slice(&report.get_output().stdout).unwrap();
    assert_eq!(
        report["refusal"],
        json!([
            {"kind": "track_structure", "track": 0, "tick": null},
            {"kind": "track_structure", "track": 1, "tick": null},
        ])
    );
    assert!(!edits.exists());
}
