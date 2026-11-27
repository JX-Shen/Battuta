use super::{UnreachableDifference, UnreachableKind};
use crate::error::{Error, Result};
use midly::num::u28;
use midly::{Smf, Timing};
use serde::Serialize;

/// The target was read in the source Take's PPQ. Factor is the ratio of the
/// larger PPQ to the smaller. The pair of PPQs states the direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Rescaling {
    pub before_ppq: u16,
    pub after_ppq: u16,
    pub factor: u16,
}

/// Why an event cannot be written at the requested PPQ.
#[derive(Debug, Clone, Copy, thiserror::Error, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PpqTickRefusal {
    #[error("its Tick is not divisible by the factor")]
    OffGrid,
    #[error("the resulting Tick exceeds the range of a Tick")]
    TickOverflow,
    #[error("the resulting gap exceeds the range of a MIDI delta")]
    DeltaOverflow,
}

/// Read the target in the source's coordinates, leaving its event values
/// and order intact (#88).
pub(super) fn rescale(before: Timing, target: &mut Smf<'_>) -> Result<Option<Rescaling>> {
    let (Timing::Metrical(before), Timing::Metrical(after)) = (before, target.header.timing) else {
        return Ok(None);
    };
    let (before_ppq, after_ppq) = (before.as_int(), after.as_int());
    if before_ppq == after_ppq {
        return Ok(None);
    }
    let smaller = before_ppq.min(after_ppq);
    let larger = before_ppq.max(after_ppq);
    if smaller == 0 || larger % smaller != 0 {
        return Err(Error::DeriveInexactPpq {
            before_ppq,
            after_ppq,
        });
    }

    let scaling = Rescaling {
        before_ppq,
        after_ppq,
        factor: larger / smaller,
    };
    for (track, events) in target.tracks.iter_mut().enumerate() {
        let mut tick = 0u32;
        let mut previous_scaled = 0;
        for event in events {
            tick += event.delta.as_int();
            let scaled = scaling.tick(tick).and_then(|scaled| {
                let delta =
                    u28::try_from(scaled - previous_scaled).ok_or(PpqTickRefusal::DeltaOverflow)?;
                Ok((scaled, delta))
            });
            let (scaled, delta) = scaled.map_err(|reason| Error::DerivePpqTick {
                rescaling: scaling,
                difference: UnreachableDifference::at(UnreachableKind::Ppq, track, tick),
                reason,
            })?;
            previous_scaled = scaled;
            event.delta = delta;
        }
    }
    target.header.timing = Timing::Metrical(before);
    Ok(Some(scaling))
}

impl Rescaling {
    fn tick(self, tick: u32) -> std::result::Result<u32, PpqTickRefusal> {
        let factor = u32::from(self.factor);
        if self.before_ppq > self.after_ppq {
            tick.checked_mul(factor).ok_or(PpqTickRefusal::TickOverflow)
        } else if tick % factor == 0 {
            Ok(tick / factor)
        } else {
            Err(PpqTickRefusal::OffGrid)
        }
    }
}
