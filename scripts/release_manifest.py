#!/usr/bin/env python3
"""Describe the actual built executable, never a timestamp guessed by CI."""
import hashlib
from pathlib import Path
import subprocess
import sys

binary = Path(sys.argv[1]).resolve()
output = Path(sys.argv[2]) if len(sys.argv) > 2 else binary.parent
output.mkdir(parents=True, exist_ok=True)
info = dict(line.split("=", 1) for line in subprocess.check_output(
    [str(binary), "--build-info"], text=True).splitlines())
with binary.open("rb") as source:
    checksum = hashlib.sha256()
    for block in iter(lambda: source.read(65536), b""):
        checksum.update(block)
    digest = checksum.hexdigest()
(output / "remember-me.manifest.toml").write_text(
    f'version = "{info["version"]}"\nbuild = {int(info["build"])}\n'
    f'target = "{info["target"]}"\nsha256 = "{digest}"\nsize = {binary.stat().st_size}\n')
(output / "remember-me.sha256").write_text(f"{digest}  remember-me\n")
print(f"{info['target']} build {info['build']}: {digest}")
