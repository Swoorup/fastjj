# Process privacy

## Guarantee

Once fastjj starts, it installs a mandatory kernel process policy **before** jj
initialization, configuration loading, or worker-thread creation. It refuses to
run if any step fails. The kernel prevents fastjj, all its existing threads, and
all future threads from creating child processes or executing other programs.
Repository paths and names therefore cannot be exposed in a subprocess's argv:
there is no subprocess. Repository reads, writes, diffing, history operations,
and Git transport run within fastjj. Work parallelizes through threads.

This includes subprocess attempts hidden in dependencies or configuration:
external editors, shell aliases, Git credential helpers, filters, signing
backends, pagers, Watchman, and Git/SSH wrappers cannot evade the filter. Requests
to run an external-only feature fail; no fallback weakens the policy.

In `--request-stdin` mode, paths, URLs, messages, and other arguments enter through
stdin and are handed to the embedded CLI in memory. The OS's argv view contains
only the executable and `--request-stdin`, not the request. The ordinary command
mode makes no promise to conceal arguments the caller supplied in argv.

## jjui boundary

`fastjjui` starts each backend child as `fastjj --request-fd`. Command arguments
and cwd travel through descriptor 3; background credential prompts use an
inherited Unix stream socket at descriptor 4. stdin remains available for inline
descriptions. There is no askpass subprocess or named credential socket.

The UI itself can create processes. Its single backend command factory, disabled
shell/Lua process APIs, and fixed-argument clipboard utilities enforce argument
privacy at that boundary. Every backend child independently installs the kernel
policy described here. Unmodified upstream jjui does not provide this guarantee.
See [the jjui guide](jjui.md) for the protocol, restrictions, and verification.

## Linux kernel enforcement

The filter sets `PR_SET_NO_NEW_PRIVS` and uses
`seccomp(SECCOMP_SET_MODE_FILTER, SECCOMP_FILTER_FLAG_TSYNC, ...)`. Any negative
result or positive thread ID (a synchronization failure) is treated as fatal.
Filters cannot be removed or relaxed; further filters can only add restrictions.

| Operation | Policy |
| --- | --- |
| `execve`, `execveat` | Return `EPERM` |
| `fork`, `vfork` on x86_64 | Return `EPERM` |
| `clone` without `CLONE_THREAD` | Return `EPERM` |
| `clone` with `CLONE_THREAD` | Allow (kernel requires shared VM and signal handlers) |
| `clone3` | Return `ENOSYS`, enabling libc's inspectable `clone` fallback |
| Syscalls using a different audit architecture | Kill the process |
| x32 ABI syscalls and the legacy 512–547 compat range on x86_64 | Return `EPERM` |
| Other native syscalls | Allow |

Classic BPF cannot dereference `clone3`'s userspace argument structure safely, so
it is never allowed. Returning `ENOSYS` supports modern glibc's thread fallback.
The architecture check precedes syscall-number interpretation; x32's shared audit
architecture is handled explicitly. aarch64 has no separate fork/vfork syscalls;
the clone and exec rules cover it.

## macOS kernel enforcement

Intel and Apple Silicon builds apply an inline Seatbelt profile through
`sandbox_init` in `libsandbox`:

```scheme
(version 1)
(allow default)
(deny process-fork)
(deny process-exec)
```

The profile is process-wide and irreversible. It covers existing and future
threads without blocking pthread creation or native filesystem/network access.
The fork rule prevents a child from being created, including through
`posix_spawn`; the exec rule also prevents replacing the current process image.
The XNU kernel applies its fork policy check before creating a spawned process.

Apple has deprecated this CLI sandbox API but still provides it on the supported
macOS releases. An unavailable API or a rejected profile, including an
incompatible enclosing sandbox, makes fastjj refuse to run. A release must pass
real fork, exec, posix_spawn, and inherited-thread restriction tests on both
Intel and Apple Silicon runners. No fallback invokes an external sandbox helper.

## Scope

This protects process **argument** privacy. It does not conceal terminal output,
filesystem paths, `/proc/<pid>/cwd`, open files, network destinations, operation
logs, process memory, or environment variables. It is not a filesystem or network
sandbox and does not defend against a privileged debugger or an already-running
service that the user instructs to perform work elsewhere. Existing authentication
agents remain separate services and are not started by fastjj.

Native SSH reads keys and configuration inside the process. Encrypted-key
prompts use the controlling terminal with echo disabled; passphrases never
enter argv or request environment variables. `ProxyJump` connections use worker
threads and SSH direct-tcpip channels. Every hop verifies its own host key before
authentication. Arbitrary command-based proxies and configuration conditions
cannot execute. The remote SSH server receives the Git repository path and may
run its own Git process; fastjj's process policy governs the local client.

Avoid putting secrets in normal CLI arguments or remote URLs. Read requests from
a protected file or an application-held pipe; do not embed their JSON in an
invoking shell's command-line arguments. Output retains upstream jj semantics
and can include repository information. jj operation descriptions can retain
command arguments inside the repository. No environment variables carry request
data. Malformed requests are reported without reflecting their contents.

The executable's own installation path and name remain visible. Linux and macOS
on x86_64 and aarch64 are supported; Windows and other architectures have no
silent unprotected mode.

## Maintaining the boundary

The client's configuration overrides select native implementations; the security
boundary is the kernel policy. Enforcement remains effective even if user configuration or a
dependency tries to start a program. The pinned native Git backend is needed for
network commands to work while that boundary is enforced.

Before changing the filter, transport features, argument plumbing, or upstream
version, run:

1. The Linux BPF tests (architecture, process, and thread decisions).
2. `kernel_guard` (real exec/fork denials, Linux clone or macOS posix_spawn,
   existing and new threads).
3. CLI integration tests (native Git and inherited privacy with real repository
   mutations).
4. On Linux, the syscall trace audit (private canaries absent from process
   syscalls and no successful child creation). macOS uses native kernel-denial
   tests and argv observation, without changing SIP or requiring DTrace privileges.

Keep TLS certificate, package checksum, and signature verification enabled.
