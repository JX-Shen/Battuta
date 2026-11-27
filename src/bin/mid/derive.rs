use battuta::{derive::UnreachableDifference, Error};
use serde_json::json;
use std::path::PathBuf;

/// Derive an Edit Set that turns one Take into another.
///
/// Takes must have the same header and track structure. Each track must have
/// exactly one End-of-Track event, as its last event. A note present in
/// both Takes by identity keeps that identity. Differences in its velocity
/// or duration are expressed by changes to those fields. A note present in
/// just one Take is expressed as a deletion or addition. Other differences
/// that the Edit Set cannot reproduce are listed by kind, track and Tick.
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

    /// State the result and any refusal as JSON on stdout.
    #[arg(long)]
    json: bool,
}

pub fn run(args: Args) -> battuta::Result<()> {
    let result = battuta::derive::derive_to_edit_set(&args.before, &args.after, &args.output);
    match &result {
        Ok(()) if args.json => println!("{}", crate::json(&json!({ "output": args.output }))),
        Ok(()) => println!("{}", args.output.display()),
        Err(Error::DeriveInvalidTrackEnds { differences })
        | Err(Error::DeriveUnreachable { differences })
        | Err(Error::DeriveVerificationFailed { differences })
        | Err(Error::DeriveReplayFailed { differences, .. }) => {
            for difference in differences {
                eprintln!("{}", crate::wording::unreachable_difference(difference));
            }
            if args.json {
                print_refusal(differences);
            }
        }
        Err(_) => {}
    }
    result
}

fn print_refusal(differences: &[UnreachableDifference]) {
    println!(
        "{}",
        crate::json(&json!({ "output": null, "refusal": differences }))
    );
}
