//! Windows usage queries get a console hidden at creation, which ordinary descendants inherit.
//! This is not a sandbox against a process explicitly requesting another console or showing UI.
use std::fs::File;
use std::io;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle};
use std::os::windows::process::ExitStatusExt as _;
use std::process::ExitStatus;

use windows::Win32::Foundation::{HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT};
use windows::Win32::System::Threading::{
    CREATE_NEW_CONSOLE, CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, CreateProcessW, EXTENDED_STARTUPINFO_PRESENT,
    GetExitCodeProcess, PROCESS_INFORMATION, ResumeThread, TerminateProcess, WaitForSingleObject,
};
use windows::core::{PCWSTR, PWSTR};

mod arguments;
mod stdio;

#[cfg(test)]
mod tests;

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
    let mut plan = arguments::command_line(program, args)?;
    let mut streams = stdio::Prepared::new()?;
    let suspended = Suspended::create(&mut plan, &mut streams)?;
    let (stdout, stderr) = streams.capture();
    suspended.start(stdout, stderr)
}

/// Own the exact process and primary thread from suspended creation until job assignment and resume commit.
/// Dropping an uncommitted launch always terminates/reaps that same retained process.
struct Suspended {
    child: Option<Child>,
    thread: OwnedHandle,
}
impl Suspended {
    fn create(plan: &mut super::windows_plan::LaunchPlan, streams: &mut stdio::Prepared) -> io::Result<Self> {
        let startup = streams.startup();
        let mut info = PROCESS_INFORMATION::default();
        // SAFETY: owned NUL-terminated application and writable command line, initialized live startup attributes,
        // and writable output. Preserve the caller's token, environment, working directory and desktop.
        unsafe {
            CreateProcessW(
                PCWSTR(plan.application.as_ptr()),
                Some(PWSTR(plan.line.as_mut_ptr())),
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
        // SAFETY: successful CreateProcessW transfers unique noninheritable process and primary-thread handles.
        let process = unsafe { OwnedHandle::from_raw_handle(info.hProcess.0) };
        let thread = unsafe { OwnedHandle::from_raw_handle(info.hThread.0) };
        Ok(Self {
            child: Some(Child {
                process,
                stdout: None,
                stderr: None,
            }),
            thread,
        })
    }

    fn start(mut self, stdout: File, stderr: File) -> io::Result<Contained> {
        let child = self.child.as_mut().unwrap();
        child.stdout = Some(stdout);
        child.stderr = Some(stderr);
        let job = crate::contained::job::Job::kill_on_close(child)
            .ok_or_else(|| io::Error::other("the background process couldn't enter its job"))?;
        // SAFETY: retain this suspended primary thread; no provider code ran before job membership.
        if unsafe { ResumeThread(HANDLE(self.thread.as_raw_handle())) } == u32::MAX {
            return Err(io::Error::other("the background process couldn't be resumed"));
        }
        Ok(Contained {
            child: self.child.take().unwrap(),
            job,
        })
    }
}
impl Drop for Suspended {
    fn drop(&mut self) {
        if let Some(child) = self.child.as_mut() {
            child.kill();
            let _ = child.wait(); // bounded confirmation, including job-assignment/resume failures.
        }
    }
}
