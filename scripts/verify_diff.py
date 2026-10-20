#!/usr/bin/env python3
"""Exercise real Delta, jjui diff/preview panes, and private process arguments."""

import argparse
import fcntl
import json
import os
from pathlib import Path
import re
import struct
import subprocess
import sys
import tempfile
import termios
import time

from verify_jjui import UI


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", type=Path)
    parser.add_argument("--ui-binary", type=Path, required=True)
    parser.add_argument("--delta", type=Path, required=True)
    options = parser.parse_args()
    binary, delta = options.binary.resolve(), options.delta.resolve()
    with tempfile.TemporaryDirectory(prefix="fjj-delta-test-") as directory:
        root = Path(directory)
        home = root / "home"
        home.mkdir()
        # Check that user Delta styling is still loaded, while its pager cannot
        # start inside a captured preview.
        (home / ".gitconfig").write_text("[delta]\n  line-numbers = true\n  paging = always\n")
        config = root / "jj.toml"
        delta_args = ["--dark", "--side-by-side"]
        config.write_text(
            '[user]\nname="Test"\nemail="test@example.invalid"\n'
            '[git]\ncolocate=false\n[ui]\npaginate="never"\n'
            f'pager={json.dumps([str(delta), *delta_args])}\ndiff-formatter=":git"\n'
        )
        env = {key: value for key, value in os.environ.items()
               if not key.startswith(("JJ_", "XDG_", "GIT_", "DELTA_"))
               and key not in ("COMPLETE", "JJUI_DEBUG")}
        env.update(HOME=str(home), JJ_CONFIG=str(config), JJ_EDITOR=":builtin",
                   RAYON_NUM_THREADS="2", DELTA_PAGER="PAGER_CANARY_MUST_NOT_RUN")
        repo = root / "REPOSITORY_CANARY"
        counter = 0

        def run(args, *, cwd=repo, render=False, error=None, overrides=None):
            nonlocal counter
            counter += 1
            trace = root / f"diff-{counter}.trace"
            argv = [str(binary), "--request-stdin"]
            if sys.platform.startswith("linux"):
                argv = ["strace", "-f", "-qq", "-s", "4096", "-e", "trace=process", "-o", str(trace), *argv]
            result = subprocess.run(argv, env=env | (overrides or {}),
                                    input=json.dumps(dict(args=args, cwd=str(cwd))),
                                    text=True, capture_output=True, timeout=30)
            if error:
                assert result.returncode != 0 and error in result.stderr, result.stdout + result.stderr
            else:
                assert result.returncode == 0, result.stdout + result.stderr
            if trace.exists():
                events = trace.read_text()
                assert "CANARY" not in events, events
                execs = [line for line in events.splitlines() if re.search(r"\bexecve(?:at)?\(", line)]
                assert len(execs) == 2 + int(render), execs
                assert json.dumps(["fjj", "--diff-service"]) in execs[1], execs
                for event in execs[2:]:
                    assert '["delta", "--paging=never", "--width=' in event, execs
                    assert str(repo) not in event and str(home) not in event, execs
            return result.stdout

        run(["git", "init", str(repo)], cwd=root)
        source = repo / "FILE_CANARY.rs"
        source.write_text('// before\n')
        run(["describe", "-m", "BASE_CANARY"])
        run(["new", "-m", "DELTA_CHANGE_CANARY"])
        source.write_text('//DELTA_CANARY\n')
        other = repo / "space λ CANARY.txt"
        other.write_text("OTHER_CANARY\n")

        output = run(["diff", "--color=always"], render=True)
        assert "\x1b[" in output and "DELTA_CANARY" in output and "FILE_CANARY.rs" in output, output
        assert "DELTA_CANARY" in run(["difftool", "--color=always"], render=True)
        assert "DELTA_CANARY" in run(["diff", "--tool", "delta", "--color=always"], render=True)
        filtered = run(["diff", "--color=always", "FILE_CANARY.rs"], render=True)
        assert "DELTA_CANARY" in filtered and "OTHER_CANARY" not in filtered, filtered
        assert "DELTA_CANARY" in run(["show", "--color=always"], render=True)
        raw = run(["diff", "--git", "--color=never"])
        assert raw.startswith("diff --git ") and "\x1b[" not in raw, raw
        uncolored = run(["diff", "--color=never"])
        assert "\x1b[" not in uncolored and "DELTA_CANARY" in uncolored, uncolored
        assert "FILE_CANARY.rs" in run(["diff", "--summary"])
        run(["describe", "-m", "DESC_CANARY"])
        evolution = run(["evolog", "-p", "-n", "1", "--color=always"], render=2)
        assert "DESC_CANARY" in evolution and "\x1b[" in evolution, evolution
        run(["describe", "-m", "DELTA_CHANGE_CANARY"])
        run(["--config", 'merge-tools.escape={program="delta",diff-args=["--config","CONFIG_CANARY"]}',
             "diff", "--tool", "escape"], error="not authorized")
        run(["--config", 'merge-tools.escape={program="/bin/sh",diff-args=["-c","echo SHELL_CANARY"]}',
             "diff", "--tool", "escape"], error="Operation not permitted")

        # A configured formatter works without a pager setting as well.
        direct = root / "direct.toml"
        direct.write_text(config.read_text().replace(
            f'pager={json.dumps([str(delta), *delta_args])}\ndiff-formatter=":git"',
            f'pager=":builtin"\ndiff-formatter={json.dumps([str(delta), "--dark", "$left", "$right"])}'))
        assert "DELTA_CANARY" in run(["diff", "--color=always"], render=True,
                                         overrides={"JJ_CONFIG": str(direct)})
        named = root / "named.toml"
        named.write_text(direct.read_text().replace(
            f'diff-formatter={json.dumps([str(delta), "--dark", "$left", "$right"])}',
            'diff-formatter="pretty"\n[merge-tools.pretty]\n'
            f'program={json.dumps(str(delta))}\ndiff-args=["--width=$width", "--dark", "$left", "$right"]'))
        assert "DELTA_CANARY" in run(["diff", "--color=always"], render=True,
                                    overrides={"JJ_CONFIG": str(named)})

        # Hold a renderer so ps can observe every process on macOS as well as
        # Linux. No original filename, directory, or message may enter argv.
        fake_dir = root / "fake"
        fake_dir.mkdir()
        fake = fake_dir / "delta"
        records = root / "renderer.json"
        proceed = root / "continue"
        fake.write_text(f"#!{sys.executable}\n" + r'''
import json, os, pathlib, stat, sys, time
assert stat.S_IMODE(pathlib.Path.cwd().stat().st_mode) == 0o700
assert stat.S_IMODE(os.fstat(0).st_mode) == 0o600
for fd in (3, 4):
    try:
        mode = os.fstat(fd).st_mode
    except OSError:
        continue
    assert not (stat.S_ISFIFO(mode) or stat.S_ISSOCK(mode)), "backend descriptor leaked"
try:
    terminal = os.open("/dev/tty", os.O_RDONLY | os.O_NONBLOCK)
except OSError:
    pass
else:
    os.close(terminal)
    raise AssertionError("renderer retained the controlling terminal")
assert os.environ["FJJ_DELTA_SETTING"] == "trusted"
text = sys.stdin.read()
record = dict(pid=os.getpid(), parent=os.getppid(), argv=sys.argv,
              cwd=os.getcwd(), pwd=os.environ.get("PWD"), patch=text)
log = pathlib.Path(os.environ["FJJ_DIFF_LOG"])
temporary = log.with_suffix(".tmp")
temporary.write_text(json.dumps(record))
temporary.replace(log)
while not pathlib.Path(os.environ["FJJ_DIFF_CONTINUE"]).exists():
    time.sleep(.01)
print(text)
sys.exit(int(os.environ.get("FJJ_DIFF_EXIT", "0")))
''')
        fake.chmod(0o755)
        fake_config = root / "fake.toml"
        fake_config.write_text(config.read_text().replace(
            f'pager={json.dumps([str(delta), *delta_args])}',
            f'pager={{command={json.dumps([str(fake), *delta_args])},'
            'env={FJJ_DELTA_SETTING="trusted"}}'))
        fake_env = dict(JJ_CONFIG=str(fake_config), FJJ_DIFF_LOG=str(records),
                        FJJ_DIFF_CONTINUE=str(proceed), TMPDIR=str(repo))
        process = subprocess.Popen([str(binary), "--request-stdin"], env=env | fake_env,
                                   stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        process.stdin.write(json.dumps(dict(cwd=str(repo), args=["diff", "--color=always"])).encode())
        process.stdin.close()
        try:
            deadline = time.monotonic() + 15
            while not records.exists():
                assert time.monotonic() < deadline and process.poll() is None
                time.sleep(.01)
            record = json.loads(records.read_text())
            assert "FILE_CANARY.rs" in record["patch"] and record["cwd"] == record["pwd"], record
            observed = subprocess.check_output(["ps", "-ww", "-o", "args=", "-p",
                                               f"{process.pid},{record['parent']},{record['pid']}"], text=True)
            assert "CANARY" not in observed and str(repo) not in observed, observed
            assert "--diff-service" in observed and "--paging=never" in observed, observed
        finally:
            proceed.touch()
            process.wait(timeout=15)
        assert process.returncode == 0, process.stderr.read().decode()
        assert not Path(record["cwd"]).exists(), "diff temporary directory was not cleaned"
        original = source.read_bytes()
        run(["diff", "--color=always"], render=True, error="Delta failed",
            overrides=fake_env | {"FJJ_DIFF_EXIT": "7"})
        assert source.read_bytes() == original

        ui_config = root / "ui-config"
        ui_config.mkdir()
        ui = UI(options.ui_binary.resolve(), repo, env | {"JJUI_CONFIG_DIR": str(ui_config)},
                root, binary.name, delta_args=delta_args)
        try:
            ui.wait(lambda text: "DELTA_CHANGE_CANARY" in text, "revision graph")
            start = len(ui.text())
            ui.send("d")
            ui.wait(lambda text: "DELTA_CANARY" in text[start:], "Delta diff viewer")
            ui.send("\x1b")
            start = len(ui.text())
            ui.send("p")
            ui.wait(lambda text: "DELTA_CANARY" in text[start:], "Delta revision preview")
            source.write_text('//FRESH_CANARY\n')
            start = len(ui.text())
            ui.send("\x12")  # Ctrl+R
            # The terminal can repaint only the changed prefix, retaining the
            # unchanged CANARY suffix already on screen.
            ui.wait(lambda text: "FRESH_" in text[start:], "working-copy preview refresh")
            # Resizing must regenerate width-sensitive Delta output.
            start = len(ui.text())
            fcntl.ioctl(ui.master, termios.TIOCSWINSZ, struct.pack("HHHH", 45, 180, 0, 0))
            ui.wait(lambda text: "FRESH_" in text[start:], "resized Delta preview")
            ui.send("p")
            ui.send("l")
            ui.wait(lambda text: "FILE_CANARY.rs" in text, "file details")
            start = len(ui.text())
            ui.send("p")
            ui.wait(lambda text: "FRESH_CANARY" in text[start:], "Delta file preview")
            ui.send("p")
            ui.send("\x1b")
        finally:
            ui.close()
    print("Real Delta diffs, difftool, filtering, jjui diff/preview/refresh/resize, and private argv verified", flush=True)


if __name__ == "__main__":
    main()
