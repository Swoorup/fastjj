import hashlib
import importlib.util
import json
from pathlib import Path
import tarfile
import tempfile
import unittest

script = Path(__file__).resolve().parent.parent / "scripts/package_release.py"
spec = importlib.util.spec_from_file_location("package_release", script)
release = importlib.util.module_from_spec(spec)
spec.loader.exec_module(release)


class ReleasePackageTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        (self.root / "Cargo.toml").write_text('[package]\nversion="0.1.0"\n')
        (self.root / "docs").mkdir()
        (self.root / "vendor/jjui").mkdir(parents=True)
        (self.root / "vendor/jjui/LICENSE").write_text("MIT license")
        (self.root / "fjjui").write_bytes(b"native UI executable")
        for name in ["LICENSE", "NOTICE", "README.md", "docs/embed.md", "docs/jjui.md"]:
            (self.root / name).write_text(name + "\n")
        self.binary = self.root / "binary"
        self.binary.write_bytes(b"executable payload")
        self.output = self.root / "out"
        self.commit = "a" * 40

    def test_portable_archive_and_checksum_for_every_target(self):
        for target in sorted(release.TARGETS):
            with self.subTest(target=target):
                archive = release.package(self.root, self.binary, target, self.output, self.commit, "v0.1.0")
                checksum = archive.with_suffix(".gz.sha256").read_text().split()
                self.assertEqual(checksum, [hashlib.sha256(archive.read_bytes()).hexdigest(), archive.name])
                with tarfile.open(archive) as tar:
                    entries = tar.getmembers()
                    self.assertEqual(len(entries), 9)
                    for entry in entries:
                        self.assertFalse(entry.name.startswith("/"))
                        self.assertNotIn("..", Path(entry.name).parts)
                        self.assertNotIn(str(self.root), entry.name)
                    binary = next(e for e in entries if e.name.endswith("/fjj"))
                    self.assertEqual(binary.mode, 0o755)
                    self.assertEqual(tar.extractfile(binary).read(), self.binary.read_bytes())
                    ui = next(e for e in entries if e.name.endswith("/fjjui"))
                    self.assertEqual(ui.mode, 0o755)
                    self.assertEqual(tar.extractfile(ui).read(), b"native UI executable")
                    info = next(e for e in entries if e.name.endswith("/build-info.json"))
                    data = json.load(tar.extractfile(info))
                    self.assertEqual(data["source_commit"], self.commit)
                    self.assertEqual(data["target"], target)

    def test_rejects_wrong_release_tag_and_untrusted_archive_names(self):
        for target, commit, tag in [
            ("x86_64-apple-darwin", self.commit, "v9.9.9"),
            ("../escape", self.commit, "v0.1.0"),
            ("x86_64-apple-darwin", "not-a-commit", "v0.1.0"),
        ]:
            with self.subTest(target=target, commit=commit, tag=tag):
                with self.assertRaises(ValueError):
                    release.package(self.root, self.binary, target, self.output, commit, tag)
        self.assertFalse(self.output.exists())

    def test_repeat_packaging_is_reproducible(self):
        path = release.package(self.root, self.binary, "aarch64-apple-darwin", self.output, self.commit)
        original = path.read_bytes()
        release.package(self.root, self.binary, "aarch64-apple-darwin", self.output, self.commit)
        self.assertEqual(path.read_bytes(), original)


if __name__ == "__main__":
    unittest.main()
