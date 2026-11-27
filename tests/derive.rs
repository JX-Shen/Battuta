//! Derivation is checked by replaying the output in a separate process and
//! reading both Takes with midly, independently of the library's comparison.

mod common;

use common::{build_note_event_take, mid};
use serde_json::{json, Value};
use std::path::Path;

fn derive(before: &Path, after: &Path, edits: &Path) -> Value {
    mid()
        .arg("derive")
        .args([before, after])
        .arg("-o")
        .arg(edits)
        .assert()
        .success();
    serde_json::from_slice(&std::fs::read(edits).unwrap()).unwrap()
}

#[test]
fn an_identical_take_derives_to_an_empty_edit_set() {
    let dir = tempfile::tempdir().unwrap();
    let take = build_note_event_take(
        &dir.path().join("take.mid"),
        &[vec![(0, 0x90, 60, 72), (240, 0x80, 60, 0)]],
    );
    let edits = dir.path().join("derived.json");
    assert_eq!(derive(&take, &take, &edits), json!({"edits": []}));
}

#[test]
fn stacked_notes_keep_the_input_identities_through_replay() {
    let dir = tempfile::tempdir().unwrap();
    let before = build_note_event_take(
        &dir.path().join("before.mid"),
        &[vec![
            (0, 0x90, 60, 45),
            (0, 0x90, 60, 70),
            (0, 0x90, 60, 90),
            (120, 0x80, 60, 0),
            (240, 0x80, 60, 0),
            (360, 0x80, 60, 0),
        ]],
    );
    let after = build_note_event_take(
        &dir.path().join("after.mid"),
        &[vec![
            (0, 0x90, 60, 45),
            (0, 0x90, 60, 100),
            (120, 0x80, 60, 0),
            (360, 0x80, 60, 0),
            (480, 0x90, 62, 60),
            (600, 0x80, 62, 0),
        ]],
    );
    let edits = dir.path().join("derived.json");
    let derived = derive(&before, &after, &edits);
    let repeat = dir.path().join("repeat.json");
    derive(&before, &after, &repeat);
    assert_eq!(
        std::fs::read(&edits).unwrap(),
        std::fs::read(&repeat).unwrap()
    );

    let actual = derived["edits"].as_array().unwrap();
    let expected = [
        json!({"kind": "delete_note", "id": "t0:c0:p60:s0:n2"}),
        json!({"kind": "set_velocity", "id": "t0:c0:p60:s0:n1", "velocity": 100}),
        json!({"kind": "resize_note", "id": "t0:c0:p60:s0:n1", "delta_ticks": 120}),
        json!({"kind": "add_note", "track": 0, "channel": 0, "pitch": 62,
            "start": 480, "duration": 120, "velocity": 60}),
    ];
    assert_eq!(actual.len(), expected.len());
    for edit in expected {
        assert!(actual.contains(&edit), "missing {edit}");
    }

    let replay = dir.path().join("replay.mid");
    mid()
        .arg("apply")
        .args([&before, &edits])
        .arg("-o")
        .arg(&replay)
        .assert()
        .success();
    assert_eq!(common::event_stream(&replay), common::event_stream(&after));
}

#[test]
fn unreachable_differences_are_located_without_replacing_the_output() {
    use common::{build_state_tracks, pitch_bend, program_change, release, strike, tempo};
    use midly::{MetaMessage, TrackEventKind::Meta};

    let dir = tempfile::tempdir().unwrap();
    let before = build_state_tracks(
        &dir.path().join("before.mid"),
        &[vec![
            program_change(0, 12),
            strike(0, 60),
            release(120, 60),
            tempo(240, 500_000),
        ]],
    );
    let after = build_state_tracks(
        &dir.path().join("after.mid"),
        &[vec![
            strike(0, 60),
            program_change(0, 12),
            release(120, 60),
            tempo(240, 510_000),
            pitch_bend(360, 100),
            (480, Meta(MetaMessage::Text(b"cantabile"))),
        ]],
    );
    let output = dir.path().join("edits.json");
    std::fs::write(&output, "previous contents").unwrap();
    let command = mid()
        .arg("derive")
        .args([&before, &after])
        .arg("--json")
        .arg("-o")
        .arg(&output)
        .assert()
        .failure();
    let report: Value = serde_json::from_slice(&command.get_output().stdout).unwrap();
    assert_eq!(report["output"], Value::Null);
    assert_eq!(report["refusal"].as_array().unwrap().len(), 3, "{report}");
    for (kind, tick) in [("event_order", 0), ("bend", 360), ("text", 480)] {
        assert!(
            report["refusal"]
                .as_array()
                .unwrap()
                .contains(&json!({"kind": kind, "track": 0, "tick": tick})),
            "{report}"
        );
    }
    assert_eq!(
        std::fs::read_to_string(&output).unwrap(),
        "previous contents"
    );
}

#[test]
fn an_added_note_with_a_nonzero_release_velocity_is_unreachable() {
    let dir = tempfile::tempdir().unwrap();
    let before = build_note_event_take(&dir.path().join("before.mid"), &[vec![]]);
    let after = build_note_event_take(
        &dir.path().join("after.mid"),
        &[vec![(0, 0x90, 60, 72), (240, 0x80, 60, 17)]],
    );
    let output = dir.path().join("edits.json");
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
        json!([{"kind": "release_velocity", "track": 0, "tick": 240}])
    );
    assert!(!output.exists());
}

#[test]
fn refused_zero_duration_additions_do_not_hide_the_reachable_state_edits() {
    use common::{build_state_tracks, control_change, program_change, release, strike, tempo};

    let dir = tempfile::tempdir().unwrap();
    let carried = [strike(0, 61), release(0, 61)];
    let before = build_state_tracks(
        &dir.path().join("before.mid"),
        &[carried
            .into_iter()
            .chain([
                program_change(10, 40),
                program_change(20, 40),
                control_change(30, 11, 40),
                tempo(40, 500_000),
            ])
            .collect()],
    );
    let after = build_state_tracks(
        &dir.path().join("after.mid"),
        &[carried
            .into_iter()
            .chain([
                program_change(10, 41),
                control_change(30, 11, 60),
                tempo(40, 600_000),
                strike(90, 60),
                release(90, 60),
                strike(100, 62),
                release(100, 62),
            ])
            .collect()],
    );
    let output = dir.path().join("edits.json");
    let refusal = mid()
        .arg("derive")
        .args([&before, &after])
        .arg("--json")
        .arg("-o")
        .arg(&output)
        .assert()
        .failure();
    let report: Value = serde_json::from_slice(&refusal.get_output().stdout).unwrap();
    assert_eq!(
        report["refusal"],
        json!([
            {"kind": "note", "track": 0, "tick": 90},
            {"kind": "note", "track": 0, "tick": 100},
            {"kind": "program", "track": 0, "tick": 20}
        ])
    );
    assert!(!output.exists());
}

#[test]
fn a_refused_note_does_not_hide_state_edits_that_need_other_note_edits() {
    use common::{build_state_tracks, program_change, release, strike};

    let dir = tempfile::tempdir().unwrap();
    let before = build_state_tracks(
        &dir.path().join("before.mid"),
        &[vec![], vec![strike(0, 60), release(60, 60)]],
    );
    let after = build_state_tracks(
        &dir.path().join("after.mid"),
        &[
            vec![program_change(0, 40)],
            vec![strike(90, 62), release(90, 62)],
        ],
    );
    let output = dir.path().join("edits.json");
    let refusal = mid()
        .arg("derive")
        .args([&before, &after])
        .arg("--json")
        .arg("-o")
        .arg(&output)
        .assert()
        .failure();
    let report: Value = serde_json::from_slice(&refusal.get_output().stdout).unwrap();
    assert_eq!(
        report["refusal"],
        json!([{"kind": "note", "track": 1, "tick": 90}])
    );
    assert!(!output.exists());
}

#[test]
fn derive_refuses_an_output_naming_either_input() {
    let dir = tempfile::tempdir().unwrap();
    let before = build_note_event_take(&dir.path().join("before.mid"), &[vec![]]);
    let after = build_note_event_take(&dir.path().join("after.mid"), &[vec![]]);
    let alias = dir.path().join("alias.mid");
    std::fs::hard_link(&after, &alias).unwrap();
    let a = std::fs::read(&before).unwrap();
    let b = std::fs::read(&after).unwrap();
    for path in [&before, &alias] {
        mid()
            .arg("derive")
            .args([&before, &after])
            .arg("-o")
            .arg(path)
            .assert()
            .failure();
    }
    assert_eq!(std::fs::read(&before).unwrap(), a);
    assert_eq!(std::fs::read(&after).unwrap(), b);
}

#[test]
fn invalid_track_ends_in_either_input_are_refused_before_replay() {
    use midly::{MetaMessage, TrackEvent, TrackEventKind::Meta};

    let dir = tempfile::tempdir().unwrap();
    let valid = build_note_event_take(
        &dir.path().join("valid.mid"),
        &[
            vec![(0, 0x90, 60, 72), (240, 0x80, 60, 0)],
            vec![(0, 0x90, 62, 72), (360, 0x80, 62, 0)],
            vec![(0, 0x90, 64, 72), (480, 0x80, 64, 0)],
        ],
    );
    let bytes = std::fs::read(&valid).unwrap();
    let mut malformed = midly::Smf::parse(&bytes).unwrap();
    malformed.tracks[0].pop(); // Missing End-of-Track.
    malformed.tracks[1].insert(
        1,
        TrackEvent {
            delta: 0.into(),
            kind: Meta(MetaMessage::EndOfTrack),
        },
    ); // Duplicate End-of-Track.
    malformed.tracks[2].pop();
    malformed.tracks[2].insert(
        1,
        TrackEvent {
            delta: 60.into(),
            kind: Meta(MetaMessage::EndOfTrack),
        },
    ); // Events after the End-of-Track.
    let invalid = dir.path().join("invalid.mid");
    malformed.save(&invalid).unwrap();

    let output = dir.path().join("edits.json");
    for pair in [[&valid, &invalid], [&invalid, &valid]] {
        let command = mid()
            .arg("derive")
            .args(pair)
            .arg("--json")
            .arg("-o")
            .arg(&output)
            .assert()
            .failure();
        let report: Value = serde_json::from_slice(&command.get_output().stdout).unwrap();
        assert_eq!(
            report["refusal"],
            json!([
                {"kind": "track_end", "track": 0, "tick": 240},
                {"kind": "track_end", "track": 1, "tick": 0},
                {"kind": "track_end", "track": 2, "tick": 60},
            ])
        );
        assert!(!output.exists());
    }
}

#[test]
fn derive_reports_independent_note_release_conflicts_found_on_reread() {
    use common::{build_state_tracks, program_change, release, strike};

    let dir = tempfile::tempdir().unwrap();
    let before = build_state_tracks(
        &dir.path().join("before.mid"),
        &[vec![release(30, 60), release(90, 62)], vec![]],
    );
    let after = build_state_tracks(
        &dir.path().join("after.mid"),
        &[
            vec![
                strike(0, 60),
                release(60, 60),
                strike(80, 62),
                release(100, 62),
            ],
            vec![program_change(0, 40)],
        ],
    );
    let output = dir.path().join("refused.json");

    let refusal = mid()
        .arg("derive")
        .args([&before, &after])
        .arg("-o")
        .arg(&output)
        .arg("--json")
        .assert()
        .code(1);
    let report: Value = serde_json::from_slice(&refusal.get_output().stdout).unwrap();
    assert_eq!(
        report["refusal"],
        json!([
            {"kind": "release_velocity", "track": 0, "tick": 30},
            {"kind": "release_velocity", "track": 0, "tick": 90},
            {"kind": "unranked", "track": 1, "tick": 0},
            {"kind": "note", "track": 0, "tick": 60},
            {"kind": "note", "track": 0, "tick": 100},
        ])
    );
    assert!(!output.exists());
}
