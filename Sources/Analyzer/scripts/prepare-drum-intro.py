"""Prepare an aligned excerpt for mixlib using local Hybrid Transformer Demucs."""
import importlib.util
import json
import sys
from pathlib import Path

import numpy as np
from scipy.signal import lfilter
import soundfile as sf
import torch
from demucs.apply import apply_model
from demucs.pretrained import get_model
from torchaudio.functional import resample


def ramp(phase, start, end):
    t = np.clip((phase - start) / (end - start), 0, 1)
    return t * t * (3 - 2 * t)


def separate(source, offset, end, model, names):
    with sf.SoundFile(source) as audio:
        rate = audio.samplerate
        audio.seek(offset)
        samples = audio.read(end - offset, dtype="float32", always_2d=True)
    if samples.shape[1] == 1:
        samples = np.repeat(samples, 2, axis=1)

    wave = torch.from_numpy(samples.T.copy())
    with torch.inference_mode():
        normalized = resample(wave, rate, model.samplerate)
        reference = normalized.mean(0)
        mean, scale = reference.mean(), reference.std()
        if not torch.isfinite(scale) or scale <= 1e-8:
            raise ValueError("excerpt is silent")
        stems = apply_model(model, ((normalized - mean) / scale)[None], shifts=0, device="cpu")
        parts = {}
        for name in names:
            centered = stems[0, model.sources.index(name)] * scale
            # Normalization removed one common mixture baseline. Restore it
            # once to accompaniment, not once per source or into an otherwise
            # silent vocal/drum estimate. This does not redistribute or conceal
            # the model's remaining reconstruction error.
            restored = centered + mean if name == "other" else centered
            signal = resample(restored, model.samplerate, rate)
            parts[name] = signal[:, :len(samples)].T.numpy()
    return samples, rate, parts


def load_separator():
    torch.set_num_threads(4)
    return get_model("htdemucs").eval()


def prepare(source, output, mode, offset, end, handoff, fade_end):
    samples, rate, parts = separate(source, offset, end, load_separator(), ("drums",))
    drums = parts["drums"]
    if mode == "remove":
        # Suppress the returning transient before its analyzed beat timestamp.
        frames = np.arange(len(samples)) + offset
        fade = ramp(frames, handoff - rate * 0.04, handoff - rate * 0.01)[:, None]
        mixed = samples - drums * fade
    elif mode == "cut":
        # Keep the drum stem alone, then de-click the incoming accompaniment at the cut.
        frames = np.arange(len(samples)) + offset
        fade = ramp(frames, fade_end, fade_end + rate * 0.005)[:, None]
        mixed = drums + (samples - drums) * fade
        stop = int(fade_end - offset + rate * 0.005) + 1
        mixed[stop:] = samples[stop:]
    elif mode == "fade":
        # Keep the incoming beat at a fixed level while the accompaniment fades in.
        frames = np.arange(len(samples)) + offset
        fade = ramp(frames, handoff, fade_end)[:, None]
        mixed = drums + (samples - drums) * fade
        mixed[fade_end - offset:] = samples[fade_end - offset:]
    else:
        # A complementary split makes the end of the handoff exactly the source mix.
        residual = samples - drums
        low_alpha, upper_alpha = 1 - np.exp(-2 * np.pi * np.array([200, 2500]) / rate)
        low = lfilter([low_alpha], [1, low_alpha - 1], residual, axis=0)
        upper = lfilter([upper_alpha], [1, upper_alpha - 1], residual, axis=0)
        phase = ((np.arange(len(samples)) + offset - handoff) / (fade_end - handoff))[:, None]
        mixed = (drums + (upper - low) * ramp(phase, 0, 0.75)
                 + (residual - upper) * ramp(phase, 0.15, 0.85)
                 + low * ramp(phase, 0.35, 1))
        mixed[fade_end - offset:] = samples[fade_end - offset:]
    if not np.isfinite(mixed).all():
        raise ValueError("non-finite separated audio")
    sf.write(output, mixed, rate, subtype="FLOAT")


class VocalGapMissing(ValueError):
    pass


class TacticEvidenceMissing(VocalGapMissing):
    def __init__(self, code, message):
        super().__init__(message)
        self.code = code


def validate_handoff_payload(payload):
    bars = np.asarray(payload["bars"], dtype=float)
    if (len(bars) < 7 or not np.isfinite(bars).all() or bars[0] != 0
            or np.any(np.diff(bars) <= 0) or len(payload["clips"]) != 2):
        raise ValueError("handoff needs two clips and increasing output bar boundaries from zero")
    for clip in payload["clips"]:
        if not (0 <= clip["offset"] <= clip["swap"] < clip["overlap_end"] <= clip["end"]):
            raise ValueError("invalid source clip bounds")
        if clip.get("warp"):
            warp = np.asarray(clip["warp"], dtype=float)
            if (warp.ndim != 2 or warp.shape[1] != 2 or len(warp) < 2
                    or not np.isfinite(warp).all() or np.any(np.diff(warp, axis=0) <= 0)
                    or abs(warp[0, 0] - clip["swap"]) > 1e-6
                    or abs(warp[-1, 0] - clip["overlap_end"]) > 1e-6
                    or abs(warp[0, 1]) > 1e-6 or abs(warp[-1, 1] - bars[-1]) > 1e-6):
                raise ValueError("warp must cover the overlap with increasing source/output coordinates")
    mashup = payload.get("mashup") or {}
    policy = mashup.get("exit_policy")
    if payload.get("tactic_id") == "M01" or policy in ("six_plus_two", "adaptive"):
        if (len(bars) != 9 or mashup.get("hold_bars", 6) != 6
                or (policy != "adaptive" and mashup.get("exit_bars", 2) != 2)):
            raise ValueError("M01 requires six hold bars and two exit bars in an eight-bar window")
    return bars


def _rms(samples):
    return float(np.sqrt(np.mean(np.square(samples, dtype=np.float64)))) if samples.size else 0.0


def _activity(samples, threshold, block):
    """Sample-domain RMS activity, with no claim about lyric or phrase boundaries."""
    if len(samples) == 0:
        return 0.0
    count = (len(samples) + block - 1) // block
    powers = np.mean(np.square(samples, dtype=np.float64), axis=1)
    sums = np.add.reduceat(powers, np.arange(count) * block)
    lengths = np.minimum(block, len(samples) - np.arange(count) * block)
    return float(np.mean(np.sqrt(sums / lengths) >= threshold))


def acoustic_vocal_evidence(vocals, timeline, rate, end):
    """Return measured sounding regions; these are explicitly not sung phrases."""
    power = np.mean(np.square(vocals, dtype=np.float64), axis=1)
    integral = np.concatenate(([0.0], np.cumsum(power)))
    edges = np.arange(0, end + 0.020, 0.020)
    edges[-1] = end
    edges = np.unique(np.minimum(edges, end))
    indices = np.searchsorted(timeline, edges)
    rms = np.sqrt(np.diff(integral[indices]) / np.maximum(1, np.diff(indices)))
    # This threshold is an amplitude heuristic, not a calibrated speech detector.
    threshold = float(np.clip(np.quantile(rms, 0.80) * 0.15, 0.0015, 0.006))
    active = rms >= threshold
    spans = []
    for i, sounding in enumerate(active):
        if sounding:
            if spans and edges[i] - spans[-1][1] <= 0.120 + 1e-9:
                spans[-1][1] = float(edges[i + 1])
            else:
                spans.append([float(edges[i]), float(edges[i + 1])])
    quiet = []
    start = 0.0
    for left, right in spans:
        if left - start >= 0.10:
            quiet.append([start, left])
        start = right
    if end - start >= 0.10:
        quiet.append([start, float(end)])
    return {
        "method": "vocal_rms_20ms_merge120ms/1",
        "semantics": "acoustic_activity_not_word_or_phrase_completion",
        "threshold_rms": threshold,
        "active_spans": spans,
        "quiet_windows": quiet,
    }


def _leakage_proxy(stems, selected, timeline, end):
    """Veto coherent copies of vocals in retained stems; absence is not proof."""
    frames = (timeline >= 0) & (timeline <= end)
    vocals = stems["vocals"][frames]
    lanes = {}
    for name in selected:
        lane = stems[name][frames]
        windows = []
        maximum_projection, maximum_correlation, active_windows = 0.0, 0.0, 0
        local_time = timeline[frames]
        for start in np.arange(0, end, 0.50):
            within = (local_time >= start) & (local_time < start + 0.50)
            reference, candidate = vocals[within], lane[within]
            reference_power = float(np.sum(reference * reference, dtype=np.float64))
            lane_power = float(np.sum(candidate * candidate, dtype=np.float64))
            dot = float(np.sum(candidate * reference, dtype=np.float64))
            projection = abs(dot) / max(reference_power, 1e-12)
            correlation = abs(dot) / max(np.sqrt(reference_power * lane_power), 1e-12)
            vocal_active = _rms(reference) >= 0.004
            if vocal_active:
                maximum_projection = max(maximum_projection, projection)
                maximum_correlation = max(maximum_correlation, correlation)
                active_windows += 1
            suspected = vocal_active and projection >= 0.08 and correlation >= 0.12
            if suspected:
                windows.append({"start_seconds": float(start), "end_seconds": float(min(end, start + 0.50)),
                                "vocal_projection_ratio": projection, "absolute_correlation": correlation})
        lanes[name] = {"suspected": bool(windows), "suspect_windows": windows,
                       "maximum_projection_ratio": maximum_projection,
                       "maximum_absolute_correlation": maximum_correlation,
                       "vocal_active_window_count": active_windows}
    return {
        "method": "coherent_vocal_projection_500ms/1",
        "semantics": "conservative_rejection_proxy_not_certified_absence",
        "status": "suspected" if any(v["suspected"] for v in lanes.values()) else "not_detected",
        "lanes": lanes,
    }


def measure_stem_evidence(clips, bars):
    """Describe actual stems and the selected A-other/bass/vocal + B-drum mix."""
    result = {"version": 1, "method": "prepared_stem_measurements/1", "clips": []}
    aligned = []
    # Analysis is deliberately modest-resolution. Rendering retains the full rate.
    common_rate = 2000
    common_timeline = np.arange(0, bars[-1], 1 / common_rate)
    for index, (_, samples, rate, stems, timeline, _) in enumerate(clips):
        times = timeline[:, 0]
        if any(part.shape != samples.shape or not np.isfinite(part).all() for part in stems.values()):
            raise ValueError("separated stems must be finite and match the source shape")
        residual = samples - sum(stems.values())
        inside = (times >= 0) & (times <= bars[-1])
        source_rms, residual_rms = _rms(samples[inside]), _rms(residual[inside])
        per_bar = []
        for start, end in zip(bars[:-1], bars[1:]):
            frames = (times >= start) & (times < end)
            source = samples[frames]
            level = _rms(source)
            per_bar.append({
                "start_seconds": float(start), "end_seconds": float(end),
                "source_rms": level,
                "source_peak": float(np.max(np.abs(source))) if source.size else 0.0,
                "stem_rms": {name: _rms(part[frames]) for name, part in stems.items()},
                "stem_activity_fraction": {
                    name: _activity(part[frames], max(0.002, level * 0.08), max(1, round(rate * 0.02)))
                    for name, part in stems.items()},
            })
        reconstruction_ratio = residual_rms / max(source_rms, 1e-9)
        result["clips"].append({
            "role": "outgoing" if index == 0 else "incoming",
            "source_rms": source_rms,
            "reconstruction_error_rms": residual_rms,
            "reconstruction_error_ratio": reconstruction_ratio,
            "reconstruction_supported": bool(reconstruction_ratio <= 0.15),
            "vocal_leakage": _leakage_proxy(
                stems, ("other", "bass") if index == 0 else ("drums",), times, bars[-1]),
            "acoustic_vocals": acoustic_vocal_evidence(stems["vocals"], times, rate, bars[-1]),
            "bars": per_bar,
        })
        aligned.append({name: np.column_stack([
            np.interp(common_timeline, times, part[:, channel]) for channel in range(2)
        ]) for name, part in stems.items()})
    retained = [aligned[0][name] for name in ("other", "bass", "vocals")]
    for key, lanes, names in (
        ("retained_bed", retained, ["A.other", "A.bass", "A.vocals"]),
        ("selected_composite", retained + [aligned[1]["drums"]],
         ["A.other", "A.bass", "A.vocals", "B.drums"]),
    ):
        composite = sum(lanes)
        per_bar = []
        for i, (start, end) in enumerate(zip(bars[:-1], bars[1:])):
            frames = (common_timeline >= start) & (common_timeline < end)
            data = composite[frames]
            level = _rms(data)
            active_lanes = sum(_activity(lane[frames], max(0.002, level * 0.08), 40) >= 0.60
                               for lane in lanes)
            peak = float(np.max(np.abs(data))) if data.size else 0.0
            crest_db = float(20 * np.log10(max(peak, 1e-9) / max(level, 1e-9)))
            # Compare the selected layers against their actual full source(s).
            reference = max(result["clips"][0]["bars"][i]["source_rms"], 1e-9)
            if key == "selected_composite":
                reference = max(reference, result["clips"][1]["bars"][i]["source_rms"])
            relative_level = level / reference
            score = (0.40 * min(active_lanes / 4, 1) + 0.30 * min(relative_level, 1)
                     + 0.30 * np.clip(1 - crest_db / 18, 0, 1))
            if active_lanes < 3 or level < 0.015:
                score = min(score, 0.61)
            per_bar.append({"start_seconds": float(start), "end_seconds": float(end),
                            "rms": level, "peak": peak, "crest_db": crest_db,
                            "active_lanes": int(active_lanes), "relative_level": relative_level,
                            "density_score": float(score), "dense": bool(score >= 0.62)})
        hold = per_bar[:6]
        levels = [bar["rms"] for bar in per_bar]
        median = float(np.median(levels))
        steady = median > 0 and all(0.35 * median <= x <= 2.8 * median for x in levels)
        result[key] = {
            "lanes": names, "method": "relative_level_crest_active_lanes/1",
            "semantics": "initial_density_heuristic_not_calibrated_probability",
            "dense": bool(len(hold) == 6 and all(bar["dense"] for bar in hold)),
            "steady": bool(steady), "bars": per_bar,
        }
    return result


def _quiet_between(acoustic, start, end):
    return not any(left < end and right > start for left, right in acoustic["active_spans"])


def fixed_mashup_vocal_schedule(payload, evidence, bars):
    """Require independent phrase evidence as well as measured acoustic support."""
    phrases = payload.get("vocal_phrase_evidence")
    if (not isinstance(phrases, dict) or not phrases.get("method") or not phrases.get("reference")
            or "rms" in str(phrases.get("method", "")).lower()):
        raise TacticEvidenceMissing("VOCAL_PHRASE_EVIDENCE_REQUIRED",
                                   "M01 requires sourced phrase-end and phrase-entry evidence; RMS gaps are only candidates")
    try:
        json.dumps(phrases, allow_nan=False)
    except (ValueError, TypeError) as error:
        raise TacticEvidenceMissing("VOCAL_PHRASE_EVIDENCE_INVALID",
                                   "phrase evidence must contain finite inspectable JSON values") from error
    evidence["vocal_phrase_evidence"] = phrases
    outgoing, incoming = (clip["acoustic_vocals"] for clip in evidence["clips"])
    sides = [phrases.get("outgoing", {}), phrases.get("incoming", {})]
    boundaries = []
    for side, acoustic, key in zip(sides, (outgoing, incoming), ("phrase_ends", "phrase_starts")):
        values = side.get(key, [])
        if (not isinstance(values, list)
                or any(not isinstance(x, (int, float)) or not np.isfinite(x) for x in values)):
            raise TacticEvidenceMissing("VOCAL_PHRASE_EVIDENCE_INVALID", "phrase boundaries must be finite output seconds")
        # The provider sees the prepared excerpt's lead-in and tail as well as
        # the overlap. Retain that evidence, but only schedule an in-window gate.
        values = [x for x in values if 0 <= x <= bars[-1]]
        if side.get("instrumental"):
            if acoustic["active_spans"]:
                raise TacticEvidenceMissing("INSTRUMENTAL_EVIDENCE_CONTRADICTED",
                                           "supplied instrumental evidence conflicts with measured vocal activity")
            values = [float(bars[6])] if key == "phrase_ends" else [float(bars[-1])]
        boundaries.append(sorted(set(values)))
    guard, fade_out, fade_in = 0.30, 0.05, 0.08
    for phrase_end in boundaries[0]:
        # Close in silence after the supplied phrase end; never truncate its tail.
        stop = max(float(bars[6]) + 0.06, phrase_end + fade_out + 0.02)
        if stop + guard + fade_in > bars[-1] or not _quiet_between(outgoing, phrase_end + 0.02, stop):
            continue
        for phrase_start in boundaries[1]:
            enter = phrase_start - fade_in
            if enter < stop + guard or phrase_start > bars[-1]:
                continue
            if not _quiet_between(incoming, enter, phrase_start - 0.02):
                continue
            # Muting B until its onset is permitted; turning it on within an earlier
            # active phrase is not. Evidence and local silence must agree.
            if any(left < enter < right for left, right in incoming["active_spans"]):
                continue
            evidence["vocal_schedule"] = {
                "method": phrases["method"], "reference": phrases["reference"],
                "outgoing_phrase_end": float(phrase_end), "incoming_phrase_start": float(phrase_start),
                "outgoing_gate_end": float(stop), "incoming_gate_start": float(enter),
                "guard_seconds": guard, "phrase_completion_inferred_from_rms": False,
            }
            return float(stop), float(enter)
    raise TacticEvidenceMissing("VOCAL_PHRASE_WINDOW_INFEASIBLE",
                               "no supported complete A ending and B entry fit the fixed eight-bar window")


def _provide_vocal_phrase_evidence(payload, clips, bars):
    config = payload.get("vocal_phrase_provider")
    if payload.get("vocal_phrase_evidence") or not config:
        return
    try:
        spec = importlib.util.spec_from_file_location("mixlib_vocal_phrase_provider", config["script"])
        if spec is None or spec.loader is None:
            raise ValueError("cannot load vocal phrase provider")
        provider = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(provider)
        loaded = [{"role": "outgoing" if index == 0 else "incoming",
                   "vocals": stems["vocals"], "sample_rate": rate,
                   "output_time": timeline[:, 0], "source": clip["source"],
                   "source_offset": clip["offset"]}
                  for index, (clip, _, rate, stems, timeline, _) in enumerate(clips)]
        payload["vocal_phrase_evidence"] = provider.build_phrase_evidence(loaded, bars, config)
    except Exception as error:
        raise TacticEvidenceMissing("VOCAL_PHRASE_PROVIDER_FAILED",
                                   f"vocal phrase provider could not verify this candidate: {error}") from error


def admit_fixed_mashup(payload, evidence, bars, clips=None):
    if not evidence["selected_composite"]["dense"]:
        raise TacticEvidenceMissing("MASHUP_NOT_DENSE", "selected audible layers do not support the dense-mashup policy")
    if not evidence["selected_composite"]["steady"]:
        raise TacticEvidenceMissing("MASHUP_NOT_STEADY", "selected layers change too much across the hold and exit")
    if any(not clip["reconstruction_supported"] for clip in evidence["clips"]):
        raise TacticEvidenceMissing("STEM_RECONSTRUCTION_UNSAFE", "separation residual is too large for a controlled handoff")
    if any(clip["vocal_leakage"]["status"] == "suspected" for clip in evidence["clips"]):
        raise TacticEvidenceMissing("STEM_VOCAL_LEAKAGE", "retained stems contain a coherent copy of the estimated vocals")
    if clips is not None:
        _provide_vocal_phrase_evidence(payload, clips, bars)
    return fixed_mashup_vocal_schedule(payload, evidence, bars)


def find_vocal_gap(outgoing_rms, incoming_rms, bars, first_search_bar=4, exit_seconds=0):
    """Find a quiet pocket on both sides and leave a guard between vocal gates."""
    earliest = bars[first_search_bar]
    latest = bars[-1]
    times = np.arange(0, latest, 0.1)
    thresholds = [
        np.clip(np.quantile([rms(t) for t in times], 0.8) * 0.30, 0.008, 0.018)
        for rms in (outgoing_rms, incoming_rms)
    ]
    guard = 0.30
    for stop in np.arange(earliest + 0.06, min(latest - guard - 0.15, latest - exit_seconds), 0.01):
        enter = stop + guard
        if outgoing_rms(stop - 0.04) <= thresholds[0] and incoming_rms(enter + 0.04) <= thresholds[1]:
            return float(stop), float(enter)
    raise VocalGapMissing(f"no separated-vocal gap after {first_search_bar} complete bars within the search window")


def prepare_vocal_handoff(payload_path):
    payload = json.loads(Path(payload_path).read_text())
    bars = validate_handoff_payload(payload)
    drum_lead_bars = payload.get("drum_lead_bars", 0)
    mashup = payload.get("mashup")
    adaptive_mashup = (mashup or {}).get("exit_policy") == "adaptive"
    fixed_mashup = (payload.get("tactic_id") == "M01"
                    or (mashup or {}).get("exit_policy") == "six_plus_two")
    if fixed_mashup:
        mashup = {"hold_bars": 6, "exit_bars": 2, "exit_policy": "six_plus_two"}
    first_search_bar = mashup["hold_bars"] if mashup else payload.get("first_vocal_search_bar", 4)
    if not isinstance(first_search_bar, int) or not 0 <= first_search_bar < len(bars) - 1:
        raise ValueError("invalid first vocal search bar")
    model = load_separator()
    clips = []
    for clip in payload["clips"]:
        samples, rate, stems = separate(
            clip["source"], clip["offset"], clip["end"], model, ("drums", "bass", "other", "vocals")
        )
        # Output seconds relative to the swap, on the planner’s shared clock.
        scale = (clip["overlap_end"] - clip["swap"]) / bars[-1]
        frames = np.arange(len(samples)) + clip["offset"]
        timeline = (frames - clip["swap"]) / scale
        if clip.get("warp"):
            source, output = np.array(clip["warp"]).T
            within = (frames >= source[0]) & (frames <= source[-1])
            timeline[within] = np.interp(frames[within], source, output)
        power = np.mean(stems["vocals"] ** 2, axis=1)
        integral = np.concatenate(([0.0], np.cumsum(power, dtype=np.float64)))

        def rms(time, timeline=timeline, integral=integral):
            lo, hi = np.searchsorted(timeline, [time - 0.065, time + 0.065])
            return np.sqrt((integral[hi] - integral[lo]) / max(1, hi - lo))

        clips.append((clip, samples, rate, stems, timeline[:, None], rms))

    decision = Path(payload["decision"])
    evidence = measure_stem_evidence(clips, bars)
    if adaptive_mashup:
        # A measured dense candidate must pass M01's stronger evidence checks.
        # A failed M01 is returned for replanning, never relabeled as a safe M02.
        fixed_mashup = evidence["selected_composite"]["dense"]
        mashup = {"hold_bars": 6, "exit_bars": 2 if fixed_mashup else 1,
                  "exit_policy": "six_plus_two" if fixed_mashup else "vocal_gap_one_bar"}
        evidence["exit_choice"] = {"requested_policy": "adaptive",
                                   "selected_tactic": "M01" if fixed_mashup else "M02",
                                   "basis": "selected_audible_layer_density"}
    exit_seconds = (
        (bars[first_search_bar + 1] - bars[first_search_bar]) * mashup["exit_bars"]
        if mashup else 0
    )
    if fixed_mashup:
        try:
            stop, enter = admit_fixed_mashup(payload, evidence, bars, clips)
        except TacticEvidenceMissing as error:
            decision.write_text(json.dumps({
                "status": "no_gap", "reason": str(error), "reason_code": error.code,
                "tactic_id": "M01", "evidence": evidence,
                "candidate_boundary_windows": {
                    clip["role"]: clip["acoustic_vocals"]["quiet_windows"]
                    for clip in evidence["clips"]},
                "required_evidence": ["complete_outgoing_phrase_end", "complete_incoming_phrase_or_pickup_start",
                                      "usable_selected_stems", "dense_stable_composite"],
            }, allow_nan=False))
            return
    else:
        try:
            stop, enter = find_vocal_gap(clips[0][-1], clips[1][-1], bars, first_search_bar, exit_seconds)
        except VocalGapMissing:
            # A later vocal pocket can still support the established gradual handoff.
            mashup = None
            drum_lead_bars = min(drum_lead_bars, 2)
            try:
                stop, enter = find_vocal_gap(clips[0][-1], clips[1][-1], bars, first_search_bar)
            except VocalGapMissing as error:
                decision.write_text(json.dumps({"status": "no_gap", "reason": str(error),
                                                "evidence": evidence}, allow_nan=False))
                return
    for index, (clip, samples, rate, stems, timeline, _) in enumerate(clips):
        if fixed_mashup:
            # The accompaniment clock is fixed independently of the vocal pocket.
            other = ramp(timeline, bars[6], bars[8])
            bass_in = ramp(timeline, bars[6] + 0.75 * (bars[7] - bars[6]),
                           bars[7] + 0.25 * (bars[8] - bars[7]))
            bass_out = 1 - bass_in
        elif mashup:
            # Sustain the composite groove, then exchange the texture and bass
            # on separate half-bar moves after the outgoing vocal phrase ends.
            other = ramp(timeline, stop, stop + exit_seconds / 2)
            bass_in = ramp(timeline, stop + exit_seconds / 2, stop + exit_seconds)
            bass_out = 1 - bass_in
        elif drum_lead_bars:
            # Leave a complete drum-only lead. Blend texture first, then exchange
            # bass with minimal overlap. Opening the vocal gate must not bypass
            # either envelope.
            start = drum_lead_bars
            other = ramp(timeline, bars[start], bars[-2])
            bass_out = 1 - ramp(timeline, bars[start + 1], bars[-3] + 0.12)
            bass_in = ramp(timeline, bars[-3] - 0.12, bars[-1])
        else:
            other = sum(
                weight * ramp(timeline, bar, bar + 0.16)
                for bar, weight in zip(bars, (0.22, 0.26, 0.26, 0.26))
            )
            bass_out, bass_in = 1 - other, other
        if index == 0:
            vocal = 1 - ramp(timeline, stop - 0.05, stop)
            kept = stems["other"] * (1 - other) + stems["bass"] * bass_out + stems["vocals"] * vocal
            # Keep the original before the swap. Remove its drums before the returning hit.
            swap = ramp(timeline, -0.04, -0.01)
            mixed = samples * (1 - swap) + kept * swap
        else:
            vocal = ramp(timeline, enter, enter + 0.08)
            # Gate the residual with the vocals, then rejoin the exact source.
            residual = samples - sum(stems.values())
            mixed = (stems["drums"] + stems["other"] * other + stems["bass"] * bass_in
                     + stems["vocals"] * vocal + residual * (np.minimum(other, vocal) if fixed_mashup else vocal))
            restore_at = bars[8] if fixed_mashup else max(stop + exit_seconds, enter + 0.08) if mashup else (
                bars[-1] if drum_lead_bars else enter + 0.08
            )
            mixed[timeline[:, 0] >= restore_at] = samples[timeline[:, 0] >= restore_at]
        if not np.isfinite(mixed).all():
            raise ValueError("non-finite prepared vocal handoff")
        sf.write(clip["prepared"], mixed, rate, subtype="FLOAT")
    decision.write_text(json.dumps({
        "status": "found", "outgoing_vocals_off": stop, "incoming_vocals_on": enter,
        "tactic_id": "M01" if fixed_mashup else "M02" if mashup else "S05" if drum_lead_bars else "S06",
        "mashup": {"hold_until": float(bars[6]), "exit_end": float(bars[8]),
                   "exit_policy": "six_plus_two"} if fixed_mashup else (
                       {"hold_until": stop, "exit_end": stop + exit_seconds,
                        "exit_policy": "vocal_gap_one_bar"} if mashup else None),
        "evidence": evidence,
    }, allow_nan=False))


if __name__ == "__main__":
    if sys.argv[1] == "vocal-handoff":
        prepare_vocal_handoff(sys.argv[2])
    else:
        prepare(*sys.argv[1:4], *map(int, sys.argv[4:]))
