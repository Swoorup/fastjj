use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[test]
fn built_in_editor_updates_description_without_an_external_editor() {
    let root = tempfile::TempDir::new().unwrap();
    let config = root.path().join("config.toml");
    fs::write(&config, "[user]\nname='Test'\nemail='test@example.invalid'\n[ui]\npaginate='never'\ncolor='never'\n").unwrap();
    let binary = env!("CARGO_BIN_EXE_fjj");
    eprintln!("terminal fixture: initialize repository");
    let output = Command::new(binary)
        .current_dir(root.path())
        .env("JJ_CONFIG", &config)
        .args(["git", "init", "repo"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let (mut master_fd, mut slave_fd) = (-1, -1);
    // SAFETY: valid output pointers, default terminal settings; ownership of
    // each resulting FD is transferred once to a File below.
    assert_eq!(
        unsafe {
            libc::openpty(
                &mut master_fd,
                &mut slave_fd,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        },
        0
    );
    let mut master = unsafe { File::from_raw_fd(master_fd) };
    let slave = unsafe { File::from_raw_fd(slave_fd) };
    let mut command = Command::new(binary);
    command
        .current_dir(root.path().join("repo"))
        .env("JJ_CONFIG", &config)
        .arg("describe")
        .stdin(Stdio::from(slave.try_clone().unwrap()))
        .stdout(Stdio::from(slave.try_clone().unwrap()))
        .stderr(Stdio::from(slave.try_clone().unwrap()));
    // This is the *test runner* setting up the client's controlling terminal
    // before exec. fjj subsequently installs its own process-denial policy.
    #[cfg(target_os = "macos")]
    let tty_request = libc::c_ulong::from(libc::TIOCSCTTY);
    #[cfg(not(target_os = "macos"))]
    let tty_request = libc::TIOCSCTTY;
    unsafe {
        command.pre_exec(move || {
            if libc::setsid() == -1 || libc::ioctl(0, tty_request, 0) == -1 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = command.spawn().unwrap();
    eprintln!("terminal fixture: editor started");
    drop(command);
    drop(slave);
    // Pump both directions concurrently: a macOS PTY can fill its output/echo
    // buffer and block a synchronous write while the child is still running.
    let flags = unsafe { libc::fcntl(master.as_raw_fd(), libc::F_GETFL) };
    assert_ne!(flags, -1);
    assert_eq!(
        unsafe { libc::fcntl(master.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) },
        0
    );
    let input = "native editor 世界\n.save\n".as_bytes();
    let mut sent = 0;
    let mut terminal_output = Vec::new();
    let mut buffer = [0; 4096];
    let mut exit_status: Option<std::process::ExitStatus> = None;
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        loop {
            match master.read(&mut buffer) {
                Ok(0) => break,
                Ok(n) => terminal_output.extend_from_slice(&buffer[..n]),
                Err(e)
                    if e.raw_os_error() == Some(libc::EIO)
                        || e.kind() == io::ErrorKind::WouldBlock =>
                {
                    break;
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => panic!("reading terminal failed: {e}"),
            }
        }
        if let Some(status) = exit_status {
            assert!(
                status.success(),
                "native editor failed: {status}\n{}",
                String::from_utf8_lossy(&terminal_output)
            );
            break;
        }
        if sent < input.len()
            && String::from_utf8_lossy(&terminal_output).contains("Enter replacement text")
        {
            match master.write(&input[sent..]) {
                Ok(n) => sent += n,
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                    ) => {}
                Err(e) => panic!("writing terminal failed: {e}"),
            }
        }
        exit_status = child.try_wait().unwrap();
        if exit_status.is_none() && Instant::now() >= deadline {
            child.kill().unwrap();
            let _ = child.wait();
            panic!(
                "native editor did not complete\n{}",
                String::from_utf8_lossy(&terminal_output)
            );
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(String::from_utf8_lossy(&terminal_output).contains("Enter replacement text"));
    eprintln!("terminal fixture: verify persisted description");
    let output = Command::new(binary)
        .current_dir(root.path().join("repo"))
        .env("JJ_CONFIG", &config)
        .args(["log", "-r", "@", "--no-graph", "-T", "description"])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "native editor 世界"
    );
}
