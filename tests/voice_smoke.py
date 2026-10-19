#!/usr/bin/env python3
"""Exercise two real TUI clients through CPAL/PipeWire, Opus and UDP.

Requires a running PulseAudio/PipeWire server, pactl, paplay and parec.
Uses temporary null sinks only; does not change defaults or record the mic.
Run after cargo build --release: python3 tests/voice_smoke.py
"""

import argparse
import array
import fcntl
import math
import os
from pathlib import Path
import pty
import re
import socket
import struct
import subprocess
import tempfile
import termios
import threading
import time
import wave


def pulse(*args):
    return subprocess.check_output(["pactl", *args], text=True).strip()


def free_port():
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


class Client:
    def __init__(self, binary, root, name, args):
        master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 40, 180, 0, 0))
        self.master = master
        self.log_path = root / (name + ".log")
        self.log_file = self.log_path.open("wb")
        env = dict(os.environ, XDG_CONFIG_HOME=str(root / name), RM_DEBUG="1", TERM="xterm-256color")
        self.proc = subprocess.Popen([str(binary), *args], stdin=slave, stdout=slave,
                                     stderr=self.log_file, env=env)
        os.close(slave)
        # A real terminal continuously consumes the rendered TUI as well.
        def drain():
            try:
                while os.read(master, 65536):
                    pass
            except OSError:
                pass
        self.reader = threading.Thread(target=drain, daemon=True)
        self.reader.start()

    def key(self, key):
        os.write(self.master, key.encode())

    def log(self):
        return self.log_path.read_text(errors="replace")

    def wait_log(self, text, timeout=10):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if text in self.log():
                return
            if self.proc.poll() is not None:
                raise AssertionError(f"client exited: {self.log()}")
            time.sleep(0.05)
        raise AssertionError(f"did not see {text!r}: {self.log()[-4000:]}")

    def close(self):
        if self.proc.poll() is None:
            self.proc.terminate()
        try:
            self.proc.wait(timeout=3)
        except subprocess.TimeoutExpired:
            self.proc.kill()
            self.proc.wait()
        self.log_file.close()
        os.close(self.master)
        self.reader.join(timeout=1)


def tone(path, frequency, duration):
    samples = array.array("h", (
        int(0.02 * 32767 * math.sin(2 * math.pi * frequency * i / 48000))
        for i in range(int(duration * 48000))
    ))
    with wave.open(str(path), "wb") as wav:
        wav.setparams((1, 2, 48000, 0, "NONE", "not compressed"))
        wav.writeframes(samples.tobytes())


def measure(root, source_sink, output_sink, wav, audible, label):
    path = root / "capture.raw"
    with path.open("wb") as output:
        recorder = subprocess.Popen([
            "parec", f"--device={output_sink}.monitor", "--raw",
            "--format=float32le", "--rate=48000", "--channels=1",
            "--latency-msec=20",
        ], stdout=output, stderr=subprocess.PIPE)
        try:
            time.sleep(0.3)
            subprocess.run(["paplay", f"--device={source_sink}", str(wav)], check=True, timeout=15)
            time.sleep(0.4)
        finally:
            recorder.terminate()
            _, err = recorder.communicate(timeout=3)
    samples = array.array("f")
    samples.frombytes(path.read_bytes())
    assert len(samples) > 10000, f"no monitor samples: {err!r}"
    level = math.sqrt(sum(s * s for s in samples) / len(samples))
    assert level > 0.003 if audible else level < 0.0005, f"{label}: RMS={level:.6f}"
    if audible:
        # Check the middle of every second too: a short burst followed by
        # silence must not pass merely because the overall RMS is nonzero.
        for start in range(48000, len(samples) - 48000, 48000):
            block = samples[start:start + 12000]
            assert math.sqrt(sum(s * s for s in block) / len(block)) > 0.003, label
    print(f"  {label}: RMS={level:.6f}", flush=True)


def scenario(binary, root, mode, relay_binary=None):
    root.mkdir()
    prefix = f"rm_voice_test_{os.getpid()}_{mode}"
    names = {key: f"{prefix}_{key}" for key in ("mic_a", "mic_b", "out_a", "out_b")}
    modules, clients = [], []
    relay = None
    try:
        for name in names.values():
            modules.append(pulse("load-module", "module-null-sink", f"sink_name={name}",
                                 "rate=48000", "channels=1", f"sink_properties=device.description={name}"))
        time.sleep(0.3)
        port = free_port()
        server = ""
        if mode == "relay":
            relay_port = free_port()
            server = f"127.0.0.1:{relay_port}"
            relay_command = ([str(relay_binary)] if relay_binary else [str(binary), "--server"])
            relay = subprocess.Popen([*relay_command, "-p", str(relay_port)],
                                     stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            time.sleep(0.3)
        for who in ("a", "b"):
            cfg = root / who / "remember-me" / "config.toml"
            cfg.parent.mkdir(parents=True)
            cfg.write_text(f'name = "{who}"\nport = {port}\n'
                           f'input_device = "Monitor of {names["mic_" + who]}"\n'
                           f'output_device = "{names["out_" + who]}"\n'
                           f'server_addr = "{server if who == "a" else ""}"\n')
        host = Client(binary, root, "a", ["--host"])
        clients.append(host)
        host.wait_log("attach_input")
        # Reproduce output callbacks running well before the first audio packet.
        time.sleep(1.0)
        target = f"127.0.0.1:{port}"
        if mode == "relay":
            host.wait_log("код комнаты")
            code = re.search(r"код комнаты ([A-Z2-9]{4})", host.log()).group(1)
            target = f"{code}@{server}"
        joiner = Client(binary, root, "b", ["--connect", target])
        clients.append(joiner)
        host.wait_log("host: HELLO")
        time.sleep(0.3)
        host.key("\r")
        joiner.wait_log("joiner: ACCEPT")
        time.sleep(0.5)
        short, long = root / "short.wav", root / "long.wav"
        tone(short, 440, 1.0)
        tone(long, 660, 6.0)
        print(mode + ":", flush=True)
        measure(root, names["mic_a"], names["out_b"], short, True, "A → B after idle")
        time.sleep(1.0)
        measure(root, names["mic_b"], names["out_a"], short, True, "B → A after pause")
        host.key("m")
        time.sleep(0.3)
        measure(root, names["mic_a"], names["out_b"], short, False, "mute")
        host.key("m")
        time.sleep(0.3)
        pongs_before = [client.log().count("rx PONG") for client in clients]
        measure(root, names["mic_a"], names["out_b"], long, True, "unmute + continuous voice")
        assert all(client.log().count("rx PONG") > before
                   for client, before in zip(clients, pongs_before)), "keepalive starved during audio"
        joiner.key("d")
        time.sleep(0.3)
        measure(root, names["mic_a"], names["out_b"], short, False, "deafen")
        joiner.key("d")
        time.sleep(0.3)
        measure(root, names["mic_a"], names["out_b"], short, True, "undeafen")
        host.key("uy")
        host.wait_log("HangUp")
        time.sleep(0.3)
        host.key("\x1b")
        time.sleep(0.3)
        host.key("s")
        time.sleep(0.3)
        host.key("l")
        time.sleep(0.3)
        measure(root, names["mic_a"], names["out_a"], short, True, "local echo test")
        for client in clients:
            assert "расшифровка не удалась" not in client.log(), client.log()[-2000:]
            assert "резерв" not in client.log(), "virtual device did not attach"
        print(f"  {mode}: PASS", flush=True)
    except Exception:
        for client in clients:
            print(client.log()[-4000:])
        raise
    finally:
        for client in reversed(clients):
            client.close()
        if relay is not None:
            relay.terminate()
            relay.wait(timeout=3)
        for module in reversed(modules):
            pulse("unload-module", module)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=Path("target/release/remember-me"))
    parser.add_argument("--mode", choices=("direct", "relay", "both"), default="both")
    parser.add_argument("--relay-binary", type=Path,
                        help="use a standalone remember-me-server for relay mode")
    args = parser.parse_args()
    binary = args.binary.resolve(strict=True)
    relay_binary = args.relay_binary.resolve(strict=True) if args.relay_binary else None
    with tempfile.TemporaryDirectory(prefix="remember-me-voice-") as tmp:
        for mode in ("direct", "relay") if args.mode == "both" else (args.mode,):
            scenario(binary, Path(tmp) / mode, mode, relay_binary)


if __name__ == "__main__":
    main()
