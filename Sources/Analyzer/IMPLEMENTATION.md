# mixlib engineering CLI

The Rust CLI now implements the scan → analysis → planning → WAV-render workflow.
This is an engineering implementation of the first four milestones, **not a
completed first release**. The product specification remains in [README.md](README.md).
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
The core never makes network requests. Python is used only by optional developer
and model-install helpers, not by the shipped analysis/render executable.

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

## Implemented boundaries

| Crate | Responsibility |
| --- | --- |
| `mixlib-types` | Shared records, Draft 2020-12 schemas, canonical hashes, diagnostics, cancellation and semantic validation |
| `mixlib-catalog` | WAV discovery/identity, single-writer publication, references, jobs, corrections and derived summaries |
| `mixlib-analysis` | Beat This! RTen adapter, streamed decode, spectral/chroma/band features, local tonal hypotheses and regions |
| `mixlib-planner` | Directed region candidates, stretch/recovery constraints, explicit fallbacks, deterministic bounded beam search |
| `mixlib-audio` | Bounded WAV decoder, resampling, linked stereo Signalsmith stretching, EQ/gain automation, meter and atomic export |
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

The installer downloads only two checksum-pinned assets from the published
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

- Official-reference beat parity, CASM comparison, half/double-tempo alternatives,
  retained activation chunks and a representative 50-recording evaluation set.
- Beat-synchronous harmonic aggregates, calibrated tuning evaluation, richer
  modal/unknown hypotheses, BS.1770 integrated/short-term loudness, rhythmic
  density and explicit silence-boundary artifacts. Current gain estimates use
  RMS and peaks and are labeled accordingly.
- Candidate scoring of the actually filtered overlap, audio-level refinement,
  transition caches/coarse indexing and listening-tuned costs. Several current
  scoring components are explicitly conservative unknown priors. Search has a
  deterministic work cap but no measured 100-track/30-second guarantee.
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
