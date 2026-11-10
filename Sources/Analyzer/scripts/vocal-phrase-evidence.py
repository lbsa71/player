"""Local, conservative lyric boundary evidence for prepared stem candidates.

Whisper word alignment is one source of evidence, not ground truth for singing.
Require confident text, acoustic agreement and a quiet completed sentence edge.
Unrecognized singing stays unknown. No audio is sent to a remote service.
"""
import hashlib
from pathlib import Path

import numpy as np
from scipy.signal import resample_poly
from math import gcd

METHOD = "whisper-words-acoustic-phrase/2"
WHISPER_VERSION = "20250625"


def _rms(audio):
    return float(np.sqrt(np.mean(np.square(audio, dtype=np.float64)))) if audio.size else 0.0


def phrase_evidence(transcription, vocals, sample_rate, output_time):
    """Build source-aligned phrase candidates, rejecting unsupported language.

    Returned scores are raw model/engineering evidence, not probabilities of
    correct phrase completion. The separator independently verifies every gate.
    """
    vocals = np.asarray(vocals, dtype=np.float32)
    output_time = np.asarray(output_time, dtype=float)
    if (vocals.ndim != 2 or len(vocals) != len(output_time) or len(vocals) < 2
            or sample_rate <= 0 or not np.isfinite(vocals).all()
            or not np.isfinite(output_time).all() or np.any(np.diff(output_time) <= 0)):
        raise ValueError("invalid vocal evidence timebase")
    duration = len(vocals) / sample_rate
    mono_power = np.mean(np.square(vocals, dtype=np.float64), axis=1)
    integral = np.concatenate(([0.0], np.cumsum(mono_power)))

    def rms_between(start, end):
        left, right = max(0, round(start * sample_rate)), min(len(vocals), round(end * sample_rate))
        if left >= right:
            return None
        return float(np.sqrt((integral[right] - integral[left]) / (right-left)))

    def mapped(seconds):
        position=float(np.clip(seconds*sample_rate,0,len(output_time)-1))
        left=int(position);right=min(left+1,len(output_time)-1)
        return float(output_time[left]+(output_time[right]-output_time[left])*(position-left))

    def source_seconds(output_seconds):
        right=int(np.clip(np.searchsorted(output_time,output_seconds),1,len(output_time)-1))
        left=right-1
        fraction=(output_seconds-output_time[left])/(output_time[right]-output_time[left])
        return (left+fraction)/sample_rate

    threshold = float(np.clip(_rms(vocals) * 0.12, 0.0015, 0.006))
    words, rejected = [], []
    for segment in transcription.get("segments", []):
        valid_segment = (float(segment.get("avg_logprob", -999)) >= -0.65
                         and float(segment.get("no_speech_prob", 1)) <= 0.35
                         and float(segment.get("compression_ratio", 999)) <= 2.2
                         and float(segment.get("temperature", 1)) == 0)
        for word in segment.get("words", []):
            start, end = float(word.get("start", -1)), float(word.get("end", -1))
            confidence = float(word.get("probability", 0))
            text = str(word.get("word", "")).strip()
            valid = (valid_segment and 0 <= start < end <= duration and end-start <= 3.0
                     and confidence >= 0.75 and any(c.isalpha() for c in text)
                     and (rms_between(start, end) or 0) >= threshold
                     and (not words or start >= words[-1]["end"] - 0.04))
            if not valid:
                rejected.append({"start": start, "end": end, "reason": "uncertain_or_acoustically_unsupported_word"})
                continue
            words.append({"start": start, "end": end, "word": text, "score": confidence})

    # Neither a silent ASR result nor its no_speech score proves instrumental.
    # Affirm absence only when the entire estimated vocal signal is negligible.
    instrumental = (_rms(vocals) < 0.0005 and float(np.max(np.abs(vocals))) < 0.003
                    and not words and not rejected)
    starts, ends, phrases = [], [], []
    phrase_start = None
    min_score = 1.0
    for i, word in enumerate(words):
        start, end = word["start"], word["end"]
        prior_end = words[i-1]["end"] if i else 0.0
        quiet_before = rms_between(max(prior_end, start-0.45), start-0.04)
        supported_start = (start >= 0.50 and start-prior_end >= 0.45
                           and quiet_before is not None and quiet_before < threshold)
        if phrase_start is None and supported_start:
            # A cropped preceding word or rejected ASR region invalidates entry.
            if not any(r["start"] < start and r["end"] > start-0.5 for r in rejected):
                phrase_start = start
                min_score = word["score"]
                starts.append(mapped(start))
        min_score = min(min_score, word["score"])
        next_start = words[i+1]["start"] if i+1 < len(words) else duration
        quiet_after = rms_between(end+0.06, min(end+0.5, next_start-0.04))
        terminal = word["word"].rstrip('\"\'”’)]}').endswith((".", "!", "?", "。", "！", "？"))
        supported_end = (terminal and next_start-end >= 0.50 and duration-end >= 0.60
                         and quiet_after is not None and quiet_after < threshold
                         and not any(r["start"] < end+0.5 and r["end"] > end-0.5 for r in rejected))
        if supported_end:
            ends.append(mapped(end))
            if phrase_start is not None:
                phrases.append({"start_seconds": mapped(phrase_start), "end_seconds": mapped(end),
                                "confidence_margin": float(min_score-0.5),
                                "entry_supported": True, "exit_supported": True})
            phrase_start = None
            min_score = 1.0

    # Check whether transcript words account for sounding material. A confidently
    # recognized final word must not authorize discarding unrecognized singing.
    step = max(1, round(sample_rate * 0.02))
    active = np.array([_rms(vocals[i:i+step]) >= threshold for i in range(0, len(vocals), step)])
    covered = np.zeros(len(active), dtype=bool)
    for word in words:
        lo = max(0, int((word["start"]-0.15)*sample_rate/step))
        hi = min(len(active), int(np.ceil((word["end"]+0.2)*sample_rate/step)))
        covered[lo:hi] = True
    coverage = float(np.sum(active & covered) / max(1, np.sum(active)))
    # A distant unrecognized chorus cannot invalidate a completely supported
    # local phrase. Its activity remains unknown to the candidate adapter.
    # Conversely, a known final word must not cover unknown singing in the
    # same phrase. Keep only whole phrases with local acoustic word coverage.
    accepted = []
    for phrase in phrases:
        lo_time = source_seconds(phrase["start_seconds"])
        hi_time = source_seconds(phrase["end_seconds"])
        lo=max(0,int(lo_time*sample_rate/step)); hi=min(len(active),int(np.ceil(hi_time*sample_rate/step)))
        local=float(np.sum(active[lo:hi] & covered[lo:hi])/max(1,np.sum(active[lo:hi])))
        if local >= 0.85:
            accepted.append({**phrase,"acoustic_word_coverage":local})
    phrases=accepted
    starts=[p["start_seconds"] for p in phrases]
    ends=[p["end_seconds"] for p in phrases]
    # Affirmative, local near-zero separated-vocal measurements. A blank ASR
    # result alone is never used as absence. Both RMS and peak must stay below
    # the same conservative limits used for whole-track instrumental evidence.
    quiet=np.array([_rms(vocals[i:i+step])<0.0005 and float(np.max(np.abs(vocals[i:i+step])))<0.003
                    for i in range(0,len(vocals),step)])
    quiet_spans=[]
    begin=None
    for i,is_quiet in enumerate(np.append(quiet,False)):
        if is_quiet and begin is None: begin=i
        if not is_quiet and begin is not None:
            left,right=begin*step,min(i*step,len(vocals))
            if right-left>=round(sample_rate*0.30):
                quiet_spans.append({"start_seconds":mapped(left/sample_rate),
                                    "end_seconds":mapped((right-1)/sample_rate)+(output_time[-1]-output_time[-2]),
                                    "verification_method":"separated_vocal_rms_and_peak_negligible/1",
                                    "reference":METHOD})
            begin=None
    return {"phrase_starts": starts, "phrase_ends": ends, "instrumental": bool(instrumental),
            "phrases": phrases, "words": [{**w, "start_seconds": mapped(w["start"]),
                                              "end_seconds": mapped(w["end"])} for w in words],
            "verified_absence_spans":quiet_spans,
            "rejected_word_count": len(rejected), "acoustic_word_coverage": coverage,
            "semantics": "uncalibrated_ASR_plus_acoustic_evidence_not_guaranteed_sung_phrase_completion"}


def build_phrase_evidence(loaded, bars, config):
    """Provider entry point called with already separated local vocal arrays."""
    import importlib.metadata
    import whisper
    if importlib.metadata.version("openai-whisper") != WHISPER_VERSION:
        raise ValueError(f"phrase evidence requires openai-whisper=={WHISPER_VERSION}")
    model_path = Path(config["model_path"]).resolve(strict=True)
    expected = config["model_sha256"]
    digest = hashlib.sha256()
    with model_path.open("rb") as source:
        for block in iter(lambda: source.read(1024*1024), b""):
            digest.update(block)
    if digest.hexdigest() != expected:
        raise ValueError("vocal phrase model checksum mismatch")
    model = whisper.load_model(str(model_path), device="cpu")
    evidence = {"method": METHOD, "reference": f"sha256:{expected}",
                "provider_version": WHISPER_VERSION, "window_seconds": [float(bars[0]), float(bars[-1])]}
    for clip in loaded:
        rate = clip["sample_rate"]
        mono = clip["vocals"].mean(axis=1)
        divisor = gcd(int(rate), 16000)
        audio = resample_poly(mono, 16000//divisor, int(rate)//divisor).astype(np.float32)
        result = model.transcribe(audio, word_timestamps=True, task="transcribe", temperature=0.0,
                                  condition_on_previous_text=False, fp16=False, verbose=None,
                                  hallucination_silence_threshold=0.5)
        evidence[clip["role"]] = phrase_evidence(result, clip["vocals"], rate, clip["output_time"])
    return evidence
