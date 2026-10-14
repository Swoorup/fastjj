use std::ffi::CString;
use std::process::Command;

// A seccomp policy is irreversible. Exercise it in a separate test-runner
// process so the ordinary integration-test harness can still launch fixtures.
#[test]
fn kernel_guard() {
    let output = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "guard_probe", "--ignored", "--nocapture"])
        .env("FASTJJ_GUARD_PROBE", "1")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("guard assertions completed"));
}

#[test]
#[ignore = "executed in an isolated process by kernel_guard"]
fn guard_probe() {
    assert!(std::env::var_os("FASTJJ_GUARD_PROBE").is_some());
    let (signal, wait) = std::sync::mpsc::channel();
    let existing_worker = std::thread::spawn(move || {
        wait.recv().unwrap();
        assert_eq!(
            Command::new("/bin/true")
                .spawn()
                .unwrap_err()
                .raw_os_error(),
            Some(libc::EPERM)
        );
    });
    fastjj::privacy::install().unwrap();
    signal.send(()).unwrap();
    existing_worker.join().unwrap();
    let denied = Command::new("/bin/true").spawn().unwrap_err();
    assert_eq!(denied.raw_os_error(), Some(libc::EPERM));
    let executable = CString::new("/bin/true").unwrap();
    let args = [executable.as_ptr(), std::ptr::null()];
    let env = [std::ptr::null::<libc::c_char>()];
    // SAFETY: all exec arguments point to live, NUL-terminated storage. The
    // policy must deny exec without replacing this test process.
    unsafe {
        assert_eq!(
            libc::execve(executable.as_ptr(), args.as_ptr(), env.as_ptr()),
            -1
        );
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::EPERM)
        );
        assert_eq!(
            libc::syscall(
                libc::SYS_execveat,
                libc::AT_FDCWD,
                executable.as_ptr(),
                args.as_ptr(),
                env.as_ptr(),
                0
            ),
            -1
        );
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::EPERM)
        );
        assert_eq!(
            libc::syscall(libc::SYS_clone, libc::SIGCHLD, 0, 0, 0, 0),
            -1
        );
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::EPERM)
        );
        assert_eq!(
            libc::syscall(libc::SYS_clone3, std::ptr::null::<libc::c_void>(), 0),
            -1
        );
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ENOSYS)
        );
        #[cfg(target_arch = "x86_64")]
        for syscall in [libc::SYS_fork, libc::SYS_vfork] {
            assert_eq!(libc::syscall(syscall), -1);
            assert_eq!(
                std::io::Error::last_os_error().raw_os_error(),
                Some(libc::EPERM)
            );
        }
    }
    let workers: Vec<_> = (0..8)
        .map(|i| {
            std::thread::spawn(move || {
                // New workers inherit the policy, including the exec restriction.
                assert_eq!(
                    Command::new("/bin/true")
                        .spawn()
                        .unwrap_err()
                        .raw_os_error(),
                    Some(libc::EPERM)
                );
                i * i
            })
        })
        .collect();
    assert_eq!(
        workers.into_iter().map(|t| t.join().unwrap()).sum::<i32>(),
        140
    );
    println!("guard assertions completed");
}
