# Metadata, identity and recovery

`library.json`, WAV sidecar analyzer bundles and saved plans are authoritative.
Track manifests, immutable analysis records and correction snapshots live inside
`<filename>.wav.json` under `analyzer.records`. Per-track paths in this document
are logical keys, not separate files. The recorder owns the top-level fields;
the analyzer preserves them, including unknown fields. Catalogue shards and the catalogue
manifest are rebuildable. Jobs and staging checkpoints record resumable work.
No command deletes source audio or old analysis generations. The disposable
`catalog/sidecars.json` index can be rebuilt from the registered source roots.

A stable `locks/writer.lock.json` inode is opened and OS-locked with `fs2` before
CLI mutation. Its JSON owner diagnostic is written in place. PID text is never
used to steal a lock. Process death releases the advisory lock. `--wait-lock`
polls with cancellation; otherwise a second writer receives `WRITER_BUSY`.
Readers need no lock and follow one captured immutable reference. The library
API exposes the lock explicitly; callers must hold it for all mutations.

Every JSON publication validates typed structure and semantics. Per-track
publication merges one logical record into the current sidecar, under an advisory
lock on its parent directory (no extra per-WAV lock file), preserving recorder
fields and prior generations. An unchanged record causes no write. It writes to a
unique same-directory tempfile, flushes/fsyncs it, reads it back, renames it,
and fsyncs the parent directory. Artifacts are published under fresh generation
IDs before the small active `track.json` pointer changes. A failure can leave
unreferenced complete artifacts, but cannot point a track at a partial artifact.
A parent-directory fsync failure can report an error after the rename already
succeeded; a subsequent read determines the committed state.

Stage checkpoints in `staging/<job_id>/checkpoint.json` contain checksummed,
versioned stage outputs. Outputs may already reside in a fresh, unreferenced
analysis generation in the same sidecar. `analyze --resume` verifies stage identities and checksums
before reuse. Corrupt output is replaced in a **new** generation; it is not
edited in place. A running job left by an interrupted process is eligible for
resume. A failed or deferred source needs rescan after a copy/replacement.
Missing models can be installed, then the saved feature stage is reused.

Correction publication writes `tracks/<shard>/<id>/overrides/<revision>-<uuid>.json`
first, then swaps `track.json`'s `active_overrides` reference/revision. The
specification's `overrides.json` path is maintained as a convenience copy. A
reader holding an older track snapshot still reads the older correction set.
A crash before updating the convenience copy has no effect on authority; inspect
uses the captured immutable reference. All extensions are preserved.

Use `validate` to inspect current analysis, model checksums, catalogue checksums,
saved plans and source audio hashes. A corrupt authoritative track or plan
fails explicitly; it is never silently dropped from the selected pool. Use
`rebuild-catalog` after deleting/corrupting catalogue files. Model files are
verified before inference. Metadata references reject absolute paths, `..`,
and symlinks. This is a local single-owner filesystem design, not a distributed
or adversarial multi-user filesystem transaction system.

## Hash formats

Hashes use `{ "algorithm": "sha256", "digest": "lowercase hex" }`.
`mixlib-json-v1` uses recursively sorted object keys, UTF-8, no whitespace,
array order preserved, and serde_json's finite-number encoding. It is a local,
versioned canonical format, **not** a claim of RFC 8785 compatibility. Non-finite
values are rejected before serialization, including nested optional values.
Effective keys include only the relevant audio, algorithms, models and config;
creation times and random generation IDs are excluded.

`mixlib-wav-audio-v1` is SHA-256 of:

1. ASCII `mixlib-wav-audio-v1` followed by a zero byte.
2. Little-endian sample rate (u32), channels (u16), encoding tag (u16: 1 integer,
   3 IEEE float), container bits (u16), valid bits (u16), channel layout (u32),
   total audio payload byte count (u64).
3. All `data` chunk payloads in their file order, excluding chunk padding.

An unspecified mono/stereo mask is normalized to centre/left-right respectively.
Metadata, RIFF padding and non-audio chunks are excluded. A separate whole-file
SHA-256 includes every byte. A changed metadata chunk therefore preserves
analysis while a changed sample, encoding, sample rate or valid-bit descriptor
invalidates it. Duplicate verified payloads share one track identity/analysis;
locations remain distinct. Stable paths alone never define audio identity.

Ordinary scans use size, nanosecond mtime and device/inode hints. A malicious or
unusual replacement preserving those hints needs `scan --verify-content`.
Rendering independently verifies all pinned audio before and after execution.
A changed source, correction revision or corrupt referenced artifact prevents
publication rather than silently changing the plan.

## Migration and retention

Only schema major 1 is supported. An unknown version is an error; no speculative
migration rewrites it. Future migration tooling must write new generations and
swap references only after validation. Back up the shared metadata directory and source WAVs together with their
sidecars. `fuse-sidecars` migrates the previous separate-file layout without
changing logical record hashes. It publishes all bundles before switching the
library layout, then checks each copied record before deleting its legacy file.
Rerunning completes cleanup interrupted after the layout switch. Before the
switch, the separate records remain authoritative. An unavailable primary source
or conflicting sidecar owner blocks migration without deleting old metadata. No automatic garbage collection is implemented;
manual deletion of generations can invalidate readers or saved plans.
