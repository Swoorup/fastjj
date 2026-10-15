#!/usr/bin/env python3
"""Verify external text editing, UI terminal handoff, and process arguments."""

import argparse
import json
import os
from pathlib import Path
import re
import shlex
import shutil
import subprocess
import sys
import tempfile
import time

from verify_jjui import UI, jj


EDITOR = r'''
import json, os, pathlib, signal, stat, sys
path = pathlib.Path(sys.argv[-1])
assert sys.argv[-1] == "COMMIT_EDITMSG"
assert stat.S_IMODE(path.stat().st_mode) == 0o600
assert stat.S_IMODE(pathlib.Path.cwd().stat().st_mode) == 0o700
for fd in (3, 4):
    try:
        mode = os.fstat(fd).st_mode
    except OSError:
        continue
    assert not (stat.S_ISFIFO(mode) or stat.S_ISSOCK(mode)), "backend descriptor leaked into editor"
record = dict(argv=sys.argv, cwd=os.getcwd(), pwd=os.environ.get("PWD"),
              pid=os.getpid(), parent=os.getppid(), text=path.read_text())
with open(os.environ["FJJ_TEST_EDITOR_LOG"], "a") as log:
    log.write(json.dumps(record) + "\n")
    log.flush()
hold = os.environ.get("FJJ_TEST_EDITOR_HOLD")
if hold:
    import time
    while not pathlib.Path(hold).exists():
        time.sleep(.01)
print("EXTERNAL_EDITOR_READY", flush=True)
if os.environ.get("FJJ_TEST_EDITOR_SIGNAL"):
    if os.environ["FJJ_TEST_EDITOR_SIGNAL"] == "continue":
        signal.signal(signal.SIGINT, lambda signum, frame: None)
    os.killpg(os.getpgrp(), signal.SIGINT)
if os.environ.get("FJJ_TEST_EDITOR_INTERACTIVE"):
    text = input() + "\n"
else:
    text = os.environ.get("FJJ_TEST_EDITOR_TEXT", "edited externally\n")
path.write_text(text)
print("editor diagnostics on stdout", flush=True)
sys.exit(int(os.environ.get("FJJ_TEST_EDITOR_EXIT", "0")))
'''


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", type=Path)
    parser.add_argument("--ui-binary", type=Path)
    options = parser.parse_args()
    binary = options.binary.resolve()
    with tempfile.TemporaryDirectory(prefix="fjj-editor-test-") as directory:
        root = Path(directory)
        repo = root / "CANARY_REPOSITORY"
        script = root / "editor with spaces.py"
        script.write_text(EDITOR)
        records = root / "editor.jsonl"
        config = root / "config.toml"
        config.write_text("[user]\nname='Test'\nemail='test@example.invalid'\n[git]\ncolocate=false\n[ui]\ncolor='never'\npaginate='never'\n")
        env = {key: value for key, value in os.environ.items()
               if not key.startswith("JJ_") and key not in ("VISUAL", "EDITOR", "COMPLETE", "JJUI_DEBUG")}
        editor_command = [sys.executable, str(script), "quoted argument"]
        editor_argv = [Path(sys.executable).name, *editor_command[1:], "COMMIT_EDITMSG"]
        env.update(EDITOR=shlex.join(editor_command), JJ_CONFIG=str(config), RAYON_NUM_THREADS="2",
                   FJJ_TEST_EDITOR_LOG=str(records), FJJ_TEST_EDITOR_TEXT="CANARY_MESSAGE edited\n")
        commands = 0

        def run(cwd, args, *, edits=False, success=True, overrides=None):
            nonlocal commands
            commands += 1
            trace = root / f"process-{commands}.trace"
            argv = [str(binary), "--request-stdin"]
            if sys.platform.startswith("linux"):
                assert shutil.which("strace"), "strace is required"
                argv = ["strace", "-f", "-qq", "-s", "4096", "-e", "trace=process", "-o", str(trace), *argv]
            result = subprocess.run(argv, env=env | (overrides or {}),
                                    input=json.dumps(dict(cwd=str(cwd), args=args)),
                                    capture_output=True, text=True, timeout=30)
            assert (result.returncode == 0) == success, result.stdout + result.stderr
            if trace.exists():
                events = trace.read_text()
                assert "CANARY" not in events, events
                execs = [line for line in events.splitlines() if re.search(r"\bexecve(?:at)?\(", line)]
                assert len(execs) == (3 if edits else 2), execs
                assert json.dumps(["fjj", "--editor-service"]) in execs[1], execs
                if edits:
                    assert json.dumps(editor_argv) in execs[2], execs
            return result.stdout.strip()

        run(root, ["git", "init", str(repo)])
        (repo / "CANARY_FILE").write_text("initial content\n")
        run(repo, ["describe"], edits=True)
        assert run(repo, ["log", "-r", "@", "--no-graph", "-T", "description"]) == "CANARY_MESSAGE edited"
        record = json.loads(records.read_text().splitlines()[-1])
        assert record["argv"][1:] == ["quoted argument", "COMMIT_EDITMSG"]
        assert record["cwd"] == record["pwd"]
        assert not Path(record["cwd"]).exists(), "editor temporary directory was not removed"

        # Errors must keep the original commit even if the editor wrote a file.
        run(repo, ["describe"], edits=True, success=False,
            overrides={"FJJ_TEST_EDITOR_EXIT": "7", "FJJ_TEST_EDITOR_TEXT": "must not save\n"})
        assert run(repo, ["log", "-r", "@", "--no-graph", "-T", "description"]) == "CANARY_MESSAGE edited"
        run(repo, ["config", "edit", "--repo"], edits=True,
            overrides={"FJJ_TEST_EDITOR_TEXT": 'user.name="Edited Config"\n'})
        assert run(repo, ["config", "get", "user.name"]) == "Edited Config"

        # Git operations keep using the guarded native backend with EDITOR set.
        run(repo, ["bookmark", "create", "main"])
        run(repo, ["git", "export"])
        git_dir = repo / ".jj/repo/store/git"
        (git_dir / "HEAD").write_text("ref: refs/heads/main\n")
        clone = root / "CANARY_CLONE"
        run(root, ["git", "clone", str(git_dir), str(clone)])
        run(clone, ["describe", "-m", "CANARY_NEW_MESSAGE"])
        (clone / "CANARY_FILE").write_text("content\n")
        run(clone, ["bookmark", "set", "main"])
        run(clone, ["git", "push", "--bookmark", "main"])
        run(clone, ["git", "fetch"])
        run(repo, ["util", "exec", "--", "sh", "-c", "echo CANARY"], success=False)
        run(repo, ["--config", "merge-tools.escape={program='/bin/sh',diff-args=['-c','echo CANARY']}",
                   "diff", "--from", "root()", "--tool", "escape"], success=False)

        # Observe all three live processes on macOS as well as Linux.
        hold = root / "continue"
        before = len(records.read_text().splitlines())
        process = subprocess.Popen([str(binary), "--request-stdin"],
                                   env=env | {"FJJ_TEST_EDITOR_HOLD": str(hold)},
                                   stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        process.stdin.write(json.dumps(dict(cwd=str(repo), args=["describe"])).encode())
        process.stdin.close()
        try:
            deadline = time.monotonic() + 15
            while len(records.read_text().splitlines()) == before:
                assert time.monotonic() < deadline and process.poll() is None
                time.sleep(.01)
            record = json.loads(records.read_text().splitlines()[-1])
            observed = subprocess.check_output(["ps", "-ww", "-o", "args=", "-p",
                                               f"{process.pid},{record['parent']},{record['pid']}"], text=True)
            assert "CANARY" not in observed and str(repo) not in observed, observed
            assert "--editor-service" in observed and "COMMIT_EDITMSG" in observed, observed
        finally:
            hold.touch()
            process.wait(timeout=15)
        assert process.returncode == 0, process.stderr.read().decode()

        # VISUAL beats EDITOR; JJ_EDITOR beats both. Quoting stays intact.
        for overrides in [dict(VISUAL=shlex.join(editor_command), EDITOR="nonexistent-editor"),
                          dict(JJ_EDITOR=shlex.join(editor_command), VISUAL="nonexistent-editor", EDITOR="nonexistent-editor")]:
            run(repo, ["describe"], edits=True, overrides=overrides)

        # Model editors that handle Ctrl+C and keep editing (such as Vim), and
        # editors that terminate. Isolate the foreground group from this runner.
        for behavior in ["continue", "abort"]:
            result = subprocess.run([str(binary), "--request-stdin"],
                                    env=env | {"FJJ_TEST_EDITOR_SIGNAL": behavior},
                                    input=json.dumps(dict(cwd=str(repo), args=["describe"])),
                                    start_new_session=True, capture_output=True, text=True, timeout=15)
            assert (result.returncode == 0) == (behavior == "continue"), result.stdout + result.stderr
        assert run(repo, ["log", "-r", "@", "--no-graph", "-T", "description"]) == "CANARY_MESSAGE edited"

        if options.ui_binary:
            ui_config = root / "ui-config"
            ui_config.mkdir()
            ui_env = env | {"JJUI_CONFIG_DIR": str(ui_config), "FJJ_TEST_EDITOR_INTERACTIVE": "1"}
            ui = UI(options.ui_binary.resolve(), repo, ui_env, root, binary.name, editor_argv=editor_argv)
            try:
                ui.wait(lambda text: "CANARY_MESSAGE" in text, "revision graph")
                editor_start = len(ui.text())
                ui.send("D")
                ui.wait(lambda text: "EXTERNAL_EDITOR_READY" in text, "external editor terminal handoff")
                ui.send("CANARY_UI_MESSAGE\r")
                ui.wait(lambda text: "CANARY_UI_MESSAGE" in jj(binary, env, repo, ["log", "-r", "@", "--no-graph", "-T", "description"]), "external description saved")
                ui.wait(lambda text: "visions " in text[editor_start:], "UI restored")
            finally:
                ui.close()

    print("External editor, config editing, Git transport, UI handoff, and private argv verified", flush=True)


if __name__ == "__main__":
    main()
