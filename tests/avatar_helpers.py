"""Avatar fixtures/import through the real settings UI, using only Python stdlib."""
from pathlib import Path
import struct
import base64
import time
import tomllib
import zlib


def picture(path, tint=0):
    def chunk(kind, body):
        return struct.pack('!I', len(body)) + kind + body + struct.pack('!I', zlib.crc32(kind + body) & 0xffffffff)
    rows = []
    for y in range(48):
        row = bytearray([0])
        for x in range(48):
            row.extend(((x * 5 + tint) % 256, y * 5, 180 if (x//8+y//8) % 2 else 60, 255))
        rows.append(row)
    path.write_bytes(b'\x89PNG\r\n\x1a\n' + chunk(b'IHDR', struct.pack('!IIBBBBB', 48, 48, 8, 6, 0, 0, 0)) + chunk(b'IDAT', zlib.compress(b''.join(rows))) + chunk(b'IEND', b''))


def choose(client, root, who, path):
    config = root / who / 'remember-me/config.toml'
    before = tomllib.loads(config.read_text())
    client.key('\x1bOQ')  # F2
    time.sleep(0.12)
    client.key('\x1b[A' * 24 + '\x1b[B' * 16 + '\r')
    time.sleep(0.12)
    client.key('\x7f' * len(before.get('avatar_file', '')))
    client.key('\x1b[200~' + str(path) + '\x1b[201~\r')
    deadline = time.monotonic() + 10
    while time.monotonic() < deadline:
        try:
            cfg = tomllib.loads(config.read_text())
            if cfg.get('avatar_file') == str(path):
                if cfg.get('avatar_smooth'):
                    assert len(base64.b64decode(cfg['avatar']['rgba'])) == 256**2*4
                else:
                    assert len(cfg['avatar']['pixels']) == cfg.get('avatar_resolution', 32)**2
                assert len(base64.b64decode(cfg['avatar_original']['rgba'])) == 256**2*4
                assert len(cfg['avatar']['palette']) == 32
                break
        except (OSError, tomllib.TOMLDecodeError):
            pass
        time.sleep(0.03)
    else:
        raise AssertionError(f'avatar import did not finish for {who}: {client.log()[-2000:]}')
    client.key('\x1b[A' * 24 + '\x1b')
    time.sleep(0.12)


def clear(client):
    client.key('\x1bOQ')
    time.sleep(0.12)
    client.key('\x1b[A' * 24 + '\x1b[B' * 18 + '\r' + '\x1b[A' * 24 + '\x1b')
    time.sleep(0.15)


def resolution(client, root, who, size):
    config = root / who / 'remember-me/config.toml'
    before = tomllib.loads(config.read_text())
    sizes = [8, 12, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 0]
    current = 0 if before.get('avatar_smooth') else before.get('avatar_resolution', 32)
    delta = (sizes.index(size) - sizes.index(current)) % len(sizes)
    direction = '\x1b[C' if delta <= len(sizes)//2 else '\x1b[D'
    steps = min(delta, len(sizes)-delta)
    client.key('\x1bOQ')
    time.sleep(0.12)
    client.key('\x1b[A' * 24 + '\x1b[B' * 19)
    for _ in range(steps):
        client.key(direction)
        time.sleep(0.4)  # background resize, including on a slow debug build
    deadline = time.monotonic() + 10
    while time.monotonic() < deadline:
        cfg = tomllib.loads(config.read_text())
        if cfg.get('avatar_resolution') == (size or 256) and bool(cfg.get('avatar_smooth')) == (size == 0):
            if 'avatar' in cfg:
                if size:
                    assert len(cfg['avatar']['pixels']) == size**2
                else:
                    assert len(base64.b64decode(cfg['avatar']['rgba'])) == 256**2*4
            break
        time.sleep(0.03)
    else:
        raise AssertionError(f'avatar detail did not change to {size}px')
    client.key('\x1b[A' * 24 + '\x1b')
    time.sleep(0.12)


def rename(client, root, who, name):
    config = root / who / 'remember-me/config.toml'
    before = tomllib.loads(config.read_text())
    client.key('\x1bOQ')
    time.sleep(0.12)
    client.key('\x1b[A' * 24 + '\r')
    time.sleep(0.12)
    client.key('\x7f' * len(before['name']))
    client.key('\x1b[200~' + name + '\x1b[201~\r')
    deadline = time.monotonic() + 5
    while time.monotonic() < deadline:
        try:
            if tomllib.loads(config.read_text()).get('name') == name:
                break
        except (OSError, tomllib.TOMLDecodeError):
            pass
        time.sleep(0.03)
    else:
        raise AssertionError('live nickname did not save')
    client.key('\x1b')
    time.sleep(0.12)
