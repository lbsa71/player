# Mixing tactics

Specification version 1.1 · 10 November 2026

This is the decision contract for choosing cues, layer ownership, transition
duration and exit treatment. It extends [the analyzer specification](README.md).
[IMPLEMENTATION.md](IMPLEMENTATION.md) documents commands, setup and engineering
behavior. All 38 tactic IDs now have executable contracts, eligibility checks
and schedules. Their render paths and negative cases are covered by automated
tests. This is implementation status, **not a claim that every tactic has been
auditioned or musically validated on the collection**. Unknown required evidence
still prevents selection.

The intended result is a musically continuous mix. Compatible passages should
have time to become a mashup, then resolve deliberately into the next song.
The planner must choose **cue + tactic + hold + exit together**. Choosing a good
pair and applying the same crossfade to every pair is insufficient.

MUST denotes a requirement. SHOULD denotes the default unless the saved plan
explains an exception. Durations and thresholds in this document are initial
application policy to evaluate against the collection. They are not universal
DJ rules. The rules below remain the decision contract; implementation status
and remaining validation are recorded separately.

## The dense-mashup rule

**Given A has a stable, intense passage whose vocals and accompaniment work
over B's drums, and B has a stable drum passage with a compatible arrival,
prefer six complete bars of mashup followed by two bars of accompaniment
crossfade, within an eight-bar transition.**

This preference applies only after checking the actual selected layers. Two
loud songs do not necessarily make a dense composite, and two individually good
passages can still clash. Overcrowded doubles should instead use a simpler
layer allocation, another cue or a drop replacement. Human instruction supports
that distinction. The exact six-plus-two schedule is this app's policy.
[Double-drop guidance][double-drop]

Use zero-based elapsed bars throughout this document. `t = 6` is the start of
the seventh audible bar, after six complete bars have played. Bars follow the
validated local meter and shared tempo map. The example below assumes 4/4.

| Lane | `0 ≤ t < 6` — hold | `6 ≤ t < 8` — resolve | `t ≥ 8` |
| --- | --- | --- | --- |
| A drums | Removed at the aligned handoff | Remain removed | Off |
| B drums | Steady foreground rhythm | Stay steady | Continue |
| A other / B other | A owns the accompaniment, B muted | A fades out while B fades in across the full two bars | B owns accompaniment |
| A bass / B bass | A owns bass, B muted | Controlled bass exchange inside the two-bar interval | B owns bass |
| A vocals | Continue intact when present | Finish the phrase, then close the vocal gate in a safe pocket | Off |
| B vocals | Muted | Enter only after A's vocal gate and any vocal effect tail have cleared | Eligible to continue |

“Pure mashup” means a stable combination of complementary layers. It does not
require playing both complete stereo mixes. An onset-sized de-click ramp is
allowed at the drum replacement and does not consume a musical hold bar.

The accompaniment fade MUST start at `t = 6` and finish at `t = 8`. Its endpoints
MUST NOT move when a vocal pocket moves. Vocal gates are a separate schedule.
A's vocals must survive at least the first four bars when a continuing vocal
phrase is present, and the default mashup keeps them available through all six
hold bars. Never extend a finished phrase artificially just to fill that time.

The earliest B vocal entry is 300 ms after A's audible vocal content ends,
including bleed and effects. B must also enter at its own supported phrase or
pickup onset. If B is already mid-line when the guard clears, keep it muted
until the next complete entry or replan; do not open its gate mid-word or
mid-phrase. If A cannot complete its phrase and leave the
required gap before B's planned vocal entry, choose another cue, a longer
supported phrase window, or another tactic. Do not truncate a word, fade out an
unfinished line, or accept simultaneous vocals to preserve eight bars.

During the exit, B's established drums MUST NOT fade down with B's accompaniment.
A's drums MUST NOT return. Bass ownership must be explicit rather than summing
two full basslines throughout the fade. The accompaniment transfer should be
continuous across the two bars, with headroom and perceived energy checked.

An eight-bar window therefore contains **six hold bars plus two exit bars**.
It is neither a six-bar transition nor six bars plus an exit appended after
bar eight. With variable tempo, bar boundaries are mapped to output frames;
they are not approximated using a single average BPM.

M01 now implements this fixed six-plus-two schedule. M02 retains the spacious
hold and brisk exit. Automatic choice checks density after lane selection,
full-window stability, actual phrase/gate timing, separation evidence and
headroom. A candidate without sufficient evidence is rejected; the presence
of an M01 renderer does not make every dense pair eligible.

## Classification

Store arrangement and perceptual character separately. A mashup can end subtly
or suddenly. Effects are treatments attached to an arrangement, with their own
input, timing and tail constraints.

| Dimension | Values | Meaning |
| --- | --- | --- |
| Arrangement | `mashup`, `blend`, `handoff`, `bridge`, `replacement`, `reset` | What the tracks contribute over time |
| Character | `subtle`, `sudden`, `abrupt` | How conspicuous the transfer is intended to be |
| Treatment | `gain`, `eq`, `filter`, `echo`, `reverb`, `loop`, `performance_fx` | Required processing, possibly several |
| Permission | `normal`, `expressive_opt_in`, `emergency_only` | Whether automatic selection may consider it |

The review UI may group these as **Mashup**, **Subtle**, **Sudden** and **Abrupt**.
Mashups retain their exit character. Sudden means a deliberate structural switch
at a convincing musical event. Abrupt means an intentionally disruptive reset.
An accidental click, clipped vocal or mistimed entrance is a defect, not a
category of transition.

The default profile prefers mashups and subtle transfers. Sudden transitions
need positive evidence for the landing. Expressive tricks are opt-in. Scratch
stops are emergency-only by user requirement. This spec also reserves brakes
and backspins for emergencies in the default profile. Human DJs do use such
effects creatively; this restriction describes our product aesthetic.
[Performance examples][crossfader]

## Analysis needed for decisions

### Segment records

Analyze overlapping one- to four-bar tiles, retaining beat-level events. Build
longer candidate windows from consecutive tiles and detected structural
boundaries. An eight-bar mashup must be assessed across its entire hold and
exit, not inferred from a promising first two bars.

All per-track metadata stays inside the existing `<filename>.wav.json` under
`analyzer`, including new segment descriptors and their analysis generations.
There MUST NOT be a physical JSON file per bar, segment or stem. Shared plans,
configuration, model assets and rendered WAVs keep their existing library
locations. Preserve the recorder's top-level metadata and the source WAV.

| Evidence | Implemented basis | Remaining uncertainty / validation |
| --- | --- | --- |
| Pulse and timing | Corrected source beat/downbeat/meter artifacts, actual cubic time maps, mapped skew, separate native pulses, measured low/high attack hierarchy for D02 | Groove thresholds and perceptual half/double-time interpretation need collection evaluation |
| Phrase structure | Repeated structural boundary support, source-time vocal phrase/pickup evidence, local build/drop/attack/ending events | No universal hierarchical phrase parser; uncertain boundaries remain ineligible |
| Segment role | One- to four-bar role/energy descriptors plus separate unmetered source windows | Structural/semantic labels are heuristics, not reliable verse/chorus recognition for every song |
| Energy and density | RMS/peak/crest, onset/band occupancy, selected active lanes, schedule-specific density and headroom bounds | No certified loudness meter or universal density threshold; actual render peak check remains mandatory |
| Tonality | Local chroma, selected-lane spectra and supported pitch events, lead/register conflict and motif comparisons | Polyphony, modulations and perceptual lead dominance can remain uncertain |
| Vocals | Local Whisper word timing with acoustic support, supported phrase boundaries, affirmative quiet spans checked against actual vocal PCM, tail/gate occupancy | Blank ASR is not absence; singing transcription, separation and phrase semantics can fail |
| Separation | Full source-aligned HTDemucs stems, reconstruction residual, discontinuity/clipping and lagged vocal-leakage proxies | Low proxy scores do not certify perfect isolation or absence of intelligible bleed |
| Loops and effects | Per-lane PCM seam/tonal-cycle checks, finite edit/transport schedules, capture bounds, repeated-language and tail checks | Musical repetition, expressive intent and perceived effect quality require listening |


Each descriptor MUST include its time span, method/version and evidence
semantics. Unknown is distinct from absent. A missing vocal estimate must not
be interpreted as “instrumental.” Preserve uncertainty during aggregation.
Classifier labels may overlap, such as `sustained_texture` and `vocal_foreground`.

Phrase confidence currently measures a best-versus-runner-up score margin,
not probability. Four-bar phase evidence does not prove an eight-, sixteen- or
thirty-two-bar phrase. A useful subdivision can still fall inside an unfinished
musical statement. Phrase mixing depends on the arrangement, whose lengths
vary. [Phrase mixing][phrase]

### Eligibility predicates

These predicates are implemented as conservative evidence gates. A predicate
can remain unknown for a particular recording or window; availability of its
code does not imply that the evidence supports it.

| Predicate | Required evidence and result |
| --- | --- |
| `grid_lock(A, B, map)` | Reliable pulse/meter mapping and acceptable beat skew across every rhythmic overlap, including tempo changes |
| `phrase_ready(A, B, window)` | The entry, hold and resolution respect supported boundaries and leave complete phrases, with pickup handling |
| `tonal_fit(audible_lanes)` | The actually audible pitched layers coexist across the interval; whole-track key labels alone are insufficient |
| `vocal_safe(schedule)` | At most one track has audible vocals at any time, with complete phrases and the required guard interval, including tails and bleed |
| `complementary(lanes)` | Roles coexist without competing drums, basslines or foreground leads beyond an explicitly controlled handoff |
| `dense(composite)` | Sustained occupancy and energy remain high after stem selection and gain, using several measured descriptors |
| `steady_through(window)` | No unplanned fill, drum disappearance, vocal arrival or harmonic change invalidates the hold or exit |
| `stem_safe(lanes)` | Required removal/isolation works well enough to satisfy ownership and vocal constraints |
| `loop_safe(span)` | Seam, pulse, harmonic cycle and language remain coherent for the planned repetition count |
| `tail_room(effect, next)` | The captured content decays safely before conflicting vocals, bass or leads, including the following transition |

Thresholds must be versioned and tuned on auditioned examples. “Dense” must not
mean simply exceeding one RMS threshold. Low-frequency EQ also cannot replace
drums while keeping the same track's bass intact. That requires usable stems.
[Stem controls][stems]

Current timing constraints provide an initial baseline. Paired beat skew is
limited to 30 ms. Normal stretch allowance is ±6%, with speed acceleration
limited to 0.02/s; automatic beat-return vocal handoffs can currently expand
the default allowance to 10%. Preserve explicit per-plan limits rather than
silently generalizing that exception. Full-track vocal evidence combines local
ASR timing with acoustic checks; local estimated absence requires affirmative
provider spans and agreeing 20 ms vocal PCM measurements. Vocal schedules keep
a minimum 300 ms guard. RMS gaps alone do not establish that a sung phrase has
finished. Separation and ASR scores are uncalibrated evidence, not guarantees.

## Tactic inventory

Each rule gives A/B eligibility, execution and rejection behavior. Global
predicates above apply even when not repeated in a card. Stated durations are
candidate defaults, constrained by the actual arrangement. Existing primitives
do not imply an automatically selectable tactic.

**Implemented** below means an executable contract, admission path and processing
schedule exist. It does not mean the tactic will qualify for the present songs,
or that its musical quality has been established. Tempo modifiers compose with
a primary tactic; emergency IDs have a separate runtime-only admission path.
Evidence links document the human technique or effect mechanics, not the
correctness of this implementation. The automatic A/B rules and numerical
limits are this specification's design inferences.

### Mashups

#### M01 — Dense drum-and-accompaniment mashup with a two-bar exit

- **Given A** has a stable vocal/texture/bass passage whose drums can be removed,
  **and B** supplies a complementary, steady groove and compatible resolution.
- **Use** the six-plus-two schedule above when the selected composite is dense
  but clear. Keep B's drums stable, preserve A's phrase, then transfer the other
  layers over the final two bars. Character `subtle`.
- **Reject** crowding, unreliable isolation or a vocal finish outside the safe
  window. Try another cue or S04/S05; never reward density alone.
- **Status** Implemented fixed six-plus-two accompaniment exit, independent phrase-safe vocal gates and measured selected-layer density admission.
  [Double-drop density guidance][double-drop], [stem controls][stems].

#### M02 — Spacious drum-and-texture mashup with a brisk resolution

- **Given A** has a sparse, sustained bed, **and B** has stable drums followed by
  a clean, decisive arrival that benefits from a short resolution.
- **Use** an extended complementary hold, then an approximately one-bar staged
  accompaniment handoff at a safe phrase/vocal boundary. Character `subtle` or
  `sudden` according to the arrival. Do not apply the short exit to a dense M01
  candidate merely because it is the current default.
- **Reject** a hold that ends before the phrase or leaves no clean exit.
- **Status** Implemented spacious-bed admission and brisk resolution; it is not an automatic downgrade for an unsafe dense M01 candidate.
  [Basic transition practice][basic].

#### M03 — Acapella over instrumental

- **Given A** has a complete, isolatable vocal phrase, **and B** has a compatible
  instrumental passage with no competing foreground lead.
- **Use** A's vocal over B's rhythm/accompaniment for one or more complete
  phrases. Resolve through a vocal gap. Character `subtle`; keep B vocals muted
  throughout A's line and guard interval.
- **Reject** bleed, incompatible melody or a word cut by the window. Do not use
  EQ alone as evidence of vocal removal.
- **Status** Implemented vocal-over-instrumental lane scheduling with phrase, lead and separation checks.
  [Vocal phrase layering][phrase], [stem controls][stems].

#### M04 — Complementary melody or bass overlay

- **Given A** has a distinctive isolatable motif, **and B** offers a stable
  backing with space in that register and no competing lead.
- **Use** the selected motif as a temporary foreground over B, then remove it
  at its own phrase end. Assign one bass owner. Character `subtle`.
- **Reject** shared-key passages whose actual notes or rhythmic accents clash.
  Check modulation throughout the hold; drums-only compatibility is weaker
  evidence than sustained melodic compatibility.
- **Status** Implemented isolated motif/bass overlay scheduling with measured pitch, lane and phrase evidence.
  [Harmonic guidance][harmonic], [stem controls][stems].

#### M05 — Controlled double drop

- **Given A and B** have aligned drop arrivals whose selected layers remain
  complementary after bass/lead ownership is assigned.
- **Use** a deliberately energetic simultaneous arrival, with a preplanned
  owner for each foreground lane and a bounded hold. Character `sudden` at
  entry; choose its exit separately.
- **Reject** two crowded choruses, competing vocals or excessive summed energy.
  Prefer C02 when replacing the drop is more convincing.
- **Status** Implemented controlled drop ownership and bounded hold; admission rejects competing complete mixes without sufficient separation/headroom.
  [Double-drop instruction][double-drop].

#### M06 — Vocal call-and-response / wordplay

- **Given A** ends a complete lyric or motif, **and B** has an intentional
  answering phrase with semantic or musical support.
- **Use** alternating phrases with a common rhythmic bed. No simultaneous
  vocals, including sampled repeats. Character `sudden` or `subtle`.
- **Reject** matches inferred only from titles, unverified transcription, or
  timing that cuts syllables. Semantic confidence must be explicit; the app
  must not invent lyrical relationships.
- **Status** Implemented alternating phrase scheduling with supported word/motif relationship evidence; unknown semantics remain ineligible.
  [Wordplay demonstration][crossfader].

### Subtle blends, handoffs and bridges

#### S01 — Long phrase-aligned EQ blend

- **Given A** has a clearing outro or sustained instrumental phrase, **and B**
  has a sparse, stable intro with compatible pitched material.
- **Use** a supported phrase-length blend, commonly considering 8 or 16 bars.
  Exchange frequency ownership while keeping one bass foundation.
- **Reject** evolving melodic conflict, a weak grid, or a phrase that cannot
  sustain the overlap. Shorten only at another valid structural boundary.
- **Status** Implemented phrase-length EQ scheduling and full-window measured admission.
  [Phrase beatmixing][basic], [phrase structure][phrase].

Dry schedules check vocal audibility after the lane gains and cut times are
known. An unclassified vocal can continue on its own deck. An opening or
closing vocal gate requires a PCM-verified quiet pocket, and possible vocal
activity on opposite decks must keep the 300 ms guard. Tactics that capture,
repeat or use the content of a line still require positive phrase evidence.

#### S02 — Short phrase crossfade

- **Given A** is ready to finish, **and B** has a clean entrance with similar
  energy and little material worth holding simultaneously.
- **Use** a one- or two-bar transfer at the structural boundary, with level and
  low-end control. Treat it as a last resort unless aligned audio establishes
  two quiet, closely matching passages, or matching drums and two matching
  frequency bands with a harmonically compatible difference in the third.
  This is also a possible exit treatment after a mashup.
- **Reject** unfinished vocals or a compelling complementary hold that the
  short transfer would discard. Move the cue when the endpoint is unsuitable.
- **Status** Implemented short phrase transfer with vocal, boundary and headroom admission.
  [Fade and beatmatch techniques][ni-basic].

#### S03 — Bass swap

- **Given A and B** have compatible upper layers but simultaneous bass would
  crowd the mix, **and B** offers a clear bass arrival.
- **Use** a bounded low-end ownership exchange at that arrival while retaining
  useful upper layers. In stem mode distinguish bass from kick/drums.
- **Reject** an energy hole or doubled low end. Do not assume a zero low-EQ
  setting produces perfect isolation.
- **Status** Implemented bounded bass ownership exchange with measured upper-layer compatibility.
  [Bassline swap][basic].

#### S04 — Three-band staged exchange

- **Given A** remains useful in some frequency regions, **and B** can enter
  progressively without colliding with A's vocals or lead.
- **Use** an ordered exchange of highs, bass and mids, with independently timed
  gains. Choose ordering from occupancy rather than treating it as universal.
- **Reject** vocal collisions hidden by band-level reasoning. Prefer stem
  handoffs when frequency bands cannot separate the competing material.
  Skip automatic band exchanges when both passages place at least 70% of
  their measured band energy in the lows.
- **Status** Implemented staged three-band scheduling and measured compatibility checks. Further listening-based ordering refinements remain possible.
  [EQ transition foundation][basic].

#### S05 — Quiet-bed drum lead and linger

- **Given A** has a low-percussion vocal/texture passage, **and B** supplies an
  unobtrusive stable drum groove.
- **Use** B drums under A for at least a complete supported unit, then exchange
  bass and accompaniment over later bars. Extend into M01/M02 when both sides
  stay useful. Maintain A's vocal phrase.
- **Reject** a supposedly quiet A passage with active drums or an imminent fill
  that invalidates the hold. Check the whole window.
- **Status** Implemented quiet-bed drum lead/linger with staged accompaniment and independent vocal ownership.
  [Percussion over a beatless passage][basic].

#### S06 — Beat-return drum replacement

- **Given A** has a break followed by a supported drum-return boundary,
  **and B** has a compatible groove ready at that point.
- **Use** the return as the exact drum handoff. Remove A drums, introduce B
  drums, preserve A's useful vocals/texture, then use S04, M01 or M02 to resolve.
- **Reject** entry before the intended phrase boundary or after an ambiguous
  pickup. Never replace A's entire stereo mix just to remove its drums.
- **Status** Implemented source-event drum replacement and phrase-safe resolution, including the earlier prepared-handoff path.
  [stem control][stems], [phrase mixing][phrase].

#### S07 — Breakdown / ambient bridge

- **Given A** reaches a complete phrase ending with a beatless tail, **and B**
  has a spacious intro or breakdown before establishing its pulse.
- **Use** a tonal/texture overlap or a restrained tail to bridge the pulse
  change. B's first drums establish the new groove.
- **Reject** concealed overlapping pulses or dissonant sustained tails. A large
  BPM difference is acceptable only while simultaneous rhythm is absent or
  explicitly mapped.
- **Status** Implemented structural bridge admission, including measured unmetered windows without fabricated bar timing.
  [Tempo and genre practice][tempo-djs].

#### S08 — High-pass or low-pass filter fade

- **Given A and B** have compatible but busy passages, with a clear frequency
  region to withdraw from A as B takes ownership.
- **Use** an intentional cutoff sweep plus gain/bass scheduling. High-pass
  withdrawal clears low frequencies; low-pass withdrawal clears highs and
  still requires a separate bass decision.
- **Reject** resonance peaks, vanishing bass before replacement or unresolved
  vocal conflict. Filtering is not proof that vocals are absent.
- **Status** Implemented actual swept filter processing, bounded resonance/cutoff automation and the associated admission checks.
  [Filter-fade instruction][ni-basic], [filter mechanics][effects].

#### S09 — Echo / delay out

- **Given A** has a clean completed phrase or isolated ending hit, **and B**
  has room for a decaying rhythmic tail.
- **Use** a bounded capture/send, remove dry A, and decay repeats into B. Choose
  beat-synced timing only when the relevant pulse is reliable. May be `sudden`
  when paired with a dry cut.
- **Reject** repeated A vocals over B vocals, muddy bass repeats or an uncleared
  tail at the next join. Schedule the tail as audible A material.
- **Status** Implemented bounded delay capture/send, wet decay and vocal-tail collision checks.
  [Phrase-ending delay use][mixer-fx], [stem echo-out][stems].

#### S10 — Filtered echo freeze bridge

- **Given A** has a clean short capture, **and B** leaves tonal and rhythmic
  space for a temporary repeating texture.
- **Use** a bounded frozen delay buffer with progressive filtering and an
  explicit release. Prefer nonvocal material when B vocals are approaching.
- **Reject** unstable feedback, indefinite repetition or delay-time changes
  that create unintended pitch motion. The selected tail must fit B's harmony.
- **Status** Implemented filtered frozen-delay state with explicit capture, release and reset boundaries.
  [Freeze-and-filter technique][mixer-fx].

#### S11 — Reverb wash / frozen atmosphere

- **Given A** offers an isolated hit or texture, **and B** has a spacious entry
  that can absorb its decay, possibly across a pulse reset.
- **Use** a controlled wet tail while removing dry A. Bound the low-frequency
  content and release time; freeze requires an explicit termination event.
- **Reject** full-mix smearing, vocal overlap or a sustained incompatible chord.
  A quieter wet tail can still be perceptually intrusive.
- **Status** Implemented bounded reverb/freeze processing and structural timing; stored tails remain audible source material for validation.
  [Reverb and freeze mechanics][effects], [effects into an ambient intro][tempo-djs].

#### S12 — Loop bridge / intro or outro extension

- **Given A** has too little clean runway, or **B** has a useful but short intro,
  and the required side contains a verified repeatable instrumental span.
- **Use** a finite loop to reach the next valid entry/exit boundary. Assess the
  harmonic cycle and phrase after repetition, then release on a known beat.
- **Reject** word truncation, audible seams, implausible repetition or excessive
  extension. Do not fabricate source time through a monotonic time map.
- **Status** Implemented explicit finite loop edits, source transport, seam/tonal and language checks.
  Fixed-period loops require a steady output clock and cannot combine with D01.
  [Loop-based extensions][loops].

### Tempo modifiers

These compose with a tactic. They are not reasons to disregard its layer,
phrase or vocal constraints.

#### D01 — Shared gradual tempo movement

- **Given A and B** have a modest tempo difference and compatible rhythmic
  material throughout the move.
- **Use** one shared tempo trajectory with small per-bar adjustments toward B,
  distributing stretch across both tracks. Preserve pitch and validate the
  entire output time map. Crossfade and vocal endpoints follow its bar anchors.
- **Reject** excessive speed/acceleration, vocal distortion or beat drift. An
  intentional large loop descent uses C14, a separate expressive bridge, not a normal
  consequence of a large BPM mismatch.
- **Status** Implemented shared mapped tempo trajectories with speed, acceleration and beat-skew validation; attaches to a primary tactic.
  [Tempo transitions][tempo], [practicing DJs' approaches][tempo-djs].

#### D02 — Half-time / double-time pulse bridge

- **Given A and B** share a convincing pulse relationship despite different
  reported BPMs, with compatible snare hierarchy, subdivisions and energy.
- **Use** an explicit pulse mapping before solving cue and tempo alignment.
  Keep actual playback speed near the source's natural speed.
- **Reject** a bare 2:1 BPM match with incompatible groove. Reinterpreting pulse
  does not authorize playing a song at twice or half its normal speed.
- **Status** Implemented measured pulse/accent mapping with separate source grids; a numerical 2:1 BPM ratio alone cannot authorize it.
  [Half/double-time practice][tempo-djs], [tempo-bridge options][tempo].

### Sudden, structurally supported transitions

#### C01 — Phrase cut

- **Given A** completes a musical statement, **and B** has a strong attack at
  the corresponding boundary.
- **Use** a musically immediate switch with a minimal de-click envelope. Keep
  phrase, vocal and energy continuity explicit even though overlap is short.
- **Reject** a cut inside a word, pickup or unresolved phrase. An attractive
  downbeat alone does not make the cut appropriate.
- **Status** Implemented supported phrase cuts with de-clicking and vocal protection.
  [Cut technique][ni-basic].

#### C02 — Drop swap / drop replacement

- **Given A** reaches a build or drop boundary, **and B** offers a more suitable
  drop than a combined A/B peak.
- **Use** B's arrival in place of A's drop. Clear A's bass/lead before B lands;
  any retained tail needs its own ownership and vocal checks.
- **Reject** a weak or mismatched landing and an unfinished A phrase. Prefer
  this to M05 when both drops together are crowded.
- **Status** Implemented measured arrival/drop replacement with explicit ownership and headroom checks.
  [Drop replacement guidance][double-drop].

#### C03 — Buildup-to-drop handoff

- **Given A** has a coherent buildup, **and B** has a drop whose timing, energy
  and harmonic landing fulfill that buildup.
- **Use** A's build to announce B's arrival. Keep A intact through the
  measured end of the build and start the cut or its de-click ramp there
  or later. Measure the pickup-to-downbeat interval explicitly.
- **Reject** a build that promises an incompatible groove or an isolated
  snare-rush peak misclassified as the actual arrival.
- **Status** Implemented measured buildup-to-arrival scheduling; unsupported event timing remains ineligible.
  [Buildup transition examples][crossfader].

#### C04 — Natural end-to-end handoff

- **Given A** has a musically complete ending, **and B** has an intentional
  beginning that works after it.
- **Use** that ending and entrance, retaining an expressive breath if warranted.
  No beatmatch is required when audible rhythmic material does not overlap.
- **Reject** accidental dead air, clipped decay or a weak mid-phrase start.
  This can be subtle when the texture carries continuity.
- **Status** Implemented natural ending/entry scheduling on structural timing, without requiring or inventing a beat grid.
  [End-to-end transitions][basic].

#### C05 — Loop tightening / roll / stutter release

- **Given A** supplies a safely repeatable rhythmic fragment, **and B** has a
  precise landing that benefits from a short tension build.
- **Use** a written sequence of loop lengths or repeat counts, then release
  at B's arrival. Declare whether A's underlying transport continues or pauses.
- **Reject** chopped essential lyrics or a release with the wrong beat phase.
  `expressive_opt_in`; bounded duration and repetition are mandatory.
- **Status** Implemented bounded loop/roll/stutter edits and transport/release validation; expressive opt-in.
  The roll's beat clock must remain steady; this schedule cannot combine with D01.
  [Loop performance examples][crossfader], [roll and stutter effect families][serato-fx].

#### C06 — Rhythmic gate / transform cuts

- **Given A and B** have complementary rhythmic gaps and reliable grids.
- **Use** quantized gain cuts to alternate attention over a short interval.
  Preserve the designated bass/drum foundation and land on a complete phrase.
- **Reject** cuts through required syllables or an uncertain subdivision.
  `expressive_opt_in`; this is a deliberate audible pattern.
- **Status** Implemented explicit rhythmic gate patterns with subdivision and vocal checks; expressive opt-in.
  Fixed-period gates require a steady output clock and cannot combine with D01.
  [Gater mechanics][effects].

#### C07 — Same-song / remix switch

- **Given A and B** are verified related versions with a corresponding phrase,
  lyric or motif, and B provides a useful change in arrangement or energy.
- **Use** the shared event as the handoff, matching its actual timing and pitch.
- **Reject** relationships guessed from filenames alone, mismatched vocal takes
  or versions with incompatible phrase lengths. Resolve phasing risk before
  overlapping substantially identical material.
- **Status** Implemented evidence-based version/motif correspondence and bounded switching; filenames alone are not evidence.
  [Version-switch demonstration][crossfader].

#### C08 — Tone play / pitch play

- **Given A** contains a clean, recognizable pitched sample, **and B** has a
  verified motif the sample can introduce.
- **Use** an explicitly transcribed and timed sample sequence, then B's entry.
  `expressive_opt_in`; distinguish intentional note changes from normal
  pitch-preserving tempo movement.
- **Reject** uncertain transcription, poor isolation or a melodic promise B
  does not fulfill. Exclude from unattended default mixes.
- **Status** Implemented individual-note capture and timed pitch events with supported pitch/motif and isolation checks; expressive opt-in.
  [Tone and pitch-play examples][crossfader].

#### C09 — Noise sweep / riser accent

- **Given A** is building or intentionally clearing, **and B** has a strong
  supported landing.
- **Use** a short, level-bounded accent with a written stop or release before
  it obscures B's attack. `expressive_opt_in`.
- **Reject** use as camouflage for an unsuitable cue, excessive loudness or a
  tail covering B's first vocal. The underlying handoff must work on its own.
- **Status** Implemented deterministic bounded noise accents and landing/tail validation; expressive opt-in.
  [Buildup effect families][rmx].

#### C10 — Spiral / pitch-delay bridge

- **Given A** offers an isolatable short fragment, **and B** leaves room for an
  intentionally conspicuous pitched departure and clear new arrival.
- **Use** a bounded rising or falling pitch-delay treatment with explicit tail
  termination. `expressive_opt_in`; character may be `abrupt`.
- **Reject** an exposed conflicting melody, uncertain tail duration or choosing
  the effect merely because ordinary matching failed.
- **Status** Implemented bounded pitch-delay processing with explicit capture and termination; expressive opt-in.
  [Spiral effect family][rmx], [FX inventory][serato-fx], [delay-time pitch behavior][mixer-fx].

#### C11 — Incoming tease / sample switch

- **Given A** has rhythmic space, **and B** contains a recognizable short motif
  that can preview its later arrival without colliding with A's foreground.
- **Use** one or a few intentional appearances, then the full entry on a
  supported boundary. Vocal snippets still obey the no-overlap rule.
- **Reject** excessive repetition, truncated language or a teaser whose final
  arrival is too distant to make musical sense. `expressive_opt_in`.
- **Status** Implemented incoming motif/sample edits, finite repetitions and validated landing; expressive opt-in.
  [Loop teasing][loops], [sample switches][crossfader].

#### C12 — Reverse / slip flourish

- **Given A** has a dispensable nonvocal fragment near a complete ending,
  **and B** has a precise landing after the flourish.
- **Use** a short explicit reverse/edit span, declaring the underlying transport
  position and exact return point. `expressive_opt_in`.
- **Reject** uncertain return phase or reversal of a required vocal phrase.
  A slip-mode flourish is distinct from an emergency backspin.
- **Status** Implemented explicit reverse/slip source edits and return-position validation; expressive opt-in.
  [Slip performance examples][crossfader], [buffer manipulation][effects].

#### C13 — Polyrhythmic loop overlay

- **Given A** has a steady, uncluttered groove, **and B** supplies a short clean
  percussion loop whose repetition creates an intentional cross-rhythm.
- **Use** a finite overlay with an explicit common landing and continuous bass
  ownership. `expressive_opt_in`; listener evaluation is essential.
- **Reject** accidental phase drift presented as polyrhythm, competing
  foreground rhythms or a release that misses the planned common boundary.
- **Status** Implemented percussion-loop overlay with explicit common landing and language/ownership checks; expressive opt-in.
  Fixed-period overlays require a steady output clock, whole cycles at the landing, and no D01 modifier.
  [Polyrhythmic-loop demonstration][crossfader].

#### C14 — Percussion-loop tempo bridge

- **Given A** can relinquish its pitched and vocal material at a complete
  ending, **and B** has a verified entry at a tempo beyond a normal D01 blend.
- **Use** a clean percussion loop as the temporary rhythmic foundation. Remove
  A, change the loop's tempo along a bounded trajectory, then introduce B and
  release the loop on its planned landing. `expressive_opt_in`.
- **Reject** unsuitable loop seams, audible artifacts or an unsupported tempo
  trajectory. Its independent render-quality limits must be explicit; D01's
  normal limits must not silently expand. Rhythm must remain intentional during
  the move, which may sound conspicuous even without pitched material.
- **Status** Implemented independent-pulse percussion-loop scheduling with a bounded expressive speed trajectory; normal stretch policy is not widened.
  [Percussion-loop tempo changes][tempo], [loop descent in practice][tempo-djs].

### Abrupt recovery tactics

These are excluded from ordinary offline route scoring. They require the
emergency policy below, a renderable fallback, and a saved reason.

#### E01 — Vinyl / tape brake

- **Given A** must stop before a normal safe continuation is possible,
  **and B** has a ready, verified landing or safe restart.
- **Use** a short intentional forward slowdown to silence, then B. Record its
  pitch/speed envelope separately from normal time-stretch constraints.
- **Reject** routine aesthetic use in the default profile or an unready B.
- **Status** Implemented runtime-only motion schedule and verified-buffer recovery; unavailable to normal selection and offline exports.
  [Turntable effect mechanics][effects], [stop behavior][backspin-manual].

#### E02 — Backspin / rewind exit

- **Given A** needs an emergency exit, **and B** has a safe restart after a
  conspicuous discontinuity.
- **Use** a bounded reverse-motion effect with defined speed and fade. Backspin
  and rewind have different motion envelopes; save the chosen variant.
- **Reject** using it to hide an avoidable poor cue or leaving reverse audio
  over B's first vocal.
- **Status** Implemented runtime-only reverse-motion recovery with explicit speed/fade and verified captured PCM.
  [Backspin and rewind distinctions][backspin-manual], [release effects][rmx].

#### E03 — Scratch stop / scratch exit

- **Given A** cannot continue safely under the emergency policy, **and B** has
  a verified recovery entry.
- **Use** only a supported, explicitly scheduled scrub/cut gesture and release.
  A brake must not be labeled a scratch because it slows audio down.
- **Reject** every nonemergency invocation, including poor compatibility or
  low tactic scores. Default automatic offline mixes must not choose this.
- **Status** Implemented runtime-only scheduled scrub/cut recovery; **emergency-only by user requirement**, never ordinary fallback.
  [Separate scratch control mechanics][backspin-manual].

#### E04 — Minimal recovery fade / cut

- **Given A** has an unexpected availability/decoding failure, **and B** is
  already verified and executable.
- **Use** the least disruptive prevalidated recovery, with de-clicking and
  immediate ownership transfer. Prefer it over dramatic effects when safer.
- **Reject** an unready fallback or an emergency that was knowable during
  offline planning. Do not silently splice a failed export.
- **Status** Implemented minimal live recovery and audit policy; offline infeasibility remains a planning error.
  [documented DJ foundation][ni-basic].

## Selection and path finding

### Hard constraints before preference scores

1. Enumerate viable A exits and B entries, supported window lengths and layer
   arrangements. Keep several options per pair instead of one preferred cut.
2. Expand them only into tactics supported by the renderer and analysis
   capabilities. Unknown required evidence makes a specialized tactic
   ineligible; it does not grant permission to assume compatibility.
3. Reject vocal collisions, truncated phrases, unsafe stems, unsupported meter
   mappings, timing/stretch violations, crowding without a feasible ownership
   solution, and insufficient source or effect-tail duration.
4. Rank the remaining cue/tactic/hold/exit combinations by phrase fit, layer
   complementarity, vocal continuity, tonal fit, groove continuity, appropriate
   energy change, useful hold duration and rendering risk. Penalize pointless
   processing, uncertain evidence and tiring repetition.
5. Search the route with its incoming state. A promising A→B must leave B a
   usable passage and later exit for B→C. Verify the prepared audio of
   shortlisted joins before committing the route.
6. Render the saved schedule, check it, and publish the WAV only after the
   entire plan passes. Record rejection reasons and the selected alternative.

No scalar “compatibility” score may override a hard constraint. The older
generic region scorer still contains placeholder collision/disruption costs.
They are not tactic admission. The tactic engine instead measures each
candidate's actual lane allocation, gains, EQ/edit phases and source coverage;
an unavailable measurement stays unknown rather than borrowing favorable
measurements from another tactic.

### Preference rules

| Given the validated candidate | Prefer | Do not infer |
| --- | --- | --- |
| Stable dense complementary drum mashup, safe eight-bar resolution | M01, six hold plus two exit bars | That any two intense full mixes should overlap |
| Spacious bed, stable incoming drums, decisive clean arrival | M02 or S05, with hold chosen from phrase evidence | That every successful mashup needs a long fade |
| Drumless outgoing vocals/texture and usable incoming percussion | S05, extending to M01/M02 when supported | That the absence of loud bass proves drums are absent |
| Outgoing break followed by a supported drum return | S06 at that return and phrase boundary | Permission to enter a beat or partial phrase early |
| Useful compatible upper layers but conflicting bass | S03 or S04 | Permission to mute all drums by removing low EQ |
| Sparse intro/outro and no valuable sustained composite | S01 or S02 | A need to manufacture a mashup |
| Dense competing drops with a strong replacement landing | C02 | A reason to force a double drop |
| Modest tempo gap with stable shared rhythm | D01 attached to the selected tactic | Unbounded stretching to make a route feasible |
| Large tempo gap and a verified beatless bridge | S07, possibly S09/S11 | Half-time compatibility based only on BPM arithmetic |
| No ordinary feasible transition | Another cue, tactic, window, order or track | An emergency warranting scratches |

Prefer compatible selected-stem harmony for long pitched overlaps. Drum-only
bridges can work across keys, whereas adjacent key-wheel labels do not prove
two foreground melodies fit. Local evidence is more useful than a single key
for the entire recording. [Harmonic mixing guidance][harmonic]

Route search should carry remaining phrase/runway, current tempo, active stems,
audible effect tails and energy trajectory as state. Preserve explicit user
cue constraints and deterministic seeds. Variety is a soft preference; it must
not force a worse transition just to use another tactic.

First and last tracks need the same musical scrutiny. Start at an intentional
entry with pickup support, and let the final track reach a complete statement
or natural exit. Neither requirement forces playback from file start to EOF.
The automatic picker carries multiple measured entries in a bounded beam
and validates the assembled route with a 45-second solo-source reserve.
Explicit ordered chains use one-join lookahead. Both approaches still
require listening to assess musical completeness.

### Emergency policy

For offline synthesis, an infeasible join is a planning failure. Replan before
rendering. If no permitted route exists, return a clear diagnostic and retain
the previous valid output. The ability to render an effect does not authorize
using it as an escape from analysis.

For the separate live recovery API, an emergency means an unexpected runtime condition
that prevents the scheduled continuation, such as loss of an audio source or
a late decode failure. Prefer a prepared safe continuation or minimal recovery
fade first. A plan may enable E01–E03 only for a concrete runtime condition,
with a verified next source, bounded effect and recovery timing. Scratch exits
remain forbidden during normal route selection.

E01–E03 also require verified healthy buffered audio and ready effect/transport
state. An unavailable or corrupt source cannot supply a brake, backspin or
scratch unless the required material was already captured safely. Otherwise
use E04 or the prepared continuation; never attempt to process missing audio.

Save `emergency_reason`, the failed condition, rejected safe recoveries, the
chosen prevalidated fallback and actual activation time. “Low score,” “boring,”
“uncertain beatgrid,” and “large BPM mismatch” are not emergency reasons.

The implemented API is [`LiveRecoveryRenderer`](crates/audio/src/live_recovery.rs).
It accepts immutable, digest-verified resident PCM and a verified next source;
it does not reopen the failed source. It prefers prepared continuation/minimal
recovery before explicitly permitted motion. The offline renderer rejects
emergency schedules, even if a saved plan claims an activation. This API has
state/PCM tests but is **not connected to the existing Player transport**.

## Plan and renderer contract

Each accepted join MUST record the following information in inspectable JSON.

- Stable tactic ID and policy version, arrangement, character and permission.
- Source identities, analysis generations, cue frames and evidence references.
- Supported phrase/bar anchors, meter, pulse mapping and confidence semantics.
- Output time map and frame boundaries for entry, hold, exit and completion.
- Ownership and gain schedules for drums, bass, other and vocals, or explicit
  stereo/EQ processing when stems are not used.
- Vocal completion evidence and gates, including B's complete phrase/pickup
  entry, the guard interval and tails.
- Effect input/capture, parameters, feedback bounds, wet/dry automation, tail
  lifetime, reset state, and source transport behavior for loops or slips.
- Stretch/headroom limits, validation results, reason for selection and
  executable alternatives. Include an emergency reason only when applicable.

The following is an **illustrative decision summary**, not a new accepted
render-plan schema. A real plan must additionally carry source frame positions,
output frame positions, exact curves and referenced analysis records.

```json
{
  "policy_version": "mixing_tactics_1",
  "tactic_id": "M01",
  "arrangement": "mashup",
  "character": "subtle",
  "permission": "normal",
  "meter": { "beats_per_bar": 4, "beat_unit": 4 },
  "window_bars": 8,
  "hold": { "start_bar": 0, "end_bar": 6 },
  "accompaniment_exit": { "start_bar": 6, "end_bar": 8 },
  "drums": { "owner_during_hold_and_exit": "B" },
  "bass": { "hold_owner": "A", "final_owner": "B" },
  "vocals": {
    "hold_owner": "A",
    "schedule": "independent_phrase_safe_gates",
    "minimum_guard_ms": 300,
    "allow_overlap": false
  },
  "required_predicates": [
    "grid_lock", "phrase_ready", "tonal_fit", "vocal_safe",
    "complementary", "dense", "steady_through", "stem_safe"
  ],
  "on_infeasible": "replan_cue_or_window"
}
```

Renderers MUST execute the plan without choosing new musical cues. Gain curves
should be correlation/headroom aware rather than assuming equal-power sums
always maintain loudness. Restore the incoming full mix only after the stem
ownership schedules finish, avoiding doubling its drums or reintroducing a
muted vocal early. Keep stereo processing linked where needed.

Effects need bounded state, tail allocation and deterministic reset behavior.
Closing an effect's input does not mean its stored audio has stopped sounding.
The vocal collision validator must include wet tails. Loop/reverse operations
need explicit source-edit instructions; existing strictly increasing time maps
cannot represent repeated or backward source traversal.

Each export must check finite samples, headroom, actual beat skew and scheduled
boundaries. Current output rejects estimated true peaks above −1 dBTP using a
4× sinc engineering meter; do not describe that meter as certified. Objective
checks cannot certify that a transition is pleasant or imperceptible.

## Implementation map

| Capability | Executable implementation | Validation boundary |
| --- | --- | --- |
| M01/M02 and stem handoffs | Independent drum/bass/other/vocal schedules; exact six-plus-two or spacious brisk exit | ASR/separation/density uncertainty can reject a real candidate; no title-specific exceptions |
| Blend, EQ and structural cuts | Per-tactic admission, ownership schedules, explicit natural/phrase arrivals and selected-lane profiles | Role/phrase/headroom heuristics need broader listening calibration |
| Tempo modifiers | Validated cubic maps, shared bar trajectories, measured half/double-pulse hierarchy and independent C14 clock | No synthetic meter or large-speed exception disguised as normal beatmatching |
| Filter, echo, reverb, freeze, gate, noise, spiral | Versioned bounded effect specifications, state, automation, captures and wet-tail validation | Numerical tests establish DSP behavior, not musical taste |
| Loop, roll, reverse, tease and pitch sampling | Explicit finite source edits, transport rules, PCM seams and supported note/motif events | Missing loop/language/motif evidence prevents admission |
| E01–E04 | Runtime-only contracts, verified PCM recovery API and activation audit; offline refusal | No normal emergency selection and no Player transport integration |
| Full-track evidence | Pinned local models, four source-aligned stems, source-time ASR/acoustics, bar/unmetered descriptors and pair profiles | Scores are uncalibrated; unknown is retained |
| Storage and cache | Immutable logical records in the one existing WAV sidecar; checksum-pinned binary stems; dependency-specific reuse and temporary recovery | 8 MiB per logical record, 256 MiB per sidecar; long-form sharding/GC remain unimplemented |

The executable contract is in [types](crates/types/src/tactics.rs), with
[eligibility and schedules](crates/planner/src/tactics.rs),
[saved-analysis adapter](crates/planner/src/tactic_evidence.rs),
[cached stem/vocal evidence](crates/planner/src/cached_tactic_evidence.rs),
[allocation-specific pair evidence](crates/planner/src/tactic_pair_evidence.rs),
[cue enumeration](crates/planner/src/stitch.rs),
[structural/native clocks](crates/planner/src/tactic_cues.rs), and
[chain assembly](crates/cli/src/chain.rs). Processing is in
[tactic rendering](crates/audio/src/tactic_render.rs),
[effect DSP](crates/audio/src/tactics.rs),
[source edits](crates/audio/src/edits.rs), and
[live recovery](crates/audio/src/live_recovery.rs).

[Full-track orchestration](scripts/analyze-tactics.py),
[vocal evidence](scripts/vocal-phrase-evidence.py), and
[signal evidence](scripts/tactic-signal-evidence.py) feed
[immutable publication](crates/cli/src/tactic_analysis.rs). Updates to signal
analysis can reuse verified stems and vocal evidence; provider updates can
reuse the stems. Source hash, timebase, beat artifact and correction revision
must still match. Supplementary generations never silently replace a saved
plan's original cue/grid generation.

Use `mixlib analyze-tactics --track TRACK_ID --separator-python PYTHON` to cache
the evidence, `mixlib tactics --plan PLAN_ID --json` to inspect eligibility, and
`mixlib tactics --plan PLAN_ID --apply --tactic M01 --json` to request a validated
schedule in a new plan. `stitch` searches cue and tactic alternatives and renders
the chosen result. `--allow-expressive` enables only the expressive family;
it does not authorize emergencies. See [setup and commands](IMPLEMENTATION.md)
for model installation, automatic chains and rendering the returned plan ID.

Automated tests cover all 38 contract IDs and every processing family, including
unsafe-evidence refusal, vocal/tail collisions, exact ownership/envelope timing,
source edits and live/offline recovery separation. Only selected real songs are
being audited. A successful render proves execution and objective checks passed;
it does not establish that a tactic is pleasant, imperceptible, or generally
appropriate. The listening cases below remain the musical validation work.

## Acceptance cases

| Case | Required result |
| --- | --- |
| Dense compatible eight-bar drum mashup | Six complete hold bars; accompaniment fade begins exactly at elapsed bar 6 and ends at 8; B drums remain steady |
| Sparse compatible bed with a strong arrival | M02/S05 remains eligible; the planner does not impose M01 purely because both source files are loud |
| A vocal finishes inside the two-bar exit | Accompaniment timing stays fixed while A finishes and B observes the ≥300 ms vocal guard |
| B is mid-line when A's vocal guard clears | B remains muted until a supported complete entry, or the planner changes the cue/window; no mid-word or mid-phrase gate opening |
| A vocal cannot finish safely in eight bars | Different cue, longer supported window or another tactic; no clipped line and no simultaneous vocals |
| Incoming vocals leak through a nominal drum stem | Reject or repair the prepared candidate; a muted vocal gain alone is not a passing check |
| Parasite → Perfect Girl phrase-entry regression | Preserve the supported complete phrase before drum entry; validate from the analysis generation, not a hardcoded title or timestamp |
| Multitasker → Seshat low-drum passage | Prefer a verified drum lead/linger when the whole candidate window supports it |
| Rome Is Burning → Anges compatible sustained material | Consider extended holds and compare exits; do not force a short fade without evidence |
| Same key, competing foreground melodies | Reject the pitched overlay despite key/chroma similarity; try fewer audible layers |
| Two dense drops without ownership/headroom | Reject M05; consider drop replacement or another cue |
| Different keys, complementary drum-only bridge | Do not reject solely for full-track key distance |
| Changing meter or ambiguous phrase evidence | Require a supported mapping or choose another tactic; never force a four-beat/eight-bar lattice |
| Tempo moves during a transition | Hold, fades, vocal gates and effects follow the same output map; check every beat, not just endpoints |
| 2:1 BPM labels with incompatible grooves | Reject D02; no automatic double-speed playback |
| Echo/reverb containing A vocals | Treat the tail as A vocals until inaudible; clear it before B vocals and subsequent C vocals |
| Loop or slip release | Correct seam and return beat, finite repetitions, completed language and no concealed source-time discontinuity |
| Offline route has no valid join | Replan or report infeasibility; E01–E03 remain unavailable |
| Unexpected runtime source failure | Only an explicitly enabled and verified recovery may activate; log its actual cause |
| Reordered multi-song route | Revalidate incoming state, subsequent runway and tails; preserve user-pinned cues |
| First/last track | Intentional opening and complete final statement, with no accidental dead air or arbitrary truncation |

Use deterministic synthetic fixtures for exact timing, ownership, headroom,
tail and emergency invariants. Use the actual collection for blinded listening
comparisons of plausible alternatives, including 6+2 versus a brisk exit.
Record judgments of phrase completion, vocal clarity, groove continuity,
crowding and exit timing. A user's approval of one example is evidence for that
example, not proof that its tactic should be universal.

## Research basis and scope

Research performed 10 November 2026. Public instructional articles, first-person
DJ interviews and official manufacturer/developer references were searched and
their page contents read. This is a broad practical inventory of established
technique families, with variants grouped together. DJ terminology overlaps
and evolves, so it is not a claim to enumerate every named performance trick.
Linked pages are the source record; no mirrored full-text articles are needed.

Sources substantiate technique families and mechanics. Selection predicates,
the category model, exact schedules, emergency restrictions, numerical limits,
storage design and acceptance tests are application requirements or design
inferences. They have not been established as universal practice by these
sources. Hardware FX inventories establish names/capabilities, not evidence
that an automatic transition will sound good.

| Source | Contribution |
| --- | --- |
| [Digital DJ Tips — Five Basic DJ Transitions][basic] | Foundational transfer and ownership techniques |
| [Native Instruments — Phrase mixing][phrase] | Arrangement-aware cueing and vocal layering |
| [Club Ready DJ School — Can you Double Drop!][double-drop] | Density, dominance and alternatives to competing drops |
| [Digital DJ Tips — Using Sync, Hot Cues and Loops][loops] | Practical loop and cue workflows |
| [Native Instruments — Harmonic mixing rules][harmonic] | Tonal matching and its limits |
| [Digital DJ Tips — Are You Stuck in the Tempo Trap?][tempo] | Different ways to connect tempo regions |
| [Native Instruments — Five basic DJ transitions][ni-basic] | Basic cuts, fades, EQ and filtering |
| [Serato — Using Stems][stems] | Separate layer controls and stem effects |
| [Crossfader — 12 High-Energy DJ Transitions][crossfader] | Performance-oriented technique inventory |
| [Native Instruments — Traktor effect reference][effects] | DSP behavior and effect-state distinctions |
| [Native Instruments — How to use mixer effects with Traktor][mixer-fx] | Phrase-conscious delay/freeze treatments |
| [Pioneer DJ — How DJs think about tempo and genre][tempo-djs] | First-person tempo and groove decisions |
| [Pioneer DJ — RMX-1000][rmx] | Buildup, breakdown and release effect families |
| [Isotonik Studios — Backspin manual][backspin-manual] | Distinct stop, reverse and scratch mechanics |
| [Serato — DJ-FX & Channel FX list][serato-fx] | Cross-check of conventional effect names |

[basic]: https://www.digitaldjtips.com/rock-the-dancefloor/five-basic-dj-transitions/
[phrase]: https://www.native-instruments.com/blogs/blog/phrase-mixing
[double-drop]: https://www.clubreadydjschool.com/tribe-talk/getting-started/can-you-double-drop
[loops]: https://www.digitaldjtips.com/rock-the-dancefloor/using-sync-hot-cues-and-loops/
[harmonic]: https://www.native-instruments.com/blogs/blog/harmonic-mixing-rules-and-how-to-break-them
[tempo]: https://www.digitaldjtips.com/how-to-change-tempo-dj-sets/
[ni-basic]: https://www.native-instruments.com/blogs/blog/dj-transitions
[stems]: https://support.serato.com/hc/en-us/articles/5700921209615-Using-Stems
[crossfader]: https://blog.wearecrossfader.co.uk/blog/12-high-energy-dj-transitions/
[effects]: https://docs.native-instruments.com/online-guides/traktor-pro-manual/en/effect-reference.html
[mixer-fx]: https://www.native-instruments.com/blogs/blog/how-to-use-mix-effects-with-traktor
[tempo-djs]: https://blog.pioneerdj.com/djtips/how-djs-think-about-tempo-and-genre/
[rmx]: https://www.pioneerdj.com/en/product/dj-effectors/rmx-1000/
[backspin-manual]: https://isotonikstudios.com/wp-content/uploads/backspin_manual-v1.0.2.pdf
[serato-fx]: https://support.serato.com/hc/en-us/articles/10271055029263-DJ-FX-Channel-FX-list
