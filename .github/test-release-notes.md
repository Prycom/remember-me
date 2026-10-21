Rolling test builds for Linux x86_64 and Windows 10/11 x64.

- GIF frames are prepared before playback starts; resizing prepares the new size before animation resumes. No coarse placeholder frames during the first loop.
- Clipboard pictures use temporary files, deleted on normal exit or cancelled insertion. Choose their folder in F2 settings.
- Chat history lives in platform state storage, separate from settings. Existing logs and permanent clipboard attachments migrate without being discarded.
- Repository cleanup, concise README and automatic privacy checks. Commit identities use GitHub noreply addresses.
- Group voice for up to 16 participants, chat, consent-based file transfers, terminal media previews and customizable avatars remain available.

Update the clients with F5 and restart. No relay update is required for these changes.

Assets:

- `remember-me`: Linux x86_64 terminal client; system Opus and audio libraries required.
- `remember-me.exe`: portable Windows x64 client; bundled Opus and C runtime.
- `remember-me-server`: static Linux x86_64 relay; no audio libraries required.
- Platform-specific `.manifest.toml` / `.sha256`: update metadata and checksums.
