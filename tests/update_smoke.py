#!/usr/bin/env python3
"""Run --update from a real ELF being replaced. Download helper is a private
stub; no GitHub requests or changes to the actual installed client."""
import hashlib
import os
from pathlib import Path
import shutil
import subprocess
import tempfile

project = Path(__file__).resolve().parent.parent
original = project / "target/release/remember-me"
with tempfile.TemporaryDirectory(prefix="remember-me-update-") as tmp:
    root = Path(tmp)
    executable = root / "remember-me"
    shutil.copy2(original, executable)
    executable.chmod(0o751)
    info = dict(line.split("=", 1) for line in subprocess.check_output([str(executable), "--build-info"], text=True).splitlines())
    updated = original.read_bytes() + b"\nupdate-smoke-verified-payload\n"
    (root / "payload").write_bytes(updated)
    digest = hashlib.sha256(updated).hexdigest()
    manifest = (f'version = "test"\nbuild = {int(info["build"]) + 1}\n'
                f'target = "{info["target"]}"\nsize = {len(updated)}\n')
    (root / "manifest").write_text(manifest + 'sha256 = "' + "0" * 64 + '"\n')
    helper = root / "curl"
    helper.write_text('''#!/usr/bin/env python3
import os, pathlib, shutil, sys
args=sys.argv[1:]
assert args[args.index('--proto')+1]=='=https'
assert args[args.index('--proto-redir')+1]=='=https'
assert args[-1].startswith('https://github.com/Prycom/remember-me/releases/download/test/')
source='manifest' if args[-1].endswith('.toml') else 'payload'
shutil.copyfile(pathlib.Path(os.environ['RM_UPDATE_FIXTURE'])/source,args[args.index('--output')+1])
''')
    helper.chmod(0o700)
    env = dict(os.environ, PATH=str(root) + os.pathsep + os.environ["PATH"], RM_UPDATE_FIXTURE=str(root))
    before = executable.read_bytes()
    failed = subprocess.run([str(executable), "--update"], env=env, capture_output=True, text=True)
    assert failed.returncode != 0 and "SHA-256" in failed.stderr, failed
    assert executable.read_bytes() == before
    broken = bytearray(128)
    broken[:4] = b"\x7fELF"
    broken[4] = 2
    broken[5] = 1
    broken[18] = 183 if info["target"].endswith("aarch64") else 62
    (root / "payload").write_bytes(broken)
    broken_digest = hashlib.sha256(broken).hexdigest()
    (root / "manifest").write_text(manifest.replace(f"size = {len(updated)}", "size = 128") + f'sha256 = "{broken_digest}"\n')
    invalid = subprocess.run([str(executable), "--update"], env=env, capture_output=True, text=True)
    assert invalid.returncode != 0 and executable.read_bytes() == before, invalid
    (root / "payload").write_bytes(updated)
    (root / "manifest").write_text(manifest + f'sha256 = "{digest}"\n')
    success = subprocess.run([str(executable), "--update"], env=env, capture_output=True, text=True)
    assert success.returncode == 0, success
    assert executable.read_bytes() == updated
    assert executable.stat().st_mode & 0o777 == 0o751
    assert not list(root.glob(".remember-me-*.tmp"))
    subprocess.run([str(executable), "--build-info"], check=True, stdout=subprocess.DEVNULL)
    print("UPDATE: bad hash/unloadable ELF preserve original; running ELF replaced atomically; permissions and next launch: PASS")
