//! Abrupt runner death is asynchronous job cleanup. Retain the live leaf before permitting that death.
use super::*;
use serde_json::{Value, json};
use std::os::windows::io::{AsRawHandle as _, FromRawHandle as _, OwnedHandle};
use std::os::windows::process::CommandExt as _;
use windows::Win32::Foundation::{FILETIME, GetLastError, WAIT_EVENT, WAIT_FAILED, WAIT_OBJECT_0, WAIT_TIMEOUT};
use windows::Win32::System::Threading::{
    GetCurrentProcess, GetProcessId, GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    PROCESS_SYNCHRONIZE, PROCESS_TERMINATE, TerminateProcess, WaitForSingleObject,
};

const SETUP: Duration = Duration::from_secs(5); // Existing fixture-start budget.
const CLEANUP: Duration = Duration::from_secs(3); // One existing budget, shared by all cleanup barriers.

fn creation(process: HANDLE) -> u64 {
    let (mut born, mut exit, mut kernel, mut user) = (
        FILETIME::default(),
        FILETIME::default(),
        FILETIME::default(),
        FILETIME::default(),
    );
    unsafe { GetProcessTimes(process, &mut born, &mut exit, &mut kernel, &mut user) }.unwrap();
    (u64::from(born.dwHighDateTime) << 32) | u64::from(born.dwLowDateTime)
}
fn publish(path: &Path, value: &Value) {
    let mut staging = path.as_os_str().to_owned();
    staging.push(".tmp");
    std::fs::write(&staging, value.to_string()).unwrap();
    std::fs::rename(staging, path).unwrap();
}
fn read(path: &Path) -> Value {
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

pub(super) fn register_leaf(marker: &Path) {
    // Published by the same still-running process that owns the marker lock, not inferred after its death.
    publish(
        &marker.with_extension("identity"),
        &json!({
            "pid": std::process::id(), "creation_time": creation(unsafe { GetCurrentProcess() }),
        }),
    );
}

struct Runner(std::process::Child);
impl Drop for Runner {
    fn drop(&mut self) {
        if !matches!(self.0.try_wait(), Ok(Some(_))) {
            // Failure cleanup addresses only our retained Child handle; never a PID search/global kill.
            if let Err(error) = self.0.kill() {
                eprintln!("owned abrupt fixture cleanup request failed: {error}");
            }
        }
    }
}
struct Leaf {
    process: OwnedHandle,
    pid: u32,
    born: u64,
}
impl Leaf {
    fn retain(identity: &Value) -> Self {
        let pid = u32::try_from(identity["pid"].as_u64().unwrap()).unwrap();
        let born = identity["creation_time"].as_u64().unwrap();
        let raw = unsafe {
            OpenProcess(
                PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE | PROCESS_TERMINATE,
                false,
                pid,
            )
        }
        .unwrap();
        let process = unsafe { OwnedHandle::from_raw_handle(raw.0) };
        let handle = HANDLE(process.as_raw_handle());
        assert!(born > 0, "leaf must publish a real creation timestamp");
        assert_eq!(unsafe { GetProcessId(handle) }, pid);
        assert_eq!(
            creation(handle),
            born,
            "ready PID must match the live leaf's creation identity"
        );
        // Do not install terminating failure cleanup until the published identity is verified.
        // A mismatched/reused PID above only closes the unverified handle; it never kills that process.
        let leaf = Self { process, pid, born };
        leaf.assert_live();
        leaf
    }
    fn handle(&self) -> HANDLE {
        HANDLE(self.process.as_raw_handle())
    }
    fn assert_live(&self) {
        assert_eq!(
            state(self.handle(), 0),
            WAIT_TIMEOUT,
            "the exact retained leaf must be alive before abrupt-exit ACK"
        );
    }
}
impl Drop for Leaf {
    fn drop(&mut self) {
        if unsafe { WaitForSingleObject(self.handle(), 0) } == WAIT_TIMEOUT {
            // This is failed-test cleanup after the assertion, never evidence that job cleanup passed.
            if let Err(error) = unsafe { TerminateProcess(self.handle(), 1) } {
                eprintln!("owned abrupt leaf cleanup request failed: {error}");
            }
        }
    }
}
fn state(process: HANDLE, milliseconds: u32) -> WAIT_EVENT {
    let state = unsafe { WaitForSingleObject(process, milliseconds) };
    if state == WAIT_FAILED {
        panic!("retained process wait failed: {}", unsafe { GetLastError() }.0);
    }
    state
}
fn wait(process: HANDLE, deadline: Instant, description: &str) {
    let remaining = deadline
        .checked_duration_since(Instant::now())
        .expect("shared abrupt-parent cleanup budget expired");
    let milliseconds = u32::try_from(remaining.as_millis()).unwrap();
    assert_eq!(
        state(process, milliseconds),
        WAIT_OBJECT_0,
        "{description} did not signal within the shared three-second budget"
    );
    assert!(
        Instant::now() <= deadline,
        "shared abrupt-parent cleanup budget exceeded"
    );
}

pub(super) fn qualify(program: &str, marker: &Path) {
    let mut runner = Runner(
        Command::new(program)
            .args(["runner-exit", marker.to_str().unwrap()])
            .creation_flags(0x0800_0000) // Synthetic runner only. The actual provider launch is unchanged.
            .spawn()
            .unwrap(),
    );
    let runner_handle = HANDLE(runner.0.as_raw_handle());
    let runner_born = creation(runner_handle);
    let setup = Instant::now();
    while !marker.with_extension("identity").is_file() {
        assert!(
            setup.elapsed() < SETUP,
            "leaf identity was not published before setup deadline"
        );
        assert!(
            runner.0.try_wait().unwrap().is_none(),
            "runner exited before retaining its live leaf"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    let identity = read(&marker.with_extension("identity"));
    let leaf = Leaf::retain(&identity);
    let ready_pid: u32 = std::fs::read_to_string(marker.with_extension("ready"))
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(ready_pid, leaf.pid);
    let resource = File::open(marker).unwrap();
    assert!(
        matches!(resource.try_lock(), Err(std::fs::TryLockError::WouldBlock)),
        "leaf must own its resource before ACK"
    );
    drop(resource);
    leaf.assert_live();
    assert_eq!(creation(runner_handle), runner_born);
    assert_eq!(state(runner_handle, 0), WAIT_TIMEOUT);
    println!(
        "abrupt-parent live identities before ACK: {}",
        json!({
            "runner_pid": runner.0.id(), "runner_creation_time": runner_born,
            "leaf_pid": leaf.pid, "leaf_creation_time": leaf.born, "retained_leaf_state": "WAIT_TIMEOUT",
        })
    );
    // Budget starts before ACK, so runner exit, leaf termination and resource cleanup share one 3s bound.
    let cleanup_started = Instant::now();
    let deadline = cleanup_started + CLEANUP;
    publish(&marker.with_extension("exit-ack"), &identity);
    wait(runner_handle, deadline, "abrupt runner");
    assert!(runner.0.wait().unwrap().success());
    wait(leaf.handle(), deadline, "owned leaf after kill-on-close job closure");
    assert_eq!(creation(leaf.handle()), leaf.born);
    // Resource release is checked after actual process termination. It is never the liveness barrier.
    let mut resource = File::open(marker).unwrap();
    loop {
        match resource.try_lock() {
            Ok(()) => break,
            Err(std::fs::TryLockError::WouldBlock) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(5))
            }
            Err(error) => {
                panic!("terminated leaf did not release its resource within the same cleanup budget: {error}")
            }
        }
    }
    assert!(Instant::now() <= deadline);
    let mut text = String::new();
    resource.read_to_string(&mut text).unwrap();
    assert_eq!(text, "ready");
    println!(
        "abrupt-parent retained cleanup: {}",
        json!({
            "runner_pid": runner.0.id(), "runner_creation_time": runner_born,
            "leaf_pid": leaf.pid, "leaf_creation_time": leaf.born, "retained_exit_state": "WAIT_OBJECT_0",
            "elapsed_us": cleanup_started.elapsed().as_micros(), "shared_budget_ms": CLEANUP.as_millis(),
            "resource_released": true,
        })
    );
}

pub(super) fn exit_with_owned_tree(marker: &Path) -> ! {
    let runner_marker = marker.to_owned();
    std::thread::spawn(move || {
        let executable = std::env::current_exe().unwrap();
        let _ = SystemCommandRunner.run(
            executable.to_str().unwrap(),
            &["descendant", runner_marker.to_str().unwrap(), "abrupt-exit"],
            Duration::from_secs(60),
        );
    });
    let start = Instant::now();
    while !marker.with_extension("identity").is_file() {
        assert!(start.elapsed() < SETUP);
        std::thread::sleep(Duration::from_millis(5));
    }
    let identity = read(&marker.with_extension("identity"));
    while !marker.with_extension("exit-ack").is_file() {
        assert!(
            start.elapsed() < SETUP,
            "runner may not die until outer test retains and ACKs the leaf"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(read(&marker.with_extension("exit-ack")), identity);
    // No destructors run: Windows closing the production runner's job is responsible for the live leaf.
    std::process::exit(0)
}
