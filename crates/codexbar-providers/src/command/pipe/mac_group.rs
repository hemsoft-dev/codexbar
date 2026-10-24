//! Read-only process-state queries for the owned macOS process group. Exited zombies are not live members.
use std::io;
use std::time::Instant;

pub(super) fn has_live_members(group: u32, deadline: Instant) -> io::Result<bool> {
    for pid in group_pids(group)? {
        super::check_deadline(deadline)?;
        if pid > 0 && member_is_live(pid, group)? {
            return Ok(true);
        }
    }
    Ok(false)
}

fn group_pids(group: u32) -> io::Result<Vec<libc::pid_t>> {
    // Bound the snapshot; an exactly full buffer cannot prove that later members weren't truncated.
    let mut pids = vec![0; 4096];
    // SAFETY: errno is thread-local. The output buffer holds exactly the number of pid_t entries advertised.
    let count = unsafe {
        *libc::__error() = 0;
        libc::proc_listpgrppids(
            group as libc::pid_t,
            pids.as_mut_ptr().cast(),
            std::mem::size_of_val(&pids[..]) as i32,
        )
    };
    if count == 0 {
        return disappeared().map(|_| Vec::new());
    }
    if count < 0 || count as usize >= pids.len() {
        return Err(io::Error::other(
            "command process snapshot failed or exceeded its bound",
        ));
    }
    pids.truncate(count as usize);
    Ok(pids)
}

fn member_is_live(pid: libc::pid_t, group: u32) -> io::Result<bool> {
    // SAFETY: proc_bsdinfo is a C output structure; all-zero is a valid initialization.
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of_val(&info) as i32;
    // SAFETY: the PID query is read-only and the output buffer has the exact PROC_PIDTBSDINFO size.
    let count = unsafe {
        *libc::__error() = 0;
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTBSDINFO,
            0,
            std::ptr::from_mut(&mut info).cast(),
            size,
        )
    };
    if count == 0 {
        return disappeared().map(|_| false);
    }
    if count != size {
        return Err(io::Error::other("incomplete command process state"));
    }
    Ok(info.pbi_pgid == group && info.pbi_status != libc::SZOMB)
}

fn disappeared() -> io::Result<()> {
    let error = io::Error::last_os_error();
    match error.raw_os_error() {
        Some(0 | libc::ESRCH) => Ok(()),
        _ => Err(error),
    }
}
