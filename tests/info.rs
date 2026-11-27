mod common;

use common::{mid, FIXTURE};

#[test]
fn reports_what_the_take_is() {
    let output = mid()
        .args(["info", FIXTURE, "--json"])
        .output()
        .expect("mid runs");
    assert!(output.status.success());
    let info: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("info --json is JSON");

    assert_eq!(info["format"], 1);
    assert_eq!(info["tracks"], 3);
    assert_eq!(info["ppq"], 480);
    assert_eq!(info["tempo"]["value"]["bpm"], 60.0);
    assert_eq!(info["time_signature"]["numerator"], 3);
    assert_eq!(info["time_signature"]["denominator"], 4);
    assert_eq!(info["length_ticks"], 11516);
}

/// The fixture's conductor track carries the tempo and the metre and holds no
/// notes, so reading them at all is the assertion: a `mid` that looked for them
/// beside the notes would find neither.
#[test]
fn reads_tempo_and_metre_from_the_conductor_track() {
    mid()
        .args(["info", FIXTURE])
        .assert()
        .success()
        .stdout(predicates::str::contains("tempo           60 bpm"))
        .stdout(predicates::str::contains("time signature  3/4"));
}

#[test]
fn fails_on_a_take_that_is_not_there() {
    mid()
        .args(["info", "fixtures/no-such-take.mid"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("no-such-take.mid"));
}

#[test]
fn fails_on_a_file_that_is_not_midi() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("not-midi.mid");
    common::write(&path, "this is not a MIDI file");
    mid().arg("info").arg(&path).assert().failure();
}

/// A metre this model cannot represent is an error, not a `3/0`. A wrong answer
/// about the metre would silently misplace every Bar computed from it.
#[test]
fn refuses_a_time_signature_it_cannot_represent() {
    use midly::{Format, Header, MetaMessage, Smf, Timing, TrackEvent, TrackEventKind};

    let mut smf = Smf::new(Header::new(Format::Parallel, Timing::Metrical(480.into())));
    smf.tracks.push(vec![
        TrackEvent {
            delta: 0.into(),
            // Denominator 2^8: a note value no MIDI file should claim.
            kind: TrackEventKind::Meta(MetaMessage::TimeSignature(4, 8, 24, 8)),
        },
        TrackEvent {
            delta: 0.into(),
            kind: TrackEventKind::Meta(MetaMessage::EndOfTrack),
        },
    ]);

    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("impossible-metre.mid");
    smf.save(&path).expect("scratch Take is writable");

    mid()
        .arg("info")
        .arg(&path)
        .assert()
        .failure()
        .stderr(predicates::str::contains("time signature"));
}

/// A Take stating 0 Ticks per quarter note places nothing in time: every
/// duration, every Bar line and every tempo reading derived from it would be a
/// division by zero. Refused where the division is read, not where it bites.
#[test]
fn refuses_a_take_with_no_ticks_per_quarter() {
    let dir = tempfile::tempdir().expect("temp dir");
    let take = common::build_take(
        &dir.path().join("no-ppq.mid"),
        0,
        &[(0, 3, 4)],
        &[(0, 1, 60)],
    );

    mid()
        .arg("info")
        .arg(&take)
        .assert()
        .failure()
        .stderr(predicates::str::contains("ticks per quarter"));
}

/// An unrepresentable denominator is refused wherever it appears, including
/// after a time signature that *could* be read. Before Bar semantics existed,
/// `info` reported the readable one and said nothing about the other; a Take
/// that states something this tool cannot read is not a Take it should describe
/// as if it had.
///
/// Built by hand rather than with `build_take`, which only speaks real note
/// values — 2^8 is not one, which is the whole point of the test.
#[test]
fn refuses_an_unreadable_denominator_stated_after_a_readable_one() {
    use midly::{Format, Header, MetaMessage, Smf, Timing, TrackEvent, TrackEventKind};

    let meta = |delta: u32, message: MetaMessage<'static>| TrackEvent {
        delta: delta.into(),
        kind: TrackEventKind::Meta(message),
    };
    let mut smf = Smf::new(Header::new(Format::Parallel, Timing::Metrical(480.into())));
    smf.tracks.push(vec![
        meta(0, MetaMessage::TimeSignature(3, 2, 24, 8)),
        meta(1440, MetaMessage::TimeSignature(4, 8, 24, 8)),
        meta(0, MetaMessage::EndOfTrack),
    ]);

    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("readable-then-not.mid");
    smf.save(&path).expect("scratch Take is writable");

    mid()
        .arg("info")
        .arg(&path)
        .assert()
        .failure()
        .stderr(predicates::str::contains("time signature"));
}

/// How long the Take is in the unit a musician talks in. `length_ticks` stays
/// the truth; this is the derived view of it, and it agrees with what
/// `inspect --bars` will select: eight Bars, the last one partial.
#[test]
fn reports_how_long_the_take_is_in_bars() {
    let output = mid()
        .args(["info", FIXTURE, "--json"])
        .output()
        .expect("mid runs");
    let info: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("info --json is JSON");
    assert_eq!(info["length_bars"], 8);

    mid()
        .args(["info", FIXTURE])
        .assert()
        .success()
        .stdout(predicates::str::contains("11516 ticks (8 bars)"));
}

/// `info` answers on every readable Take, including one whose Bars cannot be
/// derived — it is the command for finding out what you are holding, so it must
/// not refuse the Takes you most need to ask about. The count is absent rather
/// than guessed, and both outputs say so the same way: `info` reports the fact,
/// and `inspect --bars` is where the reason and its remedy live.
#[test]
fn reports_no_bar_count_when_the_bars_cannot_be_derived() {
    let dir = tempfile::tempdir().expect("temp dir");
    let take = common::build_take(
        &dir.path().join("no-time-signature.mid"),
        480,
        &[],
        &[(0, 240, 60)],
    );

    let output = mid()
        .args(["info", "--json"])
        .arg(&take)
        .output()
        .expect("mid runs");
    assert!(output.status.success(), "info refused a readable Take");
    let info: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("info --json is JSON");
    assert!(info["length_bars"].is_null());
    assert!(info["length_ticks"].as_u64().expect("a length") > 0);

    mid()
        .arg("info")
        .arg(&take)
        .assert()
        .success()
        .stdout(predicates::str::contains(
            "; bars need one stated time signature)",
        ));
}

/// The same input has to give the same answer whichever profile `mid` was built
/// in, and a refusal is the answer.
///
/// A delta time is 28 bits and a track may hold any number of them, so a
/// well-formed file can accumulate past the largest absolute Tick `battuta`
/// holds. That total was added up unchecked: a debug build panicked at it, and a
/// release build wrapped and reported the wrapped Tick as the length — one file,
/// two results, neither of them true. Exit 1 is the refusal, 101 the panic and 0
/// the false answer, so the code alone tells the three apart.
///
/// Asserted through `info` alone because the check is at `Take::read`, which is
/// the one door every command comes through. Five commands would be five tests
/// of one boundary.
#[test]
fn a_take_past_the_tick_range_is_refused_rather_than_answered() {
    let dir = tempfile::tempdir().expect("temp dir");
    let take = common::build_take_past_the_tick_range(&dir.path().join("past-the-range.mid"));

    let output = mid().arg("info").arg(&take).output().expect("mid runs");
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert_eq!(
        output.status.code(),
        Some(1),
        "a Take past the Tick range is a refusal, not a panic and not an answer: {stderr}"
    );
    assert!(
        stderr.contains("largest absolute Tick"),
        "the refusal does not say what was exceeded: {stderr}"
    );
    assert!(
        !stderr.contains("panicked"),
        "the refusal is a typed error, not a panic: {stderr}"
    );
}

/// Info and diff preserve the same conflicting Tempo sources — #42.
#[test]
fn info_and_diff_agree_that_cross_track_tempos_are_indeterminate() {
    let dir = tempfile::tempdir().expect("temp dir");
    let doubled = common::build_take_setting(
        &dir.path().join("two-tempos-at-nought.mid"),
        480,
        &[(0, 4, 4)],
        &[common::tempo(0, 250_000)],
        &[(0, 480, 60)],
    );
    let single = common::build_take_setting(
        &dir.path().join("one-tempo.mid"),
        480,
        &[(0, 4, 4)],
        &[],
        &[(0, 480, 60)],
    );
    let info = common::info_json(&doubled);
    assert_eq!(info["tempo"]["kind"], "indeterminate");
    let candidates = &info["tempo"]["candidates"];
    assert_eq!(candidates[0]["value"]["micros_per_quarter"], 500_000);
    assert_eq!(candidates[1]["value"]["micros_per_quarter"], 250_000);
    let diff: serde_json::Value = serde_json::from_str(&common::json_output(&[
        "diff",
        single.to_str().unwrap(),
        doubled.to_str().unwrap(),
        "--json",
    ]))
    .unwrap();
    assert_eq!(diff["tempos"], serde_json::json!([]));
    assert_eq!(diff["unranked_tempos"][0]["after"], *candidates);
    for args in [
        vec!["info", doubled.to_str().unwrap()],
        vec!["diff", single.to_str().unwrap(), doubled.to_str().unwrap()],
    ] {
        let human = common::human_output(&args);
        assert!(
            human.contains("track 0") && human.contains("track 1"),
            "{human}"
        );
        assert!(
            human.contains("120 bpm") && human.contains("240 bpm"),
            "{human}"
        );
    }
}

/// The tempo row of a Take that changes tempo says that it does, and how many
/// times: a bare `120 bpm` is the form for a Take at 120 throughout — #65,
/// ADR-0009. Built as #65's first reproduction: 3/4 at 120, then 60 at Bar 3.
#[test]
fn a_take_that_changes_tempo_says_so_on_the_tempo_row() {
    let dir = tempfile::tempdir().expect("temp dir");
    let take = common::build_take_setting(
        &dir.path().join("tempo-changes.mid"),
        480,
        &[(0, 3, 4)],
        &[common::tempo(2880, 1_000_000)],
        &[(4320, 1440, 60)],
    );
    let path = take.to_str().unwrap();

    assert_eq!(
        common::human_output(&["info", path]),
        "\
format          1
tracks          2
ppq             480
tempo           120 bpm (500000 us per quarter), then 1 change
time signature  3/4
length          5760 ticks (4 bars)
"
    );

    let info = common::info_json(&take);
    assert_eq!(info["tempo"]["kind"], "determinate");
    assert_eq!(info["tempo"]["value"]["micros_per_quarter"], 500_000);
    assert_eq!(info["tempo_stated_from"], 0);
    assert_eq!(info["tempo_changes"], 1);
}

/// A Take whose first tempo is stated part way in states none before it, and a
/// Take stating no tempo is a different Piece from one at 60: the row says where
/// the first statement is rather than reading 60 from the start — #65's second
/// reproduction.
#[test]
fn a_tempo_first_stated_part_way_in_leaves_the_opening_unstated() {
    let dir = tempfile::tempdir().expect("temp dir");
    let take = common::build_take_stating_no_tempo(
        &dir.path().join("tempo-starts-late.mid"),
        480,
        &[(0, 3, 4)],
        &[common::tempo(2880, 1_000_000)],
        &[(4320, 1440, 60)],
    );
    let path = take.to_str().unwrap();

    assert_eq!(
        common::human_output(&["info", path]),
        "\
format          1
tracks          2
ppq             480
tempo           unstated until tick 2880, then 60 bpm (1000000 us per quarter)
time signature  3/4
length          5760 ticks (4 bars)
"
    );

    let info = common::info_json(&take);
    assert_eq!(info["tempo"]["value"]["micros_per_quarter"], 1_000_000);
    assert_eq!(info["tempo_stated_from"], 2880);
    assert_eq!(info["tempo_changes"], 0);
}

/// A Take in 3/4 for two Bars and 4/4 after is not in 3/4: the row says the time
/// signature changes, and the length line gives the reason there is no Bar count
/// that is true of this Take — it states two, not none. #65's third
/// reproduction.
#[test]
fn a_take_that_changes_time_signature_says_so() {
    let dir = tempfile::tempdir().expect("temp dir");
    let take = common::build_take(
        &dir.path().join("time-signature-changes.mid"),
        480,
        &[(0, 3, 4), (2880, 4, 4)],
        &[(4320, 1440, 60)],
    );
    let path = take.to_str().unwrap();

    assert_eq!(
        common::human_output(&["info", path]),
        "\
format          1
tracks          2
ppq             480
tempo           120 bpm (500000 us per quarter)
time signature  3/4, then 1 change
length          5760 ticks (0:06.00; bars need one time signature throughout)
"
    );

    let info = common::info_json(&take);
    assert_eq!(info["time_signature"]["numerator"], 3);
    assert_eq!(info["time_signature_stated_from"], 0);
    assert_eq!(info["time_signature_changes"], 1);
}

/// A Take whose only time signature arrives at Bar 3 of what would be 3/4 states
/// exactly one, so "bars need one stated time signature" is false of it: the
/// row says the opening is unstated and the length line says what is missing.
/// #65's fourth reproduction.
#[test]
fn a_time_signature_first_stated_part_way_in_leaves_the_opening_unstated() {
    let dir = tempfile::tempdir().expect("temp dir");
    let take = common::build_take(
        &dir.path().join("time-signature-starts-late.mid"),
        480,
        &[(2880, 3, 4)],
        &[(4320, 1440, 60)],
    );
    let path = take.to_str().unwrap();

    assert_eq!(
        common::human_output(&["info", path]),
        "\
format          1
tracks          2
ppq             480
tempo           120 bpm (500000 us per quarter)
time signature  unstated until tick 2880, then 3/4
length          5760 ticks (0:06.00; bars need a time signature stated at tick 0)
"
    );

    let info = common::info_json(&take);
    assert_eq!(info["time_signature"]["numerator"], 3);
    assert_eq!(info["time_signature_stated_from"], 2880);
    assert_eq!(info["time_signature_changes"], 0);
}

/// Some exports restate the tempo and the time signature at every Bar. Saying
/// what is already in force changes nothing (ADR-0007), so such a Take is at one
/// tempo in one time signature and reads exactly as one that says each once —
/// #65.
#[test]
fn restating_the_tempo_and_time_signature_in_force_is_not_a_change() {
    let dir = tempfile::tempdir().expect("temp dir");
    let take = common::build_take_setting(
        &dir.path().join("restated-each-bar.mid"),
        480,
        &[(0, 3, 4), (1440, 3, 4), (2880, 3, 4)],
        &[common::tempo(1440, 500_000), common::tempo(2880, 500_000)],
        &[(4320, 1440, 60)],
    );
    let path = take.to_str().unwrap();

    assert_eq!(
        common::human_output(&["info", path]),
        "\
format          1
tracks          2
ppq             480
tempo           120 bpm (500000 us per quarter)
time signature  3/4
length          5760 ticks (4 bars)
"
    );

    let info = common::info_json(&take);
    assert_eq!(info["tempo_changes"], 0);
    assert_eq!(info["time_signature_changes"], 0);
}

/// A time signature of no Beats is stated at Tick 0 and never changes, so neither
/// reason above is true of it; the length line says only that no Bars could be
/// derived, and where the reason is — #65.
#[test]
fn a_time_signature_bars_cannot_be_derived_from_is_not_called_missing() {
    let dir = tempfile::tempdir().expect("temp dir");
    let take = common::build_take(
        &dir.path().join("no-beats.mid"),
        480,
        &[(0, 0, 4)],
        &[(0, 480, 60)],
    );

    let human = common::human_output(&["info", take.to_str().unwrap()]);
    assert!(
        human.contains("time signature  0/4\n")
            && human.contains("; bars cannot be derived; mid inspect --bars says why)"),
        "{human}"
    );
}

/// The whole block, because the layout is the thing under test: which facts are
/// reported, in what order, and lined up so the values form a column.
#[test]
fn reads_as_a_summary_of_what_the_take_is() {
    assert_eq!(
        common::human_output(&["info", FIXTURE]),
        "\
format          1
tracks          3
ppq             480
tempo           60 bpm (1000000 us per quarter)
time signature  3/4
length          11516 ticks (8 bars)
"
    );
}

fn tempo_at(path: &std::path::Path, tick: u32) -> serde_json::Value {
    let output = mid()
        .args(["info", "--json", "--at", &tick.to_string()])
        .arg(path)
        .assert()
        .success();
    let info: serde_json::Value =
        serde_json::from_slice(&output.get_output().stdout).expect("info --json is JSON");
    info["tempo_at"].clone()
}

#[test]
fn reads_tempo_at_a_tick_and_reports_the_statement_tick() {
    use serde_json::json;

    let dir = tempfile::tempdir().expect("temp dir");
    let path = common::build_state_tracks(
        &dir.path().join("time.mid"),
        &[vec![
            common::tempo(240, 500_000),
            common::tempo(480, 1_000_000),
            common::tempo(480, 625_000),
            common::tempo(960, 625_000),
        ]],
    );

    assert_eq!(
        tempo_at(&path, 239),
        json!({"tick": 239, "kind": "unstated"})
    );
    for (tick, stated_from, micros, bpm) in [
        (240, 240, 500_000, 120.0),
        (479, 240, 500_000, 120.0),
        (480, 480, 625_000, 96.0),
        (700, 480, 625_000, 96.0),
        (u32::MAX, 960, 625_000, 96.0),
    ] {
        assert_eq!(
            tempo_at(&path, tick),
            json!({
                "tick": tick,
                "stated_from": stated_from,
                "kind": "determinate",
                "value": {"micros_per_quarter": micros, "bpm": bpm}
            })
        );
    }
    mid()
        .args(["info", "--at", "700"])
        .arg(&path)
        .assert()
        .success()
        .stdout(predicates::str::contains(
            "tempo at tick 700: 96 bpm (625000 us per quarter), stated at tick 480",
        ));
}

#[test]
fn reads_every_conflicting_track_and_reports_agreement_as_determinate() {
    use serde_json::json;

    let dir = tempfile::tempdir().expect("temp dir");
    let path = common::build_state_tracks(
        &dir.path().join("two-tracks.mid"),
        &[
            vec![common::tempo(240, 500_000), common::tempo(480, 625_000)],
            vec![common::tempo(240, 1_000_000), common::tempo(480, 625_000)],
        ],
    );

    for tick in [240, 479] {
        assert_eq!(
            tempo_at(&path, tick),
            json!({
                "tick": tick, "stated_from": 240, "kind": "indeterminate",
                "candidates": [
                    {"track": 0, "track_names": [], "tick": 240,
                     "value": {"micros_per_quarter": 500000, "bpm": 120.0}},
                    {"track": 1, "track_names": [], "tick": 240,
                     "value": {"micros_per_quarter": 1000000, "bpm": 60.0}}
                ]
            })
        );
    }
    assert_eq!(
        tempo_at(&path, 480),
        json!({"tick": 480, "stated_from": 480, "kind": "determinate",
               "value": {"micros_per_quarter": 625000, "bpm": 96.0}})
    );

    mid()
        .args(["info", "--at", "479"])
        .arg(&path)
        .assert()
        .success()
        .stdout(predicates::str::contains(
            "tempo at tick 479: indeterminate (120 bpm (500000 us per quarter) from track 0 at tick 240; 60 bpm (1000000 us per quarter) from track 1 at tick 240)",
        ));
}

#[test]
fn reads_one_exact_statement_inside_a_conductor_track_accelerando() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = common::build_state_tracks(
        &dir.path().join("accelerando.mid"),
        &[
            (0..40)
                .map(|i| common::tempo(i * 40, 650_000 - 2_003 * i))
                .collect(),
            vec![common::strike(0, 60), common::release(1920, 60)],
        ],
    );

    let reading = tempo_at(&path, 1_099);
    assert_eq!(reading["tick"], 1_099);
    assert_eq!(reading["kind"], "determinate");
    assert_eq!(reading["stated_from"], 1_080);
    assert_eq!(reading["value"]["micros_per_quarter"], 595_919);
}

#[test]
fn reads_the_schubert_tempo_before_at_and_after_the_broadening() {
    let dir = tempfile::tempdir().expect("temp dir");
    let original = "fixtures/external/vienna4x22-schubert-d783-15.mid";
    let edited = dir.path().join("schubert-broadened.mid");
    let edits = common::edit_set(
        dir.path(),
        "broaden",
        r#"{"kind":"set_tempo","track":0,"tick":97010,"micros_per_quarter":625000},
           {"kind":"set_tempo","track":0,"tick":105010,"micros_per_quarter":500000}"#,
    );
    mid()
        .args(["apply", original])
        .arg(edits)
        .arg("-o")
        .arg(&edited)
        .assert()
        .success();

    for (tick, stated_from, micros) in [
        (97_009, 0, 500_000),
        (97_010, 97_010, 625_000),
        (105_010, 105_010, 500_000),
    ] {
        let reading = tempo_at(&edited, tick);
        assert_eq!(reading["tick"], tick);
        assert_eq!(reading["kind"], "determinate", "tick {tick}");
        assert_eq!(reading["stated_from"], stated_from, "tick {tick}");
        assert_eq!(
            reading["value"]["micros_per_quarter"], micros,
            "tick {tick}"
        );
    }
}

#[test]
fn rejects_an_invalid_tempo_query_tick_before_printing_a_report() {
    for tick in ["--at=-1", "--at=bar-2", "--at=4294967296"] {
        mid()
            .args(["info", FIXTURE, "--json", tick])
            .assert()
            .code(2)
            .stdout("");
    }
}
