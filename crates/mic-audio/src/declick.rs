use std::f64::consts::PI;

/// Smooth de-click and continuity processor for 16 kHz 160-sample PCM frames.
/// Transition window: 48 samples (3.0 ms at 16,000 Hz).
pub const FADE_SAMPLES: usize = 48;

#[derive(Clone, Debug, Default)]
pub struct Declicker {
    pub last_sample: i16,
    pub in_underrun: bool,
    pub fade_outs: u64,
    pub fade_ins: u64,
}

impl Declicker {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn reset(&mut self) {
        self.last_sample = 0;
        self.in_underrun = false;
    }

    /// Process a 160-sample frame.
    /// `is_missing` indicates whether this frame slot suffered a playout deadline miss / underrun.
    pub fn process(&mut self, frame: [i16; 160], is_missing: bool) -> [i16; 160] {
        self.process_frame(frame, is_missing, 160)
    }
    pub fn process_frame(
        &mut self,
        frame: [i16; 160],
        is_missing: bool,
        samples: usize,
    ) -> [i16; 160] {
        assert!(matches!(samples, 80 | 160));
        let mut out = frame;
        if is_missing {
            if !self.in_underrun {
                self.in_underrun = true;
                if self.last_sample != 0 {
                    self.fade_outs += 1;
                    for (i, out_sample) in out.iter_mut().enumerate().take(FADE_SAMPLES) {
                        let phase = PI * ((i + 1) as f64) / (FADE_SAMPLES as f64);
                        let w = 0.5 * (1.0 + phase.cos());
                        let val = (self.last_sample as f64 * w).round();
                        *out_sample = val.clamp(i16::MIN as f64, i16::MAX as f64) as i16;
                    }
                    out[FADE_SAMPLES..].fill(0);
                    self.last_sample = 0;
                    return out;
                }
            }
            out.fill(0);
            self.last_sample = 0;
            return out;
        }

        if self.in_underrun {
            self.in_underrun = false;
            self.fade_ins += 1;
            for (i, out_sample) in out.iter_mut().enumerate().take(FADE_SAMPLES) {
                let phase = PI * ((i + 1) as f64) / (FADE_SAMPLES as f64);
                let w = 0.5 * (1.0 - phase.cos());
                let val = (*out_sample as f64 * w).round();
                *out_sample = val.clamp(i16::MIN as f64, i16::MAX as f64) as i16;
            }
        }

        self.last_sample = out[samples - 1];
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn continuous_pcm_is_unmodified() {
        let mut d = Declicker::new();
        let frame: [i16; 160] = std::array::from_fn(|i| (i as i16).wrapping_mul(100));
        let processed = d.process(frame, false);
        assert_eq!(processed, frame);
        assert_eq!(d.fade_outs, 0);
        assert_eq!(d.fade_ins, 0);

        let next_frame: [i16; 160] = std::array::from_fn(|i| (i as i16 + 160).wrapping_mul(100));
        let next_processed = d.process(next_frame, false);
        assert_eq!(next_processed, next_frame);
        assert_eq!(d.fade_outs, 0);
        assert_eq!(d.fade_ins, 0);
    }

    #[test]
    fn single_frame_underrun_smooth_fade_out() {
        let mut d = Declicker::new();
        let frame = [10000i16; 160];
        let _ = d.process(frame, false);

        let missing = d.process([0i16; 160], true);
        assert_eq!(d.fade_outs, 1);
        assert!(missing[0] > 9000);
        assert!(missing[FADE_SAMPLES - 1] < 100);
        assert_eq!(missing[FADE_SAMPLES], 0);
        assert_eq!(missing[159], 0);
    }

    #[test]
    fn multiple_frame_underrun_outputs_silence() {
        let mut d = Declicker::new();
        let frame = [10000i16; 160];
        let _ = d.process(frame, false);

        let _ = d.process([0i16; 160], true);
        let second_missing = d.process([0i16; 160], true);
        let third_missing = d.process([0i16; 160], true);

        assert_eq!(d.fade_outs, 1);
        assert_eq!(second_missing, [0i16; 160]);
        assert_eq!(third_missing, [0i16; 160]);
    }

    #[test]
    fn recovery_fades_in_smoothly() {
        let mut d = Declicker::new();
        let frame = [10000i16; 160];
        let _ = d.process(frame, false);
        let _ = d.process([0i16; 160], true);

        let recovered = d.process(frame, false);
        assert_eq!(d.fade_ins, 1);
        assert!(recovered[0] < 50);
        assert!(recovered[FADE_SAMPLES / 2] > 4000 && recovered[FADE_SAMPLES / 2] < 6000);
        assert_eq!(recovered[FADE_SAMPLES], frame[FADE_SAMPLES]);
        assert_eq!(recovered[159], frame[159]);
    }

    #[test]
    fn sine_wave_interrupted_at_non_zero_amplitude() {
        let mut d = Declicker::new();
        let sine: [i16; 160] = std::array::from_fn(|i| {
            (15000.0 * (2.0 * PI * 440.0 * i as f64 / 16000.0).sin()).round() as i16
        });
        let _ = d.process(sine, false);

        let missing = d.process([0i16; 160], true);
        // Verify no jump greater than consecutive gradient
        let last = sine[159];
        let step = (missing[0] - last).abs();
        assert!(step < 1000);

        let resumed = d.process(sine, false);
        assert!(resumed[0].abs() < 50);
    }

    #[test]
    fn no_int16_overflow() {
        let mut d = Declicker::new();
        let max_frame = [i16::MAX; 160];
        let _ = d.process(max_frame, false);
        let missing = d.process([0; 160], true);
        for s in missing {
            assert!(s >= 0);
        }

        let min_frame = [i16::MIN; 160];
        let recovered = d.process(min_frame, false);
        for s in recovered {
            assert!(s <= 0);
        }
    }

    #[test]
    fn multiple_underrun_recovery_cycles() {
        let mut d = Declicker::new();
        for cycle in 0..10 {
            let frame = [((cycle + 1) * 1000) as i16; 160];
            let _ = d.process(frame, false);
            let _ = d.process([0; 160], true);
            let _ = d.process([0; 160], true);
            let recovered = d.process(frame, false);
            assert_eq!(recovered[159], frame[159]);
        }
        assert_eq!(d.fade_outs, 10);
        assert_eq!(d.fade_ins, 10);
    }
}
