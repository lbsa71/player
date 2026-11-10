"""Measured source-frame evidence for automated mixing-tactic selection.

This module accepts already separated PCM, never uploads audio and never writes
sidecars itself. Its caller owns audio/model provenance and atomic persistence.
Scores are engineering heuristics, not calibrated probabilities. In particular,
quiet estimated vocals do NOT prove the original recording is instrumental.

Pitch uses a cumulative normalized lag-difference estimator with independent
spectral harmonic/fundamental checks. The lag-difference construction follows
the YIN family (de Cheveigne/Kawahara, DOI 10.1121/1.1458024); thresholds,
polyphony vetoes, event rules and pair decisions here are application policy.
"""
from __future__ import annotations

from hashlib import sha256
import json
from math import gcd

import numpy as np
from scipy.signal import butter, correlate, find_peaks, resample_poly, sosfilt, welch, coherence

METHOD = "tactic_signal_evidence/1"
LANES = ("drums", "bass", "other", "vocals")
SCORE_SEMANTICS = "uncalibrated_engineering_evidence_not_probability_or_certified_separation_quality"
ANALYSIS_RATE = 8000
EPS = 1e-12


def _pcm(audio, name):
    x = np.asarray(audio)
    if x.ndim == 1:
        x = x[:, None]
    if x.ndim != 2 or x.shape[1] not in (1, 2) or len(x) < 2 or not np.isfinite(x).all():
        raise ValueError(f"{name} must be finite mono/stereo PCM with at least two frames")
    return x.astype(np.float32, copy=False)


def _rms(x):
    return float(np.sqrt(np.mean(np.square(x, dtype=np.float64)))) if np.size(x) else 0.0


def _db(x):
    return float(20 * np.log10(max(float(x), 1e-12)))


def _clip(x):
    return float(np.clip(x, 0.0, 1.0))


def _mono(x):
    # Opposed stereo polarity must not turn an active passage into "silence".
    mono = x.mean(axis=1)
    channel_power = np.mean(np.square(x, dtype=np.float64), axis=0)
    if np.mean(mono.astype(np.float64) ** 2) < 0.1 * np.mean(channel_power):
        mono = x[:, int(np.argmax(channel_power))]
    return mono


def _resample(x, rate):
    if rate == ANALYSIS_RATE:
        return x
    divisor = gcd(rate, ANALYSIS_RATE)
    return resample_poly(x, ANALYSIS_RATE // divisor, rate // divisor).astype(np.float32)


def _envelope(x, block):
    power = np.mean(np.square(x, dtype=np.float64), axis=1) if x.ndim == 2 else np.square(x, dtype=np.float64)
    starts = np.arange(0, len(power), block, dtype=int)
    return np.sqrt(np.add.reduceat(power, starts) / np.minimum(block, len(power) - starts))


def _onsets(x, rate, pulse_bands=False):
    hop = max(1, round(rate * 0.01))
    envelope = _envelope(x, hop)
    difference = np.maximum(0, np.diff(np.r_[0.0, envelope]))
    floor = max(0.0008, float(np.quantile(envelope, 0.90)) * 0.12,
                float(np.median(np.abs(np.diff(envelope))))) * 2.5
    # Padding lets an actual onset at source frame zero become an event.
    peaks, props = find_peaks(np.r_[0., difference, 0.], height=floor,
                             prominence=floor * 0.5, distance=max(1, round(0.06 * rate / hop)))
    peaks = peaks - 1
    result = [{"frame": int(i * hop), "strength": float(h), "resolution_frames": hop}
              for i, h in zip(peaks, props["peak_heights"]) if 0 <= i < len(envelope)]
    if pulse_bands:
        mono = _resample(_mono(x), rate)
        filtered = {"low": sosfilt(butter(3, [40, 180], btype="bandpass", fs=ANALYSIS_RATE, output="sos"), mono),
                    "high": sosfilt(butter(3, [700, 3500], btype="bandpass", fs=ANALYSIS_RATE, output="sos"), mono)}
        span = round(ANALYSIS_RATE * 0.040)
        for event in result:
            at = round(event["frame"] * ANALYSIS_RATE / rate)
            for name, band in filtered.items():
                before = _rms(band[max(0, at-span):at])
                after = _rms(band[at:min(len(band), at+span)])
                event[f"{name}_band_strength"] = max(0.0, after-before)
            event["band_strength_method"] = "attack_RMS_above_prior_40ms_40_180Hz_700_3500Hz/1"
            event["band_strength_semantics"] = "measured_accent_energy_not_categorical_kick_or_snare_label"
    return result


def _spectrum(x, rate):
    mono = _mono(x)
    n = min(4096, len(mono))
    if n < 32:
        return {"chroma": [0.0] * 12, "tonalness": 0.0, "flatness": 1.0,
                "centroid_hz": 0.0, "low_fraction": 0.0, "mid_fraction": 0.0,
                "high_fraction": 0.0, "isolated_bin_fraction": 0.0}
    frequency, power = welch(mono, fs=rate, nperseg=n, noverlap=n // 2, detrend="constant")
    usable = (frequency >= 30) & (frequency < rate * 0.49)
    f, p = frequency[usable], power[usable].astype(np.float64)
    total = float(p.sum())
    if total <= 1e-15:
        return {"chroma": [0.0] * 12, "tonalness": 0.0, "flatness": 1.0,
                "centroid_hz": 0.0, "low_fraction": 0.0, "mid_fraction": 0.0,
                "high_fraction": 0.0, "isolated_bin_fraction": 0.0}
    flatness = float(np.exp(np.mean(np.log(p + 1e-20))) / (np.mean(p) + 1e-20))
    peaks, _ = find_peaks(p, prominence=max(float(p.max()) * 0.015, 1e-20))
    peak_mask = np.zeros(len(p), bool)
    for at in peaks:
        peak_mask[max(0, at - 2):min(len(p), at + 3)] = True
    tonalness = _clip(float(p[peak_mask].sum()) / total)
    chroma = np.zeros(12)
    pitched = (f >= 45) & (f <= min(5000, rate * 0.45))
    midi = 69 + 12 * np.log2(f[pitched] / 440.)
    # A fractional-bin vote avoids hard chroma jumps as local tuning drifts.
    lo = np.floor(midi).astype(int)
    frac = midi - lo
    np.add.at(chroma, lo % 12, p[pitched] * (1 - frac))
    np.add.at(chroma, (lo + 1) % 12, p[pitched] * frac)
    chroma /= max(float(chroma.sum()), EPS)
    isolated = np.zeros(len(p), bool)
    if len(p) > 4:
        neighborhood = (p[:-4] + p[1:-3] + p[3:-1] + p[4:]) / 4
        isolated[2:-2] = p[2:-2] > 100 * np.maximum(neighborhood, 1e-20)
    return {"chroma": chroma.tolist(), "tonalness": tonalness, "flatness": _clip(flatness),
            "centroid_hz": float(np.dot(f, p) / total),
            "low_fraction": float(p[f < 200].sum() / total),
            "mid_fraction": float(p[(f >= 200) & (f < 2500)].sum() / total),
            "high_fraction": float(p[f >= 2500].sum() / total),
            "isolated_bin_fraction": float(p[isolated].sum() / total)}


def _pitch_frame(frame, rate, minimum_hz):
    x = frame.astype(np.float64) - float(np.mean(frame))
    level = _rms(x)
    if level < 0.001:
        return None, "quiet"
    n = len(x)
    min_lag = max(2, int(rate / 1500))
    max_lag = min(n // 2, int(rate / minimum_hz))
    if max_lag <= min_lag + 2:
        return None, "short"
    fft_size = 1 << int((2 * n - 1).bit_length())
    transform = np.fft.rfft(x, fft_size)
    ac = np.fft.irfft(transform * transform.conj(), fft_size)[:max_lag + 1]
    sums = np.r_[0., np.cumsum(x * x)]
    lag = np.arange(1, max_lag + 1)
    difference = np.maximum(0., sums[n - lag] + sums[n] - sums[lag] - 2 * ac[lag]) / (n - lag)
    cmnd = np.r_[1., difference * lag / np.maximum(np.cumsum(difference), EPS)]
    minima, _ = find_peaks(-cmnd[min_lag:max_lag])
    minima += min_lag
    good = minima[cmnd[minima] < 0.15]
    if not len(good):
        return None, "aperiodic_or_multiple_fundamentals"
    period = int(good[0])
    left, middle, right = cmnd[period - 1:period + 2]
    denominator = left - 2 * middle + right
    refined = period + (0.5 * (left - right) / denominator if abs(denominator) > EPS else 0.)
    hz = float(rate / refined)
    spectrum = np.abs(np.fft.rfft(x * np.hanning(n))) ** 2
    freq = np.fft.rfftfreq(n, 1 / rate)
    band = (freq >= minimum_hz * 0.8) & (freq < rate * 0.48)
    total = float(spectrum[band].sum())
    width = max(rate / n * 1.6, hz * 0.025)
    fundamental = (np.abs(freq - hz) <= width) & band
    fundamental_ratio = float(spectrum[fundamental].sum() / max(total, EPS))
    harmonic = np.zeros(len(freq), bool)
    for multiple in range(1, int((rate * 0.48) / hz) + 1):
        harmonic |= np.abs(freq - hz * multiple) <= max(rate / n * 1.6, hz * multiple * 0.015)
    explained = float(spectrum[harmonic & band].sum() / max(total, EPS))
    flatness = float(np.exp(np.mean(np.log(spectrum[band] + EPS))) / (np.mean(spectrum[band]) + EPS))
    periodicity = _clip(1 - middle)
    confidence = periodicity * min(1., fundamental_ratio / 0.25) * min(1., explained / 0.90) * (1 - flatness)
    # A chord may look periodic at a missing subharmonic. Require evidence for
    # the actual fundamental, plus little energy unexplained by its harmonics.
    if fundamental_ratio < 0.12 or explained < 0.85 or flatness > 0.12 or confidence < 0.78:
        return None, "polyphonic_noisy_or_missing_fundamental"
    midi = float(69 + 12 * np.log2(hz / 440.))
    return {"hz": hz, "midi": midi, "score": float(confidence), "periodicity": periodicity,
            "harmonic_explained_fraction": explained, "fundamental_fraction": fundamental_ratio}, None


def _pitch_notes(audio, rate, lane):
    mono = _resample(_mono(audio), rate)
    window, hop = 1024, 160
    minimum = 35 if lane == "bass" else 65
    accepted, rejected = [], {}
    for at in range(0, max(0, len(mono) - window + 1), hop):
        pitch, reason = _pitch_frame(mono[at:at + window], ANALYSIS_RATE, minimum)
        if pitch is None:
            rejected[reason] = rejected.get(reason, 0) + 1
        else:
            center = int(round((at + window / 2) * rate / ANALYSIS_RATE))
            pitch.update({"frame": center, "note": int(round(pitch["midi"]))})
            accepted.append(pitch)
    notes, group = [], []
    max_gap = round(rate * hop / ANALYSIS_RATE * 1.6)

    def finish():
        if len(group) < 5:
            return
        midi = np.array([p["midi"] for p in group])
        median = float(np.median(midi))
        cents_spread = float(np.quantile(np.abs(midi - median), 0.90) * 100)
        score = float(np.quantile([p["score"] for p in group], 0.20))
        if cents_spread > 30 or abs(median - round(median)) * 100 > 40:
            return
        half_hop = round(rate * hop / ANALYSIS_RATE / 2)
        notes.append({"start_frame": max(0, group[0]["frame"] - half_hop),
                      "end_frame": min(len(audio), group[-1]["frame"] + half_hop),
                      "midi_note": int(round(median)), "frequency_hz": float(440 * 2 ** ((median - 69) / 12)),
                      "tuning_cents": float((median - round(median)) * 100),
                      "cents_spread": cents_spread, "score": score,
                      "method": "normalized_lag_difference_harmonic_veto/1",
                      "semantics": "supported_single_fundamental_not_proof_of_one_instrument"})

    for pitch in accepted:
        if group and (pitch["note"] != group[-1]["note"] or pitch["frame"] - group[-1]["frame"] > max_gap):
            finish()
            group = []
        group.append(pitch)
    finish()
    return {"notes": notes, "accepted_frame_count": len(accepted), "rejected_frame_counts": rejected,
            "frame_hop_source_frames": round(rate * hop / ANALYSIS_RATE),
            "method": "normalized_lag_difference_harmonic_veto/1"}


def _leakage(reference, candidate, rate, start, end):
    """Lagged projection + spectral coherence, not an assertion of no singing."""
    ref = _resample(_mono(reference[start:end]), rate)
    lane = _resample(_mono(candidate[start:end]), rate)
    sos = butter(3, [200, 3500], btype="bandpass", fs=ANALYSIS_RATE, output="sos")
    ref, lane = sosfilt(sos, ref), sosfilt(sos, lane)
    spans, measured = [], []
    size = ANALYSIS_RATE // 2
    for at in range(0, len(ref), size):
        a, b = ref[at:at + size], lane[at:at + size]
        if len(a) < ANALYSIS_RATE // 5 or _rms(a) < 0.0015:
            continue
        aa, bb = float(np.dot(a, a)), float(np.dot(b, b))
        cross = correlate(b, a, mode="full", method="fft")
        center = len(a) - 1
        lag = round(ANALYSIS_RATE * 0.020)
        around = cross[max(0, center-lag):center+lag+1]
        best = int(np.argmax(np.abs(around)))
        dot = float(abs(around[best]))
        correlation = dot / max(np.sqrt(aa * bb), EPS)
        projection = dot / max(aa, EPS)
        coherent = 0.0
        if bb > EPS:
            frequency, cxy = coherence(a, b, fs=ANALYSIS_RATE, nperseg=256, noverlap=128)
            _, reference_power = welch(a, fs=ANALYSIS_RATE, nperseg=256, noverlap=128)
            band = (frequency >= 200) & (frequency <= 3500)
            coherent = float(np.sum(np.nan_to_num(cxy[band]) * reference_power[band]) / max(float(reference_power[band].sum()), EPS))
        row = {"start_frame": int(start + round(at * rate / ANALYSIS_RATE)),
               "end_frame": int(min(end, start + round((at + len(a)) * rate / ANALYSIS_RATE))),
               "lag_frames": int(round((best-lag) * rate / ANALYSIS_RATE)),
               "projection_ratio": float(projection), "correlation": _clip(correlation),
               "weighted_coherence": _clip(coherent), "reference_band_rms": _rms(a)}
        suspected = projection >= 0.05 and correlation >= 0.25 and coherent >= 0.50
        row["suspected"] = bool(suspected)
        measured.append(row)
        if suspected:
            spans.append(row)
    return {"method": "lagged_vocal_projection_and_coherence/1",
            "status": "suspected" if spans else ("not_detected" if measured else "unassessed"),
            "semantics": "measured_veto_proxy_not_certified_absence_of_intelligible_bleed",
            "measured_window_count": len(measured), "suspect_spans": spans,
            "max_projection_ratio": max((r["projection_ratio"] for r in measured), default=None),
            "max_correlation": max((r["correlation"] for r in measured), default=None),
            "max_weighted_coherence": max((r["weighted_coherence"] for r in measured), default=None)}


def _normalize_vocals(evidence, rate, frames):
    result = {"method": None, "reference": None, "words": [], "phrases": [], "verified_absence_spans": []}
    if evidence is None:
        return result
    if not isinstance(evidence, dict) or not evidence.get("method"):
        raise ValueError("vocal evidence needs an explicit method and coordinate system")
    coordinate = evidence.get("coordinate_system")
    if coordinate not in ("source_seconds", "source_frames"):
        raise ValueError("vocal evidence coordinates must be source_seconds or source_frames")
    result["method"], result["reference"] = evidence["method"], evidence.get("reference")

    def interval(row):
        if coordinate == "source_seconds":
            start, end = row.get("start_seconds"), row.get("end_seconds")
            scale = rate
        else:
            start, end = row.get("start_frame"), row.get("end_frame")
            scale = 1
        if start is None or end is None or not np.isfinite([start, end]).all():
            return None
        start, end = round(float(start) * scale), round(float(end) * scale)
        return (start, end) if 0 <= start < end <= frames else None

    for word in evidence.get("words", []):
        span = interval(word)
        score = word.get("score", word.get("probability", 0))
        if span and isinstance(score, (int, float)) and np.isfinite(score) and score >= 0.75:
            text = str(word.get("text", word.get("word", ""))).strip()
            if text:
                result["words"].append({"start_frame": span[0], "end_frame": span[1], "text": text, "score": float(score)})
    for phrase in evidence.get("phrases", []):
        span = interval(phrase)
        if span and phrase.get("entry_supported") is True and phrase.get("exit_supported") is True:
            result["phrases"].append({"start_frame": span[0], "end_frame": span[1],
                                      "entry_supported": True, "exit_supported": True,
                                      "method": evidence["method"], "reference": evidence.get("reference")})
    # The older provider's `instrumental` field is an acoustic hypothesis. It is
    # intentionally not promoted to verified vocal absence here.
    for absence in evidence.get("verified_absence_spans", []):
        span = interval(absence)
        if span and absence.get("verification_method") and absence.get("reference"):
            result["verified_absence_spans"].append({"start_frame": span[0], "end_frame": span[1],
                "verification_method": str(absence["verification_method"]), "reference": str(absence["reference"])})
    return result


def _vocal_window(vocals, normalized, start, end, threshold):
    words = [w for w in normalized["words"] if w["start_frame"] < end and w["end_frame"] > start
             and _rms(vocals[w["start_frame"]:w["end_frame"]]) >= threshold]
    phrases = [p for p in normalized["phrases"] if p["start_frame"] >= start and p["end_frame"] <= end]
    active = _rms(vocals[start:end]) >= threshold
    verified_absent = any(a["start_frame"] <= start and a["end_frame"] >= end for a in normalized["verified_absence_spans"])
    if verified_absent and (active or words):
        status, reason = "contradictory", "claimed absence conflicts with acoustic or recognized vocal evidence"
    elif words and active:
        status, reason = "present", "recognized words agree with estimated vocal activity"
    elif verified_absent:
        status, reason = "verified_absent", "explicit independently sourced absence verification"
    else:
        status, reason = "unknown", "estimated stem energy alone cannot establish singing presence or absence"
    return {"presence": status, "reason": reason, "acoustic_active": bool(active),
            "recognized_words": words, "supported_phrases": phrases,
            "method": normalized["method"], "reference": normalized["reference"]}


def _descriptor(audio, source, rate, start, end, onsets, pitches):
    x = audio[start:end]
    level, peak = _rms(x), float(np.max(np.abs(x)))
    source_level = _rms(source[start:end])
    threshold = max(0.0008, source_level * 0.04)
    envelope = _envelope(x, max(1, round(rate * 0.02)))
    notes = [n for n in pitches["notes"] if n["start_frame"] < end and n["end_frame"] > start]
    pitched_frames = sum(max(0, min(end, n["end_frame"]) - max(start, n["start_frame"])) for n in notes)
    spectral = _spectrum(x, rate)
    local_onsets = [o for o in onsets if start <= o["frame"] < end]
    pitch_fraction = min(1., pitched_frames / (end-start))
    share = level ** 2 / max(source_level ** 2, EPS)
    lead = pitch_fraction * min(1., share / 0.30) * (float(np.mean([n["score"] for n in notes])) if notes else 0.)
    # These characterize artifacts, not perceived separator quality. Reconstruction
    # is checked independently, since stems can sum perfectly while leaking.
    differences = np.diff(x.astype(np.float64), axis=0)
    step_floor = max(0.02, level * 8)
    discontinuities = float(np.mean(np.max(np.abs(differences), axis=1) > step_floor)) if len(differences) else 0.
    clipping = float(np.mean(np.abs(x) >= 0.999))
    source_clipping = float(np.mean(np.abs(source[start:end]) >= 0.999))
    new_clipping = max(0., clipping-source_clipping)
    artifact_score = _clip(0.6 * min(1., discontinuities / 0.005) + 0.4 * min(1., new_clipping / 0.001))
    return {"rms": level, "peak": peak, "level_dbfs": _db(level),
            "crest_db": _db(peak / max(level, EPS)) if level else 0.,
            "activity_fraction": float(np.mean(envelope >= threshold)), "activity_threshold_rms": threshold,
            "energy_share_of_source": float(share), "onset_count": len(local_onsets),
            "onset_density_per_second": float(len(local_onsets) * rate / (end-start)),
            "onsets": local_onsets, "pitch_notes": notes, "pitched_fraction": float(pitch_fraction),
            "foreground_lead_score": float(lead),
            "short_term_rms_quantiles": np.quantile(envelope, [0.1, 0.5, 0.9]).tolist(),
            "artifact": {"method": "pcm_discontinuity_added_clipping/1", "score": artifact_score,
                         "discontinuity_fraction": discontinuities, "added_clipping_fraction": new_clipping,
                         "semantics": "measured_defect_penalty_not_certified_perceptual_quality"}, **spectral}


def _events(bars, samples, rate):
    events = []
    for i, bar in enumerate(bars):
        drums = bar["stems"]["drums"]
        onset = min(drums["onsets"], key=lambda o: abs(o["frame"]-bar["start_frame"]), default=None)
        on_boundary = onset is not None and abs(onset["frame"]-bar["start_frame"]) <= rate * 0.05
        if on_boundary:
            events.append({"kind": "drum_attack", "frame": onset["frame"], "score": 1.0,
                           "support": {"onset_strength": onset["strength"], "boundary_distance_frames": abs(onset["frame"]-bar["start_frame"])}})
        if i:
            previous = bars[i-1]
            old = previous["stems"]["drums"]
            ratio = drums["rms"] / max(old["rms"], 0.0008)
            if on_boundary and drums["onset_count"] >= 2 and ratio >= 2.5 and old["activity_fraction"] < 0.25:
                events.append({"kind": "drum_return", "frame": bar["start_frame"], "score": _clip((ratio-1)/4),
                               "support": {"drum_rms_ratio": float(ratio), "previous_drum_activity": old["activity_fraction"],
                                           "following_drum_onsets": drums["onset_count"]}})
            energy_ratio = bar["source"]["rms"] / max(previous["source"]["rms"], 0.0008)
            if on_boundary and drums["onset_count"] >= 2 and energy_ratio >= 1.5 and bar["stems"]["bass"]["activity_fraction"] >= 0.4:
                events.append({"kind": "drop", "frame": bar["start_frame"], "score": _clip((energy_ratio-1)/2),
                               "support": {"source_rms_ratio": float(energy_ratio), "bass_activity": bar["stems"]["bass"]["activity_fraction"],
                                           "following_drum_onsets": drums["onset_count"]}})
        if i >= 2:
            window = bars[i-2:i+1]
            energy = np.array([b["source"]["rms"] for b in window])
            activity = np.array([b["stems"]["drums"]["onset_density_per_second"] for b in window])
            brightness = np.array([b["source"]["centroid_hz"] for b in window])
            rising = energy[-1] >= energy[0]*1.8 and np.all(np.diff(energy) > 0)
            second_cue = activity[-1] >= max(activity[0]*1.4, activity[0]+0.5) or brightness[-1] >= max(200,brightness[0]*1.3)
            if rising and second_cue:
                events.append({"kind": "build", "frame": window[0]["start_frame"], "end_frame": bar["end_frame"],
                               "score": _clip((energy[-1]/max(energy[0],EPS)-1)/2),
                               "support": {"rms_ratio": float(energy[-1]/max(energy[0],EPS)),
                                           "onset_density_ratio": float(activity[-1]/max(activity[0],0.01)),
                                           "brightness_ratio": float(brightness[-1]/max(brightness[0],1.))}})
    hop = max(1, round(rate * 0.02))
    envelope = _envelope(samples, hop)
    threshold = max(0.0005, float(np.max(envelope)) * 0.01)
    active = np.flatnonzero(envelope > threshold)
    if len(active):
        first, last = int(active[0]), int(active[-1])
        events.append({"kind": "audible_start", "frame": first*hop, "score": 1.,
                       "support": {"rms_threshold": threshold}, "semantics": "acoustic_attack_not_verified_song_intro"})
        quiet_seconds = (len(samples)-(last+1)*hop)/rate
        tail_start = max(0,last-round(1.5*rate/hop))
        tail = envelope[tail_start:last+1]
        if len(tail) >= 10 and quiet_seconds >= 0.40:
            peak_at = int(np.argmax(tail))
            decay = tail[peak_at:]
            if len(decay) >= round(0.2*rate/hop) and decay[0] >= decay[-1]*4:
                correlation = float(np.corrcoef(np.arange(len(decay)),np.log(np.maximum(decay,EPS)))[0,1])
                if np.isfinite(correlation) and correlation < -0.7:
                    events.append({"kind": "natural_ending", "frame": min(len(samples),(last+1)*hop),
                                   "start_frame": (tail_start+peak_at)*hop, "score": float(-correlation),
                                   "support": {"decay_log_rms_correlation": correlation, "quiet_tail_seconds": float(quiet_seconds)},
                                   "semantics": "verified_acoustic_decay_requires_independent_vocal_phrase_check"})
    return events


def _loop_measure(audio, rate, start, end, chroma_start, chroma_end):
    x = audio[start:end].astype(np.float64)
    level = _rms(x)
    if len(x) < 4 or level < 0.0008:
        return {"signal_safe": False, "reason": "insufficient_audible_material", "score": None}
    slope_start, slope_end = x[1]-x[0], x[-1]-x[-2]
    prediction_error = float(np.max(np.abs(x[0]-x[-1]-(slope_start+slope_end)/2))) / max(level,0.001)
    slope_error = float(np.max(np.abs(slope_start-slope_end))) / max(level,0.001)
    a,b = np.asarray(chroma_start),np.asarray(chroma_end)
    tonal_distance = None if np.linalg.norm(a)*np.linalg.norm(b)<EPS else float(1-np.dot(a,b)/(np.linalg.norm(a)*np.linalg.norm(b)))
    safe = prediction_error <= 0.20 and slope_error <= 0.35 and (tonal_distance is None or tonal_distance <= 0.25)
    return {"signal_safe": bool(safe), "score": _clip(1-max(prediction_error/0.2,slope_error/0.35,0 if tonal_distance is None else tonal_distance/0.25)),
            "continuation_error_ratio": prediction_error, "slope_error_ratio": slope_error,
            "tonal_cycle_distance": tonal_distance, "recommended_declick_frames": max(1,round(rate*0.005)),
            "semantics": "PCM_seam_and_tonal_boundary_only_not_lyric_or_phrase_completeness"}


def _aggregate(bars, rate, normalized):
    start,end=bars[0]["start_frame"],bars[-1]["end_frame"]
    durations=np.array([b["end_frame"]-b["start_frame"] for b in bars],float)
    weights=durations/durations.sum()
    result={"start_frame":start,"end_frame":end,"sample_rate_hz":rate,"bar_count":len(bars),
            "stems":{},"source":{},"method":METHOD,"score_semantics":SCORE_SEMANTICS}
    for name in ["source",*LANES]:
        rows=[b["source"] if name=="source" else b["stems"][name] for b in bars]
        row={}
        for key in ("activity_fraction","energy_share_of_source","onset_density_per_second","pitched_fraction",
                    "foreground_lead_score","tonalness","flatness","centroid_hz","low_fraction","mid_fraction","high_fraction"):
            row[key]=float(np.dot(weights,[r[key] for r in rows]))
        row["rms"]=float(np.sqrt(np.dot(weights,[r["rms"]**2 for r in rows])))
        row["peak"]=max(r["peak"] for r in rows)
        row["crest_db"]=_db(row["peak"]/max(row["rms"],EPS)) if row["rms"] else 0.
        chroma=sum((r["rms"]**2*np.asarray(r["chroma"]) for r in rows),np.zeros(12))
        row["chroma"]=(chroma/max(float(chroma.sum()),EPS)).tolist()
        row["onsets"]=[o for r in rows for o in r["onsets"]]
        row["onset_count"]=len(row["onsets"])
        unique={(n["start_frame"],n["end_frame"],n["midi_note"]):n for r in rows for n in r["pitch_notes"]}
        row["pitch_notes"]=list(unique.values())
        row["artifact_score"]=max(r["artifact"]["score"] for r in rows)
        if name=="source":result["source"]=row
        else:result["stems"][name]=row
    result["density"]={"active_stem_count":sum(s["activity_fraction"]>=0.25 for s in result["stems"].values()),
                       "sustained_active_stem_count":sum(s["activity_fraction"]>=0.70 for s in result["stems"].values())}
    result["reconstruction_error_ratio"]=max(b["reconstruction"]["relative_rms_error"] for b in bars)
    result["vocal_leakage"]={lane:{"status":"suspected" if any(b["vocal_leakage"][lane]["status"]=="suspected" for b in bars)
        else ("not_detected" if any(b["vocal_leakage"][lane]["status"]=="not_detected" for b in bars) else "unassessed"),
        "max_projection_ratio":max((b["vocal_leakage"][lane]["max_projection_ratio"] for b in bars if b["vocal_leakage"][lane]["max_projection_ratio"] is not None),default=None)} for lane in ("drums","bass","other")}
    result["vocal"]={"presence":"present" if any(b["vocal"]["presence"]=="present" for b in bars)
        else ("verified_absent" if all(b["vocal"]["presence"]=="verified_absent" for b in bars) else "unknown"),
        "supported_phrases":[p for p in normalized["phrases"] if start<=p["start_frame"] and p["end_frame"]<=end],
        "recognized_words":list({(w["start_frame"],w["end_frame"],w["text"]):w for b in bars for w in b["vocal"]["recognized_words"]}.values())}
    return result


def compact_signal_evidence(record):
    """Store overlapping windows by reference without losing measured evidence.

    Canonical bars, notes, events and PCM seam measurements remain intact. This
    also accepts records from storage version 1, so a caller that has verified
    their original provenance can migrate them without rerunning separation.
    The function returns a new record and does not change its input.
    """
    result = dict(record)
    summaries = []
    bars = record["bars"]
    for segment in record["segments"]:
        if "bar_indices" in segment:
            summaries.append(dict(segment))
            continue
        indices = [i for i, bar in enumerate(bars)
                   if segment["start_frame"] <= bar["start_frame"]
                   and bar["end_frame"] <= segment["end_frame"]]
        summaries.append({
            "start_frame": segment["start_frame"], "end_frame": segment["end_frame"],
            "bar_count": segment["bar_count"], "bar_indices": indices,
            "roles": segment["roles"], "density": segment["density"],
            "source_rms": segment["source"]["rms"], "source_peak": segment["source"]["peak"],
            "reconstruction_error_ratio": segment["reconstruction_error_ratio"],
            "vocal_presence": segment["vocal"]["presence"],
            "event_indices": [i for i, event in enumerate(record["events"])
                              if segment["start_frame"] <= event["frame"] < segment["end_frame"]],
        })
    result["segments"] = summaries
    result["storage_version"] = 2
    result["storage_semantics"] = "canonical_bars_and_pitch_analysis; overlapping_segments_reference_bars_and_events"
    return result


def materialize_window(record, segment):
    """Reconstruct a full comparison window from canonical measured bars.

    The accepted span must consist of complete contiguous measured bars. No
    interpolation or uncovered-audio assumptions are introduced by hydration.
    """
    if "stems" in segment:  # Storage version 1 remains readable.
        return segment
    indices = segment.get("bar_indices")
    if not isinstance(indices, list) or not indices or any(type(i) is not int for i in indices):
        raise ValueError("window requires nonempty canonical bar indices")
    if indices != list(range(indices[0], indices[-1] + 1)) or indices[0] < 0 or indices[-1] >= len(record["bars"]):
        raise ValueError("window bar references must be contiguous and in bounds")
    selected = [record["bars"][i] for i in indices]
    if (selected[0]["start_frame"] != segment["start_frame"]
            or selected[-1]["end_frame"] != segment["end_frame"]
            or any(a["end_frame"] != b["start_frame"] for a, b in zip(selected, selected[1:]))):
        raise ValueError("window references do not cover its exact source span")
    result = _aggregate(selected, record["sample_rate_hz"], record["vocal_evidence"])
    result["roles"] = sorted(set(role for bar in selected for role in bar["roles"]))
    result["events"] = [e for e in record["events"] if result["start_frame"] <= e["frame"] < result["end_frame"]]
    return result


def analyze_tactic_signals(samples, stems, rate, beat_frames, downbeat_frames, vocal_evidence=None):
    """Return JSON-safe track evidence in the supplied PCM's original timebase."""
    if not isinstance(rate,(int,np.integer)) or not 4000 <= rate <= 192000:
        raise ValueError("sample rate must be an integer between 4000 and 192000")
    rate=int(rate)
    samples=_pcm(samples,"source")
    if set(stems)!=set(LANES):
        raise ValueError("all four named separated stems are required")
    stems={name:_pcm(stems[name],name) for name in LANES}
    if any(part.shape!=samples.shape for part in stems.values()):
        raise ValueError("stems must exactly match source frame and channel counts")
    def positions(values,name,allow_end):
        raw=np.asarray(values)
        if raw.ndim!=1 or (len(raw) and (not np.issubdtype(raw.dtype,np.number) or not np.isfinite(raw).all()
                or np.any(raw!=np.round(raw)) or np.any(np.diff(raw)<=0) or raw[0]<0 or raw[-1]>(len(samples) if allow_end else len(samples)-1))):
            raise ValueError(f"{name} must be ordered, unique source frames")
        return raw.astype(np.int64)
    beats=positions(beat_frames,"beats",False)
    downbeats=positions(downbeat_frames,"downbeats",True)
    normalized=_normalize_vocals(vocal_evidence,rate,len(samples))
    pitch={name:_pitch_notes(part,rate,name) for name,part in stems.items()}
    onsets={name:_onsets(part,rate,pulse_bands=(name=="drums")) for name,part in stems.items()}
    source_onsets=_onsets(samples,rate)
    source_pitch={"notes":[]}
    def describe_window(start,end,index=None):
        start,end=int(start),int(end)
        source=_descriptor(samples,samples,rate,start,end,source_onsets,source_pitch)
        lanes={name:_descriptor(part,samples,rate,start,end,onsets[name],pitch[name]) for name,part in stems.items()}
        reconstructed=sum((part[start:end].astype(np.float64) for part in stems.values()),np.zeros_like(samples[start:end],dtype=np.float64))
        residual=samples[start:end]-reconstructed
        reconstruction=_rms(residual)/max(source["rms"],EPS)
        leakage={name:_leakage(stems["vocals"],stems[name],rate,start,end) for name in ("drums","bass","other")}
        local_beats=beats[(beats>=start)&(beats<end)]
        pulse_seconds=float(np.median(np.diff(local_beats))/rate) if len(local_beats)>=2 else None
        roles=[]
        if source["rms"] < 0.0008:roles.append("rest")
        if lanes["drums"]["activity_fraction"]<0.20 and source["rms"]>=0.002:roles.append("low_percussion_bed")
        if lanes["other"]["activity_fraction"]>=0.70 and lanes["other"]["onset_density_per_second"]<1.:roles.append("sustained_texture")
        if lanes["drums"]["onset_count"]>=2 and lanes["drums"]["rms"]>=0.003:roles.append("drum_groove")
        if not roles:roles.append("mixed")
        return {"index":index,"start_frame":start,"end_frame":end,"sample_rate_hz":rate,
            "source":source,"stems":lanes,"roles":roles,
            "pulse":{"beat_count":len(local_beats) if len(beats) else None,"median_bpm":60/pulse_seconds if pulse_seconds else None,
                     "interval_cv":float(np.std(np.diff(local_beats))/np.mean(np.diff(local_beats))) if len(local_beats)>=3 else None},
            "density":{"active_stem_count":sum(l["activity_fraction"]>=0.25 for l in lanes.values()),
                       "sustained_active_stem_count":sum(l["activity_fraction"]>=0.70 for l in lanes.values())},
            "reconstruction":{"relative_rms_error":float(reconstruction),"residual_rms":_rms(residual),
                "stem_to_source_energy_ratio":float(sum(l["rms"]**2 for l in lanes.values())/max(source["rms"]**2,EPS)),
                "score":_clip(reconstruction/0.20),"semantics":"measured_reconstruction_error_not_separation_correctness"},
            "vocal_leakage":leakage,"vocal":_vocal_window(stems["vocals"],normalized,start,end,max(0.0015,source["rms"]*0.04))}
    bars=[]
    for index,(start,end) in enumerate(zip(downbeats[:-1],downbeats[1:])):
        if end-start < round(rate*0.1):
            raise ValueError("bar span is shorter than 100 ms")
        bars.append(describe_window(start,end,index))
    # Cover source intro/tail and truly unmetered recordings without fabricating
    # bars. These scalar acoustic windows are usable by structural tactics only.
    gaps=[]
    cursor=0
    for bar in bars:
        if cursor<bar["start_frame"]:gaps.append((cursor,bar["start_frame"]))
        cursor=bar["end_frame"]
    if cursor<len(samples):gaps.append((cursor,len(samples)))
    unmetered=[]
    for start,end in gaps:
        for left in range(start,end,rate):
            row=describe_window(left,min(left+rate,end))
            row["timing_basis"]="unmetered_source_window"
            row["pulse"]={"beat_count":None,"median_bpm":None,"interval_cv":None}
            unmetered.append(row)
    events=_events(bars,samples,rate)
    segments,loops,motifs=[],[],[]
    for start in range(len(bars)):
        for count in range(1,min(4,len(bars)-start)+1):
            selected=bars[start:start+count]
            segment=_aggregate(selected,rate,normalized)
            segment["events"]=[e for e in events if segment["start_frame"]<=e["frame"]<segment["end_frame"]]
            segment["roles"]=sorted(set(role for b in selected for role in b["roles"]))
            segments.append(segment)
            lane_loops={name:_loop_measure(stems[name],rate,segment["start_frame"],segment["end_frame"],
                selected[0]["stems"][name]["chroma"],selected[-1]["stems"][name]["chroma"]) for name in LANES}
            loops.append({"start_frame":segment["start_frame"],"end_frame":segment["end_frame"],"bar_count":count,
                          "lanes":lane_loops,"method":"PCM_continuation_and_chroma_cycle/1"})
            if count in (2,4):
                for name in ("bass","other","vocals"):
                    notes=[n for n in segment["stems"][name]["pitch_notes"] if n["start_frame"]>=segment["start_frame"] and n["end_frame"]<=segment["end_frame"]]
                    if 2<=len(notes)<=16 and min(n["score"] for n in notes)>=0.85:
                        intervals=[n["midi_note"]-notes[0]["midi_note"] for n in notes]
                        duration=segment["end_frame"]-segment["start_frame"]
                        rhythm=[round((n["start_frame"]-notes[0]["start_frame"])/duration,3) for n in notes]
                        fingerprint=sha256(json.dumps([intervals,rhythm],separators=(",",":")).encode()).hexdigest()
                        motifs.append({"start_frame":notes[0]["start_frame"],"end_frame":notes[-1]["end_frame"],"lane":name,
                                       "notes":notes,"intervals_semitones":intervals,"relative_onsets":rhythm,
                                       "fingerprint":fingerprint,"score":float(min(n["score"] for n in notes)),
                                       "semantics":"supported_pitch_sequence_not_lyrical_or_cultural_recognition"})
    result={"method":METHOD,"method_version":1,"score_semantics":SCORE_SEMANTICS,"sample_rate_hz":rate,
        "source_start_frame":0,"source_end_frame":len(samples),"channel_count":samples.shape[1],
        "beat_frames":beats.tolist(),"downbeat_frames":downbeats.tolist(),"bars":bars,"unmetered_windows":unmetered,"segments":segments,
        "events":events,"loops":loops,"motifs":motifs,"vocal_evidence":normalized,
        "pitch_analysis":pitch,"provenance_scope":"caller_must_bind_audio_hash_and_separation_model_version",
        "limitations":["acoustic activity does not establish vocal absence",
            "pitch notes cover supported single-fundamental material; polyphonic or ambiguous frames are rejected",
            "event labels are measured arrangement candidates, not guaranteed verse/chorus semantics",
            "artifact and leakage scores are uncalibrated veto evidence; low scores do not certify clean separation"]}
    # Assert at the boundary rather than allowing NaN/Infinity into a sidecar.
    result = compact_signal_evidence(result)
    json.dumps(result,allow_nan=False)
    return result


def analyze_track(samples, stems, sample_rate, bar_frames, vocal_evidence=None, source_start_frame=0, beat_frames=None):
    """Full-track orchestrator adapter. Input frame lists are local to this PCM."""
    if not isinstance(source_start_frame,(int,np.integer)) or source_start_frame<0:
        raise ValueError("source offset must be a nonnegative integer")
    result=analyze_tactic_signals(samples,stems,sample_rate,[] if beat_frames is None else beat_frames,bar_frames,vocal_evidence)
    if source_start_frame:
        def offset(value):
            if isinstance(value,dict):
                for key,item in value.items():
                    if key in ("frame","start_frame","end_frame","source_start_frame","source_end_frame"):
                        value[key]=int(item)+int(source_start_frame)
                    elif key in ("beat_frames","downbeat_frames"):
                        value[key]=[int(v)+int(source_start_frame) for v in item]
                    else:offset(item)
            elif isinstance(value,list):
                for item in value:offset(item)
        # A JSON round trip removes shared note/event objects before translating.
        result=json.loads(json.dumps(result,allow_nan=False))
        offset(result)
    return result


def compare_tactic_signals(a_window,b_window,selected_lanes):
    """Compare actual selected layers; no whole-track key or RMS shortcuts.

    selected_lanes maps A/B (or outgoing/incoming) to lane-name lists. Returns
    numerical collision/headroom evidence and supported/rejected/unknown status.
    It does not replace beat alignment, phrase scheduling or render validation.
    """
    choices=[selected_lanes.get("A",selected_lanes.get("outgoing")),selected_lanes.get("B",selected_lanes.get("incoming"))]
    if any(not isinstance(names,(list,tuple)) or not names or len(set(names))!=len(names) or any(n not in LANES for n in names) for names in choices):
        raise ValueError("selected_lanes needs nonempty unique A/B stem lane lists")
    windows=[a_window,b_window]
    chosen=[[window["stems"][name] for name in names] for window,names in zip(windows,choices)]
    selected_rms=[float(np.sqrt(sum(l["rms"]**2 for l in lanes))) for lanes in chosen]
    peak_bound=float(sum(l["peak"] for lanes in chosen for l in lanes))
    reasons=[]
    def active(side,name):
        if name not in choices[side]:
            return False
        lane=windows[side]["stems"][name]
        return lane["rms"]>=0.002 and (lane["activity_fraction"]>=0.20 or (name=="drums" and lane["onset_count"]>=2))
    drum_collision=all(active(i,"drums") and windows[i]["stems"]["drums"]["onset_count"]>=2 for i in (0,1))
    bass_collision=all(active(i,"bass") for i in (0,1))
    lead_scores=[max((l["foreground_lead_score"] for name,l in zip(names,lanes) if name in ("other","vocals")),default=0.) for names,lanes in zip(choices,chosen)]
    lead_collision=min(lead_scores)>=0.6
    vocal_states=[w.get("vocal",{}).get("presence","unknown") for w in windows]
    vocal_collision=all("vocals" in choices[i] and (vocal_states[i]=="present" or active(i,"vocals")) for i in (0,1))
    leakage=[]
    for side in (0,1):
        for name in choices[side]:
            if name!="vocals" and windows[side].get("vocal_leakage",{}).get(name,{}).get("status")=="suspected":
                leakage.append({"side":"A" if side==0 else "B","lane":name})
    for condition,reason in [(drum_collision,"two active drum foundations"),(bass_collision,"two active bass owners"),
                             (lead_collision,"two supported foreground leads"),(vocal_collision,"simultaneous vocal lanes"),(bool(leakage),"suspected vocal leakage in retained lanes")]:
        if condition:reasons.append(reason)
    chromas=[]
    for names,lanes in zip(choices,chosen):
        pitched=[lane for name,lane in zip(names,lanes) if name!="drums" or lane["pitched_fraction"]>=0.5]
        chroma=sum((np.asarray(l["chroma"])*l["rms"]**2 for l in pitched if l["tonalness"]>=0.4),np.zeros(12))
        chromas.append(chroma)
    denominator=float(np.linalg.norm(chromas[0])*np.linalg.norm(chromas[1]))
    tonal_distance=None if denominator<EPS else _clip(1-float(np.dot(chromas[0],chromas[1]))/denominator)
    # Compare observed simultaneous notes under normalized window phase. Shared
    # key alone cannot resolve conflicting foreground lines (checked above).
    dissonance=[]
    for left in chosen[0]:
        for right in chosen[1]:
            for a in left["pitch_notes"]:
                for b in right["pitch_notes"]:
                    phases=[]
                    for note,w in ((a,a_window),(b,b_window)):
                        length=w["end_frame"]-w["start_frame"]
                        phases.append((max(0.,(note["start_frame"]-w["start_frame"])/length),min(1.,(note["end_frame"]-w["start_frame"])/length)))
                    overlap=min(phases[0][1],phases[1][1])-max(phases[0][0],phases[1][0])
                    if overlap>0:
                        interval=abs(a["midi_note"]-b["midi_note"])%12
                        dissonance.append((overlap,0. if interval in (0,3,4,5,7,8,9) else 1.))
    interval_conflict=None if not dissonance else float(sum(w*d for w,d in dissonance)/sum(w for w,_ in dissonance))
    if interval_conflict is not None and interval_conflict>=0.5:
        reasons.append("observed pitched layers sustain conflicting intervals")
    unresolved_vocals=any("vocals" in choices[i] and vocal_states[i] in ("unknown","contradictory") for i in (0,1))
    density=sum(l["activity_fraction"]>=0.35 or (name=="drums" and l["rms"]>=0.01 and l["onset_density_per_second"]>=1.)
                for names,lanes in zip(choices,chosen) for name,l in zip(names,lanes))
    return {"method":"selected_stem_pair_evidence/1","score_semantics":SCORE_SEMANTICS,
        "selected_lanes":{"A":list(choices[0]),"B":list(choices[1])},
        "status":"rejected" if reasons else ("unknown" if unresolved_vocals else "supported"),"reasons":reasons,
        "drum_collision":bool(drum_collision),"bass_collision":bool(bass_collision),"lead_collision":bool(lead_collision),
        "vocal_collision":bool(vocal_collision),"suspected_vocal_leakage":leakage,
        "tonal_distance":tonal_distance,"pitch_interval_conflict":interval_conflict,
        "foreground_lead_scores":lead_scores,"selected_rms_uncorrelated_estimate":selected_rms,
        "summed_peak_upper_bound":peak_bound,"headroom_to_minus_1_dbfs_bound_db":_db(10**(-1/20)/max(peak_bound,EPS)),
        "active_selected_layer_count":density,"dense_selected_composite":bool(density>=3 and min(selected_rms)>=0.025),
        "vocal_status":{"A":vocal_states[0],"B":vocal_states[1]},
        "required_followup":["validate beat/phrase alignment","schedule independent vocal gates and wet tails","measure actual rendered headroom"]}
