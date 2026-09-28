//! Safe wrappers over the C shim. Every `unsafe` block in the crate is here.
//! On targets without the kernel (`ish_stub`), each call fails cleanly.

use crate::ExitRegistry;
use std::fs::File;
use std::path::Path;
use std::sync::Arc;

/// Receives terminal output: the terminal's id and the bytes.
pub type PtyHandler = dyn Fn(i32, &[u8]) + Send + Sync;

pub struct Proc {
    pub pid: i32,
    pub stdin: File,
    pub stdout: File,
    pub stderr: File,
}

#[cfg(not(ish_stub))]
mod sys {
    use std::os::raw::{c_char, c_int, c_void};

    #[repr(C)]
    pub struct Proc {
        pub pid: c_int,
        pub stdin_write: c_int,
        pub stdout_read: c_int,
        pub stderr_read: c_int,
    }

    pub type ExitCb = extern "C" fn(pid: c_int, code: c_int, ctx: *mut c_void);
    pub type PtyOutCb = extern "C" fn(pty_id: c_int, buf: *const c_char, len: c_int, ctx: *mut c_void);

    #[repr(C)]
    pub struct Pty {
        pub pid: c_int,
        pub pty_id: c_int,
    }

    extern "C" {
        pub fn solos_ish_boot(rootfs_dir: *const c_char) -> c_int;
        pub fn solos_ish_bind_mount(guest: *const c_char, host: *const c_char, read_only: c_int) -> c_int;
        pub fn solos_ish_spawn(
            path: *const c_char,
            argv: *const *const c_char,
            envp: *const *const c_char,
            out: *mut Proc,
        ) -> c_int;
        pub fn solos_ish_kill_group(pid: c_int) -> c_int;
        pub fn solos_ish_set_exit_handler(cb: Option<ExitCb>, ctx: *mut c_void);
        pub fn solos_ish_set_dns(nameservers: *const c_char) -> c_int;
        pub fn solos_ish_set_pty_handler(cb: PtyOutCb, ctx: *mut c_void);
        pub fn solos_ish_spawn_pty(
            path: *const c_char,
            argv: *const *const c_char,
            envp: *const *const c_char,
            rows: c_int,
            cols: c_int,
            out: *mut Pty,
        ) -> c_int;
        pub fn solos_ish_pty_input(pty_id: c_int, buf: *const c_char, len: c_int) -> c_int;
        pub fn solos_ish_pty_resize(pty_id: c_int, rows: c_int, cols: c_int) -> c_int;
        pub fn solos_ish_pty_close(pty_id: c_int) -> c_int;
        pub fn solos_ish_write_file(path: *const c_char, content: *const c_char, length: c_int, mode: c_int) -> c_int;
    }
}

#[cfg(not(ish_stub))]
mod imp {
    use super::*;
    use std::ffi::CString;
    use std::os::fd::FromRawFd;
    use std::os::raw::{c_int, c_void};

    fn cstr(s: &str) -> Result<CString, String> {
        CString::new(s).map_err(|_| format!("NUL in {s:?}"))
    }

    pub fn install_exit_handler(registry: Arc<ExitRegistry>) {
        extern "C" fn on_exit(pid: c_int, code: c_int, ctx: *mut c_void) {
            // SAFETY: ctx is the registry leaked below; it lives for the process.
            let registry = unsafe { &*(ctx as *const ExitRegistry) };
            registry.notify(pid, code);
        }
        let ptr = Arc::into_raw(registry) as *mut c_void;
        unsafe { sys::solos_ish_set_exit_handler(Some(on_exit), ptr) };
    }

    pub fn boot(rootfs: &Path) -> Result<(), String> {
        let dir = cstr(&rootfs.to_string_lossy())?;
        match unsafe { sys::solos_ish_boot(dir.as_ptr()) } {
            0 => Ok(()),
            e => Err(format!("the kernel did not boot ({e}) from {}", rootfs.display())),
        }
    }

    pub fn bind_mount(guest: &str, host: &Path) -> Result<(), String> {
        let (g, h) = (cstr(guest)?, cstr(&host.to_string_lossy())?);
        match unsafe { sys::solos_ish_bind_mount(g.as_ptr(), h.as_ptr(), 0) } {
            0 => Ok(()),
            e => Err(format!("could not mount {} at {guest} ({e})", host.display())),
        }
    }

    pub fn spawn(path: &str, argv: &[String], envp: &[String]) -> Result<Proc, String> {
        let path = cstr(path)?;
        let argv: Vec<CString> = argv.iter().map(|a| cstr(a)).collect::<Result<_, _>>()?;
        let envp: Vec<CString> = envp.iter().map(|a| cstr(a)).collect::<Result<_, _>>()?;
        let mut argv_p: Vec<*const i8> = argv.iter().map(|c| c.as_ptr()).collect();
        argv_p.push(std::ptr::null());
        let mut envp_p: Vec<*const i8> = envp.iter().map(|c| c.as_ptr()).collect();
        envp_p.push(std::ptr::null());
        let mut out = sys::Proc { pid: 0, stdin_write: -1, stdout_read: -1, stderr_read: -1 };
        let rc = unsafe { sys::solos_ish_spawn(path.as_ptr(), argv_p.as_ptr(), envp_p.as_ptr(), &mut out) };
        if rc != 0 {
            return Err(format!("the guest could not start the command ({rc})"));
        }
        // SAFETY: the shim hands over ownership of these three descriptors.
        unsafe {
            Ok(Proc {
                pid: out.pid,
                stdin: File::from_raw_fd(out.stdin_write),
                stdout: File::from_raw_fd(out.stdout_read),
                stderr: File::from_raw_fd(out.stderr_read),
            })
        }
    }

    pub fn kill_group(pid: i32) {
        unsafe { sys::solos_ish_kill_group(pid) };
    }

    /// Where every terminal's output goes; called on guest threads, so it
    /// must not block. Installed once, for the life of the process.
    pub fn install_pty_handler(handler: Arc<PtyHandler>) {
        extern "C" fn on_output(pty_id: c_int, buf: *const std::os::raw::c_char, len: c_int, ctx: *mut c_void) {
            if buf.is_null() || len <= 0 {
                return;
            }
            // SAFETY: ctx is the box leaked below; buf holds len bytes for
            // the duration of the call.
            let handler = unsafe { &*(ctx as *const Arc<PtyHandler>) };
            let bytes = unsafe { std::slice::from_raw_parts(buf as *const u8, len as usize) };
            handler(pty_id, bytes);
        }
        // A thin pointer to the (fat) handler, kept for the life of the process.
        let ptr = Box::into_raw(Box::new(handler)) as *mut c_void;
        unsafe { sys::solos_ish_set_pty_handler(on_output, ptr) };
    }

    pub fn spawn_pty(path: &str, argv: &[String], envp: &[String], rows: u16, cols: u16) -> Result<(i32, i32), String> {
        let path = cstr(path)?;
        let argv: Vec<CString> = argv.iter().map(|a| cstr(a)).collect::<Result<_, _>>()?;
        let envp: Vec<CString> = envp.iter().map(|a| cstr(a)).collect::<Result<_, _>>()?;
        let mut argv_p: Vec<*const i8> = argv.iter().map(|c| c.as_ptr()).collect();
        argv_p.push(std::ptr::null());
        let mut envp_p: Vec<*const i8> = envp.iter().map(|c| c.as_ptr()).collect();
        envp_p.push(std::ptr::null());
        let mut out = sys::Pty { pid: 0, pty_id: -1 };
        let rc = unsafe {
            sys::solos_ish_spawn_pty(path.as_ptr(), argv_p.as_ptr(), envp_p.as_ptr(), rows as c_int, cols as c_int, &mut out)
        };
        if rc != 0 {
            return Err(format!("the guest could not open a terminal ({rc})"));
        }
        Ok((out.pid, out.pty_id))
    }

    /// Bytes the terminal took (possibly fewer than offered), or `None`
    /// when it is full for now, or `Err` when it is gone.
    pub fn pty_input(pty_id: i32, bytes: &[u8]) -> Result<Option<usize>, ()> {
        let n = unsafe { sys::solos_ish_pty_input(pty_id, bytes.as_ptr() as *const std::os::raw::c_char, bytes.len() as c_int) };
        match n {
            n if n > 0 => Ok(Some(n as usize)),
            // -EAGAIN: the line buffer is full until the program reads.
            -11 | 0 => Ok(None),
            _ => Err(()),
        }
    }

    pub fn pty_resize(pty_id: i32, rows: u16, cols: u16) {
        unsafe { sys::solos_ish_pty_resize(pty_id, rows as c_int, cols as c_int) };
    }

    pub fn pty_close(pty_id: i32) {
        unsafe { sys::solos_ish_pty_close(pty_id) };
    }

    /// Write a file inside the guest's own filesystem (not a mount).
    pub fn write_guest_file(path: &str, content: &[u8], mode: u32) -> Result<(), String> {
        let p = cstr(path)?;
        match unsafe { sys::solos_ish_write_file(p.as_ptr(), content.as_ptr() as *const std::os::raw::c_char, content.len() as c_int, mode as c_int) } {
            0 => Ok(()),
            e => Err(format!("could not write {path} in the guest ({e})")),
        }
    }

    pub fn set_dns() -> Result<(), String> {
        match unsafe { sys::solos_ish_set_dns(std::ptr::null()) } {
            0 => Ok(()),
            e => Err(format!("could not write the guest's resolv.conf ({e})")),
        }
    }
}

#[cfg(ish_stub)]
mod imp {
    use super::*;
    const NONE: &str = "the iSH sandbox exists only on iOS";
    pub fn install_exit_handler(_r: Arc<ExitRegistry>) {}
    pub fn boot(_r: &Path) -> Result<(), String> {
        Err(NONE.into())
    }
    pub fn bind_mount(_g: &str, _h: &Path) -> Result<(), String> {
        Err(NONE.into())
    }
    pub fn spawn(_p: &str, _a: &[String], _e: &[String]) -> Result<Proc, String> {
        Err(NONE.into())
    }
    pub fn kill_group(_pid: i32) {}
    pub fn install_pty_handler(_h: Arc<PtyHandler>) {}
    pub fn spawn_pty(_p: &str, _a: &[String], _e: &[String], _r: u16, _c: u16) -> Result<(i32, i32), String> {
        Err(NONE.into())
    }
    pub fn pty_input(_id: i32, _b: &[u8]) -> Result<Option<usize>, ()> {
        Err(())
    }
    pub fn pty_resize(_id: i32, _r: u16, _c: u16) {}
    pub fn pty_close(_id: i32) {}
    pub fn write_guest_file(_p: &str, _c: &[u8], _m: u32) -> Result<(), String> {
        Err(NONE.into())
    }
    pub fn set_dns() -> Result<(), String> {
        Err(NONE.into())
    }
}

pub use imp::*;
