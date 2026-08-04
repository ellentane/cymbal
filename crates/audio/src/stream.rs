use std::sync::Arc;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cymbal_core::error::{Error, ErrorKind, Span};
use cymbal_core::scheduler::Timeline;

use crate::engine::Engine;
use crate::ring::AudioQueue;
use crate::ring::Msg;

pub enum AudioError {
    NoDevice,
    NoDefaultConfig,
    StreamError(String),
}

impl AudioError {
    pub fn into_error(self) -> Error {
        let message = match &self {
            AudioError::NoDevice => "no audio output device found",
            AudioError::NoDefaultConfig => "no default output config",
            AudioError::StreamError(s) => s.as_str(),
        };
        Error::new(
            Span { line: 0, col: 0 },
            ErrorKind::Audio,
            message.to_string(),
        )
    }
}

pub struct AudioHandle {
    _stream: cpal::Stream,
    pub latency_ms: Option<f32>,
}

pub fn buffer_latency_ms(config: &cpal::StreamConfig) -> Option<f32> {
    match config.buffer_size {
        cpal::BufferSize::Fixed(frames) => {
            Some(frames as f32 / config.sample_rate.0 as f32 * 1000.0)
        }
        cpal::BufferSize::Default => None,
    }
}

fn expand_to_device(scratch: &[f32], data: &mut [f32], channels: usize) {
    let frames = data.len() / channels;
    for (i, frame) in scratch.chunks(2).take(frames).enumerate() {
        let l = frame[0];
        let r = frame[1];
        data[i * channels] = l;
        data[i * channels + 1] = r;
        for ch in 2..channels {
            data[i * channels + ch] = if ch % 2 == 0 { l } else { r };
        }
    }
}

pub fn start_audio(
    queue: Arc<AudioQueue>,
    initial: Arc<Timeline>,
    on_error: impl Fn(Error) + Send + 'static,
) -> Result<AudioHandle, AudioError> {
    let host = cpal::default_host();
    let device = host.default_output_device().ok_or(AudioError::NoDevice)?;
    let config = device
        .default_output_config()
        .map_err(|_| AudioError::NoDefaultConfig)?;
    let stream_config: cpal::StreamConfig = config.into();

    let mut engine = Engine::new(initial.tempo, stream_config.sample_rate.0);
    engine.submit_swap(initial);

    let err_cb = move |e: cpal::StreamError| {
        on_error(Error::new(
            Span { line: 0, col: 0 },
            ErrorKind::Audio,
            e.to_string(),
        ))
    };
    let channels = stream_config.channels as usize;
    let mut scratch = vec![0.0f32; stream_config.sample_rate.0 as usize * 2];
    let stream = device
        .build_output_stream(
            &stream_config,
            move |data: &mut [f32], _| {
                while let Some(msg) = queue.try_recv() {
                    match msg {
                        Msg::Swap(tl) => engine.submit_swap(tl),
                        Msg::RecordStart(rec) => engine.start_recording(rec),
                        Msg::RecordStop => engine.stop_recording(),
                        Msg::Shutdown => {
                            data.fill(0.0);
                            return;
                        }
                    }
                }
                let frames = data.len() / channels;
                if frames > scratch.len() / 2 {
                    scratch.resize(frames * 2, 0.0);
                }
                engine.process(&mut scratch[..frames * 2]);
                expand_to_device(&scratch, data, channels);
            },
            err_cb,
            None,
        )
        .map_err(|e| AudioError::StreamError(e.to_string()))?;

    stream
        .play()
        .map_err(|e| AudioError::StreamError(e.to_string()))?;
    Ok(AudioHandle {
        _stream: stream,
        latency_ms: buffer_latency_ms(&stream_config),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_cpal_errors_to_audio_kind() {
        let e = AudioError::NoDevice;
        assert_eq!(e.into_error().kind, ErrorKind::Audio);
    }

    #[test]
    fn buffer_latency_ms_from_fixed_buffer() {
        let cfg = cpal::StreamConfig {
            channels: 1,
            sample_rate: cpal::SampleRate(48000),
            buffer_size: cpal::BufferSize::Fixed(256),
        };
        assert_eq!(buffer_latency_ms(&cfg), Some(5.3333335));
    }

    #[test]
    fn buffer_latency_ms_none_for_default_buffer() {
        let cfg = cpal::StreamConfig {
            channels: 1,
            sample_rate: cpal::SampleRate(48000),
            buffer_size: cpal::BufferSize::Default,
        };
        assert_eq!(buffer_latency_ms(&cfg), None);
    }

    #[test]
    fn expands_stereo_to_device_channels() {
        let scratch = [0.1, 0.2, 0.3, 0.4];
        let mut data = [0.0f32; 8];
        expand_to_device(&scratch, &mut data, 4);
        assert_eq!(data, [0.1, 0.2, 0.1, 0.2, 0.3, 0.4, 0.3, 0.4]);
    }

    #[test]
    fn stereo_fits_two_channel_device() {
        let scratch = [0.1, 0.2];
        let mut data = [0.0f32; 2];
        expand_to_device(&scratch, &mut data, 2);
        assert_eq!(data, [0.1, 0.2]);
    }
}
