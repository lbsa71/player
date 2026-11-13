# Constraint-programming implementation plan

Status: implementation underway. The score contract, SCIP adapter and
materialized routing model are in `crates/set-search`. Version-2 duration
recipes can use SCIP to propose routes and direct measurements through the
CLI's existing verification path. The pinned bar and structural cue families
have resumable scans independent of ranked shortlist size.

Date: 12 November 2026.

Scope: replace the authoritative duration-set search with an implicit constraint
model, useful optimistic bounds, and lazy audio verification. Preserve the
analyzer, musical admission rules, renderer, existing library, and per-set
configuration.

Here, **CP** means constraint-based combinatorial optimization. The recommended
first backend is SCIP with an LP relaxation, rather than a requirement to use
OR-Tools CP-SAT specifically.

## 1. Intended result

The app should keep a valid hour or two-hour set available while progressively
finding better complete sets. It should consider track selection, order, entry
cues, exit cues, transition tactics, endpoints, tempo trajectories, and permitted
phrase replays together.

It should avoid most expensive evaluations by proving that entire groups of
choices are impossible or cannot improve the incumbent. It should not need to
materialize every permutation or measure every potential transition.

The user-visible contract is:

1. Find a verified duration-valid set when one is available within the explored
   domain; otherwise retain a clearly provisional draft.
2. Continue improving the **final set objective**, with long, compatible dense
   mashups and sustained handoffs receiving their existing quality credit.
3. Build different surrounding routes when a promising mashup does not fit the
   currently selected entry, exit, or next song.
4. Never discard an unmeasured possibility merely because it is absent from the
   current shortlist or cache.
5. Stop with a meaningful bound or an explicitly reported resource limit.
6. Render only an independently verified complete plan, with timetable, tactics,
   matching evidence, and final search statistics.

This is a search implementation change. It must not quietly weaken vocal,
rhythmic, harmonic, buildup, source-integrity, or renderer checks to achieve a
feasible result.

### Non-goals for the first implementation

- Guarantee fast exact optimization for every library.
- Prove that a mix is subjectively the best possible musical experience.
- Optimize arbitrary continuous cue positions or every conceivable DJ effect.
- Reanalyze the whole library whenever the search strategy changes.
- Introduce a database service, extra JSON sidecars beside WAVs, or a Python
  runtime requirement in the shipped macOS application.
- Implement both SCIP and CP-SAT backends simultaneously.
- Rewrite the audio renderer or alter tactic scoring while benchmarking the
  replacement search.

## 2. Evidence motivating the replacement

The November 2026 hour-set run demonstrates a feasibility improvement, but not
an effective quality search.

| Observation | Consequence |
|---|---|
| All 73 prepared tracks were eligible. | Pool size was not the immediate blocker. |
| The accumulated audit contained 1,311 measured windows; 1,172, or 89.4%, came from the baseline S13 proposal index. | Measurement scheduling remained dominated by feasibility tactics. |
| The audit found three admitted M01 dense mashups, including Perfect Girl → No Boundaries at 16 bars with pair fit 55.83. | Strong long transitions existed; failure to include them was not evidence of absence. |
| The rendered 57:02.4 set contained seven S13 transitions and no M01. | Finding a valid hour did not establish that musical refinement had converged. |
| The final run expanded 123 fixed-order branches, retained 28,532 pending branches, and reported zero objective-bound prunes. | The current bound and search organization did little to eliminate quality-uncompetitive work. |
| The final stop reason was `refinement_budget`. | Neither the order space nor the cue/tactic space was exhausted. |

The pair-fit score 55.83 is **not** the full-set objective. The recorded hour-set
objective was approximately -4.705815. They must never be compared directly.

Reproducible evidence is in:

- [Rendered search result](../../build/beat-stitch/hour-coverage/rendered/search.json)
- [Coverage audit](../../build/beat-stitch/hour-coverage/rendered/coverage-audit.json)
- [Rendered timetable](../../build/beat-stitch/hour-coverage/rendered/hour-set-timetable.md)
- [Earlier coverage diagnosis](../../build/beat-stitch/hour-refinement-audit/README.md)

The [versioned benchmark manifest](tests/fixtures/set-search/hour-coverage-v1.json)
pins the 73 source signatures, recipe, cache namespace, reported score and
artifact hashes. Preserve the local measurement records for warm-facts replay.

### Current structural limitations

`refinement_branches.rs` fixes an entire track order before branching over
measured alternatives for each adjacency. Other orders come from mutation and
regeneration. Exhausting that queue does not cover all possible orders.

The current upper bound grants every unresolved join up to 120 quality-seconds,
perfect future fit, and no other future penalty. It ignores much of the actual
competition for songs, time, compatible cues, and available held music.

The population retention policy parks alternatives. Parking preserves work,
but is not dominance pruning and cannot support an optimality claim.

The existing bound remains valid within its documented scope. Its weakness is
usefulness, not a license to replace it with an unjustified optimistic estimate.
See [REFINEMENT_BOUND.md](REFINEMENT_BOUND.md).

## 3. Established methods and backend decision

Use constraint propagation and branch-and-bound/branch-and-cut over an
optimistic routing model. Use lazy evaluation for expensive musical facts.
SCIP supplies constraint-integer-programming and LP-based optimization
infrastructure; the application supplies its musical domains and validation.
[SCIP overview](https://www.scipopt.org/doc/html/index.php).

The expensive-edge analogy is directly relevant: LazySP studies searching paths
when evaluating an edge dominates runtime. Our problem additionally has unique
tracks, a duration interval, cue compatibility, required adjacencies, replay
choices, and a non-additive final score. Reuse the lazy-evaluation principle,
not an assumption that ordinary shortest-path search solves the whole problem.
[Dellin and Srinivasa, LazySP](https://publications.ri.cmu.edu/a-unifying-formalism-for-shortest-path-problems-with-expensive-edge-evaluations-via-lazy-best-first-search-over-paths-with-edge-selectors).

### Initial backend: SCIP and SoPlex through a narrow Rust adapter

Prototype with the existing Rust interface, `russcip`, before considering a
custom FFI wrapper. Pin a tested wrapper, bindings, SCIP, and LP-solver build;
do not depend on an unpinned branch.
[SCIP Rust interface](https://github.com/scipopt/russcip).

The first integration should use a **solve → inspect → measure → tighten →
re-solve** loop. Expensive audio work stays outside native solver callbacks.
This makes cancellation, source snapshots, cache writes, and proof validity
easier to audit.

Do not change objective coefficients inside an active native search while
retaining its old bounds. Use score variables with fixed coefficients and
supported bound/constraint updates, or end the solve and rebuild. SCIP documents
restrictions on objective changes during branch-and-price.
[SCIP FAQ](https://www.scipopt.org/scip/doc/html/FAQ.php).

### Backend feasibility spike

Before committing to a dependency version:

- Verify Rust 1.89 compatibility with the workspace.
- Verify Apple Silicon, the repository's macOS deployment target, release
  linking, library loading, signing, and distribution packaging.
- Exercise candidate extraction, best-bound reporting, cancellation, time
  limits, repeated solves, and error propagation.
- Record numerical tolerances and whether each returned bound is usable.
- Validate FFI ownership, panic containment, and thread confinement.
- Coordinate native signal handling with the CLI cancellation mechanism.
- Inventory licenses and exact native dependencies of the selected build.
- Ensure the app does not depend on the user's Homebrew installation.
- Prefer reproducible build inputs; explicitly account for bindings-generation
  tools and any build-time downloads.
- Keep the backend feature-gated so the Apple library and future iOS client do
  not automatically acquire an unsupported native dependency.

SCIP installation details belong in this spike, not in assumptions about the
developer's machine. [Official installation documentation](https://www.scipopt.org/doc/html/INSTALL.php).

CP-SAT is the fallback design option if the SCIP spike fails. It has a natural
integer-time formulation, but its constraints require integer coefficients and
the Rust integration needs separate validation. Any score scaling must round
in the direction required by the bound. Do not treat a solution observer as a
general lazy-constraint callback.
[CP-SAT documentation](https://developers.google.com/optimization/cp/cp_solver).

Defer branch-and-price until model size measurements justify it. A pricing
procedure must certify that no omitted improving choice remains; a heuristic
top-k cue generator is not such a certificate.

### Verified backend build

The experimental `scip` feature pins Russcip 0.11.0 and scip-sys 0.1.28.
The source build downloads SCIP Optimization Suite 10.0.2 and uses CMake and
Clang bindings during the build. It links SCIP and SoPlex statically. The
bundled binary evaluated in the spike depended on Homebrew Fortran libraries.

From `Sources/Analyzer`, run:

~~~sh
MACOSX_DEPLOYMENT_TARGET=14.4 cargo test --locked --release -p mixlib-set-search --features scip
~~~

The Apple Silicon release test executable ran directly with the dynamic
library search environment unset. Its load commands declare macOS 14.4 and
list only system libc++ and libSystem. App signing, distribution notices,
and execution on the minimum OS remain rollout checks.

## 4. Invariants that must survive the rewrite

The canonical musical contracts remain
[MIXING_TACTICS.md](MIXING_TACTICS.md),
[SET_STRUCTURE_TACTICS.md](SET_STRUCTURE_TACTICS.md), and
[README.md](README.md).

### Set constraints

- An hour set finishes in 57–60 minutes; a two-hour set in 117–120 minutes.
- Track count is a decision, not a fixed eight- or seventeen-track requirement.
- Use unique recorder songs under the existing identity/deduplication policy.
- Required Spotify-ID pairs are actual direct transitions, in either order by
  default. Preserve support for explicitly ordered pairs.
- Preserve phrase-aware pacing and passage-dependent minimum development.
- Choose genuine musical opening and closing points automatically.
- Include every eligible prepared track in the declared pool; source exclusions
  must be visible and justified.

### Audio constraints

- Vocal ownership is tactic-specific and concerns what is actually audible,
  including supported bleed/effect assumptions.
- Vocal switching must respect supported between-line boundaries.
- Do not truncate an outgoing buildup before its supported release.
- Respect beat/meter confidence, tempo speed and acceleration limits, harmony,
  lane compatibility, stem quality, and measured headroom.
- Preserve actual time maps and processing schedules in immutable plans.
- Keep normally inadmissible/emergency transitions out of ordinary approved
  sets under the existing policy.
- A `nil` join is a zero-point planning placeholder, never an executable
  transition or an approved incumbent.

### Replay constraints

- Repeat or move only supported, measured clear phrase material.
- Preserve the current policy: inserted replay time is at most 15% of **base**
  set duration, subject to the requested final clock.
- Repeated material adds clock time but earns no new development credit or
  additional transition-quality credit.
- Preserve per-region repetition limits and legal insertion/seam rules.
- Replays shift the narrative clock of later material; scoring must account
  for that shift.

### Storage constraints

Original WAVs remain read-only. Each source WAV retains its single fused
`.wav.json` sidecar. Shared search facts, domains, proofs, reports, recipes, and
rendered audio remain in the existing library/workspace locations.

## 5. Define what “all combinations” means

Exactness requires a finite, versioned domain. It does not require eagerly
allocating every member of that domain.

Create a `SearchDomainManifest` containing:

| Dimension | Required definition |
|---|---|
| Pool | Eligible source identities, audio/analysis revisions, overrides, and deduplication policy |
| Cue positions | Permitted beat, bar, phrase, detected event, vocal-gap, and bookend positions |
| Tactics | Enabled tactic IDs and all permitted structural/processing variants |
| Transition endpoints | Allowed endpoint events and bounded durations; not merely the top few ranked endpoints |
| Tempo | Finite supported time-map/ramp templates and their parameter discretization |
| Replays | Finite source regions, destinations, repetition counts, and insertion choices |
| Timing | Output rate, exact-frame semantics, and any conservative interval abstractions |
| Objective | Scoring version, weights, pool-dependent profiles, and tie policy |
| Scope | Full declared domain or an explicitly restricted experimental subset |

An exact claim means exact **within this manifest and its numerical tolerance**.
It does not mean exact over every possible sample-level edit or continuous
tempo curve a human DJ might invent.

CP now writes a versioned search-domain manifest into its search namespace and
includes it in the final report. Its content hash covers the prepared pool's
source and analysis pins, admission and search implementation fingerprints,
recipe semantics, timing, the tactic catalogue and policy, replay, and the
score contract. Time and measurement budgets are outside the manifest identity.
The declaration records that musical realization coverage is incomplete.
Measured conflict checkpoints include the manifest ID, and an admission record
retains its separate acoustic fingerprint.

### Domain coverage contract

Each domain family supports:

- Stable identity and source/policy dependencies.
- Conservative bounds on feasibility-relevant resources and objective terms.
- Enumeration or splitting into child domains.
- A coverage witness: children cover the parent without losing choices.
- A terminal classification: exact choice, proven empty, or still unresolved.
- Cancellation and a persistent cursor.

Prefer disjoint partitions. Where overlap is unavoidable, use stable concrete
choice IDs to prevent duplicate rewards and measurements.

The first implemented family, `pinned-bar-windows/1`, covers reliable
downbeat windows from one to sixteen outgoing bars with the supported
one-to-one, half-time and double-time pulse mappings. It represents the raw
Cartesian cells as ordinal ranges. Splitting gives two disjoint child ranges
with the same pinned source, context and timing identity. Paging accounts
for rejected timing cells and emits valid timing skeletons for measurement.
Duration and speed intervals can prove a whole template impossible. Those
cells count toward coverage and consume no individual timing checks.
For mixed-tempo or mixed-meter templates, the scan also bounds contiguous
incoming intervals at a fixed outgoing cue. It skips an interval when
its most permissive duration and stretch limits fail or its meter set
cannot match the outgoing meter. The tree preserves the original cell
order and clips the skip at the requested range boundary. Each visited
interval consumes one unit of the page work budget. Reports count
row-pruned cells within the existing block-pruned total. An exhaustive
row fixture restarts from every ordinal for all three pulse mappings and
compares the remaining timing choices with a direct cell scan.
The 35-second 73-track run visited 76 cells on one bar pair and used
no row skips there. A timing-only audit took up to four pages each
from all 72 directed pairs among nine tracks near the middle of the
duration distribution. It visited 6,133,299 cells and skipped 552,274
of them through row intervals, with 65 timing windows emitted for
possible measurement. The audit did not run acoustic admission.
Musical admission, discovered endpoints, structural cues, and arrangement
choices have separate coverage requirements.

Every visited interval now carries an ordered disjoint page witness. It
records block timing failures, individual timing failures, pinned-map
failures, and the exact cells emitted as timing windows. A window cell
advances coverage only after the measurement cache has a verified record
for its key. Each page is written as an immutable, content-addressed
record before the pair cursor advances. The cursor points to a chain of
pages back to the root. Resume validates the page chain and its referenced
measurements. A missing page or measurement reopens the pair, including
one previously marked finished. Reports distinguish new scan work from
verified coverage loaded from disk. A measured window can still have
unresolved tactic and endpoint variants.

Each cue turn can consume up to eight pages or empty roots in its
selected family, yielding between pages after 150 ms. Timing rejections
and already durable observations can advance the cursor without taking
a PCM slot. The first window that needs a measurement or a legacy
selector refresh waits for that batch to finish. The scanner checkpoints
the pair's place in the queue at its first committed page and when it
yields to another pair. An empty root is checkpointed when discovered.
This allows the next run to resume those pairs even if the deadline
arrives before a bulk checkpoint. An empty pinned family leaves the
directed pair available to the route solver because other cue and
endpoint families may still contain a join.

In the 73-track, 35-second resume with cue slices, eight directed bar
pairs started. The scan covered 56,470 bar cells, including 7,197
row skips. The run used 12 fresh measurements and 27 cached windows.
It approved three improvements and returned a 15-track set at
58:53.717, with the score rising from −4.7058154 to −4.6551198.
This was a warm resume with other scheduling and cache state in play,
so the score gain cannot be attributed to cue slicing alone.
The app rendered the approved plan at 48 kHz stereo float32. A direct
RIFF check counted all 169,618,425 planned output frames. The renderer
reported −4.31 dBTP and the timetable matched the plan ID and all
15 track rows. Seven of the 14 joins use S13 and one uses M04.

The second timing family, `pinned-structural-windows/2`, uses the same
page witness and measurement ledger. It covers the existing natural
tail/attack templates and the full Cartesian product of detected sparse
windows. It stores distinct source intervals and derives a sparse pair
from its ordinal on demand. The ranked sampler's stride, four-percent
similarity check, and output limit do not restrict this family. Each
cell uses the existing native-clock map builder and its speed, duration,
and renderer checks. Missing signal arrays defer the pair without
reporting it empty. Explicit transition exclusions remain empty.

The structural scanner sorts incoming sparse spans by source duration.
At a fixed outgoing duration, the outgoing speed under the native
mean-duration clock decreases monotonically as incoming duration grows.
Two binary searches locate the span that can satisfy the exact outgoing
speed check. Cells outside it and cells without real source context
are covered by block rejections. Remaining cells go through both speed
checks and native map validation. The cursor clips each rejection to
its requested range and jumps across contiguous rejected cells without
using a map attempt. The profile version changes because the sort
changes ordinal identities. Pair-audio admission keys stay compatible.

The bar and structural scans have independent pair queues, profile
identities, and checkpoints. The structural scan gets one of every four
timing turns while both families have work, with the same measurement
budget and window oracle. A single scheduler owns page rotation, durable
commit, failure deferral, and resume for both. The exhaustive fixture
includes 73 outgoing sparse windows, three distinct incoming spans, and
three natural-tail choices. The scan emits the same 75 feasible timing
windows as exhaustive enumeration and accounts for 147 cells by block
rejection. A separate test exercises both speed boundaries and adjacent
floating-point values.

The version-1 73-track release run committed structural cells `[0, 256)` for
one pair. The resume run attached `[256, 512)` to that same page chain
and reported 512 native timing or map rejections. Neither structural page
emitted a timing window. The two runs processed seven bar timing windows
between them. The changed native timing code opened a new admission
namespace, so the first run began with zero cached admissions. Both runs
ended with drafts. This verifies live scheduling and cursor recovery,
with structural audio admission and full-domain certification still open.

The version-2 73-track run covered 590 cells, including 588 block
rejections and two audio reviews. Resume followed the same page chain
from cell 590 to 592 and reviewed two more windows. All four reviews
completed without an admitted tactic. Both runs ended with drafts.
The admission fingerprint matched version 1, and the run reused its
existing window facts while starting a fresh ordinal scan.

The 73-track resume smoke committed cells `[0, 54)` of a 52,008-cell
pair domain. A repeat with the same short budget stopped before opening
the queued scan. With a 40-second refinement budget the same search
restored the page and added `[54, 65)` and `[65, 69)`. The final report
counted 66 timing rejections and three measured cells, including the
first run's measurement. Tests exercise missing or damaged pages and
measurement records, and confirm that an unwitnessed finished pair is
requeued.

For a fixed measured route, CP assembles the measured joins before applying
any opening-dependent duration or development checks. The opening family
keeps the preferred start, the first audible attack, and each measured
clear accented entry before the first pair's preserved source context.
It uses the same attack and downbeat evidence as the preferred opener and
deduplicates starts at source-frame precision. The closing queue applies
each concrete opening through the renderer's existing bookend transform.
It preserves the measured source cues and recomputes the output clock,
ending window, development, and narrative score for that opening. A
preferred-opening rejection cannot create a route-wide prefix conflict.

The checkpoint records remaining opening frames separately from endings
and replay cursors already associated with a selected opening. On restore,
the selected frame must reappear in the supported options and remaining
opening frames must match a suffix of the ordered candidates. Closing
feedback waits for the opening alternatives before interpreting a missing
duration window or a terminal development rejection. The clock fixture
checks a 56:56 branch and a 57:06 branch at identical transition source
cues. A separate fixture restores unopened candidates and a selected
ending side by side.

For each fixed measured route and opening, the CP closing queue enumerates the
eligible natural tail and clear phrase fades inside a conservative source
window. It sends every resulting plan through the existing composer. Replay
options are measured one placement at a time. Within the supported replay
profile, the clock search retains every validated region and destination,
one region per placement, with zero to three extra cycles. It prunes a
prefix only when it exceeds the exact frame budget or its remaining capacity
cannot reach the minimum. The final composition score is evaluated for each
arrangement. The queue exposes pending work when the time budget stops it.

A read-only library audit checked 17 distinct current pinned first-pair
contexts across 12 tracks. Four contexts offered multiple starts.
Three Multitasker contexts each offered three starts, while one
Tangerine Dream context offered two. The preferred start remained first
in each context.

The 73-track release run reused the previous pair-admission namespace
and started with 33 known good choices. After 64 fresh windows, the
best draft still had three unresolved joins. No fully measured route
reached the new opening queue in that run. The clock and resume fixtures
verify the queue behavior, while the library audit verifies real opening
availability at the sampled cue contexts.

Every legal complete plan must project into the current optimistic master.
An unmaterialized family must retain a bounding representation. Removing that
family because a shortlist is empty would invalidate the global bound.

### Existing heuristics are not domain definitions

The following remain useful for ordering work or constructing incumbents, but
must not establish global infeasibility:

- Top-k baseline/event/generic cue shortlists.
- The six-sweep discovery schedule.
- A fixed number of dynamic continuation proposals.
- Eight attempted musical endings.
- Six retained replay regions per song.
- The replay knapsack's one-second buckets retaining only a cheapest state.
- Beam/population limits and parked candidates.

For exact coverage, enumerate the allowed alternatives, prove an interval
envelope over the omitted alternatives, or declare the proof scope restricted.
Do not silently redefine a heuristic shortlist as the entire musical domain.

## 6. Domain model

### 6.1 Cue ports

A `CuePort` identifies a source position and its relevant boundary state:

- Source identity and exact frame or conservatively represented frame set.
- Local beat/meter/phrase information and uncertainty.
- Entry, exit, event, or bookend role.
- Incoming/outgoing tempo-map boundary conditions.
- Required context for a tactic or passage compatibility check.

A cue descriptor proposes a possibility. It does not grant audio approval.

### 6.2 Passage domains

A `PassageDomain` represents a joint choice of track, entry port, exit port,
tempo trajectory, and permitted local arrangement.

Entry and exit must be linked. Selecting a good incoming transition and a good
outgoing transition independently is insufficient when they leave inadequate
source runway or incompatible tempo boundaries.

Keep source chronology, rendered-duration ranges, audible development ranges,
and applicable replay inventory available to propagation. Do not replace the
passage-dependent development requirement with a blanket 90-second rule.

### 6.3 Transition domains

A `TransitionDomain` binds:

- Outgoing and incoming tracks/ports.
- Tactic and audible lane schedule.
- Hold, resolution, and event endpoint alternatives.
- Tempo-map template and timing basis.
- Effect/source-edit parameters allowed by the tactic.
- Source and analysis pins.
- Verification state and optimistic objective/resource envelope.

Two transitions using the same songs are distinct if their cues, audible
schedule, tempo maps, or endpoint variants differ.

### 6.4 Complete route state

The model must support optional tracks, successor/predecessor choices, a start
and finish, cue/tactic choices, and frame-accurate output timing.

For incremental search or dominance, retain all future-relevant state:

- Current track, cue, tempo/phase boundary, and unresolved passage context.
- Used-song set and remaining available tracks.
- Required-pair satisfaction and pending adjacency obligations.
- Exact elapsed output time or a certified refinement interval.
- Replay decisions, available inventory, and remaining budget.
- Open bookend/transition obligations.
- Scoring accumulators and their denominators.

Track order must be a decision inside the model. It must not be fixed solely
by an external population generator.

## 7. Optimistic master formulation

Use a cue/passages routing formulation with dummy start and finish nodes.
The first implementation can materialize all choices for tiny fixtures, then
introduce domain envelopes for the real library.

Conceptual variables:

| Variable | Meaning |
|---|---|
| `y_i` | Track i is used |
| `x_p` | Passage alternative/domain p is selected |
| `z_e` | Transition alternative/domain e is selected |
| `o_p`, `c_p` | Opening/closing passage choice |
| `t_p` | Output start time or conservative time interval |
| `q_e` | Transition quality-seconds, bounded optimistically until verified |
| `f_e` | Fit shortfall, bounded below |
| `r_k` | Legal replay choice/count |
| `T` | Final output duration |

The integer master must enforce, directly or through valid learned constraints:

1. One start, one finish, and a connected path.
2. At most one passage visit per song; legal internal replays are separate.
3. One predecessor/successor for used interior passages.
4. Compatible incoming/outgoing cue and tempo states.
5. Required direct adjacencies and inclusion constraints.
6. Source chronology, source bounds, and development/pacing obligations.
7. Duration in the requested interval, including all overlaps and replays.
8. Legal bookends and replay placements.
9. Exact validation of every selected transition before publication.

Time accounting must count simultaneous overlap once. When passage durations
include both transition spans, the reference identity is:

~~~
base_duration = sum(selected passage durations) - sum(selected overlaps)
final_duration = base_duration + inserted_replay_duration
~~~

Prove equivalence against actual `chain.rs` assembly, including time maps and
bookends. Do not sum nominal recording durations or pair-preview durations.

### Relaxation versus admission

The LP relaxation may allow fractional choices, disconnected cycles, or
inconsistent concrete cue realizations inside a broad envelope. These enlarge
the feasible set and can therefore support an optimistic bound.

Such a solution is not a playable mix. It must be refined or excluded by a
valid constraint before becoming an incumbent. Connectivity can use flow or
subtour constraints; every published integer route must be connected.

Missing constraints are safe for an upper bound only when their omission
cannot exclude a real completion. Missing choices are not automatically safe.

## 8. Preserve the actual objective

Initially optimize the current complete-set objective without retuning its
weights. Extract a versioned score contract shared by the model, validator,
reports, and tests.

For a complete plan, define:

- `T`: final output seconds.
- `Q`: raw dense-mashup plus sustained-handoff quality-seconds.
- `P`: pacing penalty in quality-seconds.
- `F = sum(max(25 - transition_fit, 0))`.
- `E`: accumulated time-weighted narrative error.
- `S`: scored solo-plus-replay seconds.
- `D`: summed development-shortage penalty.
- `n`: selected track count.
- `r`: inserted replay seconds.

The current evaluator computes:

~~~
J = 60 * (Q - P - F) / T
    - 8 * E / max(S, 1)
    - 2 * D / n
    - 2 * r / T
~~~

Preserve the evaluator's denominator guards and exact treatment of incomplete
plans; only the complete-plan equation authorizes incumbent comparisons.

Important details:

- `Q - P - F` and the final objective can be negative.
- Pair-fit score, provisional draft rank, and final set score are different.
- Current quality credit comes from M01, S05, and S06. A long transition from
  another family does not automatically earn sustained-quality credit.
- S13 has zero sustained-quality credit and fixed fit 5 under the current rules.
- Narrative target positions use output time divided by the requested maximum
  set duration, not the ultimately selected final duration.
- Replays change later narrative positions and scored time, but do not invent
  new development or transition quality.
- A complete-set optimum is an optimum of this heuristic musical objective.
  Listening quality still needs separate evaluation.

Any later change to these preferences is a separately versioned policy change,
with refreshed benchmarks and bounds.

## 9. Layered early pruning

Apply inexpensive, high-impact reasoning first. Run a more expensive relaxation
only when earlier layers cannot dismiss a domain. Constraint propagation and
ordered propagation priorities are standard solver mechanisms.
[SCIP propagators](https://scipopt.org/scip/doc/html/PROP.php).

### Layer A: exact structural contradictions

Examples:

- Repeated song or conflicting required adjacencies.
- Cue outside the source or exit preceding the retained entry.
- Empty intersection of tempo-map boundary conditions.
- No possible predecessor/successor for a mandatory track.
- A source or correction revision invalidating a pinned choice.

### Layer B: resource and reachability propagation

- Propagate earliest/latest feasible source and output positions.
- Compute conservative minimum/maximum reachable final duration.
- Include only reachable unused-track capacity in upper-capacity calculations.
- Include an optimistic legal replay allowance; preserve `r <= 0.15 * base`.
- Propagate mandatory-pair requirements and potential closing reachability.
- Remove a choice only if every remaining realization violates a necessary rule.
- Iterate until a fixed point or a configured propagation budget.

Interval overlap with the target means “not ruled out,” not “feasible.”
An empty current measured-edge graph is not evidence that an unmeasured
connection cannot exist.

### Layer C: cheap score bounds

Replace the universal 120-second allowance where stronger bounds are proven.
Examples include maximum possible held output duration and the credit rules of
the particular tactic family.

Use a hold-duration quality ceiling only after proving from the scoring code
that all multiplying factors are bounded appropriately. Maintain a per-tactic
bound provider and fail validation if a measured value exceeds its bound.
Keep the coarse 120-second ceiling where no tighter proof exists.

Statistical niceness predictions, confidence scores, average segment quality,
and likelihood of admission may prioritize work. They must not serve as
certified upper bounds without a proof.

### Layer D: assignment/flow relaxation

Solve a relaxation that retains competition for songs, incoming/outgoing
choices, required pairs, duration capacity, and cue-family reward ceilings.
Initially relax difficult connectivity and nonlinear musical costs.

This prevents the bound from simultaneously claiming several incompatible
rewards from the same song or granting every remaining boundary an impossible
two-minute mashup.

### Layer E: expensive musical verification

Measure only choices that matter to competitive optimistic routes or to
tightening the dominant unresolved bound. Cache the result and propagate it
across all dependent branches.

Unknown evidence, missing optional analysis, cancellation, numerical failures,
and worker errors remain distinct from a proven musical rejection.

## 10. Sound upper bounds for the normalized score

### 10.1 Cheap interval bound

For a domain, obtain sound bounds:

~~~
Q <= Q_upper
P + F >= cost_lower
r >= replay_lower
E / max(S, 1) >= arc_lower
D / n >= dwell_lower
T_min <= T <= T_max
~~~

Let:

~~~
numerator_upper = 60 * (Q_upper - cost_lower) - 2 * replay_lower
~~~

Then:

~~~
if numerator_upper >= 0:
    U = numerator_upper / T_min
else:
    U = numerator_upper / T_max

U -= 8 * arc_lower + 2 * dwell_lower
~~~

The sign-dependent denominator is mandatory. Dividing a negative numerator by
the minimum duration can incorrectly prune a superior longer completion.
Use zero for penalty lower bounds that have not been proved.

CP applies this bound to an assembled route with measured joins. Its quality,
fit shortfall, and pacing charges remain fixed through closing and replay.
A fixed ending also gives the maximum clock reachable with the replay budget
and the minimum replay time required by the duration window. CP can suspend
that work before measuring an ending or preparing loops. Suspended jobs stay
in the checkpoint and are compared with the restored incumbent on resume.
These bounds cover the exact joins in each job. The global CP bound uses the
separate coverage accounting below.

### 10.2 Incumbent-surplus relaxation

For a verified incumbent score `L`, maximize the optimistic surplus:

~~~
G(L) = max [
    60 * (Q - P - F)
    - 2 * r
    - (L + 8 * arc_lower + 2 * dwell_lower) * T
]
~~~

over the relaxed remaining domain.

If the solver's valid **upper bound** on `G(L)` is nonpositive, no completion in
that domain can improve `L`. This handles negative incumbents and correlations
between duration and reward without pretending every set has the same length.

For a nonnegative certified surplus bound `g`, a conservative objective bound is
`L + g / T_min`. For negative `g`, using `L` itself is conservative and already
sufficient to rule out strict improvement. Take the minimum with any other
valid bound on the same domain.

Use the solver's best bound, not the objective of its best optimistic candidate.
A candidate is evidence of an achievable value in the relaxed model, not an
upper bound on all alternatives.

For nonempty measured chains, CP also computes a native-independent
assignment bound. A route with `k` joins uses `k + 1` distinct songs. The
largest `k + 1` track capacities bound its output clock, and buckets whose
capacity falls short of the minimum clock are empty. In each remaining
bucket, the best `k` outgoing owners and the best `k` incoming owners
independently bound signed reward minus fit shortfall. The tighter sum and
the bucket's clock interval bound both the objective and its surplus over
the verified incumbent. Pacing, replay cost, and narrative and development
penalties are conservatively relaxed.

The capacity calculation checks the first outgoing and last incoming
anchors against the join geometry used by assembly. For each song it uses
the slower supported boundary speed, its entire source length, the
largest outgoing overlap, output-frame rounding allowance, and the
replay fraction. Bridge fallback maps preserve the same solo endpoints.
The envelope falls back to the existing credit-only bound if any input
lacks the required geometry or falls outside its numerical range.

Before scoring the measured assignment, CP propagates duration and
required membership over the directed graph of admitted joins. The
optimistic clock for a track includes its reachable predecessors and
successors. A selected join can draw time only from tracks that reach its
departure or are reachable from its arrival. The assignment relaxation
uses the surviving joins and capacities. A duration-empty measured
region skips the native measured-only turn even before the first
incumbent. Reports count those skips separately from score-bound skips.
Mixed proposals retain their unresolved bridges and their own ceiling.

The four-track fixture gives two outgoing and two incoming owners more
than an hour of aggregate capacity. Each possible path initially uses
only two songs, so the measured-only turn is skipped before invoking SCIP.
Adding a measured bridge creates a three-song path and reopens the
measured region. The unresolved version of the bridge was already
available in the mixed model.

The 73-track warm resume retained 27 tracks and 57 pairs in the
measured-only clock relaxation. It excluded no measured pair and
skipped no native turns on duration. The mixed graph retained all
5,256 directed pairs. The measured and global ceilings stayed at
0.6333208 and 56.9721026, and the approved 58:53.717 set retained its
score of −4.6551198.

### 10.3 Tighten unavoidable penalties carefully

Ignoring narrative and development penalties can leave the relaxation too
optimistic. Add certified lower bounds after the basic model passes tests:

- Minimum narrative error over the permitted output-time and tempo intervals.
- Unavoidable development shortage given maximum remaining original material.
- Pacing costs forced by committed phrase boundaries.
- Mandatory low-fit transitions where no alternative family remains.

For ratio terms, use a lower numerator bound and a safe upper denominator bound.
Account for all still-permitted earlier replay insertions before treating a
passage's narrative cost as immutable.

The global clock bound now includes a floor on preferred-development
shortage. For each component and track-count bucket it includes the
required songs and the smallest remaining preferences
`min(150 seconds, 0.8 × source duration)`. Each finished track's actual
preference is at least that floor. Its normalized shortage is at least
the unmet part of the floor divided by 150 seconds. Solo intervals
are disjoint, and a transition can credit at most half its hold to
each adjacent song. Replay does not earn development credit. The sum
of the tracks' development time is therefore bounded by the set's
maximum output clock. The square of the mean normalized shortage
bounds the mean of its squares from below, including when clock is
redistributed optimistically across the songs. The scorer's development
multiplier is at least one.

The arithmetic rounds required and optional preference sums down and
allows an additional `1e-9` in the squared-shortage mean before applying
the score weight. This allowance exceeds the error from 128 additions
at the two-hour clock's scale and the normalized shortage calculations.
The tiny fixture enumerates unique-song orders with mandatory tracks and
sweeps feasible clocks, overlap allocations, replay, and preferences
above their floors. The bound covers joins that still need measurement
because it uses the same development-credit rules for every admitted
tactic.

The 73-track warm run lowered the global ceiling from 56.972103 to
56.970842. The verified 58:53.717 incumbent and its −4.655120 score
were unchanged. The clock still allows hypothetical joins to receive
the full admission credit, so the improvement is small on this pool.

### 10.4 Numerical contract

- Keep timing in exact output frames at validation boundaries.
- Round optimistic rewards and capacities outward, and unavoidable costs
  inward, when constructing approximate coefficients.
- Use bounded, derived big-M values where necessary.
- Record coefficient scaling, tolerances, and solver versions.
- Validate returned dual/best bounds before using them for irreversible pruning.
- Account for coefficient conversion and numerical residual error; a hard-coded
  tiny epsilon is not a general proof of numerical safety.
- Fall back to a weaker established bound when a stronger bound is unavailable
  or numerically suspect.
- Compare an outward upper bound with a conservatively scored incumbent and
  the requested absolute tolerance.

The measured assignment coefficients use a binary scale of `2^20` with
upward credit and downward shortfall rounding. The bounded scorer
accumulators and exact integer arithmetic give an additional four units
of allowance for accumulation and numerator rounding. The capacity
calculation uses outward sums and eight output frames per song. With a
source span of at most 24 hours and boundary speeds in `[0.5, 2]`, the
source-to-output conversion of a solo interval stays below `2^33` frames
at 48 kHz.
The allowance covers frame rounding in the three bridge intervals and
bookends, as well as floating conversion. Neither bound consumes a
native dual value.

The default certificate is within the declared numerical tolerance. Do not
claim bit-exact or independently verified mathematical optimality unless a
separate certificate path actually establishes it.

### 10.5 Global bound accounting

A native bound applies to the model and restrictions actually solved. Record
that scope with the bound. It is not automatically an upper bound for the
original full domain after incumbent cutoffs, closed assignments, or external
partitions have removed choices.

Maintain a coverage ledger spanning active, deferred, in-flight,
unmaterialized, and closed regions. The global objective upper bound is the
maximum of:

- The verified incumbent score, if present.
- Valid upper bounds for every remaining region, including work outside the
  current native model.
- Retained ceilings for regions closed within an absolute tolerance that may
  still contain a score above the current incumbent.

Exactly evaluated regions below the incumbent and strictly improvement-pruned
regions are already covered by the incumbent. Proven infeasible regions need
no objective contribution. If any other region lacks a usable bound, report
the global bound as unknown until a conservative fallback covers it.
Take the minimum of alternative bound estimates only when they cover the same
region and source/domain/objective snapshot.

In particular, never publish a remaining-model upper bound below the retained
incumbent as a full-domain bound. Compute the reported gap from the global
ledger, not merely from the latest native solve.

The current ledger splits new candidates into routes using only measured joins
and routes containing at least one unmeasured join. For measured routes it
relaxes the path to one outgoing choice per song, and separately to one incoming
choice per song, taking the tighter ceiling for each sustained-credit lane.
It allows all compatible bookends and replays and relaxes composition penalties.
If that measured ceiling cannot improve a verified incumbent, CP skips a
measured-only native turn. Mixed turns still search unmeasured alternatives.

For the unmeasured complement, each future admitted join can contribute up to
the continuation credit cap with the current numerical admission allowance.
Unique-song routes can contain at most the pool size minus one joins. The
ceiling rounds the recorded credit comparisons and two-lane accumulation
outward. Admission also checks that the measured record and the plan metadata
used by composition agree. The global reported bound takes the maximum of the
two ceilings and the verified incumbent. The exhaustive four-song fixture
checks the measured relaxation independently. Separate fixtures cover an
unknown region and a retained ceiling above the incumbent.

The unmeasured complement also has a clock-coupled ceiling. Duration search
validates that each join's recorded sustained credit fits its actual output
overlap, with a small admission allowance for rounding. The renderer's plan
validation ensures overlaps are disjoint within the base clock. Replays can
lengthen the output and do not add transition credit. Therefore total sustained
credit can grow at most with the output clock plus the accumulated numerical
allowance. The ceiling couples this credit to the denominator in the score and
takes the tighter result of this clock calculation and the per-join cap.

The paced clock bound also reserves original solo time for each internal
song. Every nonzero arrival admitted by phrase pacing has either a complete
inferred phrase or at least 45 seconds of source runway. The shortest phrase
allows one source frame of slack at each end. The joined map preserves the
paced overlap anchors. Its endpoint speeds bound the source-to-output
conversion, with outward allowance for map validation and the three rounded
bridge intervals. Bookends can change the first and last solo. Replays add
output time while preserving the original solos.

For a route with `k` joins, the `k - 1` shortest internal runway floors in
the pool give a lower bound on output outside the overlaps. Each bucket
takes the tighter of the per-join credit cap and the available overlap clock.
The largest bucket ceiling covers the unmeasured complement. Cached edges
must agree with the assembly anchors, and unsupported timing evidence falls
back to the original disjoint-clock ceiling. The independent fixture enumerates
all orders and lengths in a five-song pool and compares scored allocations
against the bound. The CLI fixture also checks the fallback when a cached cue
disagrees by one frame.

The paced ceiling now considers each weakly connected component of the graph
of permitted directed transitions. A route stays inside one component. Each
component contributes its own maximum join count and shortest internal
runways. The ceiling takes the largest component result. Explicit transition
corrections and necessary required-adjacency rules determine which edges the
graph excludes. Unmeasured edges and pairs with empty scans for individual
cue families stay in the graph. An edge in either direction is enough to
connect two components for this bound.

The same directed constraint filters measured joins, unresolved SCIP choices,
cue scans, and ranked proposals. The manifest lists pinned correction
exclusions. The mock tests enumerate the permitted paths and verify the bound
with and without a one-way bridge. A separate CLI fixture loads directional
corrections through the catalog and checks the native model's choices and the
reported ceiling.

Required Spotify pairs now constrain the same relaxation. A candidate
component must contain every required song, and a join-count bucket must
have enough slots to include them. A song forbidden at both bookends
contributes its own runway floor before the relaxation selects the shortest
remaining internal runways. In either-order mode this includes a song with
two required neighbors. Membership-only requirements still constrain the
component and track count. All contributions are rounded down before being
used to bound the overlap clock.

The mock exhaustively enumerates orders of an either-direction required
chain with a long middle-track runway and checks the ceiling at every
sampled duration. A separate test checks the required membership and
internal-track masks under ordered, either-order, and membership-only rules.

The clock now also gives a minimum song count before any pair is measured.
Each source contributes at most its entire duration at the slowest
permitted speed plus the full 15% replay allowance. Counting all of that
time ignores trims and counts simultaneous audio twice, which can only
raise the capacity. The computation pads the renderer's speed tolerance
and rounds upwards. The relaxation includes all required songs first, then
adds the longest available optional capacities. The proposal model uses the
pool-wide minimum song count, and the paced score ceiling repeats the
capacity test within each permitted transition component.

The mock enumerates all track subsets containing the required song and
checks their feasible clocks against the tightened ceiling. A CLI fixture
checks that the unresolved SCIP model requires four songs for a 57-minute
minimum when each source is twelve minutes long, with zero audio measurements.

The 73-track resume established a six-song minimum. The five longest songs
could supply only 55:28.787 even under the optimistic clock. The run kept
the verified 57:02.420 incumbent. The global ceiling remained 56.9721026
because its highest-scoring relaxed bucket already had enough songs.

The directed-capacity check now tests every track against its reachable
predecessors and successors in the permitted graph. A route containing
that track must find all required songs and enough optimistic duration
within that group. Removing a track can break a path that another track
relied on, so the check repeats until the retained set stabilizes. The
SCIP model fixes excluded track variables to zero. The paced ceiling also
removes their incident edges. Unmeasured joins stay eligible whenever the
pair is permitted. The capacity test relaxes the order of the other
tracks, so passing it leaves their musical and clock choices unresolved.

The same propagation now tests immediate joins. Before A → B, all songs
must be able to reach A. After it, all songs must be reachable from B.
The union includes A and B and counts each song once. If this union lacks
a required song or enough optimistic duration, the join is excluded.
Removed joins also change reachability on the next pass. The solver can
rule out a shortcut while retaining both endpoints through another song.
The score ceiling and cue schedulers use the surviving pairs. SCIP omits
unresolved variables for excluded pairs and fixes their measured choices
to zero. The report distinguishes retained and excluded directed pairs.
The unrestricted 73-track resume retained all 5,256 pairs and the verified
57:02.420 set. The shortcut fixtures exercise the join-specific exclusions.

The tests enumerate all 4,096 four-track directed graphs with three sets of
required songs. Each route that meets the optimistic clock retains all
its tracks and joins. A native-model fixture also rejects a directed fork with
enough aggregate source duration but no single route long enough, with
zero window measurements.

The CP run computes the proof once for its pinned pool and recipe. Bar and
structural scans, ranked probes, continuations, archived-route revalidation,
and PCM dispatch use that proof to skip work involving excluded tracks.
Cached acoustic evidence remains reusable for other recipes. A skipped
window does not create a negative measurement or advance timing coverage.
The three-track fixture restores an excluded shortcut from a saved queue and
verifies that both scanners omit it. It also submits a direct measurement
request and verifies that no worker starts and no cache record is written.
The 73-track scheduling smoke retained all 5,256 directed pairs and the
57:02.420 incumbent. It measured six windows and skipped zero on capacity,
as expected for the unrestricted pool.

The 73-track unrestricted resume retained all 73 tracks after one pass,
reported the same six-song minimum, and kept the verified 57:02.420
incumbent. This library has no explicit transition exclusions, so the
directed check leaves the global ceiling unchanged at 56.9721026.

On the 73-track library, requiring Lady Moon ↔ While The Light Came and
While The Light Came ↔ Primordial Temples reserved three song slots and
one internal runway. The ceiling was 56.9466546, compared with 56.9721026
without those requirements. The 35-second search returned a verified
16-track set of 57:28.951 satisfying both adjacencies.

The 73-track resume had zero explicit correction exclusions. Its global
ceiling remained 56.9721026, and it retained the approved 57:02.420 set at
−4.7058. The disconnected mock exercises the tighter bound.

The 73-track paced-clock run restored the 57:02.420 incumbent at −4.7058
and loaded 119 previously admitted choices. It reduced the global ceiling
from 60.0000013 to 56.9721026 and the gap upper bound to 61.6779179. The
measured-route ceiling was 0.6333208. The run used one fresh measurement.
The cache bridge is limited to the audited admission fingerprints and
toolchain because this change adds a read-only pacing query.

The original 73-track coverage smoke reported 0.5380 for measured routes and
151.5789 for the unmeasured complement. The disjoint-clock rerun admitted all
883 cached choices, retained the approved −5.0740 set, and reduced the global
ceiling to 60.0000013, with a certified gap upper bound of 65.0740. Tests cover
the hour and two-hour clocks, both output sample rates, replay, and the full
join-count range. The unmeasured complement still dominates. The current
partition does not yet supply a useful global quality cutoff, a full-domain
incumbent-surplus assignment certificate, or a proof of family exhaustion.

The signed assignment smoke run covered all 883 measured choices in the
73-track pool. It ruled out measured chains with fewer than five joins on
clock capacity alone. The combined measured score ceiling remained
0.5380 and the incumbent remained −5.0551, so it skipped no measured-only
turn on score in this run. The unmeasured ceiling remained 60.0000013.
The independent exhaustive fixture and the CLI scheduling test verify
that the new bound can skip measured work with a negative incumbent
while leaving mixed turns available.

With a fixed ending, closing bounds the narrative score of all legal
replays using the original solo samples. Each original midpoint can
move by up to the full replay allowance, including the scorer's frame
rounding. The minimum error for that sample is its distance from the
range of the narrative target over that interval. Repeated samples add
nonnegative error. The denominator is bounded by the maximum output
clock with floating-point accumulation headroom. The development
penalty is invariant because replays preserve original solos and their
overlap credits. The closing queue computes this bound once per ending,
recomputes it on restore, and reports suspensions attributable to the
passage penalties. Fixtures compare it with the exact composer across
four narrative shapes and verify suspension with resumable work.

Closing also applies a score ceiling before enumerating endings when the
opening and all joins are fixed. The last song's available source frames
cap the rounded output clock and establish how much replay the shortest
accepted set would need. The original solos through the penultimate song
already contribute unavoidable narrative and development penalties. The
last solo stays outside that estimate until its ending is selected. The
queue records when this tighter ceiling lets it suspend an opening and
retains the opening cursor for resume.

## 11. Lazy audio oracle and conflict learning

### Oracle outcomes

Use typed results rather than a nullable plan or a generic rejection string:

| Outcome | Meaning | Search effect |
|---|---|---|
| `VerifiedTransition` | Exact cue, tactic, schedule, and time maps passed admission | Tighten the exact choice; retain evidence |
| `RejectedChoice` | This exact realization failed a hard musical check | Exclude that realization |
| `RejectedContext` | A combination of adjacent choices or route state is impossible | Add a scoped contextual constraint |
| `NeedsRefinement` | A coarse envelope still contains unresolved realizations | Split or enumerate it |
| `Unknown` | Insufficient evidence or an incomplete heuristic attempt | Preserve possibility; schedule further work |
| `Cancelled/Error` | Execution did not establish a musical fact | Preserve work and report the execution outcome |

### Scope of learned constraints

- Failed stereo vocals do not reject a vocal-isolated tactic automatically.
- Failure at one endpoint does not reject every endpoint for the pair.
- A late entry conflicting with a later exit rejects that combination, not
  either track globally.
- A renderer/source failure must preserve its precise source/version scope.
- A family-wide rejection requires evidence covering every member of the
  family.
- A failed heuristic closing or replay search is `NeedsRefinement` or `Unknown`,
  not a proof that no closing or replay arrangement exists.

Keep a dependency index so one new fact can invalidate or tighten all affected
domains without remeasuring audio.

The window oracle reconstructs typed observations from checksum-verified
measurements, including older cache records. A completed selector review
can be memoized as having no admitted choice or discovered continuation.
It does not exclude other realizations. A missing selector diagnostic,
unavailable timing or tactic facts, or an omitted ranked realization
leaves the exact window unknown. Recorded verified variants remain
available independently. Reports separate unknown observations,
reviewed windows, and execution failures, and the bar ledger counts a
durably observed unknown window separately from a reviewed window.

New observations also record `pinned-window-selector/1`. For an exact
timing window it partitions the selector into 37 standard primary tactics,
four detected swift variants, and the M01 rhythmic variant. Modifiers are
part of the generated schedule. Each scheduled variant is linked to the
canonical hash of the exact executable schedule. Reload verifies the full
partition and matches the hashes against recorded edges. A missing schedule
stays unresolved even when another variant of the same tactic was recorded.
Legacy cached measurements still supply their admitted edges and carry no
selector partition claim.

The table records fixed-policy, renderer capability, timing basis, and
bar-count blockers, as well as a measured headroom failure. Missing exit
evidence and ambiguous eligibility or schedule failures remain unresolved.
Bar and structural pages count partitioned and resolved selector windows
separately from timing observations. A resolved selector partition is scoped
to the current deterministic modes and evidence for that exact window. Other
endpoint placements, processing choices and timing families retain their own
coverage requirements.

The 73-track run recorded partitions for all eight freshly measured windows.
Of their 336 selector branches, 127 were ruled out by policy or bar count,
four produced last-resort schedules, and 205 remained unresolved. Seven
windows were too short for any M01 variant. The remaining M01 standard
review was unresolved, and its swift and rhythmic modes lacked exit evidence.
The compatible cache supplied 119 admitted choices, including the joins for
the 57:02.420 incumbent. The eight-window run found no improved set.
A separate resume loaded the same eight partitions and their matching outcome
counts from disk with zero new measurements.

When CP encounters an exact legacy window that has a selector diagnostic
without a partition, it can spend one refresh slot per measurement batch.
The timing scan gets the slot before ordinary cached proposals. It rebuilds
the same pinned window and runs the current selector. Refresh attempts count
toward the normal measurement allowance. The refreshed observation is stored
separately under the same window key, so a refresh that encounters missing
facts preserves the original admitted joins and continuations. A completed
observation with missing evidence remains unknown and avoids repeated refresh
under the same pins. Execution failures leave the refresh pending for resume.

Timing pages advance once a window observation and any required refresh are
durable. Resume verifies the cached evidence. Losing the refresh can reopen
the timing cursor while the original measurement still supplies usable joins.
The report counts refresh attempts, completed refreshes with and without
partitions, and eligible legacy reviews still pending.

On the 73-track pool the next eight-measurement run spent seven slots on
legacy reviews and one on a new window. All eight produced selector
partitions, raising the total from eight to sixteen. The timing pages
linked partitions for five bar windows and two structural windows.
The run loaded 119 admitted choices, retained the approved 57:02.420 set,
and left 171 eligible legacy reviews pending.
An optimized resume reloaded all seven refreshes and sixteen partitions
with zero window measurements in 16.4 seconds of reported elapsed time.

The `pinned-window-selector/2` profile splits the M01 rhythmic branch by
each measured incoming drum accent in the final half-bar. Each pulse has
its own exact start frame and independently measured hold profile. The
receipt declares the detected frames, then accounts for every one, including
an unresolved row when the schedule lacks enough evidence. The ordinary
resolution and accompaniment-based swift branch remain available. The
accompaniment detector still selects one valid pause or release and now
skips a pause before the buildup release while scanning for a later one.
Earlier receipts can supply admitted schedules and are eligible for a
budgeted refresh under the new profile.

The detector fixture supplies two qualifying pulses. The selector fixture
makes the early pulse fail headroom and verifies the exact schedule for the
later pulse. The receipt fixture rejects a partition that drops one detected
frame and checks that a prior-profile receipt remains eligible for refresh.

The read-only optimized audit reopened 13 cached windows with pinned source
identities. Nine exposed two rhythmic exit pulses, giving 22 independent M01
decisions in total. All remained unresolved under the current musical
admission gates. The audit took 20.7 seconds after compilation. To repeat it,
set `MIXLIB_AUDIT_LIBRARY` and `MIXLIB_AUDIT_WINDOWS` to the Analyzer library
and a path-separated list of its cached window records, then run from
`Sources/Analyzer`:

```sh
cargo test --release -p mixlib-cli --bin mixlib --features cp-search \
  cached_windows_expose_all_detected_rhythmic_exits -- --ignored --nocapture
```

The `pinned-window-selector/3` profile expands the accompaniment branch.
The detector now retains every qualifying start on the 20 ms source PCM
grid in the final half-bar, mapped onto the exact output clock. Each pause
or completed release gets its own selected-lane evidence and admission row
for M01, M02, S05, and S06. Candidate profiling checks cancellation between
exit frames. The receipt lists both detected frame sets and rejects missing,
duplicate, or mismatched rows. Prior selector receipts remain refreshable.

The detector fixture checks six qualifying starts across two pauses and
keeps all three starts after a late buildup release. The selector fixture
admits a later M01 schedule independently of the earlier unresolved one.
The receipt fixture drops one of eight accompaniment rows and verifies
that the coverage check detects the omission.

The read-only audit found six qualifying positions across three cached
windows, with one, three, and two positions respectively. Their 24 tactic
rows yielded three bar-count rejections and 21 unresolved outcomes. The
optimized audit took 7.0 seconds after compilation. Run the same audit
command with `cached_windows_expose_all_detected_accompaniment_exits` and
cached windows with accompaniment exit evidence to reproduce it.

The `pinned-window-selector/4` receipts distinguish measured failures from
missing evidence at the eligibility gate that made the decision. They record
measured grid mismatch, buildup interruption, isolation failure, tonal
conflict, hold incompatibility, and insufficient filter crowding alongside
the existing peak and structural rejections. A cut with an unknown landing
stays open when a later landing could still respect the buildup release.
Detailed rejection records identify the tactic, selector mode, and exact
exit frame. Earlier receipts still supply admitted schedules and remain
eligible for refresh under the new profile.

The regression fixtures remove individual measurements to check that an
unknown fact does not certify a failure. They also check that a buildup
blocks an early accompaniment exit while the later schedule stays admitted.

Rerunning both pinned audits resolved 35 of the 43 previously unresolved
decisions. The accompaniment audit ruled out 18 on measured isolation
and left three open. The rhythmic audit ruled out ten on hold compatibility,
four on isolation, and three on tonal conflict. Five rhythmic decisions
remained open. Three accompaniment bar-count rejections were already
certified under the previous profile.

The `pinned-window-selector/5` receipts carry completion flags for both
exit detectors. A completed scan with no qualifying candidate closes that
detector branch on its declared grid. Missing stems, unusable mappings, or
insufficient reference evidence leave it open. An incomplete scan retains
its measured candidate rows and an explicit unresolved remainder. The
receipt verifier requires the remainder and checks any absence claim against
the matching completion flag and empty frame list.

Timing pages distinguish a processed window from an exhausted selector.
A pinned timing domain is proven empty only when its scan is complete and
every valid window has a resolved selector partition with no admitted
choice. These proofs apply to the pinned timing family and its current
selector profile. Other cue families and endpoint continuations still
have their own coverage obligations. Fixtures verify both an empty domain
and a processed window that stays open after reload.

For a fast filter check, run the selector receipt and timing-page fixtures
from `Sources/Analyzer`:

```sh
cargo test -p mixlib-cli --bin mixlib --features cp-search -- \
  duration_search::oracle::tests:: cp_search::bar_coverage::tests::
```

The six fixtures took 0.38 seconds with a warm build and use no library audio.
They check that a missing candidate row, an incomplete detector, or an
unavailable receipt cannot close a branch. The timing-page fixture also
checks that a fully accounted empty family stays closed after reload.

A bounded run over all 73 tracks reached its 12-window measurement budget in
51.8 seconds. It recorded 31 detector absence decisions, six unresolved
detector remainders, and 151 unresolved eligibility decisions. Each of the 12
typed window partitions still had at least one unresolved realization, so
the run proved zero windows empty. It also reported 177 older reviews pending
refresh. The fixture supplies every fact needed for closure. In the measured
sample, eligibility was the largest unresolved category, and remeasuring the
exit detectors alone would leave those decisions open.

The saved eligibility diagnostics explain the missing headroom measurements.
Of the 137 affected standard realizations, 89 stopped at an unverified
between-lines gap, 34 lacked vocal evidence for the schedule, 11 lacked
required shape evidence, and three stopped at a stem leakage check. These
profiling failures can also leave tonal and headroom estimates absent for
the same realization.

The `pinned-window-selector/6` profile records existing bar-count and
fixed-clock exclusions, plus measured landing mismatches, inactive bass,
and affirmative vocal absence for vocal-only tactics. The underlying
admission predicates stay the same. On the same 12 saved windows, bar
count alone rules out ten of the 151 previously unresolved decisions.
The search report now counts eligibility reasons only on matching
unresolved rows. One realization can contribute several reasons. The
fixtures check that reasons from another exit frame or an already resolved
row do not affect these counts.

The next 73-track run measured 12 windows in 55.3 seconds and reached its
measurement budget. Eight windows matched the earlier sample by exact
cache key. In those eight, 17 previously unresolved rows became terminal
rejections: six for bar count, five for fixed clock, and six for inactive
bass. All other rows in the matched sample agreed. The full run still had
at least one unresolved realization in each of its 12 reviewed windows.
The most frequent remaining reasons were missing supported phrase
boundaries (109 rows) and unknown or insufficient standard headroom (102).
The approved 57:02.420 incumbent retained its score of −4.7058.

The `pinned-window-selector/7` profile applies the timing-basis, bar-count,
and fixed-clock rules before building a selected-lane schedule for profiling.
Each skipped profile records the geometry rejection. Cut-band PCM checks run
only for landings whose selected-lane schedule produced a headroom estimate.
The estimate is a prerequisite for admission, so skipping the PCM check does
not resolve a missing headroom fact. Only the outgoing accompaniment is
scanned for swift exits, because the selector has no incoming-accompaniment
exit mode. A skipped scan does not supply a detector absence claim.

The fast fixture compares the profiles of two viable tactics with direct
profiling and confirms that the ineligible short fade is skipped. To check
recomputed schedules against prior admission on real data, set
`MIXLIB_AUDIT_LIBRARY` and `MIXLIB_AUDIT_WINDOWS` to the library and a
path-separated list of cached window records with admitted schedules. Then
run the read-only audit from `Sources/Analyzer`:

```sh
cargo test -p mixlib-cli --bin mixlib --features cp-search \
  cached_admission_survives_geometry_preflight -- --ignored --nocapture
```

The audit reproduced all six schedule hashes, scores, and last-resort flags
from three pinned windows, including a 12-bar dense mashup and three cuts.
The 73-track budgeted run recorded 90 skipped standard profiles in 11 typed
windows and performed ten cut-band PCM checks. Five exact windows matched
the previous run. Their selector rows agreed, and all five skipped the
cut-band check entirely. The run reached its 12-window budget in 53.6
seconds, with one window missing tactic facts, and retained the approved
57:02.420 incumbent. The work counts are directly comparable on the matched
windows. Elapsed time includes search over different candidate windows.

Window worker results are committed individually. A local CP execution
failure uses its window-attempt budget and leaves no negative cache
record. An affected bar page keeps its cursor and is queued for the
next run. Other successful observations in the batch remain durable,
including when cancellation ends the run. Unknown observations are
reused for their unchanged source and policy identity, without treating
them as a musical conflict or repeatedly running the same PCM check.

The 73-track smoke run classified 1,350 observed windows. Of those, 766
had a verified choice, 583 had a completed review with neither an admitted
choice nor a discovered continuation, and one stayed unknown with
`TACTIC_FACTS`. The 52 windows that supplied continuations were part of
the verified-choice group. The run reported no execution failures.
The worker regression injects an I/O failure and a cancellation alongside
two successful observations, then reloads only the successful records.

CP reconstructs its discovered continuation queue from verified parent
measurement records. Children retain the same source and analysis pins,
are deduplicated by exact window identity, and stay pending until their
own measurement is committed. A resolved child can still contribute its
own pending descendants. A missing, cancelled, or interrupted child
measurement leaves the parent record available on resume. The quality
lane alternates these children with ranked proposals when both are
available. The report counts restored, newly discovered, scheduled, and
pending children. This covers discovered windows. The ranked generation
limits still leave additional endpoints and tactic realizations open.

On an anchored CP turn, the solver also enumerates the detected endpoint
extensions of that exact measured cue pair. The family fixes both cue
starts, the original overlap endpoints, the corresponding pulse ratio,
source and analysis pins, context and timing policy. The existing
planner examines every detected later pulse length supported by the
mapping, up to 256 pulses per side and the continuation-duration limit.
It stores the resulting timing windows before scheduling them. Resume
checks the family and window pins and restores only unfinished
measurements. An unavailable pinned analysis remains unknown. This
profile covers later detected extensions of reached anchors, while
other cue pairs, other tempo choices and tactic realizations still
have separate coverage obligations.

The fixture independently expects five later endpoints. With a ranked
limit of two, only the nearest and furthest windows appear. The full
scan keeps all five at the same entry and exit-start cues. In the
73-track run, the selected Perfect Girl → No Boundaries anchor had
supported later endpoints at 76, 80, 84 and 88 outgoing pulses. The
second run restored that four-window family. Both runs retained the
verified 57:02 set at −4.7058.

In the 73-track resume comparison, the first run recovered 67 unfinished
continuation windows, scheduled four, and left 63 pending. The second run
recovered those 63 and scheduled two more. The total admitted cue and
tactic choices rose from 883 at the first startup to 890 at the second
run's end. Both runs retained the previously approved 57:02 set at
−4.7058. The mock also checks recovery of a child left unmeasured at
shutdown without remeasuring its parent.

### Do not repeatedly solve the same unresolved candidate

Every iteration must make measurable progress by:

- Verifying a previously unknown exact choice;
- Tightening a valid bound;
- Adding a sound conflict;
- Splitting an unresolved domain; or
- Completing and scoring a previously unevaluated full assignment.

A fully verified set whose score is below its optimistic estimate must tighten
the model's objective representation for that assignment. Merely learning that
all its edges are admissible leaves its narrative/development overestimate
unchanged and can cause endless reselection.

An exact scored assignment may be closed as unable to improve the retained
incumbent, with the proof recorded. Do not turn that optimization cutoff into
a general musical-infeasibility claim.

Maintain separate feasibility and improvement semantics. If a master containing
an incumbent cutoff is infeasible, it proves only that no better completion
survives that cutoff. It does not prove that no valid mix exists. Report
`infeasible_within_domain` only when the original feasibility domain, without
objective cutoffs or incumbent-relative exclusions, is proved empty.
Tag each solve explicitly as `base_feasibility` or
`improvement(incumbent, tolerance)`, with its domain restrictions and cutoff.

For eventual termination on a finite domain, each unresolved family must admit
a finite sequence of coverage-preserving splits into concrete assignments.
Bound updates alone must not permit an endless sequence of negligible changes:
detect repeated unresolved work and require a split, exact evaluation, or a
sound elimination. Fair scheduling must eventually address every competitive
remaining family. With a terminating exact oracle, these conditions permit
finite completion; an unresolved oracle result must remain visible when the
run stops. They do not imply a practical exhaustion time for the real library.
Bound retries for `Unknown` and execution errors until new evidence or a
changed dependency can resolve them; preserve their unresolved bound meanwhile.

### Orchestration sketch

~~~
snapshot = pin_library_recipe_and_domain()
incumbent = load_revalidate_and_rescore_saved_incumbent(snapshot)
master = build_optimistic_master(snapshot, unresolved_domains, reusable_facts)

while budget_remains:
    propagate_cheap_constraints(master)
    result = solve_with_bound(master, remaining_budget)
    record_scope_bound_and_recompute_global_ledger(result)

    if complete_scope_and_certified_gap_is_small():
        return incumbent_with_certificate()
    if result.proves_infeasible():
        if result.covers_original_feasibility_domain:
            return infeasible_within_declared_domain()
        if result.closes_all_improvement_domains_and_has_incumbent:
            return incumbent_with_certificate()
        close_only_the_proved_scope_and_continue()
        continue

    candidate = choose_competitive_candidate_or_domain(result)
    if candidate.requires_domain_refinement:
        split_domain_with_coverage_witness(candidate)
    elif candidate.has_unknown_audio:
        facts = measure_bounded_batch(candidate)
        persist_and_apply_facts(facts)
    else:
        outcome = assemble_close_replay_validate_and_score(candidate)
        if outcome.is_verified_and_better:
            persist_incumbent_immediately(outcome)
        tighten_exact_assignment_or_learn_scoped_conflict(outcome)

    checkpoint_if_due()

return budget_result_with_incumbent_bound_and_remaining_work()
~~~

Native solver work and audio measurements have separate cancellation points.
Background workers do not mutate a live native search tree.

## 12. Feasibility, quality improvement, and strong mashups

Use the same complete domain in both phases; change priorities rather than
silently removing difficult choices.

### Before an approved incumbent

Allow the existing layout and baseline generators to propose fast feasible
solutions. A nil draft is useful progress, but supplies no feasible objective
bound. Warm-start with compatible previously verified transitions and sets.

Do not assume a valid set must exist for every pool or contradictory recipe.
Distinguish insufficient source capacity from unsearched musical possibilities.

### After an approved incumbent

Switch the primary measurement priority to potential **full-set improvement**.
Do not continue reserving three of every four slots for baseline proposals.

Use:

1. Unknown choices on the most competitive optimistic complete routes.
2. Choices whose measurement can tighten the largest remaining bound.
3. High-quality measured mashups with unresolved surrounding route choices.
4. Aging/fairness for competitive domains that would otherwise starve.

Predicted improvement per measurement cost is a scheduling heuristic. It must
not alter certified bounds or permanent pruning.

Known strong mashups may seed primal heuristics or local repair neighborhoods.
They must not become mandatory anchors unless the user requested that
adjacency. The complete master retains other possibilities.

### Required regression: the stranded strong mashup

The measured 16-bar Perfect Girl → No Boundaries transition leaves the incoming
overlap at source time 102.46 seconds. The rendered set's No Boundaries exit was
130.14 seconds, leaving only 27.68 seconds of solo runway at those coordinates.
Actual development also depends on the tactic's allowed audible hold credit.

The solver must therefore consider:

- A different entry or endpoint variant.
- A later compatible No Boundaries exit and a different following song.
- Different preceding/following routes.
- No Boundaries as the closing track.
- Other legal passage/replay arrangements, with no invented development credit.

It must not conclude that the mashup is bad because replacing one join in a
frozen set fails.

## 13. Dominance and memoization

Introduce conservative dominance only after complete-domain and bound tests
pass.

A state dominates another only if it admits every relevant continuation of
the second and cannot produce a worse final objective for those continuations.

The first implementation should require equivalent:

- Used-song set and remaining choice domains.
- Terminal cue, tempo state, and unresolved adjacent-transition obligations.
- Required-pair state.
- Elapsed output clock.
- Replay inventory, committed insertions, and remaining resources.
- Relevant scoring denominators and future narrative-clock possibilities.

Compare exact committed quality and penalties, or prove the comparison
pointwise over every shared continuation. Penalty lower bounds alone cannot
establish dominance. Fix any replay decisions that can retime already scored
passages, or retain the prefix narrative cost as a function of those decisions.

“Earlier arrival and higher current score” is insufficient. A shorter route
may miss the minimum set duration or change the narrative placement of all
later passages. Different used-track sets may preserve different essential
continuations.

Do not use lossy elapsed-time buckets as exact dominance classes. Bucketing can
order work, or serve as an interval abstraction with a coverage proof.

Record each dominance elimination with the dominating state ID, state
equivalence key, and comparison facts. Parking remains a separate statistic.

The refiner now shares one exact measured-choice identity across
registration, route assembly, conflict and closing checkpoints, and archive
reconstruction. It includes the normalized plan, both source cue counters,
the fit and credit values at their exact floating-point bit patterns, and the
last-resort flag. In the pinned run the acoustic policy and source revisions
are already fixed. Recipe labels, search budgets and generated IDs therefore
do not create new choices. The closing queue compares the ordered sequence
of these identities. It counts duplicate requests separately from parked
closing work and keeps separate jobs if an edge's pacing or score changes.
The fixture checks that identical requests queue once and that a change to
either cue counter or the score keeps a distinct choice and closing job.
This is exact duplicate elimination. Comparing the objectives of different
prefixes across all possible continuations remains open.

The 73-track warm run recorded 97 duplicate measured-choice registrations
and 14 duplicate closing requests. It retained 158 distinct measured
choices and the verified 58:53.717 incumbent. The identity change opens
a fresh search checkpoint namespace and still reuses the pinned acoustic
measurements and the verified set.

## 14. Code organization and ownership

Introduce `crates/set-search`, tentatively named `mixlib-set-search`, with a
narrow domain/solver interface. Keep musical I/O and native backend ownership
explicit.

| Component | Responsibility |
|---|---|
| `set-search/domain.rs` | Manifest, stable IDs, family partitions, coverage witnesses |
| `set-search/model.rs` | Passage/transition variables, route and resource constraints |
| `set-search/propagation.rs` | Sound inexpensive domain reductions |
| `set-search/objective.rs` | Versioned objective contract and bound transformations |
| `set-search/bounds.rs` | Interval and relaxation-bound handling, numerical envelopes |
| `set-search/backend/scip.rs` | Native lifecycle, solves, candidate and best-bound extraction |
| `set-search/oracle.rs` | Verification requests/results and scoped conflicts |
| `set-search/session.rs` | Anytime loop, incumbent ownership, budgets, measurement selection |
| `set-search/dominance.rs` | Exact-state memoization and proved dominance |
| `set-search/checkpoint.rs` | Portable search state and compatibility validation |
| `set-search/report.rs` | Final status, bounds, progress, and explanations |
| `cli/cp_search.rs` | Library/config adapter, worker pool, planner/renderer integration |
| `planner/cue_domain.rs` | Complete supported cue/endpoint domain access, separate from ranking |

Module names are proposed; preserve these ownership boundaries if names change.
The backend must not need direct access to WAV paths, application UI, or source
mutation APIs.

### Existing components

| Existing file/component | Treatment |
|---|---|
| `duration_search.rs` | Extract/reuse source snapshot, measurement, registration, and incumbent services behind explicit contracts |
| `search_cache.rs` | Reuse checked acoustic facts; separate new domain/proof compatibility |
| `chain.rs` | Retain authoritative assembly and time-map compatibility |
| `composition.rs` | Retain complete validation/scoring; expose exact evaluation and complete closing-domain access |
| `required_pairs.rs` | Reuse identity normalization and adjacency semantics |
| Planner evidence and `tactics.rs` | Retain actual audible-schedule measurement/admission |
| `baseline_cues.rs` / `event_cues.rs` | Keep as ranking and incumbent-construction helpers |
| `refinement_layout.rs` | Keep as a feasible-draft/primal-heuristic helper |
| `set_replays.rs` | Keep heuristic incumbent arrangement; add complete finite-domain access for proofs |
| `complete_refinement.rs` | Keep as an experimental comparison/heuristic path during rollout; retire as proof authority |
| `refinement_branches.rs` | Keep legacy tests and documented bound; replace fixed-order-only proof model |
| `discovery_schedule.rs` | Optional proposal ordering, never a completeness definition |
| `set_config.rs` | Backward-compatible engine/domain/budget configuration |
| `timetable.rs` | Final CP status and bound evidence, ahead of stale incumbent metadata |
| Audio renderer and existing `MixPlan` | Preserve execution contract |

Do not duplicate the scoring formula in divergent implementations. Establish
shared score inputs/outputs before moving large policy code between crates.
An initial CLI adapter around the current composer is preferable to a broad
simultaneous architecture rewrite.

## 15. Caching, invalidation, and resume

Use distinct fingerprints:

1. **Source facts:** recording identity, audio hash/revision, analysis pins,
   overrides, rates, and source intervals.
2. **Acoustic admission:** renderer/admission implementation, tactic schedule,
   time maps, model versions, and dependencies affecting sound or eligibility.
3. **Domain:** permitted cues, timing templates, endpoint/replay rules, and
   enumeration version.
4. **Objective:** scoring weights/version, requested structure, duration rules,
   and pool-dependent profiles.
5. **Search/backend:** model schema, backend/version, numerical settings, and
   checkpoint format.

Changing a budget or scheduling heuristic should not invalidate unchanged PCM
evidence. Changing domain membership or pool size invalidates old global
coverage/bound claims. Pool growth may preserve pair facts while requiring
incumbent rescoring because intensity profiles are library-relative.

The CLI now uses separate namespaces for measured windows, cue proposals,
and search checkpoints. Pair fingerprints include the pinned-window
measurement code, acoustic source files, and the lockfile graph reachable
from the acoustic crates. Closing and replay changes invalidate search
state. Proposal-only edits invalidate proposal state while retaining
compatible window measurements.

The arm64 baseline migration is an explicit read alias for the audited
admission fingerprint. The original window records remain in place.
The pair measurement and key generation moved unchanged, and the native
stretch bindings and machine-code sections matched in the two audited builds.
Each imported measured edge still passes the source, analysis, and policy
checks on load.

Duration search uses one metadata snapshot from pool selection through
refinement. The snapshot checks the fused sidecar index on first use and
reuses it for later lookups. It decodes the recorder fields and Analyzer
records from the same pinned JSON, so a concurrent change to a Spotify ID
cannot produce a mixed view of the pool. Logical reference checksums and
exact-byte verification before publishing a result still apply. The
catalog fixture edits a Spotify ID after selection, checks that the current
snapshot retains the original ID and fails verification, then checks that
a new snapshot sees the edit. The search report includes
`budget.pool_seconds` from command start through pool selection.

With the same 35-second resume recipe and 73-track pool, preparation
through verified incumbent restore fell from 19.86 to 12.80 seconds.
Pool selection took 3.06 seconds in the new run. Both runs returned the
same 17 songs, source intervals, 164,276,155 output frames and
−4.7058154 objective. The new run measured eight fresh windows and used
48 cached windows, up from five and 23. These are sequential resume runs,
so the second also had access to window facts saved by the first.

When a CP resume has no compatible incumbent, it can use a prior
winner under the same recipe and source pins to prioritize current
measurement. It reconstructs the saved order from exact pair cues and
overlap lengths in that archive, strips executable tactics from the
windows, and remeasures them within the run budget. The old score helps
identify the original request, and the old tactic orders current choices
for the initial reconstruction.
Only today's measured choices can assemble the candidate. The composer
rechecks the opening, ending, duration, development, required pairs,
and score. The archived arrangement and approval are left in the
archive. Windows that pass are retained even if the entire route cannot
be rebuilt, and cold and warm-facts runs keep their own startup rules.

The 73-track resume run started without a compatible incumbent.
All 16 archived join windows passed current admission. Reassembly and
the current composer approved a 17-track set at 57:02.420 and
−4.7058154. Archive recovery took 12.56 seconds after cache startup,
with the first verified candidate at 26.77 seconds total. The search
retained it through the rest of the 24-window run. The exact closing
queue also visited two concrete opening choices during refinement.
The app rendered the approved plan to stereo 48 kHz float32 WAV.
Direct RIFF validation counted 164,276,155 frames, exactly the plan
length, and the timetable contains 17 track rows. The next resume
restored the same score at 14.73 seconds total with zero fresh
measurements before approval and no archive recovery pass.

Persist:

- Verified incumbent and its validation/scoring identity.
- Domain manifest and unresolved family partitions/cursors.
- Exact measured facts and scoped learned constraints.
- Branch restrictions, safe cached bounds and their dependencies.
- Measurement queue and scheduling age, so restart does not repeatedly push
  rare work behind the same historical seeds.
- Backend-independent progress and proof ledger.

Do not persist native pointers or depend on undocumented native tree layouts.
Rebuild backend state when necessary. A resumed queue may run in a different
order while preserving the same domain, incumbent, and proof scope.

Restore and revalidate the incumbent **before** any restored candidate can
overwrite the saved winner. Preserve atomic writes, checksum checks, immediate
incumbent persistence, immutable chunk reuse, and throttled full snapshots.

Separate objective/domain failures from acoustic invalidation. A changed
composition score does not automatically invalidate a measured vocal check.

## 16. Configuration and reporting

Keep version-1 recipes readable and their existing semantics unchanged. Add a
versioned extension for the CP engine; old binaries must reject unsupported
new recipes clearly instead of silently ignoring fields.

Illustrative version-2 shape, to finalize during implementation:

~~~json
{
  "version": 2,
  "name": "Hour set",
  "duration_minutes": 60,
  "structure": "story",
  "must_pairs": [],
  "pair_mode": "either",
  "search": {
    "engine": "cp",
    "domain_profile": "phrase-cues-v1",
    "seconds": 1800,
    "measurements": 8192,
    "absolute_gap": 0.05,
    "measurement_workers": 4,
    "solver_threads": 1,
    "seed": 0
  }
}
~~~

These are example budgets, not a completion-time promise or a request to
change the user's current recipe. The domain profile names a concrete manifest
definition; it must not conceal a changing top-k shortlist.

The experimental CLI accepts `"engine": "cp"` under `search` in a
version-2 duration recipe. Build `mixlib` with `--features cp-search` and run
`mixlib search-set --config PATH`. The current recipe parser accepts the
engine and the existing time and measurement budgets. Domain profiles and
gap controls follow with the certified search.

SCIP currently runs as a proposal stage within the duration refiner. Its
route model considers all directed pairs and keeps each measured variant
distinct. Unresolved pairs generate cue requests based on the preceding
measured entry. Fully measured proposals use the existing chain assembler,
bookend selection, replay planner and validator. The `search.cp_search`
report records attempts, candidates, measured routes, and solver time.
Its bound remains unknown while the model uses proxy scores and the
measurement queue has unresolved families.

For directed pairs selected by SCIP, the measurement queue also reserves
one slot per batch for the pinned bar domain. It advances the cursor after
the measurement completes or after a page contains only timing rejections.
Pair cursors and the pending pair queue survive restart. They track
scheduling progress, and a finished scan covers timing skeletons only.

### Stop/result contract

Preserve the existing outer `ready`/`draft` compatibility where useful, with a
more precise `search.termination` and proof scope.

| Termination | Required meaning |
|---|---|
| `optimal_within_domain_tolerance` | Verified incumbent and complete-domain upper bound meet tolerance |
| `infeasible_within_domain` | Complete declared domain proven infeasible |
| `budget_with_incumbent` | Valid set retained; unclosed quality gap/work remains |
| `budget_without_incumbent` | No verified set yet; no infeasibility implication |
| `cancelled_with_incumbent` / `cancelled_without_incumbent` | User/process cancellation, preserving valid state |
| `restricted_domain_complete` | Only a declared subset was exhausted |
| `backend_error` / `oracle_error` / `numerical_unknown` | Execution or proof failure, not musical impossibility |

An empty shortlist, heuristic generation exhaustion, measurement quota, or
elapsed-time limit never becomes `infeasible_within_domain`.

The current CLI final report distinguishes the wall clock, measurement cap, and
generated probe exhaustion. It emits `budget_with_incumbent` or
`budget_without_incumbent` for a budget stop, and `proposal_scope_exhausted` for
heuristic exhaustion. CP includes the conservative coverage bound and gap when
its score inputs are valid, and marks a missing bound explicitly. The same
final report is attached to the selected plan for timetable export.

CP reports `infeasible_within_domain` when directed duration propagation
eliminates every track in the optimistic graph that includes all permitted
unmeasured joins. The proof applies before a native proposal, a measurement,
or an incumbent cutoff. The report records the pinned manifest, required
track IDs, per-track optimistic capacity, replay allowance, and propagation
count. Both score regions are empty and no numerical optimum or gap is
reported. The result has no draft or approved plan.

A required adjacency chain can also prove infeasibility after duration
propagation. An either-order chain must fit in one orientation from end
to end because every song is unique and every required pair is direct.
The solver tests both complete orientations using the pinned graph,
including unmeasured joins. For ordered requirements it checks each
required direction. The report names the blocked chain and the edges
that prevent each allowed orientation. Membership-only requirements
still use the duration and reachability checks. The fixture checks all
16 edge masks on a three-song chain in all three modes. A separate
search fixture checks that the proof stops before SCIP or PCM even
when the directed duration relaxation still retains a route and each
pair individually has a permitted direction.

If exactly one orientation survives, its joins fix each source's successor
and each destination's predecessor. The solver removes conflicting edges
from the optimistic graph and reruns duration propagation, which can force
another chain or rule out a route. Cue scheduling and the mixed SCIP model
use the tightened graph, including its surviving unmeasured joins.
A four-song fixture confirms that the scanner and the model keep both
orientations until one is ruled out, then retain legal entry and exit
edges for the remaining orientation. An independent check enumerates all
4,096 directed graphs of those four songs and verifies that propagation
preserves every duration-capable route satisfying the required chain.

If the structural relaxation survives and no all-nil layout fits, CP can
still look for a measured set that stretches or replays enough material.
A deadline without a draft or an incumbent returns `pending` with
`budget_without_incumbent`. A fixture exercises the result and its saved
report for an impossible directed fork, an undersized pool, and a pool
whose original clock is short but whose optimistic clock permits a set.

### Required progress statistics

- Best verified objective and plan ID.
- Valid global remaining upper bound, or an explicit unknown value.
- Absolute gap and numerical tolerance; any percentage-gap convention must
  handle negative or near-zero objectives explicitly.
- Domain completeness and unresolved families.
- Tracks eligible/excluded; proposed and measured ordered pairs.
- Distinct windows measured and exact cue/tactic choices admitted.
- Work/admission/rejection totals by tactic family and endpoint length.
- Constraint, duration, objective-bound, and dominance prune counts separately.
- Heuristic deferrals, queue sizes, and oldest competitive unmeasured work.
- First-feasible time and every subsequent verified improvement.
- Initialization, propagation, native solve, PCM measurement, full validation,
  checkpoint, and render time separately.

Report the final run state in the timetable, not only the checkpoint that first
saved the winning plan. Explain a strong unselected mashup with scoped blockers
or remaining opportunity; distinguish “not found yet” from “proven impossible.”

Optimality gaps are a standard solver reporting concept, but our guarantee
must include the declared unmeasured/unmaterialized domain.
[Gap semantics](https://support.gurobi.com/hc/en-us/articles/8265539575953-What-is-the-MIPGap).

## 17. Correctness tests

### Independent tiny-instance oracle

Build fixtures with 3–7 tracks and multiple entries, exits, tactic variants,
tempo states, endings, and replay choices. Exhaustively enumerate their finite
assignments independently of the new solver's branching code.

Use the same final musical validator to score legal complete plans, then
compare:

- Existence of a feasible set.
- Optimal objective and accepted tolerance.
- Exact duration and required adjacency.
- Every claimed pruned subtree against its actual best completion.

Test each bound independently, as well as full solver integration. Avoid tests
that merely reconstruct the same pruning formula and compare it to itself.

### Adversarial cases

1. A negative numerator where the longer duration gives the better score.
2. A lower-ranked entry enabling a superior subsequent mashup.
3. An unmeasured or unmaterialized choice containing the actual optimum.
4. One failed tactic preserving another tactic for the same songs.
5. A late entry failing one outgoing cue but succeeding with another.
6. Individually good joins with incompatible middle-track runway or tempo.
7. A required either-order pair and a separately tested ordered pair.
8. A route that cannot reach the minimum duration despite cheap local matches.
9. A route reaching the target through legal whole-phrase replays.
10. Replays that add time but cannot repair missing original development.
11. A valid later ending beyond the old eight-attempt heuristic limit.
12. A replay solution hidden by the old six-region/one-second-bucket heuristic.
13. Different elapsed clocks or used-song sets that must not be merged.
14. Earlier replay insertions changing later narrative penalties.
15. A high optimistic route whose full-set penalty must tighten after evaluation.
16. A family split whose children exactly cover the parent, including boundaries.
17. Measurement errors and cancellation that must not become rejections.
18. Frame-boundary, rounding, tie, and solver-tolerance cases.
19. A measured reward exceeding its advertised upper bound.
20. Restoring a strong incumbent before weaker frontier completions.
21. An infeasible improvement-cutoff model with an existing valid set, which
    must report optimality within scope rather than musical infeasibility.
22. A remaining-model bound below the incumbent and tolerance-closed regions
    above it, ensuring that the reported global bound covers both.
23. A partial solve where a deferred or in-flight region contains the optimum.
24. Repeated tiny bound changes or unknown oracle results that require a split,
    evaluation, or an unresolved stop.

Use deterministic fake audio oracles with hidden incompatibilities and scores
to verify that lazy evaluation skips irrelevant work while retaining the true
optimum. Shuffle worker completion and enumeration order to detect accidental
order dependence.

For fast iteration on measured-route filters, run this from `Sources/Analyzer`:

```sh
cargo test -p mixlib-cli --features cp-search cp_search::tests::
```

The three-track fixture enumerates all nine cue combinations against an
independently specified feasibility table. It checks the bridge validator,
then pins each assignment in the production SCIP proposal model after the
speed conflicts have been learned. The unpinned solve must pick the best
surviving proposal. A separate fixture reproduces a joined cubic exceeding its
speed limit after roughly 153 seconds of source runway, with legal pair maps.
It verifies that the smooth fallback passes the same speed and acceleration
limits and preserves every overlap anchor and the exact solo frame count.
These checks exercise the measured route filter and proposal objective.
Closing and the final set objective have separate coverage.

For the duration filter alone, run:

```sh
cargo test -p mixlib-cli --features cp-search window_capacity_excludes_only
cargo test -p mixlib-cli --features cp-search exact_overlap_is_charged_once
cargo test -p mixlib-set-search directed_capacity
```

The first fixture gives a permitted pair two measured cues with different
source clocks. The early exit and late entry leave only 1,520 seconds of
original material across all three songs. Even with the maximum stretch and
replay, that cue cannot reach 57 minutes. The other cue leaves all 3,600 source
seconds available to the relaxation. The filter removes the short cue from
route refinement and fixes its variable to zero if present in SCIP. The
longer cue, the unresolved pair, and a measured cue with unconfirmed
geometry stay eligible. Measurements remain in the acoustic cache. The
directed-graph fixture also verifies that every other track in a feasible
route appears in the reachability allowance for each of its joins.

For a measured choice with pinned output clocks, both retained source
spans include the same transition. The bound subtracts one copy of the
exact output overlap before the replay allowance, rounding the deduction
down. The 57-minute boundary fixture gives two choices identical source
capacity and overlaps of four and twenty seconds. SCIP fixes the longer
choice off and keeps the shorter choice and the unresolved pair. A
mismatched output timebase does not provide evidence for this deduction.

Adjacent measured choices also pin both ends of the middle song. The
joint duration bound keeps only that source span, charges each exact
overlap once, and counts other songs only if they can precede the
first song or follow the last one. SCIP groups incompatible arrivals
under their measured departure. Missing splice-anchor evidence leaves
the combination unresolved by this check. The three-song fixture
retains two cues that separately have enough material, then rejects
their combination. A later departure and an unmeasured alternative
remain eligible.

In a bounded warm-cache run over all 73 tracks, the same filter checked all
145 admitted choices and excluded zero. Every pair had at least 18,955.69
native source seconds available from the other 71 tracks alone. The filter
allows all reachable tracks in its optimistic calculation, so that allowance
exceeded the 3,420-second minimum even before including either cue's
retained material. At the timing-cell level, the bar scan rejected 75 of
83 visited cells and the structural scan rejected 417 of 420. Each scan
had 5,255 pairs unstarted and zero finished. The run stopped at the wall
clock limit, with 0.27 seconds in SCIP out of 36.28 seconds overall and a
verified 57:02.420 incumbent. The run report exposes the separate pair,
measured-window, and timing-cell counts.

`cargo test -p mixlib-cli --features cp-search exhaustive_mock_endings`
checks every combination of two endings and replay presence on a fixed route.
The replay makes either ending long enough for the hour window. Only the
later ending gives the closing track enough original development. Both
versions of the early ending report the same track and development shortfall.
Use the saved-ending audit below to compare a real route with this control.

`cargo test -p mixlib-cli --features cp-search fixed_opening_bound` checks
every combination of three endings and three replay lengths for each
narrative shape, with both positive and negative transition reward after
fit penalties. It compares every eligible score with the ceiling computed
before ending discovery. The scheduler fixture checks that a losing
opening is suspended before loading ending analysis and survives resume.

A cached run on the 73-track pool measured 12 new windows and reused 31.
The proposal model found a fully measured route anchored on an M01 mashup.
Its only opening left the final join ending at 45:21.105. The closing song
had at most 169.46 source seconds left from its incoming cue. Even giving
it the full remainder at the slowest allowed speed and adding the maximum
15% replay yields at most 55:36.586 before frame rounding. The fixed
opening had no eligible ending in the 57–60 minute window. That failure
arises from the exact assembled clock, while the proposal model uses a
relaxed capacity bound. The run stopped on the measurement limit with one
closing job pending and the approved 57:02.420 set still best. It recorded
no score-bound suspensions.

### Proof ledger assertions

Every permanent elimination must carry one of:

- A necessary-constraint contradiction.
- A valid objective upper bound and incumbent/tolerance comparison.
- A verified exact musical rejection with scope.
- A dominance witness.
- Complete evaluation of an exact assignment that cannot improve the incumbent.

Failure to fit a heuristic completion, a low predicted score, or a queue cap
does not qualify.

## 18. Performance and musical benchmarks

Freeze the 73-track snapshot, per-track analysis, rules, scoring version, worker
count, hardware, and recipe before comparing engines.

Run three distinct cache modes:

| Mode | Available initially |
|---|---|
| Cold pair search | Per-track analysis only; no pair cache or completed winner |
| Warm facts | Compatible pair measurements, no full-set winner/order |
| Full resume | Pair facts, validated incumbent, and compatible search checkpoint |

Version-2 duration recipes select these modes through `search.cache_mode`
using `cold`, `warm_facts`, and `resume`. The first two modes keep new
measurements, proposals, and winners in an isolated run namespace. Warm
facts can read compatible measurements from the shared cache.

Compare legacy and CP under matched wall-clock and measurement budgets. Use
several deterministic seeds for heuristic helpers. Record curves, not only
the final score: incumbent score, upper bound, gap, and PCM work over time.

Suggested checkpoints are 60 seconds, 5 minutes, and 30 minutes. These are
measurement points, not guarantees that the full domain will be exhausted.

### Acceptance gates before default rollout

- Tiny complete fixtures match exhaustive feasibility and optimum.
- Crafted fixtures demonstrate meaningful early objective and feasibility
  pruning while avoiding unnecessary audio evaluations.
- No bound, source, frame-accounting, or dominance counterexample remains open.
- A cached verified hour is available promptly on warm resume and never
  regresses under unchanged policy/sources.
- Cold and warm-facts benchmarks automatically find verified duration-valid
  sets without receiving a finished winner or hand-selected order.
- At matched budgets, the new engine is compared against the actual legacy
  objective; investigate any quality or feasibility regression before rollout.
- The strong 16-bar Perfect Girl → No Boundaries family is naturally examined
  without an anchor hint. The solver either finds a compatible full route or
  reports precise scoped blockers and remaining bounds.
- Do not require that particular pair to win regardless of the complete-set
  score. Also include a synthetic fixture where a long mashup is provably part
  of the unique optimum.
- Measurement-family statistics show that quality search is not permanently
  trapped in baseline discovery.
- Time, memory, checkpoint, and cancellation costs are bounded and visible.
- Render representative winners through the unchanged app renderer. Verify
  frames, duration, finite samples, headroom, and timetable identity.
- Keep human listening assessment separate from mathematical optimality.

Do not declare success solely because a result has zero nil joins or a large
number of complete draft variations.

## 19. Implementation milestones

### P0 — Freeze contracts and benchmarks

Deliver:

- Source/domain/scoring snapshot format and existing-run baseline.
- Explicit score components and current replay/bookend limitations.
- Tiny exhaustive oracle and adversarial fixture definitions.
- Proposed proof/status schema.

Exit gate: documented legal domain and objective, with tests able to identify
an incorrect prune independently of the proposed solver.

### P1 — Native backend and packaging spike

Deliver:

- Feature-gated `mixlib-set-search` skeleton and pinned backend experiment.
- Tiny static path/assignment model.
- Candidate, upper-bound, timeout, cancellation, and re-solve integration.
- Build and dependency/license inventory.

Exit gate: reproducible supported macOS build and agreement with tiny static
optima. No audio changes.

### P2 — Complete domains and structural model

Deliver:

- Cue/passages/tactic/ending/replay domain interfaces.
- Complete tiny-domain enumerators and conservative real-domain envelopes.
- Track order, optional membership, required adjacency, duration, and
  source/tempo compatibility constraints.
- Structural propagation and explicit domain coverage tests.

Exit gate: structural feasible sets match the exhaustive oracle; unknown or
unmaterialized choices remain represented.

The materialized routing fixture compares SCIP with independent path enumeration
for competing entry cues, exact overlap-adjusted duration, required-pair
direction, and a disconnected high-reward cycle. The CLI connects measured
variants and unresolved pair requests to the refiner. Complete cue family
partitions, coverage witnesses, and production replay and closing envelopes
remain necessary for the proof domain.

A 73-track release smoke run with 32 fresh windows selected a CP proposal in
1.6 seconds of native solving. The discovery schedule selected 15 of its
probes. The best provisional draft reached 58:30 with nine measured joins and
four unresolved joins. This establishes integration with live measurement.
The first approved set and matched cold, warm-facts, and resume comparisons
remain benchmark milestones.

The subsequent pinned bar smoke run accounted for 9,779 timing cells,
including 7,784 eliminated by block timing bounds. It sent one timing
window to measurement and saved the remaining pair scans for resume.
This run also ended with a provisional 58:30 draft.

Closing fixtures cover thirteen eligible phrase fades on a fixed route and
a replay region that reaches a narrow frame window after the legacy region
ranking omits it. Closing checkpoints use stable measured edge keys and
an ordered list of content-addressed work records. Resume rebuilds each route,
rechecks its ending, and restores a replay cursor only for the same base plan.
Completed measured-route closing work stays deduplicated. Source, recipe
composition, context, or composer changes open a fresh checkpoint scope.
An approved CP render on the real library remains a rollout requirement.

The 73-track warm-facts benchmark loaded 883 admitted joins with an empty
incumbent and frontier. In 180 seconds of refinement it reached a 58:30 draft
with 17 measured joins and two nil joins. All 14 SCIP proposals still contained
unresolved pairs. An independent comparison found the exact directed cues,
tactic, overlap frames, and fit for all 16 joins of the previously rendered
57:02 legacy set in the baseline cache. This gives the benchmark a positive
control for measured-route assembly without supplying the legacy winner to CP.
The archived pacing records match those entry and exit frames at every internal
song. The route's optimistic source capacity exceeds the master's relaxed
minimum by more than 11 minutes. Use the archived set for a separate fixed-route
verification to isolate reconstruction failures from proposal-search failures.

A follow-up added an incumbent bootstrap over the measured graph and alternated
measured-only and mixed-domain SCIP solves. With a 55-second refinement budget,
the bootstrap examined 21,440 edges, reached nine tracks and 29.6 minutes, and
attempted no closings. Neither of the two measured-only SCIP attempts returned a
route. The run ended at 58:30 with 11 measured joins and four nil joins. Expose
measured bootstrap statistics and solver stops per domain in the search report
so future runs distinguish restricted-model infeasibility from time spent
without finding a candidate. Neither outcome certifies the complete audio domain.

The next 180-second warm-facts run returned one fully measured route in six
measured-only attempts. All six hit their solver time limits. That route did
not reach a closing attempt. The best draft had 16 measured joins and two nil
joins at 58:30. The search produced no approved plan for export.

Each measured outgoing cue also gets one pacing constraint over its incompatible
measured arrivals. Flow permits at most one arrival per track, so grouping these
conflicts preserves all integer routes while tightening the fractional relaxation.
The master similarly shares one position constraint among parallel joins and
fixes unused and opening positions to zero. The tiny exhaustive comparison
includes parallel tactic choices as well as a detached high-reward cycle.
When a fully measured proposal fails to assemble, the report records the first
rejection returned by the same pacing, join, and composition checks used for
normal assembly.

With those diagnostics enabled, the next matched run produced two fully
measured proposals. Both fell short of minimum song development. The master
now bounds a measured bridge's solo duration using the slower of its exact
endpoint speeds and three frames of rounding allowance. It adds the credits
earned by the two verified tactics and compares the result to the lowest
possible development minimum for that recording. Missing or mismatched ports
stay selectable. This check is confined to the current measured choices and
leaves unresolved timing and tactic families available.

A deterministic assembly rejection can close a specific opening plus its
exact measured prefix. SCIP keeps those joins available behind another
opening or in another order. Resume resolves stable edge identities and
reruns the composer before applying a restored conflict. Unknown evidence
and failed closing heuristics do not create these constraints.

The next matched warm-facts run reported 1,647 development conflicts in its
last model and one learned prefix from a joined time-map speed failure. Its
other fully measured proposal reached the closing stage. The closing lane
evaluated nine endings across measured routes. Four base plans near 49:39–49:47
had insufficient approved replay to reach 57 minutes. The best full-length
draft was still 58:30 with 16 measured joins and two nil joins. The next
benchmark should compare the relaxed replay allowance with a measured-only
pass prioritizing routes that can reach the clock on original material.

The pinned-bar scan now reserves one in four scan starts for unseen eligible
directed pairs, in addition to priority work proposed by SCIP. It checkpoints
the global cursor, finished pairs, and pending pairs. Exhausting ranked probes
leaves this scan active under the same budget. Reports distinguish eligible,
finished, queued, and unstarted pairs for this profile. Other timing and tactic
families still need their own coverage witnesses.

#### Fast filter regression and real-library comparison

From the repository root, run the production proposal model against its
synthetic clock and cue cases with:

```sh
cargo +1.89.0 test --locked --manifest-path Sources/Analyzer/Cargo.toml \
  -p mixlib-cli --features cp-search cp_search::tests
```

The fixtures distinguish a 3,560-second original-material route, a 3,240-second
route that needs the replay-enabled proposal lane, and a duration-capable route
whose selected internal cues leave only 30 seconds between joins. An earlier
arrival repairs the last case. Expected rejection requires SCIP to report
infeasibility in the fixture. A timeout does not satisfy the assertion.

The real warm-facts pool had 883 admitted choices across 333 of 5,256 possible
directed pairs. The measured replay relaxation had sent routes near 49:40 to the
composer, where verified replay could not supply the missing time. The new
original-material proposal lane estimates duration from retained solo source
at native speed plus the actual rendered overlaps. This estimate prioritizes
original-material routes as a primal heuristic. The measured replay lane and
the mixed lane continue to consider alternatives outside that estimate.

In the matched 73-track run, the original-material lane found an independently
assembled 3,481.73-second, 16-track set after 49.9 seconds of search time. It used
no replays or new window measurements. All 15 exact cue, overlap, and tactic
choices were already in the admitted window cache. The full run retained this
set. The renderer produced a stereo 48 kHz WAV and a 16-row timetable. The WAV
contains the planned 167,123,084 frames. Nine of the joins use S13, so this run
establishes feasibility from a fresh search on measured facts. Quality
improvement, timing and tactic coverage, and the global optimality certificate
remain open.

### P3 — Objective and bound ladder

Deliver:

- Shared final-score contract.
- Sign-safe interval bound and assignment/flow surplus relaxation.
- Numerical validity policy and proof ledger.
- Safe penalty lower bounds where justified.

Exit gate: every tested bound dominates every legal completion; constructed
bad branches are pruned near their root rather than only at their leaves.

### P4 — Lazy musical verification

Deliver:

- Typed oracle adapter around existing measurement and assembly.
- Measurement deduplication, worker ownership, and scoped fact propagation.
- External solve/measure/tighten loop with mandatory progress.
- Independent final validation and incumbent persistence.

Exit gate: fake-oracle fixtures reach exhaustive optima with fewer evaluations
than eager measurement; real cue choices still pass unchanged musical gates.

### P5 — Quality refinement, endings, and replays

Deliver:

- Incumbent-aware measurement priorities.
- Route construction around automatically discovered strong mashups.
- Complete supported closing/replay search or explicit restricted-domain scope.
- Conservative dominance and exact-state memoization.

Exit gate: stranded-mashup, late-ending, replay, and narrative-clock tests pass;
quality improvements are visible in matched benchmarks.

After the first approved set, CP spends two of every eight proposal turns on
temporary neighborhoods around measured sustained transitions. One turn uses
only measured joins and one leaves unresolved bridges selectable. The anchor is
selected from measured dense or handoff credit, rotating directed pairs and cue
variants. Other turns still search the unpinned domain. The ranked discovery
schedule reserves one of every three turns for its finite global cursor during
improvement, giving the other turns to available route probes.

The fast stranded-mashup fixture gives a strong measured transition a late entry
that conflicts with the otherwise preferred following track. The same production
model finds a different successor with enough runway when it explores the
strong transition's neighborhood. A separate fixture verifies that individually
valid transition maps can violate acceleration limits when composed. The solver
reuses the chain assembler's time-map splice and derivative validation on the
adjacent measured choices of each proposed route. It adds conflicts for proven
failures to subsequent models. Output-time translation leaves these derivative
checks invariant. Unresolved bridges keep their original scope.

For measured pairs, assembly first validates the original three-interval
tempo bridge. On a speed or acceleration failure, it can replace the solo
interior with a fixed-clock integrated smoothstep sampled at eight or
sixteen intervals. The overlap anchors, solo endpoints, and output frame
count are identical across these templates. A fallback must pass the same
renderer speed and acceleration checks as the original. The existing
development duration bound therefore still applies, and the manifest
identifies the templates in their deterministic evaluation order.

In the 73-track warm-facts comparison, CP automatically examined the strongest
Perfect Girl → No Boundaries M01 variant in both neighborhoods. The measured
model returned a full proposal and the composer rejected a joined map on speed.
The mixed neighborhood returned a route with seven unresolved joins. The run
improved its validated incumbent from −5.3032 to −4.9160 at 57:57.4 through
whole-set refinement. It retained eight S13 joins and did not select M01. This
benchmark preceded the early joined-map filter and the targeted-probe scheduling
change. The legacy baseline score of −4.7058 remains ahead.

The matched rerun with eager joined-map filtering materialized 1,548 conflicts
in its final proposal model and ended with two nil joins at 58:30. The saved
approved 58:01.7 route passed an independent audit against the actual admitted
window cache, including all 15 exact joins. This rules out a false joined-map
rejection for that positive control. The lazy version adds only conflicts
encountered in a proposal. Its mock regression observes an incompatible
candidate in the first solve and infeasibility in that tiny restricted model
after the exact conflict is added. The same mock admits a compatible speed map.
The solver checkpoints learned adjacent-map conflicts by stable measured join
identity and reruns the same map validation before installing each on resume.

A 95-second follow-up with lazy checks returned a 58:30 draft with two nil
joins. None of its four measured-only solves returned a candidate before its
time limit, so no joined-map filter fired. It reported `budget_without_incumbent`
and `wall_clock`. The cached positive control remains valid. This run leaves
robust startup incumbent discovery as a separate performance problem.

The next startup implementation builds compact measured prefixes. It caches
local bridge lengths after checking pacing, development and the actual
joined time map. The beam keeps distinct terminal cues and elapsed clock
regions. It materializes and composes full routes only when their estimated
original clock can reach the target. A bounded beam drop is a scheduling
decision. No alternative is removed from the CP domain by this search.

In the matched 95-second warm-facts run, this search used 883 admitted window
choices and evaluated 19,333 local pairs. It approved a 10-track, 57:22.8
original-material set at 23.3 seconds of reported search time, about six seconds
after the first draft. There were no new window measurements before approval.
The score was −5.0740 and remained unchanged during refinement. Eight joins
used S13 and one used M04. The local cache audit also matches the exact bridge
frame lengths of the previously rendered 16-track positive control.

When measurement grows a graph with no incumbent, CP now retries the compact
search in short slices after sufficient new measured choices. When startup
already found a set, CP gives the first native turn to a measured quality
neighborhood if a sustained transition is available. A fixture verifies that
first-turn priority. The matched quality-first run reproduced the 57:22.8 set
and held at −5.0740. Its first measured neighborhood proposed an M01 route around
Perfect Girl → No Boundaries and hit a joined speed conflict. Its mixed proposal
had seven unresolved joins. With the bounded retry enabled, the conflict was
reported on Multitasker, elsewhere in the proposed route. Its incoming join
ended at source frame 5,665,086 and its outgoing cue was 12,438,846. The retry
retained the same M01 anchor and hit its time limit before finding another
measured route. That benchmark retained the same −5.0740 incumbent. Cold
discovery and improvement over the legacy score still need matched runs.

The matched local-cue run then tried 24 measured cue combinations around
Multitasker, without finding a tempo-compatible full route. Its native retry
returned a second measured proposal with a conflict at Intentionally by Nicole
Moudaber. The approved score remained −5.0740.

The bounded repair now retains revisions that remove one of several speed
conflicts and can substitute an unused middle song when both replacement joins
are measured. It preserves the exact anchor and the rest of the route. Repairs
must pass the joined-map checks, normal assembly, and required-pair validation
before entering the existing closing search. Exact failed prefixes feed back
into the proposal model. The repair limits are scheduling decisions and confer
no proof that a neighborhood is exhausted.

To audit the joined-map filter against a saved approved local set, run the
ignored `approved_library_route_survives_joined_speed_filter` test with
`MIXLIB_AUDIT_LIBRARY` and `MIXLIB_AUDIT_PLAN` set to the Analyzer library path
and saved plan ID. The test reads pinned facts and skips PCM and route search.

To diagnose a rejection, set `MIXLIB_AUDIT_LIBRARY` and `MIXLIB_AUDIT_RESULT`
to absolute paths and run:

```sh
cargo test -p mixlib-cli --features cp-search cached_library_cues_explain \
  -- --ignored --nocapture
```

Use a saved CP result with a domain manifest and a reported joined-speed
conflict. The audit reads measured windows under the saved admission policy
and enumerates adjacent cue choices on the reported middle track.
In the 73-track warm-facts run, 60 cached arrivals and 47 departures around
Multitasker yielded 2,197 local passes, 377 speed-limit failures, 178 phrase
pacing failures and 68 repeated neighbors. The reported cue coordinates
reproduced `TIME_MAP_SPEED` despite 153.6 seconds of source runway. The
bounded full-route repair tried 64 combinations, including 16 middle-song
substitutions, and found no route that cleared all three initial speed
conflicts. The local passes establish that compatible measured options exist
around Multitasker. Anchor preservation, the other joins and hour-set
validation still constrain which options can be used together.

With the fixed-clock smooth fallback, the same cached audit found 2,574
local passes. The 377 previously counted speed-limit failures all passed.
At the reported cue coordinates, all 46 speed failures were cleared. There
were 387 smooth recoveries across the raw Cartesian product, including ten
pairs that still failed pacing or repeated a neighbor. This audit exercises
the renderer's time-map validator on cached measurements and skips audio
measurement, route search, and export.

The matched 73-track warm-facts search with 95 seconds of refinement and
64 fresh-window measurements retained a 57:05.1 ten-track incumbent at
−5.0551, up from −5.0740 in the earlier cue-repair run. The measured
neighborhood for Perfect Girl → No Boundaries M01 produced a fully measured
route that passed assembly and entered closing without cue repair. The
mixed neighborhood still had seven unresolved joins. Across all measured
routes, closing had one finished route, seven pending routes, and no
evaluated ending variants at the budget limit. The incumbent used M04 and
S13 joins. The M01 proposal has advanced to closing, with duration and the
final objective still unverified for that route.

### P6 — Persistence and product reporting

Deliver:

- Portable checkpoints and separated compatibility hashes.
- Versioned recipe support and legacy recipe migration behavior.
- Monotone resume and reliable interruption recovery.
- Final timetable/JSON evidence, gaps, work breakdowns, and tactic-family
  coverage.

Exit gate: resumed and uninterrupted tiny searches agree; stale sources,
changed domains, scoring changes, and damaged chunks fail closed.

SCIP hands an exact fully measured route to the closing and replay lane. Its
proposal exclusion fixes the two bookends and the selected join assignment.
Longer routes containing the same joins stay selectable. Closing reports
unfinished, finished, and unknown measured routes for its current pinned
opening and ending/replay profile. These are scoped work counts.
The checkpoint stores stable measured choice identities and reopens a route if
any referenced work record is unreadable. Resume validates the incumbent first.
If its score falls below the score saved with the closing checkpoint, previously
evaluated routes and partially evaluated routes restart at their first ending.

The closing scheduler accepts the already assembled plan from a fully
measured CP proposal. An anchored quality proposal gets an opening and
first-ending turn ahead of queued work, then rotates with the other routes.
The drain after the measurement quota interleaves closing with draft
refinement so that a busy draft frontier cannot consume the entire remaining
clock before closing runs. Checkpoints still store measured edge identities
and replay cursors. Reports link each anchored proposal to its closing work
and distinguish a missing duration window from a window with no musical
ending, with per-route ending and replay counts.

In the matched 95-second warm-facts run, the anchored M01 route was
processed by closing. Its last join ended at 44:52.0 and the last track
had no source ending within the window allowed by the hour target and
replay cap. The 57:05.1 incumbent and its −5.0551 objective were retained.
When this exact closing outcome occurs for an anchored proposal, the
solver schedules one measured retry with the same anchor and the native
source-time original-material capacity row. This row is a proposal
heuristic. The ordinary measured, replay, and unresolved searches retain
their domains, and each retry still goes through assembly and closing.
The tiny regression starts with a higher-ranked short route and checks
that the retry preserves its anchor and chooses a longer completion.

In a follow-up with 45 seconds of refinement and 24 fresh windows, the
solver retried the exact M01 cue pair once and found a fully measured
route whose last join ended at 55:57.7. Closing evaluated the route's
one eligible ending. Replay work was still pending at the budget limit.
The run evaluated 13 endings across all routes and kept the 57:05.1
incumbent at −5.0551.

Closing now reports the base duration and authoritative candidate decision.
The same composition evaluator supplies a verified score or a scoped
rejection. Replay preparation checks the original solo ranges and
development before the duration floor. A development or solo-runway
failure rejects every replay of that fixed ending, because replay
changes the narrative clock and adds repeats while preserving the
original ranges and their overlap credits. Invalid evidence keeps a
route unknown and restarts it on resume. Anchored routes get the first
replay preparation turn immediately after their first usable ending,
then can inspect up to four placements per turn while sharing the
closing queue. Reports include preparation and verified-candidate counts.

For a fixed-route audit, set `MIXLIB_AUDIT_LIBRARY` and
`MIXLIB_AUDIT_RESULT` to the library and saved run paths. Set
`MIXLIB_AUDIT_CLOSING_JOB` to the matching benchmark's saved replay job
under `state/cp-closing-jobs`, then run from `Sources/Analyzer`:

```sh
cargo test --release -p mixlib-cli --bin mixlib --features cp-search \
  saved_quality_ending_and_continuation_can_be_audited \
  -- --ignored --nocapture
```

The audit checks source, policy, admission fingerprint, route and ending
identity. It rebuilds the saved ending from pinned cached windows and
applies the current composer. If the only ending fails development on the
closing track, it runs one measured continuation proposal and checks up
to eight targeted probes for fresh windows. It skips PCM measurement
by default and leaves the saved incumbent untouched. With
`MIXLIB_AUDIT_MEASURE_CLOSING=1` it also measures those windows in a
separate warm-facts namespace and checks whether they reopen the
measured retry.

The fixed-route audit reconstructed the 14-join M01 retry at 56:40.279
and reproduced the exact saved ending identity. The ending failed minimum
original development as well as falling short of 57 minutes. Its previous
checkpoint had already prepared replay options for eight of 15 placements.
The development check now eliminates that replay family before preparation.
In the separate matched search, the time-limited native model returned a
different measured proposal with a joined-speed conflict. It evaluated no
endings and retained the same −5.0551 incumbent. The fixed-route audit
isolates the earlier ending's result from proposal timing.

Development rejections include the first failing track ID, its one-based
position, solo duration, overlap credit and required development duration.
These are the values from the final composer, including when the route also
falls below the duration window and adding replays would only fix the clock.
In the cached replay of the M01 route, No Boundaries closed the set at
position 15. It had 42.618 seconds solo and 12.976 seconds of overlap credit,
for 55.594 seconds against a 62.529-second minimum. The route offered one
eligible musical ending. The mock covers a route with a later usable ending
as well. On this machine the cue-filter suite took 0.90 seconds and the
closing fixture took 0.03 seconds after compilation. Reconstructing the
saved ending against the 73-track cache took about three minutes.

When every eligible ending of an anchored route fails development on
the closing track, the solver records a continuation hint. It gives the
same anchor one measured retry and one retry that can request fresh
measurements. In either retry, the exact arrival that previously closed
the set must have a successor if selected. A different arrival can still
close on that song. Cue repair respects this restriction. The row applies
only to the retry, and the ordinary search retains all of its choices.
Completed hints are checkpointed by route and anchor identity and resolved
against admitted choices on resume. The exact-ending fixture checks that
one failed ending leaves the other eligible, the measured retry retains
the mashup and chooses a successor, and restoring after edge indices change
still schedules the correct retry.

Successor measurement uses the used-song set of each retained closing
route, including its opening track. Routes with the same arrival cue
but different earlier songs keep their distinct successor options. The
measurement lane skips destinations that repeat a song or fail directed
propagation and ranks known destinations by measured exits that meet
phrase pacing from this arrival. The solver's separate retry can still
rebuild the prefix around the anchor.

The reserved successor slot bounds the entire contiguous measured
subpath plus the proposed window before opening PCM. It accounts for
all pinned source trims, each overlap once, and the replay allowance.
Other tracks can contribute only before the subpath or after it. If
earlier geometry is unavailable, the bound falls back to the last
arrival and proposed successor. When neither bound is available, the
window remains eligible. A window that falls short for every matching
route returns to ordinary discovery. It may work with a different
arrival or yield a later endpoint. Fixtures exercise two prefixes that
share the same arrival but differ in duration, and a mismatched splice
anchor that leaves the clock unconfirmed.

The same continuation lane handles a route whose every opening falls below
the duration floor even with its full final source tail and replay allowance.
Closing computes the latest base clock with the renderer's frame rounding,
then adds at most the integer-frame replay budget. It can reject an opening
on this capacity before loading ending analysis. The report includes the
count and the highest proven clock ceiling across the short openings. A
short opening alone leaves other openings available. The continuation is
a proposal priority, so routes with different prefixes may still end at
the same arrival. The original-material retry is also available.

`cargo test -p mixlib-cli --bin mixlib --features cp-search short_mashup_route`
exercises a high-ranked measured route that is too short after assembly.
It checks two opening failures, then resumes the hint and verifies that
the production solver retains the same mashup cue and selects a successor.

In the follow-up 73-track run, a route using the same measured M01 cue pair
had an exact closing ceiling of 54:28.559. Closing recorded the failure
before evaluating any ending. The measured successor retry was infeasible
in its restricted model. The mixed retry found a route with five unresolved
joins. The hint scheduled six fresh windows across eight targeted probes
and was present in the saved frontier. The run used 16 fresh and 40 cached
windows and retained the 57:02.420 incumbent at −4.7058. It reached the
measurement limit in 91.7 seconds of reported elapsed time, including
startup and final assembly.

The saved-ending audit triggered the measured continuation retry for the
exact Perfect Girl → No Boundaries M01 cue. With 914 admitted choices,
SCIP reported that retry model infeasible. The result applies to the
cached measured choices. The retry that allows new measurements keeps
potential successors outside that cache available.

The closing hint can now request measurement without waiting for a mixed
SCIP proposal. On alternating batches it gets one slot ahead of ordinary
discovery. It starts from the pinned arrival's phrase-pacing lower bound
and interleaves event, baseline, and ranked probes in groups of two eligible
destinations. Existing pair fit prioritizes destination order. A concrete
window must still pass pacing at the proposed exit to use this slot.
Already reviewed windows are skipped. Other batch slots and the global
cursor continue normally. A newly admitted edge that passes the local
required-pair, pacing, development-capacity and tempo-bridge checks can
reopen the measured continuation retry. The fixture verifies both a
new cue that is too early and a later cue that reopens the retry.
The first optimized audit took 23.37 seconds after compilation. All eight
event probes scheduled fresh windows at 117.220 seconds into No Boundaries.
Audio admission reviewed all eight, found one last-resort result, and
admitted zero usable choices. The most frequent tactic-level rejection
was that the proposed transfer preceded the outgoing buildup release.
This prompted interleaving cue families before advancing to lower-ranked
destinations.
The interleaved audit measured eight windows across five distinct exits,
from 117.220 to 231.700 seconds. It recorded two last-resort results and
zero usable successors. Missing supported phrase boundaries and insufficient
or unknown headroom led the tactic-level rejections. All eight reviews
completed, and this sample leaves the rest of the successor domain open.

The 73-track resume run retained the 57:02 incumbent at −4.7058 after
24 fresh windows. Its measured M01 neighborhood timed out without a
candidate. The mixed M01 neighborhood returned seven unresolved joins.
Neither reached closing, so this run did not exercise the retry or
demonstrate a score improvement.

### P7 — Shadow comparison and default rollout

Deliver:

- Cold, warm-facts, and full-resume benchmark reports.
- Musical inspection artifacts and app-rendered winners.
- Explicit go/no-go assessment against Section 18.
- Updated usage/specification documentation and fallback procedure.

Keep the legacy implementation available behind an explicit engine selection
during comparison. Do not mix its heuristic exhaustion status into CP proofs.

## 20. Operational limits and failure handling

- Bound initialization, native solves, audio batches, and checkpoint work with
  the same run deadline; record unavoidable in-flight completion separately.
- On memory pressure, spill portable work or stop with an honest bound/status.
  Do not silently discard unresolved domains and then claim completion.
- Recover gracefully if the native backend is unavailable. An explicitly
  labelled heuristic fallback may return a verified incumbent, but no CP
  certificate.
- Invalidate proof state when a source snapshot changes. Preserve unaffected
  reusable evidence only when its dependency checks pass.
- If a score exceeds its claimed ceiling, stop proof-based pruning and report
  a model invariant failure. Do not merely clamp the score.
- Keep audio tasks outside native callbacks until profiling proves a callback
  integration necessary and the ownership/cancellation semantics are tested.

For `search-set`, the duration budget starts before the prepared pool is
loaded. The first full draft and verification of an available cached
incumbent are mandatory preparation. Pair-cache scans, historical seed
discovery, frontier restoration, proposal work, and new PCM dispatch share
the remaining time. Each started PCM window may finish and commit. The
report records time spent past the deadline waiting for those workers.

CP draft routes enter the resumable closing queue once. The CP proposal
step returns its draft to the refiner before admission, so it also uses
that queue. The legacy engine keeps its synchronous closing path. Draft
improvements still log immediately and save the best draft after cache
restoration. Bulk checkpoints run periodically and at finalization when
time permits. If the deadline interrupts writing immutable checkpoint
parts, the previous complete index stays valid. Committed pair facts and
verified plans remain durable independently.

The command reports preparation, the point optional search stopped, and
finalization through plan publication. Mandatory source-snapshot verification
and publication can outlast the configured search time. A reached window
quota is reported separately when final assembly also uses the remaining
time budget. The expired-deadline fixture verifies that a new measurement
is skipped and an existing checkpoint is preserved. A second fixture
verifies that repeated admission of the same measured CP route queues one
closing job without invoking the legacy closer.

The 73-track resume with a 35-second budget stopped optional work at 35.19
seconds and published at 36.33 seconds, with seven fresh measurements and
29 cache hits. Final verification and publication took 1.15 seconds and no
PCM worker was still running at the deadline. It retained the verified
57:02.420 incumbent. An earlier one-second resume launched zero measurements
and returned the same incumbent after mandatory preparation and publication.
Preparation time varies with library and system state, so the configured
search time is not a hard process timeout.

Once the duration search has a full draft, SIGINT stops refinement and
publishes the last verified incumbent or the retained draft. The report
records `cancelled_with_incumbent` or `cancelled_without_incumbent`. The CLI
prints the result and exits with code 130. Publication still revalidates the
source snapshot. Cancellation skips bulk frontier writes and preserves
already committed pair and closing records. An interrupted closing step
is reported as unresolved, leaving its durable cursor available for
resume. Cancellation before a draft exists follows the ordinary CLI
error path. The fixtures check both cancellation outcomes and verify the
persisted report and plan metadata.

A 73-track CLI smoke sent SIGINT when the cached incumbent was announced.
The process printed `cancelled_with_incumbent`, exited 130, and preserved the
verified 17-track set at 57:02.420. Its saved plan records the same stop
reason. The signal arrived before any fresh window measurement.

Closing work now commits a route's record on admission and after each
successful closing slice. The record atomically replaces all of that
route's remaining openings, endings, and replay cursors. Only changed
routes are written. Completed routes retain their incumbent score and any
closing-continuation hint. A weaker or missing incumbent reopens the full
route. Unknown evidence also leaves a full-route restart. A slice that
errors leaves the previously committed work available.

The records are scoped to the set and source pins under `cp-closing-routes`
and restore from stable measured edge keys. The timed fixtures verify that
siblings survive an expired deadline without a full closing checkpoint,
that resumption works after edge indices change, and that a suspended replay
returns as pending work. The run report counts restored route records and
jobs separately and reports time spent committing route checkpoints,
including the part that extended past the deadline.

In two 40-second runs over the 73-track library, the first run committed one
completed closing route and the second restored it. Route writes took 0.043
seconds in the first run and 0.139 seconds on resume. Neither run ended with
a pending closing job. The deadline and replay fixtures cover resumption
with pending work. Both runs retained the verified 57:02.420 set.

## 21. Definition of done

This implementation is complete when:

1. Track order and cue/tactic alternatives are represented inside a complete
   declared search domain, rather than supplied only by heuristic roots.
2. Cheap propagation and useful optimistic bounds eliminate meaningful groups
   of possibilities before PCM evaluation.
3. Unmeasured alternatives remain accounted for in every global bound.
4. The app retains verified duration-valid incumbents and improves them under
   the same full-set objective.
5. Strong long mashups can drive changes to surrounding tracks and cues.
6. Ending, replay, and dominance reasoning has a tested completeness/safety
   contract.
7. The result distinguishes a resource-limited search from domain-optimal or
   domain-infeasible conclusions.
8. Checkpointing, cache reuse, recipe compatibility, reporting, and rendering
   preserve the existing source and audio contracts.
9. Independent tiny exhaustive tests and matched real-library benchmarks
   support the claimed correctness and improvement.

The deliverable is a solver that spends work where it can improve the best set
or tighten the bound on the remaining possibilities.
