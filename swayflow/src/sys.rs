//! Thin safe wrappers over the few POSIX calls the program needs
//! (`flock`, `kill`, `setsid`, `getuid`). All `unsafe` code lives here.

use std::fs::{File, OpenOptions};
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};

/// An exclusive `flock(2)` lock; released when the file descriptor is closed (on drop).
pub struct FileLock {
    _file: File,
}

fn flock(file: &File, operation: libc::c_int) -> io::Result<()> {
    loop {
        // SAFETY: the descriptor is valid for as long as `file` is borrowed.
        let rc = unsafe { libc::flock(file.as_raw_fd(), operation) };
        if rc == 0 {
            return Ok(());
        }
        let err = io::Error::last_os_error();
        if err.kind() != io::ErrorKind::Interrupted {
            return Err(err);
        }
    }
}

/// Python: `open(path, "w")` — create if needed, truncate.
fn open_lock_file(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)
}

/// Blocks until the exclusive lock on `path` is acquired.
pub fn lock_exclusive(path: &Path) -> io::Result<FileLock> {
    let file = open_lock_file(path)?;
    flock(&file, libc::LOCK_EX)?;
    Ok(FileLock { _file: file })
}

/// Non-blocking variant: `Ok(None)` when somebody else holds the lock.
pub fn try_lock_exclusive(path: &Path) -> io::Result<Option<FileLock>> {
    let file = open_lock_file(path)?;
    match flock(&file, libc::LOCK_EX | libc::LOCK_NB) {
        Ok(()) => Ok(Some(FileLock { _file: file })),
        Err(_) => Ok(None),
    }
}

/// Sends SIGTERM to a single process (never to a process group).
pub fn terminate(pid: i32) -> io::Result<()> {
    if pid <= 0 {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "invalid pid"));
    }
    // SAFETY: plain syscall without any pointer arguments.
    if unsafe { libc::kill(pid, libc::SIGTERM) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

pub fn getuid() -> u32 {
    // SAFETY: `getuid` has no preconditions and cannot fail.
    unsafe { libc::getuid() }
}

/// Starts `exe arg` fully detached: new session, stdio on /dev/null
/// (Python: `Popen(..., start_new_session=True)`).
pub fn spawn_detached(exe: &Path, arg: &str) -> io::Result<()> {
    let mut cmd = Command::new(exe);
    cmd.arg(arg)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // SAFETY: the closure only calls `setsid`, which is async-signal-safe,
    // and does not touch any Rust runtime state between fork and exec.
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() == -1 {
                Err(io::Error::last_os_error())
            } else {
                Ok(())
            }
        });
    }
    cmd.spawn().map(drop)
}
