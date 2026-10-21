#!/usr/bin/env bash
set -euo pipefail
# Run only after the code and tests have been reviewed. Uses gh's local login;
# credentials are never read into source, shell arguments or logs.
repo=Prycom/remember-me
gh auth status
cargo test --locked
cargo build --release --locked --bin remember-me
cargo build --release --locked --no-default-features --bin remember-me-server --target x86_64-unknown-linux-musl
python3 tests/update_smoke.py
python3 scripts/release_manifest.py target/release/remember-me target/release
gh release view test --repo "$repo" >/dev/null
release_commit=$(git rev-parse HEAD)
gh api "repos/$repo/git/refs/tags/test" --method PATCH -f sha="$release_commit" -F force=true
# Publish the manifest last: a reader seeing a mixed release refuses a bad hash
# and keeps their installed executable.
gh release upload test target/release/remember-me target/release/remember-me.sha256 target/x86_64-unknown-linux-musl/release/remember-me-server --repo "$repo" --clobber
gh release upload test target/release/remember-me.manifest.toml --repo "$repo" --clobber
release_notes=$(mktemp)
trap 'rm -f "$release_notes"' EXIT
cp .github/test-release-notes.md "$release_notes"
printf '\nBuilt from commit: %s\n' "$release_commit" >> "$release_notes"
gh release edit test --repo "$repo" --prerelease --title "Test build ${release_commit:0:7}" --notes-file "$release_notes"
