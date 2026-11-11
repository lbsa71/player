# mixlib engineering CLI

The Rust CLI now implements the scan → analysis → planning → WAV-render workflow.
It also implements the 38-ID mixing-tactic contract, measured cue/tactic
selection, stem/effect/source-edit rendering, and a separate live recovery API.
This remains an engineering implementation, **not a completed first release**.
The product specification remains in [README.md](README.md).
The release gates and remaining work are listed below so successful synthetic
tests are not mistaken for musical validation.

## Build and run

Tested on Apple Silicon macOS with Rust 1.89.0 and Xcode command-line tools.
The intended deployment target is macOS 14.4, matching the existing Player.
Build with an explicit target when distributing binaries:

```sh
cd Sources/Analyzer
MACOSX_DEPLOYMENT_TARGET=14.4 cargo build --locked --release --workspace
cargo test --locked --workspace
cargo clippy --locked --workspace --all-targets -- -D warnings
```

`rust-toolchain.toml` selects the tested toolchain when commands run in this
directory. From the repository root, `./scripts/mixlib --help` builds and runs
the CLI. It does not modify, install, or restart the existing Player or Audio
Bridge. The standalone executable is `Sources/Analyzer/target/release/mixlib`.
There is no Swift package dependency on the Rust workspace.

For the existing recorder folder, run from the repository root:

```sh
./scripts/mixlib scan
./scripts/mixlib inspect
```

The first scan creates shared metadata at
`~/Library/Application Support/Player/Analyzer` and registers `~/Documents/Audio`.
It merges identity and job state into the existing `.wav.json` files. Later
analysis, corrections and checkpoints update the same sidecars. All existing
recorder fields, including unknown fields, survive. The original WAV is never
modified. `.partial.wav`, unfinished segments, and files awaiting the recorder's
finalized JSON are deferred. Other explicitly supplied source folders can contain
plain WAVs; scanning those creates a sidecar if needed.

For an independently selected folder and shared library:

```sh
mixlib init --library ./mix-library
mixlib scan --library ./mix-library --source ./music
python3 Sources/Analyzer/scripts/install-models.py --library ./mix-library
mixlib analyze --library ./mix-library --resume
# Or limit analysis to selected IDs returned by inspect:
mixlib analyze --library ./mix-library --resume --track TRACK_ID --track ANOTHER_ID
mixlib inspect --library ./mix-library
mixlib plan --library ./mix-library --all --seed 42 --json > plan.json
# Read plan_id from plan.json, then:
mixlib preview --library ./mix-library --plan PLAN_ID
mixlib render --library ./mix-library --plan PLAN_ID --output ./renders/mix.wav
mixlib play --library ./mix-library --plan PLAN_ID
mixlib scan --library ./mix-library --verify-content
mixlib validate --library ./mix-library
```

`MIXLIB_LIBRARY` can supply `--library`; diagnostics always print the resolved
path to stderr. `--json` emits a single JSON value on stdout. Exit codes: 0
success, 2 invalid request/invariant/I/O failure, 3 a completed batch with failed
items, 130 cancellation. The writer lock fails promptly unless `--wait-lock`
is specified. `init` requires an empty directory. Source audio is read-only;
metadata writes atomically replace its JSON sidecar. Render outputs must be outside source roots. An existing render needs `--overwrite`.

`analyze --dsp-only` explicitly disables neural rhythm for that invocation.
It stores an unknown grid and requires visible fallback transitions. It is
useful for storage/DSP smoke tests; it is **not** a substitute beat detector.
Subsequent analysis without this flag selects the configured neural backend.
The Rust core runs locally. Python supports the optional model installer,
developer helpers, and drum separation workflow.

Planning accepts repeated `--track`, `--start`, `--end`, `--duration 60m`,
`--no-fallback`, and `--encoding pcm24`. With no duration, every eligible selected
track must appear once. Duration is the nearest feasible subset objective, not
permission to pad or silently trim a saved plan. Preview output defaults to
`<library>/previews/<plan_id>`; `--output DIRECTORY` overrides it.

For a short beat-matched WAV from two analyzed recordings:

```sh
./scripts/mixlib stitch --from FIRST_TRACK_ID --to SECOND_TRACK_ID \
  --overlap-beats 8 --context-beats 8 --output ./renders/beat-stitch.wav
```

`stitch` searches for locally steady passages in the requested order. The
overlap begins and ends on matching downbeats. The command chooses a shared
duration halfway between the two source overlaps, pitch-preservingly stretches
both excerpts, and rejects pairs whose predicted beat skew exceeds 30 ms.
`--context-beats` sets the number of solo beats before and after the overlap.
`--max-stretch 0.10` allows up to 10% speed change per source for that run.
The saved mix plan includes the chosen source ranges, speed maps and crossfade.

## Automatic mixing tactics

[MIXING_TACTICS.md](MIXING_TACTICS.md) defines the 38 IDs, A/B conditions and
musical rules. All IDs have executable contracts, eligibility checks and
schedules: M01–M06 mashups, S01–S12 blends/bridges, D01–D02 tempo modifiers,
C01–C14 structural/performance transitions, and E01–E04 runtime recovery.
Implementation means the planner and renderer can represent and execute an
admitted schedule. It does **not** mean every tactic has been auditioned,
every song provides the required evidence, or every numerical threshold has
been tuned on a representative collection.

Full tactic analysis uses a local Python 3.12 environment. The tested package
versions are Demucs 4.0.1, PyTorch/torchaudio 2.5.1, NumPy 1.26.4, SciPy 1.15.3,
SoundFile 0.14.0 and `openai-whisper` 20250625. From the repository root:

```sh
python3.12 -m venv build/drum-separation-venv
build/drum-separation-venv/bin/python -m pip install \
  demucs==4.0.1 torch==2.5.1 torchaudio==2.5.1 numpy==1.26.4 \
  scipy==1.15.3 soundfile==0.14.0 openai-whisper==20250625

python3 Sources/Analyzer/scripts/install-models.py \
  --library "$HOME/Library/Application Support/Player/Analyzer"
python3 Sources/Analyzer/scripts/install-stem-model.py \
  --library "$HOME/Library/Application Support/Player/Analyzer" \
  --checkpoint /path/to/955717e8-8726e21a.th
python3 Sources/Analyzer/scripts/install-vocal-model.py \
  --library "$HOME/Library/Application Support/Player/Analyzer"

export MIXLIB_SEPARATOR_PYTHON="$PWD/build/drum-separation-venv/bin/python"
./scripts/mixlib analyze --resume --track FIRST_TRACK_ID --track SECOND_TRACK_ID
./scripts/mixlib analyze-tactics --track FIRST_TRACK_ID --track SECOND_TRACK_ID
./scripts/mixlib stitch --from FIRST_TRACK_ID --to SECOND_TRACK_ID \
  --output ./renders/automatic.wav
```

The stem installer registers an existing checksum-pinned HTDemucs checkpoint;
it does not download one. The vocal installer downloads the pinned Whisper
`small` model. Both record provenance in the library model manifest and use its
writer lock. Full tactic analysis loads those explicit local files, verifies
their hashes, and does not silently fetch replacement models. Beat-model setup
is separate from stem/vocal-model setup. Rendering an already saved plan uses
its pinned audio assets and does not require Python or model inference.

`analyze-tactics` accepts repeated `--track`; omitting them selects the scanned
catalogue. It requires ordinary analysis first. `--separator-python PATH` can
replace the environment variable. Automatic `stitch` with that interpreter
ensures full tactic analysis for its selected tracks before choosing cues.
Without an interpreter, existing valid full-track evidence can still be used.

`analyze-tactics --expand-pool 4` chooses four additional distinct recordings
using the app's basic-analysis graph. It ranks feasible entrances from and later
exits to different prepared tracks, using the same phrase-aware pacing admission as set search.
This is a preparation shortlist, not admission of those transitions: full stem,
vocal and signal measurements still decide eligibility. Failed analyses are
reported and replaced from the shortlist. The saved expansion report records
selection evidence, attempts and before/after counts. Source metadata continues
through the single fused `.wav.json` per recording.

`stitch` and `render` now write `<output-stem>-timetable.md` next to the WAV.
The evidence table includes track presence, source excerpts, transition cues,
selected tactic and exit variant, fractional held bars, resolution length,
segment roles, selected-layer compatibility and the saved selection rationale.
Its structured counterpart is embedded in the stitch plan or render report.
`mixlib timetable --plan PLAN_ID --output timetable.md --json` exports this for
an existing plan without rendering or reselecting anything. It remeasures only
the saved cue windows and uses the exact normal, pause or rhythmic exit profile;
missing evidence remains unknown. Full-set alternatives include the first
lexicographic deciding criterion and problematic joins, within the saved search
budget. These scores are heuristic measurements, not confidence probabilities.


The automatic search enumerates cues and durations together, then evaluates
concrete schedules against selected-layer harmony, lead/bass/drum ownership,
headroom, phrase completion, vocal guards, loop seams and effect tails. A poor
candidate is recorded and skipped. Once full tactic evidence is available,
failing its constraints cannot silently fall back to an unvalidated legacy
crossfade. The command reports infeasibility and retains the previous output.
Automatic `stitch` and chain selection require that evidence; without it they
request `analyze-tactics` or a configured interpreter. Explicit legacy cue modes
and older general-planner schedules retain their labelled legacy behavior;
they do not constitute full tactic admission.

The bounded cue budget preserves short and long durations before admission.
Each search shares a read-only metadata snapshot and decoded source-evidence
cache. Cue-specific timing, ownership and pair measurements are still recomputed.
Before publishing a decision, the engine checks that every metadata file read
by the snapshot is unchanged; rendering independently verifies pinned source
and stem content. This reduces repeated sidecar parsing without making stale
evidence authoritative.

The measured picker now extends admitted entries to endings proposed from
persisted sustained drum energy/pattern changes and structural events. Each pair
has a separate bounded continuation budget equal to its seed budget, with half
reserved for mashups and staged handoffs. A single fill cannot certify a rhythm
change. Proposals go through the same PCM, vocal, timing and layer admission as
seeds. Per-window reports retain the cue, duration, profiles and exact rejections.

M01 requires a measured musical endpoint and can hold longer than six bars,
preserving a final two-bar resolution or admitting a measured swift phrase exit. An arbitrary seed endpoint cannot
qualify it. High-quality M01 holds supply quality-weighted seconds to the set
objective, using worst-case full-window compatibility and per-bar stability. S05/S06
keep their selected layers until that resolution instead of exiting at bar two.
Stability uses the hold as its reference and is checked separately from the exit.
Longer admitted holds receive a bounded score benefit, as do measured end events.
S04 is a last-resort candidate following repeated negative listening feedback.
Its score stays below normal admitted tactics and above unmatched cuts/fades;
fit measurements still rank S04 alternatives within that fallback tier. A
fallback S04 cannot consume the reserved continuation budget for normal held
arrangements. M01 dense mashups and S05/S06 separated-stem handoffs retain their
existing admission, schedules and preference.
Bass swaps have lower tactic preference; shared low-band energy penalizes S04
but cannot by itself reject a measured-compatible band allocation.

Pinned tactic windows validate beat reliability over their synchronized overlap;
solo lead-in/runout can change rhythm without invalidating that overlap. The
vocal scheduler tests later verified gaps when closing at the first pocket
would leave suspected vocal bleed in an audible accompaniment lane. It never
moves accompaniment automation or waives leakage checks to make a gate fit.

S05/S06 now contribute separate `handoff_quality_seconds` to the beam's
sustained-mix objective. Credit requires at least four hold bars, a real retained
bed, stable layers on both decks, and strong pair compatibility. The S06 entry
still requires a measured drum return. Dense and handoff components stay
separate in candidate, edge, join and route diagnostics. Twelve anchor slots
per family prevent either family from crowding the other out before full-route
comparison. The planner version is `sustained-anchor-beam/5`.

Sub-four-bar S03 schedules count as fallback unless measured band matching
supports a short replacement. Longer S03 schedules retain their existing
eligibility. This prevents a short EQ fade from avoiding the short-transition
policy merely by exchanging bass quickly.

The new `rhythmic_handoff` exit proposes a final-half-bar incoming drum accent
near a measured endpoint for M01. It requires energetic, compatible selected
layers through the longer hold, not quiet accompaniment. PCM attack measurement
uses the actual incoming drum source and output time map. The partial final
held bar gets PCM RMS, crest and activity measurements over exactly the
retained interval; energy changes after the handoff cannot lower its hold
density. Full-bar pitch/lead evidence and peak bounds remain conservative. Its profile and
rejections are separate from ordinary and pause/release profiles; the app
compares all admitted variants and retains the ordinary resolve on failure.

`phrase_exit.rs` measures the last two bars of source-aligned bass/other PCM
for a final-half-bar pause or completed local crescendo release. It never uses
muted drum fills as evidence. A 20 ms post-event guard precedes an 80 ms smooth
exchange. The optional typed `phrase_exit` records the event, output interval
and provenance and translates with the chain. M01/M02/S05/S06 retain ordinary
and swift profiles/candidates independently; compatibility is remeasured on
each actual schedule. Admission checks every bar touched by the extended hold,
and dense-anchor value counts its actual duration, including a partial bar.
Known buildups, incomplete vocal lines and accompaniment bleed still veto the
swift variant. Failure leaves the ordinary resolve available. This refinement
uses existing source stems and fused metadata; it adds no per-WAV sidecar.

M01 holds the selected mashup for at least six complete bars and resolves over
the final two bars of its measured window by default. B drums stay steady, A drums stay
absent, bass has an explicit exchange, and vocal gates follow independent
supported phrases with at least a 300 ms guard. M02 retains the spacious bed
and brisk exit. Density and compatibility are measured after choosing audible
lanes; loud source files alone do not establish a mashup. Uncertain vocals or
separation can reject both choices. Local Whisper timing and acoustic support
are evidence for sung phrases, not a guarantee of correct transcription.

To inspect or apply tactics to a saved plan:

```sh
./scripts/mixlib tactics --plan PLAN_ID --json
./scripts/mixlib tactics --plan PLAN_ID --apply --json
# Request one tactic; this still must pass all eligibility checks:
./scripts/mixlib tactics --plan PLAN_ID --apply --tactic M01 --json
# Render the new plan_id returned by --apply:
./scripts/mixlib render --plan NEW_PLAN_ID --output ./renders/selected.wav
```

Inspection includes evidence and rejection reasons. `--apply` writes a new
plan ID; it does not overwrite the original plan or automatically render it.
A requested tactic is a constraint, not permission to bypass evidence.
`--allow-expressive` enables the opt-in performance family for `stitch` or
`tactics`; it never enables emergency recovery during offline synthesis.
D01 and D02 attach to a primary tactic rather than being standalone `--tactic`
choices. Legacy prepared stereo handoffs cannot be rescheduled as independent
stems without their original lane assets.

For a chain, repeat `--then`, or use `--pick-four` / `--pick-eight`:

```sh
./scripts/mixlib stitch --from FIRST_TRACK_ID --to SECOND_TRACK_ID \
  --then THIRD_TRACK_ID --then FOURTH_TRACK_ID --output ./renders/chain.wav
./scripts/mixlib stitch --pick-eight --output ./renders/eight-tracks.wav
```

The automatic picker builds a measured beam from the fully prepared pool.
Each edge retains its cue, tactic, and evidence. Each partial route carries
its entry point and assembled time maps, so separate entries into the same
track can survive. Extensions require 45 seconds of solo source time and
validate the translated schedules together, including vocal tails. Complete
routes rank by fewest last-resort transitions, fewest joins below score 25,
most quality-weighted dense-mashup plus stable-handoff seconds, then weakest join and total fit.
The picker attempts routes around up to 12 measured dense-mashup and 12 stable-handoff anchors plus
an unconstrained beam. Each anchor fixes a concrete entry and endpoint;
`dense_anchor_search` records route outcomes and comparison metrics. This keeps
strong late mashups from disappearing during early forward-beam pruning.
Reachability is anchor-aware. Each anchor exhausts zero-fallback search at
both beam widths before allowing a fallback; counterfactual results retain
the actual join choices and effective search bounds. Return probes have a
separate quota of one third of the base seed budget.
Listening-disfavored S04 stereo band exchanges, unmatched ownership cuts and
ordinary short fades share a default budget of one
last-resort join per set. All-band mapped PCM agreement can promote a cut. Each
measurement pass saves a `measured_graph` report with per-pair tactic admissions
and rejection reasons, linking to separate detailed window reports so the graph
does not exceed the catalog record-size limit. The cue shortlist does not privilege simultaneous vocal
gaps over structural phrase quality.
Dry stem handoffs apply persisted leakage vetoes to the audible lane intervals.
Unknown or suspected bleed is rejected when that deck's vocal is suppressed,
unless the interval has affirmative quiet evidence. Bleed of the same line can
continue under its own fully open, unedited vocal lane. The analyzer's lagged
projection coefficient is not treated as a measured bleed amplitude in dB.
Edited/effected stem captures still require separate source-specific evidence.
Pair evidence version 3 compares added interval conflict against the internal
pitch-class distributions of both passages. It retains spectral disagreement
and the independent supported-note and foreground-lead vetoes. This prevents an
identical full major-scale passage from failing harmony admission against itself.
Cue budgets and beam width bound the search. The export records those bounds
and the measured pool size. Opening and closing cues are selected as part of
validating a complete route. Explicit ordered chains still use their own
per-pair search with one-join lookahead.

`inspect --track TRACK_ID --bars` exposes one- to four-bar segment roles and
measurements. Reliable downbeats/meter determine bars; the engine does not
invent 4/4 for missing rhythm. Separate `unmetered_windows` describe beatless
recordings and uncovered source spans before/after the reliable grid. They can
support structural bridges or natural endings without becoming artificial
bars. D02 requires measured pulse/accent compatibility, not a 2:1 BPM label.
C14 has an explicit independent-pulse path and separate expressive loop-speed
limits; it does not enlarge normal D01 stretch limits.

The earlier manual preparation controls remain available: `--drum-lead-bars`,
`--beat-return-near`, `--incoming-cut-near`, `--vocal-tail-until`,
`--auto-vocal-handoff`, and chain `--band-join`. They constrain specific existing
preparation paths. Prepared excerpts and the new full-track stem schedules
are distinct saved representations; the renderer avoids applying both to the
same audio. A quiet RMS gap alone does not prove a completed lyric.

## Tactic analysis storage and reuse

The `TacticTrackAnalysis` logical record is stored inside the existing WAV
sidecar, alongside the immutable analysis generation. It pins source audio
hash/revision, exact frame count/rate, beat artifact and correction revision,
model provenance, script/pipeline hashes, four full-length stereo stem refs,
source-time vocal evidence, and signal descriptors. Stem WAVs live under
`<library>/stems/tracks/...`, retain original frame zero, and are checksum-pinned.
There is no additional permanent JSON file per WAV, stem, segment or bar.

Signal records keep canonical per-bar measurements and compact references for
overlapping aggregates. Evidence includes reconstruction error, separation
artifact/leakage proxies, local pitch and onset measurements, loop seams,
structural events, and measured vocal absence/presence. Scores are uncalibrated
engineering evidence. A quiet estimated vocal lane plus an affirmative provider
span can establish estimated absence; blank ASR by itself cannot. Unknown or
contradictory evidence remains unknown and can make a tactic ineligible.

Cache reuse follows component dependencies. A signal-only change reuses verified
stems and vocal evidence; a phrase-provider/model change can reuse the stems.
A separator change invalidates both. Source, rate/frame count, beat artifact and
correction-generation checks remain mandatory. A matching ordinary feature
refresh retains the exact supplementary reference. Saved plans can use newer
supplementary evidence only when those pinned identities still match.

Expensive intermediate results stay in a temporary recovery directory until
publication completes. Failures can resume from verified stems/ASR rather than
rerun both models. Publication preserves the original base-analysis reference,
checks immutable asset collisions, atomically updates the active manifest,
and removes completed staging files. Staging JSON is temporary recovery state,
not another authoritative per-WAV metadata file.

Ordinary logical records remain bounded at **8 MiB**; full-track tactic
evidence has a separate **32 MiB** bound. A fused sidecar is bounded at 256 MiB. Staging results have a separate 64 MiB read bound. Selected
song records have passed these limits; long-form recordings may still exceed
them. No automatic record sharding or garbage collection is implemented.

## Live recovery boundary

Offline exports reject E01–E04 rather than using effects to hide an infeasible
join. The separate [`live_recovery`](crates/audio/src/live_recovery.rs) API accepts
an actual source failure, immutable PCM verified while healthy, and an already
verified next source. It prefers a prepared continuation or minimal fade;
E01–E03 additionally require explicit permission and healthy captured material.
Its audit records the fault, rejected safe recoveries, source digests, actual
activation, output cursor, bounded motion/tail and completion/cancellation.

`LiveRecoveryRenderer::activate` performs preparation on a control thread;
`render_into` consumes prepared audio without reopening the failed source.
Tests exercise the recovery state machine and PCM output. **This API is not
wired into the existing Swift Player transport**, and is not a claim of a
complete realtime player or device-lifecycle implementation.

## Implemented boundaries

| Crate | Responsibility |
| --- | --- |
| `mixlib-types` | Shared records, Draft 2020-12 schemas, canonical hashes, diagnostics, cancellation and semantic validation |
| `mixlib-catalog` | WAV discovery/identity, single-writer publication, references, jobs, corrections and derived summaries |
| `mixlib-analysis` | Beat This! RTen adapter, streamed decode, spectral/chroma/band features, local tonal hypotheses and regions |
| `mixlib-planner` | Region/route and cue/tactic search, measured evidence adapters, lane-specific compatibility, schedules and explicit rejection |
| `mixlib-audio` | Bounded WAV decode, linked stereo stretching, stems/EQ/effects/source edits, live recovery buffers, peak checks and atomic export |
| `mixlib-cli` | Commands and macOS playback through `afplay` after rendering |
| `mixlib-apple` | Worker-thread JSON catalogue C ABI; header in `crates/apple/include` |

Each WAV has one physical `.wav.json` sidecar. `analyzer.records` contains the
logical track manifest, all analysis generations/features, correction snapshots,
job state and resumable checkpoints. The former per-track path strings are keys
inside this map, preserving saved-plan references and hashes. Shared configuration,
model files, catalogue summaries, reports and plans remain in the library folder.
A duplicate WAV's sidecar uses `analyzer.shared_from` to reference the primary
sidecar, retaining that recording's own fields. Move WAVs together with their
sidecars; scans reconnect their identity. Changing a primary WAV into different
audio while it has duplicates currently fails explicitly to retain its old
analysis history; changing a duplicate alias is supported.

Existing separate-file libraries remain readable. To migrate one:

```sh
mixlib fuse-sidecars --library ./mix-library
```

Migration validates and publishes sidecars before switching the layout, verifies
that embedded record bytes match, then removes the separate per-track JSON files.
Retrying finishes interrupted cleanup. Saved-plan hashes remain valid. Back up
both the shared library and WAV sidecars, as both contain authoritative metadata.
The sidecar limit is 256 MiB; ordinary embedded records are limited to 8 MiB
and full-track tactic evidence to 32 MiB.
Publishing a record rewrites the complete sidecar; large retained histories have
higher write cost. The migration preflights all tracks in memory.

All authoritative records are JSON. There is no database or persistent binary
metadata index. Generated schemas and default configurations are in `schemas/`.
Regenerate with `mixlib schemas --output Sources/Analyzer/schemas`; typed Rust
validation adds cross-field and interpolation constraints that JSON Schema alone
cannot express. Field names and enum values use `snake_case`; opaque IDs may
contain ASCII letters, digits, hyphens and underscores. Unknown `extensions`
objects survive read/modify/write operations. Unknown schema versions fail.

The decoder supports mono/stereo 16/24/32-bit PCM and 32/64-bit IEEE float,
including extensible PCM headers, at integer rates from 8–192 kHz. It validates
RIFF/chunk bounds, frame alignment, channel masks, finite samples, and format
consistency. RF64, RIFX, compressed WAV and multichannel are explicitly rejected.
Original metadata chunks are never rewritten. The matrix tests cover 44.1, 48,
88.2, 96 and 192 kHz, each encoding, mono/stereo and extensible/plain headers.

Analysis uses one worker, with configurable settling time, duration cap and
feature hop. A two-hour cap bounds feature and neural spectrogram storage.
Feature chunks contain at most 1,024 windows and every JSON record is limited
to 8 MiB (full-track tactic evidence: 32 MiB). Stage identities include audio hash, implementation, relevant config,
and model checksums. A feature-profile update reuses valid rhythm output;
model changes reuse valid spectral features. A saved plan retains its exact
immutable analysis references.

SIGINT is acknowledged immediately. Decode, hash, feature, search and render
loops poll cancellation. The upstream neural `finish()` call is a noninterruptible
boundary; cancellation takes effect when it returns. Sub-stage inference
cancellation and a measured one-second worker-stop guarantee remain open.

## Models, DSP and corrections

The beat-model installer downloads two checksum-pinned assets from the published
`beat-this-rs` revision: the mel front end and converted `small1` beat model.
Downloads resume when HTTP range requests are supported, use temporary files,
and publish only after SHA-256 verification. It takes the same OS writer lock
as the CLI and preserves manifest extensions. The model manifest records
upstream URL, revision, checksum, code/weight license and attribution. Notices
are under `licenses/`; all Rust/build dependencies are inventoried in
`dependency-manifest.json` and pinned in `Cargo.lock`.

The real RTen inference path has been smoke-tested on an original synthetic
rhythm signal and three recorded WAVs spanning approximately three to six
minutes. All three real-recording analyses completed, retained their recorder
fields, and left every source WAV byte unchanged. This verifies execution and
storage, not the musical accuracy of the estimated beats or keys. The selected port's streamed mono/resampling/mel preprocessing
is used directly. Event scores are uncalibrated model activations. Meter is only
accepted after repeated consistent downbeat intervals. Missing/weak pulse stays
unreliable. No claim of parity with the official Python implementation or
accuracy on the user's music collection is made.

The DSP uses 32-tap Hann-windowed sinc resampling, one linked stereo stretcher,
latency-aware priming and absolute rounded input positions. Absolute positions
carry fractional timing across blocks. Time maps use monotone cubic Hermite
interpolation; validation checks derivative extrema analytically and the maximum
rate of speed change throughout each segment. Recovery completes before a track
can start its outgoing transition. EQ is a complementary one-pole three-band
split at 200 Hz and 2.5 kHz; its three components sum to the unfiltered signal at
unity gains. Envelope amplitudes use smoothstep interpolation.

Float32 and seeded TPDF-dithered PCM24 stereo output at 44.1/48 kHz are supported.
An engineering 4× oversampled, 32-tap windowed-sinc peak meter checks the sum;
above −1 dBTP the export fails without publication. This meter is not certified
BS.1770. Oversized RIFF output is rejected before rendering. Previews run the
same timeline/DSP and discard audio outside the requested window. Playback
pre-renders the complete mix and lets the Apple audio host own the device clock;
it does not implement a custom realtime callback.

To edit corrections, save the `overrides` object returned by `inspect --track`
to a separate JSON file, edit its operations, then run:

```sh
mixlib correct --library ./mix-library --track TRACK_ID --input corrections.json
```

Operations cover beat insertion/deletion, downbeat designation, meter, unsuitable
beatmix intervals, cue bounds and directed transition exclusion. Corrections are
anchored to original frames. Exact-frame beat reconciliation is deliberately
conservative; ambiguous edits fail with `OVERRIDE_UNRESOLVED`. Cue-region edits
require review after a new analysis generation. Source-content changes always
require review. Published correction revisions are immutable snapshots selected
by `track.json`; `overrides.json` is their convenient latest copy. See recovery
notes for this small extension to the specification's directory layout.

## Remaining release work

These are incomplete requirements, not optional acceptance criteria that have
already passed:

- Official-reference beat parity, CASM comparison, broader half/double-tempo evaluation,
  retained activation chunks and a representative 50-recording evaluation set.
- Beat-synchronous harmonic aggregates, calibrated tuning evaluation, richer
  modal/unknown hypotheses, and BS.1770 integrated/short-term loudness. Stem
  density, event/absence descriptors and local pulse mapping are implemented
  heuristics requiring calibration. Current gain estimates use RMS and peaks.
- Audition-based tuning of measured selected-lane profiles, effects and source
  edits, broader acoustic refinement, transition indexes and search cost. Tactic
  profiles now use schedule-specific measurements; the older generic region
  scorer still contains conservative priors. No measured 100-track/30-second
  search guarantee is claimed.
- Fault injection with process kills at every publication boundary and actual
  disk-full testing. Covered tests exercise validation/publication failures,
  interrupted-job state, checkpoint reuse and competing writer processes.
- The 40-transition listening study, real acoustic alignment/artifact checks,
  resource benchmarks and two-hour uninterrupted playback gate.
- SwiftUI library/correction views, native buffered playback with interactive
  pause/seek/skip/replanning and output-device lifecycle handling. The C ABI is
  catalogue-only. The existing Swift Player remains unchanged.
- Portable snapshot import/export, source-root relinking commands, future schema
  migrations, iOS clients and iOS lifecycle work. No GC runs: all generations are
  retained, which safely preserves saved-plan and reader references.

The first CLI workflow is runnable, but the full specification must not be
marked complete until these gates are implemented and measured.

## Reproducible checks and samples

```sh
cd Sources/Analyzer
cargo test --locked --workspace
cargo test --locked -p mixlib-planner --test planning variable_grid
cargo test --locked -p mixlib-audio --test render
cargo test --locked -p mixlib-planner --test tactic_evidence
cargo test --locked -p mixlib-analysis --test tactic_retention
cargo test --locked -p mixlib-audio --test tactic_render
cargo test --locked -p mixlib-audio --test live_recovery
# From the repository root, using the configured model environment:
# "$MIXLIB_SEPARATOR_PYTHON" -m unittest discover -s Sources/Analyzer/scripts -p 'test_*.py'
python3 scripts/demo.py --output ./demo-output
# Optional real neural inference instead of the default fallback-only demo:
python3 scripts/demo.py --output ./demo-neural --neural
```

The demo generates CC0 synthetic WAVs, scans, analyzes, plans and renders them,
then proves a repeat scan/analysis leaves active generations unchanged. Generated
analysis, plans, previews and reports remain in the selected output directory.
Variable-tempo pitch/alignment tests use known synthetic grids rather than
claiming a synthetic signal is an acoustic recording. Static small fixtures are
in `tests/fixtures`; example analysis/plan JSON is in `examples/`.


Tactic tests cover all 38 contract IDs and every processing family with positive
and negative fixtures: event-bounded holds with two-bar resolutions, independent vocal gates,
selected-layer compatibility, native/structural timing, bounded effects and wet
tails, explicit source edits/pitch sampling, offline emergency refusal and live
recovery. Storage tests exercise component invalidation, checksum failures,
checkpoint recovery, pinned-generation retention and immutable hardlinks.
These establish executable invariants, not listening quality. Only selected
real recordings are under audit; the broad listening study remains outstanding.

A local Parasite → The Perfect Girl smoke test on 10 November 2026 used the
pinned full-source analysis and the final automatic selector. It examined 22
cue candidates and rendered a 9.260-second, 48 kHz stereo Float32 WAV using S02
(one bar), with no render diagnostics and a measured peak of about −10.64 dBTP.
The sampled long mashups remained ineligible because of phrase uncertainty,
selected-layer instability, separation residual or incompatible material.
This is a successful automatic search/export check, not a real-recording M01
acceptance. M01's six-bar hold, full two-bar fade, drum ownership and independent
vocal gates are verified sample by sample by the dedicated rendered-WAV fixture.

Benchmark commands (run on a fixed reference machine and retain JSON reports):

```sh
/usr/bin/time -l mixlib scan --library LIBRARY --json
/usr/bin/time -l mixlib analyze --library LIBRARY --resume --json
/usr/bin/time -l mixlib plan --library LIBRARY --all --seed 42 --json
/usr/bin/time -l mixlib render --library LIBRARY --plan PLAN_ID --output renders/bench.wav --json
```

Record hardware, OS, power mode, source rates/durations, cold versus warm model
load, and cache state. These commands provide measurement entry points; no
unmeasured performance target is reported as achieved.

## Phrase-aware set pacing

`planner::pacing` owns source-time admission and the pacing preference used by
full-set beam search, its reachability pruning, basic-analysis preparation
shortlisting, and ordered-chain successor checks. Callers pass the previous
join's end and the proposed next join's start. They do not add a fixed runway.

With reliable phase evidence, an early exit must finish at least one complete
inferred phrase after the previous join and land at a supported phrase boundary.
Partial phrases and gaps in phase evidence do not count as completed phrases.
The current phrase estimator supports reliable four-beat bars and inferred
four-bar phrase phase. Other/uncertain material conservatively retains the
45-second admission rule; unsupported phase is never invented.

Four complete phrases are preferred. Each missing phrase contributes 0.75
quality-seconds of pacing penalty (at most 2.25 for an admitted short stay).
This is subtracted from the sustained dense-mashup/handoff objective after
fallback and weak-join counts, before weakest-join and total-fit comparison.
Tactic-specific vocal, buildup, layer compatibility and assembly/overlap checks
remain mandatory. Incoming drum exposure during a mashup is not misreported as
solo playback. Saved joins and the native timetable preserve the exact pacing
measurements and penalty. The planner version is `phrase-paced-anchor-beam/6`.

`inspect --track TRACK_ID --pacing-after SOURCE_TIME --pacing-exit SOURCE_TIME`
reports the native pacing decision without selecting a tactic or changing the
library. This is a pacing audit, not proof that an entire route can render.

Four-track selection retains bounded beam search. Eight-track selection uses
the lazy solver below, sharing admission contracts and optimistic bounds.
Neither strategy claims exhaustive combinations or a globally optimal set.

## Lazy anytime set search

`stitch --pick-eight` defaults to `lazy-constraint-search/3`, with a 1,536-window
and 1,200-second budget. `--search-strategy eager` retains the fully measured
comparison path; four-track selection still defaults to eager.
`search-set --tracks 8 --measurements 1536 --seconds 1200`
searches and saves plans without rendering audio. Each strictly better complete
incumbent is persisted as an independently addressable plan and recorded in a
progress report. A measurement/time/work budget returns the best admitted
incumbent; exhausting the budget without one reports `LAZY_NO_INCUMBENT`, not
proof that the library has no valid set. Cancellation stops work; already saved
incumbents remain available, but cancellation does not silently render one.

The cheap proposal stage creates bounded cue families for every directed track
pair, using the established tempo/cue/phrase and exclusion rules. It does not
compute pair PCM profiles. Each family retains an outgoing cue and an optimistic
incoming-end bound. Dynamic continuations retain those starts and can only
lengthen the overlap. This permits distinct-track/source-runway reachability
checks before audio measurement. Optimistic reachability uses the shortest
entry even if that seed's tactics later fail; it cannot authorize a join.

Surviving branches request one cue-window measurement at a time through the
same `tactics::measure_window` used by eager search. Every discovered tactic and
exit variant retains all existing audible-vocal, buildup, isolation, layer,
headroom and assembly checks. Results are cached within that immutable source
snapshot and reused across routes. The existing dynamic endpoint discovery and
per-pair continuation reserve remain active.

The pruning upper bound gives future joins zero fallback/weak-link/pacing cost,
unlimited total-fit improvement, and up to the maximum possible held duration
in sustained quality. Weighted sustained quality cannot exceed held duration;
dynamic continuation duration is a shared planner constant. Runtime checks
reject a measured edge or continuation escaping that envelope. This bound is
intentionally conservative. Frontier ordering is a separate heuristic and is
never treated as a proof that a branch cannot win.

The search periodically explores shallower alternatives, retains multiple entry
cues, and reserves frontier capacity across depths. Its diagnostics distinguish
cheap/bound rejections, measured windows, cache reuse, frontier truncation,
incumbent improvements and the stopping budget. It remains bounded in cue
proposals, measurements and frontier memory, so it does not claim a global
optimum over every segment of every track. Future work can tighten sound bounds
or make proposal discovery lazy without changing tactic admission or scoring.

`search-set --incumbent PLAN_ID` starts from a saved, verified complete plan.
Track membership, distinctness, source integrity, musical policy and current
phrase pacing must still match. Search-budget settings do not alter musical
policy equivalence. The supplied plan is copied under a new ID and explicitly
labelled `provided_incumbent`; only strictly better sets can replace it. This
provides a no-regression starting point while measurement budgets are refined.
The initial cold benchmark found a clean set quickly but favored bass swaps.
The version 3 cold benchmark independently recovered the reference 53.8024 set
at 1,024 measurements, versus 5,965 windows in the fully measured reference;
this validated switching the automatic eight-track path to lazy discovery.

Time limits are checked between work items; initial cue proposal, an in-flight
measurement and final snapshot verification can extend wall time beyond the
requested limit. Measurement budgets count unique evaluated windows, not tactics.

Incumbent comparison normalizes the generic playlist template/fallback fields
to the short-crossfade scaffold used by measured cue construction. Actual tactic
fallback limits, speed limits and all other musical settings still must match.

### Anchor discovery in lazy search, version 2

Lazy discovery and route expansion now have separate scheduling lanes. Cheap
proposal cost, supported meter, returning-beat probes, measured sustained value,
and per-pair coverage order the next measurement. Raw overlap duration no longer
wins by itself. Eight-to-sixteen-bar probes get early measurement opportunities;
this is a discovery preference, not a rendered duration limit. Measured promising
seeds still expand to the actual musical endpoints through the existing tactic
continuation rules, including longer holds.

Each window is measured once. Its admitted edges also enter a persistent graph
independent of the route frontier. Every 128 measurements and at the measurement
budget, the app reassembles full sets around the strongest measured dense-mashup
and sustained-handoff anchors. Anchors can occur at any position. Both eager and
lazy callers share `measured_beam::assemble_measured`; vocal, buildup, phrase
pacing, time-map assembly and lexicographic quality rules are unchanged.

Discovery ordering is heuristic, not a proof-based prune. Route pruning retains
its conservative bounds. Diagnostics distinguish discovery measurements, anchor
assembly checkpoints and incumbent origins; no previous mix is read unless the
caller explicitly passes `--incumbent`. Previously measured pair reports are not
used as a cache across runs.

Version 3 also learns measurement demand from its own anchor trials. Weak or
fallback joins on routes containing valuable anchors receive a temporary pair
priority; ordinary non-sustained links receive a smaller opportunity bonus.
The bonus decays after each measurement and is recomputed at the next graph
checkpoint. This directs work toward missing connections instead of repeatedly
measuring only the strongest mashup itself. Route-frontier priority uses the
same phrase-probe preference and observed sustained value, removing its raw
length bonus. No external winner, artist identity or hand-picked cue is used.
The last 16 graph checkpoints are retained in the plan; saved incumbent history
and separate pair reports preserve the search evidence.

Automatic pools now deduplicate both audio hashes and nonempty recorder `trackID`
metadata. A second capture of the same source song is not a new song. Existing
prepared captures win over unprepared copies; among prepared copies the earliest
recording is retained deterministically. Missing source IDs fall back to audio
identity, and titles are never used to merge different releases/remixes.
Preparation reports separate analyzed recording counts from unique song counts.

Preparation shortlisting now computes only prepared↔unprepared basic connections
and returns before speculative full-route construction. It retains the existing
bidirectional timing, harmony and phrase-runway ranking; full stem/vocal analysis
still decides actual transition eligibility.

The 19-song expansion benchmark exposed a remaining coverage limit: the default
1,536-window cold run scored 52.1856 and missed one reference S05 cue, while the
15-song cold run recovered 53.8024 at 1,024 windows. A larger pool is not a
guarantee of a better result at a fixed budget. Previously saved winners remain
available; a cold search does not silently seed itself with those plans.

## Set composition (2026-11-11)

`search-set` now defaults to a duration-constrained hour; `--set-minutes 120`
selects 117–120 minutes. Explicit `--tracks 8` retains the short benchmark path.
`--structure story|wave|ramp|chapters` selects a documented acoustic arc; auto
uses story for an hour and chapters for two. See `SET_STRUCTURE_TACTICS.md` for
research, exact initial weights, assumptions and unimplemented semantic tactics.

The CLI composition owner consumes already admitted measured edges. It searches
variable song counts, scores actual played source bars against output-time arc
positions, weights developmental dwell, retains elapsed/entry diversity, and
checks finished bookends against the duration window. It cannot change DSP,
relax vocal/buildup admission, pad audio or return an undersized set as complete.
Later exit proposals are protected at 35% and 60% of outgoing source duration.
Saved plans and timetables include `set_composition` evidence. This is bounded
heuristic search; it does not certify global optimality or semantic emotion.

Full `analyze-tactics` preparation now deduplicates available source song IDs,
retains prepared captures, and visits every unique song instead of a shortlist.
On machines with at least eight logical CPUs it runs two four-thread separator
jobs, serializes fused-sidecar publication, and persists batch progress. Explicit
track IDs still address particular recordings. Source data and the one-JSON-per-
WAV layout are preserved.

M04 timetable wording now identifies its isolated motif overlay and does not
present zero held bars as a full-mix hold/resolve. Perceptual masking of a quiet
motif beneath strong incoming material remains distinct from set composition.
