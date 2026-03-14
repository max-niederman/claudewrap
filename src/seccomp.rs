//! Seccomp BPF filter for the sandbox.
//!
//! Generates a denylist filter that blocks dangerous syscalls (ptrace, mount,
//! module loading, BPF, namespace manipulation, etc.) and returns a memfd
//! containing the compiled filter, suitable for passing to `bwrap --seccomp FD`.

use std::io;
use std::os::fd::{FromRawFd, OwnedFd};

// ── BPF instruction constants ───────────────────────────────────────────────

const BPF_LD: u16 = 0x00;
const BPF_W: u16 = 0x00;
const BPF_ABS: u16 = 0x20;
const BPF_JMP: u16 = 0x05;
const BPF_JEQ: u16 = 0x10;
const BPF_K: u16 = 0x00;
const BPF_RET: u16 = 0x06;

const SECCOMP_RET_ALLOW: u32 = 0x7fff_0000;
const SECCOMP_RET_ERRNO: u32 = 0x0005_0000;
const SECCOMP_RET_KILL_PROCESS: u32 = 0x8000_0000;

const EPERM: u32 = 1;

// ── Architecture ────────────────────────────────────────────────────────────

#[cfg(target_arch = "x86_64")]
const AUDIT_ARCH: u32 = 0xc000_003e; // AUDIT_ARCH_X86_64

#[cfg(target_arch = "aarch64")]
const AUDIT_ARCH: u32 = 0xc000_00b7; // AUDIT_ARCH_AARCH64

// ── Blocked syscalls ────────────────────────────────────────────────────────

#[cfg(target_arch = "x86_64")]
const BLOCKED_SYSCALLS: &[u32] = &[
    101, // ptrace              – attach to / inspect other processes
    310, // process_vm_readv    – read another process's memory
    311, // process_vm_writev   – write another process's memory
    165, // mount               – mount filesystems
    166, // umount2             – unmount filesystems
    155, // pivot_root          – change root filesystem
    161, // chroot              – change root directory
    169, // reboot              – reboot the system
    246, // kexec_load          – load a new kernel
    320, // kexec_file_load     – load a new kernel (file variant)
    175, // init_module         – load a kernel module
    313, // finit_module        – load a kernel module (file variant)
    176, // delete_module       – unload a kernel module
    298, // perf_event_open     – performance monitoring (side-channel)
    323, // userfaultfd         – used in exploit chains
    250, // keyctl              – kernel keyring manipulation
    248, // add_key             – add key to kernel keyring
    249, // request_key         – request key from kernel keyring
    321, // bpf                 – load eBPF programs
    272, // unshare             – create new namespaces
    308, // setns               – enter an existing namespace
    304, // open_by_handle_at   – bypass mount namespace isolation
    163, // acct                – process accounting
    172, // iopl                – I/O port access
    173, // ioperm              – I/O port permissions
    133, // mknod               – create device nodes
    259, // mknodat             – create device nodes (at variant)
];

#[cfg(target_arch = "aarch64")]
const BLOCKED_SYSCALLS: &[u32] = &[
    117, // ptrace
    270, // process_vm_readv
    271, // process_vm_writev
    21,  // mount
    39,  // umount2
    41,  // pivot_root
    51,  // chroot
    142, // reboot
    104, // kexec_load
    294, // kexec_file_load
    105, // init_module
    273, // finit_module
    106, // delete_module
    241, // perf_event_open
    282, // userfaultfd
    219, // keyctl
    217, // add_key
    218, // request_key
    280, // bpf
    97,  // unshare
    268, // setns
    265, // open_by_handle_at
    89,  // acct
    //     iopl/ioperm do not exist on aarch64
    33,  // mknod  (mknodat)
];

// ── BPF filter generation ───────────────────────────────────────────────────

#[repr(C)]
#[derive(Clone, Copy)]
struct SockFilter {
    code: u16,
    jt: u8,
    jf: u8,
    k: u32,
}

fn bpf_stmt(code: u16, k: u32) -> SockFilter {
    SockFilter {
        code,
        jt: 0,
        jf: 0,
        k,
    }
}

fn bpf_jump(code: u16, k: u32, jt: u8, jf: u8) -> SockFilter {
    SockFilter { code, jt, jf, k }
}

/// Create a seccomp BPF filter and return a memfd containing it.
///
/// The returned fd has CLOEXEC **not** set, so it will be inherited by child
/// processes (required for `bwrap --seccomp FD`).
#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
pub fn create_filter() -> io::Result<OwnedFd> {
    let n = BLOCKED_SYSCALLS.len();
    let mut prog: Vec<SockFilter> = Vec::with_capacity(4 + n + 2);

    // [0] Load architecture from seccomp_data.arch (offset 4)
    prog.push(bpf_stmt(BPF_LD | BPF_W | BPF_ABS, 4));
    // [1] If arch matches, skip over the kill instruction
    prog.push(bpf_jump(BPF_JMP | BPF_JEQ | BPF_K, AUDIT_ARCH, 1, 0));
    // [2] Wrong architecture — kill the process (prevents arch-switching attacks)
    prog.push(bpf_stmt(BPF_RET | BPF_K, SECCOMP_RET_KILL_PROCESS));
    // [3] Load syscall number from seccomp_data.nr (offset 0)
    prog.push(bpf_stmt(BPF_LD | BPF_W | BPF_ABS, 0));

    // [4 .. 4+n-1] Check each blocked syscall
    // If match: jump forward to the DENY return at index [4+n+1]
    // From instruction at index (4+i), the DENY is at (4+n+1),
    // so jt = (4+n+1) - (4+i+1) = n - i
    for (i, &nr) in BLOCKED_SYSCALLS.iter().enumerate() {
        let jt = (n - i) as u8;
        prog.push(bpf_jump(BPF_JMP | BPF_JEQ | BPF_K, nr, jt, 0));
    }

    // [4+n] ALLOW — syscall not in denylist
    prog.push(bpf_stmt(BPF_RET | BPF_K, SECCOMP_RET_ALLOW));
    // [4+n+1] DENY — return EPERM
    prog.push(bpf_stmt(BPF_RET | BPF_K, SECCOMP_RET_ERRNO | EPERM));

    // Create a memfd (without MFD_CLOEXEC so bwrap inherits the fd)
    let fd = unsafe {
        libc::memfd_create(b"claudewrap-seccomp\0".as_ptr() as *const _, 0)
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }

    // Write the raw filter (array of sock_filter structs)
    let bytes = unsafe {
        std::slice::from_raw_parts(
            prog.as_ptr() as *const u8,
            prog.len() * std::mem::size_of::<SockFilter>(),
        )
    };

    let written = unsafe { libc::write(fd, bytes.as_ptr() as *const _, bytes.len()) };
    if written < 0 || written as usize != bytes.len() {
        let err = io::Error::last_os_error();
        unsafe { libc::close(fd) };
        return Err(err);
    }

    // Seek back to start so bwrap can read it
    if unsafe { libc::lseek(fd, 0, libc::SEEK_SET) } < 0 {
        let err = io::Error::last_os_error();
        unsafe { libc::close(fd) };
        return Err(err);
    }

    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}
