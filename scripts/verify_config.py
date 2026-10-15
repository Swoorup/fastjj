#!/usr/bin/env python3
"""Verify jj identity discovery in a built or installed embedded client."""

import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile


def main():
    binary = Path(sys.argv[1] if len(sys.argv) > 1 else "target/debug/fjj").resolve()
    with tempfile.TemporaryDirectory(prefix="fjj-config-") as directory:
        root = Path(directory)
        home = root / "home"
        home.mkdir()
        # Isolate only the child processes, including from CI's jj overrides.
        environment = {
            key: value for key, value in os.environ.items()
            if not key.startswith(("JJ_", "XDG_", "GIT_CONFIG_"))
            and key not in ("HOME", "COMPLETE", "EMAIL")
        }
        environment.update(HOME=str(home), RAYON_NUM_THREADS="2", NO_COLOR="1")

        def run(args, *, cwd=root, private=False, overrides=None):
            request = json.dumps({"cwd": str(cwd), "args": args}) if private else ""
            result = subprocess.run(
                [str(binary), "--request-stdin"] if private else [str(binary), *args],
                # The request must select the repository itself in private mode.
                cwd=root if private else cwd, env=environment | (overrides or {}),
                input=request, text=True, capture_output=True, timeout=30,
            )
            assert result.returncode == 0, result.stdout + result.stderr
            return result.stdout.strip()

        def write(path, identity):
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(
                f"[user]\nname={json.dumps(identity)}\n"
                f"email={json.dumps(identity.lower() + '@example.invalid')}\n"
            )

        def check(identity, **kwargs):
            for private in (False, True):
                assert run(["config", "get", "user.name"], private=private, **kwargs) == identity
                email = identity.lower() + "@example.invalid" if identity else ""
                assert run(["config", "get", "user.email"], private=private, **kwargs) == email

        legacy = home / "Library/Application Support/jj"
        write(legacy / "config.toml", "Legacy")
        # macOS must discover the old jj path; Linux must not adopt it.
        check("Legacy" if sys.platform == "darwin" else "")
        write(legacy / "conf.d/10-identity.toml", "LegacyFragments")
        check("LegacyFragments" if sys.platform == "darwin" else "")
        (legacy / "config.toml").unlink()
        check("LegacyFragments" if sys.platform == "darwin" else "")
        (legacy / "conf.d/10-identity.toml").unlink()

        write(home / ".jjconfig.toml", "Home")
        check("Home")
        write(home / ".config/jj/config.toml", "Xdg")
        check("Xdg")
        write(home / ".config/jj/conf.d/10-identity.toml", "Fragments")
        check("Fragments")
        # Split identity fields across layers, as in Home Manager configurations.
        (home / ".config/jj/conf.d/20-email.toml").write_text(
            'user.email="split@example.invalid"\n'
        )
        assert run(["config", "get", "user.name"]) == "Fragments"
        assert run(["config", "get", "user.email"]) == "split@example.invalid"

        xdg = root / "custom-config"
        write(xdg / "jj/config.toml", "CustomXdg")
        environment["XDG_CONFIG_HOME"] = str(xdg)
        check("CustomXdg")
        write(legacy / "config.toml", "Legacy")
        write(legacy / "conf.d/10-identity.toml", "LegacyFragments")
        identity = "LegacyFragments" if sys.platform == "darwin" else "CustomXdg"
        check(identity)

        # New commits, including the pipe-backed request used by UI clients,
        # must record the discovered identity as both author and committer.
        repo = root / "repository"
        run(["git", "init", "--no-colocate", str(repo)], private=True)

        def check_commit(expected):
            for private in (False, True):
                run(["new", "-m", "Identity regression"], cwd=repo, private=private)
                signature = run([
                    "log", "--no-graph", "-r", "@", "-T",
                    'author.name() ++ "|" ++ author.email() ++ "|" ++ '
                    'committer.name() ++ "|" ++ committer.email()',
                ], cwd=repo, private=private)
                expected_signature = f"{expected}|{expected.lower()}@example.invalid"
                assert signature == f"{expected_signature}|{expected_signature}", signature

        check_commit(identity)
        write(repo / ".jj/repo/config.toml", "Repository")
        check("Repository", cwd=repo)
        check_commit("Repository")
        (repo / ".jj/repo/config.toml").unlink()

        explicit = root / "explicit.toml"
        write(explicit, "Explicit")
        check("Explicit", overrides={"JJ_CONFIG": str(explicit)})
        check("", overrides={"JJ_CONFIG": ""})
        fragments = root / "explicit.d"
        write(fragments / "10-identity.toml", "First")
        write(fragments / "20-identity.toml", "Last")
        check("Last", overrides={"JJ_CONFIG": os.pathsep.join([str(explicit), str(fragments)])})
        check("Environment", overrides={"JJ_USER": "Environment", "JJ_EMAIL": "environment@example.invalid"})
        for private in (False, True):
            assert run([
                "--config", "user.name='Command'", "config", "get", "user.name",
            ], private=private) == "Command"

    print("jj config discovery, precedence, and commit identities verified", flush=True)


if __name__ == "__main__":
    main()
