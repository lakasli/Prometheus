//! Fail-closed, unprivileged compiler/Component-worker isolation. This policy is
//! independent of the unavailable Harness namespace sandbox. It does not grant
//! a guest host imports, production paths, inherited descriptors, or sockets.
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const POLICY: &str = "landlock4-seccomp-stateless-v3";
pub const MEMORY_BYTES: u64 = 1024 * 1024 * 1024;
pub const TASKS: u64 = 32;
pub const FILE_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Isolation {
    pub policy: String,
    pub landlock_abi: i32,
    pub seccomp: bool,
    pub no_new_privileges: bool,
    pub inherited_fds_closed: bool,
    pub cgroup_memory_bytes: u64,
    pub cgroup_tasks: u64,
    pub rlimits_enforced: bool,
    pub pipe_ioctls_verified: bool,
    pub clone_flags_verified: bool,
    pub unit: String,
}

pub fn toolchain() -> Result<PathBuf, &'static str> {
    let path = std::env::var_os("DSH_PLUGIN_RUSTC")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|home| {
                PathBuf::from(home)
                    .join(".rustup/toolchains/1.88.0-aarch64-unknown-linux-gnu/bin/rustc")
            })
        })
        .ok_or("plugin_rustc_unavailable")?;
    let path = path
        .canonicalize()
        .map_err(|_| "plugin_rustc_unavailable")?;
    if path.file_name().and_then(|p| p.to_str()) != Some("rustc") {
        return Err("plugin_rustc_invalid");
    }
    let root = path
        .parent()
        .and_then(Path::parent)
        .ok_or("plugin_rustc_invalid")?;
    if !root.join("lib/rustlib/wasm32-unknown-unknown/lib").is_dir() {
        return Err("plugin_wasm_target_unavailable");
    }
    linker(&path)?;
    Ok(path)
}

/// An absolute sysroot linker avoids Rust 1.88's relative-PATH fork/socketpair
/// fallback. The compiler's external socket/IPC restrictions stay unchanged.
pub fn linker(rustc: &Path) -> Result<PathBuf, &'static str> {
    let host = match std::env::consts::ARCH {
        "aarch64" => "aarch64-unknown-linux-gnu",
        "x86_64" => "x86_64-unknown-linux-gnu",
        _ => return Err("plugin_linker_unavailable"),
    };
    let root = rustc
        .parent()
        .and_then(Path::parent)
        .ok_or("plugin_rustc_invalid")?
        .canonicalize()
        .map_err(|_| "plugin_rustc_invalid")?;
    let path = root
        .join("lib/rustlib")
        .join(host)
        .join("bin/rust-lld")
        .canonicalize()
        .map_err(|_| "plugin_linker_unavailable")?;
    if !path.starts_with(&root) || !path.is_file() {
        return Err("plugin_linker_unavailable");
    }
    Ok(path)
}

#[cfg(target_os = "linux")]
mod linux {
    use super::*;
    use std::{ffi::CString, os::unix::ffi::OsStrExt};
    const EXEC: u64 = 1;
    const WRITE: u64 = 1 << 1;
    const READ: u64 = 1 << 2;
    const READDIR: u64 = 1 << 3;
    const ALL_FS: u64 = (1 << 15) - 1;
    const RO: u64 = EXEC | READ | READDIR;
    #[repr(C)]
    struct Rules {
        fs: u64,
        net: u64,
    }
    #[repr(C, packed)]
    struct Beneath {
        access: u64,
        parent: i32,
    }
    #[repr(C)]
    struct Filter {
        code: u16,
        jt: u8,
        jf: u8,
        k: u32,
    }
    // Only the two clone flag combinations observed in the fixed compiler,
    // linker and glibc pthread/posix_spawn paths. CLONE_PARENT must not change
    // reaping topology; namespace, ptrace and pidfd flags are unnecessary too.
    const THREAD_FLAGS: u32 = 0x003d0f00;
    const SPAWN_FLAGS: u32 = 0x00004111;
    fn clone_rules(list: &mut Vec<Filter>, clone_nr: u32) {
        let mut rules = vec![
            Filter {
                code: 0x20,
                jt: 0,
                jf: 0,
                k: 20,
            }, // flags high word
            Filter {
                code: 0x15,
                jt: 1,
                jf: 0,
                k: 0,
            },
            Filter {
                code: 0x06,
                jt: 0,
                jf: 0,
                k: 0x00050001,
            },
            Filter {
                code: 0x20,
                jt: 0,
                jf: 0,
                k: 16,
            }, // flags low word
        ];
        for flags in [THREAD_FLAGS, SPAWN_FLAGS] {
            rules.push(Filter {
                code: 0x15,
                jt: 0,
                jf: 1,
                k: flags,
            });
            rules.push(Filter {
                code: 0x06,
                jt: 0,
                jf: 0,
                k: 0x7fff0000,
            });
        }
        rules.push(Filter {
            code: 0x06,
            jt: 0,
            jf: 0,
            k: 0x00050001,
        });
        list.push(Filter {
            code: 0x15,
            jt: 0,
            jf: rules.len() as u8,
            k: clone_nr,
        });
        list.extend(rules);
        list.push(Filter {
            code: 0x20,
            jt: 0,
            jf: 0,
            k: 0,
        });
    }
    #[repr(C)]
    struct Program {
        len: u16,
        filter: *const Filter,
    }
    #[repr(C)]
    struct Limits {
        current: u64,
        maximum: u64,
    }
    unsafe extern "C" {
        fn syscall(number: isize, ...) -> isize;
        fn prctl(option: i32, ...) -> i32;
        fn open(path: *const std::ffi::c_char, flags: i32, ...) -> i32;
        fn close(fd: i32) -> i32;
        fn fcntl(fd: i32, command: i32, ...) -> i32;
        fn getpid() -> i32;
        fn __errno_location() -> *mut i32;
        fn getrlimit(resource: i32, limits: *mut Limits) -> i32;
        fn pipe2(descriptors: *mut i32, flags: i32) -> i32;
        fn ioctl(fd: i32, request: usize, ...) -> i32;
    }
    fn ruleset(abi: i32) -> Result<i32, &'static str> {
        if abi < 4 {
            return Err("plugin_landlock4_required");
        }
        let attr = Rules { fs: ALL_FS, net: 3 };
        let fd = unsafe { syscall(444, &attr as *const Rules, std::mem::size_of::<Rules>(), 0) };
        if fd < 0 {
            Err("plugin_landlock_unavailable")
        } else {
            Ok(fd as i32)
        }
    }
    pub fn abi() -> i32 {
        unsafe { syscall(444, std::ptr::null::<u8>(), 0usize, 1u32) as i32 }
    }
    fn allow(fd: i32, path: &Path, access: u64) -> Result<(), &'static str> {
        let path = path
            .canonicalize()
            .map_err(|_| "plugin_sandbox_path_unavailable")?;
        let is_dir = path.is_dir();
        let name =
            CString::new(path.as_os_str().as_bytes()).map_err(|_| "plugin_sandbox_path_invalid")?;
        let parent = unsafe { open(name.as_ptr(), 0x200000 | 0x80000) }; // O_PATH | O_CLOEXEC
        if parent < 0 {
            return Err("plugin_sandbox_path_unavailable");
        }
        let attr = Beneath {
            access: if is_dir {
                access
            } else {
                access & (EXEC | READ | WRITE | (1 << 14))
            },
            parent,
        };
        let result = unsafe { syscall(445, fd, 1, &attr as *const Beneath, 0) };
        unsafe {
            close(parent);
        }
        if result != 0 {
            return Err("plugin_landlock_rule_rejected");
        }
        Ok(())
    }
    fn filter() -> Result<(), &'static str> {
        #[cfg(target_arch = "aarch64")]
        const ARCH: u32 = 0xc00000b7;
        #[cfg(target_arch = "x86_64")]
        const ARCH: u32 = 0xc000003e;
        // Default deny, including future syscalls. File-content operations are
        // additionally mediated by Landlock. There are no sockets, SysV/POSIX
        // IPC, peer signals, ptrace/process_vm/pidfd, io_uring, filesystem
        // metadata mutation, namespace/mount, keyring or kernel-admin syscalls.
        #[cfg(target_arch = "aarch64")]
        let allowed: &[u32] = &[
            17, 19, 20, 21, 22, 23, 24, 32, 34, 35, 37, 38, 43, 44, 45, 46, 47, 48, 49, 50, 56, 57,
            59, 61, 62, 63, 64, 65, 66, 67, 68, 69, 70, 71, 72, 73, 78, 79, 80, 82, 83, 93, 94, 95,
            96, 98, 99, 101, 102, 103, 113, 114, 115, 120, 121, 123, 124, 125, 126, 132, 133, 134,
            135, 136, 137, 139, 153, 154, 155, 156, 157, 160, 163, 165, 166, 167, 169, 172, 173,
            174, 175, 176, 177, 178, 179, 214, 215, 216, 221, 222, 226, 227, 233, 260, 276, 278,
            279, 281, 283, 286, 287, 291, 293, 437, 439, 441, 449,
        ];
        #[cfg(target_arch = "x86_64")]
        let allowed: &[u32] = &[
            0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 17, 18, 19, 20, 21, 22, 23, 24,
            25, 26, 27, 28, 32, 33, 35, 36, 37, 38, 39, 57, 58, 59, 60, 61, 63, 73, 74, 75, 76, 77,
            78, 79, 80, 81, 82, 83, 84, 85, 86, 87, 89, 95, 96, 97, 98, 99, 100, 102, 104, 107,
            108, 110, 111, 112, 115, 118, 119, 120, 121, 124, 125, 126, 127, 128, 130, 131, 133,
            134, 135, 137, 138, 158, 160, 186, 202, 204, 218, 219, 228, 229, 230, 231, 232, 233,
            257, 258, 262, 263, 264, 265, 267, 269, 270, 271, 273, 281, 285, 289, 290, 291, 292,
            293, 295, 296, 316, 318, 319, 322, 324, 327, 328, 332, 334, 437, 439, 441, 449,
        ];
        // Architecture check prevents a compat syscall ABI from bypassing the policy.
        let mut list = vec![
            Filter {
                code: 0x20,
                jt: 0,
                jf: 0,
                k: 4,
            },
            Filter {
                code: 0x15,
                jt: 1,
                jf: 0,
                k: ARCH,
            },
            Filter {
                code: 0x06,
                jt: 0,
                jf: 0,
                k: 0x80000000,
            },
            Filter {
                code: 0x20,
                jt: 0,
                jf: 0,
                k: 0,
            },
        ];
        // Reject x32 and unknown out-of-range syscall numbers as well.
        list.push(Filter {
            code: 0x35,
            jt: 0,
            jf: 1,
            k: 1024,
        });
        list.push(Filter {
            code: 0x06,
            jt: 0,
            jf: 0,
            k: 0x00050000 | 1,
        });
        // clone3 can move a child to a different cgroup using an O_PATH fd.
        // ENOSYS makes glibc fall back to legacy clone for compiler threads;
        // the legacy syscall has no CLONE_INTO_CGROUP argument.
        list.push(Filter {
            code: 0x15,
            jt: 0,
            jf: 1,
            k: 435,
        });
        list.push(Filter {
            code: 0x06,
            jt: 0,
            jf: 0,
            k: 0x00050000 | 38,
        });
        #[cfg(target_arch = "aarch64")]
        let clone_nr = 220;
        #[cfg(target_arch = "x86_64")]
        let clone_nr = 56;
        clone_rules(&mut list, clone_nr);
        // Rust's wait_with_output uses FIONBIO on its own stdout/stderr
        // pipes. No other device/TTY ioctl is needed or granted.
        #[cfg(target_arch = "aarch64")]
        let ioctl_nr = 29;
        #[cfg(target_arch = "x86_64")]
        let ioctl_nr = 16;
        list.push(Filter {
            code: 0x15,
            jt: 0,
            jf: 4,
            k: ioctl_nr,
        });
        list.push(Filter {
            code: 0x20,
            jt: 0,
            jf: 0,
            k: 24,
        });
        list.push(Filter {
            code: 0x15,
            jt: 0,
            jf: 1,
            k: 0x5421,
        });
        list.push(Filter {
            code: 0x06,
            jt: 0,
            jf: 0,
            k: 0x7fff0000,
        });
        list.push(Filter {
            code: 0x06,
            jt: 0,
            jf: 0,
            k: 0x00050000 | 1,
        });
        list.push(Filter {
            code: 0x20,
            jt: 0,
            jf: 0,
            k: 0,
        });
        // fcntl is limited to descriptor duplication/flags: F_SETOWN and file
        // leases could otherwise cause signals to another same-UID process.
        #[cfg(target_arch = "aarch64")]
        let (fcntl_nr, prlimit_nr) = (25, 261);
        #[cfg(target_arch = "x86_64")]
        let (fcntl_nr, prlimit_nr) = (72, 302);
        list.push(Filter {
            code: 0x15,
            jt: 0,
            jf: 14,
            k: fcntl_nr,
        });
        list.push(Filter {
            code: 0x20,
            jt: 0,
            jf: 0,
            k: 24,
        });
        for command in [0, 1, 2, 3, 4, 1030] {
            list.push(Filter {
                code: 0x15,
                jt: 0,
                jf: 1,
                k: command,
            });
            list.push(Filter {
                code: 0x06,
                jt: 0,
                jf: 0,
                k: 0x7fff0000,
            });
        }
        list.push(Filter {
            code: 0x06,
            jt: 0,
            jf: 0,
            k: 0x00050000 | 1,
        });
        list.push(Filter {
            code: 0x20,
            jt: 0,
            jf: 0,
            k: 0,
        });
        // prlimit64 may query/change this worker only, never a peer's limits.
        list.push(Filter {
            code: 0x15,
            jt: 0,
            jf: 6,
            k: prlimit_nr,
        });
        list.push(Filter {
            code: 0x20,
            jt: 0,
            jf: 0,
            k: 16,
        });
        for pid in [0, unsafe { getpid() } as u32] {
            list.push(Filter {
                code: 0x15,
                jt: 0,
                jf: 1,
                k: pid,
            });
            list.push(Filter {
                code: 0x06,
                jt: 0,
                jf: 0,
                k: 0x7fff0000,
            });
        }
        list.push(Filter {
            code: 0x06,
            jt: 0,
            jf: 0,
            k: 0x00050000 | 1,
        });
        list.push(Filter {
            code: 0x20,
            jt: 0,
            jf: 0,
            k: 0,
        });
        for nr in allowed {
            list.push(Filter {
                code: 0x15,
                jt: 0,
                jf: 1,
                k: *nr,
            });
            list.push(Filter {
                code: 0x06,
                jt: 0,
                jf: 0,
                k: 0x7fff0000,
            });
        }
        list.push(Filter {
            code: 0x06,
            jt: 0,
            jf: 0,
            k: 0x00050000 | 1,
        });
        let program = Program {
            len: list.len() as u16,
            filter: list.as_ptr(),
        };
        if unsafe { prctl(22, 2, &program as *const Program) } != 0 {
            return Err("plugin_seccomp_unavailable");
        }
        Ok(())
    }
    fn pipe_ioctl_probe() -> bool {
        let mut descriptors = [-1, -1];
        if unsafe { pipe2(descriptors.as_mut_ptr(), 0x80000) } != 0 {
            return false;
        }
        let mut enabled = 1i32;
        let allowed = unsafe { ioctl(descriptors[0], 0x5421, &mut enabled as *mut i32) } == 0;
        // TCGETS would normally fail ENOTTY on this private pipe. EPERM
        // specifically proves that the syscall policy blocked the request.
        let forbidden = unsafe {
            ioctl(descriptors[0], 0x5401, &mut enabled as *mut i32) == -1
                && *__errno_location() == 1
        };
        unsafe {
            close(descriptors[0]);
            close(descriptors[1]);
        }
        allowed && forbidden
    }
    fn cgroup() -> Result<(String, u64, u64), &'static str> {
        let membership =
            std::fs::read_to_string("/proc/self/cgroup").map_err(|_| "plugin_cgroup_unverified")?;
        let group = membership
            .lines()
            .find_map(|line| line.strip_prefix("0::"))
            .ok_or("plugin_cgroup_unverified")?;
        if !group.starts_with('/') || group.contains("..") || group.contains('\\') {
            return Err("plugin_cgroup_unverified");
        }
        let unit = group.rsplit('/').next().ok_or("plugin_cgroup_unverified")?;
        let uuid = unit
            .strip_prefix("dsh-plugin-")
            .and_then(|s| s.strip_suffix(".service"))
            .and_then(|s| uuid::Uuid::parse_str(s).ok());
        if uuid.is_none_or(|v| format!("dsh-plugin-{v}.service") != unit) {
            return Err("plugin_cgroup_unverified");
        }
        let root = Path::new("/sys/fs/cgroup").join(group.trim_start_matches('/'));
        let number = |name| {
            std::fs::read_to_string(root.join(name))
                .ok()
                .and_then(|s| s.trim().parse::<u64>().ok())
        };
        if number("memory.max") != Some(MEMORY_BYTES)
            || number("memory.swap.max") != Some(0)
            || number("pids.max") != Some(TASKS)
        {
            return Err("plugin_cgroup_unverified");
        }
        Ok((unit.to_owned(), MEMORY_BYTES, TASKS))
    }
    pub fn install(work: &Path, rustc: &Path) -> Result<Isolation, &'static str> {
        let abi = abi();
        let (unit, memory, tasks) = cgroup()?;
        for (resource, expected) in [(0, 60), (1, FILE_BYTES), (4, 0), (7, 64)] {
            let mut limits = Limits {
                current: 0,
                maximum: 0,
            };
            if unsafe { getrlimit(resource, &mut limits) } != 0
                || limits.current != expected
                || limits.maximum != expected
            {
                return Err("plugin_rlimits_unverified");
            }
        }
        // Close descriptors before any untrusted source or wasm is read. No
        // secret/config/daemon socket can cross this process boundary.
        let closed = unsafe { syscall(436, 3u32, u32::MAX, 0u32) } == 0;
        if !closed {
            return Err("plugin_close_range_unavailable");
        }
        for fd in 3..64 {
            if unsafe { fcntl(fd, 1) } != -1 {
                return Err("plugin_inherited_fd");
            }
        }
        let fd = ruleset(abi)?;
        let result = (|| {
            let scratch = WRITE
                | READ
                | READDIR
                | (1 << 4)
                | (1 << 5)
                | (1 << 7)
                | (1 << 8)
                | (1 << 13)
                | (1 << 14);
            allow(fd, work, scratch)?;
            let chain = rustc
                .parent()
                .and_then(Path::parent)
                .ok_or("plugin_rustc_invalid")?;
            allow(fd, chain, RO)?;
            // System runtime and read-only tools. No /etc directory, /home,
            // /proc, /sys, /run, /tmp or user login directory is granted.
            for name in [
                "/usr",
                "/lib",
                "/lib64",
                "/etc/ld.so.cache",
                "/dev/null",
                "/dev/urandom",
            ] {
                let p = Path::new(name);
                if p.exists() {
                    allow(
                        fd,
                        p,
                        if name == "/dev/null" {
                            READ | WRITE
                        } else {
                            RO
                        },
                    )?;
                }
            }
            if unsafe { prctl(38, 1, 0, 0, 0) } != 0 {
                return Err("plugin_no_new_privileges_unavailable");
            }
            if unsafe { syscall(446, fd, 0) } != 0 {
                return Err("plugin_landlock_enforcement_failed");
            }
            Ok(())
        })();
        unsafe {
            close(fd);
        }
        result?;
        filter()?;
        if !pipe_ioctl_probe() {
            return Err("plugin_pipe_ioctls_unverified");
        }
        if !clone_flags_probe() {
            return Err("plugin_clone_flags_unverified");
        }
        Ok(Isolation {
            policy: POLICY.into(),
            landlock_abi: abi,
            seccomp: true,
            no_new_privileges: true,
            inherited_fds_closed: true,
            cgroup_memory_bytes: memory,
            cgroup_tasks: tasks,
            rlimits_enforced: true,
            pipe_ioctls_verified: true,
            clone_flags_verified: true,
            unit,
        })
    }
    fn clone_flags_probe() -> bool {
        #[cfg(target_arch = "aarch64")]
        const CLONE: isize = 220;
        #[cfg(target_arch = "x86_64")]
        const CLONE: isize = 56;
        // Both variants omit CLONE_SIGHAND required by CLONE_THREAD, so even
        // without our filter Linux returns EINVAL without creating a child.
        // EPERM specifically demonstrates our earlier seccomp rejection.
        [0x0001800ausize, 0x10010000usize]
            .iter()
            .all(|flags| unsafe {
                syscall(CLONE, *flags, 0usize, 0usize, 0usize, 0usize) == -1
                    && *__errno_location() == 1
            })
            && unsafe { syscall(435, 0usize, 0usize) == -1 && *__errno_location() == 38 }
    }
    pub fn process_probe(peer: u32) -> bool {
        // Signal 0 and an empty read verify EPERM on a controlled synthetic
        // peer without sending a signal or reading any memory bytes.
        #[cfg(target_arch = "aarch64")]
        let (kill, read) = (129, 270);
        #[cfg(target_arch = "x86_64")]
        let (kill, read) = (62, 310);
        unsafe {
            syscall(kill, peer, 0) == -1
                && *__errno_location() == 1
                && syscall(read, peer, 0usize, 0usize, 0usize, 0usize, 0usize) == -1
                && *__errno_location() == 1
        }
    }
    pub fn socket_probe() -> bool {
        #[cfg(target_arch = "aarch64")]
        const SOCKET: isize = 198;
        #[cfg(target_arch = "x86_64")]
        const SOCKET: isize = 41;
        [1, 2, 10].iter().all(|family| unsafe {
            syscall(SOCKET, *family, 1, 0) == -1 && *__errno_location() == 1
        })
    }
    #[cfg(test)]
    mod tests {
        use super::*;
        fn decision(program: &[Filter], nr: u32, flags: u64) -> u32 {
            let (mut pc, mut a) = (0, nr);
            loop {
                let instruction = &program[pc];
                match instruction.code {
                    0x20 => {
                        a = match instruction.k {
                            0 => nr,
                            16 => flags as u32,
                            20 => (flags >> 32) as u32,
                            _ => panic!("unexpected seccomp load"),
                        }
                    }
                    0x15 => {
                        pc += if a == instruction.k {
                            instruction.jt
                        } else {
                            instruction.jf
                        } as usize
                    }
                    0x06 => return instruction.k,
                    _ => panic!("unexpected seccomp instruction"),
                }
                pc += 1;
            }
        }
        #[test]
        fn clone_policy_accepts_only_observed_thread_and_spawn_flags() {
            for nr in [56, 220] {
                let mut program = vec![];
                clone_rules(&mut program, nr);
                program.push(Filter {
                    code: 0x06,
                    jt: 0,
                    jf: 0,
                    k: 0x7fff0000,
                });
                for flags in [0x3d0f00u64, 0x4111] {
                    assert_eq!(decision(&program, nr, flags), 0x7fff0000);
                    for bit in 0..64 {
                        assert_eq!(
                            decision(&program, nr, flags ^ (1u64 << bit)),
                            0x00050001,
                            "unexpected allowance for flag mutation {bit}"
                        );
                    }
                }
                for flags in [0, 17, 0x1800a, 0x10010000, u64::MAX] {
                    assert_eq!(decision(&program, nr, flags), 0x00050001);
                }
                assert_eq!(decision(&program, nr + 1, u64::MAX), 0x7fff0000);
            }
        }
    }
}
pub fn available() -> bool {
    #[cfg(target_os = "linux")]
    {
        linux::abi() >= 4 && toolchain().is_ok()
    }
    #[cfg(not(target_os = "linux"))]
    {
        false
    }
}
pub fn install(work: &Path, rustc: &Path) -> Result<Isolation, &'static str> {
    #[cfg(target_os = "linux")]
    {
        linux::install(work, rustc)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (work, rustc);
        Err("plugin_linux_required")
    }
}
pub fn process_probe(peer: u32) -> bool {
    #[cfg(target_os = "linux")]
    {
        linux::process_probe(peer)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = peer;
        false
    }
}
pub fn socket_probe() -> bool {
    #[cfg(target_os = "linux")]
    {
        linux::socket_probe()
    }
    #[cfg(not(target_os = "linux"))]
    {
        false
    }
}
