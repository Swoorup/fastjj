#!/usr/bin/env python3
"""Trace real client operations; only the initial client exec and threads may run."""

import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile


def main():
    if not shutil.which("strace"):
        sys.exit("strace is required for the process privacy audit")
    binary = Path(sys.argv[1] if len(sys.argv) > 1 else "target/debug/fastjj").resolve()
    if not binary.is_file():
        sys.exit("build fastjj before running the process privacy audit")
    with tempfile.TemporaryDirectory(prefix="fastjj-audit-") as temp:
        root = Path(temp)
        repo = root / "CANARY_REPOSITORY_NAME"
        clone = root / "CANARY_CLONE_NAME"
        config = root / "config.toml"
        config.write_text("[user]\nname='Test'\nemail='test@example.invalid'\n[ui]\npaginate='never'\ncolor='never'\n")
        environment = os.environ.copy()
        environment.update(JJ_CONFIG=str(config), RAYON_NUM_THREADS="2")
        for name in ["COMPLETE", "JJ_CONFIG_TOML"]:
            environment.pop(name, None)
        checks = 0

        def audit(cwd, args, expected_success=True):
            nonlocal checks
            trace = root / f"trace-{checks}.txt"
            result = subprocess.run(
                ["strace", "-f", "-qq", "-e", "trace=process", "-o", str(trace), str(binary), "--request-stdin"],
                input=json.dumps({"cwd":str(cwd), "args":args}), text=True,
                stdout=subprocess.PIPE, stderr=subprocess.PIPE, env=environment,
            )
            if (result.returncode == 0) != expected_success:
                raise AssertionError(f"unexpected outcome for {args[0]}: {result.stderr}")
            events = trace.read_text()
            # Sensitive request data must never appear in any process syscall.
            for canary in ["CANARY_REPOSITORY_NAME", "CANARY_CLONE_NAME", "CANARY_PRIVATE_PATH", "CANARY_DESCRIPTION"]:
                if canary in events:
                    raise AssertionError(f"private data leaked into process trace: {canary}")
            execs = [line for line in events.splitlines() if re.search(r"\bexecve(?:at)?\(", line)]
            if len(execs) != 1 or "--request-stdin" not in execs[0]:
                raise AssertionError(f"unexpected exec activity: {execs}")
            for line in events.splitlines():
                if re.search(r"\b(?:clone3?|fork|vfork)\(", line) and re.search(r"= [1-9][0-9]*$", line):
                    if "CLONE_THREAD" not in line:
                        raise AssertionError(f"child process created: {line}")
            checks += 1
            return result

        audit(root, ["git", "init", str(repo)])
        (repo / "CANARY_PRIVATE_PATH").write_text("initial\n")
        audit(repo, ["status"])
        audit(repo, ["diff", "--git"])
        audit(repo, ["describe", "-m", "CANARY_DESCRIPTION"])
        audit(repo, ["bookmark", "create", "main"])
        audit(repo, ["new"])
        audit(repo, ["git", "export"])
        git_dir = repo / ".jj/repo/store/git"
        (git_dir / "HEAD").write_text("ref: refs/heads/main\n")
        audit(root, ["git", "clone", str(git_dir), str(clone)])
        (clone / "CANARY_PRIVATE_PATH").write_text("updated\n")
        audit(clone, ["describe", "-m", "updated"])
        audit(clone, ["bookmark", "set", "main"])
        audit(clone, ["git", "push", "--bookmark", "main"])
        audit(clone, ["git", "fetch"])
        audit(clone, ["log"])
        audit(clone, ["--config", "merge-tools.escape.program=sh", "--config", "merge-tools.escape.diff-args=['-c','echo CANARY_REPOSITORY_NAME']", "diff", "--tool", "escape"], expected_success=False)
        print(f"Process privacy audit passed: {checks} commands; no child processes or private argv; worker threads allowed")


if __name__ == "__main__":
    main()
