#!/usr/bin/env python3
"""Install a checksum-pinned Delta fixture for native integration tests."""

import argparse
import hashlib
import io
from pathlib import Path
import platform
import tarfile
import urllib.request

VERSION = "0.18.2"
HASHES = {
    "aarch64-apple-darwin": "6ba38dce9f91ee1b9a24aa4aede1db7195258fe176c3f8276ae2d4457d8170a0",
    "aarch64-unknown-linux-gnu": "adf7674086daa4582f598f74ce9caa6b70c1ba8f4a57d2911499b37826b014f9",
    "x86_64-apple-darwin": "e9b5c4a9e73d2119c687e074f0d97f188e30e5fe8ae0e6d455755f74bb5cc0cf",
    "x86_64-unknown-linux-gnu": "99607c43238e11a77fe90a914d8c2d64961aff84b60b8186c1b5691b39955b0f",
}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("directory", type=Path)
    parser.add_argument("--archive", type=Path, help="Use an already downloaded archive")
    options = parser.parse_args()
    machine = {"arm64": "aarch64", "aarch64": "aarch64", "x86_64": "x86_64"}[platform.machine()]
    system = {"Darwin": "apple-darwin", "Linux": "unknown-linux-gnu"}[platform.system()]
    target = f"{machine}-{system}"
    name = f"delta-{VERSION}-{target}"
    if options.archive:
        data = options.archive.read_bytes()
    else:
        url = f"https://github.com/dandavison/delta/releases/download/{VERSION}/{name}.tar.gz"
        with urllib.request.urlopen(url, timeout=120) as response:
            data = response.read()
    assert hashlib.sha256(data).hexdigest() == HASHES[target], "Delta archive checksum mismatch"
    with tarfile.open(fileobj=io.BytesIO(data), mode="r:gz") as archive:
        member = archive.getmember(f"{name}/delta")
        assert member.isfile(), "Delta archive must contain a regular executable"
        binary = archive.extractfile(member).read()
    options.directory.mkdir(parents=True, exist_ok=True)
    destination = options.directory / "delta"
    destination.write_bytes(binary)
    destination.chmod(0o755)
    print(f"Verified Delta {VERSION} installed at {destination}", flush=True)


if __name__ == "__main__":
    main()
