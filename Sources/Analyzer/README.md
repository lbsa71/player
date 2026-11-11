Implementation entry point: [CLI build, usage, tested behavior and remaining release gates](IMPLEMENTATION.md).
The specification below remains the acceptance contract; the engineering CLI does not yet satisfy every release gate.

Transition selection contract: [Mixing tactics, A/B eligibility rules, timing and research](MIXING_TACTICS.md).

Build a local music player that analyses a folder of WAV files, selects a suitable play order, and automatically performs reasonably smooth transitions. The application should retain the expressive timing of acoustic recordings while supporting music with a steady pulse. Its scope is playlist selection and automated mixing, with a small interface for reviewing and correcting results.

The system has two computational passes: an incremental library analyser and a mix planner. A separate renderer executes the resulting plan for previews, continuous playback or WAV export. Rust owns the reusable core. macOS is the first delivery platform; the architecture must allow a later iOS client.

**Metadata storage is JSON files in the filesystem. SQLite, other embedded databases, and database services are excluded.** Audio files, model weights and rendered audio may use their appropriate binary formats. All authoritative catalogue, analysis, configuration, correction and mix-plan data must remain inspectable JSON.

Specification version: 1.0. Date: 6 November 2026. MUST indicates a required behaviour; SHOULD indicates the expected implementation unless a documented reason justifies another choice. Numerical defaults and performance targets below are initial engineering targets to validate against the actual music collection.

# Product scope

The user selects one or more music directories, starts analysis, chooses a pool of tracks and requests a mix. The application proposes a sequence, explains weak transitions, provides short transition previews, and plays or exports the mix. New files are incorporated without repeating valid analysis of unchanged audio.

The first complete release MUST provide variable beat grids, local harmonic analysis, entry and exit regions, playlist planning, pitch preservation during tempo changes, three-band EQ transitions, gain management, previews and continuous output. A track may contain tempo drift, meter changes, modulation, silence or passages with no reliable pulse. These conditions must be represented explicitly rather than forced into one BPM, one key or a regular four-beat bar.

The first engineering milestone is a macOS command-line application. The macOS graphical client follows once the full analysis-to-render workflow is usable. iOS, stem-assisted mixing and learned transition ranking are later extensions. The initial release does not require streaming services, cloud analysis, controller support, scratching, a sampler, generative music or a general-purpose effects rack.

Original WAVs MUST remain unchanged. The default recording folder is `~/Documents/Audio`, using the recorder's `<album> - <artist> - <track name>.wav` naming. Each WAV MUST have only one JSON sidecar, `<filename>.wav.json`. Preserve the recorder's existing top-level fields and add analyzer metadata under `analyzer`. Shared library records and rendered audio belong outside the source folder. Partial recordings and recordings awaiting finalized recorder metadata MUST be deferred.

# Architecture and execution boundaries

| Module | Responsibilities | Outputs |
| - | - | - |
| Catalogue | Source discovery, identity, change detection, availability and job scheduling | Track manifests and rebuildable catalogue summaries |
| Analysis | Decode audio, infer rhythm, extract harmonic and spectral features, identify usable passages | Immutable analysis generations |
| Planning | Retrieve candidate passages, evaluate transitions and search for a playable sequence | Immutable mix plans and transition scores |
| Rendering | Read source audio, apply time maps, EQ and gains, produce aligned output | Preview audio, playback buffers or a rendered WAV |
| Apple integration | Folder access, app lifecycle, audio sessions and user interface | Platform services around the Rust core |

Pass one MUST be independent of any particular playlist. Pass two MUST normally work from saved analysis and selectively decode only shortlisted passages when an audio-level check is necessary. Reordering tracks must not trigger full-track neural analysis.

The renderer MUST execute explicit decisions from the plan. It must not silently choose different cue points, stretch policies or transition types during export or playback. If a plan cannot be executed, return a diagnostic and require replanning or use a fallback already recorded in the plan.

Each algorithm MUST sit behind a versioned interface. Replacing beat decoding, harmonic estimation or transition scoring must not require changing source discovery or the playback client. Research tools may be used during development and evaluation, but the initial shipped macOS core SHOULD run without a Python installation.

# Filesystem metadata design

## Directory layout

The storage revision of 7 November 2026 fuses per-track records into the existing
recorder sidecar. This supersedes the original separate-file layout. The default
shared metadata directory is `~/Library/Application Support/Player/Analyzer`;
`--library` or `MIXLIB_LIBRARY` can select another directory.

| Physical path | Contents and authority |
| - | - |
| `<source>/<filename>.wav.json` | Recorder fields plus the authoritative `analyzer` bundle |
| `<library>/library.json` | Library UUID, source roots and metadata layout version |
| `<library>/config/{analysis,mixing}.json` | Analysis and planner configuration |
| `<library>/models/manifest.json` | Model identifiers, checksums, provenance and licences |
| `<library>/catalog/sidecars.json` | Rebuildable track-to-sidecar location index |
| `<library>/catalog/manifest.json` and generation shards | Rebuildable catalogue summaries |
| `<library>/plans/<plan_id>.json` | Complete playback order and rendering instructions |
| `<library>/reports/<report_id>.json` | Scan, validation and render diagnostics |
| `<library>/locks/writer.lock.json` | Stable shared writer lock and owner diagnostics |

The `analyzer` object has its own schema version, library/track/bundle IDs, an
optional `shared_from` location for identical audio, and a `records` map. Existing
recorder fields retain their camelCase names; analyzer fields use snake_case.
An identical WAV at another location keeps its own recorder metadata and a
reference to the primary sidecar that owns the shared analysis.

Record keys such as `tracks/<shard>/<track_id>/track.json`,
`tracks/<shard>/<track_id>/analysis/<analysis_id>/beats.json`,
`tracks/<shard>/<track_id>/overrides/<revision>.json`, `jobs/<track_id>.json` and
`staging/<track_id>/checkpoint.json` are **logical keys inside `analyzer.records`**,
not extra physical files. References and checksums resolve these embedded
records through the catalogue storage adapter. All historical generations,
feature chunks, corrections and job checkpoints stay in that single sidecar.

Shards use two hexadecimal characters derived from record IDs. Numeric features
remain chunked into logical records of at most 8 MiB (full-track tactic evidence:
32 MiB); the complete sidecar is
limited to 256 MiB. Exceeding either limit fails while retaining the previous
valid sidecar. Publishing a record atomically replaces the whole sidecar, so
large analyses incur more write traffic. There is no giant JSON file containing
every track's full analysis. Models, previews and rendered audio remain separate
binary assets.

Catalogue summaries and transition caches are disposable accelerators. Authoritative track manifests, analysis generations, corrections and plans MUST be sufficient to reconstruct them. Searching may build an in-memory index. A persistent vector database or opaque binary metadata index is not part of this design.

## JSON conventions and schemas

FS-01. Analyzer records MUST be UTF-8 JSON with a `schema_version` and a stable record identifier. In a fused recorder sidecar these fields belong to the nested `analyzer` object and its records, leaving the recorder envelope intact. Use JSON Schema Draft 2020-12 and typed Rust serialization. Validate semantic constraints in Rust as well as structural schemas.

FS-02. Keys and enum values MUST use a documented naming convention. Human-edited configuration and summaries SHOULD be pretty-printed. Dense numeric arrays may be compact JSON. NaN and infinity are forbidden; unavailable values use `null` with a reason where needed.

FS-03. Machine timestamps MUST use UTC RFC 3339. Source timing MUST use integer sample-frame positions plus the source sample rate. A frame contains one sample per channel. Output timing MUST use a separately declared output sample rate. Frame indices must be nonnegative and below 2^53 for safe interoperation with common JSON readers.

FS-04. A stored key estimate MUST declare tonic encoding, mode vocabulary, window bounds and score semantics. A beat confidence or key score must not be described as a calibrated probability unless calibration has been measured. Chroma records MUST declare pitch-class order, feature method, tuning reference, bin count and normalisation.

FS-05. Unsupported schema major versions MUST fail explicitly. Migrations MUST preserve the old valid generation until the new one is committed. Reserve an `extensions` object for optional additions; editing supported fields must preserve unknown extensions.

FS-06. Hash inputs MUST use a documented canonical JSON representation and reject non-finite values. Cache keys must not depend on incidental whitespace, JSON key order or creation timestamps.

## Core record contracts

The following fields form the minimum logical contracts. Published JSON schemas must define their exact required/optional status, enums, dimensions and cross-field constraints. IDs are strings; content hashes declare their algorithm; frame positions are integers; scores and gains are finite numbers. File references pair logical record keys (or shared-library relative paths) with content hashes.

| Record | Required field groups |
| - | - |
| Library | `schema_version`, `library_id`, `source_roots`, `configuration_refs`, `created_at` |
| Source root | `root_id`, `display_name`, `location_kind`, platform locator or portable relative location, availability |
| Track | `track_id`, `audio_revision`, audio-format descriptor, source locations, active analysis reference, override revision, lifecycle status |
| Analysis manifest | `analysis_id`, source audio hash, stage identities and dependencies, checkpoint/preprocessing hashes, artifact references, quality flags |
| Beats | Source sample rate, beat events with stable event IDs and source frames, downbeat references, meter segments, unreliable intervals, score semantics |
| Harmonic features | Window bounds or declared hop, chroma bin order and normalisation, tuning convention, numeric feature chunks, local tonal candidate intervals |
| Region | `region_id`, entry/exit role, bounds, beat references, tempo/harmonic/energy summary, available overlap, quality and exclusion reasons |
| Overrides | `revision`, base audio hash, base analysis reference, correction operations, user preferences and migration status |
| Transition | Source/destination IDs and revisions, region IDs, cue bounds, template, time maps, automation, component scores, fallback/validity information |
| Mix plan | `plan_id`, engine/policy identities, seed, requested objective, ordered track placements, transition references or embedded instances, output format and diagnostics |

An Apple bookmark may be represented as base64 in a platform-locator object inside JSON. It is opaque to the portable core and must not replace the stable root ID. A portable export excludes device-specific locators and prompts for relinking on import.

Corrections MUST be anchored to original audio frames and retain their base audio hash. A model update may change beat IDs, so replaying an override against a new analysis requires explicit reconciliation by time and meaning. If reconciliation is ambiguous, keep the correction unresolved and expose it for review; do not silently attach it to a different beat. Audio-content changes require review of affected corrections.

## Identity and invalidation

Each track receives a stable UUID. Path is a location, not identity. A track can have several source locations, and a moved or renamed file should reconnect to an existing identity when its content can be verified.

Store a whole-file SHA-256 and a separate audio-content SHA-256. The latter MUST cover a deterministic descriptor of the audio format and the ordered WAV audio payload, excluding unrelated metadata chunks. Include sample rate, channel layout, encoding and valid bit depth. Document this hashing format and test it with fixtures. Equivalent audio in a different encoding may conservatively receive a different audio hash.

File size, modification time and platform file identity are discovery hints. Ordinary scans may use them to skip rehashing unchanged files. A `--verify-content` scan MUST reread and verify all selected files, covering replacements that preserve size and timestamps.

The effective analysis key MUST include the audio-content hash, stage implementation versions, checkpoint hashes, preprocessing parameters and configuration affecting the result. Manual corrections receive their own revision. Dependent catalogue summaries, candidate scores and plans must record the correction revision they used.

Metadata-only WAV changes that leave the verified audio-content hash unchanged MUST preserve reusable analysis. Audio changes invalidate dependent analysis. A model update invalidates only the stages using that model and their downstream products. Stem outputs, if enabled later, also require their own model and settings identity.

## Atomic publication and concurrent access

FS-07. The first implementation MUST use a single writer coordinator per metadata directory. Worker threads may compute concurrently, but the coordinator controls publication. A second writer process must exit clearly or wait under an explicit option; it must not race the active writer.

FS-08. Acquire an operating-system advisory lock on the stable lock-file inode. Diagnostic PID and timestamp fields are not sufficient proof of lock ownership. Never replace the locked inode while holding the lock. Process termination must release the OS lock without requiring unsafe PID-based lock stealing.

FS-09. Write each file to a unique temporary path on the same filesystem, flush it, validate it, and atomically rename it into place. Synchronise file and directory state as supported by the platform. Disk-full, permission and cancellation errors must leave the previous committed record readable.

FS-10. Analysis generations are immutable after publication. Write and validate all referenced logical records before atomically replacing the sidecar with an updated `track.json` record selecting the new generation. Readers retain a snapshot reference and therefore see either the old complete generation or the new complete generation. A catalogue update can lag behind; it must never be required to recover authoritative track state.

FS-11. Completed stages in staging MUST carry checksums and input/version identities so an interrupted job can reuse them safely. On restart, validate before reuse. Corrupt or incomplete work is isolated and regenerated without deleting a last-known-good analysis.

FS-12. Garbage collection must retain every generation referenced by a saved plan, active reader, current track manifest or retained history policy. Deletion of source audio is never part of metadata garbage collection.

FS-13. The first release guarantees concurrent read access and one writer on a local filesystem. Simultaneous writers on different machines or iCloud-synchronised copies are outside scope. iOS portability uses an exported snapshot or a single owning device, not opportunistic file merging.

# Pass one source discovery and analysis

## Discovery and job lifecycle

AN-01. Recursively discover `.wav` files case-insensitively. Support several roots and relative source paths. Do not follow symbolic links by default; if enabled, prevent cycles and duplicate traversal. Exclude the metadata and output directories from discovery.

AN-02. Detect new, changed, unchanged, moved, duplicate and missing sources. Identical verified audio should share expensive analysis; duplicate locations should not become repeated musical selections unless the user enables that behaviour. Missing files retain metadata and become unavailable for new plans.

AN-03. Defer a file whose size or modification state changes during a configurable settling interval or during analysis. Verify the source revision before publication. A partially copied file must not become a completed analysis.

AN-04. Persist job states `queued`, `running`, `completed`, `failed`, `cancelled` and `deferred`, with stage progress, diagnostic codes and retry counts. Recover interrupted `running` jobs on the next launch. A bad WAV must not terminate the whole scan.

AN-05. A watcher MAY request a rescan while the application is active. Startup and manual scans remain authoritative. This behaviour belongs to the application, not an external scheduled service.

## Audio input and preprocessing

AN-06. The initial input contract MUST cover mono and stereo RIFF/WAVE files containing 16-, 24- and 32-bit integer PCM or 32- and 64-bit IEEE floating-point PCM, including applicable WAVE_FORMAT_EXTENSIBLE headers. Preserve BWF and unknown metadata chunks by never rewriting the source. Test 44.1, 48, 88.2, 96 and 192 kHz sources; declare the actual decoder's other supported rates.

AN-07. Unsupported compressed WAV encodings and channel layouts must produce actionable errors. Multichannel downmixing requires an explicit documented policy. Malformed headers, truncated payloads, non-finite samples and unsupported large-file containers must be detected. RF64 input may be added as a separately tested capability.

AN-08. Model preprocessing MUST match the reference implementation, including channel reduction, resampling, amplitude scaling, spectral windows, padding and chunk overlap. Keep an exact mapping back to original sample frames and compensate for deterministic preprocessing delay. Detect destructive stereo cancellation when making mono analysis input and flag affected results.

AN-09. Bound memory by processing chunks and limiting concurrency. Cache only features that have a defined downstream use. Original audio must not be normalised or time-stretched as a preprocessing side effect.

## Rhythm analysis

AN-10. Use Beat This! as the initial neural beat/downbeat baseline. Evaluate the available Rust port against the official implementation before adopting a checkpoint/backend combination. The port offers RTen and optional ONNX Runtime backends; this is an implementation candidate, not evidence of identical accuracy on the user's collection. [Beat This! reference](https://github.com/CPJKU/beat_this) and [Rust implementation documentation](https://docs.rs/crate/beat-this/latest/source/README.md).

AN-11. Store strictly increasing beat and downbeat sample-frame events, event scores, inferred bar positions where reliable, meter segments and explicit unreliable/free-time intervals. Downbeat events must correspond consistently to the chosen beat interpretation. Retain half/double-tempo alternatives when ambiguity materially affects transitions.

AN-12. Derive local BPM from inter-beat intervals and provide a separately smoothed display/search curve. Define the pulse unit, especially for compound meter. Do not treat every onset as a beat or every tempo fluctuation as an error. The beat grid is the event timeline, not a fixed BPM plus offset.

AN-13. Retain chunked beat activations when the chosen backend exposes them and when later decoding benefits justify storage. Compare direct decoding with CASM as a replaceable decoder. CASM must pass the same benchmark and licence checks before becoming a default. [CASM implementation](https://github.com/zhanh-he/casm-beat-tracking).

AN-14. Identify low-confidence sections, abrupt interpretation changes, missing beats and inconsistent bars. Manual correction must support beat insertion/deletion, downbeat changes, meter overrides and marking an interval unsuitable for beat mixing.

## Harmonic and band analysis

AN-15. Compute pitch-class features on a regular short-time grid and aggregate them into beat-synchronous features. Retain enough within-beat detail to inspect short harmonic changes; beat averaging alone must not be the only saved evidence. Estimate tuning offset and preserve the tuning convention in the result.

AN-16. Estimate local key candidates over multiple context lengths. Initial contexts may span approximately 4, 8 and 16 reliable bars, with a seconds-based fallback when meter is unknown. Apply temporal regularisation to suppress unsupported key changes. Preserve ambiguous, modal and unknown outcomes instead of forcing a major/minor label.

AN-17. Store the pitch-class timeline even if the discrete key estimator is uncertain. Chord/root estimates may supplement it, but must not be mandatory evidence when unreliable. A chord change is not automatically a modulation.

AN-18. Extract low/mid/high energy envelopes, onset strength, rhythmic density, spectral balance, integrated/short-term loudness, peaks and silence boundaries. Analysis crossover frequencies must be configurable and recorded. An initial three-band profile may use 200 Hz and 2.5 kHz; these are starting settings, not universal musical boundaries.

AN-19. Band comparison MUST account for harmonic interactions between retained bands from different tracks. Do not infer separate authoritative keys for bass, mids and highs. If a transition uses different filters from the analysis profile, recompute the affected candidate features or evaluate the filtered audio directly.

AN-20. A vocal/lead activity detector MAY improve ranking. Absence of a detector or uncertain predictions must be represented as unknown. Optional stem separation belongs to pass one or a resumable candidate-refinement job, with saved provenance. It is not required for the first EQ-based release.

## Structure and transition regions

AN-21. Detect structural changes from beat-synchronous harmonic, rhythmic and energy features. Compare a conventional novelty/self-similarity baseline with a learned structure model when a distributable implementation is available. All-In-One is an evaluation candidate for beats, downbeats and functional sections; converting and deploying it is a separate integration task. [All-In-One](https://github.com/mir-aidj/all-in-one).

AN-22. Generate multiple entry and exit regions. Each region must include source bounds, beat/downbeat references, available overlap length, local tempo envelope, harmonic summary, energy, reliability and any exclusion reasons. Regions need not be limited to file beginnings and endings.

AN-23. Prefer musical boundaries and preserve enough playable material between entry and exit. Do not assume every phrase is 8 or 16 bars. Sections with weak pulse, unresolved meter, rapid harmonic change or strong lead activity may require shorter overlaps or a fallback transition.

# Pass two transition evaluation and playlist planning

## Planning request

PL-01. Accept an eligible track pool and a policy. Required policy fields include ordering mode, seed, repeat policy, transition limits, duration/coverage objective and optional fixed start/end tracks. Support both `order_all`, which uses every selected available track once, and `target_duration`, which selects a subset. Use `order_all` when only a folder or track selection is supplied.

PL-02. Include minimum played duration or fraction, permitted cue truncation, maximum stretch, overlap lengths, meter rules, energy preferences and fallback policy. Preserve fixed user choices unless they make the request infeasible; then explain the conflict.

PL-03. Return a complete plan or an explicitly partial result with reasons. Never silently omit required tracks. For an infeasible all-track request, identify the blocking tracks/constraints and propose a plan with permitted fades or boundary transitions if the policy allows them.

## Candidate retrieval and scoring

PL-04. Treat A-to-B as a directed relationship. Compare an outgoing region of A with an incoming region of B under a concrete transition template. Keep several candidates per track pair; a single global pairwise compatibility number is insufficient.

PL-05. First shortlist candidates using local tempo, meter, harmonic features, energy and density. Expand the shortlist when necessary. Avoid decoding and deeply scoring every possible track pair. A small library may use exhaustive candidate enumeration; larger libraries should use an in-memory coarse index built from JSON summaries.

PL-06. Generate candidate cue points, overlap lengths, beat correspondences, time maps and EQ/gain envelopes jointly. Scoring MUST refer to the material that remains audible after those transformations. Same-key or neighbouring-key relationships are retrieval hints, not a guarantee of compatible simultaneous chords.

PL-07. Apply hard validity checks before ranking: available sources, valid chronological ranges, reliable pulse for a beatmix, compatible beat interpretation, monotonic time maps, allowed stretch, adequate played material and no forbidden lead/vocal overlap when such activity is known.

PL-08. Rank valid candidates using separate recorded costs for timing/groove mismatch, harmonic conflict, spectral crowding, bass collision, loudness/energy discontinuity, phrase disruption, uncertain analysis and processing artifacts. Each cost must declare its normalisation and weight. Initial weights are heuristics to be tuned with listening tests; do not present the aggregate score as a validated probability of listener satisfaction.

The scoring objective must preserve the intended musical handoff. Muting both tracks or removing nearly all harmonic content cannot be rewarded simply for eliminating clashes. Templates must constrain minimum audibility, reasonable spectral balance and successful arrival at the incoming track.

PL-09. Candidate evaluation SHOULD render or inspect a short transformed overlap for the best few candidates, checking actual alignment, combined energy and artifacts. Cache evaluations using both audio hashes, effective analysis/correction revisions, transition parameters, scoring version and rendering configuration.

## Sequence search and state

PL-10. Use deterministic beam search as the initial sequence planner, with a recorded seed for tie-breaking and optional variation. A bounded local-improvement pass may follow. Keep a greedy baseline for evaluation. The search must respect a time/work budget and report the quality and feasibility of its best result.

PL-11. Search state must include used tracks, accumulated output duration, the selected entry state of the current track, feasible exit regions, tempo state and applicable sequence preferences. Two good pairwise transitions are invalid if their entry/exit ranges cannot coexist within the middle track.

PL-12. The initial tempo policy keeps the outgoing passage on its native timing, maps the incoming passage to it during the overlap, then returns the incoming track smoothly to its native timing. That recovery interval must finish before its next outgoing transition. More general shared tempo trajectories are an extension, not an implicit assumption.

PL-13. Calculate total output duration from the actual time maps, trims and overlaps, not the sum of source durations. Enforce no-repeat rules using verified audio identity as well as track ID. A plan must record any deliberately allowed duplicate recording.

PL-14. Replanning after a skip or track exclusion must preserve the already committed playback horizon and return a new immutable plan revision. The renderer must never jump to an incompatible new state midway through an active transition.

# Transition policies and mix plan contract

The first release MUST implement three transition templates: a short crossfade, a beat-aligned blend with a bass handoff, and a beat-aligned three-band exchange. A fade or clean boundary transition is a valid fallback when reliable beat mixing is unavailable. Fallbacks must be visible in previews and plan diagnostics.

| Initial policy | Default and interpretation |
| - | - |
| Overlap candidates | 4, 8 and 16 bars within a stable meter segment; shorter phrase-aligned choices may be generated |
| Playback speed | Incoming speed initially limited to 0.94–1.06 of native speed; 1.06 means consuming source frames 6 percent faster |
| Pitch | Preserve original pitch; automatic transposition disabled initially |
| Pulse interpretation | Prefer the same metrical interpretation; half/double-time requires explicit compatible evidence |
| Native timing recovery | Smooth and completed before the next outgoing transition |
| Unreliable grid | Use a permitted fallback or mark the candidate infeasible |
| Coarse shortlist | Start with 32 incoming candidates per outgoing region and expand under the planner budget |
| Search | Start with a beam width of 32; record the actual value used |

The permitted stretch range and rate-of-change limit MUST be distinct controls. A curve staying within 6 percent can still sound poor if it changes rapidly. Defaults must be tuned on the target repertoire, and a user may explicitly select a more permissive profile.

MP-01. A saved plan MUST include its schema version, plan ID, policy hash, planner version, seed, output format, track sequence, source audio hashes, effective analysis/correction references, output placements, transition instances, time maps, gain/EQ envelopes and diagnostics.

MP-02. A time map is a strictly increasing sequence of source-frame/output-frame anchor pairs with a declared interpolation method. The implementation must guarantee monotonic interpolation, finite rates and no overshoot beyond policy limits. Source and output frame rates must be declared separately. Every audible source range, including tempo recovery and stretcher pre-roll, must be available.

MP-03. EQ and gain automation must use explicit units and timebases. Declare crossover design, cutoff frequencies, slopes, gain units and interpolation. Store silence/mute states explicitly rather than non-finite negative-infinity numbers.

MP-04. Plans are immutable and reproducible against pinned source revisions and algorithm versions. Before rendering, verify source identity and referenced artifacts. Changed or missing audio invalidates execution; it must not be silently substituted. Cross-platform floating-point differences need numerical tolerances, not a promise of byte-identical output on every backend.

The following is a valid JSON example of the time-map representation. It illustrates one second of local alignment, not a complete mix plan or a recommended transition length.

```json
{
  "schema_version": 1,
  "map_id": "example_incoming_alignment",
  "source_sample_rate_hz": 44100,
  "output_sample_rate_hz": 48000,
  "interpolation": "monotone_cubic",
  "anchors": [
    {"source_frame": 0, "output_frame": 2880000},
    {"source_frame": 22050, "output_frame": 2904000},
    {"source_frame": 44541, "output_frame": 2928000}
  ],
  "speed_ratio_limits": {"minimum": 0.94, "maximum": 1.06},
  "preserve_pitch": true
}
```

The second interval consumes 0.51 seconds of source audio in 0.50 seconds of output. The full plan must also provide boundary conditions and recovery anchors, and validate the derivative throughout interpolation rather than checking anchor spacing alone.

# Rendering and playback requirements

AU-01. Decode original sources into a common internal floating-point representation while preserving stereo relationships. Resample explicitly to the selected output rate. Time stretching must operate coherently across channels; independent channel processing that moves transients differently is unacceptable.

AU-02. Use a tested polyphonic time-stretch implementation behind a small interface. Signalsmith Stretch is the initial candidate because its implementation is available under MIT and documents time/pitch processing and latency. Rubber Band is an alternative to compare for quality and integration under its stated GPL/commercial distribution options. Neither is declared universally superior. [Signalsmith Stretch](https://github.com/Signalsmith-Audio/signalsmith-stretch) and [Rubber Band](https://breakfastquay.com/rubberband/).

AU-03. Compensate input/output latency and provide sufficient pre-roll and tail flushing. Verify realised alignment in rendered audio. Carry rounding residuals through block processing so variable ratios do not accumulate sample drift.

AU-04. Preserve expressive timing within beats where possible. Do not quantise every onset or force swing subdivisions straight. If reliable beat anchors require audible rapid stretching, reject the long blend or choose a shorter transition.

AU-05. EQ automation MUST be smoothed and free of zipper noise. The crossover/filter implementation must be tested for reconstruction, phase behaviour, transient response and gain changes. Bass handoff should avoid two dominant bass parts overlapping at full level while maintaining sensible perceived energy.

AU-06. Plan loudness gains with headroom and inspect the sum. A limiter is optional protection, not a substitute for suitable gain envelopes. The initial export profile targets no more than −1 dBTP true peak, measured using a documented oversampled meter. Report peak and gain behaviour in a render report.

AU-07. Use the same DSP implementation for preview, offline export and playback. Offline rendering and buffered playback may use different scheduling, but must follow identical plan semantics. Pre-render transition passages when this improves reliability; their cache identity must include the renderer version and output settings.

AU-08. The device audio callback must perform bounded work with preallocated buffers. No filesystem access, JSON parsing, neural inference, blocking locks or memory allocation may occur in that callback. Decoding, planning and expensive rendering run ahead on workers and communicate through bounded buffers.

AU-09. Handle pause, seek, skip, source unavailability, audio interruption and output-device changes without crashes. A seek must reset/reprime stretch/filter state. A skip may only change the uncommitted future of the plan; until a new valid transition is ready, continue the current track or apply a documented fade policy.

AU-10. Export stereo WAV at 44.1 or 48 kHz with selectable 24-bit PCM or 32-bit float output. Apply documented dithering when reducing to integer precision. Use RF64 when a supported export exceeds ordinary RIFF size limits, or reject the requested export size explicitly before rendering. Write to a temporary output and publish the final filename only after successful completion and validation.

# Rust implementation and Apple clients

## Core modules and dependencies

Organise the Rust workspace around stable domain boundaries: `catalog`, `analysis`, `planner`, `audio`, `cli` and an Apple binding crate. Shared data types and JSON schema generation must have one owner. Use Rust errors with stable diagnostic codes across the CLI and graphical clients.

Candidate infrastructure includes `serde`/`serde_json` for records, a JSON Schema validator, `sha2` for SHA-256, a filesystem walker/watcher, a WAV-capable decoder, and FFT/resampling libraries. Pin tested dependency versions in the application lockfile. Select the WAV decoder against the explicit format test matrix rather than assuming that any crate advertising WAV covers every required subtype. Symphonia is a candidate Rust decoder. [Symphonia](https://github.com/pdeljanov/Symphonia).

Use RTen CPU inference as a simple deployment candidate for the Beat This! port. Evaluate ONNX Runtime and its Core ML execution provider separately on target Apple hardware. Model conversion must preserve preprocessing and outputs within tested tolerances; enabling Core ML does not establish that every operation executes on the Neural Engine or that it is faster. [ONNX Runtime Core ML provider](https://onnxruntime.ai/docs/execution-providers/CoreML-ExecutionProvider.html).

The beat, harmony, structure, transition-score and time-stretch interfaces MUST permit alternative implementations. The first release combines a modern learned beat model with established signal processing and application-specific planning. Multiscale chroma/key smoothing and initial transition weights are baselines to evaluate, not claims of state-of-the-art local-key or end-to-end DJ performance.

## macOS client

The first supported application target is Apple Silicon macOS. Intel support is optional pending build and performance tests. Set and document the deployment target after selecting tested dependency versions; do not inherit an accidental minimum from development hardware.

The graphical client SHOULD use SwiftUI and expose source selection, analysis status, a searchable track list, a proposed playlist, transition previews, playback/export controls and correction tools. A basic waveform/beat overlay is sufficient. Users must be able to correct a downbeat, mark a passage unsuitable, adjust a cue region, exclude a transition and retain these changes through reanalysis.

UniFFI may expose catalogue and planning operations to Swift. Keep sample-buffer transport on a deliberately designed audio boundary. CPAL/CoreAudio or a native Apple audio host may drive output; choose one owner for the device clock. [UniFFI](https://github.com/mozilla/uniffi-rs) and [CPAL](https://github.com/RustAudio/cpal).

## iOS extension

Reuse the Rust analysis contracts, planner and DSP, but keep folder access, audio sessions and lifecycle handling in the Apple shell. Access selected directories through document-picker URLs and persisted bookmarks, handling revoked access and unavailable file-provider content. Copy or materialise required playback files into a managed local cache where appropriate. [Apple directory access](https://developer.apple.com/documentation/uikit/providing-access-to-directories).

Heavy analysis MUST support cancellation and checkpointing. A scheduled background task is system-controlled, and user-started continued processing can also be terminated. Do not assume an always-running folder watcher. Provide an explicit foreground import/analyse action, optional supported background continuation, and a resume action. [Apple background strategies](https://developer.apple.com/documentation/backgroundtasks/choosing-background-strategies-for-your-app) and [continued processing](https://developer.apple.com/documentation/backgroundtasks/bgcontinuedprocessingtask).

An initial iOS release may import audio plus a validated metadata/plan snapshot generated on macOS. Local iOS analysis is an extension gated on memory, thermal, energy and model-deployment measurements. File portability must use root IDs and relinking, never hard-coded macOS absolute paths.

# Command line contract

Use `mixlib` as a provisional executable name. Commands below define the intended interface, not an already implemented tool. The library path may also come from an environment variable or explicit application setting, but its resolution must be printed in diagnostics.

| Command | Required behaviour |
| - | - |
| `mixlib init --library ./mix-library` | Create a versioned metadata directory without touching audio |
| `mixlib scan --library ./mix-library --source ./music` | Discover files and queue new, changed or incomplete work |
| `mixlib analyze --library ./mix-library --resume` | Execute queued analysis with bounded concurrency |
| `mixlib scan --library ./mix-library --verify-content` | Verify content hashes for registered source roots |
| `mixlib inspect --library ./mix-library --track TRACK_ID` | Show provenance, analysis status, regions and quality flags |
| `mixlib plan --library ./mix-library --all --seed 42` | Plan an order covering all eligible selected tracks |
| `mixlib plan --library ./mix-library --duration 60m --seed 42` | Select and sequence a subset for the requested duration |
| `mixlib preview --library ./mix-library --plan PLAN_ID` | Render short auditions around each transition |
| `mixlib render --library ./mix-library --plan PLAN_ID --output ./mix.wav` | Render a complete mix from the saved plan |
| `mixlib play --library ./mix-library --plan PLAN_ID` | Execute the same plan through the audio device |
| `mixlib validate --library ./mix-library` | Check schemas, references, hashes and plan invariants |
| `mixlib rebuild-catalog --library ./mix-library` | Reconstruct derived summaries from track manifests |

Commands MUST support cancellation, meaningful exit codes and `--json` machine-readable output. Progress belongs on stderr so JSON stdout remains parseable. A scan report must distinguish unchanged, queued, deferred, unavailable and failed items. Render output must never overwrite an existing file without an explicit overwrite option.

# Licensing and model distribution

Every shipped model and native dependency MUST have a manifest entry recording upstream URL, exact revision/checksum, code licence, weight licence where applicable, attribution and any redistribution restriction. Verify availability of actual pretrained weights and an executable inference path; a paper or model architecture alone is insufficient.

Beat This!'s official repository states MIT for its code and published weights; the selected port and converted assets still require their own provenance verification. Essentia and its pretrained models have separate terms, while S-KEY's published weights are restricted to non-commercial research. They may serve as development references where permitted, but bundling must follow the selected artefact's actual terms. [Beat This!](https://github.com/CPJKU/beat_this), [Essentia licensing](https://essentia.upf.edu/licensing_information.html), and [S-KEY](https://github.com/deezer/skey).

Music analysis and rendering MUST run locally. Network access is limited to explicit model acquisition or optional updates; no user audio or analysis metadata is uploaded by default. Downloaded assets require checksum verification and a resumable install. Audio, metadata and model files are untrusted inputs: impose documented size/dimension limits and reject path traversal in imported manifests.

# Validation and acceptance criteria

## Functional and recovery tests

| ID | Test | Acceptance |
| - | - | - |
| AC-01 | Scan an analysed library again | No unchanged track invokes expensive analysis; report skipped counts |
| AC-02 | Add one WAV | Analyse only the new audio and newly required dependent records |
| AC-03 | Rename or move a verified WAV | Reuse identity and analysis after successful relinking |
| AC-04 | Change metadata or audio bytes | Preserve analysis for unchanged audio hash; invalidate it for changed audio |
| AC-05 | Kill the process during each publication boundary | Previous committed state remains valid; safe staged work resumes |
| AC-06 | Fill the disk during analysis or export | No truncated authoritative JSON or falsely completed output |
| AC-07 | Start two writers and a reader | One writer owns publication; reader sees complete generations |
| AC-08 | Corrupt a feature or summary file | Detect corruption; rebuild derived summaries or isolate affected analysis |
| AC-09 | Upgrade a single algorithm | Recompute its dependent stages; preserve unrelated valid stages and overrides |
| AC-10 | Save a plan, then modify a source | Refuse stale execution with track-specific diagnostics |
| AC-11 | Remove all derived catalogue files | Rebuild the usable catalogue from authoritative JSON |
| AC-12 | Plan an impossible all-track sequence | Explain infeasibility or return explicitly permitted fallback transitions |

JSON schemas MUST have positive and negative fixtures. Tests must cover unknown versions, malformed arrays, non-finite numeric input, missing references, changed source hashes, duplicate beats, backward time maps and invalid interpolation derivatives.

## Musical and DSP evaluation

Build a representative evaluation set of at least 50 recordings before choosing production defaults. Include steady electronic music, live drums, acoustic music with tempo drift, compound/odd meter, sparse intros, tempo changes, modulation, tuning offsets and deliberately unsuitable/free-time passages. Manually annotate representative entry/exit windows, not necessarily every full recording.

Compare the Rust beat pipeline with the official reference on identical decoded/preprocessed inputs. Record event differences and numerical activation tolerances. Report beat/downbeat F1 with its tolerance, continuity metrics, half/double-tempo errors and performance on accepted transition windows. Report coverage alongside accuracy so a system cannot appear reliable solely by rejecting nearly everything.

For deterministic DSP fixtures with known beat/transient positions, target no more than one output sample of scheduling error before the stretcher and no more than 10 ms residual alignment error in rendered test overlaps after latency compensation. Real-music timing quality needs separate annotated measurements and listening; beat annotations alone do not establish perceptual quality.

Test loudness and true peak, stereo coherence, time-map continuity, absence of unintended silence, click/zipper artifacts, stretcher priming and tail handling. Compare preview, export and buffered playback output within documented floating-point tolerances using the same plan and engine settings.

Evaluate at least 40 representative transition auditions against a simple loudness-matched crossfade baseline in shuffled, level-matched listening. The initial release target is at least 80 percent rated acceptable by the library owner and no repeated class of severe timing or harmonic failure. Record fallback rate, rejected-candidate coverage and source genres. These are product acceptance targets to tune before release, not established model performance claims.

## Resource and responsiveness targets

Freeze a reference Apple Silicon Mac with 16 GiB RAM, a local SSD, OS version and power mode for reproducible measurements. Initial budgets exclude optional stem separation and must be reported with track durations, sample rates, backend and concurrency.

| Workload | Initial target |
| - | - |
| Unchanged scan of 10,000 local files | At most 30 seconds without full content verification or audio decoding |
| Browse 10,000 cached track summaries | First useful view within 2 seconds using lazy loading |
| Single-worker analysis of a 20-minute stereo track | Peak resident memory below 2 GiB |
| Plan 100 selected tracks from cached analysis | A feasible result or explicit infeasibility within 30 seconds; deeper refinement bounded separately |
| Offline rendering | At least real-time throughput for stereo 48 kHz EQ-based mixing on the reference Mac |
| Continuous playback | Two-hour test with no app-caused underruns at a supported buffer size; report worst callback time |
| Cancellation | UI/CLI acknowledges within 1 second; worker stops at a documented safe boundary |

Cold model loading, model inference, candidate audio refinement and warm-cache planning MUST be timed separately. iOS performance targets are defined after choosing a reference device; macOS results must not be presented as iOS guarantees.

# Delivery milestones

| Milestone | Deliverables | Completion gate |
| - | - | - |
| 1 JSON catalogue | Directory layout, schemas, scanner, identity, atomic writes, resumable jobs and inspect/validate commands | Recovery and incremental-scan criteria pass |
| 2 Musical analysis | Beat This! integration, port comparison, harmonic/band features, regions and overrides | Representative analysis fixtures and saved provenance pass |
| 3 Transition engine | Time maps, latency handling, three transition templates and preview export | Synthetic alignment, peak and artifact checks pass |
| 4 Playlist planner | Candidate retrieval, explainable scoring, beam search, exact plan output and complete WAV export | Valid plans, fallback handling and listening evaluation pass |
| 5 macOS client | Library view, analysis progress, correction tools, playlist previews and playback | End-to-end usability and sustained playback tests pass |
| 6 iOS client | Portable snapshots, file access, lifecycle integration and playback | Device memory, interruption and background behaviour tests pass |

Full stem analysis, a learned compatibility ranker, automatic transposition, more flexible tempo trajectories and local iOS analysis can be added without changing the JSON authority model or mix-plan contract.

# Implementation handoff

The implementation repository must contain the Rust workspace, versioned JSON schemas, example configurations, small redistributable test fixtures, model acquisition instructions, dependency/model manifests, benchmark commands, and a description of metadata recovery and migration. Provide sample analysis and mix-plan JSON with a reproducible preview render.

The first executable demonstration must take a directory of WAVs through scanning, analysis, planning and complete WAV rendering, then repeat the scan without reanalysing unchanged audio. It must also demonstrate a variable-tempo acoustic transition and an explicit fallback for an unsuitable passage. This is the foundation for the simplified playlist-and-beatmixing client.
