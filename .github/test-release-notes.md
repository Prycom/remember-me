Rolling test builds for Linux x86_64 and Windows 10/11 x64.

- Short replies no longer depend on RNNoise reaching a fixed speech-probability threshold before VOX or automatic gain can work. Quiet voiced onsets have an independent lightweight check.
- VOX retains up to 60 ms before an onset. The bounded burst fits the existing media queue; microphone gain still affects the result.
- Background estimates adapt to stationary noise and recover when the noise disappears. Established cleaning remains unchanged during confident speech; extra preservation is limited to weak voiced onsets.
- Mute, PTT, device and capture-setting changes invalidate queued PCM, resampler state and onset buffers. Previously blocked speech is not replayed after re-enabling the microphone.
- Regression coverage includes isolated 80/160/320 ms speech through Opus, low-pitch signals, changing backgrounds, hum, keyboard impacts and the actual media queue.
- Existing group rooms, chat, consent-based files, terminal media previews and avatars remain available.

Update the clients with F5 and restart. No relay update is required for these changes.

Assets:

- `remember-me`: Linux x86_64 terminal client; system Opus and audio libraries required.
- `remember-me.exe`: portable Windows x64 client; bundled Opus and C runtime.
- `remember-me-server`: static Linux x86_64 relay; no audio libraries required.
- Platform-specific `.manifest.toml` / `.sha256`: update metadata and checksums.
