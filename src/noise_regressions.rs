#[test]
fn stationary_noise_hum_and_keyboard_do_not_open_vox_after_adaptation() {
    use crate::voice_level::{SpeechLeveler, VoiceGate};
    for kind in 0..6 {
        for amplitude in [0.001_f32, 0.005, 0.02] {
            let mut denoiser = NoiseSuppressor::default();
            let mut gate = VoiceGate::default();
            let mut leveler = SpeechLeveler::default();
            let mut seed = 23_u32;
            let mut colored = 0.0;
            let mut active = 0;
            let mut power = 0.0;
            let mut was = false;
            for block in 0..400 {
                let mut frame = vec![0.0; 960];
                for (i, s) in frame.iter_mut().enumerate() {
                    seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                    let white = (seed as f64 / u32::MAX as f64 * 2.0 - 1.0) as f32;
                    let time = (block * 960 + i) as f32 / 48_000.0;
                    *s = match kind {
                        0 => white * amplitude * 3.0_f32.sqrt(),
                        1 | 2 => {
                            let a = if kind == 1 { 0.95_f32 } else { 0.995 };
                            colored = a * colored + (1.0 - a * a).sqrt() * white * 3.0_f32.sqrt();
                            colored * amplitude
                        }
                        3 => {
                            (time * 50.0 * std::f32::consts::TAU).sin() * amplitude * 2.0_f32.sqrt()
                        }
                        4 => {
                            (time * 180.0 * std::f32::consts::TAU).sin() * amplitude
                                + white * amplitude * 0.15
                        }
                        _ => {
                            white * amplitude * 0.1
                                + if block % 13 == 0 && i < 240 {
                                    white * amplitude * 10.0 * (1.0 - i as f32 / 240.0)
                                } else {
                                    0.0
                                }
                        }
                    };
                }
                denoiser.process(&mut frame, true);
                let (send, strong) =
                    gate.update(denoiser.activity_level(), 0.001, true, false, false);
                if send {
                    if !was {
                        leveler.prime(denoiser.reference_rms(), true);
                    }
                    leveler.process_input(
                        &mut frame,
                        strong,
                        true,
                        1.0,
                        Some(denoiser.reference_rms()),
                    );
                    gate.apply(&mut frame);
                    if block >= 100 {
                        active += 1;
                        power += rms(&frame).powi(2);
                    }
                } else {
                    leveler.idle();
                }
                was = send;
            }
            assert!(
                active <= 3,
                "background opened VOX: kind={kind}, rms={amplitude}, active={active}/300"
            );
            assert!(
                (power / 300.0).sqrt() < 0.002,
                "background amplified: kind={kind}, rms={amplitude}"
            );
        }
    }
}

#[test]
fn quiet_short_reply_recovers_after_noise_disappears() {
    use crate::voice_level::VoiceGate;
    let speech = reference_speech();
    for pause in [25, 150] {
        for start_ms in [500, 1300, 2100, 6100, 7300, 8500] {
            let mut clip = speech[start_ms * 48..start_ms * 48 + 3840].to_vec();
            let scale = 0.002 / rms(&clip);
            for s in &mut clip {
                *s *= scale;
            }
            let mut denoiser = NoiseSuppressor::default();
            let mut gate = VoiceGate::default();
            let mut seed = 8_u32;
            for _ in 0..100 {
                let mut frame = noise_frame(&mut seed);
                denoiser.process(&mut frame, true);
                gate.update(denoiser.activity_level(), 0.001, true, false, false);
            }
            for _ in 0..pause {
                let mut frame = vec![0.0; 960];
                denoiser.process(&mut frame, true);
                gate.update(denoiser.activity_level(), 0.001, true, false, false);
            }
            let mut opened = false;
            clip.extend([0.0; 960]);
            for input in clip.chunks_exact(960) {
                let mut frame = input.to_vec();
                denoiser.process(&mut frame, true);
                opened |= gate
                    .update(denoiser.activity_level(), 0.001, true, false, false)
                    .0;
            }
            assert!(
                opened,
                "quiet answer rejected after old noise: start={start_ms}, pause={}ms",
                pause * 20
            );
        }
    }
}

#[test]
fn coherence_keeps_low_voices_and_rejects_smooth_random_noise() {
    for frequency in [80.0, 90.0, 120.0, 220.0, 400.0] {
        let mut signal = [0.0; 160];
        for (i, s) in signal.iter_mut().enumerate() {
            *s = (i as f32 * frequency * std::f32::consts::TAU / 8000.0).sin();
        }
        assert!(periodicity(&signal) > 0.55, "low voice lost: {frequency}Hz");
    }
    let mut random = [0.0; 160];
    let mut seed = 17_u32;
    let mut previous = 0.0;
    for s in &mut random {
        seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        previous = 0.995 * previous + (seed as f64 / u32::MAX as f64 * 2.0 - 1.0) as f32;
        *s = previous;
    }
    assert!(
        periodicity(&random) < 0.5,
        "smooth noise mistaken for voiced coherence"
    );
}
