//! Repeated snapshots share one cleanup deadline, including members discovered after a previous snapshot.
use std::io;
use std::time::{Duration, Instant};

pub(super) fn run(
    deadline: Instant,
    mut snapshot: impl FnMut() -> io::Result<Vec<u32>>,
    mut stop: impl FnMut(u32) -> io::Result<()>,
) -> io::Result<()> {
    loop {
        time_left(deadline)?;
        let pids = snapshot()?;
        time_left(deadline)?;
        if pids.is_empty() {
            return Ok(());
        }
        for pid in pids {
            time_left(deadline)?;
            stop(pid)?;
        }
    }
}

pub(super) fn time_left(deadline: Instant) -> io::Result<Duration> {
    deadline
        .checked_duration_since(Instant::now())
        .ok_or_else(|| io::Error::other("command job cleanup timed out"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    #[test]
    fn every_snapshot_is_drained_including_descendants_found_after_the_first_stop() {
        let mut snapshots = VecDeque::from([vec![10], vec![20, 30], vec![]]);
        let mut stopped = Vec::new();
        run(
            Instant::now() + Duration::from_secs(5),
            || Ok(snapshots.pop_front().unwrap()),
            |pid| {
                stopped.push(pid);
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(stopped, [10, 20, 30]);
        assert!(snapshots.is_empty());
    }

    #[test]
    fn snapshot_and_stop_errors_propagate_without_processing_later_members() {
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut snapshots = VecDeque::from([Ok(vec![10]), Err(io::ErrorKind::PermissionDenied.into())]);
        let mut stopped = Vec::new();
        let result = run(
            deadline,
            || snapshots.pop_front().unwrap(),
            |pid| {
                stopped.push(pid);
                Ok(())
            },
        );
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(stopped, [10]);
        stopped.clear();
        let result = run(
            deadline,
            || Ok(vec![20, 30]),
            |pid| {
                stopped.push(pid);
                Err(io::ErrorKind::BrokenPipe.into())
            },
        );
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::BrokenPipe);
        assert_eq!(stopped, [20]);
    }

    #[test]
    fn the_same_deadline_applies_before_a_snapshot_and_after_a_slow_snapshot() {
        let expired = Instant::now() - Duration::from_secs(1);
        assert!(
            run(
                expired,
                || panic!("must not snapshot after the deadline"),
                |_| panic!("must not stop after the deadline")
            )
            .is_err()
        );
        let deadline = Instant::now() + Duration::from_millis(20);
        assert!(
            run(
                deadline,
                || {
                    std::thread::sleep(Duration::from_millis(25));
                    Ok(vec![])
                },
                |_| panic!("a slow snapshot must not extend the stop deadline"),
            )
            .is_err()
        );
        assert!(time_left(Instant::now() + Duration::from_secs(5)).unwrap() > Duration::ZERO);
    }
}
