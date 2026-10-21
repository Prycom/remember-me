Rolling test builds for Linux x86_64 and Windows 10/11 x64.

- Downloaded pictures, animated GIFs and text files open inside the terminal, with an external-app preference and O shortcut. Downloads and per-recipient consent are unchanged.
- Paste clipboard images with Ctrl+V, Shift+Insert or F6 on Linux/Windows; Enter offers the captured PNG, Escape cancels while preserving the draft.
- Avatars use the same renderer in thumbnails and profiles. Choose an explicit pixel grid or Maximum (full colour and smoothing), plus optional circular decoration. Update all clients for full-colour avatar delivery; the relay is unchanged.
- GIF decoding, native image encoding and playback are bounded and run outside the voice loop; closing previews releases image resources.

- File offers require each recipient’s consent within two minutes; Y/Enter accepts, N declines. Update all clients; the relay is unchanged.
- Chat message selection/scrolling with arrows, highlighted rows, contextual file actions and Unicode cursor editing.
- Portable `remember-me.exe` for Windows x64: bundled Opus and C runtime, WASAPI audio, verified self-updates with F5.
- Live nickname updates reach existing and late-joining participants, including over the current relay; authenticated updates retry until acknowledged. Update all clients; no relay update needed.
- Compact avatar thumbnails stay the same size at every detail setting. Native terminal graphics show finer pixels inside that area when supported; ordinary terminals retain the half-block/ASCII fallback. Graphics encoding runs in a background worker and is cached.
- Participant cards have vertical spacing, no redundant room label and a fixed sidebar width. Photos fit without cropping; old photos reimport automatically when the source is available.
- Configurable avatar detail from 8×8 to 256×256, larger profile viewer, cached source for live resizing and MTU-safe encrypted chunk delivery.
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
