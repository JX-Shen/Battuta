//! #83's process-boundary replay of a captured performance.

mod common;

use common::{mid, write};
use midly::Smf;

const SCHUBERT: &str = "fixtures/external/vienna4x22-schubert-d783-15.mid";

#[test]
fn a_broadened_schubert_cadence_replays_event_for_event() {
    let dir = tempfile::tempdir().unwrap();
    let source_edits = dir.path().join("cadence.json");
    let target = dir.path().join("broadened.mid");
    let derived = dir.path().join("derived.json");
    let replay = dir.path().join("replay.mid");

    write(
        &source_edits,
        r#"{"edits":[
          {"kind":"set_tempo","track":0,"tick":98670,"micros_per_quarter":666667},
          {"kind":"set_tempo","track":0,"tick":105010,"micros_per_quarter":500000}
        ]}"#,
    );
    mid()
        .args(["apply", SCHUBERT])
        .arg(&source_edits)
        .arg("-o")
        .arg(&target)
        .assert()
        .success();
    mid()
        .args(["derive", SCHUBERT])
        .arg(&target)
        .arg("-o")
        .arg(&derived)
        .assert()
        .success();
    mid()
        .args(["apply", SCHUBERT])
        .arg(&derived)
        .arg("-o")
        .arg(&replay)
        .assert()
        .success();

    let expected_bytes = std::fs::read(target).unwrap();
    let replay_bytes = std::fs::read(replay).unwrap();
    let expected = Smf::parse(&expected_bytes).unwrap();
    let actual = Smf::parse(&replay_bytes).unwrap();
    assert_eq!(actual.header, expected.header);
    assert_eq!(actual.tracks.len(), expected.tracks.len());
    for (track, (actual, expected)) in actual.tracks.iter().zip(&expected.tracks).enumerate() {
        assert_eq!(actual.len(), expected.len(), "track {track}");
        for (index, (actual, expected)) in actual.iter().zip(expected).enumerate() {
            assert_eq!(actual, expected, "track {track} event {index}");
        }
    }
}
