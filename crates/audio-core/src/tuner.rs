/// Latest pitch reading produced by the real-time stage tuner.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct TunerReading {
    pub active: bool,
    pub freq_hz: f32,
    pub midi_note: i16,
    pub cents: f32,
}

const NOTE_NAMES: [&str; 12] = [
    "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
];

pub fn format_midi_note(midi_note: i16) -> String {
    let idx = midi_note.rem_euclid(12) as usize;
    let octave = midi_note.div_euclid(12) - 1;
    format!("{}{}", NOTE_NAMES[idx], octave)
}

/// Real-time lock-free monophonic chromatic pitch detector using Normalized Square Difference (NSDF).
pub struct StageTuner {
    sample_rate: f64,
    buf: Vec<f32>,
    nsdf: Vec<f32>,
    write_pos: usize,
    last_reading: TunerReading,
}

impl StageTuner {
    const WINDOW_LEN: usize = 2048;

    pub fn new(sample_rate: f64) -> Self {
        Self {
            sample_rate: sample_rate.max(8_000.0),
            buf: vec![0.0; Self::WINDOW_LEN],
            nsdf: vec![0.0; Self::WINDOW_LEN / 2 + 1],
            write_pos: 0,
            last_reading: TunerReading::default(),
        }
    }

    pub fn set_sample_rate(&mut self, sample_rate: f64) {
        self.sample_rate = sample_rate.max(8_000.0);
        self.reset();
    }

    pub fn reset(&mut self) {
        self.buf.fill(0.0);
        self.nsdf.fill(0.0);
        self.write_pos = 0;
        self.last_reading = TunerReading::default();
    }

    pub fn reading(&self) -> TunerReading {
        self.last_reading
    }

    pub fn push_samples(&mut self, left: &[f32], right: &[f32]) -> TunerReading {
        let frames = left.len().min(right.len());
        for i in 0..frames {
            self.buf[self.write_pos] = 0.5 * (left[i] + right[i]);
            self.write_pos += 1;
            if self.write_pos >= self.buf.len() {
                self.write_pos = 0;
                self.last_reading = self.analyze_window();
            }
        }
        self.last_reading
    }

    fn analyze_window(&mut self) -> TunerReading {
        let n = self.buf.len();
        let energy: f32 = self.buf.iter().map(|&x| x * x).sum();
        let rms = (energy / (n as f32)).sqrt();
        if rms < 0.008 {
            return TunerReading::default();
        }

        let min_tau = ((self.sample_rate / 1200.0) as usize).max(8);
        let max_tau = ((self.sample_rate / 55.0) as usize).min(n / 2 - 1);
        if min_tau >= max_tau {
            return TunerReading::default();
        }

        let mut max_nsdf = 0.0f32;
        for tau in min_tau..=max_tau {
            let mut acf = 0.0f64;
            let mut m = 0.0f64;
            let limit = n - tau;
            for i in 0..limit {
                let a = self.buf[i] as f64;
                let b = self.buf[i + tau] as f64;
                acf += a * b;
                m += a * a + b * b;
            }
            let val = if m > 1e-12 { (2.0 * acf / m) as f32 } else { 0.0 };
            self.nsdf[tau] = val;
            if val > max_nsdf {
                max_nsdf = val;
            }
        }

        if max_nsdf < 0.65 {
            return TunerReading::default();
        }

        let threshold = max_nsdf * 0.82;
        let peak_tau = find_first_major_peak(&self.nsdf[..=max_tau], min_tau, max_tau, threshold);
        let Some(tau) = peak_tau else {
            return TunerReading::default();
        };

        let refined_tau = parabolic_peak(&self.nsdf, tau, min_tau, max_tau);
        let freq_hz = (self.sample_rate / refined_tau) as f32;
        let midi_f = 69.0 + 12.0 * (freq_hz / 440.0).log2();
        let midi_note = midi_f.round() as i16;
        let cents = (midi_f - (midi_note as f32)) * 100.0;

        TunerReading {
            active: true,
            freq_hz,
            midi_note,
            cents,
        }
    }
}

fn find_first_major_peak(nsdf: &[f32], min_tau: usize, max_tau: usize, threshold: f32) -> Option<usize> {
    let mut tau = min_tau;
    // Skip only a descending zero-lag tail if min_tau falls inside the lag-0 lobe.
    while tau + 1 < max_tau && nsdf[tau] > 0.0 && nsdf[tau + 1] < nsdf[tau] {
        tau += 1;
    }
    while tau < max_tau {
        if nsdf[tau] >= threshold {
            let mut best = tau;
            while tau < max_tau && nsdf[tau + 1] >= 0.0 {
                tau += 1;
                if nsdf[tau] > nsdf[best] {
                    best = tau;
                }
            }
            return Some(best);
        }
        tau += 1;
    }
    None
}

fn parabolic_peak(nsdf: &[f32], tau: usize, min_tau: usize, max_tau: usize) -> f64 {
    if tau <= min_tau || tau >= max_tau {
        return tau as f64;
    }
    let y0 = nsdf[tau - 1] as f64;
    let y1 = nsdf[tau] as f64;
    let y2 = nsdf[tau + 1] as f64;
    let denom = 2.0 * (2.0 * y1 - y0 - y2);
    if denom.abs() < 1e-12 {
        return tau as f64;
    }
    let delta = (y2 - y0) / denom;
    (tau as f64) + delta.clamp(-0.5, 0.5)
}

#[cfg(test)]
mod tests {
    use std::f32::consts::TAU;

    use super::*;

    #[test]
    fn tuner_detects_a440_and_low_e_accurately() {
        let mut tuner = StageTuner::new(48_000.0);
        let mut buf = vec![0.0f32; 2048];
        for (i, sample) in buf.iter_mut().enumerate() {
            *sample = 0.25 * (TAU * 440.0 * (i as f32) / 48_000.0).sin();
        }
        let a4 = tuner.push_samples(&buf, &buf);
        assert!(a4.active);
        assert_eq!(format_midi_note(a4.midi_note), "A4");
        assert!(a4.cents.abs() < 3.0, "expected ~0 cents on A4, got {}", a4.cents);

        tuner.reset();
        for (i, sample) in buf.iter_mut().enumerate() {
            *sample = 0.25 * (TAU * 82.4069 * (i as f32) / 48_000.0).sin();
        }
        let e2 = tuner.push_samples(&buf, &buf);
        assert!(e2.active);
        assert_eq!(format_midi_note(e2.midi_note), "E2");
        assert!(e2.cents.abs() < 5.0, "expected <5 cents on E2, got {}", e2.cents);
    }
}
