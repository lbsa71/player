# Set structure tactics

Status: first implementation, 2026-11-11. This policy sits above the measured
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

## Duration is a feasibility constraint

Only **57:00–60:00** and **117:00–120:00** qualify as completed composed sets.
Measure the final output timeline after time stretching, overlaps and musical
bookend selection. Do not sum source lengths, pad with silence, repeat recordings,
cut a buildup to satisfy the clock, or label an undersized result complete.
The number of songs is variable, up to the available distinct-song pool.

`search-set` with no track count defaults to the hour. `--set-minutes 120` selects
the two-hour window. `--tracks 8` remains an explicit short diagnostic operation;
it does not claim to be a completed duration-targeted set. Existing `stitch
--pick-eight` likewise remains a transition audition tool.

## Passage evidence and dwell weighting

For each completed song placement, evaluate the source region between the end
of its incoming join and the beginning of its outgoing join. For the opening
and closing songs, use the selected bookends. Overlap quality is scored separately
by the measured tactic, so a nominally long presence cannot hide an almost absent
solo passage. All duration calculations use the actual output time maps.

Acoustic intensity is a library-relative percentile of a proxy combining band
occupancy (45%), onset rate (25%), high-band occupancy (15%) and RMS (15%). This
is deliberately independent of BPM. Compare each played bar with the selected
curve at its actual output time, weighted by its played duration. One song can
therefore serve different functions depending on the chosen source passage.

Development evidence D in [0,1] combines sustained-texture, build and harmonic
variation measurements. Initial minimum solo time is min(60 + 30D seconds,
45% of recording duration). Preferred solo time is min(150 + 120D seconds,
80% of recording duration). A squared relative shortfall, weighted by 1+D,
penalizes leaving before the preferred duration. Existing phrase feasibility
still applies. These thresholds are tunable policy, not genre detection.

Retain cue proposals after 35% and 60% of the outgoing recording in addition to
the ordinary best proposals. Preserve alternative entries and elapsed-time
buckets during beam pruning. This gives later suitable segments a route into
the search instead of merely making early exits more expensive.

Full-set ranking uses quality-weighted sustained overlap seconds per output
minute, minus 8 × mean intensity mismatch and 2 × mean development shortfall.
Normal composed sets admit no emergency or weak joins (score below 25). This
prevents duration or an energy curve from buying unsafe/poor connections, and
normalization avoids rewarding more song changes simply for their count.

## Search contract and evidence

1. Include every available distinct song with full tactic analysis. Duplicate
   captures of the same recorder song ID do not occupy extra pool slots. Prefer
   a capture that contains the old source interval and adds at least 15 seconds
   or 5% more captured duration (whichever is larger), allowing 0.5 seconds of
   observed timing jitter at either end. Otherwise retain prepared audio and deterministic capture order.
   Unknown offsets and later excerpts do not imply superior coverage.
2. Propose compatible cue families, including later exits, before PCM work.
3. Lazily measure families through the existing tactic admission path and retain
   all admitted choices in the measured graph.
4. Periodically assemble variable-length routes with duration and passage scoring.
   Retain entry and elapsed-time diversity; prune impossible lengths using an
   optimistic remaining-source-time bound.
5. Apply musical bookends, validate actual duration and per-song minimums, and
   persist only fully valid improvements. A budget failure reports no completed
   set rather than silently returning an eight-track mix.
6. Save `set_composition` evidence: structure, target bounds, actual duration,
   objective, intensity mismatch, development shortfall and per-song source
   regions/minimum/preferred/actual solo durations. Timetables expose those values.

The search is bounded and heuristic, not a proof of optimality or exhaustive
coverage of all segment combinations. A larger pool can need a larger measurement
budget. `auto` chooses the duration-appropriate template; it does not claim to
have compared every possible narrative shape.

## Commands

Run from `Sources/Analyzer` with the existing library and separator environment:

```sh
mixlib --wait-lock scan --source ~/Documents/Audio
mixlib --wait-lock analyze --resume
mixlib --wait-lock analyze-tactics --separator-python /path/to/separator/python
mixlib --wait-lock search-set --set-minutes 60 --structure story --measurements 8192 --seconds 7200
mixlib --wait-lock search-set --set-minutes 120 --structure chapters --measurements 16384 --seconds 14400
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
