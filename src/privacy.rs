//! A fail-closed, inherited Linux seccomp policy. Install before initializing jj.
//!
//! Denying process creation as well as exec means even a dependency cannot start
//! a child with sensitive argv. pthread/Rayon/Tokio workers remain available.

use std::io;

#[cfg(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
mod linux {
    use super::io;
    use libc::{sock_filter, sock_fprog};

    const LD_ABS: u16 = 0x20;
    const JEQ: u16 = 0x15;
    const JGE: u16 = 0x35;
    const JSET: u16 = 0x45;
    const RET: u16 = 0x06;
    const KILL_PROCESS: u32 = 0x8000_0000;
    const ERRNO: u32 = 0x0005_0000;
    const ALLOW: u32 = 0x7fff_0000;
    const FILTER: u32 = 1;
    const TSYNC: u32 = 1;

    #[cfg(target_arch = "x86_64")]
    const ARCH: u32 = 0xc000_003e;
    #[cfg(target_arch = "aarch64")]
    const ARCH: u32 = 0xc000_00b7;

    fn instruction(code: u16, jt: u8, jf: u8, k: u32) -> sock_filter {
        sock_filter { code, jt, jf, k }
    }

    fn deny(program: &mut Vec<sock_filter>, syscall: libc::c_long, errno: i32) {
        program.push(instruction(JEQ, 0, 1, syscall as u32));
        program.push(instruction(RET, 0, 0, ERRNO | errno as u32));
    }

    fn policy() -> Vec<sock_filter> {
        // Offsets are from Linux struct seccomp_data: nr=0, arch=4, args=16.
        // Check the architecture before interpreting syscall numbers.
        let mut p = vec![
            instruction(LD_ABS, 0, 0, 4),
            instruction(JEQ, 1, 0, ARCH),
            instruction(RET, 0, 0, KILL_PROCESS),
            instruction(LD_ABS, 0, 0, 0),
        ];
        #[cfg(target_arch = "x86_64")]
        {
            // The x32 ABI shares AUDIT_ARCH_X86_64 but uses different syscall
            // numbers. Deny it rather than leaving an alternate-ABI bypass.
            p.push(instruction(JGE, 0, 1, 0x4000_0000));
            p.push(instruction(RET, 0, 0, ERRNO | libc::EPERM as u32));
            // Kernels before Linux 5.4 also accepted x32 numbers 512..=547
            // without the ABI bit. Reject that legacy compat range as well.
            p.push(instruction(JGE, 0, 2, 512));
            p.push(instruction(JGE, 1, 0, 548));
            p.push(instruction(RET, 0, 0, ERRNO | libc::EPERM as u32));
            deny(&mut p, libc::SYS_fork, libc::EPERM);
            deny(&mut p, libc::SYS_vfork, libc::EPERM);
        }
        deny(&mut p, libc::SYS_execve, libc::EPERM);
        deny(&mut p, libc::SYS_execveat, libc::EPERM);
        // classic BPF cannot dereference clone3's userspace clone_args. ENOSYS
        // makes glibc use clone, whose flags the filter can safely examine.
        deny(&mut p, libc::SYS_clone3, libc::ENOSYS);
        p.extend([
            instruction(JEQ, 0, 3, libc::SYS_clone as u32),
            instruction(LD_ABS, 0, 0, 16),
            instruction(JSET, 1, 0, libc::CLONE_THREAD as u32),
            instruction(RET, 0, 0, ERRNO | libc::EPERM as u32),
            instruction(RET, 0, 0, ALLOW),
        ]);
        p
    }

    pub fn install() -> io::Result<()> {
        let program = policy();
        let filter = sock_fprog {
            len: program.len() as u16,
            filter: program.as_ptr().cast_mut(),
        };
        // SAFETY: prctl takes integer arguments here. The program and descriptor
        // remain alive during seccomp; the kernel copies and verifies the BPF.
        // TSYNC applies the policy to all existing threads, and future threads
        // inherit it. A positive result identifies a thread TSYNC couldn't lock
        // down: that is a failure too, never a reason to continue unprotected.
        unsafe {
            if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0 {
                return Err(io::Error::last_os_error());
            }
            match libc::syscall(libc::SYS_seccomp, FILTER, TSYNC, &filter) {
                0 => Ok(()),
                -1 => Err(io::Error::last_os_error()),
                _ => Err(io::Error::other(
                    "could not apply privacy policy to every thread",
                )),
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        // Interpret this small BPF subset to cover alternate ABIs without
        // actually executing unknown syscalls on the test machine.
        fn evaluate(arch: u32, nr: u32, flags: u32) -> u32 {
            let p = policy();
            let mut pc = 0;
            let mut accumulator = 0;
            loop {
                let ins = &p[pc];
                pc += 1;
                match ins.code {
                    LD_ABS => {
                        accumulator = match ins.k {
                            0 => nr,
                            4 => arch,
                            16 => flags,
                            _ => panic!("unexpected seccomp_data offset"),
                        }
                    }
                    JEQ | JGE | JSET => {
                        let condition = match ins.code {
                            JEQ => accumulator == ins.k,
                            JGE => accumulator >= ins.k,
                            JSET => accumulator & ins.k != 0,
                            _ => unreachable!(),
                        };
                        pc += usize::from(if condition { ins.jt } else { ins.jf });
                    }
                    RET => return ins.k,
                    _ => panic!("unexpected BPF opcode"),
                }
            }
        }

        #[test]
        fn rejects_other_architectures_and_processes_but_allows_threads() {
            assert_eq!(evaluate(0, libc::SYS_execve as u32, 0), KILL_PROCESS);
            for syscall in [libc::SYS_execve, libc::SYS_execveat, libc::SYS_clone] {
                assert_eq!(
                    evaluate(ARCH, syscall as u32, 0),
                    ERRNO | libc::EPERM as u32
                );
            }
            assert_eq!(
                evaluate(ARCH, libc::SYS_clone3 as u32, 0),
                ERRNO | libc::ENOSYS as u32
            );
            assert_eq!(
                evaluate(ARCH, libc::SYS_clone as u32, libc::CLONE_THREAD as u32),
                ALLOW
            );
            assert_eq!(evaluate(ARCH, libc::SYS_read as u32, 0), ALLOW);
        }

        #[cfg(target_arch = "x86_64")]
        #[test]
        fn rejects_fork_vfork_and_x32() {
            for syscall in [
                libc::SYS_fork as u32,
                libc::SYS_vfork as u32,
                512,
                520,
                547,
                0x4000_0000 | 520,
                0x4000_0000 | 545,
            ] {
                assert_eq!(evaluate(ARCH, syscall, 0), ERRNO | libc::EPERM as u32);
            }
        }
    }
}

/// Install the mandatory policy, or refuse to run. There is no opt-out.
pub fn install() -> io::Result<()> {
    #[cfg(all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    ))]
    return linux::install();

    #[cfg(not(all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    )))]
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "process privacy requires Linux x86_64 or aarch64 with seccomp",
    ))
}
