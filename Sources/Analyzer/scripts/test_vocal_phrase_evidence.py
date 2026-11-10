import importlib.util
from pathlib import Path
import unittest
import numpy as np

spec = importlib.util.spec_from_file_location("vocal_phrase_evidence", Path(__file__).with_name("vocal-phrase-evidence.py"))
provider = importlib.util.module_from_spec(spec)
spec.loader.exec_module(provider)


class PhraseEvidenceTests(unittest.TestCase):
    def fixture(self, text="done.", score=0.95):
        rate = 1000
        audio = np.zeros((4000, 2), dtype=np.float32)
        audio[1000:1800] = 0.05
        transcript = {"segments": [{"avg_logprob": -0.2, "no_speech_prob": 0.01,
            "compression_ratio": 1.1, "temperature": 0.,
            "words": [{"start": 1., "end": 1.8, "word": text, "probability": score}]}]}
        return transcript, audio, rate, np.arange(4000)/rate

    def test_complete_confident_sentence_is_mapped_to_output_time(self):
        transcript, audio, rate, times = self.fixture()
        result = provider.phrase_evidence(transcript, audio, rate, times*1.1-0.4)
        self.assertAlmostEqual(result["phrase_starts"][0], 0.7)
        self.assertAlmostEqual(result["phrase_ends"][0], 1.58)
        self.assertEqual(len(result["phrases"]), 1)

    def test_breath_or_low_confidence_is_not_a_phrase_finish(self):
        result = provider.phrase_evidence(*self.fixture(text="still"))
        self.assertEqual(result["phrase_ends"], [])
        result = provider.phrase_evidence(*self.fixture(score=0.4))
        self.assertEqual(result["phrase_starts"], [])
        self.assertFalse(result["instrumental"])

    def test_untranscribed_singing_vetoes_an_apparent_safe_boundary(self):
        transcript, audio, rate, times = self.fixture()
        audio[2200:3500] = 0.05
        result = provider.phrase_evidence(transcript, audio, rate, times)
        self.assertLess(result["acoustic_word_coverage"], 0.85)
        self.assertEqual(result["phrase_ends"], [])

    def test_hallucinated_words_on_silence_are_not_evidence(self):
        transcript, audio, rate, times = self.fixture()
        audio[:] = 0
        result = provider.phrase_evidence(transcript, audio, rate, times)
        self.assertEqual(result["phrase_starts"], [])
        self.assertEqual(result["phrase_ends"], [])

    def test_empty_asr_does_not_mean_instrumental(self):
        _, audio, rate, times = self.fixture()
        result = provider.phrase_evidence({"segments": []}, audio, rate, times)
        self.assertFalse(result["instrumental"])
        audio[:] = 0
        self.assertTrue(provider.phrase_evidence({"segments": []}, audio, rate, times)["instrumental"])

    def test_local_absence_does_not_erase_unrecognized_activity(self):
        _,audio,rate,times=self.fixture()
        result=provider.phrase_evidence({"segments":[]},audio,rate,times)
        spans=result["verified_absence_spans"]
        self.assertTrue(any(abs(s["start_seconds"])<1e-9 and abs(s["end_seconds"]-1.)<1e-9 for s in spans))
        self.assertFalse(any(s["start_seconds"]<1.8 and s["end_seconds"]>1. for s in spans))
        self.assertFalse(result["instrumental"])

    def test_unrecognized_distant_passage_does_not_erase_complete_local_phrase(self):
        transcript,audio,rate,times=self.fixture()
        audio[2800:3900]=0.05
        result=provider.phrase_evidence(transcript,audio,rate,times)
        self.assertLess(result["acoustic_word_coverage"],0.85)
        self.assertEqual(result["phrase_ends"],[1.8])
        self.assertFalse(any(s["start_seconds"]<3.9 and s["end_seconds"]>2.8 for s in result["verified_absence_spans"]))


if __name__ == "__main__":
    unittest.main()
