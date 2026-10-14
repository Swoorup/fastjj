use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::os::fd::FromRawFd;
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[test]
fn built_in_editor_updates_description_without_an_external_editor() {
    let root = tempfile::TempDir::new().unwrap();
    let config = root.path().join("config.toml");
    fs::write(&config, "[user]\nname='Test'\nemail='test@example.invalid'\n[ui]\npaginate='never'\ncolor='never'\n").unwrap();
    let binary = env!("CARGO_BIN_EXE_fastjj");
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
                std::ptr::null(),
                std::ptr::null(),
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
    // before exec. fastjj subsequently installs its own process-denial policy.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 || libc::ioctl(0, libc::TIOCSCTTY, 0) == -1 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = command.spawn().unwrap();
    drop(command);
    drop(slave);
    master
        .write_all("native editor 世界\n.save\n".as_bytes())
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert!(status.success(), "native editor failed: {status}");
            break;
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            let _ = child.wait();
            panic!("native editor did not complete");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let mut terminal_output = Vec::new();
    let mut buffer = [0; 4096];
    loop {
        match master.read(&mut buffer) {
            Ok(0) => break,
            Ok(n) => terminal_output.extend_from_slice(&buffer[..n]),
            Err(e) if e.raw_os_error() == Some(libc::EIO) => break,
            Err(e) => panic!("reading terminal failed: {e}"),
        }
    }
    assert!(String::from_utf8_lossy(&terminal_output).contains("Enter replacement text"));
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
