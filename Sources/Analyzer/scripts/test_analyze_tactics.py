"""Full-track staging contract tests with deterministic local model doubles."""
import importlib.util
import json
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

import numpy as np
import soundfile as sf

SPEC = importlib.util.spec_from_file_location("analyze_tactics", Path(__file__).with_name("analyze-tactics.py"))
analyzer = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(analyzer)


class TacticAnalysisStaging(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.directory = Path(self.temp.name)
        self.rate = 8000
        self.source = self.directory / "source.wav"
        samples = np.repeat((0.1 * np.sin(2 * np.pi * 220 * np.arange(self.rate) / self.rate))[:, None], 2, axis=1)
        sf.write(self.source, samples, self.rate, subtype="FLOAT")
        self.separator = self.directory / "separator.py"
        self.separator.write_text('''
import soundfile as sf
def separate(source, offset, end, model, names):
    samples, rate = sf.read(source, dtype='float32', always_2d=True)
    return samples, rate, {name: samples / 4 for name in names}
''')
        self.signal = self.directory / "signals.py"
        self.signal.write_text('''
def analyze_track(samples, stems, sample_rate, bar_frames, vocal_evidence=None,
                  source_start_frame=0, beat_frames=None):
    assert len(stems) == 4
    assert vocal_evidence['coordinate_system'] == 'source_seconds'
    return {'method':'fixture_signals/1','sample_rate_hz':sample_rate,
            'source_start_frame':source_start_frame,'source_end_frame':len(samples),
            'bar_frames':bar_frames,'beat_frames':beat_frames,'vocal_presence':'unknown'}
''')
        self.vocal = self.directory / "vocal.py"
        self.vocal.write_text('''
def build_phrase_evidence(loaded, bounds, config):
    assert len(loaded) == 1 and loaded[0]['role'] == 'track'
    assert loaded[0]['output_time'][0] == 0
    assert len(loaded[0]['output_time']) == len(loaded[0]['vocals'])
    return {'method':'fixture_voice/1','reference':'fixture:model',
            'track':{'phrases':[],'words':[],'instrumental':False}}
''')
        model = self.directory / "model.bin"
        model.write_bytes(b"fixture model bytes")
        pinned = {"model_path": str(model), "model_sha256": analyzer.sha256_file(model)}
        self.request = {
            "source": str(self.source), "source_file_sha256": analyzer.sha256_file(self.source),
            "sample_rate_hz": self.rate, "source_frames": self.rate,
            "beat_frames": [0, 1000, 2000, 3000, 4000, 5000, 6000, 7000],
            "bar_frames": [0, 3000, 6000],
            "separator_script": str(self.separator), "signal_script": str(self.signal),
            "vocal_provider_script": str(self.vocal), "separator_model": pinned, "vocal_model": pinned,
            "stems": {lane: str(self.directory / f"{lane}.wav") for lane in analyzer.LANES},
            "result": str(self.directory / "result.json"),
        }

    def run_analysis(self):
        with patch.object(analyzer, "pinned_separator", return_value=None):
            return analyzer.analyze(self.request)

    def test_source_identity_timebase_and_actual_meter_survive_staging(self):
        original = self.source.read_bytes()
        result = self.run_analysis()
        self.assertEqual(self.source.read_bytes(), original)
        self.assertEqual(result["source_frames"], self.rate)
        self.assertEqual(result["signal_evidence"]["bar_frames"], [0, 3000, 6000])
        self.assertEqual(result["vocal_evidence"]["coordinate_system"], "source_seconds")
        self.assertFalse(result["vocal_evidence"]["track"]["instrumental"])
        self.assertEqual(json.loads(Path(self.request["result"]).read_text()), result)
        reconstruction = 0
        for path in self.request["stems"].values():
            samples, rate = sf.read(path)
            self.assertEqual(rate, self.rate)
            self.assertEqual(samples.shape, (self.rate, 2))
            reconstruction = reconstruction + samples
        np.testing.assert_allclose(reconstruction, sf.read(self.source)[0], atol=1e-8)

    def test_absent_grid_is_not_replaced_by_four_beats_per_bar(self):
        self.request["beat_frames"] = []
        self.request["bar_frames"] = []
        result = self.run_analysis()
        self.assertEqual(result["signal_evidence"]["bar_frames"], [])
        self.assertEqual(result["signal_evidence"]["beat_frames"], [])

    def test_changed_source_and_unsafe_destinations_are_rejected(self):
        with self.source.open("ab") as source:
            source.write(b"changed")
        with self.assertRaisesRegex(ValueError, "changed before"):
            self.run_analysis()
        self.assertFalse(Path(self.request["result"]).exists())
        self.request["source_file_sha256"] = analyzer.sha256_file(self.source)
        self.request["stems"]["vocals"] = str(self.source)
        with self.assertRaisesRegex(ValueError, "replace the source"):
            self.run_analysis()

    def test_nonfinite_evidence_cannot_replace_previous_complete_result(self):
        result = Path(self.request["result"])
        result.write_text('{"previous":"valid"}')
        self.signal.write_text('''
def analyze_track(samples, stems, sample_rate, *args, **kwargs):
    return {'method':'fixture_signals/1','sample_rate_hz':sample_rate,
            'source_start_frame':0,'source_end_frame':len(samples),'invalid':float('nan')}
''')
        with self.assertRaises(ValueError):
            self.run_analysis()
        self.assertEqual(json.loads(result.read_text()), {"previous": "valid"})

    def test_truncated_stem_preparation_never_publishes_metadata(self):
        self.separator.write_text('''
import soundfile as sf
def separate(source, offset, end, model, names):
    samples, rate = sf.read(source, dtype='float32', always_2d=True)
    return samples, rate, {name: samples[:-1] for name in names}
''')
        with self.assertRaisesRegex(ValueError, "stem timebase"):
            self.run_analysis()
        self.assertFalse(Path(self.request["result"]).exists())

    def test_signal_refresh_reuses_verified_stems_and_voice_without_models(self):
        first = self.run_analysis()
        self.request["reuse"] = {"stems": first["stem_assets"], "vocal_evidence": first["vocal_evidence"]}
        self.separator.write_text("raise AssertionError('separator must not load')")
        self.vocal.write_text("raise AssertionError('voice provider must not load')")
        with patch.object(analyzer, "pinned_separator", side_effect=AssertionError("model must not load")):
            second = analyzer.analyze(self.request)
        self.assertEqual(second["vocal_evidence"], first["vocal_evidence"])
        self.assertEqual(second["stem_assets"], first["stem_assets"])

    def test_corrupt_reuse_cannot_publish_or_be_silently_trusted(self):
        first = self.run_analysis()
        self.request["reuse"] = {"stems": first["stem_assets"], "vocal_evidence": first["vocal_evidence"]}
        with Path(self.request["stems"]["vocals"]).open("ab") as wav:
            wav.write(b"changed")
        with self.assertRaisesRegex(ValueError, "checksum mismatch"):
            self.run_analysis()
        self.assertEqual(json.loads(Path(self.request["result"]).read_text()), first)

    def test_downstream_failure_resumes_checkpoint_without_separator_or_asr(self):
        self.request["checkpoint"] = str(self.directory / "inputs.checkpoint.json")
        self.request["generation_key"] = "pinned-audio-beats-corrections"
        original_signal = self.signal.read_text()
        self.signal.write_text("def analyze_track(*args, **kwargs): raise RuntimeError('signal failed')")
        with self.assertRaisesRegex(RuntimeError, "signal failed"):
            self.run_analysis()
        self.assertTrue(Path(self.request["checkpoint"]).exists())
        self.signal.write_text(original_signal)
        with patch.object(analyzer, "pinned_separator", side_effect=AssertionError("no repeated separation")), \
             patch.object(analyzer, "module_at", wraps=analyzer.module_at) as imported:
            result = analyzer.analyze(self.request)
        self.assertEqual([call.args[0] for call in imported.call_args_list], ["mixlib_signal_evidence"])
        self.assertEqual(result["source_frames"], self.rate)

    def test_provider_change_reuses_stems_but_refreshes_voice(self):
        self.request["checkpoint"] = str(self.directory / "inputs.checkpoint.json")
        self.run_analysis()
        self.vocal.write_text(self.vocal.read_text().replace("fixture_voice/1", "fixture_voice/2"))
        with patch.object(analyzer, "pinned_separator", side_effect=AssertionError("no repeated separation")):
            result = analyzer.analyze(self.request)
        self.assertEqual(result["vocal_evidence"]["method"], "fixture_voice/2")

    def test_changed_generation_cannot_reuse_staging_checkpoint(self):
        self.request["checkpoint"] = str(self.directory / "inputs.checkpoint.json")
        self.request["generation_key"] = "before-correction"
        self.run_analysis()
        self.request["generation_key"] = "after-correction"
        with patch.object(analyzer, "pinned_separator", side_effect=RuntimeError("fresh separation required")):
            with self.assertRaisesRegex(RuntimeError, "fresh separation required"):
                analyzer.analyze(self.request)

    def test_regeneration_never_overwrites_existing_immutable_hardlink(self):
        old_path = Path(self.request["stems"]["vocals"])
        old_path.write_bytes(b"immutable prior publication")
        immutable = self.directory / "published-vocals.wav"
        os.link(old_path, immutable)
        self.run_analysis()
        self.assertEqual(immutable.read_bytes(), b"immutable prior publication")
        self.assertNotEqual(old_path.stat().st_ino, immutable.stat().st_ino)


if __name__ == "__main__":
    unittest.main()
