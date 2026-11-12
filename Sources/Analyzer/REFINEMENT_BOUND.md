# Bounds for complete-set refinement

The solver keeps a complete draft while exploring fixed-order branches. A branch
fixes its song order and a prefix of measured join choices. Its remaining joins
are provisional. Changing the order, replacing a song or reopening a locked join
creates independent work, so pruning this branch cannot delete that work.

For a finished set of duration `T` minutes, the current composition objective is

```
(sum(quality_seconds) - sum(max(25 - fit_score, 0))) / T
    - other_nonnegative_penalties
```

Here `quality_seconds` is dense-mashup plus handoff credit, with at most one family
earning credit for a join. Pacing, intensity, development and replay penalties
are included among the nonnegative deductions. Measured join quality is bounded
by 120 seconds. The draft's own ranking score is a separate heuristic and is
never used as the incumbent in this proof.

Let `L` be the locked joins, `U` the number of open adjacencies, and
`Tmin <= T <= Tmax` the requested duration interval in minutes. An admissible
upper bound is

```
UB = (sum(quality_seconds for L) + 120 * U) / Tmin
     - sum(max(25 - fit_score, 0) for L) / Tmax
```

Every open adjacency receives the maximum quality allowance and perfect future
fit, including currently nil, rejected and unmeasured choices. Using `Tmin`
maximizes the nonnegative benefit. Using `Tmax` minimizes the unavoidable locked
fit penalty. Dropping all other penalties can only increase the objective.
Therefore every approved completion below the branch has objective at most
`UB`. The implementation adds a small outward allowance for floating-point
rounding.

Prune the branch when this bound cannot exceed the best approved plan's objective.
Invalid or nonfinite inputs produce an unknown bound and cannot authorize
pruning. The fit deduction matters even when the approved incumbent's objective
is negative. For example, fifteen locked baseline blends with fit 5 and zero
quality credit have an upper bound of approximately -5 for an hour set. They
cannot improve an approved incumbent scoring -4.

A locked nil join supplies a separate feasibility proof. Since locked decisions
cannot change and approved exports require zero nil joins, that branch cannot
produce an approved plan. Its complete draft can still remain available for
independent refinement that reopens the join.

A failed provisional layout is not proof that every completion fails. Different
unlocked cues can change the time maps, overlaps and available ending. Likewise,
exhausting supplied alternatives is only exhaustion of that generated branch
domain. Finishing a time, measurement or layout-generation budget is not a proof
of global optimality or library-wide infeasibility.

The bounds and lock invariants are implemented and tested in
`crates/cli/src/refinement_branches.rs`. Actual pruning, required-order checks,
draft preservation and frontier resume are covered by tests in
`crates/cli/src/complete_refinement.rs`.
