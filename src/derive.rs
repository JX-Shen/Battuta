//! Derive an ordinary Edit Set and check its reach against the Take it makes.
//! See ADR-0010 and #86.

mod ppq;

pub use ppq::{PpqTickRefusal, Rescaling};

use crate::controller::{FIRST_CHANNEL_MODE, RESET_ALL_CONTROLLERS};
use crate::edit::{apply_traced, same_file, Edit, EditSet};
use crate::error::{Error, Result};
use crate::note::{match_notes, Note};
use crate::rank::departures;
use crate::take::{write_atomically, Take};
use crate::track::{stated_by, valued, ChannelState, Statement};
use crate::uncompared::{uncompared, Uncompared};
use midly::{MetaMessage, MidiMessage, Smf, TrackEvent, TrackEventKind};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet, HashMap};
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
    Bend,
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
    pub tolerance_ticks: u32,
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
        tolerance_ticks,
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
    let blocked = derive_notes(&before_notes, &after_notes, tolerance_ticks, &mut edits);
    let (corrections, new_orders) = rank_changes(input, target);
    differences.extend(new_orders);
    derive_channel_state(input, target, &corrections, &mut edits);
    derive_tempos(input, target, &mut edits);

    let edit_set = EditSet { edits };
    let (replay, origins) = match apply_traced(before, &edit_set, &[]) {
        Ok(replay) => replay,
        Err(source) => {
            // The apply error identifies the blocker. At other addresses only
            // differences without a planned Edit are known to be unreachable.
            let planned = planned_state_addresses(&edit_set.edits);
            differences.extend(event_differences(
                input,
                target,
                |_, track, _, tick, event| {
                    let address = event_address(event);
                    !matches!(
                        address.0,
                        UnreachableKind::Note
                            | UnreachableKind::TrackEnd
                            | UnreachableKind::Uncompared(Uncompared::ReleaseVelocity)
                    ) && !planned.contains(&(track, tick, address))
                },
            ));
            differences.extend(blocked.differences);
            if let Some(difference) = replay_blocker(&source, &after_notes) {
                if !differences.contains(&difference) {
                    differences.push(difference);
                }
                return Err(unreachable(differences));
            }
            differences.sort_unstable();
            return Err(Error::DeriveReplayFailed {
                differences,
                source: Box::new(source),
            });
        }
    };
    let re_read = replay.smf()?;
    let mut replay_differences =
        event_differences(&re_read, target, |source, track, index, _, _| {
            if source {
                origins[track][index]
                    .is_none_or(|original| !blocked.source.contains(&(track, original)))
            } else {
                !blocked.target.contains(&(track, index))
            }
        });
    replay_differences.retain(|difference| {
        difference.kind != UnreachableKind::EventOrder || !differences.contains(difference)
    });
    differences.extend(replay_differences);
    note_order_differences(
        &replay,
        &re_read,
        &after_notes,
        target,
        &blocked,
        &mut differences,
    )?;
    if differences.is_empty() && blocked.differences.is_empty() {
        return Ok(edit_set);
    }
    // A positive-duration note's strike is fully stated by the normal form.
    // Failure to reproduce it after a successful apply is a verification defect.
    if differences
        .iter()
        .any(|difference| difference.kind == UnreachableKind::Note)
    {
        differences.extend(blocked.differences);
        differences.sort_unstable();
        return Err(Error::DeriveVerificationFailed { differences });
    }
    differences.extend(blocked.differences);
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
    // A Tempo may move between otherwise identical tracks (#90).
    let without_tempo = |(_, kind): &(u32, TrackEventKind<'_>)| {
        !matches!(kind, TrackEventKind::Meta(MetaMessage::Tempo(_)))
    };
    if before.iter().zip(after).all(|(before, after)| {
        events_at_ticks(before)
            .filter(without_tempo)
            .eq(events_at_ticks(after).filter(without_tempo))
    }) {
        return Vec::new();
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

fn replay_blocker(error: &Error, notes: &[Note]) -> Option<UnreachableDifference> {
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
        Error::StateUnrankedAgainstNotes { stating, tick, .. }
        | Error::StatesUnrankedAgainstEachOther { stating, tick, .. }
        | Error::ControllerUnrankedAgainstReset { stating, tick, .. } => {
            (UnreachableKind::Unranked, *stating, *tick)
        }
        Error::TemposUnranked { track, tick, .. } => (UnreachableKind::Unranked, *track, *tick),
        _ => return None,
    };
    Some(UnreachableDifference::at(kind, track, tick))
}

#[derive(Default)]
struct BlockedNotes {
    differences: Vec<UnreachableDifference>,
    source: BTreeSet<(usize, usize)>,
    target: BTreeSet<(usize, usize)>,
    strikes: BTreeSet<(usize, u8, u8, u32)>,
}

impl BlockedNotes {
    fn skip_zero_duration(&mut self, source: Option<&Note>, target: &Note) -> bool {
        if target.duration != 0 || source.is_some_and(|note| note.duration == 0) {
            return false;
        }
        self.differences.push(UnreachableDifference::at(
            UnreachableKind::Note,
            target.track,
            target.start,
        ));
        if let Some(note) = source {
            self.source
                .extend([(note.track, note.on_event), (note.track, note.off_event)]);
            self.strikes.insert(strike_address(note));
        }
        self.target.extend([
            (target.track, target.on_event),
            (target.track, target.off_event),
        ]);
        self.strikes.insert(strike_address(target));
        true
    }
}

fn strike_address(note: &Note) -> (usize, u8, u8, u32) {
    (note.track, note.channel, note.pitch, note.start)
}

fn derive_notes(
    before: &[Note],
    after: &[Note],
    tolerance_ticks: u32,
    edits: &mut Vec<Edit>,
) -> BlockedNotes {
    let (matched, taken) = match_notes(before, after, tolerance_ticks);
    let mut moved_from = vec![None; after.len()];
    let mut blocked = BlockedNotes::default();
    for (note, target) in before.iter().zip(matched) {
        match target {
            None => edits.push(Edit::DeleteNote {
                id: note.id.clone(),
            }),
            Some(index) if after[index].id == note.id => {
                let target = &after[index];
                if !blocked.skip_zero_duration(Some(note), target) {
                    change_note(note, target, false, edits);
                }
            }
            Some(index) => moved_from[index] = Some(note),
        }
    }

    // Moves and additions arrive in target strike order, which decides the
    // occurrence indices of notes that land at the same address (ADR-0002).
    for (index, note) in after.iter().enumerate() {
        if let Some(source) = moved_from[index] {
            if !blocked.skip_zero_duration(Some(source), note) {
                change_note(source, note, true, edits);
            }
        } else if !taken[index] && !blocked.skip_zero_duration(None, note) {
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
    blocked
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
fn derive_channel_state(
    before: &Smf<'_>,
    after: &Smf<'_>,
    corrections: &BTreeSet<ChannelAddress>,
    edits: &mut Vec<Edit>,
) {
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
                    && (before.last() != Some(&value)
                        || corrections.contains(&(track, tick, statement)))
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

type ChannelAddress = (usize, u32, Statement);
type EventAddress = (UnreachableKind, u8, u8);

fn planned_state_addresses(edits: &[Edit]) -> BTreeSet<(usize, u32, EventAddress)> {
    edits
        .iter()
        .filter_map(|edit| {
            let (track, tick, kind, channel, parameter) = match *edit {
                Edit::SetController {
                    track,
                    tick,
                    channel,
                    controller,
                    ..
                }
                | Edit::DeleteController {
                    track,
                    tick,
                    channel,
                    controller,
                } => (
                    track,
                    tick,
                    UnreachableKind::Controller,
                    channel,
                    controller,
                ),
                Edit::SetProgram {
                    track,
                    tick,
                    channel,
                    ..
                } => (track, tick, UnreachableKind::Program, channel, 0),
                Edit::SetTempo { track, tick, .. } | Edit::DeleteTempo { track, tick } => {
                    (track, tick, UnreachableKind::Tempo, 0, 0)
                }
                _ => return None,
            };
            Some((
                track as usize,
                tick as u32,
                (kind, channel as u8, parameter as u8),
            ))
        })
        .collect()
}

/// Read new counter-rule pairs and repairs of existing pairs independently
/// of apply: an authored Edit Set can also land a new counter-rule pair (#91).
fn rank_changes(
    before: &Smf<'_>,
    after: &Smf<'_>,
) -> (BTreeSet<ChannelAddress>, Vec<UnreachableDifference>) {
    let mut corrections = BTreeSet::new();
    let mut refusals = Vec::new();
    for (track, (before, after)) in before.tracks.iter().zip(&after.tracks).enumerate() {
        for (tick, (before, after)) in event_sites(before, after, |_, _, _, _| true) {
            if before == after {
                continue;
            }
            let reset_before = reset_controllers(&before);
            let reset_after = reset_controllers(&after);
            corrections.extend(
                reset_before
                    .difference(&reset_after)
                    .map(|&statement| (track, tick, statement)),
            );
            let (_, old) = departures(&before);
            let (target_events, new) = departures(&after);
            if !new.is_subset(&old) {
                refusals.push(UnreachableDifference::at(
                    UnreachableKind::EventOrder,
                    track,
                    tick,
                ));
            }
            for &(governing, governed) in old.difference(&new) {
                if !target_events.contains(&governing) || !target_events.contains(&governed) {
                    continue;
                }
                let event = TrackEventKind::Midi {
                    channel: midly::num::u4::new(governing.channel),
                    message: governing.message,
                };
                if let Some((statement, _)) = channel_statement(&event) {
                    corrections.insert((track, tick, statement));
                }
            }
        }
    }
    (corrections, refusals)
}

/// Even with equal event multisets, repeated notes can pair differently.
fn note_order_differences(
    replay: &Take,
    replay_smf: &Smf<'_>,
    target_notes: &[Note],
    target_smf: &Smf<'_>,
    blocked: &BlockedNotes,
    differences: &mut Vec<UnreachableDifference>,
) -> Result<()> {
    let replay_notes = replay.notes()?;
    let replay_by_id: HashMap<_, _> = replay_notes.iter().map(|note| (&note.id, note)).collect();
    for target in target_notes {
        // An omitted strike can change the occurrence indices at its address.
        if blocked.strikes.contains(&strike_address(target)) {
            continue;
        }
        let Some(replayed) = replay_by_id.get(&target.id) else {
            continue;
        };
        let (kind, tick) =
            if (target.velocity, target.duration) != (replayed.velocity, replayed.duration) {
                (UnreachableKind::Note, target.start)
            } else if target_smf.tracks[target.track][target.off_event].kind
                != replay_smf.tracks[replayed.track][replayed.off_event].kind
            {
                (
                    UnreachableKind::Uncompared(Uncompared::ReleaseVelocity),
                    target.start + target.duration,
                )
            } else {
                continue;
            };
        let difference = UnreachableDifference::at(kind, target.track, tick);
        if !differences.contains(&difference) {
            differences.push(difference);
        }
    }
    Ok(())
}

type EventSites<'a> = BTreeMap<u32, (Vec<TrackEventKind<'a>>, Vec<TrackEventKind<'a>>)>;

fn event_sites<'a>(
    before: &[TrackEvent<'a>],
    after: &[TrackEvent<'a>],
    include: impl Fn(bool, usize, u32, TrackEventKind<'_>) -> bool,
) -> EventSites<'a> {
    let mut sites = EventSites::new();
    for (index, (tick, event)) in events_at_ticks(before).enumerate() {
        if include(true, index, tick, event) {
            sites.entry(tick).or_default().0.push(event);
        }
    }
    for (index, (tick, event)) in events_at_ticks(after).enumerate() {
        if include(false, index, tick, event) {
            sites.entry(tick).or_default().1.push(event);
        }
    }
    sites
}

/// Compare re-read events by absolute Tick: a note Edit changes its
/// neighbours' delta times even when those events themselves stay put.
fn event_differences(
    before: &Smf<'_>,
    after: &Smf<'_>,
    include: impl Fn(bool, usize, usize, u32, TrackEventKind<'_>) -> bool,
) -> Vec<UnreachableDifference> {
    let mut differences = Vec::new();
    for (track, (before, after)) in before.tracks.iter().zip(&after.tracks).enumerate() {
        for (tick, (before, after)) in event_sites(before, after, |source, index, tick, event| {
            include(source, track, index, tick, event)
        }) {
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
        if departures(&before).1 != departures(&after).1 {
            differences.push(UnreachableKind::EventOrder);
        }
    }
    differences
}

/// Reordering repeated statements can change their meaning without changing
/// their multiset. The last statement at an address gives the value in force.
fn channel_order_differences<'a>(
    before: &[TrackEventKind<'a>],
    after: &[TrackEventKind<'a>],
) -> Vec<UnreachableKind> {
    fn statements<'a>(
        events: &[TrackEventKind<'a>],
    ) -> BTreeMap<(UnreachableKind, u8, u8), Vec<TrackEventKind<'a>>> {
        let mut addresses = BTreeMap::<_, Vec<_>>::new();
        for &event in events {
            let address = event_address(event);
            if matches!(
                address.0,
                UnreachableKind::Controller
                    | UnreachableKind::Program
                    | UnreachableKind::Tempo
                    | UnreachableKind::Bend
            ) {
                addresses.entry(address).or_default().push(event);
            }
        }
        addresses
    }
    let reset_before = reset_controllers(before);
    let reset_after = reset_controllers(after);
    let before = statements(before);
    let after = statements(after);
    before
        .into_iter()
        .filter(|(address, values)| {
            if after.get(address) != Some(values) {
                return true;
            }
            let (kind, channel, parameter) = *address;
            let statement = Statement::controller(channel, parameter);
            kind == UnreachableKind::Controller
                && reset_before.contains(&statement) != reset_after.contains(&statement)
        })
        .map(|((kind, _, _), _)| kind)
        .collect()
}

/// Controller addresses at this track and Tick whose last statement is
/// followed by Reset All Controllers (#63, #91).
fn reset_controllers(events: &[TrackEventKind<'_>]) -> BTreeSet<Statement> {
    let mut resets = BTreeSet::new();
    let mut seen = BTreeSet::new();
    let mut cleared = BTreeSet::new();
    for event in events.iter().rev() {
        let Some(statement) = stated_by(event) else {
            continue;
        };
        match statement.state {
            ChannelState::Controller(RESET_ALL_CONTROLLERS) => {
                resets.insert(statement.channel);
            }
            ChannelState::Controller(number)
                if number < FIRST_CHANNEL_MODE
                    && seen.insert(statement)
                    && resets.contains(&statement.channel) =>
            {
                cleared.insert(statement);
            }
            _ => {}
        }
    }
    cleared
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
fn event_address(event: TrackEventKind<'_>) -> EventAddress {
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
                MidiMessage::PitchBend { .. } => (UnreachableKind::Bend, 0),
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

/// The file derivation's outcome and the coordinate settings it reports.
#[derive(Debug)]
pub struct DerivationReport {
    pub tolerance_ticks: u32,
    pub rescaling: Option<Rescaling>,
    pub result: Result<()>,
}

/// Open both Takes read-only and write the verified account to a different file.
pub fn derive_to_edit_set(
    before: &Path,
    after: &Path,
    output: &Path,
    tolerance_ticks: u32,
) -> DerivationReport {
    let mut rescaling = None;
    let mut tolerance_used = tolerance_ticks;
    let result = (|| {
        if same_file(before, output) || same_file(after, output) {
            return Err(Error::DeriveWriteInPlace(output.to_path_buf()));
        }
        let derived = match derive(&Take::read(before)?, &Take::read(after)?, tolerance_ticks) {
            Err(Error::DeriveAfterRescaling {
                rescaling: applied,
                source,
            }) => {
                rescaling = Some(applied);
                return Err(*source);
            }
            result => result?,
        };
        rescaling = derived.rescaling;
        tolerance_used = derived.tolerance_ticks;
        let mut bytes = serde_json::to_vec_pretty(&derived.edit_set).expect("Edit Sets serialise");
        bytes.push(b'\n');
        write_atomically(output, &bytes)
    })();
    DerivationReport {
        tolerance_ticks: tolerance_used,
        rescaling,
        result,
    }
}
