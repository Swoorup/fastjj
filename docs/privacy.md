# Process privacy

## Guarantee

Once fastjj starts, it installs a mandatory Linux seccomp filter **before** jj
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
stdin and are handed to the embedded CLI in memory. `/proc/<pid>/cmdline` contains
only the executable and `--request-stdin`, not the request. The ordinary command
mode makes no promise to conceal arguments the caller supplied in argv.

## Kernel enforcement

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

## Scope

This protects process **argument** privacy. It does not conceal terminal output,
filesystem paths, `/proc/<pid>/cwd`, open files, network destinations, operation
logs, process memory, or environment variables. It is not a filesystem or network
sandbox and does not defend against a privileged debugger or an already-running
service that the user instructs to perform work elsewhere. Existing authentication
agents remain separate services and are not started by fastjj.

Avoid putting secrets in normal CLI arguments or remote URLs. Read requests from
a protected file or an application-held pipe; do not embed their JSON in an
invoking shell's command-line arguments. Output retains upstream jj semantics
and can include repository information. jj operation descriptions can retain
command arguments inside the repository. No environment variables carry request
data. Malformed requests are reported without reflecting their contents.

The executable's own installation path and name remain visible. Linux x86_64 and
aarch64 are the supported targets; macOS, Windows, and other architectures have
no silent unprotected mode.

## Maintaining the boundary

The command-line safety settings are usability defaults, not the security
boundary. Kernel enforcement remains effective even if user configuration or a
dependency tries to start a program. The pinned native Git backend is needed for
network commands to work while that boundary is enforced.

Before changing the filter, transport features, argument plumbing, or upstream
version, run:

1. The pure BPF tests (architecture, process, and thread decisions).
2. `kernel_guard` (real exec/fork/clone denials, existing and new threads).
3. CLI integration tests (native Git and inherited privacy with real repository
   mutations).
4. The syscall trace audit (private canaries absent from process syscalls and no
   successful child creation).

Keep TLS certificate, package checksum, and signature verification enabled.
