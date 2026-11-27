# There is one edit language, and its completeness is what it can reach

Every change one Take can undergo to become another is said in one language: the
Edit Set. The language grows by adding mechanical Edits, one kind at a time, each
knowing what it touches. It does not grow by adding a second form that can say
anything. Where a difference between two Takes cannot be said in it, the tool
refuses and names the difference; that refusal is how the language's
completeness is measured.

## The founding case

The first proposal for deriving the Edit Set between two Takes was an exact
replay document: header changes, track order, and whole groups of events at a
Tick replaced by the target's, including their order. It reached every
difference at once. It was rejected on 2026-11-27 in the alignment recorded in
#83, for what it would have cost rather than what it could do.

A document that replaces arbitrary events knows nothing about what a note or a
Controller is, so every pair of Takes is one step apart in it, and the
distinction the charter relies on — a local change against a rewrite, *Edit,
don't regenerate*, *Preserve intent by default* — has no expression in it. It
would state an order within a Tick, which ADR-0008 forbids. It is a second
durable change format, which #47 had declined. And nothing that reads Edit Sets
— combination, withdrawal — could have read it, so it would have been an open
loop no later work closes.

## What the language owes

**Completeness is transitivity on a stated domain.** The domain is every Take
the library can read and write, taken at the event level (ADR-0003) and without
regard to the order of events at one Tick that the Rank rule leaves unordered
(ADR-0008). The language is complete when, for any two Takes in it, some Edit
Set turns the first into the second. Deriving that Edit Set is the constructive
proof, and an unreachable difference is the counterexample, reported.

**Every Edit kind has a bounded, typed reach.** It knows the kind of thing it
changes and leaves everything outside that untouched. A kind that would replace
events it does not understand is the second language under another name, and an
escape hatch that always succeeds becomes the default path.

**Coordinates are not Edits.** A change of units that leaves the music as it was
— an exact rescaling of the Ticks per quarter note — is not something an Edit
does; two Takes differing only by it are compared after rescaling, and a
rescaling that is not exact is refused, because it would round a Tick.

**Reversibility is measured, not promised.** Each new kind is checked for an
inverse in the language — a kind that states something where nothing was needs
one that takes it away — and where none exists its issue says so.

**Choosing among Edit Sets is a judgement.** Many Edit Sets can turn one Take
into another. The default uses only what the files state; reading a deletion and
an addition as one note moved is ADR-0004's kind of judgement and is bounded,
stated and declinable.

## Considered Options

- **An exact replay document** — rejected above.
- **The Edit Set plus a fallback kind for whatever it cannot reach** — rejected:
  the fallback never fails, so it takes over, and the refusal that measured the
  language stops being produced.
- **No derivation; agents write Edit Sets by hand** — rejected: completeness
  would exist only where somebody happened to succeed, and the account of how two
  Takes relate would live in an agent's context rather than in the repository.

## Consequences

Every Edit kind added after this has its priority argued from the refusals
deriving produces on real Takes, and states its inverse. A difference no kind
reaches is reported by kind, track and Tick, never approximated. Order within a
Tick that contradicts the Rank rule and was not carried from the source is the
one place completeness yields to another principle, and is refused.
