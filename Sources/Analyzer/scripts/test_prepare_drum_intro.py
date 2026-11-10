"""Stem-envelope regressions, without loading or running the separator model."""
import importlib.util
import json
from pathlib import Path
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

import numpy as np
import soundfile as sf


SCRIPT = Path(__file__).with_name("prepare-drum-intro.py")
SPEC = importlib.util.spec_from_file_location("prepare_drum_intro", SCRIPT)
prepare = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(prepare)


class SeparatorNormalization(unittest.TestCase):
    def fixture(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        path = Path(temp.name) / "source.wav"
        rate = 8000
        signal = 0.12 * np.sin(2 * np.pi * 125 * np.arange(rate) / rate)
        samples = np.column_stack((0.2 + signal, 0.3 + signal)).astype(np.float32)
        sf.write(path, samples, rate, subtype="FLOAT")
        model = SimpleNamespace(samplerate=rate, sources=["drums", "bass", "other", "vocals"])
        return path, samples, model

    @staticmethod
    def centered_model(model, normalized, **kwargs):
        # A perfectly summing centered model, with a silent vocal estimate.
        return prepare.torch.stack([normalized * fraction for fraction in (0.2, 0.3, 0.5, 0.)], dim=1)

    def test_centered_sum_restores_removed_mixture_baseline_exactly_once(self):
        path, source, model = self.fixture()
        original = path.read_bytes()
        with patch.object(prepare, "apply_model", side_effect=self.centered_model):
            samples, rate, parts = prepare.separate(path, 0, len(source), model, model.sources)
        np.testing.assert_allclose(sum(parts.values()), samples, rtol=0, atol=8e-8)
        np.testing.assert_array_equal(parts["vocals"], 0.)
        baseline = float(source.mean(axis=1).mean())
        # Only the other lane receives the common baseline; left/right balance
        # stays in the centered estimates rather than being replaced by mono.
        np.testing.assert_allclose(parts["drums"], (samples - baseline) * 0.2, atol=3e-8)
        np.testing.assert_allclose(parts["other"], (samples - baseline) * 0.5 + baseline, atol=6e-8)
        self.assertEqual(path.read_bytes(), original)
        self.assertEqual(rate, model.samplerate)

    def test_partial_stem_request_does_not_duplicate_or_move_the_baseline(self):
        path, source, model = self.fixture()
        with patch.object(prepare, "apply_model", side_effect=self.centered_model):
            _, _, full = prepare.separate(path, 0, len(source), model, model.sources)
            _, _, drums = prepare.separate(path, 0, len(source), model, ("drums",))
            _, _, other = prepare.separate(path, 0, len(source), model, ("other",))
        np.testing.assert_array_equal(drums["drums"], full["drums"])
        np.testing.assert_array_equal(other["other"], full["other"])

    def test_restoring_dc_does_not_repair_or_hide_model_residual(self):
        path, source, model = self.fixture()
        def imperfect(model, normalized, **kwargs):
            return self.centered_model(model, normalized, **kwargs) * 0.9
        with patch.object(prepare, "apply_model", side_effect=imperfect):
            samples, _, parts = prepare.separate(path, 0, len(source), model, model.sources)
        baseline = float(source.mean(axis=1).mean())
        residual = samples - sum(parts.values())
        np.testing.assert_allclose(residual, (samples - baseline) * 0.1, atol=8e-8)
        self.assertGreater(prepare._rms(residual), 0.009)


class HandoffEnvelopes(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.directory = Path(self.temp.name)
        self.sequence = 0
        self.rate = 1000
        self.swap = 1000
        self.source_bars = self.swap + np.arange(7) * 2000

    def render(self, mode, stem, bars=None, gap=None, lead_bars=None):
        self.sequence += 1
        directory = self.directory / str(self.sequence)
        directory.mkdir()
        bars = np.array(bars if bars is not None else np.arange(7) * 2.0)
        self.source_bars = self.swap + np.arange(len(bars)) * 2000
        hold_bars = len(bars) - 3 if mode == "mashup" else 4
        warp = np.column_stack((self.source_bars, bars)).tolist()
        clips = [
            {"source": name, "prepared": str(directory / f"{name}.wav"),
             "offset": 0, "end": int(self.source_bars[-1]) + 2000, "swap": self.swap,
             "overlap_end": int(self.source_bars[-1]), "warp": warp}
            for name in ("outgoing", "incoming")
        ]
        payload = {"clips": clips, "bars": bars.tolist(),
                   "decision": str(directory / "decision.json"),
                   "first_vocal_search_bar": hold_bars,
                   "drum_lead_bars": {"mashup": hold_bars, "linger": 2}.get(mode, 0)}
        if lead_bars is not None:
            payload["drum_lead_bars"] = lead_bars
        if mode == "mashup":
            payload["mashup"] = {"hold_bars": hold_bars, "entry_beats": 1, "exit_bars": 1}

        def separate(source, offset, end, model, names):
            self.assertEqual(set(names), {"drums", "bass", "other", "vocals"})
            parts = {name: np.zeros((end - offset, 2)) for name in names}
            if stem in parts:
                # Quiet vocals exercise their gates while still providing a safe gap.
                parts[stem][:] = 0.005 if stem == "vocals" else 0.2
            if gap is not None:
                timeline = np.interp(np.arange(offset, end), self.source_bars, bars)
                parts["vocals"][:] = 0.15
                parts["vocals"][(timeline >= gap[0]) & (timeline <= gap[1])] = 0
            samples = sum(parts.values())
            if stem == "residual":
                samples = samples + 0.2
            return samples, self.rate, parts

        request = directory / "request.json"
        request.write_text(json.dumps(payload))
        with patch.object(prepare, "load_separator", return_value=None), patch.object(
            prepare, "separate", side_effect=separate
        ):
            prepare.prepare_vocal_handoff(request)
        decision = json.loads(Path(payload["decision"]).read_text())
        audio = [sf.read(clip["prepared"])[0] if Path(clip["prepared"]).exists() else None
                 for clip in clips]
        return bars, decision, audio

    def frame(self, time, bars):
        return int(round(np.interp(time, bars, self.source_bars)))

    def value(self, audio, time, bars):
        return audio[self.frame(time, bars), 0]

    def test_mashup_holds_required_bars_then_exchanges_layers_within_one_bar(self):
        for bar_times in (None, [0, 2.15, 4.1, 6.2, 8.25, 10.1, 12.2],
                          list(np.arange(9) * 2.0),
                          [0, 2.15, 4.1, 6.2, 8.25, 10.1, 12.2, 14.1, 16.05]):
            for stem in ("other", "bass"):
                with self.subTest(stem=stem, bars=bar_times):
                    bars, decision, (outgoing, incoming) = self.render("mashup", stem, bar_times)
                    self.assertEqual(decision["status"], "found")
                    start = decision["mashup"]["hold_until"]
                    end = decision["mashup"]["exit_end"]
                    hold_bars = len(bars) - 3
                    self.assertGreaterEqual(start, bars[hold_bars])
                    self.assertAlmostEqual(end - start, bars[hold_bars + 1] - bars[hold_bars])
                    self.assertLessEqual(end, bars[-1])
                    hold = slice(self.frame(0.03, bars), self.frame(bars[hold_bars], bars))
                    np.testing.assert_allclose(outgoing[hold], 0.2, atol=1e-7)
                    np.testing.assert_allclose(incoming[hold], 0, atol=1e-7)
                    quarter = start + (end - start) / 4
                    middle = (start + end) / 2
                    if stem == "other":
                        self.assertGreater(self.value(incoming, quarter, bars), 0)
                        self.assertLess(self.value(incoming, quarter, bars), 0.2)
                        self.assertAlmostEqual(self.value(outgoing, middle, bars), 0, places=5)
                    else:
                        self.assertAlmostEqual(self.value(incoming, quarter, bars), 0, places=5)
                        self.assertAlmostEqual(self.value(outgoing, quarter, bars), 0.2, places=5)
                    combined = outgoing + incoming
                    exchange = slice(self.frame(start, bars), self.frame(end, bars))
                    np.testing.assert_allclose(combined[exchange], 0.2, atol=1e-6)
                    self.assertAlmostEqual(self.value(outgoing, end + 0.02, bars), 0, places=5)
                    np.testing.assert_allclose(incoming[self.source_bars[-1]:], 0.2, atol=1e-7)

    def test_mashup_drums_enter_quickly_and_vocal_gates_never_overlap(self):
        bars, _, (outgoing, incoming) = self.render("mashup", "drums")
        self.assertAlmostEqual(self.value(outgoing, 0.03, bars), 0, places=5)
        self.assertAlmostEqual(self.value(incoming, 0.02, bars), 0.2, places=5)
        for stem in ("vocals", "residual"):
            with self.subTest(stem=stem):
                bars, decision, (outgoing, incoming) = self.render("mashup", stem)
                stop = decision["outgoing_vocals_off"]
                enter = decision["incoming_vocals_on"]
                self.assertGreaterEqual(stop, bars[4])
                self.assertGreaterEqual(enter - stop, 0.299)
                self.assertFalse(np.any((outgoing > 1e-7) & (incoming > 1e-7)))
                silent = slice(self.frame(stop, bars) + 1, self.frame(enter, bars))
                np.testing.assert_allclose(outgoing[silent], 0, atol=1e-7)
                np.testing.assert_allclose(incoming[silent], 0, atol=1e-7)

    def test_opening_vocal_gate_does_not_restore_mashup_accompaniment(self):
        for lead_bars in (0, 4):
            with self.subTest(lead_bars=lead_bars):
                bars, decision, (outgoing, incoming) = self.render(
                    "mashup", "bass", lead_bars=lead_bars
                )
                after_gate = decision["incoming_vocals_on"] + 0.09
                self.assertAlmostEqual(self.value(incoming, after_gate, bars), 0, places=5)
                self.assertAlmostEqual(self.value(outgoing, after_gate, bars), 0.2, places=5)

    def test_mashup_rejects_gap_that_occurs_before_four_bars(self):
        _, decision, audio = self.render("mashup", "other", gap=(3.0, 4.0))
        self.assertEqual(decision["status"], "no_gap")
        self.assertEqual(audio, [None, None])

    def test_late_gap_uses_gradual_handoff_instead_of_truncating_vocals(self):
        bars, decision, (outgoing, incoming) = self.render("mashup", "other", gap=(11.0, 12.0))
        self.assertEqual(decision["status"], "found")
        self.assertIsNone(decision["mashup"])
        self.assertGreater(decision["outgoing_vocals_off"], bars[5])
        self.assertAlmostEqual(self.value(outgoing, bars[2] - 0.01, bars), 0.35, places=5)
        self.assertAlmostEqual(self.value(incoming, bars[2] - 0.01, bars), 0, places=5)
        self.assertGreater(self.value(incoming, bars[3], bars), 0)

    def test_eight_bar_mashup_keeps_vocals_for_six_bars_and_rejects_earlier_gap(self):
        bar_times = list(np.arange(9) * 2.0)
        bars, decision, (outgoing, incoming) = self.render("mashup", "vocals", bar_times)
        self.assertGreaterEqual(decision["outgoing_vocals_off"], bars[6])
        held = slice(self.frame(0.03, bars), self.frame(bars[6], bars))
        np.testing.assert_allclose(outgoing[held], 0.005, atol=1e-7)
        np.testing.assert_allclose(incoming[held], 0, atol=1e-7)
        self.assertFalse(np.any((outgoing > 1e-7) & (incoming > 1e-7)))
        _, decision, audio = self.render("mashup", "other", bar_times, gap=(9.0, 10.0))
        self.assertEqual(decision["status"], "no_gap")
        self.assertEqual(audio, [None, None])

    def test_eight_bar_late_gap_preserves_two_bar_lead_and_uses_full_window(self):
        bars, decision, (outgoing, incoming) = self.render(
            "mashup", "other", list(np.arange(9) * 2.0), gap=(15.0, 16.0)
        )
        self.assertEqual(decision["status"], "found")
        self.assertIsNone(decision["mashup"])
        self.assertGreater(decision["outgoing_vocals_off"], bars[7])
        self.assertAlmostEqual(self.value(outgoing, bars[2] - 0.01, bars), 0.35, places=5)
        self.assertAlmostEqual(self.value(incoming, bars[2] - 0.01, bars), 0, places=5)
        self.assertGreater(self.value(incoming, bars[6], bars), 0)
        self.assertLess(self.value(incoming, bars[6], bars), 0.2)

    def test_legacy_linger_keeps_two_bar_lead_and_independent_gates(self):
        bars, decision, (outgoing, incoming) = self.render("linger", "other")
        held = slice(self.frame(0.03, bars), self.frame(bars[2], bars))
        np.testing.assert_allclose(outgoing[held], 0.2, atol=1e-7)
        np.testing.assert_allclose(incoming[held], 0, atol=1e-7)
        self.assertGreater(self.value(incoming, bars[3], bars), 0)
        self.assertLess(self.value(incoming, bars[3], bars), 0.2)
        bars, decision, (_, incoming) = self.render("linger", "bass")
        self.assertLess(self.value(incoming, decision["incoming_vocals_on"] + 0.09, bars), 0.2)
        np.testing.assert_allclose(incoming[self.source_bars[-1]:], 0.2, atol=1e-7)

    def test_legacy_vocal_handoff_restores_source_after_gate(self):
        bars, decision, (_, incoming) = self.render("vocal", "other")
        self.assertGreater(self.value(incoming, 0.2, bars), 0)
        self.assertLess(self.value(incoming, 0.2, bars), 0.2)
        restored = self.frame(decision["incoming_vocals_on"] + 0.09, bars)
        np.testing.assert_allclose(incoming[restored:], 0.2, atol=1e-7)


class FixedDenseMashup(unittest.TestCase):
    """Independent tonal lanes make unintended gain/ownership changes observable."""

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.directory = Path(self.temp.name)
        self.rate = 2000
        self.swap = self.rate
        self.sequence = 0

    def render(self, bars=None, phrase_end=13.1, phrase_start=14.0,
               proof=True, leak=0, residual=0, sparse=False, provider=None,
               exit_policy="six_plus_two"):
        self.sequence += 1
        directory = self.directory / str(self.sequence)
        directory.mkdir()
        bars = np.array(np.arange(9) * 2.0 if bars is None else bars)
        source_bars = self.swap + np.arange(9) * 2 * self.rate
        clips = [{"source": name, "prepared": str(directory / f"{name}.wav"),
                  "offset": 0, "end": int(source_bars[-1]) + self.rate,
                  "swap": self.swap, "overlap_end": int(source_bars[-1]),
                  "warp": np.column_stack((source_bars, bars)).tolist()}
                 for name in ("outgoing", "incoming")]
        request = {"clips": clips, "bars": bars.tolist(), "decision": str(directory / "decision.json"),
                   "mashup": {"hold_bars": 6, "exit_bars": 2, "exit_policy": exit_policy}}
        if exit_policy == "six_plus_two":
            request["tactic_id"] = "M01"
        if proof:
            request["vocal_phrase_evidence"] = {
                "method": "verified_fixture_phrase_boundaries/1", "reference": "fixture:phrases-v1",
                "outgoing": {"phrase_ends": [phrase_end]},
                "incoming": {"phrase_starts": [phrase_start]},
            }
        if provider:
            request["vocal_phrase_provider"] = {"script": str(provider)}
        original = []

        def separator(source, offset, end, model, names):
            index = int(source == "incoming")
            frames = np.arange(offset, end)
            time = (frames - self.swap) / self.rate * bars[-1] / 16.0
            inside = (frames >= source_bars[0]) & (frames <= source_bars[-1])
            time[inside] = np.interp(frames[inside], source_bars, bars)
            frequencies = ({"drums": 310, "bass": 45, "other": 130, "vocals": 230},
                           {"drums": 410, "bass": 65, "other": 170, "vocals": 270})[index]
            stems = {name: np.repeat((0.08 * np.sin(2 * np.pi * frequency * frames / self.rate))[:, None],
                                     2, axis=1) for name, frequency in frequencies.items()}
            if index == 0:
                stems["vocals"][(time < 0.2) | (time >= phrase_end)] = 0
                if sparse:
                    stems["other"][:] = 0
                    stems["bass"][:] = 0
            else:
                stems["vocals"][(time >= 10) & (time < phrase_start)] = 0
                # Even a quiet coherent copy in B's drums must veto an automatic vocal-safe claim.
                stems["drums"] += leak * stems["vocals"]
            samples = sum(stems.values())
            if residual:
                samples += residual * np.repeat(np.sin(2 * np.pi * 530 * frames / self.rate)[:, None], 2, axis=1)
            original.append({"samples": samples, "stems": stems, "time": time})
            return samples, self.rate, stems

        path = directory / "request.json"
        path.write_text(json.dumps(request))
        with patch.object(prepare, "load_separator", return_value=None), patch.object(
                prepare, "separate", side_effect=separator):
            prepare.prepare_vocal_handoff(path)
        decision = json.loads(Path(request["decision"]).read_text())
        audio = [sf.read(clip["prepared"])[0] if Path(clip["prepared"]).exists() else None for clip in clips]
        return bars, decision, audio, original

    def test_exact_six_plus_two_with_variable_tempo_and_independent_vocals(self):
        for bar_times in (None, [0, 2.15, 4.1, 6.2, 8.25, 10.1, 12.2, 14.1, 16.05]):
            with self.subTest(bars=bar_times):
                bars, decision, audio, original = self.render(bar_times)
                self.assertEqual(decision["status"], "found", decision.get("reason"))
                self.assertEqual(decision["tactic_id"], "M01")
                self.assertEqual(decision["mashup"]["hold_until"], bars[6])
                self.assertEqual(decision["mashup"]["exit_end"], bars[8])
                self.assertGreater(decision["outgoing_vocals_off"], 13.1)
                self.assertAlmostEqual(decision["incoming_vocals_on"] + 0.08, 14.0)
                self.assertGreater(decision["incoming_vocals_on"] - decision["outgoing_vocals_off"], 0.30)
                for index, data in enumerate(original):
                    held = (data["time"] >= 0.03) & (data["time"] < bars[6])
                    expected = (data["stems"]["other"] + data["stems"]["bass"] + data["stems"]["vocals"]
                                if index == 0 else data["stems"]["drums"])
                    np.testing.assert_allclose(audio[index][held], expected[held], atol=2e-8)
                # Halfway through the fixed accompaniment exit both unrelated other lanes
                # must be present, even though A's vocals have already stopped.
                time = original[0]["time"]
                middle = (bars[6] + bars[8]) / 2
                at = np.argmin(abs(time - middle))
                other_a = original[0]["stems"]["other"][at, 0]
                bass_a = original[0]["stems"]["bass"][at, 0]
                self.assertAlmostEqual(audio[0][at, 0], 0.5 * (other_a + bass_a), delta=0.002)
                # The prepared B becomes its exact source only after all lane schedules finish.
                after = original[1]["time"] >= bars[8]
                np.testing.assert_allclose(audio[1][after], original[1]["samples"][after], atol=2e-8)

    def test_vocal_boundary_moves_without_moving_accompaniment(self):
        first = self.render(phrase_end=12.8, phrase_start=13.6)
        later = self.render(phrase_end=14.2, phrase_start=15.0)
        for rendered in (first, later):
            self.assertEqual(rendered[1]["status"], "found", rendered[1].get("reason"))
        self.assertEqual(first[1]["mashup"], later[1]["mashup"])
        self.assertNotEqual(first[1]["outgoing_vocals_off"], later[1]["outgoing_vocals_off"])
        # Incoming drums and accompaniment before either vocal entry are identical.
        before_vocals = first[3][1]["time"] < 13.5
        np.testing.assert_allclose(first[2][1][before_vocals], later[2][1][before_vocals], atol=2e-8)

    def test_missing_phrase_evidence_returns_candidates_without_rendering(self):
        _, decision, audio, _ = self.render(proof=False)
        self.assertEqual(decision["status"], "no_gap")
        self.assertEqual(decision["reason_code"], "VOCAL_PHRASE_EVIDENCE_REQUIRED")
        self.assertTrue(decision["candidate_boundary_windows"]["outgoing"])
        self.assertIn("complete_incoming_phrase_or_pickup_start", decision["required_evidence"])
        self.assertEqual(audio, [None, None])
        self.assertEqual(decision["evidence"]["clips"][0]["acoustic_vocals"]["semantics"],
                         "acoustic_activity_not_word_or_phrase_completion")

    def test_adaptive_exit_uses_selected_layers_and_never_downgrades_unsafe_m01(self):
        _, dense, _, _ = self.render(exit_policy="adaptive")
        self.assertEqual(dense["tactic_id"], "M01")
        self.assertEqual(dense["mashup"]["exit_policy"], "six_plus_two")
        _, sparse, _, _ = self.render(exit_policy="adaptive", sparse=True, proof=False)
        self.assertEqual(sparse["status"], "found")
        self.assertEqual(sparse["tactic_id"], "M02")
        self.assertEqual(sparse["mashup"]["exit_policy"], "vocal_gap_one_bar")
        self.assertAlmostEqual(sparse["mashup"]["exit_end"] - sparse["mashup"]["hold_until"], 2.0)
        _, unsafe, audio, _ = self.render(exit_policy="adaptive", proof=False)
        self.assertEqual(unsafe["status"], "no_gap")
        self.assertEqual(unsafe["reason_code"], "VOCAL_PHRASE_EVIDENCE_REQUIRED")
        self.assertEqual(unsafe["tactic_id"], "M01")
        self.assertEqual(audio, [None, None])

    def test_unsafe_or_unsuitable_selected_layers_are_rejected(self):
        for kwargs, reason in (({"leak": 0.25}, "STEM_VOCAL_LEAKAGE"),
                               ({"residual": 0.10}, "STEM_RECONSTRUCTION_UNSAFE"),
                               ({"sparse": True}, "MASHUP_NOT_DENSE")):
            with self.subTest(reason=reason):
                _, decision, audio, _ = self.render(**kwargs)
                self.assertEqual(decision["status"], "no_gap")
                self.assertEqual(decision["reason_code"], reason)
                self.assertEqual(audio, [None, None])

    def test_incomplete_late_phrase_cannot_be_sacrificed_for_eight_bars(self):
        _, decision, audio, _ = self.render(phrase_end=15.8, phrase_start=16.5)
        self.assertEqual(decision["reason_code"], "VOCAL_PHRASE_WINDOW_INFEASIBLE")
        self.assertEqual(audio, [None, None])

    def test_provider_uses_loaded_vocals_without_a_second_separation(self):
        provider = self.directory / "provider.py"
        provider.write_text('''
def build_phrase_evidence(loaded, bars, config):
    assert len(loaded) == 2
    assert loaded[0]['vocals'].shape[1] == 2
    assert loaded[0]['output_time'][0] < 0
    assert bars[6] == 12
    return {'method': 'verified_fixture_provider/1', 'reference': 'fixture:provider',
            'outgoing': {'phrase_ends': [13.1]}, 'incoming': {'phrase_starts': [14.0]}}
''')
        _, decision, _, _ = self.render(proof=False, provider=provider)
        self.assertEqual(decision["status"], "found", decision.get("reason"))
        self.assertEqual(decision["evidence"]["vocal_schedule"]["reference"], "fixture:provider")
        provider.write_text("def build_phrase_evidence(*args):\n    raise RuntimeError('model unavailable')\n")
        _, failed, audio, _ = self.render(proof=False, provider=provider)
        self.assertEqual(failed["reason_code"], "VOCAL_PHRASE_PROVIDER_FAILED")
        self.assertEqual(audio, [None, None])


class VocalPhraseSchedule(unittest.TestCase):
    def test_guard_cannot_open_in_the_middle_of_bs_line(self):
        evidence = {"clips": [
            {"acoustic_vocals": {"active_spans": [[0, 13.0]]}},
            {"acoustic_vocals": {"active_spans": [[12.0, 14.5], [15, 17]]}},
        ]}
        payload = {"vocal_phrase_evidence": {
            "method": "verified_boundaries/1", "reference": "fixture:test",
            "outgoing": {"phrase_ends": [-8.0, -1.0, 13.0]},
            "incoming": {"phrase_starts": [-1.5, 13.5, 15.0, 18.0]},
        }}
        stop, enter = prepare.fixed_mashup_vocal_schedule(payload, evidence, np.arange(9) * 2.0)
        self.assertAlmostEqual(enter, 14.92)
        self.assertGreater(enter - stop, 0.30)
        self.assertEqual(evidence["vocal_phrase_evidence"]["outgoing"]["phrase_ends"][0], -8.0)

    def test_false_instrumental_claim_is_rejected(self):
        evidence = {"clips": [{"acoustic_vocals": {"active_spans": [[0, 13]]}},
                              {"acoustic_vocals": {"active_spans": []}}]}
        payload = {"vocal_phrase_evidence": {
            "method": "verified_boundaries/1", "reference": "fixture:test",
            "outgoing": {"instrumental": True}, "incoming": {"instrumental": True},
        }}
        with self.assertRaisesRegex(prepare.TacticEvidenceMissing, "conflicts"):
            prepare.fixed_mashup_vocal_schedule(payload, evidence, np.arange(9) * 2.0)

    def test_invalid_warp_is_rejected_before_separation(self):
        payload = {"bars": list(np.arange(9) * 2.0), "clips": [
            {"offset": 0, "swap": 0, "overlap_end": 16000, "end": 17000,
             "warp": [[0, 0], [4000, 8], [3000, 12], [16000, 16]]} for _ in range(2)]}
        with self.assertRaisesRegex(ValueError, "increasing source/output"):
            prepare.validate_handoff_payload(payload)


if __name__ == "__main__":
    unittest.main()
