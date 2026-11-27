//! Tempo Edits, observed through the binary and read back with midly (#82).

mod common;

use common::mid;
use midly::num::{u24, u28};
use midly::{Format, Header, MetaMessage, MidiMessage, Smf, Timing, TrackEvent, TrackEventKind};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

type Event = (u32, TrackEventKind<'static>);

const END: TrackEventKind<'static> = TrackEventKind::Meta(MetaMessage::EndOfTrack);

fn tempo(micros_per_quarter: u32) -> TrackEventKind<'static> {
    TrackEventKind::Meta(MetaMessage::Tempo(u24::new(micros_per_quarter)))
}

fn note_on(channel: u8, key: u8, velocity: u8) -> TrackEventKind<'static> {
    TrackEventKind::Midi {
        channel: channel.into(),
        message: MidiMessage::NoteOn {
            key: key.into(),
            vel: velocity.into(),
        },
    }
}

fn write_take(path: &Path, tracks: &[&[Event]]) {
    let mut smf = Smf::new(Header::new(Format::Parallel, Timing::Metrical(480.into())));
    for track in tracks {
        let mut previous = 0;
        smf.tracks.push(
            track
                .iter()
                .map(|&(tick, kind)| {
                    let delta = u28::new(tick - previous);
                    previous = tick;
                    TrackEvent { delta, kind }
                })
                .collect(),
        );
    }
    smf.save(path).expect("write the input Take");
}

fn read_take(bytes: &[u8]) -> Vec<Vec<(u32, TrackEventKind<'_>)>> {
    Smf::parse(bytes)
        .expect("read the output Take")
        .tracks
        .into_iter()
        .map(|track| {
            let mut tick = 0;
            track
                .into_iter()
                .map(|event| {
                    tick += event.delta.as_int();
                    (tick, event.kind)
                })
                .collect()
        })
        .collect()
}

fn write_edits(path: &Path, edits: Vec<Value>) {
    std::fs::write(path, json!({ "edits": edits }).to_string()).expect("write the Edit Set");
}

fn apply(input: &Path, edits: Vec<Value>, output: &Path) -> assert_cmd::assert::Assert {
    let edit_path = output.with_extension("json");
    write_edits(&edit_path, edits);
    mid()
        .arg("apply")
        .arg(input)
        .arg(edit_path)
        .arg("-o")
        .arg(output)
        .assert()
}

fn set_tempo(track: i64, tick: i64, micros_per_quarter: i64) -> Value {
    json!({
        "kind": "set_tempo",
        "track": track,
        "tick": tick,
        "micros_per_quarter": micros_per_quarter,
    })
}

fn delete_tempo(track: i64, tick: i64) -> Value {
    json!({ "kind": "delete_tempo", "track": track, "tick": tick })
}

fn take_paths(dir: &tempfile::TempDir) -> (PathBuf, PathBuf) {
    (dir.path().join("input.mid"), dir.path().join("output.mid"))
}

#[test]
fn inserts_a_tempo_immediately_before_the_first_strike_on_its_track() {
    let dir = tempfile::tempdir().expect("temp dir");
    let (input, output) = take_paths(&dir);
    let conductor = [(0, tempo(500_000)), (960, END)];
    let text = TrackEventKind::Meta(MetaMessage::Marker(b"cadence"));
    let release = note_on(2, 60, 0);
    let strike = note_on(4, 62, 91);
    let later = (960, tempo(480_000));
    write_take(
        &input,
        &[
            &conductor,
            &[
                (0, note_on(2, 60, 70)),
                (480, release),
                (480, text),
                (480, strike),
                (480, note_on(2, 64, 82)),
                (720, note_on(2, 64, 0)),
                (720, note_on(4, 62, 0)),
                later,
                (1_200, END),
            ],
        ],
    );

    apply(&input, vec![set_tempo(1, 480, 625_003)], &output).success();

    let bytes = std::fs::read(output).expect("output written");
    assert_eq!(
        read_take(&bytes),
        vec![
            conductor.to_vec(),
            vec![
                (0, note_on(2, 60, 70)),
                (480, release),
                (480, text),
                (480, tempo(625_003)),
                (480, strike),
                (480, note_on(2, 64, 82)),
                (720, note_on(2, 64, 0)),
                (720, note_on(4, 62, 0)),
                later,
                (1_200, END),
            ],
        ]
    );
}

#[test]
fn refuses_a_new_conflicting_tempo_on_another_track() {
    let dir = tempfile::tempdir().expect("temp dir");
    let (input, output) = take_paths(&dir);
    write_take(
        &input,
        &[&[(480, tempo(500_000)), (960, END)], &[(960, END)]],
    );

    apply(
        &input,
        vec![set_tempo(1, 240, 535_003), set_tempo(1, 480, 625_003)],
        &output,
    )
    .failure()
    .stderr(predicates::str::contains("tempo"))
    .stderr(predicates::str::contains("tick 480"))
    .stderr(predicates::str::contains("track 1"))
    .stderr(predicates::str::contains("track 0"));

    assert!(!output.exists());
}

#[test]
fn combine_refuses_an_alternative_by_its_tempo_edit_kind() {
    let dir = tempfile::tempdir().expect("temp dir");
    let (input, _) = take_paths(&dir);
    write_take(&input, &[&[(0, tempo(500_000)), (960, END)], &[(960, END)]]);

    let empty = dir.path().join("empty.json");
    let combined = dir.path().join("combined.mid");
    write_edits(&empty, vec![]);
    let set_track = [(0, tempo(625_003)), (960, END)];
    let deleted_track = [(960, END)];
    for (kind, edit, track) in [
        ("set_tempo", set_tempo(0, 0, 625_003), set_track.as_slice()),
        ("delete_tempo", delete_tempo(0, 0), deleted_track.as_slice()),
    ] {
        let alternative = dir.path().join(format!("{kind}.mid"));
        let edits = alternative.with_extension("json");
        write_take(&alternative, &[track, &[(960, END)]]);
        write_edits(&edits, vec![edit]);

        mid()
            .arg("combine")
            .arg(&input)
            .arg("--side")
            .arg(&alternative)
            .arg(edits)
            .arg("--side")
            .arg(&input)
            .arg(&empty)
            .arg("-o")
            .arg(&combined)
            .assert()
            .failure()
            .stderr(predicates::str::contains("combine does not take up"))
            .stderr(predicates::str::contains(format!("is `{kind}`")));
        assert!(!combined.exists());
    }
}

#[test]
fn changes_the_last_tempo_at_the_address_without_moving_it() {
    let dir = tempfile::tempdir().expect("temp dir");
    let (input, output) = take_paths(&dir);
    let struck = note_on(3, 60, 70);
    let mark = TrackEventKind::Meta(MetaMessage::Marker(b"theme"));
    let released = note_on(3, 60, 0);
    let earlier = (480, tempo(500_000));
    let later = (960, tempo(480_000));
    write_take(
        &input,
        &[
            &[(1_200, END)],
            &[
                earlier,
                (480, struck),
                (480, tempo(600_000)),
                (480, mark),
                (720, released),
                later,
                (1_200, END),
            ],
        ],
    );

    apply(&input, vec![set_tempo(1, 480, 625_003)], &output).success();

    let bytes = std::fs::read(output).expect("output written");
    assert_eq!(
        read_take(&bytes),
        vec![
            vec![(1_200, END)],
            vec![
                earlier,
                (480, struck),
                (480, tempo(625_003)),
                (480, mark),
                (720, released),
                later,
                (1_200, END),
            ],
        ]
    );
}

#[test]
fn inserts_at_the_end_of_a_tick_without_strikes_and_extends_the_track_end_if_needed() {
    let dir = tempfile::tempdir().expect("temp dir");
    let (input, output) = take_paths(&dir);
    let conductor = [(1_200, END)];
    let mark = TrackEventKind::Meta(MetaMessage::Marker(b"intro"));
    write_take(&input, &[&conductor, &[(480, mark), (960, END)]]);

    apply(
        &input,
        vec![set_tempo(1, 480, 1), set_tempo(1, 1_080, 16_777_215)],
        &output,
    )
    .success();

    let bytes = std::fs::read(output).expect("output written");
    assert_eq!(
        read_take(&bytes),
        vec![
            conductor.to_vec(),
            vec![
                (480, mark),
                (480, tempo(1)),
                (1_080, tempo(16_777_215)),
                (1_080, END),
            ],
        ]
    );
}

#[test]
fn refuses_out_of_range_tempo_fields_without_writing_a_take() {
    let dir = tempfile::tempdir().expect("temp dir");
    let (input, output) = take_paths(&dir);
    write_take(&input, &[&[(960, END)], &[(960, END)]]);
    for (edit, reason) in [
        (set_tempo(0, 480, 0), "tempo 0 is out of range"),
        (
            set_tempo(0, 480, 16_777_216),
            "tempo 16777216 is out of range",
        ),
        (set_tempo(0, -1, 500_000), "tempo tick -1 is out of range"),
        (set_tempo(2, 480, 500_000), "track 2 is not in the Take"),
    ] {
        apply(&input, vec![set_tempo(0, 0, 500_000), edit], &output)
            .failure()
            .stderr(predicates::str::contains(reason));
        assert!(!output.exists());
    }
}

#[test]
fn keeps_carried_conflicts_and_accepts_agreement_with_the_last_statement() {
    let dir = tempfile::tempdir().expect("temp dir");
    let (input, output) = take_paths(&dir);
    let first = [(0, tempo(500_000)), (960, END)];
    let second = [
        (0, tempo(600_000)),
        (480, tempo(700_000)),
        (480, tempo(625_003)),
        (960, END),
    ];
    write_take(&input, &[&first, &second]);

    apply(&input, vec![set_tempo(0, 480, 625_003)], &output).success();

    let bytes = std::fs::read(output).expect("output written");
    assert_eq!(
        read_take(&bytes),
        vec![
            vec![(0, tempo(500_000)), (480, tempo(625_003)), (960, END)],
            second.to_vec(),
        ]
    );
}

#[test]
fn refuses_a_new_conflicting_pair_at_a_tick_with_a_carried_conflict() {
    let dir = tempfile::tempdir().expect("temp dir");
    let (input, output) = take_paths(&dir);
    write_take(
        &input,
        &[
            &[(480, tempo(500_000)), (960, END)],
            &[(480, tempo(600_000)), (960, END)],
        ],
    );

    apply(&input, vec![set_tempo(1, 480, 625_003)], &output)
        .failure()
        .stderr(predicates::str::contains("conflicting tempo statements"))
        .stderr(predicates::str::contains("625003"));
    assert!(!output.exists());
}

#[test]
fn judges_cross_track_tempo_after_the_whole_edit_set() {
    let dir = tempfile::tempdir().expect("temp dir");
    let (input, output) = take_paths(&dir);
    write_take(
        &input,
        &[&[(480, tempo(500_000)), (960, END)], &[(960, END)]],
    );

    apply(
        &input,
        vec![set_tempo(1, 480, 625_003), set_tempo(0, 480, 625_003)],
        &output,
    )
    .success();

    let bytes = std::fs::read(output).expect("output written");
    let agreed = vec![(480, tempo(625_003)), (960, END)];
    assert_eq!(read_take(&bytes), vec![agreed.clone(), agreed]);
}

#[test]
fn deletes_only_the_last_tempo_at_the_named_address() {
    let dir = tempfile::tempdir().expect("temp dir");
    let (input, output) = take_paths(&dir);
    let other_track = [(0, tempo(480_000)), (960, END)];
    let marker = (480, TrackEventKind::Meta(MetaMessage::Marker(b"theme")));
    let earlier = (480, tempo(500_000));
    let strike = (480, note_on(3, 60, 90));
    let release = (720, note_on(3, 60, 0));
    write_take(
        &input,
        &[
            &other_track,
            &[
                marker,
                earlier,
                strike,
                (480, tempo(625_000)),
                release,
                (960, END),
            ],
        ],
    );

    apply(&input, vec![delete_tempo(1, 480)], &output).success();

    assert_eq!(
        read_take(&std::fs::read(&output).expect("read output")),
        vec![
            other_track.to_vec(),
            vec![marker, earlier, strike, release, (960, END)]
        ],
    );
}

#[test]
fn refuses_a_second_deletion_of_the_same_original_tempo() {
    let dir = tempfile::tempdir().expect("temp dir");
    let (input, output) = take_paths(&dir);
    write_take(
        &input,
        &[&[(480, tempo(500_000)), (480, tempo(625_000)), (960, END)]],
    );

    apply(
        &input,
        vec![delete_tempo(0, 480), delete_tempo(0, 480)],
        &output,
    )
    .failure()
    .stderr(predicates::str::contains("already deleted"))
    .stderr(predicates::str::contains("track 0 at tick 480"));
    assert!(!output.exists());
}

#[test]
fn refuses_deletion_that_exposes_a_new_cross_track_tempo_conflict() {
    let dir = tempfile::tempdir().expect("temp dir");
    let (input, output) = take_paths(&dir);
    write_take(
        &input,
        &[
            &[(480, tempo(500_000)), (960, END)],
            &[(480, tempo(625_000)), (480, tempo(500_000)), (960, END)],
        ],
    );

    apply(&input, vec![delete_tempo(1, 480)], &output)
        .failure()
        .stderr(predicates::str::contains(
            "conflicting tempo statements at tick 480",
        ))
        .stderr(predicates::str::contains("track 1 states 625000"))
        .stderr(predicates::str::contains("track 0 states 500000"));
    assert!(!output.exists());
}

#[test]
fn refuses_to_delete_an_address_empty_in_the_input_take() {
    let dir = tempfile::tempdir().expect("temp dir");
    let (input, output) = take_paths(&dir);
    write_take(&input, &[&[(0, tempo(500_000)), (960, END)]]);

    apply(
        &input,
        vec![set_tempo(0, 480, 625_000), delete_tempo(0, 480)],
        &output,
    )
    .failure()
    .stderr(predicates::str::contains(
        "the input Take states no tempo on track 0 at tick 480",
    ));
    assert!(!output.exists());
}

#[test]
fn insertion_and_deletion_round_trip_an_unstated_tempo_within_the_track_end() {
    let dir = tempfile::tempdir().expect("temp dir");
    let (input, intermediate) = take_paths(&dir);
    let output = dir.path().join("restored.mid");
    let strike = (480, note_on(0, 60, 90));
    let release = (720, note_on(0, 60, 0));
    write_take(&input, &[&[(960, END)], &[strike, release, (960, END)]]);

    apply(&input, vec![set_tempo(1, 480, 625_000)], &intermediate).success();
    apply(&intermediate, vec![delete_tempo(1, 480)], &output).success();

    let before = std::fs::read(&input).expect("read original");
    let after = std::fs::read(&output).expect("read restored Take");
    assert_eq!(Smf::parse(&before).unwrap(), Smf::parse(&after).unwrap());
}

#[test]
fn setting_the_original_tempo_value_restores_the_event_stream() {
    let dir = tempfile::tempdir().expect("temp dir");
    let (input, intermediate) = take_paths(&dir);
    let output = dir.path().join("restored.mid");
    let strike = (480, note_on(0, 60, 90));
    let release = (720, note_on(0, 60, 0));
    write_take(
        &input,
        &[
            &[(960, END)],
            &[strike, (480, tempo(500_000)), release, (960, END)],
        ],
    );

    apply(&input, vec![set_tempo(1, 480, 625_000)], &intermediate).success();
    apply(&intermediate, vec![set_tempo(1, 480, 500_000)], &output).success();

    let before = std::fs::read(&input).expect("read original");
    let after = std::fs::read(&output).expect("read restored Take");
    assert_eq!(Smf::parse(&before).unwrap(), Smf::parse(&after).unwrap());
}

#[test]
fn deleting_an_inserted_tempo_keeps_the_extended_track_end() {
    let dir = tempfile::tempdir().expect("temp dir");
    let (input, intermediate) = take_paths(&dir);
    let output = dir.path().join("after-delete.mid");
    let music = [
        (0, note_on(0, 60, 90)),
        (960, note_on(0, 60, 0)),
        (960, END),
    ];
    write_take(&input, &[&[(0, tempo(500_000)), (240, END)], &music]);

    apply(&input, vec![set_tempo(0, 480, 625_000)], &intermediate).success();
    apply(&intermediate, vec![delete_tempo(0, 480)], &output).success();

    assert_eq!(
        read_take(&std::fs::read(&output).expect("read output")),
        vec![vec![(0, tempo(500_000)), (480, END)], music.to_vec()],
    );
}
