// End-to-end capture regressions: real denoising, VOX, AGC and Opus.
async fn decoded_capture(input: Vec<f32>, vox: bool, denoise: bool) -> Vec<f32> {
    let (params, tx, mut rx) = encoder();
    params.set_noise_suppression(denoise);
    params.set_vox_mode(vox);
    params.set_ptt_active(!vox);
    tx.send(input).unwrap();
    drop(tx);
    let mut decoder = Decoder::new().unwrap();
    let mut output = Vec::new();
    while let Some(packet) = tokio::time::timeout(Duration::from_secs(10), rx.recv())
        .await
        .unwrap()
    {
        output.extend(decoder.decode(&packet).unwrap());
    }
    output
}

fn short_reference(start_ms: usize, duration_ms: usize, amplitude: f32) -> Vec<f32> {
    let speech = crate::noise_suppress::tests::reference_speech();
    let mut clip = speech[start_ms * 48..(start_ms + duration_ms) * 48].to_vec();
    let scale = amplitude / rms(&clip).max(1e-9);
    for sample in &mut clip {
        *sample *= scale;
    }
    clip
}

fn best_voice_offset(reference: &[f32], output: &[f32]) -> (usize, f32) {
    if output.len() < reference.len() {
        return (0, 0.0);
    }
    let energy = reference.iter().step_by(8).map(|s| s * s).sum::<f32>();
    let mut best = (0, 0.0_f32);
    // Compensate bounded VOX lookback and Opus lookahead, using waveform
    // correlation, rather than assuming packets represent uninterrupted time.
    for offset in (0..=output.len() - reference.len()).step_by(24) {
        let mut dot = 0.0;
        let mut power = 0.0;
        for (a, b) in reference
            .iter()
            .step_by(8)
            .zip(output[offset..].iter().step_by(8))
        {
            dot += a * b;
            power += b * b;
        }
        let correlation = dot / (energy * power).sqrt().max(1e-12);
        if correlation > best.1 {
            best = (offset, correlation);
        }
    }
    best
}

#[tokio::test]
async fn isolated_short_replies_survive_vox_and_ptt_after_silence_through_opus() {
    for vox in [true, false] {
        for duration in [80, 160, 320] {
            for start in [500, 1300, 2500, 3700, 4900, 6100, 7300, 8500] {
                for amplitude in [0.002, 0.006] {
                    let original = crate::noise_suppress::tests::reference_speech();
                    if rms(&original[start * 48..(start + duration) * 48]) < 0.015 {
                        continue;
                    }
                    let clip = short_reference(start, duration, amplitude);
                    let mut input = vec![0.0; 48_000];
                    input.extend_from_slice(&clip);
                    input.extend(vec![0.0; 24_001]);
                    let output = decoded_capture(input, vox, true).await;
                    // This old broadcast fixture includes a ~0.01 RMS noise
                    // floor. Label clearly voiced regions above that recorded floor
                    // before normalization so increasing
                    // test gain does not turn recorded background into speech.
                    let source_level = rms(&original[start * 48..(start + duration) * 48]);
                    let reference_threshold = 0.001_f32.max(0.04 * amplitude / source_level);
                    let first = clip
                        .chunks_exact(480)
                        .position(|c| rms(c) >= reference_threshold)
                        .unwrap_or(0)
                        .saturating_sub(1)
                        * 480;
                    let voice = &clip[first..];
                    let (offset, correlation) = best_voice_offset(voice, &output);
                    assert!(
                        correlation > 0.35,
                        "short reply lost: vox={vox}, start={start}, duration={duration}, rms={amplitude}, correlation={correlation}"
                    );
                    let signal = &output[offset..offset + voice.len()];
                    assert!(
                        rms(signal) > 0.02,
                        "short reply inaudible: vox={vox}, start={start}, duration={duration}, rms={amplitude}, output={}",
                        rms(signal)
                    );
                    let head = 1920.min(voice.len());
                    let total_gain = rms(signal) / rms(voice);
                    assert!(
                        rms(&signal[..head]) > rms(&voice[..head]) * total_gain * 0.22,
                        "start of short reply lost: vox={vox}, start={start}, duration={duration}"
                    );
                }
            }
        }
    }
}

#[tokio::test]
async fn short_replies_survive_a_learned_background_and_pitch_variation() {
    for pitch in [0.8_f32, 1.0, 1.25] {
        let reference = crate::noise_suppress::tests::reference_speech();
        let mut clip: Vec<f32> = (0..7680)
            .map(|i| {
                let position = 1300.0 * 48.0 + i as f32 * pitch;
                let index = position as usize;
                let fraction = position - index as f32;
                reference[index] * (1.0 - fraction) + reference[index + 1] * fraction
            })
            .collect();
        let scale = 0.006 / rms(&clip);
        for s in &mut clip {
            *s *= scale;
        }
        let mut seed = 17_u32;
        let mut input = vec![0.0; 96_000 + clip.len() + 24_001];
        for s in &mut input {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            *s = (seed as f64 / u32::MAX as f64 * 2.0 - 1.0) as f32 * 0.001 * 3.0_f32.sqrt();
        }
        for (s, voice) in input[96_000..].iter_mut().zip(&clip) {
            *s += voice;
        }
        let output = decoded_capture(input, true, true).await;
        let first = clip
            .chunks_exact(480)
            .position(|c| rms(c) >= 0.001_f32.max(rms(&clip) * 0.25))
            .unwrap_or(0)
            .saturating_sub(1)
            * 480;
        let (_, correlation) = best_voice_offset(&clip[first..], &output);
        assert!(
            correlation > 0.35,
            "short noisy speech lost at pitch {pitch}: {correlation}"
        );
    }
}

#[tokio::test]
async fn capture_revisions_discard_queued_pcm_and_resampler_remainders() {
    for ptt in [false, true] {
        let (params, tx, mut rx) = encoder();
        params.set_noise_suppression(true);
        if ptt {
            params.set_vox_mode(false);
            params.set_ptt_active(true);
        }
        // Less than a frame: these samples remain in the resampler, not VOX.
        tx.send(vec![0.1; 480]).unwrap();
        tokio::time::sleep(Duration::from_millis(30)).await;
        let old_epoch = params.capture_epoch.load(Ordering::Acquire);
        if ptt {
            params.set_ptt_active(false);
            params.set_ptt_active(true);
        } else {
            params.set_mute_mic(true);
            params.set_mute_mic(false);
        }
        // Simulate an old callback that was queued across a complete toggle.
        tx.sender
            .send(CapturedAudio {
                epoch: old_epoch,
                samples: vec![0.1; 3840],
            })
            .unwrap();
        tx.send(vec![0.0; 24_001]).unwrap();
        drop(tx);
        let mut decoder = Decoder::new().unwrap();
        while let Some(packet) = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .unwrap()
        {
            let samples = decoder.decode(&packet).unwrap();
            assert!(
                rms(&samples) < 0.0001,
                "old microphone audio was replayed after a permission revision (ptt={ptt})"
            );
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn vox_lookback_fits_the_actual_six_packet_media_route() {
    let (params, tx, encoded) = encoder();
    params.set_auto_gain(false);
    let (router, _) = AudioEngine::for_test();
    let (media_tx, mut media) = tokio::sync::mpsc::channel(MAX_PENDING_AUDIO);
    let route = router.route_media(media_tx);
    let pump = tokio::spawn(router.clone().forward_encoded(encoded));
    let mut input = vec![0.0007; 3 * FRAME_SAMPLES];
    input.extend(vec![0.02; FRAME_SAMPLES + 1]);
    tx.send(input).unwrap();
    drop(tx);
    pump.await.unwrap();
    drop(route);
    let mut packets = Vec::new();
    while let Some(packet) = media.recv().await {
        packets.push(packet);
    }
    assert_eq!(
        packets.len(),
        4,
        "bounded onset burst was dropped by the media route"
    );
    let mut decoder = Decoder::new().unwrap();
    let output: Vec<f32> = packets
        .into_iter()
        .flat_map(|p| decoder.decode(&p).unwrap())
        .collect();
    assert_eq!(output.len(), 4 * FRAME_SAMPLES);
}
