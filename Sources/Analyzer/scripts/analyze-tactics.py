"""Local full-track tactic analysis. Results are staged for Rust publication.

The caller owns immutable record publication and the one-sidecar-per-WAV rule.
This process writes only explicitly requested temporary audio/JSON paths.
"""
import hashlib
import importlib.metadata
import importlib.util
import json
import os
from pathlib import Path
import sys

import numpy as np
import soundfile as sf

METHOD = "full-track-stems-phrases-signals/1"
DEMUCS_VERSION = "4.0.1"
LANES = ("drums", "bass", "other", "vocals")
STEM_PIPELINE_VERSION = "source-aligned-stereo/1"
VOCAL_PIPELINE_VERSION = "source-seconds-full-track/1"


def sha256_file(path):
    digest = hashlib.sha256()
    with Path(path).open("rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def module_at(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    if spec is None or spec.loader is None:
        raise ValueError(f"cannot load local {name} implementation")
    module = importlib.util.module_from_spec(spec)
    # Execute the exact hashed source; timestamp/size-based .pyc reuse can load
    # stale code after a same-second script refresh in a recovery directory.
    exec(compile(Path(path).read_bytes(), str(path), "exec"), module.__dict__)
    return module


def pinned_separator(config):
    if importlib.metadata.version("demucs") != DEMUCS_VERSION:
        raise ValueError(f"tactic analysis requires demucs=={DEMUCS_VERSION}")
    path = Path(config["model_path"]).resolve(strict=True)
    if sha256_file(path) != config["model_sha256"]:
        raise ValueError("separator model checksum mismatch")
    from demucs.pretrained import get_model
    import torch
    torch.set_num_threads(4)
    # Explicit local repository prevents a missing pin from triggering downloads.
    return get_model(config.get("signature", "955717e8"), repo=path.parent).eval()


def atomic_json(path, value):
    encoded = json.dumps(value, allow_nan=False, separators=(",", ":"))
    output = Path(path)
    temporary = output.with_name(output.name + ".staging")
    with temporary.open("w") as destination:
        destination.write(encoded)
        destination.flush()
        os.fsync(destination.fileno())
    os.replace(temporary, output)


def load_stems(assets, frames, rate):
    if set(assets) != set(LANES):
        raise ValueError("cached inputs require four stem assets")
    stems = {}
    for lane in LANES:
        asset = assets[lane]
        if sha256_file(asset["path"]) != asset["sha256"]:
            raise ValueError(f"cached {lane} stem checksum mismatch")
        samples, actual_rate = sf.read(asset["path"], dtype="float32", always_2d=True)
        if actual_rate != rate or samples.shape != (frames, 2) or not np.isfinite(samples).all():
            raise ValueError(f"invalid cached {lane} stem timebase or samples")
        stems[lane] = samples
    return stems


def analyze(request):
    source = Path(request["source"]).resolve(strict=True)
    if sha256_file(source) != request["source_file_sha256"]:
        raise ValueError("source changed before tactic analysis")
    info = sf.info(source)
    if (info.frames != request["source_frames"] or info.samplerate != request["sample_rate_hz"]
            or info.channels not in (1, 2)):
        raise ValueError("source format no longer matches requested analysis")
    beats = np.asarray(request["beat_frames"], dtype=np.int64)
    bars = np.asarray(request["bar_frames"], dtype=np.int64)
    for events in (beats, bars):
        if (np.any(events < 0) or np.any(events >= info.frames)
                or np.any(np.diff(events) <= 0)):
            raise ValueError("source event frames must be increasing and inside the recording")
    if not set(bars).issubset(set(beats)):
        raise ValueError("source downbeats must reference actual beat events")
    if set(request["stems"]) != set(LANES):
        raise ValueError("all four explicit stem output paths are required")
    if source in [Path(path).resolve() for path in request["stems"].values()]:
        raise ValueError("stem output must not replace the source WAV")
    for config in (request["separator_model"], request["vocal_model"]):
        if sha256_file(config["model_path"]) != config["model_sha256"]:
            raise ValueError("pinned model checksum mismatch")
    binding = {key: request[key] for key in ("source_file_sha256", "sample_rate_hz", "source_frames")}
    # Grid/correction generation is pinned even though separation itself does not
    # use beats. It prevents recovery from silently mixing analysis generations.
    binding["generation_key"] = request.get("generation_key")
    binding["stem_pipeline"] = STEM_PIPELINE_VERSION
    binding["separator_script_sha256"] = sha256_file(request["separator_script"])
    binding["separator_model_sha256"] = request["separator_model"]["model_sha256"]
    vocal_binding = {"pipeline": VOCAL_PIPELINE_VERSION,
                     "script_sha256": sha256_file(request["vocal_provider_script"]),
                     "model_sha256": request["vocal_model"]["model_sha256"]}
    checkpoint_path = request.get("checkpoint")
    reuse = request.get("reuse")
    checkpoint = None
    if checkpoint_path and Path(checkpoint_path).exists():
        try:
            cached = json.loads(Path(checkpoint_path).read_text())
            if cached.get("binding") == binding:
                # Validate before trusting either the audio or cached transcript.
                load_stems(cached["stems"], info.frames, info.samplerate)
                checkpoint = cached
                if reuse is None:
                    reuse = {"stems": cached["stems"]}
                    if cached.get("vocal_binding") == vocal_binding:
                        reuse["vocal_evidence"] = cached.get("vocal_evidence")
        except (OSError, ValueError, KeyError, TypeError):
            # A damaged staging checkpoint cannot invalidate a complete record.
            checkpoint = None
    signals = module_at("mixlib_signal_evidence", request["signal_script"])
    if reuse is not None:
        print("tactic analysis: reusing checksum-verified source-aligned stems", file=sys.stderr, flush=True)
        samples, rate = sf.read(source, dtype="float32", always_2d=True)
        if samples.shape[1] == 1:
            samples = np.repeat(samples, 2, axis=1)
        stems = load_stems(reuse["stems"], info.frames, info.samplerate)
        assets = reuse["stems"]
    else:
        separator = module_at("mixlib_separator", request["separator_script"])
        model = pinned_separator(request["separator_model"])
        print("tactic analysis: separating source-aligned stems", file=sys.stderr, flush=True)
        samples, rate, stems = separator.separate(str(source), 0, info.frames, model, LANES)
        assets = {}
        for lane in LANES:
            if stems[lane].shape != (info.frames, 2) or not np.isfinite(stems[lane]).all():
                raise ValueError(f"invalid {lane} stem timebase or samples")
            # A failed prior publication may leave a hardlink to an immutable
            # asset. Replace the staging name atomically, never overwrite it.
            stem_path = Path(request["stems"][lane])
            staged_stem = stem_path.with_name(stem_path.name + ".staging.wav")
            sf.write(staged_stem, stems[lane], rate, subtype="FLOAT")
            os.replace(staged_stem, stem_path)
            assets[lane] = {"path": request["stems"][lane], "sha256": sha256_file(request["stems"][lane])}
    if rate != info.samplerate or samples.shape != (info.frames, 2) or not np.isfinite(samples).all():
        raise ValueError("separator changed the source format or returned nonfinite samples")
    if checkpoint_path:
        checkpoint = {"binding": binding, "stems": assets}
        atomic_json(checkpoint_path, checkpoint)
    phrase_data = reuse.get("vocal_evidence") if reuse else None
    if phrase_data is None:
        vocal = module_at("mixlib_vocal_evidence", request["vocal_provider_script"])
        print("tactic analysis: verifying vocal phrase evidence", file=sys.stderr, flush=True)
        phrase_data = vocal.build_phrase_evidence(
            [{"role": "track", "vocals": stems["vocals"], "sample_rate": rate,
              "output_time": np.arange(len(samples), dtype=float) / rate,
              "source": str(source), "source_offset": 0}],
            np.array([0.0, len(samples) / rate]), request["vocal_model"])
    else:
        print("tactic analysis: reusing provider/model-pinned vocal evidence", file=sys.stderr, flush=True)
    if not isinstance(phrase_data, dict) or not isinstance(phrase_data.get("track"), dict):
        raise ValueError("vocal provider returned an invalid source evidence record")
    phrase_data["coordinate_system"] = "source_seconds"
    if checkpoint_path:
        checkpoint.update(vocal_binding=vocal_binding, vocal_evidence=phrase_data)
        atomic_json(checkpoint_path, checkpoint)
    vocal_for_signals = {"method": phrase_data["method"], "reference": phrase_data["reference"],
                         "coordinate_system": "source_seconds", **phrase_data["track"]}
    print("tactic analysis: measuring segment and lane compatibility", file=sys.stderr, flush=True)
    signal_data = signals.analyze_track(samples, stems, rate, bars.tolist(),
                                       vocal_evidence=vocal_for_signals,
                                       source_start_frame=0, beat_frames=beats.tolist())
    if (signal_data.get("sample_rate_hz") != rate or signal_data.get("source_start_frame") != 0
            or signal_data.get("source_end_frame") != len(samples)):
        raise ValueError("signal evidence did not preserve source frame coordinates")
    if sha256_file(source) != request["source_file_sha256"]:
        raise ValueError("source changed during tactic analysis")
    for config in (request["separator_model"], request["vocal_model"]):
        if sha256_file(config["model_path"]) != config["model_sha256"]:
            raise ValueError("pinned model changed during tactic analysis")
    result = {"method": METHOD, "sample_rate_hz": rate, "source_frames": len(samples),
              "source_file_sha256": request["source_file_sha256"],
              "vocal_evidence": phrase_data, "signal_evidence": signal_data, "stem_assets": assets,
              "limitations": ["ASR and source separation are imperfect and their scores are not calibrated probabilities",
                              "unverified language, lead identity and isolation remain unknown",
                              "tactic selection and rendered audio require independent validation"]}
    atomic_json(request["result"], result)
    return result


if __name__ == "__main__":
    analyze(json.loads(Path(sys.argv[1]).read_text()))
