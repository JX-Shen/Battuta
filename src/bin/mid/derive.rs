use battuta::{
    derive::{UnreachableDifference, UnreachableKind},
    Error,
};
use serde_json::{json, Value};
use std::path::PathBuf;

/// Derive an Edit Set that turns one Take into another.
///
/// Takes must have the same format and track structure. Each track must have
/// exactly one End-of-Track event, as its last event. When the PPQs differ by
/// an integer factor, the target is read at the source PPQ if every event can
/// be placed there exactly. A note keeps its identity across both Takes if
/// only velocity or duration changes. A note present in just one Take is a
/// deletion or addition by default. A positive tolerance pairs such notes
/// as moves, a judgement based on their start Ticks in the source PPQ.
/// Controller and Program statements are reached at their own addresses.
/// Unreachable differences are listed by kind, track and Tick.
///
/// Before writing, the Edit Set is applied and the resulting Take is
/// re-read to verify the events against the target. On refusal or failed
/// verification the output is left untouched. With --json, the refusal is
/// a list of objects with kind, track and tick, with null for an unavailable
/// location.
#[derive(clap::Args)]
#[command(verbatim_doc_comment)]
pub struct Args {
    /// The Take the Edit Set applies to. Opened read-only.
    before: PathBuf,

    /// The Take the Edit Set must reproduce. Opened read-only.
    after: PathBuf,

    /// Where to write the verified Edit Set. Required, and never an input.
    #[arg(short = 'o', long = "output")]
    output: PathBuf,

    /// Maximum distance between start Ticks for pairing as a move. 0 switches off pairing.
    #[arg(long = "tolerance", value_name = "TICKS", default_value_t = 0)]
    tolerance_ticks: u32,

    /// State the result and any refusal as JSON on stdout.
    #[arg(long)]
    json: bool,
}

pub fn run(args: Args) -> battuta::Result<()> {
    let result = battuta::derive::derive_to_edit_set(
        &args.before,
        &args.after,
        &args.output,
        args.tolerance_ticks,
    );
    eprintln!("tolerance: {} ticks", args.tolerance_ticks);
    let (error, rescaling) = match &result {
        Ok(rescaling) => (None, *rescaling),
        Err(Error::DeriveAfterRescaling { rescaling, source }) => {
            (Some(source.as_ref()), Some(*rescaling))
        }
        Err(error) => (Some(error), None),
    };
    if let Some(scaling) = rescaling {
        eprintln!(
            "rescaled target from {} PPQ to {} PPQ (factor {})",
            scaling.after_ppq, scaling.before_ppq, scaling.factor
        );
    }

    let report = match error {
        None => Some(json!({ "output": args.output })),
        Some(error) => refusal(error).or_else(|| rescaling.map(|_| json!({ "output": null }))),
    };
    if args.json {
        if let Some(mut report) = report {
            report["tolerance_ticks"] = json!(args.tolerance_ticks);
            if let Some(scaling) = rescaling {
                report["rescaling"] = json!(scaling);
            }
            println!("{}", crate::json(&report));
        }
    } else if error.is_none() {
        println!("{}", args.output.display());
    }
    result.map(|_| ())
}

fn refusal(error: &Error) -> Option<Value> {
    match error {
        Error::DeriveInexactPpq {
            before_ppq,
            after_ppq,
        } => {
            let difference = UnreachableDifference {
                kind: UnreachableKind::Ppq,
                track: None,
                tick: None,
            };
            Some(json!({
                "output": null,
                "refusal": [difference],
                "before_ppq": before_ppq,
                "after_ppq": after_ppq
            }))
        }
        Error::DerivePpqTick {
            rescaling,
            difference,
            reason,
        } => {
            eprintln!("{}", crate::wording::unreachable_difference(difference));
            Some(json!({
                "output": null,
                "refusal": [difference],
                "before_ppq": rescaling.before_ppq,
                "after_ppq": rescaling.after_ppq,
                "reason": reason
            }))
        }
        Error::DeriveInvalidTrackEnds { differences }
        | Error::DeriveUnreachable { differences }
        | Error::DeriveVerificationFailed { differences }
        | Error::DeriveReplayFailed { differences, .. } => {
            for difference in differences {
                eprintln!("{}", crate::wording::unreachable_difference(difference));
            }
            Some(json!({ "output": null, "refusal": differences }))
        }
        _ => None,
    }
}
