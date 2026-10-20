# fjj — embedded Jujutsu

A Rust client that embeds Jujutsu, Git, and SSH in one process. Repository work
runs in-process with worker threads; a mandatory kernel policy prevents the
repository backend from launching external programs. An explicitly selected
text editor runs through a separate helper with a generic temporary filename.
Delta can format diffs through a helper that receives patches over pipes.

fjj embeds **jj 0.37.0**, with an in-process libgit2 implementation of its Git
transport. It includes **fjjui**, a fork of jjui 0.10.11 connected to the embedded
backend. It works with jj's repository format and keeps jj's commands, revsets,
templates, automatic working-copy snapshots, operation history, bookmarks,
rebasing, and conflict handling. It does not launch an installed `jj` or `git`.

**Supported platforms:** Linux and macOS, on Intel/x86_64 and ARM64/Apple Silicon.
Linux uses synchronized seccomp filters; macOS uses a process-wide Seatbelt
profile denying fork and exec. Platforms that cannot install their kernel policy
are rejected before loading repository configuration. The process policy cannot
be disabled.

## Build and install

Install Rust using [rustup](https://rustup.rs). The toolchain is pinned in
`rust-toolchain.toml`. A C compiler, make, Perl, and pkg-config are required to
build the bundled libgit2, libssh2, and OpenSSL dependencies. On Debian/Ubuntu:

```sh
sudo apt-get install build-essential pkg-config perl
cargo build --release --locked
cargo install --path . --locked
fjj embed
```

On macOS, install Xcode Command Line Tools with `xcode-select --install` and
pkg-config with `brew install pkg-config`, then run the same Cargo commands.
The built executable is `target/release/fjj`. Source installation is from this
checkout; no crates.io release is published. TLS verification remains enabled.

## fjjui

Run `fjjui` inside your repository. It supports jjui's graph, previews,
bookmarks, history editing, workspaces, and Git fetch/push while sending command
arguments and credentials over inherited descriptors. Install it beside
`fjj`; both binaries are included in releases and the Nix package.

```sh
nix run github:Swoorup/fastjj#fjjui
```

Use this bundled fork to communicate with the embedded backend. Unmodified jjui
still exposes its backend command arguments, even when `jj` is a symlink to fjj.
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
SHA-256 checksum; it does not compile fjj or Rust dependencies. It supports
`x86_64-linux`, `aarch64-linux`, `x86_64-darwin`, and `aarch64-darwin`. Nix downloads
the required packaging tools and runtime libraries as well. Linux executables
have their interpreter and library paths patched for NixOS. The installed
`bin/fjj` is a native executable with no launcher script. macOS requires 13.0
or later.

To use it from a NixOS, nix-darwin, or Home Manager flake, add the input:

```nix
inputs.fjj.url = "github:Swoorup/fastjj";
```

Pass `inputs` to your modules through `specialArgs` (or `extraSpecialArgs` for
Home Manager), then add the package to `environment.systemPackages` or
`home.packages`:

```nix
{ inputs, pkgs, ... }: {
  environment.systemPackages = [
    inputs.fjj.packages.${pkgs.stdenv.hostPlatform.system}.default
  ];
}
```

`packages.<system>.default`, `.fjj`, and `.fjjui` select the binary
release recorded in [`nix/binary-release.json`](nix/binary-release.json).
Following `main` updates the packaging; it selects a newer binary only when that
manifest is updated. Your consumer flake lock pins the repository revision.
Maintainers should verify all four release builds, update the archive hashes and
source commit in the manifest, and publish the verified assets. CI checks every
installed package after publication.
The **Publish verified binaries** workflow can publish an existing successful
**Release binaries** run without rebuilding: it checks the run's source commit,
all archive hashes, and embedded provenance against the manifest before upload.
Create the version tag at that source commit before dispatching the workflow.
Interrupted uploads can be resumed; existing assets must match exactly.

To keep arguments out of the process command line, run
`nix run github:Swoorup/fastjj -- --request-stdin < request.json`
using the [request format below](#send-arguments-to-the-embedded-client).
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

Pushes to `main` and manual workflow runs upload downloadable build artifacts.
Changes confined to Nix packaging and the CI workflow skip rebuilding unchanged
release binaries; source and installed-package CI checks still run.
Version tags such as `v0.2.5` additionally publish all four archives to
[GitHub Releases](https://github.com/Swoorup/fastjj/releases), after every target
passes its optimized tests. The tag must match the version in `Cargo.toml`.
Published assets are never overwritten: a repeated tag build must match the
existing assets, or the publish job fails and a new version is needed. No tag is
created by the workflow itself.

Each `.tar.gz` includes `fjj` and `fjjui`, documentation, licenses, and
`build-info.json` recording the exact source commit. An adjacent `.sha256` file
provides its checksum. After extracting an archive, install both executables
in the same directory on your PATH and run `fjj embed`.

```sh
# Linux
sha256sum --check fjj-v0.2.5-x86_64-unknown-linux-gnu.tar.gz.sha256
# macOS (choose the matching architecture's archive)
shasum -a 256 --check fjj-v0.2.5-aarch64-apple-darwin.tar.gz.sha256
```

Linux binaries are built on Ubuntu 24.04 and need a compatible glibc installation.
macOS binaries use a macOS 13.0 deployment target and are tested on macOS 15.
They are not Developer ID signed or notarized.

## Use jj commands

```sh
fjj git init
fjj status
fjj diff --git
fjj describe -m 'Implement the feature'
fjj bookmark create main -r @
fjj new
fjj log
fjj rebase -r @ -d main
fjj undo
```

Run from the desired workspace to avoid putting its path in argv. Messages and
file arguments you type normally are still visible in **fjj's own** argv.
To keep arguments out of the process command line, use the stdin interface below.

`fjj --help` and `fjj help <command>` provide the embedded jj command
reference. Normal jj user/repository configuration, aliases, identity, revsets,
templates, color, and output remain available. The client always selects native
Git transport, the built-in pager and diff/merge editors, and filesystem scanning
without Watchman. Repository configuration cannot enable child processes in the backend.

### Existing jj configuration

`fjj` and `fjjui` use your **jj** configuration; no separate `fjj` config is
needed. On Linux and macOS, files load in this order (later values win):

1. `~/.jjconfig.toml`
2. `$XDG_CONFIG_HOME/jj/config.toml` (default: `~/.config/jj/config.toml`)
3. `$XDG_CONFIG_HOME/jj/conf.d/*.toml`, in filename order
4. On macOS, existing `~/Library/Application Support/jj/config.toml` and
   `~/Library/Application Support/jj/conf.d/*.toml`, retaining older jj's precedence

`JJ_CONFIG` replaces these default locations; an empty value disables user
config loading. Repository/workspace config, `JJ_USER` / `JJ_EMAIL`, and
`--config` retain jj's normal precedence. New user config files use the XDG
location. This lookup reads files in-process.

Use `fjj config path --user`, `fjj config get user.name`, and
`fjj config get user.email` to check the active configuration. Config identity
changes apply to new commits; existing commits keep their recorded authors.

## Send arguments to the embedded client

Create a JSON request file with restrictive permissions. Its `args` contain jj
arguments; `cwd` optionally chooses the working directory without a `-R` argument.

```json
{
  "cwd": "/work/customer/project",
  "args": ["git", "clone", "https://example.org/team/project.git", "./checkout"]
}
```

```sh
chmod 600 request.json
fjj --request-stdin < request.json
```

Process observers see `fjj --request-stdin`. The request passes directly into
jj as an in-memory argument vector; fjj never starts a process with its data.
The shell does not pass the redirection's filename to fjj. Avoid a shell
command that includes the JSON or sensitive data in its own arguments.

The request is one JSON document, bounded to 1 MiB. `args` is required and must
be an array of strings. Unknown fields and NUL bytes are rejected. After receiving
EOF, fjj executes one command and returns its jj exit status. Standard output
and error remain ordinary jj output, not JSON. Parsing errors do not echo request
contents. The stdin request interface consumes stdin; commands that need stdin again should use
their file or message options instead. The built-in text editor reads the
controlling terminal directly and therefore also works with embedded requests.

## Diffs, Delta, and previews

Built-in diff formats and the interactive diff editor work in-process. To use
[Delta](https://github.com/dandavison/delta), install it separately and keep your
usual **user jj configuration**, for example:

```toml
[ui]
pager = "delta"
diff-formatter = ":git"
```

This also enables Delta in captured output, including fjjui previews. The
built-in pager handles terminal scrolling; Delta's own pager is disabled.
Alternatively, configure `ui.diff-formatter = ["delta", "--side-by-side"]`.
Named Delta entries under `merge-tools` work too. An absolute executable path
ending in `delta` is supported.

```sh
fjj diff
fjj diff --tool delta
fjj difftool             # alias for diff; respects your formatter
fjj diff --git           # explicit Git format
```

In fjjui, **d** opens the diff viewer and **p** toggles the preview. File details
have the same diff/preview actions. Ctrl+R refreshes working-copy changes; pane
and terminal resizing regenerate width-sensitive output. Renderer errors appear
in the preview and can be retried after fixing the configuration.
Custom preview commands using explicit `--git` select Git output; omit that flag
to use Delta. `--color=never` produces a plain Git patch without ANSI escapes.
Summary, stat, types, and name-only formats keep their built-in behavior.

Delta receives Git patches on stdin, including filenames needed for syntax
highlighting. Repository paths, filenames, and contents are never appended to
its argv. Only user-level jj configuration authorizes Delta's program/options;
repository or command-line configuration cannot add executable commands or new
argument vectors. Configure additional options in your user config first.
Global Delta settings in `~/.gitconfig` remain available. The renderer runs in a
private temporary directory, so repository-local Git configuration is not read;
use absolute paths for explicit configuration-file options. Structured user
commands with `command` and `env` also preserve their configured environment.

Delta is a trusted external renderer, like an explicitly selected editor.
Its user-supplied options and any subprocesses it starts are outside the
backend's process restriction. The supported Delta workflow is tested with its
pager disabled. Delta is read-only: `diffedit` and merge editing continue to use
the built-in interactive tools.

## Commit-message editors

Set `EDITOR` to use your usual editor in `fjj describe`, other description-editing
commands, and `fjjui`'s Shift+D action:

```sh
export EDITOR=nvim
fjj describe
fjjui
```

Selection order is `JJ_EDITOR`, `VISUAL`, then `EDITOR`; empty variables are
ignored. Quoted arguments work, for example `EDITOR='code --wait'`. Use a command
that waits until the file is saved and closed. `JJ_EDITOR=:builtin` selects the
native editor even when another editor variable is set. Select external editors
through these environment variables; repository `ui.editor` settings cannot
enable external programs. `fjj config edit` and `fjjui --config` use the same
text editor.

The editor receives only `COMMIT_EDITMSG`, in a private temporary directory outside
the repository. Message text travels to the helper over pipes; no repository path
is appended to the editor's argv. Its working directory is that temporary directory,
so use absolute paths for any editor-specific configuration arguments.
The chosen editor and its plugins are trusted external programs and can start
their own processes. Their own arguments and behavior are outside the backend's
process guarantee; Git/SSH operations in fjj remain kernel-restricted.

Without a selected external editor, description commands open a native,
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
the two default files. Its value is an environment variable, visible according to the operating system's environment access controls.

Ed25519, ECDSA, and RSA private keys are read directly. Encrypted keys prompt on
the controlling terminal with echo disabled, also in `--request-stdin` mode.
Passphrases are cached only for the current invocation and their buffers are
cleared on drop. `BatchMode yes` disables terminal prompts. Existing agents can
provide unlocked keys or hardware-backed identities; fjj never starts an
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
`github-work:OWNER/REPO.git`. Use a stdin request when setting that URL.
Then run `fjj git fetch` or `fjj git push` inside the repository. For SSH
over port 443, use `HostName ssh.github.com` and `Port 443` with its verified host
key. Git credential helpers for HTTPS still cannot execute; use native terminal
credentials instead.

Commands whose purpose is running other programs (`util exec`, `fix`, and `util gc`)
return an explicit process-policy error. External diff/merge tools other than the
authorized Delta renderer, signing
programs, hooks, and credential helpers also cannot run.
Use the selected text editor or `describe -m` / `commit -m`, `config set`, and
built-in diff/merge editors.
Signing with GPG or ssh-keygen requires external tooling and is unavailable.
Run repository maintenance separately when needed.

See [the embedded process model](docs/embed.md) for the guarantees, kernel policy, and
scope of protection. Existing repositories created by newer jj versions are
subject to jj 0.37's format compatibility; back up repositories before migrating
between versions.

## Development and verification

```sh
cargo fmt --all --check
cargo clippy --locked --all-targets --no-deps -- -D warnings
cargo test --locked
python3 scripts/verify_embed.py target/debug/fjj
```

The last check uses the built-in editor mode, is Linux-only, and requires `strace`. It traces real init, snapshot, diff, history,
clone, fetch, push, and forbidden-editor requests and checks for child processes
and argument exposure. Rust tests exercise kernel enforcement, inherited restrictions
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

`scripts/verify_editor.py` checks the selected external editor, environment
precedence, failure/cancellation behavior, private temporary files, config editing,
and fjjui terminal handoff on all four platforms. It observes live process argv
on macOS and Linux and audits the full process tree on Linux. SSH checks also run
with the editor service enabled to verify that credentials and repository paths
stay out of arguments while the backend's kernel restrictions remain effective.

`scripts/verify_diff.py` exercises real Delta, named diff tools, filename
filtering, built-in output formats, jjui diffs/previews, refresh and resize,
renderer failures, user-config authorization, and live process arguments.
CI downloads a checksum-pinned Delta test executable for each native platform.

CI and optimized release builds run `scripts/verify_ssh.py` against disposable
SSH servers and keys. It performs real clone/fetch/push with custom Ed25519 and
RSA keys, directly encrypted keys through a PTY, an encrypted agent key with
`IdentitiesOnly`, aliases and includes, and a two-hop native `ProxyJump`. It also
checks unknown/changed/revoked host rejection, identity selection, external
command rejection, and embedded process enforcement. Linux traces each client process tree;
both platforms observe argv and run kernel subprocess-denial tests. Install
OpenSSH server/client tools and `strace` on Linux to run it locally:

```sh
python3 scripts/verify_ssh.py target/debug/fjj
```

CI and release builds also run `scripts/verify_https_clone.py` against a small
public GitHub repository to verify native HTTPS and each OS's default certificate
trust. It requires network access; the Rust integration tests run locally without
network services.

## Upstream and license

Apache-2.0. The CLI fork is in `vendor/jj-cli`, with upstream notices retained.
[NOTICE](NOTICE) records its source and changes. `vendor/jj-lib` contains the native Git transport adaptation. The jjui fork in
`vendor/jjui` retains its MIT license. To update either upstream, preserve the
embedded process boundaries and rerun kernel, transport, UI, and syscall tests.
