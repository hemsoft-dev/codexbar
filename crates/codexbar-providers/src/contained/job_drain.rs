//! A job's zero active count is not a process-termination barrier. Retain verified members and wait on handles.
use std::io;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{CloseHandle, ERROR_INVALID_PARAMETER, HANDLE, WAIT_OBJECT_0};
use windows::Win32::System::JobObjects::{IsProcessInJob, JobObjectBasicProcessIdList, QueryInformationJobObject};
use windows::Win32::System::Threading::{
    OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE, TerminateProcess,
    WaitForSingleObject,
};

pub(super) fn terminate(job: HANDLE) -> io::Result<()> {
    let deadline = Instant::now() + Duration::from_secs(1);
    super::drain::run(deadline, || members(job), |pid| stop_member(job, pid, deadline))
}

fn stop_member(job: HANDLE, pid: u32, deadline: Instant) -> io::Result<()> {
    if let Some(member) = Member::open(job, pid)? {
        member.stop(deadline)?;
    }
    Ok(())
}

#[repr(C)]
struct ProcessIds {
    assigned: u32,
    count: u32,
    ids: [usize; 4096],
}

fn members(job: HANDLE) -> io::Result<Vec<u32>> {
    // Layout of JOBOBJECT_BASIC_PROCESS_ID_LIST, with storage for a bounded variable-length PID array.
    let mut info = Box::new(ProcessIds {
        assigned: 0,
        count: 0,
        ids: [0; 4096],
    });
    // SAFETY: valid job, C-layout output header and PID array, and the exact allocated byte size.
    unsafe {
        QueryInformationJobObject(
            Some(job),
            JobObjectBasicProcessIdList,
            std::ptr::from_mut(info.as_mut()).cast(),
            std::mem::size_of::<ProcessIds>() as u32,
            None,
        )
    }
    .map_err(io::Error::other)?;
    if info.count as usize > info.ids.len() || info.assigned != info.count {
        return Err(io::Error::other("command job process list exceeded its bound"));
    }
    Ok(info.ids[..info.count as usize].iter().map(|pid| *pid as u32).collect())
}

struct Member(HANDLE);
impl Member {
    fn open(job: HANDLE, pid: u32) -> io::Result<Option<Self>> {
        // SAFETY: retain this exact process identity before checking membership or requesting termination.
        let handle = unsafe {
            OpenProcess(
                PROCESS_SYNCHRONIZE | PROCESS_TERMINATE | PROCESS_QUERY_LIMITED_INFORMATION,
                false,
                pid,
            )
        };
        let member = match handle {
            Ok(handle) => Self(handle),
            Err(error) if error.code() == ERROR_INVALID_PARAMETER.to_hresult() => return Ok(None),
            Err(error) => return Err(io::Error::other(error)),
        };
        let mut inside = windows::core::BOOL::default();
        // SAFETY: retained process handle, owned job, and a valid writable BOOL. Reject a reused PID outside this job.
        unsafe { IsProcessInJob(member.0, Some(job), &mut inside) }.map_err(io::Error::other)?;
        Ok(inside.as_bool().then_some(member))
    }

    fn stop(&self, deadline: Instant) -> io::Result<()> {
        let milliseconds = remaining(deadline)?;
        // SAFETY: the retained handle was verified in this job and grants terminate/synchronize rights.
        // Termination may fail if it already exited; the wait proves the result independently.
        let _ = unsafe { TerminateProcess(self.0, 1) };
        let state = unsafe { WaitForSingleObject(self.0, milliseconds) };
        if state != WAIT_OBJECT_0 {
            return Err(io::Error::other("command process termination could not be confirmed"));
        }
        Ok(())
    }
}

impl Drop for Member {
    fn drop(&mut self) {
        // SAFETY: this handle came from OpenProcess and is closed exactly once.
        let _ = unsafe { CloseHandle(self.0) };
    }
}

fn remaining(deadline: Instant) -> io::Result<u32> {
    let left = super::drain::time_left(deadline)?;
    Ok((left.as_millis() + 1).min(u128::from(u32::MAX)) as u32)
}
