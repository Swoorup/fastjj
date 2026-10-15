//! Optional trusted editor service, started before the repository sandbox.
//!
//! Its protocol carries text only, with no fields for repository paths, command
//! arguments, or credentials. The editor command is
//! selected exclusively from the user's inherited environment.

use std::fs::{self, OpenOptions, Permissions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::{Arc, Mutex, OnceLock};

const MAX_TEXT: usize = 16 * 1024 * 1024;
const FILE_NAME: &str = "COMMIT_EDITMSG";
static CONNECTION: OnceLock<Arc<Mutex<Connection>>> = OnceLock::new();

// The editor shares the terminal's foreground process group. Let it handle
// Ctrl+C/Ctrl+\ itself without terminating its waiting parent or this service.
// Keeping the process group also preserves normal shell suspend/resume behavior.
struct EditorSignals(Vec<(i32, libc::sigaction)>);

impl EditorSignals {
    fn ignore() -> io::Result<Self> {
        let mut guard = Self(Vec::new());
        for signal in [libc::SIGINT, libc::SIGQUIT] {
            // SAFETY: sigaction initializes the saved action; the new action
            // contains only SIG_IGN and an empty signal mask.
            unsafe {
                let mut action: libc::sigaction = std::mem::zeroed();
                let mut previous = std::mem::zeroed();
                action.sa_sigaction = libc::SIG_IGN;
                libc::sigemptyset(&mut action.sa_mask);
                if libc::sigaction(signal, &action, &mut previous) < 0 {
                    return Err(io::Error::last_os_error());
                }
                guard.0.push((signal, previous));
            }
        }
        Ok(guard)
    }
}

impl Drop for EditorSignals {
    fn drop(&mut self) {
        for (signal, previous) in self.0.iter().rev() {
            // SAFETY: previous was initialized by sigaction above.
            unsafe { libc::sigaction(*signal, previous, std::ptr::null_mut()) };
        }
    }
}

struct Connection {
    child: Child,
    input: Option<ChildStdin>,
    output: ChildStdout,
}

/// Keeps the service alive only for this invocation, including on CLI errors.
pub struct Service(Arc<Mutex<Connection>>);

impl Drop for Service {
    fn drop(&mut self) {
        let mut connection = self.0.lock().unwrap_or_else(|e| e.into_inner());
        connection.input.take(); // EOF ends the service loop.
        let _ = connection.child.wait();
    }
}

fn command_from_environment() -> io::Result<Option<Vec<String>>> {
    for variable in ["JJ_EDITOR", "VISUAL", "EDITOR"] {
        let Some(value) = std::env::var_os(variable) else {
            continue;
        };
        let value = value.into_string().map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidInput, "editor command is not UTF-8")
        })?;
        if value.trim().is_empty() {
            continue;
        }
        if value.trim() == ":builtin" {
            return Ok(None);
        }
        let args = shlex::split(&value).filter(|args| {
            args.first().is_some_and(|program| !program.is_empty())
                && args.iter().all(|arg| !arg.contains('\0'))
        });
        return args.map(Some).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid editor command quoting",
            )
        });
    }
    Ok(None)
}

/// Called while single-threaded, before installing the backend's kernel policy.
/// With no external editor selected, no helper or other child is created.
pub fn start() -> io::Result<Option<Service>> {
    if command_from_environment()?.is_none() {
        return Ok(None);
    }
    let mut command = Command::new(std::env::current_exe()?);
    command
        .arg0("fjj")
        .arg("--editor-service")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    // jjui's request and credential descriptors must stay in the backend only.
    // Mark them before std::process allocates its own pipes. CLOEXEC leaves them
    // available to this backend, but closes them in the editor service.
    for fd in [3, 4] {
        // SAFETY: fcntl only inspects/sets flags on these borrowed descriptors.
        unsafe {
            let flags = libc::fcntl(fd, libc::F_GETFD);
            if flags >= 0 && libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) < 0 {
                return Err(io::Error::last_os_error());
            }
        }
    }
    let mut child = command.spawn()?;
    let input = child.stdin.take();
    let output = child.stdout.take().expect("piped service stdout");
    let connection = Arc::new(Mutex::new(Connection {
        child,
        input,
        output,
    }));
    CONNECTION
        .set(connection.clone())
        .map_err(|_| io::Error::other("editor service already initialized"))?;
    Ok(Some(Service(connection)))
}

pub fn enabled() -> bool {
    CONNECTION.get().is_some()
}

pub fn edit(path: &Path) -> io::Result<()> {
    let original = read_text_file(path)?;
    let connection = CONNECTION
        .get()
        .ok_or_else(|| io::Error::other("editor service is unavailable"))?;
    let mut connection = connection
        .lock()
        .map_err(|_| io::Error::other("editor service failed"))?;
    let _signals = EditorSignals::ignore()?;
    write_frame(connection.input.as_mut().expect("live service"), &original)?;
    let mut status = [0];
    connection.output.read_exact(&mut status)?;
    let response = read_frame(&mut connection.output)?
        .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "editor service closed"))?;
    match status[0] {
        0 => fs::write(path, response),
        1 => Err(io::Error::other(
            String::from_utf8_lossy(&response).into_owned(),
        )),
        _ => Err(io::Error::other("invalid editor service response")),
    }
}

fn read_text_file(path: &Path) -> io::Result<Vec<u8>> {
    let mut text = Vec::new();
    fs::File::open(path)?
        .take(MAX_TEXT as u64 + 1)
        .read_to_end(&mut text)?;
    check_size(text.len())?;
    Ok(text)
}

fn check_size(size: usize) -> io::Result<()> {
    if size > MAX_TEXT {
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "editor text exceeds 16 MiB",
        ))
    } else {
        Ok(())
    }
}

fn write_frame(writer: &mut impl Write, text: &[u8]) -> io::Result<()> {
    check_size(text.len())?;
    writer.write_all(&(text.len() as u32).to_be_bytes())?;
    writer.write_all(text)?;
    writer.flush()
}

fn read_frame(reader: &mut impl Read) -> io::Result<Option<Vec<u8>>> {
    let mut length = [0; 4];
    // Distinguish clean EOF between requests from a truncated request.
    loop {
        match reader.read(&mut length[..1]) {
            Ok(0) => return Ok(None),
            Ok(_) => break,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        }
    }
    reader.read_exact(&mut length[1..])?;
    let length = u32::from_be_bytes(length) as usize;
    check_size(length)?;
    let mut text = vec![0; length];
    reader.read_exact(&mut text)?;
    Ok(Some(text))
}

/// Entry point for the fixed-argument service. It never initializes jj or SSH.
pub fn serve() -> io::Result<()> {
    let editor = command_from_environment()?
        .ok_or_else(|| io::Error::other("no external editor selected"))?;
    let _signals = EditorSignals::ignore()?;
    let mut input = io::stdin().lock();
    let mut output = io::stdout().lock();
    while let Some(original) = read_frame(&mut input)? {
        match run_editor(&editor, &original) {
            Ok(edited) => {
                output.write_all(&[0])?;
                write_frame(&mut output, &edited)?;
            }
            Err(error) => {
                output.write_all(&[1])?;
                write_frame(&mut output, error.to_string().as_bytes())?;
            }
        }
    }
    Ok(())
}

fn run_editor(editor: &[String], text: &[u8]) -> io::Result<Vec<u8>> {
    // Do not honor a repository-local TMPDIR. The directory is private from the
    // moment it is created, and the only appended argv is a constant basename.
    let directory = tempfile::Builder::new()
        .prefix("fjj-editor-")
        .permissions(Permissions::from_mode(0o700))
        .tempdir_in("/tmp")?;
    let working_directory = directory.path().canonicalize()?;
    let path = working_directory.join(FILE_NAME);
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)?
        .write_all(text)?;
    let program = Path::new(&editor[0]);
    let program = if program.is_relative() && program.components().count() > 1 {
        std::env::current_dir()?.join(program)
    } else {
        program.to_owned()
    };
    let mut command = Command::new(program);
    command
        .arg0(
            Path::new(&editor[0])
                .file_name()
                .unwrap_or_else(|| editor[0].as_ref()),
        )
        .args(&editor[1..])
        .arg(FILE_NAME)
        .current_dir(&working_directory)
        .env("PWD", &working_directory);
    if let Ok(terminal) = OpenOptions::new().read(true).write(true).open("/dev/tty") {
        command
            .stdin(terminal.try_clone()?)
            .stdout(terminal.try_clone()?)
            .stderr(terminal);
    } else {
        // stdout belongs to the framed protocol. Noninteractive editors may
        // emit diagnostics, but must never corrupt the response stream.
        command
            .stdin(Stdio::null())
            .stdout(Stdio::from(io::stderr()))
            .stderr(Stdio::inherit());
    }
    // SAFETY: only async-signal-safe signal operations run after fork. The
    // editor gets normal signal handling even though the supervisor ignores it.
    unsafe {
        command.pre_exec(|| {
            for signal in [libc::SIGINT, libc::SIGQUIT] {
                let mut action: libc::sigaction = std::mem::zeroed();
                action.sa_sigaction = libc::SIG_DFL;
                libc::sigemptyset(&mut action.sa_mask);
                if libc::sigaction(signal, &action, std::ptr::null_mut()) < 0 {
                    return Err(io::Error::last_os_error());
                }
            }
            Ok(())
        });
    }
    let status = command.status()?;
    if !status.success() {
        return Err(io::Error::other(format!(
            "external editor failed: {status}"
        )));
    }
    read_text_file(&path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_distinguish_eof_empty_text_truncation_and_oversized_input() {
        assert!(read_frame(&mut &[][..]).unwrap().is_none());
        let mut encoded = Vec::new();
        write_frame(&mut encoded, b"").unwrap();
        write_frame(&mut encoded, b"message\n").unwrap();
        let mut input = encoded.as_slice();
        assert_eq!(read_frame(&mut input).unwrap(), Some(vec![]));
        assert_eq!(read_frame(&mut input).unwrap(), Some(b"message\n".to_vec()));
        assert!(read_frame(&mut input).unwrap().is_none());
        assert!(read_frame(&mut &[0, 0][..]).is_err());
        assert!(read_frame(&mut &[0, 0, 0, 1][..]).is_err());
        assert!(read_frame(&mut (MAX_TEXT as u32 + 1).to_be_bytes().as_slice()).is_err());
    }
}
