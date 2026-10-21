#!/usr/bin/env python3
"""Three real TUI clients, independent virtual microphones/outputs, local relay."""
import hashlib
import argparse
import fcntl
import math
import os
from pathlib import Path
import random
import re
import signal
import struct
import subprocess
import tempfile
import threading
import termios
import time
import avatar_helpers
from voice_smoke import Client, LossyRelayProxy, free_port, pulse, tone, measure, file_consent


def wait(predicate, label, timeout=10):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if predicate():
            return
        time.sleep(0.05)
    raise AssertionError(label)


def amplitude(samples, frequency):
    n = len(samples)
    real = sum(s * math.cos(2 * math.pi * frequency * i / 48000) for i, s in enumerate(samples))
    imag = sum(s * math.sin(2 * math.pi * frequency * i / 48000) for i, s in enumerate(samples))
    return 2 * math.hypot(real, imag) / n


def ui_stalls(root, names, host):
    """The creator must keep sending voice despite input floods / a slow terminal."""
    wav = root / "ui_stall.wav"
    tone(wav, 440, 6.0)
    for freeze in (False, True):
        def stall():
            time.sleep(1.0)
            if freeze:
                host.output_enabled.clear()
                # Force full redraws so the PTY really fills up, rather than
                # merely pausing a reader while unchanged frames fit in it.
                for i in range(80):
                    fcntl.ioctl(host.master, termios.TIOCSWINSZ,
                                struct.pack("HHHH", 40, 180 + i % 2, 0, 0))
                    os.kill(host.proc.pid, signal.SIGWINCH)
                    time.sleep(0.05)
                host.output_enabled.set()
            else:
                until = time.monotonic() + 4.0
                while time.monotonic() < until:
                    host.key("\x1b[97;1:3u")  # ignored key releases every 3 ms
                    time.sleep(0.003)
        thread = threading.Thread(target=stall)
        thread.start()
        try:
            samples = measure(root, names['a', 'mic'], names['b', 'out'], wav, True,
                              "host voice with " + ("blocked terminal" if freeze else "continuous input"))
        finally:
            thread.join()
            host.output_enabled.set()
        # Require every 40 ms of steady speech, not just the average RMS.
        quiet = []
        for start in range(2 * 48000, 5 * 48000, 1920):
            block = samples[start:start + 1920]
            if math.sqrt(sum(s*s for s in block) / len(block)) < 0.003:
                quiet.append(start / 48000)
        assert not quiet, f"UI interrupted host audio at {quiet} seconds"
        if freeze:
            wait(lambda: "[ui] slow terminal/render:" in host.log(),
                 "test did not actually block terminal rendering")
    print("  voice independent of input and terminal rendering: PASS", flush=True)


def main():
    project = Path(__file__).resolve().parent.parent
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary",type=Path,default=project / "target/release/remember-me")
    parser.add_argument("--relay-binary",type=Path,default=project / "target/x86_64-unknown-linux-musl/release/remember-me-server")
    parser.add_argument("--ui-stall-only", action="store_true")
    options=parser.parse_args()
    binary=options.binary.resolve();relay_binary=options.relay_binary.resolve()
    modules, clients = [], []
    relay = None
    proxy = None
    with tempfile.TemporaryDirectory(prefix="remember-me-group-") as tmp:
        root = Path(tmp)
        log = (root / "relay.log").open("wb")
        try:
            names = {}
            for who in "abc":
                for kind in ("mic", "out"):
                    name = f"rm_group_{os.getpid()}_{who}_{kind}"
                    names[who, kind] = name
                    modules.append(pulse("load-module", "module-null-sink", f"sink_name={name}", "rate=48000", "channels=1", f"sink_properties=device.description={name}"))
            port = free_port()
            server = f"127.0.0.1:{port}"
            relay = subprocess.Popen([str(relay_binary), "-p", str(port)], stdout=log, stderr=log)
            time.sleep(0.3)
            proxy=LossyRelayProxy(("127.0.0.1",port))
            server=f"{proxy.address[0]}:{proxy.address[1]}"
            for who in "abc":
                config = root / who / "remember-me/config.toml"
                config.parent.mkdir(parents=True)
                config.write_text(f'name = "{who}"\nauto_update = false\n'
                                  f'input_device = "Monitor of {names[who, "mic"]}"\n'
                                  f'output_device = "{names[who, "out"]}"\n'
                                  f'download_dir = "{root / ("downloads_" + who)}"\n'
                                  f'server_addr = "{server if who == "a" else ""}"\n')
            a = Client(binary, root, "a", ["--host"], graphics=True)
            clients.append(a)
            a.wait_log("код комнаты")
            code = re.search(r"код комнаты ([A-Z2-9]{4})", a.log()).group(1)
            a.wait_log("joined room")
            b = Client(binary, root, "b", ["--connect", f"{code}@{server}"])
            clients.append(b)
            for client in clients:
                client.wait_log("roster: 1 remote members")
            time.sleep(0.5)
            proxy.impair_profiles = True
            avatar = root / "avatar.png"
            avatar_helpers.picture(avatar)
            avatar_helpers.resolution(a, root, 'a', 256)
            avatar_helpers.choose(a, root, 'a', avatar)
            b.wait_log('avatar received from a revision=2')
            avatar.unlink()
            proxy.impair_names = True
            avatar_helpers.rename(a, root, 'a', 'Солнышко')
            b.wait_log('name received: Солнышко revision=2')
            c = Client(binary, root, "c", ["--connect", f"{code}@{server}"])
            clients.append(c)
            for client in clients:
                client.wait_log("roster: 2 remote members")
            c.wait_log('name received: Солнышко revision=2')
            c.wait_log('avatar received from Солнышко revision=2')
            avatar_helpers.rename(a, root, 'a', 'a')
            for client in (b,c): client.wait_log('name received: a revision=3')
            assert proxy.name_lost == 1 and proxy.name_ack_lost == 1
            print('  live names: packet/ACK loss, stale relay roster and late join: PASS', flush=True)
            assert proxy.avatar_lost == 1 and proxy.avatar_ack_lost == 1
            avatar_helpers.resolution(a, root, 'a', 224)
            for client in (b, c):
                client.wait_log('avatar received from a revision=3')
            avatar_helpers.clear(a)
            for client in (b, c):
                client.wait_log('avatar received from a revision=4')
            print('  group 256px avatar: chunk/ACK loss, late join, cached resize, live reset: PASS', flush=True)
            time.sleep(0.5)
            assert a.graphics_placeholders > 0 and len(a.graphics_headers) >= 3, 'native thumbnails were not rendered'
            assert all(w == 64 and h == 64 for _,w,h in a.graphics_headers), a.graphics_headers
            ids = [image for image,_,_ in a.graphics_headers]
            assert len(ids) == len(set(ids)), 'cached thumbnail was transmitted more than once'
            print('  native graphics: fixed 64px canvas, Unicode placements, one transmission per cached image: PASS', flush=True)
            ui_stalls(root, names, a)
            if options.ui_stall_only:
                return
            sounds = {}
            for who, frequency in zip("abc", (440, 660, 880)):
                sounds[who] = root / f"{who}.wav"
                tone(sounds[who], frequency, 1.2)
            for source, receiver in [("a", "b"), ("a", "c"), ("b", "a"), ("b", "c"), ("c", "a"), ("c", "b")]:
                measure(root, names[source, "mic"], names[receiver, "out"], sounds[source], True, f"group {source} → {receiver}")
            # Different sine frequencies must coexist at C, proving independent
            # decoders and summation instead of alternating playback streams.
            proxy.impair_audio=True
            long_a, long_b = root / "mix_a.wav", root / "mix_b.wav"
            tone(long_a, 440, 3.5)
            tone(long_b, 660, 3.5)
            def second_speaker():
                time.sleep(0.3)
                subprocess.run(["paplay", f"--device={names['b', 'mic']}", str(long_b)], check=True)
            thread = threading.Thread(target=second_speaker)
            thread.start()
            mixed = measure(root, names["a", "mic"], names["c", "out"], long_a, True, "A+B heard simultaneously by C")
            thread.join()
            proxy.impair_audio=False
            block = mixed[48000:96000]
            levels = [amplitude(block, f) for f in (440, 660)]
            assert min(levels) > 0.025, levels
            print(f"  separate decoders/mixing: amplitudes {levels}: PASS", flush=True)
            a.key("iгрупповое сообщение https://example.org/\r")
            wait(lambda: all(any("групповое сообщение" in p.read_text() and "@rm2\t0\ta\t" in p.read_text() for p in (root / who / "remember-me").glob("chat-group-*.log")) for who in "bc"), "group text missing or wrong author")
            source = root / "group file.bin"
            payload = random.Random(17).randbytes(256 * 1024 + 57)
            source.write_bytes(payload)
            a.key("\x1bOR\x1b[200~" + str(source) + "\x1b[201~\r")
            for client in (b,c): client.wait_log('[file] offer '+source.name)
            time.sleep(0.3)
            assert not any((root / ('downloads_'+who) / source.name).exists() for who in 'bc')
            file_consent(b,source.name)
            file_consent(c,source.name,False)
            wait(lambda:(root/'downloads_b'/source.name).exists(),"accepted group file missing")
            assert hashlib.sha256((root/'downloads_b'/source.name).read_bytes()).digest()==hashlib.sha256(payload).digest()
            assert not (root/'downloads_c').exists() or not list((root/'downloads_c').iterdir())
            print("  group file: no download before consent, B accepts, C declines independently: PASS",flush=True)
            # Muting one participant affects only them; peers remain audible.
            a.key("m")
            time.sleep(0.5)
            measure(root, names["a", "mic"], names["c", "out"], sounds["a"], False, "A muted in group")
            measure(root, names["b", "mic"], names["c", "out"], sounds["b"], True, "B audible while A muted")
            a.key("u")
            a.key("y")
            for client in (b, c):
                client.wait_log("roster: 1 remote members")
            measure(root, names["b", "mic"], names["c", "out"], sounds["b"], True, "room survives creator leaving")
            c.key("u")
            c.key("y")
            b.wait_log("roster: 0 remote members")
            print("  member leave + creator-independent room lifecycle: PASS", flush=True)
            assert proxy.dropped and proxy.file_lost==2 and proxy.audio_lost>0
            print("  lost roster/files + audio jitter/loss recovered: PASS",flush=True)
            print("GROUP: PASS", flush=True)
        finally:
            for client in clients:
                client.close()
            if relay:
                relay.terminate()
                relay.wait(timeout=5)
            if proxy:
                proxy.close()
            log.close()
            for module in reversed(modules):
                try:
                    pulse("unload-module", module)
                except subprocess.CalledProcessError:
                    pass


if __name__ == "__main__":
    main()
