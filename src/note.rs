use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

/// A note's identity, derived from its content: track, channel, pitch, start
/// tick, and an occurrence index separating notes that collide on all four.
///
/// The occurrence index is always present, even when nothing collides. Omitting
/// it while a note is unique would mean that adding a second note at the same
/// place silently renamed the first — and identity stability across an Edit Set
/// is exactly what ADR-0002 is protecting.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct NoteId(String);

impl NoteId {
    pub(crate) fn new(track: usize, channel: u8, pitch: u8, start: u32, occurrence: u32) -> Self {
        NoteId(format!(
            "t{track}:c{channel}:p{pitch}:s{start}:n{occurrence}"
        ))
    }
}

impl fmt::Display for NoteId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// One note of a Take, in the Take's own units: ticks, not seconds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Note {
    pub id: NoteId,
    pub track: usize,
    /// The names that track states, in written order; empty where it states
    /// none. Not part of the identity: a name is what the track calls itself,
    /// and the track is still the same track when it states another. See #61.
    pub track_names: Arc<[String]>,
    pub channel: u8,
    pub pitch: u8,
    pub start: u32,
    pub duration: u32,
    pub velocity: u8,

    /// Which of the notes colliding on track, channel, pitch and start Tick this
    /// one is, counted in note-on order — the fifth component of `id`, and the
    /// only one `Note` did not publish as a number.
    ///
    /// Not serialised: `id` already carries it, and an agent consuming the
    /// payload is entitled to one spelling of an identity rather than two.
    #[serde(skip)]
    pub occurrence: u32,

    /// Where the note-on lives in its track's event list. Carried so that an
    /// Edit changes the event it names and leaves every other byte of the Take
    /// alone; never part of the published contract.
    #[serde(skip)]
    pub(crate) on_event: usize,

    /// Where the note-off that ends it lives, in the same list. Carried so that
    /// restricting a Take to a passage can keep a note whole — both of its
    /// events or neither — without pairing the track a second time.
    #[serde(skip)]
    pub(crate) off_event: usize,
}

/// Match notes first by identity, then by the stated Tick tolerance (#6, #10, #89).
///
/// Returns the target index for each source note and which target notes were taken.
pub(crate) fn match_notes(
    before_notes: &[Note],
    after_notes: &[Note],
    tolerance_ticks: u32,
) -> (Vec<Option<usize>>, Vec<bool>) {
    // Which after-note each before-note was matched with, by position in
    // `after_notes`, and the other way round. Positions rather than identities:
    // the second pass matches notes whose identities differ, so an identity is
    // not a key it can use.
    let mut matched_to: Vec<Option<usize>> = vec![None; before_notes.len()];
    let mut taken: Vec<bool> = vec![false; after_notes.len()];

    let after_by_id: HashMap<&NoteId, usize> = after_notes
        .iter()
        .enumerate()
        .map(|(index, note)| (&note.id, index))
        .collect();
    for (index, note) in before_notes.iter().enumerate() {
        if let Some(&found) = after_by_id.get(&note.id) {
            matched_to[index] = Some(found);
            taken[found] = true;
        }
    }

    // A tolerance of nought asks for the first pass alone, and the bound cannot
    // deliver that by itself: a note replaced at its own Tick is nought Ticks
    // away, so the filter below admits it however tight the tolerance is, and
    // pitch reaches the comparison only as a tie-break. Not running the pass is
    // therefore the whole of the difference between matching by identity and
    // matching by nearness — #29.
    if tolerance_ticks > 0 {
        // Greedy, in the order `Take::notes` fixes — track order, then note-on
        // order — because greedy makes that order observable: two unmatched
        // notes the same distance from a candidate produce different pairings
        // depending on which is reached first. The order is part of `notes`'
        // contract, so the answer is the same on every run rather than whatever
        // iteration happened to do.
        for (index, note) in before_notes.iter().enumerate() {
            if matched_to[index].is_some() {
                continue;
            }
            let nearest = after_notes
                .iter()
                .enumerate()
                .filter(|&(candidate, other)| {
                    !taken[candidate]
                        && other.track == note.track
                        && other.channel == note.channel
                        && other.start.abs_diff(note.start) <= tolerance_ticks
                })
                // Nearest in Ticks, which is what the tolerance bounds. Pitch
                // breaks a tie because a transposed note sits at the same Tick
                // as whatever else did not move, and the after Take's own order
                // breaks the rest.
                .min_by_key(|&(candidate, other)| {
                    (
                        other.start.abs_diff(note.start),
                        other.pitch.abs_diff(note.pitch),
                        candidate,
                    )
                })
                .map(|(candidate, _)| candidate);
            if let Some(candidate) = nearest {
                matched_to[index] = Some(candidate);
                taken[candidate] = true;
            }
        }
    }
    (matched_to, taken)
}

/// A pitch under the two conventions a MIDI file does not carry: which letter
/// and accidental name a semitone, and which octave number it sits in.
///
/// Sharps only, and pitch 60 is C4. Both are choices about a file that states
/// neither — see #7. Held apart into letter, accidental and octave rather
/// than handed over as `"F#4"`, because the choice of *which note this is* is
/// the library's and the choice of how to write it down is the consumer's; the
/// same cut as ADR-0005.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PitchName {
    /// `A` through `G`.
    pub letter: char,

    /// Whether the letter is raised a semitone. Never lowered: a MIDI file
    /// carries no enharmonic spelling, so G♭ is not available to be wrong about.
    pub sharp: bool,

    /// Middle C — pitch 60 — is octave 4, which puts pitch 0 in octave −1 and
    /// pitch 127 in octave 9.
    pub octave: i8,
}

/// What to call a pitch.
///
/// Total over every value MIDI can hold: the table has an entry per semitone and
/// the octave is arithmetic, so there is no pitch this refuses to name.
pub fn pitch_name(pitch: u8) -> PitchName {
    /// The twelve semitones from C, as a letter and whether it is raised.
    const SEMITONES: [(char, bool); 12] = [
        ('C', false),
        ('C', true),
        ('D', false),
        ('D', true),
        ('E', false),
        ('F', false),
        ('F', true),
        ('G', false),
        ('G', true),
        ('A', false),
        ('A', true),
        ('B', false),
    ];

    let (letter, sharp) = SEMITONES[usize::from(pitch % 12)];
    PitchName {
        letter,
        sharp,
        // At most 127 / 12 = 10, so the subtraction cannot leave `i8`.
        octave: (pitch / 12) as i8 - 1,
    }
}
