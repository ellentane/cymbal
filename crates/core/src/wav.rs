use std::io::Write;
use std::path::Path;
use std::sync::Arc;

use crate::error::{Error, ErrorKind, Result, Span};
use crate::scheduler::SampleData;

fn quantize(s: f32) -> i16 {
    ((s.clamp(-1.0, 1.0) * 32768.0).round() as i32).clamp(-32768, 32767) as i16
}

fn check_len_fits(current: u32, add: usize) -> bool {
    current as u64 + add as u64 <= u32::MAX as u64
}

pub fn encode_wav(samples: &[f32], sample_rate: u32, channels: u16) -> Vec<u8> {
    let mut pcm = Vec::with_capacity(samples.len() * 2);
    for s in samples {
        pcm.extend_from_slice(&quantize(*s).to_le_bytes());
    }
    let data_len = pcm.len() as u32;
    let byte_rate = sample_rate * channels as u32 * 2;
    let block_align = channels * 2;
    let mut wav = Vec::with_capacity(44 + pcm.len());
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + data_len).to_le_bytes());
    wav.extend_from_slice(b"WAVE");
    wav.extend_from_slice(b"fmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&channels.to_le_bytes());
    wav.extend_from_slice(&sample_rate.to_le_bytes());
    wav.extend_from_slice(&byte_rate.to_le_bytes());
    wav.extend_from_slice(&block_align.to_le_bytes());
    wav.extend_from_slice(&16u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_len.to_le_bytes());
    wav.extend_from_slice(&pcm);
    wav
}

pub fn write_wav(path: &Path, samples: &[f32], sample_rate: u32) -> std::io::Result<()> {
    if !samples.len().is_multiple_of(2) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "odd sample count for stereo wav",
        ));
    }
    let mut f = std::fs::File::create(path)?;
    f.write_all(&encode_wav(samples, sample_rate, 2))?;
    Ok(())
}

pub struct WavWriter {
    file: std::fs::File,
    data_len: u32,
}

impl WavWriter {
    pub fn create(path: &Path, sample_rate: u32, channels: u16) -> std::io::Result<Self> {
        let mut file = std::fs::File::create(path)?;
        let header = encode_wav(&[], sample_rate, channels);
        file.write_all(&header)?;
        Ok(Self { file, data_len: 0 })
    }

    pub fn write_interleaved(&mut self, frames: &[f32]) -> std::io::Result<()> {
        let mut pcm = Vec::with_capacity(frames.len() * 2);
        for s in frames {
            pcm.extend_from_slice(&quantize(*s).to_le_bytes());
        }
        if !check_len_fits(self.data_len, pcm.len()) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::FileTooLarge,
                "recording exceeds wav file size limit",
            ));
        }
        self.data_len += pcm.len() as u32;
        self.file.write_all(&pcm)
    }

    pub fn finalize(mut self) -> std::io::Result<()> {
        use std::io::{Seek, SeekFrom};
        self.file.seek(SeekFrom::Start(4))?;
        self.file.write_all(&(36 + self.data_len).to_le_bytes())?;
        self.file.seek(SeekFrom::Start(40))?;
        self.file.write_all(&self.data_len.to_le_bytes())?;
        self.file.sync_all()
    }
}

fn err(message: &str) -> Error {
    Error::new(Span { line: 0, col: 0 }, ErrorKind::Eval, message)
}

pub fn decode_wav(bytes: &[u8]) -> Result<SampleData> {
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err(err("not a RIFF/WAVE file"));
    }
    let mut channels = None;
    let mut sample_rate = None;
    let mut data = None;
    let mut pos = 12usize;
    while pos + 8 <= bytes.len() {
        let id = &bytes[pos..pos + 4];
        let size = u32::from_le_bytes(bytes[pos + 4..pos + 8].try_into().unwrap()) as usize;
        let payload = pos + 8;
        match id {
            b"fmt " => {
                if size < 16 || payload + 16 > bytes.len() {
                    return Err(err("wav fmt chunk truncated"));
                }
                if u16::from_le_bytes(bytes[payload..payload + 2].try_into().unwrap()) != 1 {
                    return Err(err("unsupported wav format"));
                }
                channels = Some(u16::from_le_bytes(
                    bytes[payload + 2..payload + 4].try_into().unwrap(),
                ));
                sample_rate = Some(u32::from_le_bytes(
                    bytes[payload + 4..payload + 8].try_into().unwrap(),
                ));
                if u16::from_le_bytes(bytes[payload + 14..payload + 16].try_into().unwrap()) != 16 {
                    return Err(err("only 16-bit wav files are supported"));
                }
            }
            b"data" => {
                if payload + size > bytes.len() {
                    return Err(err("wav data chunk truncated"));
                }
                data = Some((payload, size));
            }
            _ => {}
        }
        pos = payload + size + size % 2;
    }
    let channels = channels.ok_or_else(|| err("unsupported wav format"))?;
    let sample_rate = sample_rate.ok_or_else(|| err("unsupported wav format"))?;
    let (data_start, data_len) = data.ok_or_else(|| err("wav data chunk missing"))?;
    if !data_len.is_multiple_of(2) {
        return Err(err("wav data chunk must contain whole samples"));
    }
    if channels == 2 && !(data_len / 2).is_multiple_of(2) {
        return Err(err("wav data chunk must contain whole stereo frames"));
    }
    let pcm = &bytes[data_start..data_start + data_len];
    let mut samples = Vec::with_capacity(data_len / 2);
    for chunk in pcm.chunks_exact(2) {
        let s = i16::from_le_bytes([chunk[0], chunk[1]]) as f32 / 32768.0;
        samples.push(s);
    }
    if channels == 2 {
        samples = samples.chunks(2).map(|c| (c[0] + c[1]) * 0.5).collect();
    }
    Ok(SampleData {
        frames: Arc::new(samples),
        sample_rate,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_header_and_samples() {
        let wav = encode_wav(&[0.0, 0.5, -0.5, 1.0, -1.0], 48000, 1);
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
        let wav = encode_wav(&[2.0, -2.0], 48000, 1);
        let pcm: &[u8] = &wav[44..];
        let samples: Vec<i16> = pcm
            .chunks(2)
            .map(|c| i16::from_le_bytes([c[0], c[1]]))
            .collect();
        assert_eq!(samples, vec![32767, -32768]);
    }

    #[test]
    fn decodes_mono_wav() {
        let wav = encode_wav(&[0.0, 0.5, -0.5, 1.0, -1.0], 48000, 1);
        let data = decode_wav(&wav).unwrap();
        assert_eq!(data.sample_rate, 48000);
        assert_eq!(data.frames.len(), 5);
        assert_eq!(
            data.frames.as_slice(),
            &[0.0, 0.5, -0.5, 32767.0 / 32768.0, -1.0]
        );
    }

    #[test]
    fn decodes_stereo_wav_downmixed() {
        let wav = encode_wav(&[0.5, -0.5, -0.5, 0.5], 48000, 2);
        let data = decode_wav(&wav).unwrap();
        assert_eq!(data.frames.as_slice(), &[0.0, 0.0]);
    }

    #[test]
    fn garbage_is_rejected() {
        assert!(decode_wav(b"not a wav").is_err());
        assert!(decode_wav(&[b'R', b'I', b'F', b'F', 0, 0, 0, 0]).is_err());
    }

    #[test]
    fn decode_rejects_odd_data_len() {
        let mut wav = encode_wav(&[0.0; 3], 48000, 1);
        wav[40..44].copy_from_slice(&7u32.to_le_bytes());
        wav.push(0);
        decode_wav(&wav).unwrap_err();
    }

    #[test]
    fn stereo_decode_does_not_panic_on_odd_samples() {
        assert!(decode_wav(&encode_wav(&[0.0, 0.5, -0.5], 48000, 2)).is_err());
    }

    #[test]
    fn decodes_wav_with_intervening_junk_chunk() {
        let wav = encode_wav(&[0.5, -0.5, 0.25], 48000, 1);
        let mut with_junk = Vec::new();
        with_junk.extend_from_slice(&wav[..36]);
        with_junk.extend_from_slice(b"JUNK");
        with_junk.extend_from_slice(&4u32.to_le_bytes());
        with_junk.extend_from_slice(b"abcd");
        with_junk.extend_from_slice(&wav[36..]);
        let data_len = u32::from_le_bytes(wav[40..44].try_into().unwrap());
        with_junk[4..8].copy_from_slice(&(36 + data_len + 12).to_le_bytes());
        let data = decode_wav(&with_junk).unwrap();
        assert_eq!(data.sample_rate, 48000);
        assert_eq!(data.frames.as_slice(), &[0.5, -0.5, 0.25]);
    }

    #[test]
    fn missing_fmt_chunk_is_rejected() {
        let mut wav = Vec::new();
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&36u32.to_le_bytes());
        wav.extend_from_slice(b"WAVE");
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&4u32.to_le_bytes());
        wav.extend_from_slice(&[0, 0, 0, 0]);
        let err = decode_wav(&wav).unwrap_err();
        assert_eq!(err.kind, ErrorKind::Eval);
        assert!(err.message.contains("unsupported wav format"));
    }

    #[test]
    fn short_fmt_chunk_is_rejected() {
        let mut wav = Vec::new();
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&44u32.to_le_bytes());
        wav.extend_from_slice(b"WAVE");
        wav.extend_from_slice(b"fmt ");
        wav.extend_from_slice(&4u32.to_le_bytes());
        wav.extend_from_slice(&[1, 0, 1, 0]);
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&8u32.to_le_bytes());
        wav.extend_from_slice(&[0u8; 8]);
        let err = decode_wav(&wav).unwrap_err();
        assert_eq!(err.kind, ErrorKind::Eval);
        assert!(err.message.contains("wav fmt chunk truncated"));
    }

    #[test]
    fn write_wav_rejects_odd_stereo_sample_count() {
        let path = std::env::temp_dir().join(format!("cymbal_test_odd_{}.wav", std::process::id()));
        assert!(write_wav(&path, &[0.0, 0.5, -0.5], 48000).is_err());
        assert!(!path.exists());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn wav_writer_round_trips() {
        let path = std::env::temp_dir().join(format!("cymbal_ww_{}.wav", std::process::id()));
        let mut w = WavWriter::create(&path, 48000, 1).unwrap();
        let frames: Vec<f32> = vec![0.0, 0.5, -0.5, 1.0, -1.0];
        w.write_interleaved(&frames).unwrap();
        w.finalize().unwrap();
        let bytes = std::fs::read(&path).unwrap();
        let data = decode_wav(&bytes).unwrap();
        assert_eq!(data.sample_rate, 48000);
        assert_eq!(data.frames.len(), 5);
        assert_eq!(
            data.frames.as_slice(),
            &[0.0, 0.5, -0.5, 32767.0 / 32768.0, -1.0]
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn wav_writer_patches_header_sizes() {
        let path = std::env::temp_dir().join(format!("cymbal_ww2_{}.wav", std::process::id()));
        let mut w = WavWriter::create(&path, 48000, 2).unwrap();
        w.write_interleaved(&[0.0, 0.0, 0.0, 0.0]).unwrap();
        w.finalize().unwrap();
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(&bytes[0..4], b"RIFF");
        let riff_len = u32::from_le_bytes(bytes[4..8].try_into().unwrap());
        assert_eq!(riff_len, 36 + 8, "2 frames * 2 ch * 2 bytes = 8 data bytes");
        let data_len = u32::from_le_bytes(bytes[40..44].try_into().unwrap());
        assert_eq!(data_len, 8);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn wav_writer_len_check_rejects_overflow() {
        assert!(check_len_fits(0, 0));
        assert!(check_len_fits(u32::MAX, 0));
        assert!(check_len_fits(u32::MAX - 2, 2));
        assert!(!check_len_fits(u32::MAX, 2));
        assert!(!check_len_fits(u32::MAX - 1, 2));
        assert!(check_len_fits(0, u32::MAX as usize));
        assert!(!check_len_fits(0, u32::MAX as usize + 1));
    }
}
