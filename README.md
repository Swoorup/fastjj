# fastjj

A Rust Jujutsu client that performs repository operations in the current process
and allows worker threads, while preventing subprocesses from exposing repository
names, paths, or remote URLs in their command lines.

fastjj embeds **jj 0.29.0**, the final jj release with an in-process libgit2 Git
transport. It works with jj's repository format and keeps jj's commands, revsets,
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
Version tags such as `v0.1.0` additionally publish all four archives to
[GitHub Releases](https://github.com/Swoorup/fastjj/releases), after every target
passes its optimized tests. The tag must match the version in `Cargo.toml`.
Re-running a tag build replaces that release's assets. No tag is created by the
workflow itself.

Each `.tar.gz` includes the executable, documentation, license, and
`build-info.json` recording the exact source commit. An adjacent `.sha256` file
provides its checksum. After extracting an archive, install its `fastjj` executable
in a directory on your PATH and run `fastjj privacy`.

```sh
# Linux
sha256sum --check fastjj-v0.1.0-x86_64-unknown-linux-gnu.tar.gz.sha256
# macOS (choose the matching architecture's archive)
shasum -a 256 --check fastjj-v0.1.0-aarch64-apple-darwin.tar.gz.sha256
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

Clone, fetch, push, local repositories, HTTP(S), and SSH use libgit2/libssh2 in the
same process. SSH agent authentication uses the existing agent socket, and SSH
keys and terminal HTTPS prompts use the embedded jj backend. Git credential
helpers and pinentry cannot execute; use in-process authentication or an existing
SSH agent. In-process SSH does not execute `ssh`, `ProxyCommand`, or SSH wrappers.
Authentication and transport options differ from a subprocess-based Git client.

Commands whose purpose is running other programs (`util exec`, `fix`, `util gc`,
and `config edit`) return an explicit privacy error. External text editors,
diff/merge tools, signing programs, hooks, and credential helpers also cannot run.
Use the built-in text editor or `describe -m` / `commit -m`, `config set`, and
built-in diff/merge editors.
Signing with GPG or ssh-keygen requires external tooling and is unavailable.
Run repository maintenance separately when needed.

See [the privacy model](docs/privacy.md) for the guarantees, kernel policy, and
scope of protection. Existing repositories created by newer jj versions are
subject to jj 0.29's format compatibility; back up repositories before migrating
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

## Upstream and license

Apache-2.0. The CLI fork is in `vendor/jj-cli`, with upstream notices retained.
[NOTICE](NOTICE) records its source and changes. `jj-lib` stays an unmodified,
version-pinned dependency. To update the embedded jj release, retain in-process
transport and rerun both the kernel and syscall-trace tests; newer upstream
versions cannot be substituted without a transport implementation change.
