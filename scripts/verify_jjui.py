#!/usr/bin/env python3
"""Exercise the actual UI and backend in a PTY; audit the whole UI process tree."""
import argparse
import errno
import fcntl
import json
import os
from pathlib import Path
import pty
import re
import shutil
import struct
import subprocess
import sys
import tempfile
import termios
import time


class UI:
    def __init__(self, binary, cwd, environment, root):
        self.trace = root / "jjui-process.trace"
        argv = [str(binary)]
        if sys.platform.startswith("linux"):
            assert shutil.which("strace"), "strace is required"
            argv = ["strace", "-f", "-qq", "-s", "4096", "-e", "trace=process", "-o", str(self.trace)] + argv
        self.master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 40, 160, 0, 0))
        def terminal():
            os.setsid()
            fcntl.ioctl(slave, termios.TIOCSCTTY, 0)
        environment = dict(environment, TERM="xterm-256color", COLORTERM="truecolor")
        self.child = subprocess.Popen(argv, cwd=cwd, env=environment, stdin=slave, stdout=slave, stderr=slave,
                                      preexec_fn=terminal, pass_fds=(slave,))
        os.close(slave)
        os.set_blocking(self.master, False)
        self.output = bytearray()

    def read(self):
        try:
            data = os.read(self.master, 65536)
            self.output.extend(data)
            # Answer terminal capability queries so tests never depend on probe timeouts.
            for query, answer in [(b"\x1b]11;?", b"\x1b]11;rgb:0000/0000/0000\x1b\\"),
                                  (b"\x1b]10;?", b"\x1b]10;rgb:ffff/ffff/ffff\x1b\\"),
                                  (b"\x1b[6n", b"\x1b[1;1R"), (b"\x1b[c", b"\x1b[?1;2c")]:
                if query in data:
                    os.write(self.master, answer)
        except OSError as error:
            if error.errno not in (errno.EIO, errno.EAGAIN):
                raise
        return self.text()

    def text(self):
        text = self.output.decode(errors="replace")
        return re.sub(r"\x1b(?:\[[0-?]*[ -/]*[@-~]|\][^\x07\x1b]*(?:\x07|\x1b\\))", "", text)

    def wait(self, predicate, label, timeout=25):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            text = self.read()
            if predicate(text):
                return
            if self.child.poll() is not None:
                raise AssertionError(f"UI exited during {label}: {text[-8000:]}")
            time.sleep(.025)
        raise AssertionError(f"UI timed out during {label}: {self.text()[-8000:]}")

    def send(self, data):
        os.write(self.master, data.encode())
        # Allow input dispatch/rendering before sending a different key sequence.
        end = time.monotonic() + .2
        while time.monotonic() < end:
            self.read()
            time.sleep(.01)

    def close(self):
        if self.child.poll() is None:
            self.send("\x1b")
            self.send("q")
            try:
                self.child.wait(timeout=5)
            except subprocess.TimeoutExpired:
                self.child.terminate()
                self.child.wait(timeout=5)
                raise AssertionError("UI failed to exit cleanly")
        self.read()
        os.close(self.master)
        assert self.child.returncode == 0, self.text()[-8000:]
        if self.trace.exists():
            events = self.trace.read_text()
            assert "CANARY" not in events, "private data leaked into process syscalls"
            execs = [line for line in events.splitlines() if re.search(r"\bexecve(?:at)?\(", line)]
            assert len(execs) > 3, "UI did not exercise backend subprocesses"
            for line in execs[1:]:
                assert '["fastjj", "--request-fd"]' in line or '["fastjj", "--interface-version"]' in line, line


def jj(binary, env, cwd, args):
    result = subprocess.run([str(binary), "--request-stdin"], env=env,
                            input=json.dumps({"cwd":str(cwd),"args":args}).encode(), capture_output=True, timeout=15)
    assert result.returncode == 0, (result.stdout + result.stderr).decode(errors="replace")
    return result.stdout.decode()


def git_ui(ui_binary, backend, environment, root, cwd, password, expected, cancel=False, action="f", check=None, lua_fetch=False):
    """Used by the disposable SSH fixture to test real background credential UI."""
    config = root / "jjui-config"
    config.mkdir(exist_ok=True)
    if lua_fetch:
        (config / "config.lua").write_text('function setup(config)\n  config.action("private-fetch", function()\n    local output, err = jj({"git", "fetch"})\n    assert(err == nil, err)\n    assert(output ~= nil)\n    flash("LUA_FETCH_DONE")\n  end, { key = "ctrl+g", scope = "revisions", desc = "private fetch" })\nend\n')
    environment = dict(environment, JJUI_CONFIG_DIR=str(config))
    ui = UI(ui_binary, cwd, environment, root)
    try:
        ui.wait(lambda text: "revisions" in text, "initial revisions")
        if lua_fetch:
            ui.send("\x07")
        else:
            ui.send("g")
            ui.send(action)
            ui.send("\r")
        ui.wait(lambda text: "Passphrase for SSH key" in text, "native credential panel")
        if cancel:
            ui.send("\x1b")
            ui.wait(lambda text: "prompt cancelled" in text.lower(), "credential cancellation")
        else:
            ui.send(password + "\r")
            ui.wait(lambda text: check() if check else expected in jj(backend,environment,cwd,["log","--no-graph","-r","main@origin","-T","description"]), "SSH fetch result")
        if lua_fetch:
            ui.wait(lambda text: "LUA_FETCH_DONE" in text, "Lua command result and continuation")
        assert password not in ui.text(), "credential echoed in UI"
    finally:
        ui.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", type=Path)
    parser.add_argument("ui_binary", type=Path)
    args = parser.parse_args()
    binary, ui_binary = args.binary.resolve(), args.ui_binary.resolve()
    with tempfile.TemporaryDirectory(prefix="fastjj-ui-") as directory:
        root = Path(directory)
        config = root / "config.toml"
        config.write_text("[user]\nname='UI Test'\nemail='ui@example.invalid'\n[git]\ncolocate=false\n[ui]\ncolor='never'\npaginate='never'\n")
        ui_config = root / "ui-config"
        ui_config.mkdir()
        env = dict(os.environ, JJ_CONFIG=str(config), JJUI_CONFIG_DIR=str(ui_config), RAYON_NUM_THREADS="2")
        for key in ["JJ_CONFIG_TOML", "COMPLETE", "JJUI_DEBUG"]:
            env.pop(key,None)
        repo = root / "REPOSITORY_CANARY"
        jj(binary,env,root,["git","init",str(repo)])
        (repo / "FILE_CANARY").write_text("CONTENT_CANARY\n")
        jj(binary,env,repo,["describe","-m","INITIAL_CANARY"])
        ui = UI(ui_binary,repo,env,root)
        try:
            ui.wait(lambda text: "INITIAL_CANARY" in text, "revision graph")
            ui.send("p")
            ui.wait(lambda text: "CONTENT_CANARY" in text, "streaming diff preview")
            ui.send("p")
            ui.send("\r")
            ui.send("\x18")
            ui.send("DESCRIPTION_CANARY λ")
            ui.send("\x13")
            ui.wait(lambda text: "DESCRIPTION_CANARY λ" in jj(binary,env,repo,["log","--no-graph","-r","@","-T","description"]), "inline stdin description")
            before = jj(binary,env,repo,["log","--no-graph","-r","@","-T","change_id"])
            ui.send("n")
            ui.wait(lambda text: before != jj(binary,env,repo,["log","--no-graph","-r","@","-T","change_id"]), "new revision")
            ui.send("u")
            ui.send("y")
            ui.wait(lambda text: before == jj(binary,env,repo,["log","--no-graph","-r","@","-T","change_id"]), "undo")
            ui.send("U")
            ui.send("y")
            ui.wait(lambda text: before != jj(binary,env,repo,["log","--no-graph","-r","@","-T","change_id"]), "redo")
            ui.send("D")
            ui.wait(lambda text: ".save" in text, "native terminal editor")
            ui.send("EDITOR_CANARY\n.save\n")
            ui.wait(lambda text: "EDITOR_CANARY" in jj(binary,env,repo,["log","--no-graph","-r","all()","-T","description"]), "interactive description")
            ui.send("$")
            ui.send("touch SHOULD_NOT_EXIST\r")
            ui.wait(lambda text: "external shell commands are disabled" in text, "shell privacy rejection")
            assert not (repo / "SHOULD_NOT_EXIST").exists()
        finally:
            ui.close()
    print("jjui graph, previews, stdin editing, new/undo/redo, native editor, shell rejection and argv privacy verified", flush=True)


if __name__ == "__main__":
    main()
