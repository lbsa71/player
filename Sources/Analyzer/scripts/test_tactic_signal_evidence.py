"""Synthetic signal regressions. No models or copyrighted recordings needed."""
import importlib.util
import json
from pathlib import Path
import unittest

import numpy as np

SPEC = importlib.util.spec_from_file_location("tactic_signal_evidence", Path(__file__).with_name("tactic-signal-evidence.py"))
signals = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(signals)

RATE = 8000


def stereo(x):
    return np.column_stack((x, x)).astype(np.float32)


def tone(frequency, frames, gain=0.1):
    return gain * np.sin(2 * np.pi * frequency * np.arange(frames) / RATE)


def parts(seconds):
    return {name: np.zeros((round(seconds * RATE), 2), np.float32) for name in signals.LANES}


def drums(frames, first=0, until=None, interval=0.25, gain=0.3):
    rng = np.random.default_rng(42)
    output = np.zeros(frames)
    for start in np.arange(first, frames / RATE if until is None else until, interval):
        at = round(start * RATE)
        length = min(round(RATE * 0.06), frames-at)
        if length <= 0:
            continue
        time = np.arange(length) / RATE
        attack = np.minimum(1., time/0.001)
        output[at:at+length] += gain * attack * np.exp(-time/0.015) * (0.7*np.sin(2*np.pi*70*time)+0.3*rng.normal(size=length))
    return output


def analyze(stems, vocal=None, source=None):
    source = sum(stems.values()) if source is None else source
    frames = len(source)
    return signals.analyze_tactic_signals(source, stems, RATE,
        np.arange(0, frames, RATE//4).tolist(), np.arange(0, frames+1, RATE).tolist(), vocal)


class PitchEvidence(unittest.TestCase):
    def test_structural_windows_cover_audio_without_inventing_a_grid(self):
        stems=parts(3.2)
        stems["other"][:]=stereo(tone(220,len(stems["other"])))
        source=sum(stems.values())
        record=signals.analyze_tactic_signals(source,stems,RATE,[],[])
        self.assertEqual(record["bars"],[])
        self.assertEqual(record["segments"],[])
        windows=record["unmetered_windows"]
        self.assertEqual(windows[0]["start_frame"],0)
        self.assertEqual(windows[-1]["end_frame"],len(source))
        self.assertTrue(all(a["end_frame"]==b["start_frame"] for a,b in zip(windows,windows[1:])))
        self.assertTrue(all(w["pulse"]["beat_count"] is None for w in windows))
        self.assertTrue(all(w["stems"]["other"]["rms"]>0.05 for w in windows))

    def test_bar_intro_and_tail_windows_do_not_duplicate_coverage(self):
        stems=parts(3.2)
        source=sum(stems.values())
        record=signals.analyze_tactic_signals(source,stems,RATE,[4000,8000,12000,16000,20000],[4000,12000,20000])
        windows=sorted(record["bars"]+record["unmetered_windows"],key=lambda w:w["start_frame"])
        self.assertEqual(sum(w["end_frame"]-w["start_frame"] for w in windows),len(source))
        self.assertTrue(all(a["end_frame"]==b["start_frame"] for a,b in zip(windows,windows[1:])))

    def test_monophonic_note_sequence_is_transcribed_at_the_right_pitch(self):
        pieces=[]
        for midi in (60, 62, 64, 67):
            hz=440*2**((midi-69)/12)
            length=round(0.65*RATE)
            wave=tone(hz,length,0.12)+tone(hz*2,length,0.025)+tone(hz*3,length,0.01)
            ramp=np.minimum(1.,np.arange(length)/(RATE*0.01))*np.minimum(1.,np.arange(length)[::-1]/(RATE*0.01))
            pieces.extend([wave*ramp,np.zeros(round(0.15*RATE))])
        result=signals._pitch_notes(stereo(np.concatenate(pieces)),RATE,"other")
        self.assertEqual([n["midi_note"] for n in result["notes"]],[60,62,64,67])
        self.assertTrue(all(abs(n["tuning_cents"])<12 for n in result["notes"]))
        self.assertTrue(all(n["score"]>0.85 for n in result["notes"]))

    def test_noise_and_polyphonic_chords_do_not_receive_confident_single_notes(self):
        count=3*RATE
        noise=np.random.default_rng(8).normal(0,0.10,count)
        chord=sum(tone(f,count,0.08) for f in (261.625565,329.627557,391.995436))
        for audio in (noise,chord,tone(440,count,0.04)+noise):
            with self.subTest(kind=float(np.std(audio))):
                result=signals._pitch_notes(stereo(audio),RATE,"other")
                self.assertEqual(result["notes"],[])
                self.assertGreater(sum(result["rejected_frame_counts"].values()),0)

    def test_bass_fundamental_and_opposed_stereo_are_retained(self):
        x=tone(55,2*RATE,0.1)
        result=signals._pitch_notes(np.column_stack((x,-x)),RATE,"bass")
        self.assertEqual(len(result["notes"]),1)
        self.assertEqual(result["notes"][0]["midi_note"],33)


class TrackEvidence(unittest.TestCase):
    def test_compact_segments_reconstruct_identical_metrics_without_repeated_arrays(self):
        stems=parts(5)
        stems["drums"]=stereo(drums(5*RATE))
        stems["other"]=stereo(tone(440,5*RATE))
        evidence=analyze(stems)
        self.assertEqual(evidence["storage_version"],2)
        expanded=dict(evidence)
        expanded["segments"]=[signals.materialize_window(evidence,s) for s in evidence["segments"]]
        restored=signals.compact_signal_evidence(expanded)
        self.assertEqual(restored,evidence)
        self.assertLess(len(json.dumps(evidence)),len(json.dumps(expanded))*0.40)
        for compact, full in zip(evidence["segments"],expanded["segments"]):
            self.assertNotIn("stems",compact)
            self.assertNotIn("source",compact)
            self.assertEqual(compact["source_rms"],full["source"]["rms"])
            self.assertEqual(compact["bar_count"],len(compact["bar_indices"]))
            self.assertEqual(full["stems"]["drums"]["onset_count"],
                sum(evidence["bars"][i]["stems"]["drums"]["onset_count"] for i in compact["bar_indices"]))
        corrupt=dict(evidence["segments"][0],bar_indices=[-1])
        with self.assertRaises(ValueError):signals.materialize_window(evidence,corrupt)
        corrupt=dict(evidence["segments"][0],end_frame=RATE+1)
        with self.assertRaises(ValueError):signals.materialize_window(evidence,corrupt)

    def test_drums_and_bass_are_distinguished_by_stems_not_low_frequency_energy(self):
        stems=parts(4)
        stems["bass"]=stereo(tone(70,4*RATE,0.12))
        bass=analyze(stems)
        self.assertEqual(bass["bars"][1]["stems"]["drums"]["activity_fraction"],0)
        self.assertGreater(bass["bars"][1]["stems"]["bass"]["activity_fraction"],0.95)
        self.assertNotIn("drum_groove",bass["bars"][1]["roles"])
        stems=parts(4)
        stems["drums"]=stereo(drums(4*RATE))
        groove=analyze(stems)
        self.assertGreaterEqual(groove["bars"][1]["stems"]["drums"]["onset_count"],3)
        self.assertIn("drum_groove",groove["bars"][1]["roles"])
        self.assertEqual(groove["bars"][1]["stems"]["bass"]["activity_fraction"],0)

    def test_lagged_filtered_vocal_copy_is_flagged_but_unrelated_drums_are_not(self):
        stems=parts(3)
        time=np.arange(3*RATE)/RATE
        voice=(tone(330,3*RATE,0.14)+tone(660,3*RATE,0.045))*(0.6+0.4*np.sin(2*np.pi*3*time))
        stems["vocals"]=stereo(voice)
        stems["drums"]=stereo(drums(3*RATE,gain=0.08))
        clean=signals._leakage(stems["vocals"],stems["drums"],RATE,0,3*RATE)
        self.assertEqual(clean["status"],"not_detected")
        copied=np.r_[np.zeros(80),voice[:-80]]*0.15
        stems["drums"]+=stereo(copied)
        leaked=signals._leakage(stems["vocals"],stems["drums"],RATE,0,3*RATE)
        self.assertEqual(leaked["status"],"suspected")
        self.assertGreater(leaked["max_projection_ratio"],0.08)
        self.assertTrue(any(abs(abs(s["lag_frames"])-80)<10 for s in leaked["suspect_spans"]))

    def test_quiet_vocal_stem_and_instrumental_hint_do_not_prove_absence(self):
        stems=parts(2)
        stems["other"]=stereo(tone(250,2*RATE))
        evidence={"method":"whisper-words-acoustic-phrase/1","coordinate_system":"source_seconds",
                  "reference":"fixture:model","words":[],"phrases":[],"instrumental":True}
        result=analyze(stems,evidence)
        self.assertTrue(all(b["vocal"]["presence"]=="unknown" for b in result["bars"]))
        self.assertEqual(result["bars"][0]["vocal_leakage"]["other"]["status"],"unassessed")

    def test_supported_words_agree_with_audio_and_keep_source_frame_boundaries(self):
        stems=parts(2)
        voice=np.zeros(2*RATE)
        voice[2000:6000]=tone(330,4000,0.1)
        stems["vocals"]=stereo(voice)
        evidence={"method":"whisper-words-acoustic-phrase/1","coordinate_system":"source_seconds","reference":"fixture:model",
            "words":[{"start_seconds":0.25,"end_seconds":0.75,"word":"finished.","score":0.95}],
            "phrases":[{"start_seconds":0.25,"end_seconds":0.75,"entry_supported":True,"exit_supported":True}]}
        result=analyze(stems,evidence)
        self.assertEqual(result["bars"][0]["vocal"]["presence"],"present")
        self.assertEqual(result["vocal_evidence"]["phrases"][0]["end_frame"],6000)
        shifted=signals.analyze_track(sum(stems.values()),stems,RATE,[0,RATE,2*RATE],evidence,source_start_frame=123,
                                     beat_frames=list(range(0,2*RATE,RATE//4)))
        self.assertEqual(shifted["vocal_evidence"]["phrases"][0]["end_frame"],6123)
        self.assertEqual(shifted["bars"][0]["vocal"]["supported_phrases"][0]["end_frame"],6123)
        self.assertEqual(shifted["segments"][0]["start_frame"],123)

    def test_pcm_seam_measurement_distinguishes_periodic_and_broken_loops(self):
        audio=stereo(tone(250,RATE,0.1))
        c=[1.]+[0.]*11
        good=signals._loop_measure(audio,RATE,0,RATE,c,c)
        self.assertTrue(good["signal_safe"],good)
        broken=audio.copy();broken[-1]=0.8
        bad=signals._loop_measure(broken,RATE,0,RATE,c,c)
        self.assertFalse(bad["signal_safe"])
        different=[0.]*6+[1.]+[0.]*5
        self.assertFalse(signals._loop_measure(audio,RATE,0,RATE,c,different)["signal_safe"])

    def test_recognized_words_need_acoustic_support_at_their_own_timestamps(self):
        stems=parts(2)
        stems["vocals"][5000:7000]=stereo(tone(330,2000,0.1))
        evidence={"method":"word_alignment/1","reference":"fixture:words","coordinate_system":"source_frames",
                  "words":[{"start_frame":1000,"end_frame":3000,"text":"hallucinated", "score":0.99}]}
        result=analyze(stems,evidence)
        self.assertEqual(result["bars"][0]["vocal"]["presence"],"unknown")
        self.assertEqual(result["bars"][0]["vocal"]["recognized_words"],[])
        self.assertEqual(signals.materialize_window(result,result["segments"][0])["vocal"]["recognized_words"],[])

    def test_drum_onsets_preserve_alternating_low_and_high_accent_hierarchy(self):
        rng=np.random.default_rng(26)
        wave=np.zeros(2*RATE)
        for i,at in enumerate((0,RATE//2,RATE,3*RATE//2)):
            size=round(RATE*0.09)
            time=np.arange(size)/RATE
            envelope=np.minimum(1,time/0.001)*np.exp(-time/0.022)
            if i%2==0:
                hit=0.5*np.sin(2*np.pi*70*time)*envelope
            else:
                from scipy.signal import butter,sosfilt
                hit=sosfilt(butter(3,1000,btype="highpass",fs=RATE,output="sos"),rng.normal(0,0.5,size))*envelope
            wave[at:at+size]+=hit
        onsets=signals._onsets(stereo(wave),RATE,pulse_bands=True)
        self.assertEqual(len(onsets),4)
        for i,event in enumerate(onsets):
            self.assertLessEqual(abs(event["frame"]-i*RATE//2),RATE*0.02)
            if i%2==0:
                self.assertGreater(event["low_band_strength"],event["high_band_strength"]*3)
            else:
                self.assertGreater(event["high_band_strength"],event["low_band_strength"]*3)

    def test_reconstruction_and_artifact_scores_are_measured_not_default_success(self):
        stems=parts(2)
        stems["other"]=stereo(tone(250,2*RATE,0.10))
        source=sum(stems.values())+stereo(np.random.default_rng(3).normal(0,0.03,2*RATE))
        result=analyze(stems,source=source)
        self.assertGreater(result["bars"][0]["reconstruction"]["relative_rms_error"],0.3)
        stems["other"][np.arange(100,2*RATE,200)]=1.2
        altered=analyze(stems,source=source)
        self.assertGreater(altered["bars"][0]["stems"]["other"]["artifact"]["score"],0.1)
        json.dumps(altered,allow_nan=False)

    def test_arrangement_events_need_energy_and_actual_drum_bass_support(self):
        stems=parts(8)
        stems["other"][:]=stereo(tone(250,8*RATE,0.004))
        stems["drums"]=stereo(drums(8*RATE,first=2,until=6,gain=0.3))
        stems["bass"][2*RATE:6*RATE]=stereo(tone(70,4*RATE,0.1))
        # A natural, genuinely decaying final note followed by quiet PCM.
        tail_time=np.arange(RATE)/RATE
        stems["other"][6*RATE:7*RATE]=stereo(tone(250,RATE,0.15)*np.exp(-tail_time*7))
        stems["other"][7*RATE:]=0
        result=analyze(stems)
        events=result["events"]
        self.assertTrue(any(e["kind"]=="drum_return" and e["frame"]==2*RATE for e in events),events)
        self.assertTrue(any(e["kind"]=="drop" and e["frame"]==2*RATE for e in events),events)
        self.assertTrue(any(e["kind"]=="natural_ending" for e in events),events)
        # Sustained bass alone must never establish a drum return or drop.
        stems=parts(4);stems["bass"][2*RATE:]=stereo(tone(70,2*RATE,0.2))
        self.assertFalse(any(e["kind"] in ("drum_return","drop") for e in analyze(stems)["events"]))

    def test_build_requires_a_second_rising_cue_and_motifs_have_note_evidence(self):
        stems=parts(4)
        for i,(interval,gain) in enumerate(((0.5,0.06),(0.25,0.12),(0.125,0.24))):
            stems["drums"][i*RATE:(i+1)*RATE]=stereo(drums(RATE,interval=interval,gain=gain))
        for i,midi in enumerate((60,62,64,67)):
            stems["other"][i*RATE:(i+1)*RATE]=stereo(tone(440*2**((midi-69)/12),RATE,0.02))
        result=analyze(stems)
        self.assertTrue(any(e["kind"]=="build" for e in result["events"]))
        self.assertTrue(any(m["lane"]=="other" and len(m["notes"])>=2 for m in result["motifs"]))

    def test_invalid_nonfinite_or_unordered_timebases_fail(self):
        stems=parts(2)
        source=sum(stems.values());source[0,0]=np.nan
        with self.assertRaises(ValueError):analyze(stems,source=source)
        with self.assertRaises(ValueError):signals.analyze_tactic_signals(sum(stems.values()),stems,RATE,[0,1,1],[0,RATE,2*RATE])


class PairEvidence(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        a=parts(4);a["other"]=stereo(tone(440,4*RATE,0.1));a["bass"]=stereo(tone(110,4*RATE,0.08))
        b=parts(4);b["drums"]=stereo(drums(4*RATE));b["bass"]=stereo(tone(116.54094,4*RATE,0.08));b["other"]=stereo(tone(466.16376,4*RATE,0.1))
        evidence_a, evidence_b = analyze(a), analyze(b)
        cls.a=signals.materialize_window(evidence_a,evidence_a["segments"][0])
        cls.b=signals.materialize_window(evidence_b,evidence_b["segments"][0])

    def test_selected_drums_are_complementary_despite_other_unused_conflicting_stems(self):
        result=signals.compare_tactic_signals(self.a,self.b,{"A":["other","bass"],"B":["drums"]})
        self.assertEqual(result["status"],"supported",result)
        self.assertFalse(result["bass_collision"])
        self.assertFalse(result["lead_collision"])

    def test_bass_and_lead_collisions_follow_the_actually_selected_lanes(self):
        bass=signals.compare_tactic_signals(self.a,self.b,{"A":["bass"],"B":["bass"]})
        self.assertTrue(bass["bass_collision"])
        self.assertEqual(bass["status"],"rejected")
        lead=signals.compare_tactic_signals(self.a,self.b,{"A":["other"],"B":["other"]})
        self.assertTrue(lead["lead_collision"])
        self.assertGreater(lead["pitch_interval_conflict"],0.5)
        self.assertEqual(lead["status"],"rejected")

    def test_short_percussive_hits_still_count_as_two_competing_drum_foundations(self):
        result=signals.compare_tactic_signals(self.b,self.b,{"A":["drums"],"B":["drums"]})
        self.assertTrue(result["drum_collision"])
        self.assertEqual(result["status"],"rejected")


if __name__=="__main__":
    unittest.main()
