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

M01 holds the selected mashup for six complete bars and crossfades accompaniment
from elapsed bar 6 to bar 8. B drums stay steady, A drums stay absent, bass has
an explicit exchange, and vocal gates follow independent supported phrases
with at least a 300 ms guard. M02 retains the spacious bed and brisk exit.
Density and compatibility are measured after choosing the audible lanes; loud
source files alone do not establish a mashup. Uncertain vocals or separation
can reject both choices. Local Whisper timing and acoustic support are evidence
for sung phrases, not a guarantee that the transcription or phrase is correct.

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

Route ranking considers local beat windows, stretch, chroma, energy, remaining
runway and the next join. Each admitted join retains its own tactic and evidence.
Middle tracks remain continuous in source time, with a 45-second solo reserve
between joins and a checked outgoing continuation. Opening and closing cues
seek an intentional entry and a supported ending. Those are conservative
planning heuristics, not proof that a whole route is musically complete.

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

The persisted logical-record limit remains **8 MiB**, and a fused sidecar is
bounded at 256 MiB. Staging results have a separate 64 MiB read bound. Selected
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
The sidecar limit is 256 MiB; each embedded record remains limited to 8 MiB.
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
to 8 MiB. Stage identities include audio hash, implementation, relevant config,
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
and negative fixtures: fixed six-plus-two envelopes, independent vocal gates,
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
