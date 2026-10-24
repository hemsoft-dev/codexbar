//! Nonblocking reads from the command's exclusively owned anonymous pipes. No reader threads are created.
use std::io::{self, Read};
use std::process::Child;

#[cfg(all(unix, any(target_os = "linux", test)))]
mod linux_group;
#[cfg(target_os = "macos")]
mod mac_group;

#[cfg(unix)]
pub(super) trait Pipe: Read + std::os::fd::AsRawFd {}
#[cfg(unix)]
impl<T: Read + std::os::fd::AsRawFd> Pipe for T {}
#[cfg(windows)]
pub(super) trait Pipe: Read + std::os::windows::io::AsRawHandle {}
#[cfg(windows)]
impl<T: Read + std::os::windows::io::AsRawHandle> Pipe for T {}

#[cfg(unix)]
pub(super) fn prepare(pipe: &impl Pipe) -> io::Result<()> {
    // SAFETY: a valid, exclusively owned, newly created anonymous pipe. No other reader can change its flags.
    if unsafe { libc::fcntl(pipe.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK) } == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(windows)]
pub(super) fn prepare(_: &impl Pipe) -> io::Result<()> {
    Ok(())
}

#[cfg(unix)]
pub(super) fn read(pipe: &mut impl Pipe, bytes: &mut [u8]) -> io::Result<Option<usize>> {
    read_result(pipe.read(bytes))
}

#[cfg(windows)]
pub(super) fn read(pipe: &mut impl Pipe, bytes: &mut [u8]) -> io::Result<Option<usize>> {
    use windows::Win32::Foundation::{ERROR_BROKEN_PIPE, HANDLE};
    use windows::Win32::System::Pipes::PeekNamedPipe;
    let mut available = 0;
    // SAFETY: the pipe handle is valid and this thread is its sole reader. No competing read can consume the
    // reported bytes or block this synchronous handle. Query only; `available` is valid writable storage.
    let peek = unsafe { PeekNamedPipe(HANDLE(pipe.as_raw_handle()), None, 0, None, Some(&mut available), None) };
    if let Err(error) = peek {
        return if error.code() == ERROR_BROKEN_PIPE.to_hresult() {
            Ok(Some(0))
        } else {
            Err(io::Error::other("command pipe query failed"))
        };
    }
    if available == 0 {
        return Ok(None);
    }
    let count = bytes.len().min(available as usize);
    read_result(pipe.read(&mut bytes[..count]))
}

fn read_result(result: io::Result<usize>) -> io::Result<Option<usize>> {
    match result {
        Ok(count) => Ok(Some(count)),
        Err(error) if matches!(error.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted) => Ok(None),
        Err(error) => Err(error),
    }
}

/// Observe exit without reaping on Unix: the root PID still reserves the process-group ID until cleanup.
#[cfg(unix)]
pub(super) fn finished(child: &mut Child) -> io::Result<bool> {
    // SAFETY: zero is a valid initial siginfo_t; waitid initializes it on an observed exit.
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    // SAFETY: this is our unreaped child. WNOHANG never waits and WNOWAIT leaves the PID reserved until `wait`.
    let result = unsafe {
        libc::waitid(
            libc::P_PID,
            child.id(),
            &mut info,
            libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
        )
    };
    if result == -1 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: with P_PID/WEXITED, si_pid identifies the exited child, or stays zero if no exit is ready.
    Ok(unsafe { info.si_pid() } != 0)
}

#[cfg(windows)]
pub(super) fn finished(child: &mut Child) -> io::Result<bool> {
    child.try_wait().map(|status| status.is_some())
}

#[cfg(unix)]
pub(super) fn stop_group(child: &Child) {
    // SAFETY: process_group(0) made this child's own group. It has not been reaped, so the ID cannot be reused.
    // Kill the group before reaping, including descendants that inherited pipes after the root exited.
    let _ = unsafe { libc::kill(-(child.id() as libc::pid_t), libc::SIGKILL) };
}

/// Verify termination, not disappearance. Linux zombies may remain visible until somebody else reaps them.
/// After reaping the root these probes never send another kill signal, since the group ID could be reused.
#[cfg(unix)]
pub(super) fn wait_group(id: u32) -> io::Result<()> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
    while group_has_live_members(id, deadline)? {
        check_deadline(deadline)?;
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    Ok(())
}

#[cfg(unix)]
fn check_deadline(deadline: std::time::Instant) -> io::Result<()> {
    if std::time::Instant::now() >= deadline {
        return Err(io::Error::other("command group cleanup timed out"));
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn group_has_live_members(id: u32, deadline: std::time::Instant) -> io::Result<bool> {
    linux_group::has_live_members(id, deadline)
}

#[cfg(target_os = "macos")]
fn group_has_live_members(id: u32, deadline: std::time::Instant) -> io::Result<bool> {
    mac_group::has_live_members(id, deadline)
}

#[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
fn group_has_live_members(id: u32, _: std::time::Instant) -> io::Result<bool> {
    // SAFETY: signal zero is a read-only fallback for Unix targets outside the supported macOS/Linux matrix.
    if unsafe { libc::kill(-(id as libc::pid_t), 0) } == 0 {
        return Ok(true);
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ESRCH) {
        Ok(false)
    } else {
        Err(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nonblocking_reads_distinguish_pending_eof_data_and_failure() {
        assert_eq!(read_result(Ok(0)).unwrap(), Some(0));
        assert_eq!(read_result(Ok(3)).unwrap(), Some(3));
        for kind in [io::ErrorKind::WouldBlock, io::ErrorKind::Interrupted] {
            assert_eq!(read_result(Err(kind.into())).unwrap(), None);
        }
        assert_eq!(
            read_result(Err(io::ErrorKind::BrokenPipe.into())).unwrap_err().kind(),
            io::ErrorKind::BrokenPipe
        );
    }

    #[cfg(unix)]
    #[test]
    fn cleanup_verification_times_out_without_signalling_an_existing_group() {
        // SAFETY: getpgrp reads our own group. wait_group only queries state and must not wait forever for us.
        let group = unsafe { libc::getpgrp() } as u32;
        assert!(wait_group(group).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn cleanup_ignores_an_exited_member_held_as_a_zombie() {
        use std::os::unix::process::CommandExt as _;
        struct Reap(Child);
        impl Drop for Reap {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        let mut child = Reap(
            std::process::Command::new("/usr/bin/true")
                .process_group(0)
                .spawn()
                .unwrap(),
        );
        let start = std::time::Instant::now();
        while !finished(&mut child.0).unwrap() {
            assert!(start.elapsed() < std::time::Duration::from_secs(2));
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        // SAFETY: signal zero probes the owned zombie's group without changing it. WNOWAIT has not reaped it.
        #[cfg(target_os = "linux")]
        assert_eq!(unsafe { libc::kill(-(child.0.id() as libc::pid_t), 0) }, 0);
        wait_group(child.0.id()).expect("a terminated zombie is not a live child");
    }

    #[cfg(unix)]
    #[test]
    fn an_invalid_pipe_fails_setup_without_starting_a_reader() {
        struct Invalid;
        impl std::os::fd::AsRawFd for Invalid {
            fn as_raw_fd(&self) -> std::os::fd::RawFd {
                -1
            }
        }
        impl Read for Invalid {
            fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
                unreachable!()
            }
        }
        assert!(prepare(&Invalid).is_err());
    }
}
