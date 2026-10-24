//! Linux retains zombies in process groups. Probe membership first and inspect state only for our group.
use std::io;
use std::path::Path;
use std::time::Instant;

#[cfg(target_os = "linux")]
pub(super) fn has_live_members(group: u32, deadline: Instant) -> io::Result<bool> {
    live_in(Path::new("/proc"), group, deadline, &group_of)
}

fn live_in(
    root: &Path,
    group: u32,
    deadline: Instant,
    query: &impl Fn(i32) -> io::Result<Option<u32>>,
) -> io::Result<bool> {
    for entry in std::fs::read_dir(root)? {
        super::check_deadline(deadline)?;
        if member_is_live(entry?, group, query)? {
            return Ok(true);
        }
    }
    Ok(false)
}

fn member_is_live(
    entry: std::fs::DirEntry,
    group: u32,
    query: &impl Fn(i32) -> io::Result<Option<u32>>,
) -> io::Result<bool> {
    let name = entry.file_name();
    let Some(pid) = name.to_str().and_then(|s| s.parse::<i32>().ok()).filter(|pid| *pid > 0) else {
        return Ok(false);
    };
    // No unrelated process's stat file is read, including those hidden by procfs permissions.
    if query(pid)? != Some(group) {
        return Ok(false);
    }
    let Some(bytes) = read_record(&entry.path().join("stat"))? else {
        return Ok(false);
    };
    live_record(&String::from_utf8_lossy(&bytes), group)
}

fn group_of(pid: i32) -> io::Result<Option<u32>> {
    // SAFETY: getpgid is a read-only query. ESRCH is the ordinary race with an exiting process.
    let group = unsafe { libc::getpgid(pid) };
    if group == -1 {
        let error = io::Error::last_os_error();
        return if error.raw_os_error() == Some(libc::ESRCH) {
            Ok(None)
        } else {
            Err(error)
        };
    }
    Ok(Some(group as u32))
}

fn read_record(path: &Path) -> io::Result<Option<Vec<u8>>> {
    record_result(std::fs::read(path))
}

fn record_result(result: io::Result<Vec<u8>>) -> io::Result<Option<Vec<u8>>> {
    match result {
        Ok(bytes) => Ok(Some(bytes)),
        // procfs can report ESRCH from a descriptor opened before the task was reaped.
        Err(error) if error.kind() == io::ErrorKind::NotFound || error.raw_os_error() == Some(libc::ESRCH) => Ok(None),
        Err(error) => Err(error),
    }
}

fn live_record(stat: &str, group: u32) -> io::Result<bool> {
    // comm may contain spaces, newlines and ')'; the final ') ' precedes fixed ASCII fields.
    let (_, fields) = stat.rsplit_once(") ").ok_or_else(malformed)?;
    let mut fields = fields.split_whitespace();
    let state = fields.next().ok_or_else(malformed)?;
    let pgid = fields
        .nth(1)
        .ok_or_else(malformed)?
        .parse::<u32>()
        .map_err(|_| malformed())?;
    // Recheck the group, since the PID could have been reused between getpgid and this snapshot.
    Ok(pgid == group && !["Z", "X"].contains(&state))
}

fn malformed() -> io::Error {
    io::Error::other("invalid command process state")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_process_reaped_during_its_stat_read_is_absent_but_other_errors_fail_closed() {
        assert_eq!(record_result(Ok(b"stat".to_vec())).unwrap(), Some(b"stat".to_vec()));
        assert_eq!(record_result(Err(io::ErrorKind::NotFound.into())).unwrap(), None);
        assert_eq!(
            record_result(Err(io::Error::from_raw_os_error(libc::ESRCH))).unwrap(),
            None
        );
        for error in [
            io::Error::from_raw_os_error(libc::EIO),
            io::ErrorKind::PermissionDenied.into(),
            io::Error::other("fixture"),
        ] {
            assert!(record_result(Err(error)).is_err());
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn reading_an_open_stat_descriptor_after_reaping_reports_a_vanished_member() {
        use std::io::Read as _;
        let mut child = std::process::Command::new("/usr/bin/true").spawn().unwrap();
        let mut stat = std::fs::File::open(format!("/proc/{}/stat", child.id())).unwrap();
        child.wait().unwrap();
        let error = stat.read_to_end(&mut Vec::new()).unwrap_err();
        assert_eq!(error.raw_os_error(), Some(libc::ESRCH));
        assert_eq!(record_result(Err(error)).unwrap(), None);
    }

    #[test]
    fn process_state_separates_terminated_members_from_live_and_reused_pids() {
        for state in ["R", "S", "D", "T", "t", "I"] {
            assert!(live_record(&format!("71 (command ) with\nparens)) {state} 3 42 0 0"), 42).unwrap());
        }
        for state in ["Z", "X"] {
            assert!(!live_record(&format!("71 (command) {state} 3 42 0 0"), 42).unwrap());
        }
        assert!(!live_record("71 (command) S 3 43 0 0", 42).unwrap());
        for invalid in ["bad", "71 (command) ", "71 (command) S 3", "71 (command) S 3 nope"] {
            assert!(live_record(invalid, 42).is_err());
        }
    }

    #[test]
    fn proc_scan_ignores_zombies_vanished_and_unrelated_members_and_fails_closed() {
        let mut bytes = [0u8; 8];
        getrandom::fill(&mut bytes).unwrap();
        let dir = std::env::temp_dir().join(format!("codexbar-proc-{:x}", u64::from_le_bytes(bytes)));
        std::fs::create_dir_all(dir.join("71")).unwrap();
        std::fs::create_dir_all(dir.join("72")).unwrap();
        std::fs::create_dir_all(dir.join("self")).unwrap();
        let record = dir.join("71/stat");
        let deadline = || Instant::now() + std::time::Duration::from_secs(2);
        let query = |_: i32| Ok(Some(42));
        assert!(!live_in(&dir, 42, deadline(), &query).unwrap());
        std::fs::write(&record, "71 (command) Z 3 42 0").unwrap();
        assert!(!live_in(&dir, 42, deadline(), &query).unwrap());
        std::fs::write(&record, "71 (command) S 3 42 0").unwrap();
        assert!(live_in(&dir, 42, deadline(), &query).unwrap());
        assert!(!live_in(&dir, 42, deadline(), &|_| Ok(None)).unwrap());
        assert!(!live_in(&dir, 42, deadline(), &|_| Ok(Some(43))).unwrap());
        assert!(live_in(&dir, 42, deadline(), &|_| Err(io::Error::other("fixture"))).is_err());
        assert!(live_in(&dir, 42, Instant::now(), &query).is_err());
        std::fs::write(&record, "malformed").unwrap();
        assert!(live_in(&dir, 42, deadline(), &query).is_err());
        std::fs::remove_file(&record).unwrap();
        std::fs::create_dir(&record).unwrap();
        assert!(read_record(&record).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(live_in(&dir, 42, deadline(), &query).is_err());
    }

    #[test]
    fn group_query_observes_our_process_and_an_absent_pid_without_signalling() {
        // SAFETY: these queries read the current test process's group only.
        assert_eq!(
            group_of(std::process::id() as i32).unwrap(),
            Some(unsafe { libc::getpgrp() } as u32)
        );
        assert_eq!(group_of(i32::MAX).unwrap(), None);
    }
}
