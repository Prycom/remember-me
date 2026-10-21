"""Avatar fixtures/import through the real settings UI, using only Python stdlib."""
from pathlib import Path
import struct
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
                assert len(cfg['avatar']['pixels']) == 1024
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
