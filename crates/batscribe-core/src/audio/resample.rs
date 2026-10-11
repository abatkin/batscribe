//! Streaming sample-rate conversion for speech: a windowed-sinc low-pass (when downsampling)
//! followed by linear interpolation. Adequate for recognition; not a mastering-grade resampler.

pub struct Resampler {
    ratio: f64,
    taps: Vec<f32>,
    history: Vec<f32>,
    /// Filtered input not yet consumed by interpolation.
    buffer: Vec<f32>,
    /// Fractional read position into `buffer`.
    pos: f64,
}

impl Resampler {
    pub fn new(from: u32, to: u32) -> Self {
        let ratio = f64::from(from.max(1)) / f64::from(to.max(1));
        let taps = if ratio > 1.0 {
            // Cut off a little below the output Nyquist frequency.
            let cutoff = 0.45 / ratio;
            let n = 63;
            let mid = (n - 1) as f64 / 2.0;
            let mut taps: Vec<f32> = (0..n)
                .map(|i| {
                    let x = i as f64 - mid;
                    let sinc = if x == 0.0 {
                        2.0 * cutoff
                    } else {
                        (2.0 * std::f64::consts::PI * cutoff * x).sin() / (std::f64::consts::PI * x)
                    };
                    let window =
                        0.5 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / (n - 1) as f64).cos();
                    (sinc * window) as f32
                })
                .collect();
            let sum: f32 = taps.iter().sum();
            taps.iter_mut().for_each(|t| *t /= sum);
            taps
        } else {
            vec![1.0]
        };
        Self {
            ratio,
            history: vec![0.0; taps.len() - 1],
            taps,
            buffer: Vec::new(),
            pos: 0.0,
        }
    }

    pub fn process(&mut self, input: &[f32]) -> Vec<f32> {
        if (self.ratio - 1.0).abs() < f64::EPSILON {
            return input.to_vec();
        }
        for &x in input {
            self.history.push(x);
            let start = self.history.len() - self.taps.len();
            let y: f32 = self.history[start..]
                .iter()
                .zip(&self.taps)
                .map(|(a, b)| a * b)
                .sum();
            self.buffer.push(y);
        }
        let keep = self.taps.len() - 1;
        if self.history.len() > keep {
            let drop = self.history.len() - keep;
            self.history.drain(..drop);
        }
        let mut out = Vec::with_capacity((input.len() as f64 / self.ratio) as usize + 1);
        while self.pos + 1.0 < self.buffer.len() as f64 {
            let i = self.pos as usize;
            let frac = (self.pos - i as f64) as f32;
            out.push(self.buffer[i] * (1.0 - frac) + self.buffer[i + 1] * frac);
            self.pos += self.ratio;
        }
        let consumed = self.pos as usize;
        self.buffer.drain(..consumed.min(self.buffer.len()));
        self.pos -= consumed as f64;
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_rate_and_preserves_low_tones() {
        let mut r = Resampler::new(48_000, 16_000);
        let tone: Vec<f32> = (0..48_000)
            .map(|i| (2.0 * std::f32::consts::PI * 440.0 * i as f32 / 48_000.0).sin() * 0.5)
            .collect();
        let mut out = Vec::new();
        for chunk in tone.chunks(480) {
            out.extend(r.process(chunk));
        }
        assert!((15_990..=16_000).contains(&out.len()), "{}", out.len());
        let peak = out[1000..].iter().fold(0.0f32, |m, s| m.max(s.abs()));
        assert!((0.45..0.55).contains(&peak), "{peak}");
        // A tone above the output Nyquist frequency is strongly attenuated.
        let mut r = Resampler::new(48_000, 16_000);
        let high: Vec<f32> = (0..48_000)
            .map(|i| (2.0 * std::f32::consts::PI * 12_000.0 * i as f32 / 48_000.0).sin() * 0.5)
            .collect();
        let out = r.process(&high);
        let peak = out[1000..].iter().fold(0.0f32, |m, s| m.max(s.abs()));
        assert!(peak < 0.05, "{peak}");
    }

    #[test]
    fn same_rate_passes_through() {
        let mut r = Resampler::new(16_000, 16_000);
        assert_eq!(r.process(&[0.1, 0.2]), vec![0.1, 0.2]);
    }
}
