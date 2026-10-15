#!/usr/bin/env python3
"""Package a native build with docs, source provenance, and a SHA-256 checksum."""

import argparse
import gzip
import hashlib
import io
import json
from pathlib import Path
import re
import tarfile
import tomllib

TARGETS = {
    "x86_64-unknown-linux-gnu",
    "aarch64-unknown-linux-gnu",
    "x86_64-apple-darwin",
    "aarch64-apple-darwin",
}


def package(root, binary, target, output, commit, tag=None, ui_binary=None):
    if target not in TARGETS:
        raise ValueError("unsupported release target")
    if not re.fullmatch(r"[0-9a-f]{40}", commit):
        raise ValueError("source commit must be a full Git SHA")
    version = tomllib.loads((root / "Cargo.toml").read_text())["package"]["version"]
    if not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?", version):
        raise ValueError("package version must be a safe SemVer string")
    if tag is not None and tag != f"v{version}":
        raise ValueError("release tag must equal v plus the Cargo.toml package version")

    name = f"fjj-v{version}-{target}"
    files = {"fjj": binary.read_bytes(), "fjjui": (ui_binary or binary.with_name("fjjui")).read_bytes()}
    files["licenses/jjui-MIT.txt"] = (root / "vendor/jjui/LICENSE").read_bytes()
    for filename in ["LICENSE", "NOTICE", "README.md", "docs/embed.md", "docs/jjui.md"]:
        files[filename] = (root / filename).read_bytes()
    files["build-info.json"] = (json.dumps({
        "name":"fjj", "version":version, "target":target, "source_commit":commit,
    }, indent=2, sort_keys=True) + "\n").encode()
    output.mkdir(parents=True, exist_ok=True)
    archive = output / f"{name}.tar.gz"
    with archive.open("wb") as destination:
        with gzip.GzipFile(filename="", fileobj=destination, mode="wb", mtime=0) as compressed:
            with tarfile.open(fileobj=compressed, mode="w") as tar:
                for filename, data in sorted(files.items()):
                    entry = tarfile.TarInfo(f"{name}/{filename}")
                    entry.size = len(data)
                    entry.mode = 0o755 if filename in ("fjj", "fjjui") else 0o644
                    entry.mtime = 0
                    tar.addfile(entry, io.BytesIO(data))
    digest = hashlib.sha256(archive.read_bytes()).hexdigest()
    archive.with_suffix(archive.suffix + ".sha256").write_text(f"{digest}  {archive.name}\n")
    return archive


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--ui-binary", type=Path)
    parser.add_argument("--target", choices=sorted(TARGETS), required=True)
    parser.add_argument("--output", type=Path, default=Path("dist"))
    parser.add_argument("--commit", required=True)
    parser.add_argument("--tag")
    args = parser.parse_args()
    try:
        archive = package(Path(__file__).resolve().parent.parent, args.binary, args.target, args.output, args.commit, args.tag, args.ui_binary)
    except (OSError, ValueError) as error:
        parser.error(str(error))
    print(archive)


if __name__ == "__main__":
    main()
