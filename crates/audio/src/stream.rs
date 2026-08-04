use std::sync::Arc;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cymbal_core::error::{Error, ErrorKind, Span};
use cymbal_core::scheduler::Timeline;
use cymbal_core::transport::Transport;

use crate::engine::Engine;
use crate::resampler::Resampler;
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
    pub device_rate: u32,
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
        for ch in 0..channels {
            data[i * channels + ch] = if ch % 2 == 0 { frame[0] } else { frame[1] };
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

    let mut engine = Engine::new(initial.tempo, Transport::SAMPLE_RATE);
    engine.submit_swap(initial, 1);

    let err_cb = move |e: cpal::StreamError| {
        on_error(Error::new(
            Span { line: 0, col: 0 },
            ErrorKind::Audio,
            e.to_string(),
        ))
    };
    let channels = stream_config.channels as usize;
    let device_rate = stream_config.sample_rate.0;
    let mut resampler = Resampler::new(device_rate);
    let mut scratch = vec![0.0f32; Transport::SAMPLE_RATE as usize * 2];
    let mut stereo = vec![0.0f32; stream_config.sample_rate.0 as usize * 2];
    let stream = device
        .build_output_stream(
            &stream_config,
            move |data: &mut [f32], _| {
                while let Some(msg) = queue.try_recv() {
                    match msg {
                        Msg::Swap(tl, seq) => engine.submit_swap(tl, seq),
                        Msg::RecordStart(rec) => engine.start_recording(rec),
                        Msg::RecordStop => engine.stop_recording(),
                        Msg::Shutdown => {
                            data.fill(0.0);
                            return;
                        }
                    }
                }
                let frames = data.len() / channels;
                let needed = resampler
                    .frames_needed(frames)
                    .saturating_sub(resampler.buffered_frames());
                if needed > 0 {
                    if needed * 2 > scratch.len() {
                        scratch.resize(needed * 2, 0.0);
                    }
                    engine.process(&mut scratch[..needed * 2]);
                    resampler.push(&scratch[..needed * 2]);
                }
                if frames * 2 > stereo.len() {
                    stereo.resize(frames * 2, 0.0);
                }
                resampler.process(&mut stereo[..frames * 2]);
                expand_to_device(&stereo[..frames * 2], data, channels);
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
        device_rate,
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

    #[test]
    fn stereo_fits_mono_device() {
        let scratch = [0.1, 0.9, 0.2, 0.8];
        let mut data = [0.0f32; 2];
        expand_to_device(&scratch, &mut data, 1);
        assert_eq!(data, [0.1, 0.2]);
    }

    #[test]
    fn engine_through_resampler_places_hits_on_bar_grid() {
        use crate::engine::Engine;
        use crate::resampler::Resampler;
        use cymbal_core::ast::VoiceKind;
        use cymbal_core::scheduler::{Event, Timeline};
        use std::sync::Arc;

        let mut engine = Engine::new(120.0, 48000);
        let tl = Arc::new(Timeline {
            events: vec![
                Event {
                    sample_offset: 0,
                    loop_name: "b".into(),
                    voice: VoiceKind::Hat,
                    pitch: None,
                    semitone: 0,
                    velocity: 1.0,
                    duration: 2400,
                    generation: 0,
                    pan: 0.0,
                    delay_send: 0.0,
                    reverb_send: 0.0,
                    sample: None,
                },
                Event {
                    sample_offset: 24000,
                    loop_name: "b".into(),
                    voice: VoiceKind::Hat,
                    pitch: None,
                    semitone: 0,
                    velocity: 1.0,
                    duration: 2400,
                    generation: 0,
                    pan: 0.0,
                    delay_send: 0.0,
                    reverb_send: 0.0,
                    sample: None,
                },
            ],
            generation: 0,
            tempo: 120.0,
            bar_samples: 24000,
            sample_rate: 48000,
            loops: vec!["b".into()],
            loop_generations: vec![("b".into(), 0)],
        });
        engine.submit_swap(tl, 1);

        let mut resampler = Resampler::new(44100);
        let mut scratch = vec![0.0f32; resampler.frames_needed(44100) * 2];
        engine.process(&mut scratch);
        resampler.push(&scratch);
        let mut out = vec![0.0f32; 44100 * 2];
        resampler.process(&mut out);

        assert!(
            out[0..16].iter().any(|s| *s != 0.0),
            "first hit at device frame 0"
        );
        assert!(
            out[10000 * 2..20000 * 2].iter().all(|s| *s == 0.0),
            "silence between hits"
        );
        assert!(
            out[22050 * 2..22050 * 2 + 16].iter().any(|s| *s != 0.0),
            "second hit lands at 22050 = 24000 * 44100 / 48000"
        );
        assert!(out.iter().all(|s| s.is_finite()));
    }

    #[test]
    fn repeats_stereo_across_device_channels() {
        for channels in 1..=6 {
            let scratch = [0.1, 0.2, 0.3, 0.4, 0.5, 0.6];
            let mut data = vec![0.0f32; 3 * channels];
            expand_to_device(&scratch, &mut data, channels);
            for frame in 0..3 {
                for ch in 0..channels {
                    let expected = if ch % 2 == 0 {
                        scratch[frame * 2]
                    } else {
                        scratch[frame * 2 + 1]
                    };
                    assert_eq!(data[frame * channels + ch], expected);
                }
            }
        }
    }
}
