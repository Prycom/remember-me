//! Speech level control at 48 kHz. Update AGC only on detected speech so
//! silence/hangover does not progressively amplify microphone noise.

const TARGET_RMS: f32 = 0.12; // about -18 dBFS at unity input gain
const MAX_GAIN: f32 = 48.0;
const PEAK_LIMIT: f32 = 0.95;

/// Two Butterworth biquads: remove DC/rumble and high frequency hiss before
/// VOX/AGC. No FFT, allocations, extra thread, or buffering delay.
pub struct SpeechFilter {
    high_pass: Biquad,
    low_pass: Biquad,
    active: bool,
}

struct Biquad {
    b: [f32; 3],
    a: [f32; 2],
    z: [f32; 2],
}

impl Biquad {
    fn new(frequency: f32, high_pass: bool) -> Self {
        let w = std::f32::consts::TAU * frequency / 48_000.0;
        let cos = w.cos();
        let alpha = w.sin() / std::f32::consts::SQRT_2;
        let a0 = 1.0 + alpha;
        let b = if high_pass {
            [(1.0 + cos) / 2.0, -(1.0 + cos), (1.0 + cos) / 2.0]
        } else {
            [(1.0 - cos) / 2.0, 1.0 - cos, (1.0 - cos) / 2.0]
        };
        Self {
            b: b.map(|b| b / a0),
            a: [-2.0 * cos / a0, (1.0 - alpha) / a0],
            z: [0.0; 2],
        }
    }

    fn sample(&mut self, sample: f32) -> f32 {
        let out = self.b[0] * sample + self.z[0];
        self.z[0] = self.b[1] * sample - self.a[0] * out + self.z[1];
        self.z[1] = self.b[2] * sample - self.a[1] * out;
        // Avoid denormal CPU penalties during long periods of silence.
        for z in &mut self.z {
            if z.abs() < 1e-20 {
                *z = 0.0;
            }
        }
        out
    }
}

impl Default for SpeechFilter {
    fn default() -> Self {
        Self {
            high_pass: Biquad::new(80.0, true),
            low_pass: Biquad::new(8_000.0, false),
            active: false,
        }
    }
}

impl SpeechFilter {
    pub fn process(&mut self, frame: &mut [f32], enabled: bool) {
        if !enabled {
            if self.active {
                self.high_pass.z = [0.0; 2];
                self.low_pass.z = [0.0; 2];
            }
            self.active = false;
            return;
        }
        self.active = true;
        for sample in frame {
            let clean = if sample.is_finite() { *sample } else { 0.0 };
            *sample = self.low_pass.sample(self.high_pass.sample(clean));
        }
    }
}

pub struct PeakLimiter {
    gain: f32,
}

impl Default for PeakLimiter {
    fn default() -> Self {
        Self { gain: 1.0 }
    }
}

impl PeakLimiter {
    /// The callback/frame is already buffered. Inspect its peak before
    /// emitting any samples so the first loud wave is scaled intact, rather
    /// than changing gain mid-wave as an instantaneous sample limiter does.
    pub fn process(&mut self, samples: &mut [f32]) {
        if samples.is_empty() {
            return;
        }
        let mut peak = 0.0_f32;
        for sample in samples.iter_mut() {
            if !sample.is_finite() {
                *sample = 0.0;
            }
            peak = peak.max(sample.abs());
        }
        let required = PEAK_LIMIT / peak.max(PEAK_LIMIT);
        let start = self.gain.min(required);
        let release = 1.0 - (-(samples.len() as f32) / 4_800.0).exp();
        let end = (start + (1.0 - start) * release).min(required);
        let count = samples.len() as f32;
        for (i, sample) in samples.iter_mut().enumerate() {
            *sample *= start + (end - start) * (i + 1) as f32 / count;
        }
        self.gain = end;
    }
}

pub struct SpeechLeveler {
    gain: f32,
    limiter: PeakLimiter,
    levels: [f32; 50],
    count: usize,
    cursor: usize,
}

impl Default for SpeechLeveler {
    fn default() -> Self {
        Self {
            gain: 1.0,
            limiter: PeakLimiter::default(),
            levels: [0.0; 50],
            count: 0,
            cursor: 0,
        }
    }
}

impl SpeechLeveler {
    /// A new utterance must not inherit attenuation from an earlier laugh.
    pub fn idle(&mut self) {
        *self = Self::default();
    }

    #[cfg(test)]
    pub fn process(&mut self, frame: &mut [f32], speech: bool, automatic: bool, volume: f32) {
        for sample in frame.iter_mut() {
            *sample *= volume;
        }
        self.process_input(frame, speech, automatic, volume, None);
    }

    /// The input preamp has already been applied before denoising/VOX.
    /// Measure AGC in original microphone units so it cannot cancel the user's
    /// input gain. The raw reference prevents boosting suppressed noise.
    pub fn process_input(
        &mut self,
        frame: &mut [f32],
        speech: bool,
        automatic: bool,
        input_gain: f32,
        reference_rms: Option<f32>,
    ) {
        let mut start = self.gain;
        if !automatic {
            self.gain = 1.0;
        } else if speech && !frame.is_empty() {
            let rms = (frame.iter().map(|s| s * s).sum::<f32>() / frame.len() as f32).sqrt()
                / input_gain.max(0.0001);
            let rms = reference_rms
                .filter(|s| s.is_finite())
                .map_or(rms, |reference| reference.max(rms));
            if rms.is_finite() && rms > 0.0001 {
                self.levels[self.cursor] = rms;
                self.cursor = (self.cursor + 1) % self.levels.len();
                self.count = (self.count + 1).min(self.levels.len());
                let mut sorted = self.levels;
                sorted[..self.count].sort_unstable_by(f32::total_cmp);
                // Follow the stronger parts of the last second, not each
                // fading syllable. A brief laugh does not set the baseline.
                let reference = sorted[(self.count - 1) * 3 / 4].max(rms);
                let target = (TARGET_RMS / reference).clamp(0.125, MAX_GAIN);
                if target < self.gain || self.count == 1 {
                    // This entire 20 ms frame is available: do not ramp down
                    // from the old quiet-speech gain across a loud onset.
                    self.gain = target;
                    start = target;
                } else {
                    self.gain += (target - self.gain) * 0.15;
                }
            }
        }
        let count = frame.len().max(1) as f32;
        for (i, sample) in frame.iter_mut().enumerate() {
            // Ramp automatic gain without applying the input preamp twice.
            let gain = if automatic {
                start + (self.gain - start) * (i + 1) as f32 / count
            } else {
                1.0
            };
            *sample *= gain;
        }
        self.limiter.process(frame);
    }
}

/// VOX hysteresis and a continuous envelope at 48 kHz / 20 ms per frame.
#[derive(Default)]
pub struct VoiceGate {
    remaining: u8,
    envelope: f32,
    target: f32,
}

impl VoiceGate {
    /// Returns (send frame, strong speech for learning the microphone level).
    pub fn update(
        &mut self,
        level: f32,
        threshold: f32,
        vox: bool,
        ptt: bool,
        muted: bool,
    ) -> (bool, bool) {
        if muted || (!vox && !ptt) {
            *self = Self::default();
            return (false, false);
        }
        let strong = level >= threshold;
        let sustained = self.remaining > 0 && level >= threshold * 0.5;
        if !vox || strong || sustained {
            self.remaining = 15; // 300 ms for quiet consonants / brief pauses
            self.target = 1.0;
            return (true, strong);
        }
        if self.remaining == 0 {
            *self = Self::default();
            return (false, false);
        }
        self.remaining -= 1;
        self.target = (self.remaining as f32 / 3.0).min(1.0);
        (true, false)
    }

    pub fn apply(&mut self, frame: &mut [f32]) {
        for sample in frame {
            let step = if self.target > self.envelope {
                1.0 / 240.0
            } else {
                1.0 / 2_880.0
            };
            self.envelope += (self.target - self.envelope).clamp(-step, step);
            *sample *= self.envelope;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(amplitude: f32) -> Vec<f32> {
        (0..960)
            .map(|i| amplitude * (i as f32 * 440.0 * std::f32::consts::TAU / 48_000.0).sin())
            .collect()
    }

    fn rms(samples: &[f32]) -> f32 {
        (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt()
    }

    /// Nonlinear/ramped distortion relative to a scaled copy of the input.
    fn waveform_error(input: &[f32], output: &[f32]) -> f32 {
        let scale = input.iter().zip(output).map(|(a, b)| a * b).sum::<f32>()
            / input.iter().map(|s| s * s).sum::<f32>();
        let error: Vec<_> = input
            .iter()
            .zip(output)
            .map(|(a, b)| b - a * scale)
            .collect();
        rms(&error) / rms(output)
    }

    #[test]
    fn first_loud_frame_after_quiet_speech_is_not_buzzed_or_overamplified() {
        let mut leveler = SpeechLeveler::default();
        for _ in 0..100 {
            leveler.process(&mut tone(0.006), true, true, 1.0);
        }
        let input = tone(0.95);
        let mut output = input.clone();
        leveler.process(&mut output, true, true, 2.0);
        let error = waveform_error(&input, &output);
        assert!(error < 0.01, "first-frame waveform distortion: {error:.3}");
        assert!(
            rms(&output) < 0.3,
            "first-frame RMS overshoot: {}",
            rms(&output)
        );
    }

    #[test]
    fn limiter_preserves_the_waveform_from_the_first_peak() {
        let mut limiter = PeakLimiter::default();
        let input = tone(4.0);
        let mut output = input.clone();
        limiter.process(&mut output);
        let error = waveform_error(&input, &output);
        assert!(
            error < 0.001,
            "limiter adds distortion at onset: {error:.3}"
        );
    }

    #[test]
    fn quiet_and_loud_speech_converge_to_normal_volume() {
        for amplitude in [0.006, 0.02, 0.1, 0.8] {
            let mut leveler = SpeechLeveler::default();
            let mut frame = Vec::new();
            for _ in 0..100 {
                frame = tone(amplitude);
                leveler.process(&mut frame, true, true, 1.0);
            }
            assert!(
                (0.10..0.14).contains(&rms(&frame)),
                "input={amplitude}, RMS={}",
                rms(&frame)
            );
        }
    }

    #[test]
    fn silence_does_not_increase_gain_and_manual_volume_still_works() {
        let mut leveler = SpeechLeveler::default();
        for _ in 0..100 {
            let mut noise = tone(0.0001);
            leveler.process(&mut noise, false, true, 1.0);
            assert!(rms(&noise) < 0.0001);
        }
        let mut quiet = tone(0.02);
        leveler.process(&mut quiet, true, false, 2.0);
        assert!((rms(&quiet) - rms(&tone(0.04))).abs() < 0.00001);
        leveler.process(&mut quiet, true, true, 0.0);
        assert!(quiet.iter().all(|&s| s == 0.0));
    }

    #[test]
    fn fading_phrase_does_not_make_the_gain_chase_its_tail() {
        let mut leveler = SpeechLeveler::default();
        let mut steady = Vec::new();
        for _ in 0..60 {
            steady = tone(0.02);
            leveler.process(&mut steady, true, true, 1.0);
        }
        let normal = rms(&steady);
        let mut tail = Vec::new();
        for i in 1..=30 {
            tail = tone(0.02 * 0.1_f32.powf(i as f32 / 30.0));
            leveler.process(&mut tail, true, true, 1.0);
        }
        assert!(
            rms(&tail) < normal * 0.2,
            "a 20 dB input fade was amplified back up: normal={normal}, tail={}",
            rms(&tail)
        );
    }

    #[test]
    fn quiet_ending_survives_vox_and_the_final_frame_fades_to_zero() {
        let mut gate = VoiceGate::default();
        assert!(gate.update(0.002, 0.001, true, false, false).0);
        gate.apply(&mut tone(0.02));
        // Below the opening threshold: quiet syllables still keep VOX open.
        for _ in 0..50 {
            assert_eq!(
                gate.update(0.0006, 0.001, true, false, false),
                (true, false)
            );
            let mut frame = vec![1.0; 960];
            gate.apply(&mut frame);
            assert!(frame.iter().all(|s| *s > 0.99));
        }
        let mut ending = Vec::new();
        while gate.update(0.0001, 0.001, true, false, false).0 {
            let mut frame = vec![1.0; 960];
            gate.apply(&mut frame);
            ending.extend(frame);
            assert!(ending.len() <= 48_000);
        }
        assert!(ending.len() >= 48_000 / 4, "quiet ending was cut short");
        assert!(ending.last().unwrap().abs() < 0.0001, "abrupt final cut");
        assert!(ending.windows(2).all(|s| (s[1] - s[0]).abs() < 0.001));
    }

    #[test]
    fn a_quiet_new_phrase_does_not_inherit_loud_phrase_attenuation() {
        let mut leveler = SpeechLeveler::default();
        for _ in 0..50 {
            leveler.process(&mut tone(0.9), true, true, 2.0);
        }
        leveler.idle();
        let mut quiet = tone(0.006);
        leveler.process(&mut quiet, true, true, 1.0);
        assert!(rms(&quiet) > 0.1, "new phrase starts too quietly");
    }

    #[test]
    fn optional_filter_preserves_speech_and_reduces_rumble_and_hiss() {
        for (frequency, min, max) in [
            (20.0, 0.0, 0.1),
            (1000.0, 0.98, 1.02),
            (16_000.0, 0.0, 0.15),
        ] {
            let mut filter = SpeechFilter::default();
            let input: Vec<_> = (0..48_000)
                .map(|i| 0.1 * (i as f32 * frequency * std::f32::consts::TAU / 48_000.0).sin())
                .collect();
            let mut output = input.clone();
            filter.process(&mut output, true);
            let ratio = rms(&output[24_000..]) / rms(&input[24_000..]);
            assert!((min..max).contains(&ratio), "{frequency} Hz: {ratio}");
        }
        let mut filter = SpeechFilter::default();
        let mut dc = vec![0.1; 48_000];
        filter.process(&mut dc, true);
        assert!(rms(&dc[24_000..]) < 0.00001);
        let original = tone(0.02);
        let mut bypassed = original.clone();
        filter.process(&mut bypassed, false);
        assert_eq!(bypassed, original);
    }

    #[test]
    #[ignore = "manual CPU cost measurement in a release build"]
    fn filter_cpu_cost() {
        let mut filter = SpeechFilter::default();
        let input = tone(0.02);
        let mut frame = input.clone();
        let start = std::time::Instant::now();
        for _ in 0..5000 {
            frame.copy_from_slice(&input);
            filter.process(std::hint::black_box(&mut frame), true);
            std::hint::black_box(&frame);
        }
        println!(
            "filter + frame copy: {:.3} s wall time per 100 s audio",
            start.elapsed().as_secs_f64()
        );
    }

    #[test]
    fn sudden_loud_speech_after_quiet_speech_never_exceeds_peak_limit() {
        let mut leveler = SpeechLeveler::default();
        for _ in 0..100 {
            leveler.process(&mut tone(0.006), true, true, 1.0);
        }
        for _ in 0..20 {
            let mut loud = tone(0.95);
            leveler.process(&mut loud, true, true, 2.0);
            assert!(loud.iter().all(|s| s.is_finite() && s.abs() <= 0.95001));
        }
    }
}
