# Set structure tactics

Status: complete-set refinement update, 2026-11-12. This policy sits above the measured
cue/tactic search in `crates/cli/src/composition.rs`. Transition compatibility,
vocal-line protection, buildup protection and dynamic mashup endpoints remain
owned by the existing tactic planner. Set structure cannot waive those checks.

## Research and its limits

Roland describes ramp, wave and story structures: progressive lift; alternating
peaks and recovery; or smaller climaxes leading to a main peak and resolution.
It also recommends preparing small compatible groups and using bridge records
when changing direction. These are programming patterns, not a universal optimum.
[David Whitehead, Crafting a Great DJ Set](https://articles.roland.com/crafting-a-great-dj-set/).

Daniel Sokolovskiy's practice emphasizes contrast, including restrained warmup
teases, recovery before powerful passages, and planning in bundles. A permanently
maximal set can make its strongest moments less effective.
[When to lower or raise energy](https://dsokolovskiy.com/blog/all/energy-buildup/).

Energy is not interchangeable with BPM or harmonic compatibility. A whole-track
rating also misses changes inside a song; auditioning the actual cue regions
matters. [Mixed In Key, energy-level preparation](https://mixedinkey.com/workflows/use-energy-level-detection/).

There is no established universal minimum song length. DJTechReviews gives a
rough two-to-four-minute guideline while explicitly qualifying it by style,
event and listener response, including longer electronic-music selections.
We use development evidence and alternative musical exits, not a compulsory
three-minute cut. [Dex Jones, song duration](https://djtechreviews.com/guides/tutorials/how-long-should-a-dj-play-a-song-for).

The numeric curves, feature weights and dwell thresholds below are **our initial
engineering heuristics**, not results established by those sources. The app has
no crowd feedback or reliable semantic emotion recognition. It can arrange
acoustic contrast and development; it cannot certify maximal emotional depth.

## Tactics

| ID | Given the set needs… | Use… | Selection behavior | Status |
|---|---|---|---|---|
| ST01 | A self-contained hour with a memorable finish | Story | Establish mood, first lift, recovery, stronger development, late climax, release | Implemented; 60-minute default |
| ST02 | Sustained engagement with repeated opportunities to recover | Wave | Several peaks separated by lower-intensity passages; avoid continuous maximum density | Implemented, selectable |
| ST03 | A warmup or handoff toward a higher-energy slot | Ramp | Gradual intensity increase; do not force a closing energy drop | Implemented, selectable |
| ST04 | Two hours and enough compatible material for distinct movements | Chapters | Three rising movements with recovery between them and a final resolution | Implemented; 120-minute default |
| ST05 | An exceptional compatible mashup/handoff | Anchor development | Preserve the measured overlap; choose surrounding tracks and later exits that permit development | Implemented through measured graph reuse and sustained-quality objective |
| ST06 | A track whose arrangement evolves or sustains a strong texture | Let it develop | Prefer a longer actual passage; search beyond the first compatible exit | Implemented |
| ST07 | A change of energy or texture | Recovery bridge | Allow a lower-intensity compatible passage in the arc's recovery region | Implemented through arc fit; no emergency cut exemption |
| ST08 | An identifiable theme to return later | Motif recall | Recognize a recurring melodic/timbral theme across distant chapters | Future; not inferred from key equality |
| ST09 | Evidence of a tired or excited audience | Reactive reprogramming | Adjust upcoming chapters from observed crowd response | Future; no audience sensor exists |
| ST10 | A clear instrumental cycle and room for sustained development | Phrase replay / loop relocation | Repeat a measured 4/8/16-bar cycle, or reuse it at a compatible later boundary in the same solo passage | Implemented; bounded closing alternatives |

## Duration is a feasibility constraint

Only **57:00–60:00** and **117:00–120:00** qualify as completed composed sets.
Measure the final output timeline after time stretching, overlaps and musical
bookend selection and measured phrase replays. Do not sum source lengths, pad
with silence, duplicate whole recordings, or cut a buildup to satisfy the clock, or label an undersized result complete.
The number of songs is variable, up to the available distinct-song pool.

`search-set` with no track count defaults to the hour. `--set-minutes 120` selects
the two-hour window. `--tracks 8` remains an explicit short diagnostic operation;
it does not claim to be a completed duration-targeted set. Existing `stitch
--pick-eight` likewise remains a transition audition tool.

## ST10: measured phrase replay and relocation

A song is still selected once, but its clear instrumental cycles may recur.
Candidates contain 4, 8 or 16 contiguous measured four-beat bars, with steady
pulse intervals and levels, a natural return to the opening pattern after the full cycle: matching chroma,
band balance, brightness, stem energy shares and percussion density at equivalent
positions in successive cycles. The final bar may contain a different chord or
fill; it does not have to resemble the opening bar.
Original stereo PCM must pass continuation/slope checks at the cycle seam and
at **both** destination splices (normalized PCM error ≤ 0.20, paired with the
renderer’s 5 ms smoothing; this is not a sample-identical-seam claim). A later destination must have a matching bar
period, harmony, level, band balance and percussion density. An empty transcript or an
“instrumental” label is insufficient: verified absence must cover the capture
and destination with 300 ms guards; any continuing recognized phrase vetoes it.
Builds, drops and beat returns veto reuse at those boundaries.

The first policy permits one selected capture per song and one to three extra
cycles, at most 60 seconds per capture. Added time is capped at 15% of the original
set timeline. These are adjustable engineering limits in code, not DJ rules.
Only steady-clock solo passages with constant gain/EQ and no transition/effect
ownership qualify. A captured passage can play again immediately or at one of
two later phase-aligned matching boundaries. This is deliberately bounded reuse,
not arbitrary verse rearrangement, cross-song sampling, or extending an M01 hold;
those require additional audible-lane and endpoint evidence.

Closing search considers both ordinary musical endings and shorter endings that
can reach the duration window through verified repeats. A bounded multiple-choice
search picks whole cycles without overshooting the clock. Repeats earn **zero new
song-development or mashup-quality credit**, incur a repetition penalty, and their
actual listening position contributes to the intensity arc. They cannot rescue
an insufficiently developed song or legalize a weak join. Reachability allows an
optimistic 15% replay allowance only for pruning; actual completion requires the
measured, renderable choices. Loops are currently closing alternatives rather
than independent partial-route moves; cross-song and backward relocation remain
future work.

Saved `set_arrangement` records retain capture/destination frames on the base
schedule, the additional cycles and measurement evidence. `total_output_frames`
and timetable `mix_seconds` use the final clock. Source maps and saved join frame
fields retain base coordinates, explicitly identified in timetable JSON. The
renderer captures the processed solo audio, inserts exact whole cycles with 5 ms
seam smoothing, then resumes the original timeline without skipping source.
Rendered previews use the same arranged clock and seeded dither as full exports.
No additional metadata files are created beside WAVs.

`mixlib inspect --track TRACK_ID --loops` exposes source-backed candidates before
placement-specific solo/clock checks; it does not claim every candidate belongs
in a particular set.

## Passage evidence and dwell weighting

For each completed song placement, evaluate the source region between the end
of its incoming join and the beginning of its outgoing join. For the opening
and closing songs, use the selected bookends. Overlap quality is scored separately
by the measured tactic, so a nominally long presence cannot hide an almost absent
solo passage. A measured M01 hold earns both songs half-time development
credit; S05/S06 holds earn that credit only for the outgoing musical material.
The incoming drums-only part earns none. Other tactics currently earn no overlap
credit. This conservative heuristic preserves the value of strong mashups
without equating a nominal gain envelope with audible song identity. All duration
calculations use the actual output time maps.

Acoustic intensity is a library-relative percentile of a proxy combining band
occupancy (45%), onset rate (25%), high-band occupancy (15%) and RMS (15%). This
is deliberately independent of BPM. Sample the actual nonlinear playback map
at one-second output intervals and compare the corresponding measured bars with
the selected curve. Slowed bars receive their actual listening-time weight;
missing bar measurements remain explicit unknown coverage. One song can
therefore serve different functions depending on the chosen source passage.

Development evidence D in [0,1] combines sustained-texture, build and harmonic
variation measurements. Initial minimum development time is min(60 + 30D seconds,
45% of recording duration). Preferred development time is min(150 + 120D seconds,
80% of recording duration). A squared relative shortfall, weighted by 1+D,
penalizes leaving before the preferred duration. Existing phrase feasibility
still applies. These thresholds are tunable policy, not genre detection.

Retain cue proposals after 35% and 60% of the outgoing recording in addition to
the ordinary best proposals. Whole-set repairs and fixed-order cue branches
retain alternative entries and later exits. These choices allow a song to reach
a later suitable segment instead of merely making an early exit more expensive.

Finished-set ranking uses quality-weighted sustained overlap seconds per output
minute, minus 8 × mean intensity mismatch and 2 × mean development shortfall.
Complete drafts use their full fitted duration as the scoring horizon. Their
provisional rank first reduces unresolved joins, then increases measured
transition-fit and sustained-overlap credit per minute. Nil contributes zero.
This draft score is distinct from the final composition score and cannot
authorize export or hide unvalidated bookends.
Emergency/last-resort joins remain inadmissible. A normally admitted tactic
below the preferred fit score of 25 remains feasible. Subtract its positive
score shortfall, summed across joins and divided by the set duration in minutes,
from the objective. This keeps an arbitrary quality threshold from blocking the
first complete set, while refinement rewards better fit. Vocal, buildup, timing,
source and per-song development admission still apply.

## Search contract and evidence

1. Include every available distinct song with full tactic analysis. Duplicate
   captures of the same recorder song ID do not occupy extra pool slots. Prefer
   a capture that contains the old source interval and adds at least 15 seconds
   or 5% more captured duration (whichever is larger), allowing 0.5 seconds of
   observed timing jitter at either end. Otherwise retain prepared audio and deterministic capture order.
   Unknown offsets and later excerpts do not imply superior coverage.
2. Construct and persist complete duration-fitted drafts before expensive pair
   measurements. Nil is an explicit zero-point unresolved join between provisional
   passages, not an executable tactic or emergency exemption. Source capacity,
   unique-song selection and required direct adjacencies still constrain the
   layout. Available material and compatible requirements are necessary even for
   this provisional baseline. A draft's tail is not yet an approved musical exit.
3. Measure broadly applicable low-score real tactics first. S13 provides a
   four-to-eight-bar gradual stereo crossfade worth five transition points, with
   no dense or handoff-duration bonus. It requires measured grid, actual audible
   vocal safety, compatible harmony and lead occupancy, headroom and protection
   against an unfinished buildup. Sparse-role/stability/density proxies and exact
   complete structural endpoints do not gate this baseline. A confirmed S13 can
   replace nil while rarer, higher-quality tactics remain available for later
   refinement. Do not use a short cut as an unlabelled substitute.
4. Repeatedly generate new complete layouts and mutate existing ones by changing
   tracks, order, cues or tactics. Preserve compatible measured subchains and
   refit the complete output duration after a repair. Fixed-order branches also
   retain multiple cue/tactic choices for each adjacency, including choices that
   have not yet been measured. Check shared-song entry/exit runway when combining
   choices. Interleave bounded repair work with lazy fresh measurements, including
   generic windows and detected drum-return, buildup-release and drop proposals.
   A plateau in the active population must not freeze candidate generation.
5. Distinguish scheduling from proof-based pruning. Heuristic population limits
   park less-promising alternatives for later work. Only an optimistic bound
   that cannot beat the approved incumbent authorizes permanent quality pruning
   of a branch. With a fixed order and locked choices, use
   `(locked sustained-quality seconds + 120 × open joins) / minimum set minutes`,
   subtracting only locked fit shortfalls divided by the maximum set minutes;
   relax all other composition penalties. See [the bound proof](REFINEMENT_BOUND.md).
   An open join includes an unmeasured
   alternative; absence from today's cache cannot lower its bound. Unrestricted
   track replacements require the looser whole-pool bound. Draft nil count and
   heuristic rank are not admissible upper bounds on final musical quality.
6. A draft becomes an approved plan only after all nil joins have been replaced
   and musical bookends, final duration, pacing/development, source verification
   and required adjacencies pass. The final JSON returns `ready` with an approved
   plan or `draft` with the complete provisional arrangement and unresolved work.
   Zero nil alone is insufficient. Verified phrase replays remain optional
   closing alternatives. Do not pad silence, truncate lyrics/builds or relabel a
   provisional concatenation to satisfy the export gate. Persist an approved
   incumbent immediately, then replace it only on a better full-set objective.
7. Save each draft's passages, provisional endpoints, nil/real join assignments,
   measured evidence and scoring semantics. Approved plans additionally retain
   `set_composition` evidence, final bookends, intensity mismatch and development
   shortfall. Reports expose complete candidates, generated layouts, parked
   alternatives, branch bounds, proposed/measured/cached windows, rejected repairs,
   elapsed time and saved improvements. Draft progress prints full duration and
   unresolved count; approved improvements print duration, score gain, ordered
   songs and the plan path immediately. Final stdout remains one JSON result.
8. Persist cue proposals, positive/negative measurements, continuations,
   generation position, candidate alternatives and approved plans under
   `<library>/search-cache/v1`. No extra JSON files are placed beside WAVs. Resume
   compatible generation/candidates and reuse measured decisions before fresh PCM
   work. Cache identity includes audio revision/hash, analysis and correction
   references, musical policy, admission/rendering code, dependencies, compiler
   and target. Admission/rendering changes invalidate decisions. Scheduling and
   scoring changes retain pair measurements, while restored candidates are
   reconstructed and approved plans rescored/revalidated. Budget or recipe-name
   changes retain pair evidence. Set reuse also requires matching duration,
   structure, required-pair semantics and available current sources. Adding
   unrelated songs does not invalidate an otherwise valid approved plan.

The default duration search uses complete-set refinement. Short 3–8-song
diagnostics retain the older fixed-count search. Measurement/time budgets begin
after the initial complete draft and bound the entire refinement phase, even
when no approved set has yet been found. Candidate-generation work is bounded
as well. Reaching any limit leaves the best saved draft and any approved
incumbent available; it establishes neither infeasibility nor global optimality.
The cue/layout universe remains heuristic. This contract does not claim an
empirically successful nil-free hour on the current collection. `auto` chooses
the duration-appropriate structure template.

## One config per set

Keep a portable JSON recipe for each set, separate from the shared audio library.
Create one without starting a search or changing library defaults:

```sh
mixlib init-set --output Sets/hour.json --name "Hour set" --set-minutes 60 --structure story
mixlib search-set --config Sets/hour.json
```

The generated file has this form:

```json
{
  "version": 1,
  "name": "Hour set",
  "duration_minutes": 60,
  "structure": "story",
  "must_pairs": [],
  "pair_mode": "either",
  "search": { "measurements": 8192, "seconds": 7200 }
}
```

Add pairs as two-ID arrays in `must_pairs`. They remain required direct transitions
in either direction by default. Change `duration_minutes` to 120 for a two-hour
recipe (and optionally increase its explicit search budgets). For a short
benchmark, omit `duration_minutes`, set `tracks` to 3–8 and use `structure: "auto"`.
`init-set --tracks 8` creates that variant. An optional `incumbent` works under
the same short-set restrictions as the CLI; it cannot accompany a duration target.

The name and version are required. An omitted target defaults to 60 minutes;
omitted search budgets use the matching duration/track-count defaults. Unknown
fields, unsupported versions, invalid pairs/targets and out-of-range budgets are
rejected before the library writer lock. `init-set` never overwrites an existing
recipe. Edit or copy recipes to create further sets.

`--config` is exclusive with inline target, structure, pair, incumbent and search
budget flags. Global `--library`, `--wait-lock` and `--json` remain available.
A recipe owns the complete set/search request: an empty pair list clears any
library-level pair defaults for that run, and one set cannot inherit another's
incumbent or target. The underlying audio pool and general mixing safety policy
remain shared. Applying a recipe never writes `config/mixing.json` or creates
additional metadata beside WAVs.

Plans and search reports retain `set_config`: name, absolute recipe path,
canonical hash of the normalized effective settings, and a complete settings
snapshot. The timetable identifies the recipe and carries the snapshot in JSON.
Later edits to the file therefore do not alter the record of an earlier run.
Name/path/provenance do not change incumbent musical compatibility; actual pair,
structure and other musical constraints still do.

## Required Spotify pairs

Use repeatable `search-set --must-pair "SPOTIFY_ID_A,SPOTIFY_ID_B"` arguments.
Each pair is a hard requirement. The default `--must-pair-mode either` requires
an adjacent transition in whichever direction produces the better set.
`ordered` specifically requires A → B. `anywhere` requires both songs
but does not require a join between them. The mode applies to all supplied pairs.
Both full duration sets and explicit `--tracks 8` searches support these flags.

Track IDs, `spotify:track:…` URIs and `https://open.spotify.com/track/…` URLs
normalize to the same song identity. Resolution uses recorder `trackID` metadata
in the canonical prepared pool, not titles or local recording UUIDs. No Spotify
fetch or automatic substitution occurs. Missing/unprepared or ambiguous identities
fail before cue discovery. Repeated requests deduplicate; shared pairs such as
A–B and B–C form a required chain (either direction under the default). Conflicting branches, cycles and too many
required songs for a fixed track count fail immediately.

The solver filters impossible edges before proposing or measuring cues, gives
required pairs additional discovery priority, keeps different requirement-progress
states alive and enforces adjacency during both composition and short-set route
expansion. Every saved winner and supplied incumbent must satisfy the constraints.
Cue points, tactics, duration, vocal safety, buildup protection and quality admission
remain automatic. A requirement never forces an unsafe join or authorizes an
emergency cut. If the bounded search finds no satisfying route, it returns
`REQUIRED_PAIRS_NO_SET`, not an unconstrained substitute.

Saved policy extensions contain `required_spotify_pairs` (an array of two-ID
arrays) and `required_pair_mode`. CLI pairs replace a configured list for that
search; omitted flags preserve configured requirements. Saved plan/timetable
`required_pairs` evidence includes original Spotify identities, resolved local
recordings, mode and satisfaction. Failure reports also contain admitted/clean
choice counts for each required pair; zero measured choices is not proof that
no musical solution exists outside the search budget.

Example (replace the ID placeholders):

```sh
mixlib search-set --set-minutes 60 \
  --must-pair "SPOTIFY_ID_A,SPOTIFY_ID_B" \
  --must-pair "SPOTIFY_ID_C,SPOTIFY_ID_D"
```

## Commands

Run from `Sources/Analyzer` with the existing library and separator environment:

```sh
mixlib --wait-lock scan --source ~/Documents/Audio
mixlib --wait-lock analyze --resume
mixlib --wait-lock analyze-tactics --separator-python /path/to/separator/python
mixlib --wait-lock search-set --set-minutes 60 --structure story --measurements 8192 --seconds 7200
mixlib --wait-lock search-set --set-minutes 120 --structure chapters --measurements 16384 --seconds 14400
mixlib inspect --track TRACK_ID --loops
mixlib render --plan PLAN_ID --output set.wav
mixlib timetable --plan PLAN_ID --output set-timetable.md
```

No-argument `analyze-tactics` prepares every distinct available song, reusing
existing analyses; explicit `--track` still prepares a particular capture. It
preserves the existing fused `.wav.json` layout. Failed analyses are reported;
unprepared songs cannot be represented as full tactic candidates.

## Join #5 in the previous comparison

Radio Slave → Worakls / Anges used **M04**, an isolated outgoing motif over the
incoming track. It had **zero bars of full-mix hold**, not an eight-bar held dense
mashup. A dominant incoming arrangement can mask that motif. The timetable now
labels this as a motif overlay rather than claiming a hold/resolve sequence.
Audible prominence is a separate remaining improvement; set-level energy scoring
must not be mistaken for a perceptual masking measurement.

Full-set search defaults to 8,192 windows / 7,200 seconds for an hour and
16,384 / 14,400 for two hours. These are maximum work budgets, not promised
completion times. Short explicit track-count benchmarks retain 1,536 / 1,200.
