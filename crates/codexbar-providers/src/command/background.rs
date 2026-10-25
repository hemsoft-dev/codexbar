//! Windows usage queries get a console hidden at creation, which ordinary descendants inherit.
//! This is not a sandbox against a process explicitly requesting another console or showing UI.
use std::fs::{File, OpenOptions};
use std::io;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle};
use std::os::windows::process::ExitStatusExt as _;
use std::process::ExitStatus;

use windows::Win32::Foundation::{HANDLE, HANDLE_FLAG_INHERIT, SetHandleInformation, WAIT_OBJECT_0, WAIT_TIMEOUT};
use windows::Win32::Security::SECURITY_ATTRIBUTES;
use windows::Win32::System::Pipes::CreatePipe;
use windows::Win32::System::Threading::{
    CREATE_NEW_CONSOLE, CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, CreateProcessW, DeleteProcThreadAttributeList,
    EXTENDED_STARTUPINFO_PRESENT, GetExitCodeProcess, InitializeProcThreadAttributeList, LPPROC_THREAD_ATTRIBUTE_LIST,
    PROC_THREAD_ATTRIBUTE_HANDLE_LIST, PROCESS_INFORMATION, ResumeThread, STARTF_USESHOWWINDOW, STARTF_USESTDHANDLES,
    STARTUPINFOEXW, TerminateProcess, UpdateProcThreadAttribute, WaitForSingleObject,
};
use windows::core::{PCWSTR, PWSTR};

mod arguments;

pub(super) struct Child {
    process: OwnedHandle,
    pub stdout: Option<File>,
    pub stderr: Option<File>,
}

impl AsRawHandle for Child {
    fn as_raw_handle(&self) -> RawHandle {
        self.process.as_raw_handle()
    }
}

impl Child {
    pub(super) fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        // SAFETY: a retained owned process handle; no blocking wait or PID lookup.
        match unsafe { WaitForSingleObject(HANDLE(self.as_raw_handle()), 0) } {
            WAIT_OBJECT_0 => self.wait().map(Some),
            WAIT_TIMEOUT => Ok(None),
            _ => Err(io::Error::last_os_error()),
        }
    }

    pub(super) fn wait(&mut self) -> io::Result<ExitStatus> {
        self.wait_for(1000)
    }

    fn wait_for(&mut self, milliseconds: u32) -> io::Result<ExitStatus> {
        let mut code = 0;
        // SAFETY: this process was created by this launcher and its handle stays owned during both queries.
        unsafe {
            if WaitForSingleObject(HANDLE(self.as_raw_handle()), milliseconds) != WAIT_OBJECT_0 {
                return Err(io::Error::other("background process exit could not be confirmed"));
            }
            GetExitCodeProcess(HANDLE(self.as_raw_handle()), &mut code).map_err(io::Error::other)?;
        }
        Ok(ExitStatus::from_raw(code))
    }

    fn kill(&self) {
        // SAFETY: terminate only our retained process. A failed kill of an exited process is harmless.
        let _ = unsafe { TerminateProcess(HANDLE(self.as_raw_handle()), 1) };
    }
}

pub(super) struct Contained {
    pub child: Child,
    job: crate::contained::job::Job,
}

impl Contained {
    pub(super) fn stop(&mut self) -> io::Result<ExitStatus> {
        let cleanup = self.job.terminate();
        self.child.kill();
        // The job drain already spent the one-second cleanup budget and verified retained members.
        // Never extend it with an unbounded root wait after a failed drain or kill.
        let status = self.child.wait_for(0);
        cleanup?;
        status
    }
}

pub(super) fn spawn(program: &str, args: &[&str]) -> io::Result<Contained> {
    let (application, mut line) = arguments::command_line(program, args)?;
    // Cooperate with the legacy contained/login/browser launchers, not just other native usage queries.
    // Declare before the handles so every error path closes them before releasing this guard.
    let guard = crate::platform::spawn_guard();
    let input = OpenOptions::new().read(true).open("NUL")?;
    let (stdout, out) = pipe()?;
    let (stderr, err) = pipe()?;
    let handles = [raw(&input), raw(&out), raw(&err)];
    for handle in handles {
        // SAFETY: only these newly owned child-side handles become inheritable. Parent read ends never do.
        unsafe { SetHandleInformation(handle, HANDLE_FLAG_INHERIT.0, HANDLE_FLAG_INHERIT) }
            .map_err(io::Error::other)?;
    }
    let mut attributes = Attributes::handles(&handles)?;
    let mut startup = STARTUPINFOEXW::default();
    startup.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
    startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES | STARTF_USESHOWWINDOW;
    startup.StartupInfo.wShowWindow = 0; // SW_HIDE, applied by Windows at console creation, never ShowWindow.
    startup.StartupInfo.hStdInput = handles[0];
    startup.StartupInfo.hStdOutput = handles[1];
    startup.StartupInfo.hStdError = handles[2];
    startup.lpAttributeList = attributes.pointer();
    let mut info = PROCESS_INFORMATION::default();
    // SAFETY: NUL-terminated application and writable command line; startup attributes and the three handles
    // outlive the call. Inherit the existing token, environment, working directory and desktop unchanged.
    unsafe {
        CreateProcessW(
            PCWSTR(application.as_ptr()),
            Some(PWSTR(line.as_mut_ptr())),
            None,
            None,
            true,
            CREATE_NEW_CONSOLE | CREATE_SUSPENDED | CREATE_UNICODE_ENVIRONMENT | EXTENDED_STARTUPINFO_PRESENT,
            None,
            PCWSTR::null(),
            &startup.StartupInfo,
            &mut info,
        )
    }
    .map_err(|error| io::Error::from_raw_os_error(error.code().0 & 0xffff))?;
    // SAFETY: successful CreateProcessW transfers these two unique, noninheritable handles to us.
    let process = unsafe { OwnedHandle::from_raw_handle(info.hProcess.0) };
    let thread = unsafe { OwnedHandle::from_raw_handle(info.hThread.0) };
    drop((input, out, err, attributes));
    drop(guard);
    let mut child = Child {
        process,
        stdout: Some(stdout),
        stderr: Some(stderr),
    };
    let Some(job) = crate::contained::job::Job::kill_on_close(&child) else {
        return Err(failed_start(&mut child));
    };
    // SAFETY: the sole primary thread is still suspended; attach the job before running any provider code.
    if unsafe { ResumeThread(HANDLE(thread.as_raw_handle())) } == u32::MAX {
        return Err(failed_start(&mut child));
    }
    Ok(Contained { child, job })
}

fn failed_start(child: &mut Child) -> io::Error {
    child.kill();
    let _ = child.wait();
    io::Error::other("the background process couldn't be started")
}

fn raw(file: &File) -> HANDLE {
    HANDLE(file.as_raw_handle())
}

fn pipe() -> io::Result<(File, File)> {
    let mut read = HANDLE::default();
    let mut write = HANDLE::default();
    let security = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        ..Default::default()
    };
    // SAFETY: writable output handles; default security and noninheritable until explicitly selected above.
    unsafe { CreatePipe(&mut read, &mut write, Some(&security), 0) }.map_err(io::Error::other)?;
    // SAFETY: successful CreatePipe returned distinct owned file handles, each closed once by File.
    Ok(unsafe { (File::from_raw_handle(read.0), File::from_raw_handle(write.0)) })
}

/// Aligned native attribute storage. The handle-list value must outlive this object and CreateProcessW.
struct Attributes(Vec<usize>);
impl Attributes {
    fn handles(handles: &[HANDLE]) -> io::Result<Self> {
        let mut bytes = 0;
        // SAFETY: documented sizing query. Failure with the required byte count is expected.
        let _ = unsafe { InitializeProcThreadAttributeList(None, 1, None, &mut bytes) };
        if bytes == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut storage = vec![0; bytes.div_ceil(std::mem::size_of::<usize>())];
        let pointer = LPPROC_THREAD_ATTRIBUTE_LIST(storage.as_mut_ptr().cast());
        // SAFETY: sufficiently sized and aligned writable storage. Only construct the RAII owner on success.
        unsafe { InitializeProcThreadAttributeList(Some(pointer), 1, None, &mut bytes) }.map_err(io::Error::other)?;
        let mut attributes = Self(storage);
        // SAFETY: initialized list and a live exact-size array of inheritable handles, no pseudo handles.
        unsafe {
            UpdateProcThreadAttribute(
                attributes.pointer(),
                0,
                PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
                Some(handles.as_ptr().cast()),
                std::mem::size_of_val(handles),
                None,
                None,
            )
        }
        .map_err(io::Error::other)?;
        Ok(attributes)
    }

    fn pointer(&mut self) -> LPPROC_THREAD_ATTRIBUTE_LIST {
        LPPROC_THREAD_ATTRIBUTE_LIST(self.0.as_mut_ptr().cast())
    }
}
impl Drop for Attributes {
    fn drop(&mut self) {
        // SAFETY: the initialized list is deleted before its backing allocation is freed.
        unsafe { DeleteProcThreadAttributeList(self.pointer()) };
    }
}
