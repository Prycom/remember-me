#!/usr/bin/env python3
"""Check actual UDP routing, room changes, leave and reachability diagnostics."""

import argparse
from pathlib import Path
import socket
import subprocess
import tempfile
import time


def endpoint():
    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    sock.bind(("127.0.0.1", 0))
    sock.settimeout(0.3)
    return sock


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--server-binary", type=Path,
                        default=Path("target/x86_64-unknown-linux-musl/release/remember-me-server"))
    binary = parser.parse_args().server_binary.resolve(strict=True)
    sockets = [endpoint() for _ in range(5)]
    host, old_friend, new_friend, replacement, blackhole = sockets
    with endpoint() as reservation:
        port = reservation.getsockname()[1]
    server_address = ("127.0.0.1", port)
    with tempfile.TemporaryFile(mode="w+") as log:
        server = subprocess.Popen([str(binary), "-p", str(port)], stdout=log, stderr=log)
        try:
            time.sleep(0.3)
            assert server.poll() is None, "relay did not start"
            result = subprocess.run([str(binary), "--check-server", f"127.0.0.1:{port}"],
                                    capture_output=True, text=True, timeout=10)
            assert result.returncode == 0 and "получено 3/3" in result.stdout, result.stdout + result.stderr
            print("bidirectional UDP probe: PASS", flush=True)

            def register(sock, code):
                sock.sendto(bytes([20]) + code, server_address)
                time.sleep(0.05)

            def transfer(sender, receiver, payload):
                sender.sendto(payload, server_address)
                assert receiver.recvfrom(1500) == (payload, server_address)

            register(host, b"AX3P")
            register(old_friend, b"AX3P")
            transfer(host, old_friend, b"old-room")
            transfer(old_friend, host, b"old-room-return")
            register(host, b"4UN2")  # Exactly the same socket / UDP endpoint.
            register(new_friend, b"4UN2")
            for _ in range(10):
                transfer(host, new_friend, b"new-room")
                transfer(new_friend, host, b"new-room-return")
                old_friend.sendto(b"stale-dial", server_address)
            try:
                host.recvfrom(1500)
                raise AssertionError("stale room still routes to host")
            except socket.timeout:
                pass
            print("same endpoint moving between rooms with old peer still active: PASS", flush=True)

            new_friend.sendto(bytes([22]) + b"4UN2", server_address)
            time.sleep(0.1)
            register(replacement, b"4UN2")
            transfer(host, replacement, b"slot-released")
            transfer(replacement, host, b"slot-released-return")
            print("explicit leave releases slot immediately: PASS", flush=True)

            # An open socket that never replies simulates a blocked return path.
            address = blackhole.getsockname()
            result = subprocess.run([str(binary), "--check-server", f"{address[0]}:{address[1]}"],
                                    capture_output=True, text=True, timeout=10)
            assert result.returncode != 0 and "0/3 ответов" in result.stderr, result.stdout + result.stderr
            print("missing UDP replies produce a failure, not a false connection: PASS", flush=True)
        finally:
            server.terminate()
            server.wait(timeout=3)
            for sock in sockets:
                sock.close()
            if server.returncode not in (0, -15):
                log.seek(0)
                print(log.read())


if __name__ == "__main__":
    main()
