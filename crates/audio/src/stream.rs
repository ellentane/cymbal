use std::sync::Arc;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cymbal_core::error::{Error, ErrorKind, Span};
use cymbal_core::scheduler::Timeline;

use crate::engine::Engine;
use crate::ring::AudioQueue;

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
    let mut scratch = vec![0.0f32; stream_config.sample_rate.0 as usize];
    let stream = device
        .build_output_stream(
            &stream_config,
            move |data: &mut [f32], _| {
                while let Some(msg) = queue.try_recv() {
                    match msg {
                        crate::ring::Msg::Swap(tl) => engine.submit_swap(tl),
                        crate::ring::Msg::Shutdown => {
                            data.fill(0.0);
                            return;
                        }
                    }
                }
                let frames = data.len() / channels;
                if frames > scratch.len() {
                    scratch.resize(frames, 0.0);
                }
                engine.process(&mut scratch[..frames]);
                for (i, frame) in scratch[..frames].iter().enumerate() {
                    for out in data[i * channels..(i + 1) * channels].iter_mut() {
                        *out = *frame;
                    }
                }
            },
            err_cb,
            None,
        )
        .map_err(|e| AudioError::StreamError(e.to_string()))?;

    stream
        .play()
        .map_err(|e| AudioError::StreamError(e.to_string()))?;
    Ok(AudioHandle { _stream: stream })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_cpal_errors_to_audio_kind() {
        let e = AudioError::NoDevice;
        assert_eq!(e.into_error().kind, ErrorKind::Audio);
    }
}
