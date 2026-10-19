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
import random
import re
import socket
import struct
import subprocess
import tempfile
import termios
import threading
import time
import tomllib
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


class LossyRelayProxy:
    """Preserve separate relay endpoints, and drop exactly the first ACCEPT."""
    def __init__(self, target):
        self.target = target
        self.front = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        self.front.bind(("127.0.0.1", 0))
        self.front.settimeout(0.1)
        self.address = self.front.getsockname()
        self.upstreams = {}
        self.stopped = threading.Event()
        self.lock = threading.Lock()
        self.dropped = False
        self.impair_audio = False
        self.audio_lost = 0
        self.threads = []
        self.thread = threading.Thread(target=self.forward, daemon=True)
        self.thread.start()

    def forward(self):
        while not self.stopped.is_set():
            try:
                data, client = self.front.recvfrom(1500)
            except socket.timeout:
                continue
            if client not in self.upstreams:
                upstream = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
                upstream.bind(("127.0.0.1", 0))
                upstream.settimeout(0.1)
                self.upstreams[client] = upstream
                thread = threading.Thread(target=self.backward, args=(upstream, client), daemon=True)
                thread.start()
                self.threads.append(thread)
            self.upstreams[client].sendto(data, self.target)

    def backward(self, upstream, client):
        audio_count = 0
        while not self.stopped.is_set():
            try:
                data, _ = upstream.recvfrom(1500)
            except socket.timeout:
                continue
            with self.lock:
                if data[:1] == b"\x03" and not self.dropped:
                    self.dropped = True
                    continue
            if self.impair_audio and data[:1] == b"\x07":
                audio_count += 1
                if audio_count % 37 == 0:
                    self.audio_lost += 1
                    continue
                if audio_count % 2 == 0:
                    time.sleep(0.012)  # alternate ~8/32 ms arrival intervals
            self.front.sendto(data, client)

    def close(self):
        self.stopped.set()
        self.thread.join(timeout=2)
        for thread in self.threads:
            thread.join(timeout=2)
        for upstream in self.upstreams.values():
            upstream.close()
        self.front.close()


def tone(path, frequency, duration, amplitude=0.02):
    samples = array.array("h", (
        int(amplitude * 32767 * math.sin(2 * math.pi * frequency * i / 48000))
        for i in range(int(duration * 48000))
    ))
    with wave.open(str(path), "wb") as wav:
        wav.setparams((1, 2, 48000, 0, "NONE", "not compressed"))
        wav.writeframes(samples.tobytes())


def bursts(path):
    # Warm AGC on quiet input, then mimic repeated brief loud laugh onsets.
    samples = array.array("h")
    offset = 0
    for amplitude, duration in [(0.006, 1.0), (0.9, 0.06), (0.006, 0.24),
                                (0.9, 0.1), (0.006, 0.6)]:
        count = round(duration * 48000)
        samples.extend(int(amplitude * 32767 * math.sin(2 * math.pi * 1000 * (offset + i) / 48000))
                       for i in range(count))
        offset += count
    with wave.open(str(path), "wb") as wav:
        wav.setparams((1, 2, 48000, 0, "NONE", "not compressed"))
        wav.writeframes(samples.tobytes())


def fading_phrases(path):
    samples = array.array("h")
    for i in range(round(3.6 * 48000)):
        t = i / 48000
        if t < 1.2:
            amplitude = 0.02
        elif t < 1.8:
            amplitude = 0.02 * 0.1 ** ((t - 1.2) / 0.6)
        elif t < 2.5:
            amplitude = 0.0
        elif t < 3.3:
            amplitude = 0.0025
        else:
            amplitude = 0.0
        samples.append(int(amplitude * 32767 * math.sin(2 * math.pi * 660 * t)))
    with wave.open(str(path), "wb") as wav:
        wav.setparams((1, 2, 48000, 0, "NONE", "not compressed"))
        wav.writeframes(samples.tobytes())


def noisy_speech(path):
    """Known speech plus hiss/key-like impacts; no recording from the mic."""
    with wave.open(str(Path(__file__).parent / "fixtures" / "jfk.wav"), "rb") as wav:
        assert wav.getparams()[:3] == (1, 2, 16000)
        pcm = array.array("h", wav.readframes(wav.getnframes()))
    voice = []
    for a, b in zip(pcm[:72000], pcm[1:72001]):
        voice.extend((a / 32768, (2*a+b) / 98304, (a+2*b) / 98304))
    scale = 0.02 / math.sqrt(sum(s*s for s in voice) / len(voice))
    rng = random.Random(42)
    samples = array.array("h")
    for i in range(7 * 48000):
        sample = rng.gauss(0, 0.002)
        # Brief, decaying broad-band switch impacts, including while talking.
        phase = i % 12480
        if phase < 180:
            sample += rng.gauss(0, 0.025) * (1 - phase/180)
        if 48000 <= i < 48000 + len(voice):
            sample += voice[i - 48000] * scale
        samples.append(round(max(-0.95, min(0.95, sample)) * 32767))
    with wave.open(str(path), "wb") as wav:
        wav.setparams((1, 2, 48000, 0, "NONE", "not compressed"))
        wav.writeframes(samples.tobytes())


def noise_metrics(samples):
    def level(block):
        return math.sqrt(sum(s*s for s in block) / len(block))
    # The file ends in >1 s of noise only. These relative-to-end windows
    # tolerate recorder/CPAL/network startup latency and the trailing 400 ms.
    background = level(samples[-round(0.9*48000):-round(0.5*48000)])
    voice = level(samples[round(2.0*48000):round(4.5*48000)])
    return background, voice


def measure(root, source_sink, output_sink, wav, audible, label, allow_pauses=False):
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
    assert all(math.isfinite(s) and abs(s) <= 0.9501 for s in samples), f"{label}: output limiter exceeded"
    # The old quiet path produced ~0.01 RMS. Require an audible normalized
    # level at default 100% input/output volume, not just a nonzero signal.
    if audible is not None:
        assert level > 0.055 if audible else level < 0.0005, f"{label}: RMS={level:.6f}"
    if audible and not allow_pauses:
        # Check the middle of every second too: a short burst followed by
        # silence must not pass merely because the overall RMS is nonzero.
        for start in range(48000, len(samples) - 48000, 48000):
            block = samples[start:start + 12000]
            assert math.sqrt(sum(s * s for s in block) / len(block)) > 0.003, label
    print(f"  {label}: RMS={level:.6f}", flush=True)
    return samples


def check_phrase_envelope(samples):
    # Align to actual decoded audio, independent of Pulse/network latency.
    onset = next(i for i, s in enumerate(samples) if abs(s) > 0.01)

    def level(start, end):
        block = samples[onset + round(start * 48000):onset + round(end * 48000)]
        assert len(block) == round((end - start) * 48000)
        return math.sqrt(sum(s * s for s in block) / len(block))

    normal = level(0.5, 1.0)
    tail = level(1.65, 1.78)
    pause = level(2.15, 2.4)
    new_quiet_phrase = level(2.65, 3.1)
    assert tail < normal * 0.25, f"AGC raised fading tail: {tail} vs {normal}"
    assert pause < 0.0005, f"pause amplified: {pause}"
    assert new_quiet_phrase > 0.06, f"new quiet phrase suppressed: {new_quiet_phrase}"
    print(f"  phrase envelope: steady={normal:.4f}, tail={tail:.4f}, "
          f"pause={pause:.6f}, next quiet phrase={new_quiet_phrase:.4f}: PASS", flush=True)


def scenario(binary, root, mode, relay_binary=None, drop_first_accept=False):
    root.mkdir()
    prefix = f"rm_voice_test_{os.getpid()}_{mode}"
    names = {key: f"{prefix}_{key}" for key in ("mic_a", "mic_b", "out_a", "out_b")}
    modules, clients = [], []
    relay = None
    proxy = None
    relay_log = None
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
            relay_log = (root / "relay.log").open("wb")
            relay = subprocess.Popen([*relay_command, "-p", str(relay_port)],
                                     stdout=relay_log, stderr=relay_log)
            time.sleep(0.3)
            if drop_first_accept:
                proxy = LossyRelayProxy(("127.0.0.1", relay_port))
                server = f"{proxy.address[0]}:{proxy.address[1]}"
        for who in ("a", "b"):
            cfg = root / who / "remember-me" / "config.toml"
            cfg.parent.mkdir(parents=True)
            cfg.write_text(f'name = "{who}"\nport = {port}\n'
                           'noise_gate = true\n'
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
        if proxy:
            assert proxy.dropped
            host.wait_log("повторяем ACCEPT")
            print("recovery after first ACCEPT is lost: PASS", flush=True)
        time.sleep(0.5)
        short, long = root / "short.wav", root / "long.wav"
        quiet, loud = root / "quiet.wav", root / "loud.wav"
        transient = root / "bursts.wav"
        phrases = root / "phrases.wav"
        tone(short, 440, 1.0)
        tone(long, 660, 6.0)
        tone(quiet, 440, 1.5, amplitude=0.0025)
        tone(loud, 440, 1.0, amplitude=0.8)
        bursts(transient)
        fading_phrases(phrases)
        print(mode + ":", flush=True)
        measure(root, names["mic_a"], names["out_b"], short, True, "A → B after idle")
        time.sleep(1.0)
        measure(root, names["mic_b"], names["out_a"], short, True, "B → A after pause")
        measure(root, names["mic_a"], names["out_b"], quiet, True, "very quiet mic at 100% normalized")
        measure(root, names["mic_a"], names["out_b"], loud, True, "sudden loud mic limited")
        measure(root, names["mic_a"], names["out_b"], transient, True, "repeated loud onsets after quiet speech")
        check_phrase_envelope(measure(root, names["mic_a"], names["out_b"], phrases,
                                     True, "fading phrase, silence, new quiet phrase", allow_pauses=True))
        # Exercise the actual crossterm parser with Kitty shifted characters
        # and Alacritty-style repeat events, rather than only typed Rust keys.
        host.key("t")
        host.key("\x1b[97:65;2u\x1b[59:58;2u\x1b[1078:1046;2u123")
        host.key("\x1b[127;1:2u" * 3)
        host.key("\r")
        time.sleep(0.3)
        own_history = root / "a" / "remember-me" / "chat-b.log"
        peer_history = root / "b" / "remember-me" / "chat-a.log"
        assert "@rm2\t1\ta\tA:Ж" in own_history.read_text()
        assert "@rm2\t0\ta\tA:Ж" in peer_history.read_text()
        print("  actual terminal Shift/punctuation/Unicode/repeating Backspace + named history: PASS", flush=True)

        host.key("tDraft:")
        host.key("\x1bOQ")  # F2, even while composing a message
        time.sleep(0.2)
        host.key("\x1b[B" * 5 + "\x1b[D")  # VOX threshold 25 -> 20
        time.sleep(0.2)
        cfg_path = root / "a" / "remember-me" / "config.toml"
        assert tomllib.loads(cfg_path.read_text())["vad_threshold"] == 20
        measure(root, names["mic_a"], names["out_b"], short, True, "voice continues while editing VOX during call")
        host.key("\x1b[B" * 7 + "\r")  # optional filter
        time.sleep(0.2)
        assert tomllib.loads(cfg_path.read_text())["voice_filter"] is True
        measure(root, names["mic_a"], names["out_b"], short, True, "light voice filter enabled live")
        host.key("\x1b")
        time.sleep(0.2)
        host.key("\r")
        time.sleep(0.3)
        assert "@rm2\t1\ta\tDraft:" in own_history.read_text(), "settings discarded the chat draft"
        assert "@rm2\t0\ta\tDraft:" in peer_history.read_text(), "settings disconnected the session"
        noisy = root / "noisy-speech.wav"
        noisy_speech(noisy)
        before = noise_metrics(measure(root, names["mic_a"], names["out_b"], noisy,
                                       None, "speech + background/keys, suppression OFF", allow_pauses=True))
        host.key("\x1bOQ")  # settings selection was left on the light filter
        time.sleep(0.2)
        host.key("\x1b[B\r")
        time.sleep(0.2)
        assert tomllib.loads(cfg_path.read_text())["noise_suppression"] is True
        after = noise_metrics(measure(root, names["mic_a"], names["out_b"], noisy,
                                      None, "speech + background/keys, suppression ON", allow_pauses=True))
        assert after[0] < max(0.0005, before[0] * 0.15), f"residual background: {before} -> {after}"
        assert after[1] > 0.04, f"speech damaged: {after}"
        print(f"  denoiser: background {before[0]:.6f} -> {after[0]:.6f}, "
              f"speech {before[1]:.4f} -> {after[1]:.4f}: PASS", flush=True)
        host.key("\r")  # exact bypass: existing synthetic-tone tests still pass
        time.sleep(0.2)
        assert tomllib.loads(cfg_path.read_text())["noise_suppression"] is False
        host.key("\x1b")
        time.sleep(0.2)
        if proxy:
            proxy.impair_audio = True
            measure(root, names["mic_a"], names["out_b"], long, True, "voice with jitter and isolated lost packets")
            proxy.impair_audio = False
            assert proxy.audio_lost > 0
            joiner.wait_log("пропущенных кадров")
        host.key("m")
        joiner.wait_log("rx status mute=true deaf=false")
        time.sleep(0.3)
        measure(root, names["mic_a"], names["out_b"], short, False, "mute")
        host.key("m")
        time.sleep(0.3)
        pongs_before = [client.log().count("rx PONG") for client in clients]
        measure(root, names["mic_a"], names["out_b"], long, True, "unmute + continuous voice")
        assert all(client.log().count("rx PONG") > before
                   for client, before in zip(clients, pongs_before)), "keepalive starved during audio"
        joiner.key("d")
        host.wait_log("rx status mute=true deaf=true")
        time.sleep(0.3)
        measure(root, names["mic_a"], names["out_b"], short, False, "deafen")
        joiner.key("d")
        time.sleep(0.3)
        measure(root, names["mic_a"], names["out_b"], short, True, "undeafen")
        measure(root, names["mic_b"], names["out_a"], short, True, "undeafen restores microphone")
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
        if relay is not None:
            server_output = (root / "relay.log").read_text(errors="replace")
            assert server_output.count(" вышел") >= 2, "relay membership did not leave after hangup/cancellation: " + server_output[-4000:]
            print("  hangup and cancelled host release both relay memberships: PASS", flush=True)
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
        if relay_log is not None:
            relay_log.close()
        if proxy is not None:
            proxy.close()
        for module in reversed(modules):
            pulse("unload-module", module)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=Path("target/release/remember-me"))
    parser.add_argument("--mode", choices=("direct", "relay", "both"), default="both")
    parser.add_argument("--relay-binary", type=Path,
                        help="use a standalone remember-me-server for relay mode")
    parser.add_argument("--drop-first-accept", action="store_true",
                        help="simulate losing the initial ACCEPT in relay mode")
    args = parser.parse_args()
    binary = args.binary.resolve(strict=True)
    relay_binary = args.relay_binary.resolve(strict=True) if args.relay_binary else None
    with tempfile.TemporaryDirectory(prefix="remember-me-voice-") as tmp:
        for mode in ("direct", "relay") if args.mode == "both" else (args.mode,):
            scenario(binary, Path(tmp) / mode, mode, relay_binary, args.drop_first_accept)


if __name__ == "__main__":
    main()
