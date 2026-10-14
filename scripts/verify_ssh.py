#!/usr/bin/env python3
"""Exercise native SSH Git against disposable servers, keys, agents and PTYs.

Fixture helpers run outside fastjj. Only the client process tree is audited.
No user keys, SSH configuration or GitHub repository is modified.
"""

import argparse
import errno
import fcntl
import json
import os
from pathlib import Path
import pty
import pwd
import re
import shutil
import socket
import subprocess
import sys
import tempfile
import termios
import time


def checked(argv, **kwargs):
    result = subprocess.run(argv, capture_output=True, text=True, timeout=60, **kwargs)
    if result.returncode:
        raise AssertionError(f"fixture command failed: {argv[0]}\n{result.stdout}{result.stderr}")
    return result


def audit_trace(path):
    events = path.read_text()
    for private in ["REPOSITORY_CANARY", "KEY_CANARY", "PASSPHRASE_CANARY"]:
        assert private not in events, f"private value appeared in process syscalls: {private}"
    execs = [line for line in events.splitlines() if re.search(r"\bexecve(?:at)?\(", line)]
    assert len(execs) == 1 and "--request-stdin" in execs[0], execs
    for line in events.splitlines():
        if re.search(r"\b(?:clone3?|fork|vfork)\(", line) and re.search(r"= [1-9][0-9]*$", line):
            assert "CLONE_THREAD" in line, line


def private_command(binary, environment, cwd, args, root, password=None, expected_error=None):
    trace = root / "process.trace"
    argv = [str(binary), "--request-stdin"]
    if sys.platform.startswith("linux"):
        if not shutil.which("strace"):
            raise AssertionError("strace is required for the Linux SSH privacy checks")
        argv = ["strace", "-f", "-qq", "-s", "4096", "-e", "trace=process", "-o", str(trace)] + argv
    request = json.dumps({"cwd": str(cwd), "args": args}).encode()
    if password is None:
        result = subprocess.run(argv, input=request, capture_output=True, env=environment, timeout=60)
        output = (result.stdout + result.stderr).decode(errors="replace")
        code = result.returncode
    else:
        responses = password if isinstance(password, list) else [password]
        master, slave = pty.openpty()

        def terminal():
            os.setsid()
            fcntl.ioctl(slave, termios.TIOCSCTTY, 0)

        child = subprocess.Popen(argv, stdin=subprocess.PIPE, stdout=slave, stderr=slave,
                                 env=environment, preexec_fn=terminal, pass_fds=(slave,))
        os.set_blocking(master, False)
        captured = bytearray()
        sent = 0
        try:
            child.stdin.write(request)
            child.stdin.close()
            deadline = time.monotonic() + 60
            prompts = 0
            while True:
                try:
                    data = os.read(master, 65536)
                    captured.extend(data)
                except OSError as error:
                    if error.errno not in (errno.EIO, errno.EAGAIN):
                        raise
                text = captured.decode(errors="replace")
                found = text.count("Passphrase for SSH key")
                # Wait for echo to be disabled as well as the visible prompt.
                if found > prompts and not termios.tcgetattr(slave)[3] & termios.ECHO:
                    response = responses[min(prompts, len(responses) - 1)]
                    os.write(master, (response + "\n").encode())
                    prompts += 1
                    sent += 1
                code = child.poll()
                if code is not None:
                    while True:
                        try:
                            data = os.read(master, 65536)
                            if not data:
                                break
                            captured.extend(data)
                        except OSError as error:
                            if error.errno in (errno.EIO, errno.EAGAIN):
                                break
                            raise
                    break
                if time.monotonic() >= deadline:
                    raise AssertionError("native SSH terminal prompt or transfer timed out")
                time.sleep(.01)
            output = captured.decode(errors="replace")
            assert sent, f"encrypted-key test never exercised a terminal prompt: {output}"
            assert all(response not in output for response in responses), "key passphrase was echoed"
            assert sent >= len(responses), "passphrase retry was not exercised"
        finally:
            if child.poll() is None:
                child.kill()
            child.wait(timeout=10)
            os.close(master)
            os.close(slave)
    if sys.platform.startswith("linux"):
        audit_trace(trace)
    if expected_error:
        assert code != 0 and expected_error in output, output
    else:
        assert code == 0, output
    return output


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("binary", type=Path)
    args = parser.parse_args()
    binary = args.binary.resolve()
    sshd = shutil.which("sshd") or "/usr/sbin/sshd"
    with tempfile.TemporaryDirectory(prefix="fastjj-ssh-") as directory:
        root = Path(directory)
        config = root / "jj.toml"
        config.write_text("[user]\nname='SSH Test'\nemail='test@example.invalid'\n[ui]\npaginate='never'\ncolor='never'\n")
        environment = os.environ.copy()
        environment.update(JJ_CONFIG=str(config), RAYON_NUM_THREADS="2", GIT_CONFIG_NOSYSTEM="1")
        for name in ["SSH_AUTH_SOCK", "SSH_AGENT_PID", "JJ_CONFIG_TOML", "COMPLETE"]:
            environment.pop(name, None)
        ssh_config = root / "ssh_config"
        environment["FASTJJ_SSH_CONFIG"] = str(ssh_config)
        passphrase = "PASSPHRASE_CANARY"
        keys = {}
        for name, kind, secret in [("host", "ed25519", ""), ("disk", "ed25519", ""),
                                   ("rsa", "rsa", ""), ("encrypted", "ed25519", passphrase),
                                   ("encrypted-rsa", "rsa", passphrase), ("encrypted-ecdsa", "ecdsa", passphrase),
                                   ("agent", "ed25519", passphrase), ("wrong", "ed25519", "")]:
            path = root / ("KEY_CANARY-" + name)
            checked(["ssh-keygen", "-q", "-t", kind, "-N", secret, "-f", str(path)])
            keys[name] = path
        authorized = root / "authorized_keys"
        authorized.write_text("".join(keys[name].with_suffix(".pub").read_text() for name in ["disk", "rsa", "encrypted", "encrypted-rsa", "encrypted-ecdsa", "agent"]))
        servers = []
        agent = None
        try:
            ports = []
            for number in range(3):
                with socket.socket() as reservation:
                    reservation.bind(("127.0.0.1", 0))
                    port = reservation.getsockname()[1]
                ports.append(port)
                server_config = root / f"sshd-{number}.conf"
                server_config.write_text(f"""Port {port}
ListenAddress 127.0.0.1
HostKey {keys['host']}
PidFile {root / f'sshd-{number}.pid'}
AuthorizedKeysFile {authorized}
StrictModes no
UsePAM no
PermitRootLogin yes
PasswordAuthentication no
KbdInteractiveAuthentication no
PubkeyAuthentication yes
AllowTcpForwarding yes
LogLevel VERBOSE
""")
                log = root / f"sshd-{number}.log"
                with log.open("w") as output:
                    server = subprocess.Popen([sshd, "-D", "-e", "-f", str(server_config)], stdout=subprocess.DEVNULL, stderr=output)
                servers.append(server)
                for _ in range(100):
                    if server.poll() is not None:
                        raise AssertionError(log.read_text())
                    try:
                        with socket.create_connection(("127.0.0.1", port), timeout=.1):
                            break
                    except OSError:
                        time.sleep(.05)
                else:
                    raise AssertionError("fixture SSH server did not start")

            known_hosts = root / "known_hosts"
            host_fields = keys["host"].with_suffix(".pub").read_text().split()
            trusted = "".join(f"[127.0.0.1]:{port} {host_fields[0]} {host_fields[1]}\n" for port in ports)
            known_hosts.write_text(trusted)
            username = pwd.getpwuid(os.getuid()).pw_name
            agent_socket = root / "agent.sock"
            agent = subprocess.Popen(["ssh-agent", "-D", "-a", str(agent_socket)], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            for _ in range(100):
                if agent_socket.exists():
                    break
                time.sleep(.02)
            askpass = root / "askpass"
            askpass.write_text(f"#!/bin/sh\nprintf '%s\\n' '{passphrase}'\n")
            askpass.chmod(0o700)
            agent_environment = environment.copy()
            agent_environment.update(SSH_AUTH_SOCK=str(agent_socket), SSH_ASKPASS=str(askpass), SSH_ASKPASS_REQUIRE="force", DISPLAY="fixture")
            checked(["ssh-add", str(keys["agent"])], stdin=subprocess.DEVNULL, env=agent_environment)

            def write_config(key, use_agent=False, jumps=None, extra=""):
                include = root / "identity.conf"
                include.write_text(f'IdentityFile "{key}"\nIdentitiesOnly yes\n')
                ssh_config.write_text(f"""Host private-alias
 HostName 127.0.0.1
 Port {ports[-1]}
 User {username}
 Include "{include}"
 IdentityAgent {agent_socket if use_agent else 'none'}
 {('ProxyJump ' + jumps) if jumps else ''}
 {extra}
Host jump-one
 HostName 127.0.0.1
 Port {ports[0]}
 User {username}
 IdentityFile "{keys['disk']}"
 IdentityAgent none
Host jump-two
 HostName 127.0.0.1
 Port {ports[1]}
 User {username}
 IdentityFile "{keys['disk']}"
 IdentityAgent none
Host *
 UserKnownHostsFile "{known_hosts}"
 GlobalKnownHostsFile none
 StrictHostKeyChecking yes
 ConnectTimeout 10
""")

            # Observe the client's real argv before supplying private data.
            probe = subprocess.Popen([str(binary), "--request-stdin"], stdin=subprocess.PIPE,
                                     stdout=subprocess.PIPE, stderr=subprocess.PIPE, env=environment)
            try:
                argv = checked(["ps", "-ww", "-p", str(probe.pid), "-o", "args="]).stdout
                assert "--request-stdin" in argv and "REPOSITORY_CANARY" not in argv
                probe.communicate(json.dumps({"args": ["privacy"]}).encode(), timeout=10)
                assert probe.returncode == 0
            finally:
                if probe.poll() is None:
                    probe.kill()
                    probe.wait()

            cases = [("disk", False, None), ("rsa", False, None), ("encrypted", False, None),
                     ("encrypted-rsa", False, None), ("encrypted-ecdsa", False, None),
                     ("agent", True, None), ("disk", False, "jump-one,jump-two")]
            for index, (name, use_agent, jumps) in enumerate(cases):
                write_config(keys[name], use_agent, jumps, extra="PubkeyAcceptedAlgorithms ^rsa-sha2-512" if name == "rsa" else "")
                password = passphrase if name.startswith("encrypted") else None

                def jj(cwd, args, interactive=False, expected_error=None):
                    response = ["WRONG_PASSPHRASE_CANARY", password] if name == "encrypted" and "clone" in args else password
                    return private_command(binary, environment, cwd, args, root,
                                           password=response if interactive else None, expected_error=expected_error)

                origin = root / f"REPOSITORY_CANARY-origin-{index}"
                jj(root, ["git", "init", str(origin)])
                (origin / "hello.txt").write_text("initial\n")
                for args in [["describe", "-m", "initial"], ["bookmark", "create", "main"], ["new"], ["git", "export"]]:
                    jj(origin, args)
                store = origin / ".jj/repo/store/git"
                (store / "HEAD").write_text("ref: refs/heads/main\n")
                clone = root / f"REPOSITORY_CANARY-checkout-{index}"
                # SCP-style URLs exercise alias, User, Port and relative/absolute path handling.
                jj(root, ["git", "clone", f"private-alias:{store}", str(clone)], interactive=True)
                assert (clone / "hello.txt").read_text() == "initial\n"
                (clone / "change.txt").write_text(f"update-{index}\n")
                jj(clone, ["describe", "-m", f"push-{index}"])
                jj(clone, ["bookmark", "set", "main"])
                jj(clone, ["git", "push", "--bookmark", "main"], interactive=True)
                # An independent server-side Git read verifies what was pushed.
                assert checked(["git", "--git-dir", str(store), "show", "main:change.txt"]).stdout == f"update-{index}\n"
                # Advance the fixture's remote without disturbing a jj workspace.
                parent = checked(["git", "--git-dir", str(store), "rev-parse", "main"]).stdout.strip()
                tree = checked(["git", "--git-dir", str(store), "rev-parse", "main^{tree}"]).stdout.strip()
                commit_environment = environment.copy()
                commit_environment.update(GIT_AUTHOR_NAME="Test", GIT_AUTHOR_EMAIL="test@example.invalid", GIT_COMMITTER_NAME="Test", GIT_COMMITTER_EMAIL="test@example.invalid")
                commit = checked(["git", "--git-dir", str(store), "commit-tree", tree, "-p", parent], input=f"fetch-{index}\n", env=commit_environment).stdout.strip()
                checked(["git", "--git-dir", str(store), "update-ref", "refs/heads/main", commit])
                jj(clone, ["git", "fetch"], interactive=True)
                assert f"fetch-{index}" in jj(clone, ["log", "-r", "main@origin", "-T", "description"])
                print(f"SSH clone/fetch/push verified: {name}{' with native two-hop ProxyJump' if jumps else ''}", flush=True)

            # IdentityAgent is selected explicitly; no SSH_AUTH_SOCK is supplied.
            write_config(keys["disk"], extra="PreferredAuthentications password\n BatchMode yes")
            private_command(binary, environment, clone, ["git", "fetch"], root, expected_error="SSH authentication failed")
            write_config(keys["disk"], extra="PubkeyAuthentication no\n BatchMode yes")
            private_command(binary, environment, clone, ["git", "fetch"], root, expected_error="SSH authentication failed")
            write_config(keys["wrong"], True, extra="BatchMode yes")
            private_command(binary, environment, clone, ["git", "fetch"], root, expected_error="SSH authentication failed")
            write_config(keys["disk"], extra="BatchMode yes")
            known_hosts.write_text("")
            private_command(binary, environment, clone, ["git", "fetch"], root, expected_error="unknown SSH host key")
            wrong = keys["wrong"].with_suffix(".pub").read_text().split()
            known_hosts.write_text(f"[127.0.0.1]:{ports[-1]} {wrong[0]} {wrong[1]}\n")
            private_command(binary, environment, clone, ["git", "fetch"], root, expected_error="SSH host key changed")
            known_hosts.write_text(trusted + f"@revoked [127.0.0.1]:{ports[-1]} {host_fields[0]} {host_fields[1]}\n")
            private_command(binary, environment, clone, ["git", "fetch"], root, expected_error="SSH host key is revoked")
            known_hosts.write_text(trusted)
            write_config(keys["disk"], extra="HostKeyAlgorithms -ssh-rsa")
            checked(["ssh-keygen", "-H", "-f", str(known_hosts)])
            private_command(binary, environment, clone, ["git", "fetch"], root)
            write_config(keys["disk"], extra="HostKeyAlias server-key")
            known_hosts.write_text(f"[server-key]:{ports[-1]} {host_fields[0]} {host_fields[1]}\n")
            private_command(binary, environment, clone, ["git", "fetch"], root)
            write_config(keys["disk"], extra="StrictHostKeyChecking accept-new")
            known_hosts.write_text("")
            private_command(binary, environment, clone, ["git", "fetch"], root)
            assert host_fields[1] in known_hosts.read_text(), "accepted host key was not persisted"
            known_hosts.write_text(f"[127.0.0.1]:{ports[-1]} {wrong[0]} {wrong[1]}\n")
            private_command(binary, environment, clone, ["git", "fetch"], root, expected_error="SSH host key changed")
            known_hosts.write_text(trusted)
            write_config(keys["disk"], extra="ProxyCommand touch SHOULD_NOT_EXIST")
            private_command(binary, environment, clone, ["git", "fetch"], root, expected_error="requires an external program")
            assert not (root / "SHOULD_NOT_EXIST").exists()
            assert not (clone / "SHOULD_NOT_EXIST").exists()
            print("Identity selection, unknown/changed/revoked host keys, external-command rejection and argv privacy verified", flush=True)
        finally:
            if agent:
                agent.terminate()
                agent.wait(timeout=10)
            for server in servers:
                server.terminate()
                server.wait(timeout=10)


if __name__ == "__main__":
    main()
