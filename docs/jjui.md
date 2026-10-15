# fjjui and the embedded backend

`fjjui` is the bundled fork of jjui 0.10.11, paired with fjj 0.2.2 and its
embedded jj 0.37.0. The UI is Go; repository operations and native Git/SSH are
Rust. Linux and macOS support both x86_64 and ARM64.

Install **both binaries in the same directory**, then run `fjjui` from inside
your repository. It discovers the adjacent fjj executable, or searches PATH
if there is no adjacent binary, and checks the private protocol version before
opening the repository. It never falls back to ordinary `jj`.

```sh
# Prebuilt package, including both executables
nix profile install github:Swoorup/fastjj
fjjui

# Or run the UI directly using the flake
nix run github:Swoorup/fastjj#fjjui
```

Existing jjui configuration, themes, key bindings, and Lua actions remain usable.
Lua `jj(...)` actions yield while the backend runs, so credential panels remain
responsive and scripts receive the same output/error return values.
The configuration directory is still jjui's directory; `JJUI_CONFIG_DIR` selects
an alternative. Repository configuration is supported. The `[askpass] enabled`
setting controls native background credential prompts; it does not run askpass.

## Workflows

The normal jjui graph, revsets, previews, file details, bookmarks, workspaces,
operation history, undo/redo, and history editing use the private backend.

- Enter opens the inline description editor. Ctrl+S saves. Its text travels on
  stdin while command arguments travel separately on descriptor 3.
- Shift+D opens fjj's native terminal description editor. Enter replacement
  lines and finish with `.save`, or use `.keep` / `.cancel`.
- `g`, then `f` or `p`, opens fetch or push choices. Enter runs the selection.
  Encrypted SSH keys, host-key confirmation, HTTPS usernames/tokens, and SSH
  password/keyboard-interactive authentication use the UI credential panel.
  Secrets are masked; Escape cancels. SSH agent authentication remains native.
- `:` runs a jj command with terminal access and private arguments. Commands
  needing the built-in diff or merge editor can take over the terminal normally.
- `fjjui --config` uses fjj's built-in text editor.

The backend's restrictions still apply: external editors, signing commands,
credential helpers, `fix`, and commands that require external programs cannot
execute. Shell actions (`$`, Lua `exec_shell`, `os.execute`, `io.popen`) return a
privacy error. Use jj actions or Lua's jj API for repository automation. Clipboard
support may start the platform clipboard utility with fixed arguments and send
clipboard text over stdin; no repository data is placed in its argv.

## Process boundary

Every repository command starts a child with exactly:

```text
fjj --request-fd
```

The UI sends `{ "args": [...], "cwd": "...", "prompt": true }` over an inherited
pipe at descriptor 3 and closes it after one bounded JSON document. stdin,
stdout, and stderr keep their ordinary meanings, including streamed previews and
interactive tools. A connected inherited Unix stream socket at descriptor 4
carries newline-delimited JSON credential requests and responses. There are no
socket filenames, askpass helpers, environment tokens, shells, or argv-encoded
paths. Each command has its own prompt channel. Request size is bounded to 1 MiB
and credential frames to 128 KiB. EOF, cancellation, malformed data, and oversized
frames fail rather than falling back to an external helper.

The startup probe uses the constant `fjj --interface-version` command. fjj
installs its kernel subprocess prohibition before processing any request, so
Git, SSH, and editors inside the backend cannot create another process. The UI
must be able to start these backend children; its fixed-argument command factory,
blocked shell entry points, and audited clipboard calls enforce the UI boundary.
This is a separate boundary from the backend's kernel policy.

Unmodified upstream jjui passes command arguments through argv. Pointing it at a
symlink named `jj` does **not** provide the UI's privacy guarantee. Use `fjjui`.
Start it inside the repository: an explicitly supplied repository CLI argument
would already be visible in the UI's own argv. Terminal contents, jj operation
logs, the existing jjui command history, process memory, environment, open files,
and the working directory remain outside argument privacy. See [embed.md](embed.md).

## Building and checking

Build fjj first, then install Go 1.27.2 or newer and build the UI beside it:

```sh
cargo build --release --locked
(cd vendor/jjui && CGO_ENABLED=0 go build -trimpath \
  -ldflags '-s -w -X main.Version=fjjui-0.2.2-jjui-0.10.11' \
  -o ../../target/release/fjjui ./cmd/jjui)
(cd vendor/jjui && go test ./...)
python3 scripts/verify_jjui.py target/release/fjj target/release/fjjui
python3 scripts/verify_ssh.py target/release/fjj --ui-binary target/release/fjjui --lua-prompts
```

CI runs these checks on all four native platforms. The real-terminal tests cover
graph loading, streamed previews, stdin descriptions, new/undo/redo, native text
editing, shell denial, and encrypted-key fetch/push with credential cancellation.
Linux traces the UI's whole process tree and rejects any unexpected backend argv
or private canary in process syscalls. Cross-platform Rust tests also observe the
inherited-request argv through `ps`; kernel tests check the backend independently.
