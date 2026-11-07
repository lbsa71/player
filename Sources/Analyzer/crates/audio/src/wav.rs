use mixlib_types::*;
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::Path,
};

pub struct WavReader {
    file: File,
    pub format: AudioFormat,
    data: Vec<(u64, u64)>,
    position: u64,
}
fn u16le(b: &[u8]) -> u16 {
    u16::from_le_bytes(b[..2].try_into().unwrap())
}
fn u32le(b: &[u8]) -> u32 {
    u32::from_le_bytes(b[..4].try_into().unwrap())
}
impl WavReader {
    pub fn open(path: &Path) -> Result<Self> {
        let mut file = File::open(path)?;
        let len = file.metadata()?.len();
        let mut head = [0u8; 12];
        file.read_exact(&mut head)?;
        require(
            &head[..4] == b"RIFF" && &head[8..] == b"WAVE",
            "WAV_CONTAINER",
            "expected RIFF/WAVE; RF64/RIFX and compressed containers are unsupported",
        )?;
        let end = u32le(&head[4..]) as u64 + 8;
        require(
            end == len && end >= 12,
            "WAV_TRUNCATED",
            "RIFF length differs from file length (incomplete copy or malformed header)",
        )?;
        let mut pos = 12;
        let mut fmt = None;
        let mut data = vec![];
        while pos < end {
            require(pos + 8 <= end, "WAV_CHUNK", "truncated chunk header")?;
            file.seek(SeekFrom::Start(pos))?;
            let mut ch = [0u8; 8];
            file.read_exact(&mut ch)?;
            let n = u32le(&ch[4..]) as u64;
            let start = pos + 8;
            require(
                start + n <= end,
                "WAV_TRUNCATED",
                "chunk payload exceeds RIFF bounds",
            )?;
            if &ch[..4] == b"fmt " {
                require(
                    fmt.is_none() && (16..=4096).contains(&n),
                    "WAV_FORMAT",
                    "duplicate or invalid fmt chunk",
                )?;
                let mut b = vec![0u8; n as usize];
                file.read_exact(&mut b)?;
                let mut tag = u16le(&b);
                let channels = u16le(&b[2..]);
                let rate = u32le(&b[4..]);
                let align = u16le(&b[12..]);
                let bits = u16le(&b[14..]);
                let mut valid = bits;
                let mut mask = if channels == 1 { 4 } else { 3 };
                if tag == 0xfffe {
                    require(
                        n >= 40 && u16le(&b[16..]) >= 22 && 18 + u16le(&b[16..]) as u64 <= n,
                        "WAV_EXTENSIBLE",
                        "truncated extensible header",
                    )?;
                    valid = u16le(&b[18..]);
                    mask = u32le(&b[20..]);
                    require(
                        b[28..40] == [0, 0, 16, 0, 128, 0, 0, 170, 0, 56, 155, 113],
                        "WAV_ENCODING",
                        "unknown extensible subtype GUID",
                    )?;
                    let subtype = u32le(&b[24..]);
                    require(
                        subtype == 1 || subtype == 3,
                        "WAV_ENCODING",
                        "only integer and IEEE floating-point PCM are supported",
                    )?;
                    tag = subtype as u16;
                }
                let encoding = match tag {
                    1 => Encoding::PcmInteger,
                    3 => Encoding::IeeeFloat,
                    _ => {
                        return Err(Error::new(
                            "WAV_ENCODING",
                            format!(
                                "compressed WAV format {tag} unsupported; convert a copy to PCM"
                            ),
                        ))
                    }
                };
                require(
                    match encoding {
                        Encoding::PcmInteger => [16, 24, 32].contains(&bits),
                        Encoding::IeeeFloat => [32, 64].contains(&bits) && valid == bits,
                    },
                    "WAV_ENCODING",
                    "unsupported PCM bit depth",
                )?;
                require(
                    (channels == 1 && [0, 4].contains(&mask))
                        || (channels == 2 && [0, 3].contains(&mask)),
                    "WAV_CHANNELS",
                    "only mono centre and stereo left/right layouts are supported",
                )?;
                require(
                    align == channels * (bits / 8)
                        && u32le(&b[8..]) as u64 == rate as u64 * align as u64,
                    "WAV_FORMAT",
                    "inconsistent block alignment/byte rate",
                )?;
                fmt = Some(AudioFormat {
                    sample_rate_hz: rate,
                    channels,
                    encoding,
                    container_bits: bits,
                    valid_bits: valid,
                    channel_mask: mask,
                    frames: 1,
                });
            } else if &ch[..4] == b"data" {
                data.push((start, n));
            }
            pos = start + n + (n % 2);
            require(pos <= end, "WAV_CHUNK", "missing odd-chunk padding")?;
        }
        let mut format = fmt.ok_or_else(|| Error::new("WAV_FORMAT", "missing fmt chunk"))?;
        format.validate()?;
        let align = format.channels as u64 * (format.container_bits / 8) as u64;
        require(
            !data.is_empty() && data.iter().all(|(_, n)| n % align == 0),
            "WAV_DATA",
            "missing data or partial sample frame",
        )?;
        format.frames = data.iter().map(|(_, n)| n / align).sum();
        format.validate()?;
        Ok(Self {
            file,
            format,
            data,
            position: 0,
        })
    }
    pub fn seek_frame(&mut self, frame: u64) -> Result<()> {
        require(
            frame <= self.format.frames,
            "FRAME_RANGE",
            "seek beyond WAV",
        )?;
        self.position = frame;
        Ok(())
    }
    pub fn read_frames(&mut self, count: usize) -> Result<Vec<[f32; 2]>> {
        require(
            count <= 65536,
            "BUFFER_LIMIT",
            "decode requests are limited to 65536 frames",
        )?;
        let count = count.min((self.format.frames - self.position) as usize);
        let bytes_per_sample = (self.format.container_bits / 8) as usize;
        let align = self.format.channels as usize * bytes_per_sample;
        let mut raw = vec![0u8; count * align];
        let mut left = count as u64 * align as u64;
        let mut logical = self.position * align as u64;
        let mut dst = 0;
        for &(offset, len) in &self.data {
            if logical >= len {
                logical -= len;
                continue;
            }
            let n = left.min(len - logical) as usize;
            self.file.seek(SeekFrom::Start(offset + logical))?;
            self.file.read_exact(&mut raw[dst..dst + n])?;
            left -= n as u64;
            dst += n;
            logical = 0;
            if left == 0 {
                break;
            }
        }
        let mut frames = Vec::with_capacity(count);
        for bytes in raw.chunks_exact(align) {
            let mut frame = [0.; 2];
            for ch in 0..self.format.channels as usize {
                let b = &bytes[ch * bytes_per_sample..(ch + 1) * bytes_per_sample];
                let v = match (&self.format.encoding, self.format.container_bits) {
                    (Encoding::PcmInteger, 16) => {
                        i16::from_le_bytes(b.try_into().unwrap()) as f64 / 32768.
                    }
                    (Encoding::PcmInteger, 24) => {
                        let n = ((b[0] as i32) | ((b[1] as i32) << 8) | ((b[2] as i32) << 16)) << 8
                            >> 8;
                        n as f64 / 8388608.
                    }
                    (Encoding::PcmInteger, 32) => {
                        i32::from_le_bytes(b.try_into().unwrap()) as f64 / 2147483648.
                    }
                    (Encoding::IeeeFloat, 32) => f32::from_le_bytes(b.try_into().unwrap()) as f64,
                    (Encoding::IeeeFloat, 64) => f64::from_le_bytes(b.try_into().unwrap()),
                    _ => unreachable!(),
                };
                require(
                    v.is_finite() && (v as f32).is_finite(),
                    "WAV_NON_FINITE",
                    "WAV contains non-finite or unrepresentable samples",
                )?;
                frame[ch] = v as f32;
            }
            if self.format.channels == 1 {
                frame[1] = frame[0];
            }
            frames.push(frame);
        }
        self.position += count as u64;
        Ok(frames)
    }
    /// mixlib-wav-audio-v1: prefix + LE descriptor + data payloads in file order.
    /// Descriptor fields: rate u32, channels u16, tag u16 (1/3), container/valid bits u16,
    /// canonical layout mask u32, payload length u64. Metadata and chunk padding excluded.
    pub fn hashes(&mut self, cancel: &Cancellation) -> Result<(ContentHash, ContentHash)> {
        let mut file_hasher = Sha256::new();
        self.file.seek(SeekFrom::Start(0))?;
        let mut buf = vec![0u8; 65536];
        loop {
            cancel.check()?;
            let n = self.file.read(&mut buf)?;
            if n == 0 {
                break;
            }
            file_hasher.update(&buf[..n]);
        }
        let f = &self.format;
        let mut audio = Sha256::new();
        audio.update(b"mixlib-wav-audio-v1\0");
        audio.update(f.sample_rate_hz.to_le_bytes());
        audio.update(f.channels.to_le_bytes());
        audio.update(
            (if f.encoding == Encoding::PcmInteger {
                1u16
            } else {
                3u16
            })
            .to_le_bytes(),
        );
        audio.update(f.container_bits.to_le_bytes());
        audio.update(f.valid_bits.to_le_bytes());
        let mask = if f.channel_mask == 0 {
            if f.channels == 1 {
                4u32
            } else {
                3u32
            }
        } else {
            f.channel_mask
        };
        audio.update(mask.to_le_bytes());
        audio.update(self.data.iter().map(|(_, n)| n).sum::<u64>().to_le_bytes());
        for &(offset, len) in &self.data {
            self.file.seek(SeekFrom::Start(offset))?;
            let mut left = len;
            while left > 0 {
                cancel.check()?;
                let n = left.min(buf.len() as u64) as usize;
                self.file.read_exact(&mut buf[..n])?;
                audio.update(&buf[..n]);
                left -= n as u64;
            }
        }
        Ok((
            ContentHash {
                algorithm: HashAlgorithm::Sha256,
                digest: format!("{:x}", file_hasher.finalize()),
            },
            ContentHash {
                algorithm: HashAlgorithm::Sha256,
                digest: format!("{:x}", audio.finalize()),
            },
        ))
    }
}
