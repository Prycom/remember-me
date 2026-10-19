//! Optional local RNNoise denoising. All processing stays on the encoder
//! thread; the built-in quantized model is part of the executable.

use nnnoiseless::{DenoiseState, RnnModel};

#[derive(Default)]
pub struct NoiseSuppressor {
    state: Option<Box<DenoiseState<'static>>>,
}

impl NoiseSuppressor {
    /// Input/output are normalized 48 kHz mono PCM. RNNoise works on 10 ms
    /// blocks in i16-scale floats, with one block of algorithmic delay.
    /// Disabled mode is an exact bypass and releases per-stream RNN state.
    pub fn process(&mut self, samples: &mut [f32], enabled: bool) -> Option<f32> {
        if !enabled {
            self.state = None;
            return None;
        }
        debug_assert_eq!(samples.len() % DenoiseState::FRAME_SIZE, 0);
        let state = self.state.get_or_insert_with(|| {
            DenoiseState::from_model(
                RnnModel::from_static_bytes(include_bytes!("../assets/voice.rnn"))
                    .expect("bundled voice model"),
            )
        });
        let mut probability = 0.0_f32;
        let mut input = [0.0; DenoiseState::FRAME_SIZE];
        let mut output = [0.0; DenoiseState::FRAME_SIZE];
        for chunk in samples.chunks_exact_mut(DenoiseState::FRAME_SIZE) {
            for (dst, src) in input.iter_mut().zip(chunk.iter()) {
                *dst = if src.is_finite() {
                    src.clamp(-1.0, 1.0) * 32_768.0
                } else {
                    0.0
                };
            }
            probability = probability.max(state.process_frame(&mut output, &input));
            for (dst, src) in chunk.iter_mut().zip(output) {
                *dst = if src.is_finite() { src / 32_768.0 } else { 0.0 };
            }
        }
        Some(probability)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    fn noise_frame(seed: &mut u32) -> Vec<f32> {
        (0..960)
            .map(|_| {
                let mut noise = 0.0;
                for _ in 0..4 {
                    *seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                    noise += (*seed as f64 / u32::MAX as f64 * 2.0 - 1.0) as f32;
                }
                noise * 0.0075
            })
            .collect()
    }

    fn rms(samples: &[f32]) -> f32 {
        (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt()
    }

    pub fn reference_speech() -> Vec<f32> {
        let wav = include_bytes!("../tests/fixtures/jfk.wav");
        let mut cursor = 12;
        let data = loop {
            let size = u32::from_le_bytes(wav[cursor + 4..cursor + 8].try_into().unwrap()) as usize;
            if &wav[cursor..cursor + 4] == b"data" {
                break &wav[cursor + 8..cursor + 8 + size];
            }
            cursor += 8 + size + size % 2;
        };
        let pcm: Vec<_> = data
            .chunks_exact(2)
            .map(|b| i16::from_le_bytes(b.try_into().unwrap()) as f32 / 32768.0)
            .collect();
        // The fixture is 16 kHz. Linear interpolation to the engine's 48 kHz.
        pcm.windows(2)
            .flat_map(|s| [s[0], (2.0 * s[0] + s[1]) / 3.0, (s[0] + 2.0 * s[1]) / 3.0])
            .collect()
    }

    #[test]
    fn actual_speech_survives_while_noise_and_key_clicks_are_reduced() {
        let speech = reference_speech();
        let speech_rms = rms(&speech);
        let mut suppressor = NoiseSuppressor::default();
        let mut seed = 42;
        let mut noisy = Vec::new();
        let mut clean = Vec::new();
        let mut output = Vec::new();
        for block in 0..(speech.len() / 960 + 100) {
            let mut frame = noise_frame(&mut seed);
            for (i, sample) in frame.iter_mut().enumerate() {
                let signal = if block >= 50 {
                    speech.get((block - 50) * 960 + i).copied().unwrap_or(0.0) * 0.04 / speech_rms
                } else {
                    0.0
                };
                // Short broadband transients approximate key switch impacts.
                if block % 13 == 0 && i < 180 {
                    *sample *= 8.0 * (1.0 - i as f32 / 180.0);
                }
                clean.push(signal);
                *sample += signal;
            }
            noisy.extend_from_slice(&frame);
            suppressor.process(&mut frame, true);
            output.extend(frame);
        }
        let delay = DenoiseState::FRAME_SIZE;
        let length = clean.len() - delay;
        let noise_region = 20 * 960..45 * 960;
        let noise_reduction =
            20.0 * (rms(&output[noise_region.clone()]) / rms(&noisy[noise_region])).log10();
        let voice_region = 60 * 960..length - 60 * 960;
        let voice_gain = rms(&output[voice_region.start + delay..voice_region.end + delay])
            / rms(&clean[voice_region.clone()]);
        // Scale-invariant projection SNR accounts for benign level changes.
        let projection_snr = |signal: &[f32]| {
            let reference = &clean[voice_region.clone()];
            let scale = reference
                .iter()
                .zip(signal)
                .map(|(a, b)| a * b)
                .sum::<f32>()
                / reference.iter().map(|a| a * a).sum::<f32>();
            let error = reference
                .iter()
                .zip(signal)
                .map(|(a, b)| (b - scale * a).powi(2))
                .sum::<f32>();
            10.0 * ((scale * scale * reference.iter().map(|a| a * a).sum::<f32>()) / error).log10()
        };
        let before = projection_snr(&noisy[voice_region.clone()]);
        let after = projection_snr(&output[voice_region.start + delay..voice_region.end + delay]);
        println!(
            "noise {noise_reduction:.1} dB, speech gain {voice_gain:.2}, noisy SI-SNR {before:.1} dB -> {after:.1} dB"
        );
        assert!(noise_reduction < -12.0);
        assert!(
            voice_gain > 0.4 && voice_gain < 1.2,
            "speech lost or amplified: {voice_gain}"
        );
        assert!(
            after > before + 2.0,
            "did not improve speech/noise separation"
        );
    }

    #[test]
    fn stationary_noise_is_reduced_without_turning_up_the_input() {
        let mut suppressor = NoiseSuppressor::default();
        let mut seed = 17;
        let mut input_power = 0.0;
        let mut output_power = 0.0;
        for i in 0..200 {
            let mut frame = noise_frame(&mut seed);
            let before = rms(&frame);
            suppressor.process(&mut frame, true);
            assert!(frame.iter().all(|s| s.is_finite()));
            if i >= 100 {
                input_power += before * before;
                output_power += rms(&frame).powi(2);
            }
        }
        let reduction = 10.0 * (output_power / input_power).log10();
        assert!(
            reduction < -12.0,
            "stationary noise reduction only {reduction:.1} dB"
        );
    }

    #[test]
    fn disabled_is_bit_exact_and_releases_stream_state() {
        let mut suppressor = NoiseSuppressor::default();
        let mut seed = 4;
        let original = noise_frame(&mut seed);
        let mut frame = original.clone();
        assert_eq!(suppressor.process(&mut frame, false), None);
        assert_eq!(frame, original);
        assert!(suppressor.state.is_none());
        suppressor.process(&mut frame, true);
        assert!(suppressor.state.is_some());
        frame.copy_from_slice(&original);
        suppressor.process(&mut frame, false);
        assert_eq!(frame, original);
        assert!(suppressor.state.is_none());
    }

    #[test]
    #[ignore = "manual denoiser CPU and RSS measurement in a release build"]
    fn denoiser_cpu_and_memory() {
        fn rss_kib() -> usize {
            std::fs::read_to_string("/proc/self/status")
                .unwrap()
                .lines()
                .find_map(|line| {
                    line.strip_prefix("VmRSS:")
                        .and_then(|v| v.split_whitespace().next())
                        .and_then(|v| v.parse().ok())
                })
                .unwrap()
        }
        fn cpu_seconds() -> f64 {
            let mut ts = libc::timespec {
                tv_sec: 0,
                tv_nsec: 0,
            };
            assert_eq!(
                unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut ts) },
                0
            );
            ts.tv_sec as f64 + ts.tv_nsec as f64 * 1e-9
        }
        let before = rss_kib();
        let mut suppressor = NoiseSuppressor::default();
        let mut seed = 4;
        let mut input = noise_frame(&mut seed);
        for (i, s) in input.iter_mut().enumerate() {
            for h in 1..=8 {
                *s += 0.04 / h as f32
                    * (i as f32 * 135.0 * h as f32 * std::f32::consts::TAU / 48_000.0).sin();
            }
        }
        let mut frame = input.clone();
        for _ in 0..30 {
            frame.copy_from_slice(&input);
            suppressor.process(&mut frame, true);
        }
        let extra_rss = rss_kib().saturating_sub(before);
        let start = cpu_seconds();
        for _ in 0..5000 {
            frame.copy_from_slice(&input);
            suppressor.process(std::hint::black_box(&mut frame), true);
            std::hint::black_box(&frame);
        }
        println!(
            "RNNoise: {:.3} s thread CPU / 100 s audio; RSS delta {extra_rss} KiB; stream struct {} bytes",
            cpu_seconds() - start,
            std::mem::size_of::<DenoiseState>()
        );
    }
}
