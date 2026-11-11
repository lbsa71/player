# Feasibility validation

This document distinguishes software checks from physical/audio acceptance.
Do not mark the audio gate passed using callbacks alone.

## Audio Bridge extension — 6 Nov 2026

This is a new audio path. The older process-tap acceptance results below do not
establish live HAL driver or forwarding acceptance.

- Universal arm64/x86_64 HAL bundle builds with warnings treated as errors and
  passes local signature verification.
- Bundle factory harness loads the actual plug-in and passes all 42 rate/channel
  combinations, exact loopback, two independent readers, stale-frame silence,
  invalid-format/short-buffer rejection and monotonic clock checks.
- The harness uses address/undefined-behavior sanitizers; the dynamically loaded
  release driver itself is not sanitizer-instrumented.
- All 34 Swift tests pass. Coverage includes raw endpoint recording while monitoring is off/on/muted/reset;
  sample bit patterns remain unchanged, with no invalid buffers or record drops.
  Monitor reset does not replay its previous queue.
- Recording can arm while Spotify is paused without creating an unlabeled WAV;
  supplied metadata starts the first named segment.
- Release Player and the independent AudioBridge LaunchAgent are installed.
  MCP discovery/handshake, all 25 tool definitions, read-only requests and invalid
  gain handling pass. Initial installation left defaults unchanged; later listening
  tests explicitly selected Audio Bridge with user authorization.
- Administrator installation passed at 19:05 CET on 6 Nov 2026. Package receipt
  `se.lbsa71.AudioBridge.driver` reports 1.0.0, and the installed root-owned bundle
  passes strict code-signature verification. macOS SIP rejected the installer’s
  Core Audio service restart (error 150); the user's restart loaded the driver
  successfully. No protection settings or default audio devices were changed.
- Live virtual input passed at both 44.1 and 48 kHz: 352,414 and 383,807 nonzero
  signal frames respectively matched the generated Float32 sequence exactly,
  with zero discontinuities and zero invalid buffers. Both WAVs are finalized in
  `/Users/stefan/Documents/Audio`. Monitoring was off and gain changes did not
  alter samples. Evidence: `build/bridge-live-44100-validation.json` and
  `build/bridge-live-48000-validation.json` (local ignored artifacts).
- Live forwarding from 44.1 kHz endpoint to the 48 kHz MacBook speakers ran muted
  with zero monitor underflows/overflows. A continuous 441,024-frame signal stayed
  exact across two WAV files and monitor stop/restart. This validates routing and
  recording continuity, not audible output quality or physical unplug recovery.
  Evidence: `build/bridge-monitor-validation.json`.
- Sending SIGTERM to the independent LaunchAgent finalized shutdown and launchd
  restarted it; capture resumed on the same loaded endpoint, recording remained
  off, and the Mac's default output stayed unchanged. Evidence:
  `build/bridge-agent-restart-validation.json`.
- Fixed the agent's hardened-runtime signing by adding the audio-input entitlement;
  macOS then displayed its permission prompt and the user approved it. Only the
  agent was updated; the installed driver was unchanged.
- Spotify now routes through Audio Bridge to wired External Headphones at 44.1
  kHz. Initial listening was silent: live diagnostics revealed AVAudioEngine had
  followed the default-device change back onto the virtual endpoint. Fixed by
  releasing its graph before changing the Mac default and rebuilding against the
  physical output afterward. Configuration notifications and periodic checks stop
  monitoring if its actual device changes or its engine stops, while recording
  continues. The source render block also explicitly clears the silence flag.
  A new offline engine test verifies nonzero output after an initial silent block.
- Live verification after the fix confirms the actual monitor UID stays
  `BuiltInHeadphoneOutputDevice`, the engine runs, nonzero rendered frames advance,
  and buffer/monitor faults remain zero. Evidence:
  `build/bridge-headphone-route-validation.json`. The user confirmed clear audio
  through headphones on the corrected route. A subsequent snapshot after about
  21 minutes of capture still shows the physical headphone UID, zero invalid
  buffers, zero monitor underflows/overflows, and recording off. Physical unplug
  continuity and the full one-hour endurance check remain unverified.
- Spotify WAV capture finalized named Float32 files and followed an automatic
  playlist transition from Stronger to Thinking it Over. Seeking near the end
  caused a brief unavailable/paused metadata observation and therefore an extra
  numbered Stronger segment, as documented. All three WAV headers and nonzero
  sample data were validated. Evidence:
  `build/bridge-spotify-transition-validation.json`. Recording was stopped after
  the test; ordinary playlist playback continues through the endpoint.
- Driver operation across sleep/wake, one-hour drift/latency, wider-than-stereo
  external clients, multiple concurrent writers, live rates above 48 kHz and OS
  updates remain unverified. The current service deliberately accepts stereo only.

Reproduce driver checks with `./scripts/test-driver.sh`, service tests with
`swift test`, and MCP checks with `python3 scripts/test-mcp.py`. The output-only
integration probe `scripts/bridge-test-tone.c` selects the endpoint explicitly;
it does not change the system default or access any microphone. Compile with
CoreAudio, AudioToolbox and CoreFoundation, then run while generic recording is
active. It emits exact Float32 stereo values on a 997-frame sequence for
independent file comparison; use monitoring off for the initial check.

## Automated checks

Run `swift test`. Renderer coverage includes:

- Interleaved and planar stereo, both channel orders preserved at 44.1/48 kHz.
- Capture probe produces silence at its output (no duplicate route).
- Buffer-size mismatch clears output and increments a fault counter.
- NaN/infinite audio and gain cannot propagate to output.
- Gain ramps and silence cannot replay samples from earlier callbacks.
- Unsupported rates and integer PCM are rejected before I/O starts.
- Empty-source startup and repeated partial cleanup are safe.

These are synthetic buffer tests, not real hardware sample-rate, latency or
drift measurements. The callback is C, uses compile-time checked lock-free
atomics, and performs no allocations, locks, file access or logging. It has no
application queue: each callback maps that callback's stereo input to output.
HAL owns rate/clock alignment through the aggregate hardware clock and tap drift
compensation. Mapping or frame-count disagreement renders silence and stops the
route, rather than accumulating delay.

## Manual acceptance record

Record OS/build, architecture, Spotify version, output, tap/aggregate formats,
and the exact observations for each run. Export diagnostics before stopping
when possible; the final counters are also retained after stop.

| Check | Status |
| --- | --- |
| Release build, local signature and launch | Passed locally, 6 Nov 2026 (ad-hoc signature) |
| Nonzero Spotify samples on selected output | Passed: External Headphones, 48 kHz tap → 44.1 kHz aggregate |
| Source isolation | Passed with a separate `afplay` tone while Spotify was paused; browser/notification variants remain untested |
| Direct-output suppression; local gain 0% silences Spotify | Passed: user confirmed complete mute followed by audible return on the repeated five-second test |
| Direct Spotify audio returns after normal Quit | Passed: user confirmed Spotify remained audible after the repeated normal-quit test; Stop also returned cleanly |
| Direct Spotify audio returns after forced termination | Passed: user confirmed Spotify stayed audible after the repeated SIGKILL test; relaunch/rebuild also passed |
| Real hardware 44.1 and 48 kHz without speed/pitch change | Pending |
| One-hour wired run, zero buffer faults and no growing delay | Pending |
| Output unplug/reconnect and sleep/wake | Unplug passed: user confirmed Spotify paused, route released; reconnect and sleep/wake pending |
| Seek and track transitions without stale audio | Live API reconciliation passed while routing, zero buffer faults; listening confirmation of transition quality pending |
| Measured added latency below 50 ms | Pending |

For isolation, pause Spotify while another app plays a tone. Player's **signal
frames** must stop advancing (a short tail is possible); the other application's
ordinary output should remain audible. Resume Spotify and confirm signal
frames advance. Source eligibility is an explicit Core Audio process-object
allowlist, with Spotify bundle ID and executable path verification; there is no
system-wide capture fallback.

For suppression, set Player's local gain to 0% while routing. Spotify should
become silent, while the independent app remains audible. Restore local gain,
stop the route, and confirm Spotify's direct output returns. Repeat normal quit
and forced termination separately. A private aggregate and conditional muting
are implementation choices, not evidence that crash recovery has passed.

## Gate and next milestones

### Local run, 6 November 2026

- macOS 26.6.2 (25G83), Apple silicon, Spotify 1.3.6.1921,
  External Headphones at 44.1 kHz.
- Found the active Spotify Core Audio process object (not a raw PID passed as an object).
- Initial aggregate-format validation stopped startup. Explicitly setting the
  private aggregate to the physical output's existing sample rate resolved that
  validation failure.
- System logs show an `AudioCapture` permission request for `se.lbsa71.player`.
  The HAL call to create the callback waited for that response; startup later
  failed with OSStatus 268451843 (`0x10004003`, a Mach IPC receive timeout).
  No callbacks or nonzero samples arrived. The route was released with no
  reported cleanup errors. After the user granted permission, a fresh routing
  attempt succeeded immediately.
- At 17:00–17:02 Stockholm time, the first live route processed 5,545,984 frames
  in 10,832 callbacks with **zero invalid buffers**. Tap format was 48 kHz,
  aggregate/output format 44.1 kHz. This is a short functional run, not a latency
  or long-term drift measurement.
- While Spotify was paused, a separate `afplay` process played a five-second
  synthetic 660 Hz tone at 4% amplitude. Captured nonzero frames remained exactly
  3,092,001 before and after; callback count advanced from 7,123 to 7,834, peak
  stayed zero and no buffer faults occurred. Resuming Spotify restored captured
  signal. Synthetic PCM was generated locally; no Spotify PCM was saved.
- Local gain was set to zero for five seconds and restored to 0.5. On the
  repeated test, the user explicitly confirmed that music muted and then came
  back. Audible direct-output suppression passed on this configuration.
- The subsequent route reached 41,252,864 processed frames (about 15.6 minutes
  at 44.1 kHz) with zero invalid buffers before the repeated normal-quit test.
  This does not establish the one-hour or measured latency/drift targets.
- Stop returned cleanly. Normal quit while routing exited the process and
  removed its control socket. A new route succeeded after relaunch. Forced
  termination (`SIGKILL`) while routing also allowed relaunch and a new healthy
  route, with no reboot or audio-service reset. The user confirmed audible
  playback after both repeated normal-quit and forced-quit tests. The initial
  isolation, conditional muting and teardown feasibility gate is passed on this
  Mac/output configuration; the broader acceptance matrix remains open.
- Seven synthetic renderer/lifecycle tests passed. MCP protocol smoke tests and
  live status/device/log reads passed. Socket permissions verified as 0600 in a
  0700 directory; MCP registered locally as `player-local`.

### Spotify and player shell, 6 November 2026

- Implemented PKCE in the system browser, a loopback-only callback with state
  validation, Keychain refresh credentials and in-memory access tokens.
- Google-backed Spotify sign-in succeeded. Live device discovery and current
  playback reads succeeded for the registered development application.
- Added explicit local Spotify device selection, serialized play/pause,
  next/previous, seek and transfer requests, conservative polling and observed
  playback reconciliation. Live pause/resume, seek, next/previous and transfer
  all matched observed Spotify state on the user-confirmed local Mac. Next
  changed the track, previous returned to the original, and seek restored its
  position. Transfer to the same Mac paused playback as requested; resume
  restored it. These tests ran with the headphone route active and zero buffer
  faults. They do not establish transfer from a separate remote device.
  Local evidence is in the ignored `build/live-control-validation.json`.
- Added now-playing artwork, seek, transport, local volume, output selection,
  connection settings and bounded diagnostics in the native SwiftUI shell.
- Added best-effort pause on output loss with a two-second teardown deadline;
  the headphone-unplug test passed: Spotify accepted pause, observed playback
  changed to paused, and the user confirmed audible playback stopped. Route
  teardown completed without cleanup errors. Sixteen invalid buffers occurred
  during device disappearance, so this is not a zero-fault endurance run.
  Reconnection, sleep/wake and network-failure behavior remain unverified.
- All 19 automated tests passed: seven audio tests and twelve Spotify tests
  covering PKCE/callback validation, refresh coalescing and invalid grants,
  serialization, 204, 401, 403, 429 and non-retry of ambiguous transport failure.
- Updated MCP protocol/live read-only smoke tests passed for the expanded tool
  catalog. The installed app exposes authentication/device/transport commands
  as well as audio routing, logs, status and normal quit.

### Remaining acceptance work

- Obtain listening confirmation of seek/track-transition quality; cross-device
  transfer and remote-playback route teardown still need a second device.
- Complete the hardware matrix above: real 48 kHz output, one-hour wired run,
  latency measurements, unplug/reconnect and sleep/wake cycles.
- Broaden device support beyond output-only stereo when needed.
- Developer ID signing, notarization and clean-Mac permission validation.

The installed app is locally ad-hoc signed. Distribution requires an Apple
Developer ID certificate and notarization access. No credentials belong in
this repository; the public client ID is configured locally in preferences.

### WAV debug recording, 6 November 2026

- Version 0.3 adds opt-in post-renderer Float32 stereo WAV recording in the
  configured debug folder, with metadata-driven track rotation and JSON mapping.
- Thirty automated tests pass (15 audio/recording, 15 Spotify). Recording
  tests compare every rendered Float32 bit with captured output, exercise ring
  wrap/overflow and concurrent producer/consumer ordering, decode 44.1/48 kHz WAVs through Core Audio, rotate without lost
  frames, preserve renderer storage after HAL ownership is released, split on
  pause/restart, reject WAV size overflow, and preserve existing filenames.
- The expanded MCP read-only/protocol smoke test passes after installation.
- After the user approved Keychain access, live WAV capture and rotation passed
  on External Headphones at 44.1 kHz. Two excerpts were saved in the requested
  Audio folder: Favorite Place (313,856 frames, 7.117 s) and No6 Sol (313,344
  frames, 7.105 s). Both have matching album/artist/track filenames and JSON
  metadata, valid RIFF sizes, Float32 stereo samples, nonzero signal, and were
  decoded by Core Audio (`afinfo`). The route reported zero invalid buffers.
  Evidence: ignored `build/live-wav-validation.json`.
- Normal quit while recording also passed: a third 158,208-frame WAV finalized
  with a valid header and metadata before Player exited. Relaunch restored
  Spotify authentication and left recording off. Evidence:
  `build/live-wav-quit-validation.json`.
- No claim of a full original track or sample-accurate track boundaries is made:
  files are observed recording segments, after local gain and HAL rate
  conversion. Force quit can leave an explicitly partial WAV.
- Playlist-link start uses the existing playback scopes, disables shuffle/repeat
  and checks their observed state before requesting the first playlist item.
  This flow has mocked request tests; an actual playlist start is still pending.

## 2026-11-11 — catalog recovery and independent Spotify route

- Added a Player-owned sidecar catalog; no driver/service catalog RPC. Existing
  `trackID` is reused. Schema 2 adds observed duration/continuity evidence; legacy
  sidecars remain unverified. Large Analyzer objects are omitted from the bounded
  cached response without changing their files. Header/length validation reads no
  PCM payload in normal library scans.
- The dedicated Spotify tap now feeds Audio Bridge at unity gain while the Mac's
  default output remains Built-in Speakers. Monitoring stayed off. Live source
  and endpoint sample counters advanced with zero invalid buffers; the user
  confirmed no music was audible through either device. Default output changes
  previously traced to Control Center were user selections, not driver fallback.
- HAL ignores `channels-in: 0` and reassigns stream object IDs when aggregate
  composition changes. Regression tests cover selecting the tap by validated
  terminal type and StartingChannel, separately mapping the IOProc stream mask.
  Physical loopback input is disabled; ambiguous layouts are rejected before I/O.
- With Spotify paused and the corrected tap active, an `afplay` Tink sound on the
  ordinary output produced zero nonzero samples in 905,280 recorded bridge frames.
  The test WAV is under ignored `build/isolation-check/`.
- Live full-track completion: “Spitting Off the Edge of the World (2).wav” contains
  11,354,880 frames at 44,100 Hz (257.480272 s), expected duration 257.746 s,
  observed start 0.835026 s/end 257.696057 s. Continuity/finalization are true,
  natural transition reason is retained, size is exactly 58 + frames × 8.
  Player reports `audioVerified=true`, `status=complete`, and one complete track;
  it advanced naturally into “Elephant Machine”. A bounded PCM check at 30 seconds
  contained nonzero audio (peak 0.181406).
- All 70 Swift tests passed (31 Spotify/recovery, 39 audio/catalog/renderer),
  release build/signature verification passed, and the installed MCP protocol
  smoke test passed. The HAL driver was not changed or reinstalled.
- Completeness uses a two-second observation tolerance, not sample-accurate source
  verification. A final track that pauses without a natural transition remains
  conservatively incomplete. Fresh physical-output loss/sleep/restart tests of
  this dedicated route remain separate from the earlier legacy-route validation.
- After the final app restart and renewed macOS permission approvals, live recovery
  restarted the same playlist, skipped the complete “Spitting Off the Edge of the
  World” without creating a third copy, and began “Elephant Machine (3).partial.wav”
  from the next incomplete track. Status reports one skipped track, active recovery,
  healthy source callbacks and no errors. Mac default remains Built-in Speakers;
  monitoring remains off. A bounded sample check confirms nonzero PCM in the new
  WAV. The installed MCP smoke test passed again after this restart.
