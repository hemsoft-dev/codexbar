//! An unrelated inheritable writer must not enter a live production-launched provider.
use super::*;
use std::os::windows::io::{AsRawHandle as _, FromRawHandle as _, OwnedHandle};
use windows::Win32::Foundation::{ERROR_BROKEN_PIPE, HANDLE, HANDLE_FLAG_INHERIT, SetHandleInformation, WAIT_TIMEOUT};
use windows::Win32::System::Pipes::{CreatePipe, PeekNamedPipe};
use windows::Win32::System::Threading::{OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject};

pub(super) fn qualify() {
    let mut read = HANDLE::default();
    let mut write = HANDLE::default();
    // SAFETY: writable handle outputs; neither endpoint initially inheritable.
    unsafe { CreatePipe(&mut read, &mut write, None, 0) }.unwrap();
    let read = unsafe { File::from_raw_handle(read.0) };
    let write = unsafe { File::from_raw_handle(write.0) };
    unsafe {
        SetHandleInformation(
            HANDLE(write.as_raw_handle()),
            HANDLE_FLAG_INHERIT.0,
            HANDLE_FLAG_INHERIT,
        )
    }
    .unwrap();
    let scratch = Scratch::new();
    let marker = scratch.marker("unrelated-pipe-sentinel");
    std::thread::scope(|scope| {
        let marker = &marker;
        let running = scope.spawn(move || {
            let executable = std::env::current_exe().unwrap();
            SystemCommandRunner.run(
                executable.to_str().unwrap(),
                &["hang", marker.to_str().unwrap()],
                Duration::from_secs(3),
            )
        });
        let started = Instant::now();
        while !marker.with_extension("ready").exists() {
            assert!(started.elapsed() < Duration::from_secs(2));
            std::thread::sleep(Duration::from_millis(5));
        }
        // The provider is alive and holding its lock. Close the sole intended writer while it remains alive.
        let pid = std::fs::read_to_string(marker.with_extension("ready"))
            .unwrap()
            .parse()
            .unwrap();
        let process = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, false, pid) }.unwrap();
        let process = unsafe { OwnedHandle::from_raw_handle(process.0) };
        assert_eq!(
            unsafe { WaitForSingleObject(HANDLE(process.as_raw_handle()), 0) },
            WAIT_TIMEOUT
        );
        let held = File::open(marker).unwrap();
        assert!(matches!(held.try_lock(), Err(std::fs::TryLockError::WouldBlock)));
        drop(write);
        // If the unrelated writer leaked into that live provider, this returns Ok with no bytes instead of EOF.
        let error = unsafe { PeekNamedPipe(HANDLE(read.as_raw_handle()), None, 0, None, None, None) }.unwrap_err();
        assert_eq!(
            error.code(),
            ERROR_BROKEN_PIPE.to_hresult(),
            "unrelated inheritable writer entered live provider"
        );
        assert_eq!(
            unsafe { WaitForSingleObject(HANDLE(process.as_raw_handle()), 0) },
            WAIT_TIMEOUT,
            "sentinel EOF must be verified while the retained provider process is alive"
        );
        assert!(
            matches!(held.try_lock(), Err(std::fs::TryLockError::WouldBlock)),
            "EOF must be proved before provider exit"
        );
        assert_eq!(running.join().unwrap(), Err(CommandError::TimedOut));
    });
    released(&marker);
    println!("unrelated inheritable pipe reached EOF while provider was still alive");
}
