# Tactic gate review — 2026-11-12

The current admission rules mix execution safety, tactic identity and aesthetic
preferences. Several rules are useful conservative defaults, but their failure
does not prove that two tracks cannot be joined. A complete draft with explicit
zero-point unresolved joins removes the pressure to weaken these rules just to
obtain an hour of material.

The existing tactic thresholds remain unchanged. The subsequent S13 baseline
implements a separate gradual blend with a narrower structural claim, described
below. References and cache counts in the findings capture the pre-S13 review.

## Evidence and its limits

The event-cue audit screened 5,256 ordered pairs among 73 prepared tracks, then
measured 1,537 cue windows. It found 214 normal cue/tactic choices across 71
ordered pairs. Only three choices were dense mashups, while 73 were bass swaps
and 63 filter fades. Those are measurements of a bounded proposal sample, not
an inventory of every feasible cue combination. See
`build/beat-stitch/hour-event-cues/audit.json` in the repository root.

A subsequent read-only snapshot of the active cache contained 3,662 records and
3,264 window diagnostics. The common phrase-boundary condition appeared in
2,632 window rejections for S06/S08 (80.6%) and 2,872 for C02/C03 (88.0%). The
same condition was the sole reported eligibility failure on 61 S06, 96 S08,
153 S09, 27 C02 and 20 C03 candidate variants. These are inspection counts from
the later search cache, not additional unlocked joins. Clearing an eligibility
failure can still expose later scheduling or set constraints.

Raw reason totals overstate some failures. For example, 2,684 M01 profiles could
not be constructed because their window shape was unsupported, and 1,976 S08
profiles failed the audible-vocal gate. Their resulting unknown harmony and
headroom fields then produce additional rejection messages. Those messages do
not establish harmonic clashes or clipping. Report the first failed stage and
conditional pass rates before adjusting a threshold.

## Findings and proposed corrections

| Priority | Class | Finding and code evidence | Proposed correction |
|---|---|---|---|
| 1 | Strategy | Every non-emergency tactic requires complete A-exit/B-entry phrase evidence at nominal window endpoints, except a local quiet C01 cut. `crates/planner/src/tactics.rs:827–850`. Repeated structure and four-bar phase populate this evidence in `tactic_evidence.rs:692–810`; measured drum/drop events alone do not, in `cached_tactic_evidence.rs:827–850`. | Describe required boundaries for each actual ownership change. A verified drum return can support percussion entry; a completed build and measured drop can support a replacement. Require separate evidence for the later accompaniment exit. Preserve outgoing lyric completion and buildup release. Do not simply mark every detected event a complete musical phrase. |
| 1 | Safety evidence | S08's rendered high/low-pass sweeps are real, but the pair profile ignores cutoff frequency when computing tonal/lead compatibility. Filter handling only applies a resonance bound in `tactic_pair_evidence.rs:524–526`; spectral gain then uses EQ bands at `571–579`. | Keep the conservative peak bound, but measure filtered spectra/lead occupancy along the actual sweep, ideally using rendered excerpts. A filter intended to remove a conflict cannot demonstrate that benefit using unfiltered pitch evidence. A failure here is currently conservative, not proof that the audible filtered join is bad. |
| 1 | Diagnostics | Profile construction failures become unknown safety/quality values, generating several downstream reasons. See `tactics.rs:798–801, 864–880, 2827–2858` and `tactic_pair_evidence.rs` profile evidence. | Separate unavailable schedule, unavailable evidence, measured incompatibility and aesthetic preference. Count each stage against candidates that reached it. Include the primary profile error alongside downstream fields. |
| 2 | Strategy | S08's density requirement uses a window mean of mid occupancy, onset count, crest factor and active stems (`tactics.rs:463–490, 1055–1068`). It is a proxy for busyness, not a measurement that filtering helps. | Use crowding to prioritize proposals. Admit/reward a filter tactic from measured improvement in contested audible bands, preserved musical energy and safe ownership. Retain one-sided crowding support; do not treat the current 0.5 threshold as acoustically universal. |
| 2 | Quality | All C01/C02/C03/C04/C07 cuts become last resorts unless all mapped PCM bands nearly match (`tactics.rs:2665–2678`; `tactic_evidence.rs:345–395`). The comparator is sample error, limited to 20% RMS relative error (`short_fade.rs:79–86`), not perceptual compatibility. | Keep ordinary unmatched cuts low, as requested. Treat a future buildup-release/drop strategy as its own proven musical case if it should graduate from last resort. Do not silently weaken the all-band rule or count current fallback cuts as clean improvements. Zero normal C02/C03 in the audit follows this policy even after their event checks pass. |
| 2 | Quality | Dense mashups require every held bar to exceed density 0.62 and per-layer RMS stability 0.65; sparse mashups require every retained-bed bar below 0.62, including resolution (`tactics.rs:389–397, 895–952`; `tactic_pair_evidence.rs:74–87`). Dynamic musical passages can fail these proxies without becoming poor mixes. | Keep the selected-layer and full-hold measurements. Validate thresholds against the positively reviewed mashups and deliberately bad controls. Score duration-weighted density/stability, with explicit limits for genuine dropout or collision, instead of assuming any single threshold crossing invalidates the musical span. Do not change this before regression evidence exists. |
| 2 | Strategy | Arrival evidence is collapsed into one `arrival_frame`. Later events overwrite earlier events in `cached_tactic_evidence.rs:827–843`; an arrival near window start overrides it again at `125–134`. | Preserve typed event candidates and explicitly select the landing required by a tactic. Test multiple returns/drops in one window and a groove entrance followed by a later drop. Event ordering should not choose the landing accidentally. |
| 3 | Safety/quality boundary | Peak bounds sum possible source peaks and include conservative FX gains (`tactic_pair_evidence.rs:510–563`). A rejected bound is not proof of actual clipping. | Permit an explicit, bounded gain-adjusted candidate followed by peak/loudness verification. Do not remove the output ceiling or assume normalization preserves perceived balance. Report conservative-bound rejection separately from a rendered peak failure. |

The absence of M03/M06 or other phrase-dependent tactics also reflects missing
positive vocal/motif/semantic evidence. It is not enough to lower a role score.
Blank transcription must continue to mean unknown, and expressive/emergency
permission requirements must remain explicit.

## Constraints that should stay hard for an approved join

- Source availability, valid frame ranges, monotonic time maps, renderer
  capability and bounded effects/source edits.
- Beat alignment wherever two audible rhythmic layers are meant to coincide.
- Vocal ownership checked on audible lanes, including stem bleed and effects.
  Vocal gate changes must land in supported gaps and preserve complete lines.
- No outgoing buildup truncation before its supported release.
- Known, adequate separation for a tactic that depends on isolated stems.
- Verified output headroom after any explicit gain adjustment.

Role labels, density targets, preferred lengths and exact tactic precedence
belong to tactic strategy or quality. Their uncertainty should be visible and
guide refinement rather than preventing a complete draft from existing.

## Zero-point unresolved joins

An unresolved join is a search placeholder, not a newly approved DJ tactic or
an emergency. It contributes zero transition points and carries no claim of
phrase, vocal, tempo or band compatibility. A draft can therefore contain the
required duration and tracks before any expensive pair analysis succeeds.

Refinement can replace one or several placeholders, change order, move cues,
replace tracks and repair duration. Every accepted best-so-far draft must keep
its total duration and required-pair contract. Preserve alternative complete
drafts so replacing a locally attractive join does not freeze the remainder of
the route. Persist each improvement and report unresolved count alongside
musical score; a high sum must not conceal unresolved gaps.

An approved export requires zero unresolved joins plus the normal source,
schedule and set checks. Preserve the complete draft when this filter fails,
and explain which joins remain unresolved. An explicitly requested draft
audition may use a documented basic concatenation, but must remain visibly a
draft. Never promote a placeholder to an existing validated tactic ID.

## Implemented real baseline

S13 now provides a four-to-eight-bar gradual stereo crossfade worth five
transition points. It keeps grid, actual audible-vocal, harmony, headroom and
buildup-release checks while omitting exact structural phrase endpoints and
sparse-role/stability/density proxies. Its saved shape must contain two
whole-window gain ramps, with no source edits, effects or compressed cutover.
It earns no dense-mashup or handoff-duration credit. See
[the S13 contract](MIXING_TACTICS.md#s13--baseline-gradual-crossfade).

S13 is an eligible low-score candidate, rather than a claim that arbitrary
tracks safely overlap. Stereo vocal conflicts can still prevent it. A complete
candidate should use these broadly applicable real arrangements first, then
improve their cues, order and tactics. Any remaining zero-point placeholders
retain the separate unresolved/export meaning above.

## Focused regression work

1. A no-edge pool still produces a complete duration-valid draft with zero
   transition points. Approved export rejects its unresolved joins. A measured
   replacement improves the draft and preserves duration/required pairs.
2. Confirmed drum-return entry with ambiguous global phrase phase remains
   distinguishable from an arbitrary off-phrase start; later accompaniment and
   vocal exits retain their own checks.
3. A completed build/drop landing and a cut just before release must not receive
   the same eligibility result. Ordinary incompatible cuts remain last resorts.
4. Two filter candidates with the same dry spectrum but different cutoff
   trajectories should have different measured residual conflict. Suppressed
   bass conflict may disappear; remaining vocal overlap must still reject.
5. Compare smooth dynamic mashups, brief musical rests and real layer collapse.
   Record the effect of each density/stability threshold before changing it.
6. Diagnostics distinguish a short-window M01 shape failure from a measured
   harmonic clash and distinguish unknown headroom from an excessive peak.
