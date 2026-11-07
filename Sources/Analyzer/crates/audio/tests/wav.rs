#[path = "../../../tests/support.rs"]
mod support;
use mixlib_audio::*;
use mixlib_types::*;
#[test]
fn format_matrix_and_metadata_independence() {
    let dir = tempfile::tempdir().unwrap();
    for rate in [44100, 48000, 88200, 96000, 192000] {
        for channels in [1, 2] {
            for (float, bits) in [
                (false, 16),
                (false, 24),
                (false, 32),
                (true, 32),
                (true, 64),
            ] {
                for extensible in [false, true] {
                    let a = dir.path().join("a.wav");
                    let b = dir.path().join("b.wav");
                    let sample = |i: usize, c: usize| {
                        if i % 2 == 0 {
                            0.25 / (c + 1) as f64
                        } else {
                            -0.25 / (c + 1) as f64
                        }
                    };
                    support::wav(
                        &a, rate, channels, bits, float, extensible, 31, b"one", sample,
                    );
                    support::wav(
                        &b,
                        rate,
                        channels,
                        bits,
                        float,
                        extensible,
                        31,
                        b"different metadata",
                        sample,
                    );
                    let mut a = WavReader::open(&a).unwrap();
                    let mut b = WavReader::open(&b).unwrap();
                    assert_eq!(a.format.frames, 31);
                    let ah = a.hashes(&Cancellation::default()).unwrap();
                    let bh = b.hashes(&Cancellation::default()).unwrap();
                    assert_ne!(ah.0, bh.0);
                    assert_eq!(ah.1, bh.1);
                    let frames = a.read_frames(64).unwrap();
                    assert_eq!(frames.len(), 31);
                    assert!((frames[0][0] - 0.25).abs() < 1e-6);
                    assert!((frames[1][0] + 0.25).abs() < 1e-6);
                }
            }
        }
    }
}
#[test]
fn reject_nonfinite_truncated_compressed_and_layout() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("a.wav");
    support::wav(&p, 48000, 1, 64, true, false, 4, b"", |_, _| f64::NAN);
    assert_eq!(
        WavReader::open(&p)
            .unwrap()
            .read_frames(4)
            .unwrap_err()
            .code,
        "WAV_NON_FINITE"
    );
    support::tone(&p, 0.01, 440.);
    let mut bytes = std::fs::read(&p).unwrap();
    bytes.pop();
    std::fs::write(&p, &bytes).unwrap();
    assert!(WavReader::open(&p).is_err());
    support::tone(&p, 0.01, 440.);
    let mut bytes = std::fs::read(&p).unwrap();
    bytes[20] = 6;
    std::fs::write(&p, &bytes).unwrap();
    assert!(WavReader::open(&p).is_err());
    support::wav(&p, 48000, 4, 16, false, false, 4, b"", |_, _| 0.);
    assert!(WavReader::open(&p).is_err());
}
