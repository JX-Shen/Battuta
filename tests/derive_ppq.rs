mod common;

use common::mid;
use midly::num::{u15, u24, u28, u4, u7};
use midly::{Format, Header, MetaMessage, MidiMessage, Smf, Timing, TrackEvent, TrackEventKind};
use serde_json::{json, Value};
use std::path::Path;

fn take(path: &Path, ppq: u16, start: u32, end: u32, velocity: u8) {
    let note = |message| TrackEventKind::Midi {
        channel: u4::new(0),
        message,
    };
    let event = |delta, kind| TrackEvent {
        delta: u28::new(delta),
        kind,
    };
    let key = u7::new(60);
    Smf {
        header: Header::new(Format::SingleTrack, Timing::Metrical(u15::new(ppq))),
        tracks: vec![vec![
            event(
                0,
                TrackEventKind::Meta(MetaMessage::Tempo(u24::new(600_000))),
            ),
            event(
                start,
                note(MidiMessage::NoteOn {
                    key,
                    vel: u7::new(velocity),
                }),
            ),
            event(
                end - start,
                note(MidiMessage::NoteOff {
                    key,
                    vel: u7::new(0),
                }),
            ),
            event(0, TrackEventKind::Meta(MetaMessage::EndOfTrack)),
        ]],
    }
    .save(path)
    .unwrap();
}

#[test]
fn the_same_music_at_twice_the_ppq_needs_no_edits_in_either_direction() {
    let dir = tempfile::tempdir().unwrap();
    let coarse = dir.path().join("coarse.mid");
    let fine = dir.path().join("fine.mid");
    let edits = dir.path().join("edits.json");
    take(&coarse, 480, 60, 180, 75);
    take(&fine, 960, 120, 360, 75);

    for (before, after, from_ppq, to_ppq) in
        [(&coarse, &fine, 960, 480), (&fine, &coarse, 480, 960)]
    {
        let run = mid()
            .arg("derive")
            .arg(before)
            .arg(after)
            .arg("-o")
            .arg(&edits)
            .arg("--json")
            .assert()
            .success();
        let disclosure: Value = serde_json::from_slice(&run.get_output().stdout).unwrap();
        assert_eq!(
            disclosure["rescaling"],
            json!({"factor":2, "before_ppq": to_ppq, "after_ppq": from_ppq})
        );
        let stderr = String::from_utf8_lossy(&run.get_output().stderr);
        assert!(stderr.contains(&format!("{from_ppq} PPQ to {to_ppq} PPQ")));
        assert!(stderr.contains("factor 2"));
        let contents: Value = serde_json::from_slice(&std::fs::read(&edits).unwrap()).unwrap();
        assert_eq!(contents, json!({"edits":[]}));
    }
}

#[test]
fn applying_the_derived_edits_reproduces_the_target_in_the_source_grid() {
    let dir = tempfile::tempdir().unwrap();
    let before = dir.path().join("before.mid");
    let after = dir.path().join("after.mid");
    let expected = dir.path().join("expected.mid");
    let replayed = dir.path().join("replayed.mid");
    let edits = dir.path().join("edits.json");
    take(&before, 480, 60, 180, 75);
    take(&after, 960, 120, 480, 91);
    take(&expected, 480, 60, 240, 91);

    mid()
        .arg("derive")
        .arg(&before)
        .arg(&after)
        .arg("-o")
        .arg(&edits)
        .assert()
        .success();
    mid()
        .arg("apply")
        .arg(&before)
        .arg(&edits)
        .arg("-o")
        .arg(&replayed)
        .assert()
        .success();

    assert_eq!(
        common::event_stream(&replayed),
        common::event_stream(&expected)
    );
}

#[test]
fn an_off_grid_event_is_refused_at_its_first_track_and_tick() {
    let dir = tempfile::tempdir().unwrap();
    let before = dir.path().join("coarse.mid");
    let after = dir.path().join("fine.mid");
    let output = dir.path().join("absent.json");
    take(&before, 480, 60, 180, 75);
    take(&after, 960, 121, 361, 75);

    let run = mid()
        .arg("derive")
        .arg(&before)
        .arg(&after)
        .arg("-o")
        .arg(&output)
        .arg("--json")
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&run.get_output().stderr);
    assert!(stderr.contains("track 0 at Tick 121"));
    assert!(stderr.contains("not divisible"));
    let report: Value = serde_json::from_slice(&run.get_output().stdout).unwrap();
    assert_eq!(
        report["refusal"],
        json!([{"kind": "ppq", "track": 0, "tick": 121}])
    );
    assert_eq!(report["reason"], "off_grid");
    assert!(!output.exists());
}

#[test]
fn the_two_schubert_editions_have_incommensurate_ppqs() {
    let dir = tempfile::tempdir().unwrap();
    let output = dir.path().join("absent.json");
    let run = mid()
        .args([
            "derive",
            "fixtures/external/vienna4x22-schubert-d783-15.mid",
            "fixtures/external/cpjku-vienna4x22-schubert-d783-15.mid",
            "-o",
        ])
        .arg(&output)
        .arg("--json")
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&run.get_output().stderr);
    assert!(stderr.contains("480 PPQ to 4000 PPQ"));
    assert!(stderr.contains("integer multiple"));
    let report: Value = serde_json::from_slice(&run.get_output().stdout).unwrap();
    assert_eq!(
        report["refusal"],
        json!([{"kind": "ppq", "track": null, "tick": null}])
    );
    assert_eq!(report["before_ppq"], 4000);
    assert_eq!(report["after_ppq"], 480);
    assert!(!output.exists());
}

#[test]
fn an_upsampled_event_that_cannot_be_written_is_located() {
    let dir = tempfile::tempdir().unwrap();
    let before = dir.path().join("fine.mid");
    let after = dir.path().join("coarse.mid");
    let output = dir.path().join("absent.json");
    take(&before, 960, 120, 360, 75);

    for (events, delta, reason, tick) in [
        (1, 1 << 27, "delta_overflow", 1 << 27),
        (17, (1 << 27) - 1, "tick_overflow", 2_281_701_359u32),
    ] {
        let mut track = vec![
            TrackEvent {
                delta: u28::new(delta),
                kind: TrackEventKind::Meta(MetaMessage::Text(b"distance")),
            };
            events
        ];
        track.push(TrackEvent {
            delta: u28::new(0),
            kind: TrackEventKind::Meta(MetaMessage::EndOfTrack),
        });
        Smf {
            header: Header::new(Format::SingleTrack, Timing::Metrical(u15::new(480))),
            tracks: vec![track],
        }
        .save(&after)
        .unwrap();

        let run = mid()
            .arg("derive")
            .arg(&before)
            .arg(&after)
            .arg("-o")
            .arg(&output)
            .arg("--json")
            .assert()
            .failure();
        let report: Value = serde_json::from_slice(&run.get_output().stdout).unwrap();
        assert_eq!(
            report["refusal"],
            json!([{"kind": "ppq", "track": 0, "tick": tick}])
        );
        assert_eq!(report["reason"], reason);
        assert!(!output.exists());
    }
}

#[test]
fn a_later_musical_refusal_reports_the_rescaling_already_used() {
    let dir = tempfile::tempdir().unwrap();
    let before = dir.path().join("coarse.mid");
    let after = dir.path().join("fine.mid");
    let output = dir.path().join("absent.json");
    take(&before, 480, 60, 180, 75);
    take(&after, 960, 120, 360, 75);
    let bytes = std::fs::read(&after).unwrap();
    let mut target = Smf::parse(&bytes).unwrap();
    target.tracks[0].insert(
        0,
        TrackEvent {
            delta: u28::new(0),
            kind: TrackEventKind::Meta(MetaMessage::Text(b"cantabile")),
        },
    );
    target.save(&after).unwrap();

    let run = mid()
        .arg("derive")
        .arg(&before)
        .arg(&after)
        .arg("-o")
        .arg(&output)
        .arg("--json")
        .assert()
        .failure();
    let report: Value = serde_json::from_slice(&run.get_output().stdout).unwrap();
    assert_eq!(
        report["rescaling"],
        json!({"factor":2, "before_ppq":480, "after_ppq":960})
    );
    assert_eq!(
        report["refusal"],
        json!([{"kind":"text", "track":0, "tick":0}])
    );
    assert!(!output.exists());
}
