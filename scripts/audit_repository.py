#!/usr/bin/env python3
"""Check tracked content for deployment artifacts, credentials and private identities.

Print paths and rule names only: never echo a detected secret into CI logs.
"""
import argparse
import ipaddress
from pathlib import Path
import re
import subprocess


def git(*args):
    return subprocess.check_output(["git", *args])


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--history", action="store_true", help="also inspect reachable commit metadata and blobs")
    args = parser.parse_args()
    failures = set()
    patterns = {
        "private key": rb"-----BEGIN (?:OPENSSH |RSA |EC )?PRIVATE KEY-----",
        "GitHub token": rb"gh[pousr]_[A-Za-z0-9]{30,}",
        "personal home path": rb"/(?:home|Users)/[A-Za-z][A-Za-z0-9_-]+/",
    }
    # Public DNS servers and the old documentation-only placeholder. New docs
    # should use SERVER_IP/HOST_IP or RFC 5737 reserved addresses instead.
    example_ips = {"1.1.1.1", "8.8.8.8", "1.2.3.4"}

    def check(path, data):
        name = Path(path).name
        if "@" in name or name.endswith((".log", ".pem", ".key")) or name == "config.toml":
            failures.add((path, "local/deployment file"))
        for label, pattern in patterns.items():
            if re.search(pattern, data):
                failures.add((path, label))
        if b"\0" in data:
            return
        for match in re.finditer(rb"(?<![\d.])(?:\d{1,3}\.){3}\d{1,3}(?![\d.])", data):
            try:
                ip = ipaddress.ip_address(match.group().decode())
            except ValueError:
                continue
            if ip.is_global and str(ip) not in example_ips:
                failures.add((path, "unexpected public IP address"))

    if args.history:
        for line in git("rev-list", "--objects", "HEAD").decode().splitlines():
            oid, _, path = line.partition(" ")
            if path and git("cat-file", "-t", oid).strip() == b"blob":
                check(path, git("cat-file", "blob", oid))
        identities = git("log", "--format=%ae%n%ce", "HEAD").decode().splitlines()
        if any(not (address.endswith("@users.noreply.github.com") or address == "noreply@github.com") for address in identities):
            failures.add(("commit metadata", "personal email address"))
    else:
        for path in git("ls-files", "-z").decode().split("\0"):
            if path and Path(path).is_file():
                check(path, Path(path).read_bytes())
    for path, rule in sorted(failures):
        print(f"{path}: {rule}")
    if failures:
        raise SystemExit(1)
    print("Repository privacy checks: PASS")


if __name__ == "__main__":
    main()
