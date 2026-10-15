import importlib.util
import os
from pathlib import Path
import pty
import subprocess
import sys
import tempfile
import tty
import unittest
from unittest import mock


script = Path(__file__).resolve().parent.parent / "scripts/verify_jjui.py"
spec = importlib.util.spec_from_file_location("verify_jjui", script)
fixture = importlib.util.module_from_spec(spec)
spec.loader.exec_module(fixture)


class TerminalFixtureTests(unittest.TestCase):
    def test_shutdown_drains_output_larger_than_the_terminal_buffer(self):
        with tempfile.TemporaryDirectory() as directory:
            master, slave = pty.openpty()
            tty.setraw(slave)
            os.set_blocking(master, False)
            ui = fixture.UI.__new__(fixture.UI)
            ui.master = master
            ui.output = bytearray()
            ui.trace = Path(directory) / "unused.trace"
            ui.child = subprocess.Popen(
                [sys.executable, "-c", "import os\nwhile os.read(0, 1) != b'q': pass\ndata = b'x' * (2 * 1024 * 1024)\nwhile data: data = data[os.write(1, data):]\n"],
                stdin=slave, stdout=slave, stderr=slave, start_new_session=True,
            )
            os.close(slave)
            try:
                # Model small PTY reads even on hosts that usually return large
                # chunks. Shutdown must not rescan the entire growing transcript
                # for every read (quadratic work on macOS).
                read = os.read
                with mock.patch.object(fixture.os, "read", side_effect=lambda fd, size: read(fd, min(size, 256))):
                    ui.close()
                self.assertEqual(ui.child.returncode, 0)
                self.assertEqual(len(ui.output), 2 * 1024 * 1024)
            finally:
                if ui.child.poll() is None:
                    ui.child.kill()
                    ui.child.wait(timeout=5)
                try:
                    os.close(master)
                except OSError:
                    pass


if __name__ == "__main__":
    unittest.main()
