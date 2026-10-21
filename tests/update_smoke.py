#!/usr/bin/env python3
"""Run --update from a real executable being replaced. Download helper is a private
stub; no GitHub requests or changes to the actual installed client."""
import hashlib
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import time

project = Path(__file__).resolve().parent.parent
windows = os.name == "nt"
asset = "remember-me.exe" if windows else "remember-me"
original = Path(os.environ.get("RM_UPDATE_BINARY", project / "target/release" / asset)).resolve()
with tempfile.TemporaryDirectory(prefix="remember-me-update-") as tmp:
    root = Path(tmp)
    executable = root / asset
    shutil.copy2(original, executable)
    executable.chmod(0o751)
    info = dict(line.split("=", 1) for line in subprocess.check_output([str(executable), "--build-info"], text=True).splitlines())
    updated = original.read_bytes() + b"\nupdate-smoke-verified-payload\n"
    (root / "payload").write_bytes(updated)
    digest = hashlib.sha256(updated).hexdigest()
    manifest = (f'version = "test"\nbuild = {int(info["build"]) + 1}\n'
                f'target = "{info["target"]}"\nsize = {len(updated)}\n')
    (root / "manifest").write_text(manifest + 'sha256 = "' + "0" * 64 + '"\n')
    helper = root / ("curl.exe" if windows else "curl")
    if not windows:
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
    else:
        source = root / 'curl_fixture.rs'
        source.write_text(r'''
use std::{env, fs, path::PathBuf};
fn main() {
    let args: Vec<_> = env::args_os().skip(1).collect();
    let output = args.iter().position(|a| a == "--output").unwrap()+1;
    let url = args.last().unwrap().to_str().unwrap();
    assert!(url.starts_with("https://github.com/Prycom/remember-me/releases/download/test/"));
    let file = if url.ends_with(".toml") {"manifest"} else {"payload"};
    fs::copy(PathBuf::from(env::var_os("RM_UPDATE_FIXTURE").unwrap()).join(file), &args[output]).unwrap();
}
''')
        subprocess.run(['rustc', '--edition=2024', str(source), '-o', str(helper)], check=True)
    env = dict(os.environ, PATH=str(root) + os.pathsep + os.environ["PATH"], RM_UPDATE_FIXTURE=str(root))
    before = executable.read_bytes()
    failed = subprocess.run([str(executable), "--update"], env=env, capture_output=True, text=True, encoding="utf-8")
    assert failed.returncode != 0 and "SHA-256" in failed.stderr, failed
    assert executable.read_bytes() == before
    broken = bytearray(128)
    if windows:
        broken[:2] = b'MZ'
        broken[60:64] = (64).to_bytes(4, 'little')
        broken[64:68] = b'PE\0\0'
        broken[68:70] = (0x8664).to_bytes(2, 'little')
        broken[88:90] = (0x20b).to_bytes(2, 'little')
    else:
        broken[:4] = b"\x7fELF"
        broken[4] = 2
        broken[5] = 1
        broken[18] = 183 if info["target"].endswith("aarch64") else 62
    (root / "payload").write_bytes(broken)
    broken_digest = hashlib.sha256(broken).hexdigest()
    (root / "manifest").write_text(manifest.replace(f"size = {len(updated)}", "size = 128") + f'sha256 = "{broken_digest}"\n')
    invalid = subprocess.run([str(executable), "--update"], env=env, capture_output=True, text=True, encoding="utf-8")
    assert invalid.returncode != 0 and executable.read_bytes() == before, invalid
    (root / "payload").write_bytes(updated)
    (root / "manifest").write_text(manifest + f'sha256 = "{digest}"\n')
    success = subprocess.run([str(executable), "--update"], env=env, capture_output=True, text=True, encoding="utf-8")
    assert success.returncode == 0, success
    assert executable.read_bytes() == updated
    if not windows:
        assert executable.stat().st_mode & 0o777 == 0o751
    assert not list(root.glob(".remember-me-*.tmp*"))
    subprocess.run([str(executable), "--build-info"], check=True, stdout=subprocess.DEVNULL)
    # Windows' cleanup helper waits for the old process to exit.
    if windows:
        deadline = time.monotonic() + 10
        while time.monotonic() < deadline and list(root.glob("*.__selfdelete__.exe")):
            time.sleep(0.05)
    print("UPDATE: bad hash/unloadable executable preserve original; running executable replaced; next launch: PASS")
