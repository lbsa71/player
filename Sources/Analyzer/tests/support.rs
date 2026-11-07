#![allow(dead_code, clippy::too_many_arguments)]
use std::{fs, io::Write, path::Path};
/// CC0 synthetic fixture generator; no recordings or copyrighted samples.
pub fn wav(
    path: &Path,
    rate: u32,
    channels: u16,
    bits: u16,
    float: bool,
    extensible: bool,
    frames: usize,
    metadata: &[u8],
    sample: impl Fn(usize, usize) -> f64,
) {
    let align = channels * (bits / 8);
    let tag = if float { 3u16 } else { 1 };
    let mut fmt = vec![];
    fmt.extend(if extensible { 0xfffeu16 } else { tag }.to_le_bytes());
    fmt.extend(channels.to_le_bytes());
    fmt.extend(rate.to_le_bytes());
    fmt.extend((rate * align as u32).to_le_bytes());
    fmt.extend(align.to_le_bytes());
    fmt.extend(bits.to_le_bytes());
    if extensible {
        fmt.extend(22u16.to_le_bytes());
        fmt.extend(bits.to_le_bytes());
        fmt.extend((if channels == 1 { 4u32 } else { 3 }).to_le_bytes());
        fmt.extend((tag as u32).to_le_bytes());
        fmt.extend([0, 0, 16, 0, 128, 0, 0, 170, 0, 56, 155, 113]);
    }
    let mut payload = vec![];
    for frame in 0..frames {
        for channel in 0..channels as usize {
            let s = sample(frame, channel);
            match (float, bits) {
                (true, 32) => payload.extend((s as f32).to_le_bytes()),
                (true, 64) => payload.extend(s.to_le_bytes()),
                (false, 16) => payload.extend(((s * 32768.) as i16).to_le_bytes()),
                (false, 24) => payload.extend(&((s * 8388608.) as i32).to_le_bytes()[..3]),
                (false, 32) => payload.extend(((s * 2147483648.) as i32).to_le_bytes()),
                _ => panic!("test format"),
            }
        }
    }
    let mut chunks = vec![];
    chunk(&mut chunks, b"fmt ", &fmt);
    if !metadata.is_empty() {
        chunk(&mut chunks, b"bext", metadata);
    }
    chunk(&mut chunks, b"data", &payload);
    let mut f = fs::File::create(path).unwrap();
    f.write_all(b"RIFF").unwrap();
    f.write_all(&(chunks.len() as u32 + 4).to_le_bytes())
        .unwrap();
    f.write_all(b"WAVE").unwrap();
    f.write_all(&chunks).unwrap();
}
fn chunk(out: &mut Vec<u8>, id: &[u8; 4], data: &[u8]) {
    out.extend(id);
    out.extend((data.len() as u32).to_le_bytes());
    out.extend(data);
    if data.len() % 2 == 1 {
        out.push(0);
    }
}
pub fn tone(path: &Path, seconds: f64, freq: f64) {
    let rate = 8000;
    wav(
        path,
        rate,
        2,
        32,
        true,
        false,
        (seconds * rate as f64) as usize,
        b"CC0",
        |i, c| {
            0.15 * (2. * std::f64::consts::PI * freq * i as f64 / rate as f64).sin()
                * if c == 0 { 1. } else { 0.8 }
        },
    );
}
