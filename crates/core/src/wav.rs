use std::io::Write;
use std::path::Path;

pub fn encode_wav(samples: &[f32], sample_rate: u32) -> Vec<u8> {
    let mut pcm = Vec::with_capacity(samples.len() * 2);
    for s in samples {
        // scale by 32768 then clamp to i16 range so 1.0 -> 32767 and -1.0 -> -32768
        let v = ((s.clamp(-1.0, 1.0) * 32768.0).round() as i32).clamp(-32768, 32767) as i16;
        pcm.extend_from_slice(&v.to_le_bytes());
    }
    let data_len = pcm.len() as u32;
    let mut wav = Vec::with_capacity(44 + pcm.len());
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + data_len).to_le_bytes());
    wav.extend_from_slice(b"WAVE");
    wav.extend_from_slice(b"fmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&sample_rate.to_le_bytes());
    wav.extend_from_slice(&(sample_rate * 2).to_le_bytes());
    wav.extend_from_slice(&2u16.to_le_bytes());
    wav.extend_from_slice(&16u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_len.to_le_bytes());
    wav.extend_from_slice(&pcm);
    wav
}

pub fn write_wav(path: &Path, samples: &[f32], sample_rate: u32) -> std::io::Result<()> {
    let mut f = std::fs::File::create(path)?;
    f.write_all(&encode_wav(samples, sample_rate))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_header_and_samples() {
        let wav = encode_wav(&[0.0, 0.5, -0.5, 1.0, -1.0], 48000);
        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(&wav[8..12], b"WAVE");
        assert_eq!(&wav[12..16], b"fmt ");
        let sample_rate = u32::from_le_bytes(wav[24..28].try_into().unwrap());
        assert_eq!(sample_rate, 48000);
        let pcm: &[u8] = &wav[44..];
        assert_eq!(pcm.len(), 10);
        let samples: Vec<i16> = pcm
            .chunks(2)
            .map(|c| i16::from_le_bytes([c[0], c[1]]))
            .collect();
        assert_eq!(samples, vec![0, 16384, -16384, 32767, -32768]);
    }

    #[test]
    fn clamps_out_of_range() {
        let wav = encode_wav(&[2.0, -2.0], 48000);
        let pcm: &[u8] = &wav[44..];
        let samples: Vec<i16> = pcm
            .chunks(2)
            .map(|c| i16::from_le_bytes([c[0], c[1]]))
            .collect();
        assert_eq!(samples, vec![32767, -32768]);
    }
}
