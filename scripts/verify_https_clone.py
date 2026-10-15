#!/usr/bin/env python3
"""Verify native Git HTTPS and the platform's default TLS certificate trust."""

import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile


def main():
    binary = Path(sys.argv[1] if len(sys.argv) > 1 else "target/debug/fjj").resolve()
    with tempfile.TemporaryDirectory(prefix="fjj-https-") as directory:
        root = Path(directory)
        config = root / "config.toml"
        config.write_text("[user]\nname='Test'\nemail='test@example.invalid'\n[ui]\npaginate='never'\ncolor='never'\n")
        environment = os.environ.copy()
        environment["JJ_CONFIG"] = str(config)
        for name in ["JJ_CONFIG_TOML", "COMPLETE"]:
            environment.pop(name, None)
        request = {"cwd":str(root), "args":["git", "clone", "https://github.com/octocat/Hello-World.git", "checkout"]}
        result = subprocess.run([str(binary), "--request-stdin"], input=json.dumps(request),
                                text=True, capture_output=True, env=environment, timeout=90)
        if result.returncode != 0:
            sys.stderr.write(result.stdout + result.stderr)
            raise SystemExit(result.returncode)
        if (root / "checkout/README").read_text().strip() != "Hello World!":
            raise SystemExit("HTTPS clone did not materialize the expected repository content")
        print("Native HTTPS clone verified with default TLS certificate verification")


if __name__ == "__main__":
    main()
