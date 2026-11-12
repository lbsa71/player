# Constraint-programming implementation plan

Status: proposed implementation, not an implemented solver.

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

These local artifacts are benchmark inputs, not production dependencies. Before
implementation, freeze their relevant manifests and measurements into a
versioned benchmark snapshot.

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
    - 8 * E / S
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
E / S >= arc_lower
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

Then compare a Pareto vector of accumulated quality and unavoidable penalties,
rather than an arbitrary provisional score.

“Earlier arrival and higher current score” is insufficient. A shorter route
may miss the minimum set duration or change the narrative placement of all
later passages. Different used-track sets may preserve different essential
continuations.

Do not use lossy elapsed-time buckets as exact dominance classes. Bucketing can
order work, or serve as an interval abstraction with a coverage proof.

Record each dominance elimination with the dominating state ID, state
equivalence key, and comparison facts. Parking remains a separate statistic.

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

Audit `crates/cli/build.rs`: hashing the entire planner source currently couples
proposal-only edits to acoustic namespace changes. Replace broad assumptions
with explicit, tested dependency boundaries.

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

Use deterministic fake audio oracles with hidden incompatibilities and scores
to verify that lazy evaluation skips irrelevant work while retaining the true
optimum. Shuffle worker completion and enumeration order to detect accidental
order dependence.

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

### P6 — Persistence and product reporting

Deliver:

- Portable checkpoints and separated compatibility hashes.
- Versioned recipe support and legacy recipe migration behavior.
- Monotone resume and reliable interruption recovery.
- Final timetable/JSON evidence, gaps, work breakdowns, and tactic-family
  coverage.

Exit gate: resumed and uninterrupted tiny searches agree; stale sources,
changed domains, scoring changes, and damaged chunks fail closed.

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
