/// Converts decibels to linear amplitude (`-60 dB` and below smoothly clamps toward 0).
#[inline]
pub fn db_to_linear(db: f32) -> f32 {
    if db <= -80.0 {
        0.0
    } else {
        10.0f32.powf(db * 0.05)
    }
}

/// Converts linear amplitude to decibels (clamped at `-96.0 dB` for silence).
#[inline]
pub fn linear_to_db(linear: f32) -> f32 {
    if linear <= 1e-5 {
        -96.0
    } else {
        20.0 * linear.log10()
    }
}

/// Computes equal-power stereo pan multipliers `(left, right)` for `pan` in `[-1.0, 1.0]`.
#[inline]
pub fn equal_power_pan(pan: f32) -> (f32, f32) {
    let clamped = pan.clamp(-1.0, 1.0);
    let angle = (clamped + 1.0) * 0.25 * std::f32::consts::PI;
    (angle.cos() * std::f32::consts::SQRT_2, angle.sin() * std::f32::consts::SQRT_2)
}

/// Smooth soft-knee brickwall limiter preventing harsh digital clipping above `0.98`.
#[inline]
pub fn soft_limit(sample: f32) -> f32 {
    let abs = sample.abs();
    if abs <= 0.90 {
        sample
    } else {
        sample.signum() * (0.90 + 0.10 * ((abs - 0.90) / 0.10).tanh())
    }
}

/// Pre-allocated stereo audio scratch buffer used inside the real-time graph (zero callback allocs).
pub struct StereoScratchBuffer {
    pub left: Vec<f32>,
    pub right: Vec<f32>,
}

impl StereoScratchBuffer {
    pub fn new(capacity: usize) -> Self {
        Self {
            left: vec![0.0; capacity],
            right: vec![0.0; capacity],
        }
    }

    pub fn ensure_capacity(&mut self, frames: usize) {
        if self.left.len() < frames {
            self.left.resize(frames, 0.0);
            self.right.resize(frames, 0.0);
        }
    }

    #[inline]
    pub fn clear(&mut self, frames: usize) {
        let n = frames.min(self.left.len());
        self.left[..n].fill(0.0);
        self.right[..n].fill(0.0);
    }

    #[inline]
    pub fn peak(&self, frames: usize) -> (f32, f32) {
        let n = frames.min(self.left.len());
        let mut pl = 0.0f32;
        let mut pr = 0.0f32;
        for i in 0..n {
            pl = pl.max(self.left[i].abs());
            pr = pr.max(self.right[i].abs());
        }
        (pl, pr)
    }
}
