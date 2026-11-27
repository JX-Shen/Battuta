//! Derive an ordinary Edit Set and check its reach against the Take it makes.
//! See ADR-0010 and #86.

mod ppq;

pub use ppq::{PpqTickRefusal, Rescaling};

use crate::controller::FIRST_CHANNEL_MODE;
use crate::edit::{apply, same_file, Edit, EditSet};
use crate::error::{Error, Result};
use crate::note::{match_notes, Note};
use crate::take::{write_atomically, Take};
use crate::track::{stated_by, valued, ChannelState, Statement};
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

/// The verified Edit Set and any coordinate conversion used to derive it.
#[derive(Debug)]
pub struct Derived {
    pub edit_set: EditSet,
    pub rescaling: Option<Rescaling>,
}

/// Derive a verified Edit Set from the note identities in two Takes. A
/// refusal names the differences the normal form could not reproduce.
pub fn derive(before: &Take, after: &Take, tolerance_ticks: u32) -> Result<Derived> {
    let input = before.smf()?;
    let mut target = after.smf()?;
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
    let rescaling = ppq::rescale(input.header.timing, &mut target)?;
    let result = if let Some(rescaling) = rescaling {
        Take::from_smf(&target)
            .and_then(|after| {
                derive_at_same_coordinates(before, &after, &input, &target, tolerance_ticks)
            })
            .map_err(|source| Error::DeriveAfterRescaling {
                rescaling,
                source: Box::new(source),
            })
    } else {
        derive_at_same_coordinates(before, after, &input, &target, tolerance_ticks)
    };
    result.map(|edit_set| Derived {
        edit_set,
        rescaling,
    })
}

fn derive_at_same_coordinates(
    before: &Take,
    after: &Take,
    input: &Smf<'_>,
    target: &Smf<'_>,
    tolerance_ticks: u32,
) -> Result<EditSet> {
    let mut differences = structural_differences(input, target);
    let reordered = reordered_tracks(&input.tracks, &target.tracks);
    if !reordered.is_empty() {
        differences.extend(reordered);
        return Err(unreachable(differences));
    }
    let mut edits = Vec::new();
    let before_notes = before.notes()?;
    let mut after_notes = after.notes()?;
    after_notes.retain(|note| note.track < input.tracks.len());
    derive_notes(&before_notes, &after_notes, tolerance_ticks, &mut edits);
    derive_channel_state(input, target, &mut edits);
    derive_tempos(input, target, &mut edits);

    let edit_set = EditSet { edits };
    let replay = match apply(before, &edit_set) {
        Ok(replay) => replay,
        Err(source) => {
            differences.extend(event_differences(input, target, |kind| {
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
    differences.extend(event_differences(&replay.smf()?, target, |_| true));
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

/// A whole-track permutation changes coordinates, even if erasing and
/// rebuilding the note contents at those track numbers would replay it (#83).
fn reordered_tracks<'a>(
    before: &[Vec<TrackEvent<'a>>],
    after: &[Vec<TrackEvent<'a>>],
) -> Vec<UnreachableDifference> {
    if before.len() != after.len() || before == after {
        return Vec::new();
    }
    let mut occurrences = HashMap::<&[TrackEvent<'a>], usize>::new();
    for track in before {
        *occurrences.entry(track).or_default() += 1;
    }
    for track in after {
        let Some(remaining) = occurrences.remove(track.as_slice()) else {
            return Vec::new();
        };
        if remaining > 1 {
            occurrences.insert(track, remaining - 1);
        }
    }
    before
        .iter()
        .zip(after)
        .enumerate()
        .filter(|(_, (before, after))| before != after)
        .map(|(track, _)| UnreachableDifference {
            kind: UnreachableKind::TrackStructure,
            track: Some(track),
            tick: None,
        })
        .collect()
}

/// Address the last statement at each track and Tick, as the tempo Edits
/// do (#90). Earlier statements stay in their written order.
fn derive_tempos(before: &Smf<'_>, after: &Smf<'_>, edits: &mut Vec<Edit>) {
    for (track, (before, after)) in before.tracks.iter().zip(&after.tracks).enumerate() {
        let mut sites = BTreeMap::<_, (Vec<_>, Vec<_>)>::new();
        for (tick, value) in tempos_on_track(before) {
            sites.entry(tick).or_default().0.push(value);
        }
        for (tick, value) in tempos_on_track(after) {
            sites.entry(tick).or_default().1.push(value);
        }
        for (tick, (source, target)) in sites {
            let delete = source.len() == target.len() + 1;
            let remaining = &source[..source.len() - usize::from(delete)];
            let replacement = if remaining == target {
                None
            } else {
                let Some((&value, shadowed)) = target.split_last() else {
                    continue;
                };
                let prior = remaining.split_last().map_or(&[][..], |(_, prefix)| prefix);
                if value == 0 || prior != shadowed {
                    continue;
                }
                Some(value)
            };
            if delete {
                edits.push(Edit::DeleteTempo {
                    track: track as i64,
                    tick: i64::from(tick),
                });
            }
            if let Some(value) = replacement {
                edits.push(Edit::SetTempo {
                    track: track as i64,
                    tick: i64::from(tick),
                    micros_per_quarter: i64::from(value),
                });
            }
        }
    }
}

fn tempos_on_track<'a>(events: &'a [TrackEvent<'_>]) -> impl Iterator<Item = (u32, u32)> + 'a {
    events_at_ticks(events).filter_map(|(tick, event)| match event {
        TrackEventKind::Meta(MetaMessage::Tempo(value)) => Some((tick, value.as_int())),
        _ => None,
    })
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

fn derive_notes(before: &[Note], after: &[Note], tolerance_ticks: u32, edits: &mut Vec<Edit>) {
    let (matched, taken) = match_notes(before, after, tolerance_ticks);
    let mut moved_from = vec![None; after.len()];
    for (note, target) in before.iter().zip(matched) {
        match target {
            None => edits.push(Edit::DeleteNote {
                id: note.id.clone(),
            }),
            Some(index) if after[index].id == note.id => {
                change_note(note, &after[index], false, edits);
            }
            Some(index) => moved_from[index] = Some(note),
        }
    }

    // Moves and additions arrive in target strike order, which decides the
    // occurrence indices of notes that land at the same address (ADR-0002).
    for (index, note) in after.iter().enumerate() {
        if let Some(source) = moved_from[index] {
            change_note(source, note, true, edits);
        } else if !taken[index] {
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
}

fn change_note(note: &Note, target: &Note, moved: bool, edits: &mut Vec<Edit>) {
    let mut resize = (note.duration != target.duration).then(|| Edit::ResizeNote {
        id: note.id.clone(),
        delta_ticks: i64::from(target.duration) - i64::from(note.duration),
    });
    // Resize first when moving the old duration would exceed the Tick range.
    let resize_first = moved && target.start.checked_add(note.duration).is_none();
    if resize_first {
        edits.extend(resize.take());
    }
    if moved {
        edits.push(Edit::MoveNote {
            id: note.id.clone(),
            delta_ticks: i64::from(target.start) - i64::from(note.start),
        });
        if note.pitch != target.pitch {
            edits.push(Edit::TransposeNote {
                id: note.id.clone(),
                semitones: i64::from(target.pitch) - i64::from(note.pitch),
            });
        }
    }
    if target.velocity != note.velocity {
        edits.push(Edit::SetVelocity {
            id: note.id.clone(),
            velocity: i64::from(target.velocity),
        });
    }
    edits.extend(resize);
}

#[derive(Default)]
struct ChannelValues {
    before: Vec<u8>,
    after: Vec<(usize, u8)>,
}

/// The statement in force is last at its address (#87).
fn derive_channel_state(before: &Smf<'_>, after: &Smf<'_>, edits: &mut Vec<Edit>) {
    for (track, (before, after)) in before.tracks.iter().zip(&after.tracks).enumerate() {
        let mut addresses = BTreeMap::<_, ChannelValues>::new();
        for (tick, event) in events_at_ticks(before) {
            if let Some((statement, value)) = channel_statement(&event) {
                addresses
                    .entry((tick, statement))
                    .or_default()
                    .before
                    .push(value);
            }
        }
        for (position, (tick, event)) in events_at_ticks(after).enumerate() {
            if let Some((statement, value)) = channel_statement(&event) {
                addresses
                    .entry((tick, statement))
                    .or_default()
                    .after
                    .push((position, value));
            }
        }

        let mut sets = Vec::new();
        for ((tick, statement), ChannelValues { before, after }) in addresses {
            let original_shadowed = &before[..before.len().saturating_sub(1)];
            if let Some((&(position, value), target_shadowed)) = after.split_last() {
                if original_shadowed
                    .iter()
                    .copied()
                    .eq(target_shadowed.iter().map(|&(_, value)| value))
                    && before.last() != Some(&value)
                {
                    sets.push((position, set_channel_state(track, tick, statement, value)));
                    continue;
                }
            }
            if let ChannelState::Controller(controller) = statement.state {
                if !before.is_empty()
                    && original_shadowed
                        .iter()
                        .copied()
                        .eq(after.iter().map(|&(_, value)| value))
                {
                    edits.push(Edit::DeleteController {
                        track: track as i64,
                        tick: i64::from(tick),
                        channel: i64::from(statement.channel),
                        controller: i64::from(controller),
                    });
                }
            }
        }

        // When several statements land at one Tick, apply places them in the
        // order the Edit Set asks for. Read that order from the target.
        sets.sort_unstable_by_key(|&(position, _)| position);
        edits.extend(sets.into_iter().map(|(_, edit)| edit));
    }
}

fn channel_statement(event: &TrackEventKind<'_>) -> Option<(Statement, u8)> {
    let statement = stated_by(event)?;
    if matches!(statement.state, ChannelState::Controller(number) if number >= FIRST_CHANNEL_MODE) {
        return None;
    }
    Some((statement, valued(event)?))
}

fn set_channel_state(track: usize, tick: u32, statement: Statement, value: u8) -> Edit {
    let track = track as i64;
    let tick = i64::from(tick);
    let channel = i64::from(statement.channel);
    let value = i64::from(value);
    match statement.state {
        ChannelState::Program => Edit::SetProgram {
            track,
            channel,
            tick,
            program: value,
        },
        ChannelState::Controller(controller) => Edit::SetController {
            track,
            channel,
            controller: i64::from(controller),
            tick,
            value,
        },
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
    let before = common_events(before, common.clone());
    let after = common_events(after, common);
    if before != after {
        differences.extend(channel_order_differences(&before, &after));
        // Relative order within one statement address changes its content.
        // Report it as that statement kind, leaving order between addresses
        // (or between other event kinds) to the event order diagnostic.
        let order_key = |event: &TrackEventKind<'a>| {
            channel_statement(event)
                .map(|(statement, _)| statement)
                .ok_or(*event)
        };
        if before.iter().map(order_key).ne(after.iter().map(order_key)) {
            differences.push(UnreachableKind::EventOrder);
        }
    }
    differences
}

/// Reordering repeated statements can change their meaning without changing
/// their multiset. The last one states the value in force (#87).
fn channel_order_differences(
    before: &[TrackEventKind<'_>],
    after: &[TrackEventKind<'_>],
) -> Vec<UnreachableKind> {
    let values = |events: &[TrackEventKind<'_>]| {
        let mut addresses = BTreeMap::<_, Vec<_>>::new();
        for (statement, value) in events.iter().filter_map(channel_statement) {
            addresses.entry(statement).or_default().push(value);
        }
        addresses
    };
    let before = values(before);
    let after = values(after);
    before
        .into_iter()
        .filter(|(statement, values)| after.get(statement) != Some(values))
        .map(|(statement, _)| match statement.state {
            ChannelState::Controller(_) => UnreachableKind::Controller,
            ChannelState::Program => UnreachableKind::Program,
        })
        .collect()
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
pub fn derive_to_edit_set(
    before: &Path,
    after: &Path,
    output: &Path,
    tolerance_ticks: u32,
) -> Result<Option<Rescaling>> {
    if same_file(before, output) || same_file(after, output) {
        return Err(Error::DeriveWriteInPlace(output.to_path_buf()));
    }
    let derived = derive(&Take::read(before)?, &Take::read(after)?, tolerance_ticks)?;
    let mut bytes = serde_json::to_vec_pretty(&derived.edit_set).expect("Edit Sets serialise");
    bytes.push(b'\n');
    write_atomically(output, &bytes)?;
    Ok(derived.rescaling)
}
