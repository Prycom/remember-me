Rolling test build for Linux x86_64.

- Group voice rooms for up to 16 participants; the room survives its creator leaving.
- Encrypted chat and file transfers, browser links and configurable download directory.
- Improved noise suppression and microphone input gain.
- Voice transmission continues independently of terminal input and rendering.
- Automatic client updates at startup, with F5 or `--update`; SHA-256 verification and atomic replacement. Restart the client after installing an update.

Assets:

- `remember-me`: desktop terminal client; requires system libopus and PulseAudio/PipeWire libraries.
- `remember-me-server`: standalone static Linux x86_64 relay; no audio libraries required.
- `remember-me.manifest.toml` and `remember-me.sha256`: client update metadata and checksum.

Update the relay and all clients when migrating from the old two-person protocol.
