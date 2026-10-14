# fastjj

A Rust Jujutsu client that performs repository operations in the current process
and allows worker threads, while preventing subprocesses from exposing repository
names, paths, or remote URLs in their command lines.

fastjj embeds **jj 0.37.0**, with an in-process libgit2 implementation of its Git
transport. It includes **fastjjui**, a privacy-preserving fork of jjui 0.10.11. It works with jj's repository format and keeps jj's commands, revsets,
templates, automatic working-copy snapshots, operation history, bookmarks,
rebasing, and conflict handling. It does not launch an installed `jj` or `git`.

**Supported platforms:** Linux and macOS, on Intel/x86_64 and ARM64/Apple Silicon.
Linux uses synchronized seccomp filters; macOS uses a process-wide Seatbelt
profile denying fork and exec. Platforms that cannot install their kernel policy
are rejected before loading repository configuration. There is no privacy opt-out.

## Build and install

Install Rust using [rustup](https://rustup.rs). The toolchain is pinned in
`rust-toolchain.toml`. A C compiler, make, Perl, and pkg-config are required to
build the bundled libgit2, libssh2, and OpenSSL dependencies. On Debian/Ubuntu:

```sh
sudo apt-get install build-essential pkg-config perl
cargo build --release --locked
cargo install --path . --locked
fastjj privacy
```

On macOS, install Xcode Command Line Tools with `xcode-select --install` and
pkg-config with `brew install pkg-config`, then run the same Cargo commands.
The built executable is `target/release/fastjj`. Source installation is from this
checkout; no crates.io release is published. TLS verification remains enabled.

## jjui

Run `fastjjui` inside your repository. It supports jjui's graph, previews,
bookmarks, history editing, workspaces, and Git fetch/push while sending command
arguments and credentials over private inherited descriptors. Install it beside
`fastjj`; both binaries are included in releases and the Nix package.

```sh
nix run github:Swoorup/fastjj#jjui
```

Use this bundled fork for private UI operation. Unmodified jjui still exposes
its backend command arguments, even when `jj` is a symlink to fastjj.
[The jjui guide](docs/jjui.md) covers supported workflows, SSH credential prompts,
configuration, source builds, and the restrictions on external shell actions.

## Nix

With Nix flakes enabled, run directly from this repository or install into your
profile:

```sh
nix run github:Swoorup/fastjj -- --version
nix run github:Swoorup/fastjj -- status
nix profile install github:Swoorup/fastjj
```

The default package downloads a public GitHub release binary with a pinned
SHA-256 checksum; it does not compile fastjj or Rust dependencies. It supports
`x86_64-linux`, `aarch64-linux`, `x86_64-darwin`, and `aarch64-darwin`. Nix downloads
the required packaging tools and runtime libraries as well. Linux executables
have their interpreter and library paths patched for NixOS. The installed
`bin/fastjj` is a native executable with no launcher script. macOS requires 13.0
or later.

To use it from a NixOS, nix-darwin, or Home Manager flake, add the input:

```nix
inputs.fastjj.url = "github:Swoorup/fastjj";
```

Pass `inputs` to your modules through `specialArgs` (or `extraSpecialArgs` for
Home Manager), then add the package to `environment.systemPackages` or
`home.packages`:

```nix
{ inputs, pkgs, ... }: {
  environment.systemPackages = [
    inputs.fastjj.packages.${pkgs.stdenv.hostPlatform.system}.default
  ];
}
```

`packages.<system>.default`, `.fastjj`, and `.fastjjui` select the binary
release recorded in [`nix/binary-release.json`](nix/binary-release.json).
Following `main` updates the packaging; it selects a newer binary only when that
manifest is updated. Your consumer flake lock pins the repository revision.
Maintainers should publish a new version, update all four archive hashes and
the source commit in the manifest, and run `nix flake check` before pushing.
The **Publish verified binaries** workflow can publish an existing successful
**Release binaries** run without rebuilding: it checks the run's source commit,
all archive hashes, and embedded provenance against the manifest before upload.
Create the version tag at that source commit before dispatching the workflow.
Interrupted uploads can be resumed; existing assets must match exactly.

For private arguments, run `nix run github:Swoorup/fastjj -- --request-stdin < request.json`
using the [request format below](#keep-the-clients-own-arguments-private).
Ordinary command arguments are visible to process observers, including in Nix's
own command line.

## Release binaries

The [Release binaries workflow](https://github.com/Swoorup/fastjj/actions/workflows/release.yml)
builds and tests optimized binaries on native runners for:

| OS | Architecture | Target |
| --- | --- | --- |
| Linux | x86_64 | `x86_64-unknown-linux-gnu` |
| Linux | ARM64 | `aarch64-unknown-linux-gnu` |
| macOS | Intel | `x86_64-apple-darwin` |
| macOS | Apple Silicon | `aarch64-apple-darwin` |

Every push to `main` and manual workflow run uploads downloadable build artifacts.
Version tags such as `v0.2.1` additionally publish all four archives to
[GitHub Releases](https://github.com/Swoorup/fastjj/releases), after every target
passes its optimized tests. The tag must match the version in `Cargo.toml`.
Published assets are never overwritten: a repeated tag build must match the
existing assets, or the publish job fails and a new version is needed. No tag is
created by the workflow itself.

Each `.tar.gz` includes `fastjj` and `fastjjui`, documentation, licenses, and
`build-info.json` recording the exact source commit. An adjacent `.sha256` file
provides its checksum. After extracting an archive, install both executables
in the same directory on your PATH and run `fastjj privacy`.

```sh
# Linux
sha256sum --check fastjj-v0.2.1-x86_64-unknown-linux-gnu.tar.gz.sha256
# macOS (choose the matching architecture's archive)
shasum -a 256 --check fastjj-v0.2.1-aarch64-apple-darwin.tar.gz.sha256
```

Linux binaries are built on Ubuntu 24.04 and need a compatible glibc installation.
macOS binaries use a macOS 13.0 deployment target and are tested on macOS 15.
They are not Developer ID signed or notarized.

## Use jj commands

```sh
fastjj git init
fastjj status
fastjj diff --git
fastjj describe -m 'Implement the feature'
fastjj bookmark create main -r @
fastjj new
fastjj log
fastjj rebase -r @ -d main
fastjj undo
```

Run from the desired workspace to avoid putting its path in argv. Messages and
file arguments you type normally are still visible in **fastjj's own** argv.
For private arguments, use the stdin interface below.

`fastjj --help` and `fastjj help <command>` provide the embedded jj command
reference. Normal jj user/repository configuration, aliases, identity, revsets,
templates, color, and output remain available. The client always selects native
Git transport, the built-in pager, text and diff/merge editors, and filesystem scanning
without Watchman. Configuration cannot enable child processes.

## Keep the client's own arguments private

Create a JSON request file with restrictive permissions. Its `args` contain jj
arguments; `cwd` optionally chooses the working directory without a `-R` argument.

```json
{
  "cwd": "/private/customer/project",
  "args": ["git", "clone", "https://example.org/private/project.git", "./checkout"]
}
```

```sh
chmod 600 request.json
fastjj --request-stdin < request.json
```

Process observers see `fastjj --request-stdin`. The request passes directly into
jj as an in-memory argument vector; fastjj never starts a process with its data.
The shell does not pass the redirection's filename to fastjj. Avoid a shell
command that includes the JSON or private data in its own arguments.

The request is one JSON document, bounded to 1 MiB. `args` is required and must
be an array of strings. Unknown fields and NUL bytes are rejected. After receiving
EOF, fastjj executes one command and returns its jj exit status. Standard output
and error remain ordinary jj output, not JSON. Parsing errors do not echo request
contents. Private mode consumes stdin; commands that need stdin again should use
their file or message options instead. The built-in text editor reads the
controlling terminal directly and therefore also works with private requests.

## Built-in description editor

`fastjj describe` and other commands that edit descriptions open a native,
line-oriented editor on the controlling terminal. It displays the current text
and accepts replacement lines, including Unicode. Finish with `.save`, keep the
original with `.keep`, or abort with `.cancel`. `.undo` removes the last entered
line; `.clear` removes all entered lines. Double the first dot to enter a literal
command line (for example `..save` inserts `.save`). No editor program starts.
In scripts without a controlling terminal, pass `-m` instead.

## Git and external tools

Clone, fetch, push, local repositories, HTTP(S), and SSH run in the same process.
The native SSH smart transport connects libgit2 to libssh2 without invoking
`git`, `ssh`, a shell, credential helpers, or pinentry. It reads SSH configuration
from `~/.ssh/config` and `/etc/ssh/ssh_config`, with user settings taking priority.

SSH supports host aliases and patterns, `HostName`, `User`, `Port`, ordered
`IdentityFile` entries, `IdentitiesOnly`, `IdentityAgent` (including `none`),
`Include`, and native `Match host`, `originalhost`, `user`, and `localuser`.
Paths support quoting, `~/`, and `%d`, `%h`, `%n`, `%r`, `%p`, and `%%` tokens.
Algorithm lists (including `+`, `-`, and `^` modifiers) for `KexAlgorithms`,
`HostKeyAlgorithms`, `PubkeyAcceptedAlgorithms`, `Ciphers`, and `MACs`,
compression, and `ConnectTimeout` are supported. `PreferredAuthentications`,
`PubkeyAuthentication`, `PasswordAuthentication`, and
`KbdInteractiveAuthentication` control native key, password, and terminal
keyboard-interactive authentication.
`FASTJJ_SSH_CONFIG` can select a separate SSH configuration file; this replaces
the two default files. Its value is an environment variable, outside argv privacy.

Ed25519, ECDSA, and RSA private keys are read directly. Encrypted keys prompt on
the controlling terminal with echo disabled, also in `--request-stdin` mode.
Passphrases are cached only for the current invocation and their buffers are
cleared on drop. `BatchMode yes` disables terminal prompts. Existing agents can
provide unlocked keys or hardware-backed identities; fastjj never starts an
agent. On macOS, `UseKeychain yes` also tries accessible legacy OpenSSH keychain
entries through the native Security framework. Apple's protected keychain items
and hardware key operations should be accessed through an existing SSH agent.

`UserKnownHostsFile`, `GlobalKnownHostsFile`, `HostKeyAlias`, hashed host entries,
and revoked keys are supported. `StrictHostKeyChecking yes` requires a trusted
key; `ask` (the default) prompts with its SHA256 fingerprint; `accept-new` stores
previously unknown keys. Changed or revoked keys always fail. Host verification
cannot be disabled. Verify a new GitHub fingerprint against GitHub's published
host keys before accepting it.

`ProxyJump` supports aliases, explicit users/ports, and multiple hops. Each hop
authenticates and verifies its host key. SSH forwarding uses bounded buffers and
worker threads; no proxy subprocess starts. Cyclic configurations fail.
`ProxyCommand`, `Match exec`, local/remote commands, and `KnownHostsCommand`
cannot run. SSH certificates are not implemented;
an explicit request for these returns an error. Other OpenSSH features such as
connection sharing and background forwarding are outside this Git transport.

For GitHub, this configuration selects a dedicated key:

```ssh-config
Host github-work
    HostName github.com
    User git
    IdentityFile ~/.ssh/github-work
    IdentitiesOnly yes
```

Register the corresponding public key with GitHub and configure a remote such as
`github-work:OWNER/REPO.git`. Use a private stdin request when setting that URL.
Then run `fastjj git fetch` or `fastjj git push` inside the repository. For SSH
over port 443, use `HostName ssh.github.com` and `Port 443` with its verified host
key. Git credential helpers for HTTPS still cannot execute; use native terminal
credentials instead.

Commands whose purpose is running other programs (`util exec`, `fix`, `util gc`,
and `config edit`) return an explicit privacy error. External text editors,
diff/merge tools, signing programs, hooks, and credential helpers also cannot run.
Use the built-in text editor or `describe -m` / `commit -m`, `config set`, and
built-in diff/merge editors.
Signing with GPG or ssh-keygen requires external tooling and is unavailable.
Run repository maintenance separately when needed.

See [the privacy model](docs/privacy.md) for the guarantees, kernel policy, and
scope of protection. Existing repositories created by newer jj versions are
subject to jj 0.37's format compatibility; back up repositories before migrating
between versions.

## Development and verification

```sh
cargo fmt --all --check
cargo clippy --locked --all-targets --no-deps -- -D warnings
cargo test --locked
python3 scripts/verify_process_privacy.py target/debug/fastjj
```

The last check is Linux-only and requires `strace`. It traces real init, snapshot, diff, history,
clone, fetch, push, and forbidden-editor requests and checks for child processes
and private argv. Rust tests exercise kernel enforcement, inherited restrictions
on existing and new threads, the x32 ABI, stdin argv, configuration overrides,
history edits, and native Git transport. The ignored `guard_probe` fixture is
actually executed in isolation by the `kernel_guard` test so its irreversible
policy does not restrict the test runner. On macOS the kernel tests verify direct
fork, exec, and posix_spawn denials and worker inheritance; argv observation uses
the platform's `ps`. CI runs on x86_64 and ARM64 Linux and macOS. Release packaging
tests use Python 3.11 or newer:

```sh
python3 -m unittest discover -s tests -p 'test_*.py'
```

CI and optimized release builds run `scripts/verify_ssh.py` against disposable
SSH servers and keys. It performs real clone/fetch/push with custom Ed25519 and
RSA keys, directly encrypted keys through a PTY, an encrypted agent key with
`IdentitiesOnly`, aliases and includes, and a two-hop native `ProxyJump`. It also
checks unknown/changed/revoked host rejection, identity selection, external
command rejection, and process privacy. Linux traces each client process tree;
both platforms observe argv and run kernel subprocess-denial tests. Install
OpenSSH server/client tools and `strace` on Linux to run it locally:

```sh
python3 scripts/verify_ssh.py target/debug/fastjj
```

CI and release builds also run `scripts/verify_https_clone.py` against a small
public GitHub repository to verify native HTTPS and each OS's default certificate
trust. It requires network access; the Rust integration tests run locally without
network services.

## Upstream and license

Apache-2.0. The CLI fork is in `vendor/jj-cli`, with upstream notices retained.
[NOTICE](NOTICE) records its source and changes. `vendor/jj-lib` contains the native Git transport adaptation. The jjui fork in
`vendor/jjui` retains its MIT license. To update either upstream, preserve the
private process boundaries and rerun kernel, transport, UI, and syscall tests.
