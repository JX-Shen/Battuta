//! Derive an ordinary Edit Set and check its reach against the Take it makes.
//! See ADR-0010 and #86.

use crate::edit::{apply, same_file, Edit, EditSet};
use crate::error::{Error, Result};
use crate::note::Note;
use crate::take::{write_atomically, Take};
use crate::uncompared::{uncompared, Uncompared};
use midly::{MetaMessage, MidiMessage, Smf, TrackEvent, TrackEventKind};
use serde::Serialize;
use std::collections::{BTreeMap, HashMap};
use std::path::Path;

/// A difference the Edit Set could not reproduce. Header-level differences
/// have no track or Tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct UnreachableDifference {
    pub kind: UnreachableKind,
    pub track: Option<usize>,
    pub tick: Option<u32>,
}

impl UnreachableDifference {
    pub(crate) fn at(kind: UnreachableKind, track: usize, tick: u32) -> Self {
        Self {
            kind,
            track: Some(track),
            tick: Some(tick),
        }
    }
}

/// The kind of event or coordinate a difference concerns.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UnreachableKind {
    Ppq,
    Header,
    TrackStructure,
    TrackEnd,
    Note,
    Controller,
    Program,
    Tempo,
    PitchBend,
    EventOrder,
    Unranked,
    #[serde(untagged)]
    Uncompared(Uncompared),
}

/// Derive a verified Edit Set from the note identities in two Takes. A
/// refusal names the differences the normal form could not reproduce.
pub fn derive(before: &Take, after: &Take) -> Result<EditSet> {
    let input = before.smf()?;
    let target = after.smf()?;
    let mut track_ends: Vec<_> = [&input, &target]
        .into_iter()
        .flat_map(invalid_track_ends)
        .collect();
    if !track_ends.is_empty() {
        track_ends.sort_unstable();
        track_ends.dedup();
        return Err(Error::DeriveInvalidTrackEnds {
            differences: track_ends,
        });
    }
    let mut differences = structural_differences(&input, &target);
    let mut edits = Vec::new();
    let before_notes = before.notes()?;
    let mut after_notes = after.notes()?;
    after_notes.retain(|note| note.track < input.tracks.len());
    derive_notes(&before_notes, &after_notes, &mut edits);

    let edit_set = EditSet { edits };
    let replay = match apply(before, &edit_set) {
        Ok(replay) => replay,
        Err(source) => {
            differences.extend(event_differences(&input, &target, |kind| {
                !matches!(
                    event_address(kind).0,
                    UnreachableKind::Note
                        | UnreachableKind::TrackEnd
                        | UnreachableKind::Uncompared(Uncompared::ReleaseVelocity)
                )
            }));
            if let Some(difference) = blocked_note(&source, &after_notes) {
                differences.push(difference);
                return Err(unreachable(differences));
            }
            differences.sort_unstable();
            return Err(Error::DeriveReplayFailed {
                differences,
                source: Box::new(source),
            });
        }
    };
    differences.extend(event_differences(&replay.smf()?, &target, |_| true));
    if differences.is_empty() {
        return Ok(edit_set);
    }
    // A positive-duration note's strike is fully stated by the normal form.
    // Failure to reproduce it after a successful apply is a verification defect.
    if differences
        .iter()
        .any(|difference| difference.kind == UnreachableKind::Note)
    {
        differences.sort_unstable();
        return Err(Error::DeriveVerificationFailed { differences });
    }
    Err(unreachable(differences))
}

fn unreachable(mut differences: Vec<UnreachableDifference>) -> Error {
    differences.sort_unstable();
    Error::DeriveUnreachable { differences }
}

/// Exactly one End-of-Track must occupy the last event of every track (#86).
fn invalid_track_ends<'a>(take: &'a Smf<'_>) -> impl Iterator<Item = UnreachableDifference> + 'a {
    take.tracks
        .iter()
        .enumerate()
        .filter_map(|(track, events)| {
            let mut last = 0;
            for (index, (tick, event)) in events_at_ticks(events).enumerate() {
                last = tick;
                if event == TrackEventKind::Meta(MetaMessage::EndOfTrack) {
                    return (index + 1 != events.len()).then_some(UnreachableDifference::at(
                        UnreachableKind::TrackEnd,
                        track,
                        tick,
                    ));
                }
            }
            Some(UnreachableDifference::at(
                UnreachableKind::TrackEnd,
                track,
                last,
            ))
        })
}

fn structural_differences(before: &Smf<'_>, after: &Smf<'_>) -> Vec<UnreachableDifference> {
    let mut differences = Vec::new();
    if before.header.format != after.header.format {
        differences.push(UnreachableDifference {
            kind: UnreachableKind::Header,
            track: None,
            tick: None,
        });
    }
    if before.header.timing != after.header.timing {
        differences.push(UnreachableDifference {
            kind: UnreachableKind::Ppq,
            track: None,
            tick: None,
        });
    }
    let tracks = before.tracks.len();
    let target = after.tracks.len();
    for track in tracks.min(target)..tracks.max(target) {
        differences.push(UnreachableDifference {
            kind: UnreachableKind::TrackStructure,
            track: Some(track),
            tick: None,
        });
    }
    differences
}

fn blocked_note(error: &Error, notes: &[Note]) -> Option<UnreachableDifference> {
    let (kind, track, tick) = match error {
        Error::NotesIndistinguishable { track, second, .. } => {
            (UnreachableKind::Note, *track, *second)
        }
        Error::ResizeOutOfRange { id, .. } | Error::NoteReleaseChanged { id, .. } => {
            let note = notes.iter().find(|note| note.id.to_string() == *id)?;
            (
                UnreachableKind::Note,
                note.track,
                note.start + note.duration,
            )
        }
        Error::DurationOutOfRange(0) => {
            let note = notes.iter().find(|note| note.duration == 0)?;
            (UnreachableKind::Note, note.track, note.start)
        }
        Error::StateUnrankedAgainstNotes { stating, tick, .. }
        | Error::StatesUnrankedAgainstEachOther { stating, tick, .. }
        | Error::ControllerUnrankedAgainstReset { stating, tick, .. } => {
            (UnreachableKind::Unranked, *stating, *tick)
        }
        _ => return None,
    };
    Some(UnreachableDifference::at(kind, track, tick))
}

fn derive_notes(before: &[Note], after: &[Note], edits: &mut Vec<Edit>) {
    let mine: HashMap<_, _> = before.iter().map(|note| (&note.id, note)).collect();
    let theirs: HashMap<_, _> = after.iter().map(|note| (&note.id, note)).collect();

    // Scan each Take in track and strike order. In particular, additions at
    // one Tick arrive in the order the target states them (ADR-0002, #86).
    for note in before {
        let Some(target) = theirs.get(&note.id) else {
            edits.push(Edit::DeleteNote {
                id: note.id.clone(),
            });
            continue;
        };
        if target.velocity != note.velocity {
            edits.push(Edit::SetVelocity {
                id: note.id.clone(),
                velocity: i64::from(target.velocity),
            });
        }
        if target.duration != note.duration {
            edits.push(Edit::ResizeNote {
                id: note.id.clone(),
                delta_ticks: i64::from(target.duration) - i64::from(note.duration),
            });
        }
    }

    for note in after.iter().filter(|note| !mine.contains_key(&note.id)) {
        edits.push(Edit::AddNote {
            track: note.track as i64,
            channel: i64::from(note.channel),
            pitch: i64::from(note.pitch),
            start: i64::from(note.start),
            duration: i64::from(note.duration),
            velocity: i64::from(note.velocity),
        });
    }
}

/// Compare re-read events by absolute Tick: a note Edit changes its
/// neighbours' delta times even when those events themselves stay put.
fn event_differences(
    before: &Smf<'_>,
    after: &Smf<'_>,
    include: impl Fn(TrackEventKind<'_>) -> bool,
) -> Vec<UnreachableDifference> {
    let mut differences = Vec::new();
    for (track, (before, after)) in before.tracks.iter().zip(&after.tracks).enumerate() {
        let mut sites = BTreeMap::<_, (Vec<_>, Vec<_>)>::new();
        for (tick, event) in events_at_ticks(before).filter(|(_, event)| include(*event)) {
            sites.entry(tick).or_default().0.push(event);
        }
        for (tick, event) in events_at_ticks(after).filter(|(_, event)| include(*event)) {
            sites.entry(tick).or_default().1.push(event);
        }
        for (tick, (before, after)) in sites {
            if before == after {
                continue;
            }
            for kind in differences_at(&before, &after) {
                differences.push(UnreachableDifference::at(kind, track, tick));
            }
        }
    }
    differences
}

fn events_at_ticks<'a, 'b>(
    track: &'a [TrackEvent<'b>],
) -> impl Iterator<Item = (u32, TrackEventKind<'b>)> + 'a
where
    'b: 'a,
{
    track.iter().scan(0, |tick, event| {
        *tick += event.delta.as_int();
        Some((*tick, event.kind))
    })
}

fn differences_at<'a>(
    before: &[TrackEventKind<'a>],
    after: &[TrackEventKind<'a>],
) -> Vec<UnreachableKind> {
    // Cancel equal events first, including duplicate statements at a site.
    let mut counts = HashMap::<_, (usize, usize)>::new();
    for event in before {
        counts.entry(*event).or_default().0 += 1;
    }
    for event in after {
        counts.entry(*event).or_default().1 += 1;
    }

    let mut addressed = BTreeMap::<_, (usize, usize)>::new();
    for (&event, &(before, after)) in &counts {
        if before != after {
            let totals = addressed.entry(event_address(event)).or_default();
            totals.0 += before.saturating_sub(after);
            totals.1 += after.saturating_sub(before);
        }
    }
    let mut differences = Vec::new();
    for ((kind, _, _), (removed, added)) in addressed {
        differences.extend(std::iter::repeat_n(kind, removed.max(added)));
    }

    // The common events can change order even where another event was added
    // or removed. Compare only the occurrences present on both sides.
    let common: HashMap<_, _> = counts
        .into_iter()
        .map(|(event, (before, after))| (event, before.min(after)))
        .collect();
    if common_events(before, common.clone()) != common_events(after, common) {
        differences.push(UnreachableKind::EventOrder);
    }
    differences
}

fn common_events<'a>(
    events: &[TrackEventKind<'a>],
    mut wanted: HashMap<TrackEventKind<'a>, usize>,
) -> Vec<TrackEventKind<'a>> {
    events
        .iter()
        .copied()
        .filter(|event| {
            let n = wanted.get_mut(event).expect("counts cover every event");
            let found = *n > 0;
            *n = n.saturating_sub(1);
            found
        })
        .collect()
}

/// Group a removed and an added value at the same address as one difference.
fn event_address(event: TrackEventKind<'_>) -> (UnreachableKind, u8, u8) {
    if let Some(kind) = uncompared(&event) {
        let kind = match kind {
            Uncompared::EndOfTrack => UnreachableKind::TrackEnd,
            kind => UnreachableKind::Uncompared(kind),
        };
        return (kind, 0, 0);
    }
    match event {
        TrackEventKind::Midi { channel, message } => {
            let (kind, parameter) = match message {
                MidiMessage::NoteOn { key, vel } if vel.as_int() > 0 => {
                    (UnreachableKind::Note, key.as_int())
                }
                MidiMessage::NoteOn { key, .. } | MidiMessage::NoteOff { key, .. } => (
                    UnreachableKind::Uncompared(Uncompared::ReleaseVelocity),
                    key.as_int(),
                ),
                MidiMessage::Controller { controller, .. } => {
                    (UnreachableKind::Controller, controller.as_int())
                }
                MidiMessage::ProgramChange { .. } => (UnreachableKind::Program, 0),
                MidiMessage::PitchBend { .. } => (UnreachableKind::PitchBend, 0),
                MidiMessage::Aftertouch { .. } | MidiMessage::ChannelAftertouch { .. } => {
                    unreachable!("uncompared classifies aftertouch")
                }
            };
            (kind, channel.as_int(), parameter)
        }
        TrackEventKind::Meta(MetaMessage::Tempo(_)) => (UnreachableKind::Tempo, 0, 0),
        _ => unreachable!("uncompared classifies non-tempo meta and system events"),
    }
}

/// Open both Takes read-only and write the verified account to a different file.
pub fn derive_to_edit_set(before: &Path, after: &Path, output: &Path) -> Result<()> {
    if same_file(before, output) || same_file(after, output) {
        return Err(Error::DeriveWriteInPlace(output.to_path_buf()));
    }
    let edits = derive(&Take::read(before)?, &Take::read(after)?)?;
    let mut bytes = serde_json::to_vec_pretty(&edits).expect("Edit Sets serialise");
    bytes.push(b'\n');
    write_atomically(output, &bytes)
}
