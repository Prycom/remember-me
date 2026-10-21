Rolling test builds for Linux x86_64 and Windows 10/11 x64.

- Portable `remember-me.exe` for Windows x64: bundled Opus and C runtime, WASAPI audio, verified self-updates with F5.
- Configurable avatar detail from 16×16 to 128×128, larger profile viewer, cached source for live resizing and MTU-safe encrypted chunk delivery. Update both clients; no relay update needed.
- Custom avatars in PNG/JPEG/WebP/GIF: pixel/ASCII portraits, speaking highlights, live encrypted sync and automatic nickname identicons.
- Panel-based TUI with mouse controls, OSC 52 copy, room member scrolling and inline chat input (PR #1 by Forwall100).
- Group voice rooms for up to 16 participants; the room survives its creator leaving.
- Encrypted chat and file transfers, browser links and configurable download directory.
- Improved noise suppression and microphone input gain.
- Voice transmission continues independently of terminal input and rendering.
- Automatic client updates at startup, with F5 or `--update`; SHA-256 verification and executable replacement. Restart the client after installing an update.

Assets:

- `remember-me`: desktop terminal client; requires system libopus and PulseAudio/PipeWire libraries.
- `remember-me.exe`: portable Windows x64 client; no extra codec DLLs or audio server required.
- `remember-me-server`: standalone static Linux x86_64 relay; no audio libraries required.
- `remember-me.manifest.toml` / `remember-me.sha256`: Linux update metadata and checksum.
- `remember-me.exe.manifest.toml` / `remember-me.exe.sha256`: Windows update metadata and checksum.

Update the relay and all clients when migrating from the old two-person protocol.
