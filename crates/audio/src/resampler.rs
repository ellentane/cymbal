pub struct Resampler {
    step: f64,
    n0: u64,
    consumed: usize,
    buf: Vec<f32>,
}

impl Resampler {
    pub fn new(device_rate: u32) -> Self {
        Self {
            step: 48000.0 / device_rate.max(1) as f64,
            n0: 0,
            consumed: 0,
            buf: Vec::new(),
        }
    }

    pub fn frames_needed(&self, out_frames: usize) -> usize {
        if out_frames == 0 {
            return 0;
        }
        ((self.n0 + out_frames as u64 - 1) as f64 * self.step).floor() as usize + 2 - self.consumed
    }

    pub fn buffered_frames(&self) -> usize {
        self.buf.len() / 2
    }

    pub fn push(&mut self, src: &[f32]) {
        self.buf.extend_from_slice(src);
    }

    pub fn process(&mut self, out: &mut [f32]) {
        let m = out.len() / 2;
        let frames = self.buf.len() / 2;
        for k in 0..m {
            let p = (self.n0 + k as u64) as f64 * self.step;
            let i = (p.floor() as usize).saturating_sub(self.consumed);
            let frac = (p - p.floor()) as f32;
            let a = frame_at(i, &self.buf, frames);
            let b = frame_at(i + 1, &self.buf, frames);
            out[k * 2] = a[0] + (b[0] - a[0]) * frac;
            out[k * 2 + 1] = a[1] + (b[1] - a[1]) * frac;
        }
        self.n0 += m as u64;
        let new_consumed = (self.n0 as f64 * self.step).floor() as usize;
        let drain = new_consumed - self.consumed;
        if drain >= frames {
            self.buf.clear();
        } else if drain > 0 {
            self.buf.drain(..drain * 2);
        }
        self.consumed = new_consumed;
    }
}

fn frame_at(i: usize, buf: &[f32], frames: usize) -> [f32; 2] {
    if frames == 0 {
        return [0.0, 0.0];
    }
    let j = i.min(frames - 1);
    [buf[j * 2], buf[j * 2 + 1]]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ramp() -> Vec<f32> {
        // one second of 48k stereo ramp: L = t, R = -t
        let mut v = Vec::with_capacity(48000 * 2);
        for i in 0..48000 {
            v.push(i as f32 / 48000.0);
            v.push(-(i as f32) / 48000.0);
        }
        v
    }

    #[test]
    fn passthrough_is_exact_at_48000() {
        let mut r = Resampler::new(48000);
        let src = ramp();
        r.push(&src);
        let mut out = vec![0.0f32; 48000 * 2];
        r.process(&mut out);
        assert_eq!(out, src, "48k device must be bit-identical");
    }

    #[test]
    fn downsample_interpolates_linearly() {
        // 44.1k out of 48k: step = 160/147; output frame k sits at 48k position k*160/147.
        // k = 22050 -> position exactly 24000 -> value 0.5 on L, -0.5 on R.
        let mut r = Resampler::new(44100);
        r.push(&ramp());
        let mut out = vec![0.0f32; 44100 * 2];
        r.process(&mut out);
        assert_eq!(out[0], 0.0, "first output frame = first input frame");
        assert_eq!(out[22050 * 2], 0.5);
        assert_eq!(out[22050 * 2 + 1], -0.5);
        let last = (44100 - 1) as f64 * 160.0 / 147.0; // 47989.8 -> value ~0.9998
        assert!((out[(44100 - 1) * 2] - (last / 48000.0) as f32).abs() < 1e-4);
    }

    #[test]
    fn dc_passthrough() {
        let mut r = Resampler::new(44100);
        r.push(&vec![0.5f32; 48000 * 2]);
        let mut out = vec![0.0f32; 44100 * 2];
        r.process(&mut out);
        assert!(out.iter().all(|s| *s == 0.5));
    }

    #[test]
    fn output_is_identical_across_block_boundaries() {
        let mut src = Vec::new();
        for i in 0..96000 {
            let l = (i % 97) as f32 / 97.0 + (i / 48000) as f32 * 0.25;
            src.push(l);
            src.push(-l);
        }
        // one block
        let mut r = Resampler::new(44100);
        r.push(&src);
        let mut full = vec![0.0f32; 88200 * 2];
        r.process(&mut full);
        // two blocks, incremental pushes
        let mut r2 = Resampler::new(44100);
        let mut o1 = vec![0.0f32; 30000 * 2];
        let n1 = r2.frames_needed(30000);
        r2.push(&src[..n1 * 2]);
        r2.process(&mut o1);
        let stream_pos = n1;
        let mut o2 = vec![0.0f32; 58200 * 2];
        let n2 = r2.frames_needed(58200);
        r2.push(&src[stream_pos * 2..(stream_pos + n2) * 2]);
        r2.process(&mut o2);
        let mut split = o1;
        split.extend_from_slice(&o2);
        assert_eq!(split.len(), full.len());
        assert_eq!(split, full, "block splits must not change output");
    }
}
