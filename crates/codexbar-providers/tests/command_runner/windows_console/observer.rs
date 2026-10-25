//! Inert hosted qualification only. No vendor, account, global window contents or external broker probing.
//! A prearmed WinEvent message pump and independent enumeration cover owned fixture windows.
//! Retained process intervals qualify temporal coverage. Outside-broker ownership remains unknown.
use super::*;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::os::windows::io::{AsRawHandle as _, FromRawHandle as _, OwnedHandle};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering},
    mpsc,
};
use windows::Win32::Foundation::{
    FILETIME, GetHandleInformation, GetLastError, RECT, SetLastError, WAIT_OBJECT_0, WAIT_TIMEOUT, WIN32_ERROR,
};
use windows::Win32::System::Console::{
    CONSOLE_MODE, GetConsoleMode, GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation, SetInformationJobObject,
};
use windows::Win32::System::StationsAndDesktops::{
    CloseDesktop, DESKTOP_CONTROL_FLAGS, DESKTOP_READOBJECTS, OpenInputDesktop,
};
use windows::Win32::System::SystemInformation::GetSystemTimeAsFileTime;
use windows::Win32::System::Threading::{
    GetCurrentProcess, GetProcessIdOfThread, GetProcessTimes, OpenProcess, OpenThread,
    PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE, THREAD_QUERY_LIMITED_INFORMATION, WaitForSingleObject,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DestroyWindow, DispatchMessageW, EVENT_OBJECT_CREATE, EVENT_OBJECT_DESTROY, EVENT_OBJECT_HIDE,
    EVENT_OBJECT_SHOW, GA_ROOT, GetAncestor, GetWindowRect, MSG, PM_REMOVE, PeekMessageW, SW_SHOWNOACTIVATE,
    ShowWindow, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_POPUP,
};
use windows::core::{PCWSTR, w};

pub(super) const DIRECTORY_ENV: &str = "CODEXBAR_SYNTHETIC_OBSERVER_DIRECTORY";
const CAP: usize = 8192;
const SAMPLE_CAP: usize = 512;
const READY: Duration = Duration::from_secs(2);
const CONTROL: Duration = Duration::from_millis(800);
const POLL: Duration = Duration::from_millis(5);

type Result<T> = std::result::Result<T, String>;
// Fixture metadata only, avoiding new Cargo features/dependencies.
#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetFileType(handle: HANDLE) -> u32;
    fn ProcessIdToSessionId(pid: u32, session: *mut u32) -> BOOL;
}
fn session(pid: u32) -> Result<u32> {
    let mut session = 0;
    if !unsafe { ProcessIdToSessionId(pid, &mut session) }.as_bool() {
        return Err(format!("ProcessIdToSessionId: {}", unsafe { GetLastError() }.0));
    }
    Ok(session)
}
fn failure(api: &str, error: windows::core::Error) -> String {
    let last_error = unsafe { GetLastError() }.0;
    format!("{api}: HRESULT {}, immediate GetLastError {last_error}", error.code().0)
}
fn ticks(time: FILETIME) -> u64 {
    (u64::from(time.dwHighDateTime) << 32) | u64::from(time.dwLowDateTime)
}
fn creation(handle: HANDLE) -> Result<u64> {
    Ok(process_times(handle)?.0)
}
fn process_times(handle: HANDLE) -> Result<(u64, u64)> {
    let (mut born, mut exit, mut kernel, mut user) = (
        FILETIME::default(),
        FILETIME::default(),
        FILETIME::default(),
        FILETIME::default(),
    );
    unsafe { GetProcessTimes(handle, &mut born, &mut exit, &mut kernel, &mut user) }
        .map_err(|e| failure("GetProcessTimes", e))?;
    Ok((ticks(born), ticks(exit)))
}
fn open(pid: u32) -> Result<OwnedHandle> {
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE, false, pid) }
        .map_err(|e| failure("OpenProcess owned fixture", e))?;
    Ok(unsafe { OwnedHandle::from_raw_handle(handle.0) })
}
fn name(handle: HANDLE) -> Result<String> {
    let mut buffer = [0u16; 256];
    unsafe {
        GetUserObjectInformationW(
            handle,
            UOI_NAME,
            Some(buffer.as_mut_ptr().cast()),
            std::mem::size_of_val(&buffer) as u32,
            None,
        )
    }
    .map_err(|e| failure("UOI_NAME", e))?;
    let end = buffer.iter().position(|c| *c == 0).ok_or("unterminated desktop name")?;
    String::from_utf16(&buffer[..end]).map_err(|e| e.to_string())
}
fn thread_desktop(thread: u32) -> Result<String> {
    let desktop = unsafe { GetThreadDesktop(thread) }.map_err(|e| failure("GetThreadDesktop", e))?;
    name(HANDLE(desktop.0))
}
fn input_desktop() -> Result<String> {
    let desktop = unsafe { OpenInputDesktop(DESKTOP_CONTROL_FLAGS(0), false, DESKTOP_READOBJECTS) }
        .map_err(|e| failure("OpenInputDesktop read-only", e))?;
    let result = name(HANDLE(desktop.0));
    unsafe { CloseDesktop(desktop) }.map_err(|e| failure("CloseDesktop", e))?;
    result
}
fn atomic_json(path: &Path, value: &Value) -> Result<()> {
    let staging = path.with_extension("tmp");
    std::fs::write(&staging, value.to_string()).map_err(|e| e.to_string())?;
    std::fs::rename(staging, path).map_err(|e| e.to_string())
}

pub(super) fn stdio() -> Value {
    let mut rows = Vec::new();
    for (role, selector) in [
        ("stdin", STD_INPUT_HANDLE),
        ("stdout", STD_OUTPUT_HANDLE),
        ("stderr", STD_ERROR_HANDLE),
    ] {
        let row = match unsafe { GetStdHandle(selector) } {
            Err(e) => json!({"role": role, "get_std_handle_error": e.code().0}),
            Ok(handle) => {
                unsafe { SetLastError(WIN32_ERROR(0)) };
                let file_type = unsafe { GetFileType(handle) };
                let file_type_error = if file_type == 0 {
                    Some(unsafe { GetLastError() }.0)
                } else {
                    None
                };
                let mut flags = 0;
                let flags_error = unsafe { GetHandleInformation(handle, &mut flags) }
                    .err()
                    .map(|e| e.code().0);
                let mut mode = CONSOLE_MODE::default();
                let mode_error = unsafe { GetConsoleMode(handle, &mut mode) }.err().map(|e| e.code().0);
                json!({"role": role, "handle": handle.0 as usize, "handle_flags": flags,
                    "handle_information_error": flags_error, "console_mode": mode.0, "console_mode_error": mode_error,
                    "file_type": file_type, "file_type_error": file_type_error,
                    "expected_provider_contract": if role == "stdin" { "NUL" } else { "separate captured pipe" }})
            }
        };
        rows.push(row);
    }
    json!(rows)
}
fn console_sample() -> Value {
    // Capture last-error immediately after the failing API, before any subsequent OS call.
    let mut members = [0; 16];
    let count = unsafe { GetConsoleProcessList(&mut members) } as usize;
    let error = if count == 0 {
        Some(unsafe { GetLastError() }.0)
    } else {
        None
    };
    let hwnd = unsafe { GetConsoleWindow() };
    let mut owner = 0;
    let tid = unsafe { GetWindowThreadProcessId(hwnd, Some(&mut owner)) };
    json!({"timestamp_100ns": ticks(unsafe { GetSystemTimeAsFileTime() }), "hwnd": hwnd.0 as usize, "console_process_list_return": count, "console_process_list_error": error,
        "members": if count > 0 && count <= members.len() { Some(&members[..count]) } else { None },
        "members_truncated": count > members.len(), "owner": owner, "thread": tid,
        "valid": unsafe { IsWindow(Some(hwnd)) }.as_bool(), "visible": unsafe { IsWindowVisible(hwnd) }.as_bool()})
}

pub(super) fn own_experiment_job() -> Result<()> {
    // Only this newly invoked synthetic fixture joins this job. No existing process is assigned/killed.
    let raw =
        unsafe { CreateJobObjectW(None, PCWSTR::null()) }.map_err(|e| failure("experiment CreateJobObjectW", e))?;
    let job = unsafe { OwnedHandle::from_raw_handle(raw.0) };
    let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
    limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    unsafe {
        SetInformationJobObject(
            raw,
            JobObjectExtendedLimitInformation,
            std::ptr::from_ref(&limits).cast(),
            std::mem::size_of_val(&limits) as u32,
        )
        .map_err(|e| failure("experiment SetInformationJobObject", e))?;
        AssignProcessToJobObject(raw, GetCurrentProcess())
            .map_err(|e| failure("experiment AssignProcessToJobObject", e))?;
    }
    // Deliberately process-lifetime ownership: dropping a job containing ourselves would terminate us
    // before the report is flushed. Normal exit or the script's retained Popen kill closes this handle.
    std::mem::forget(job);
    println!(
        "{}",
        json!({"result": "experiment-containment", "owner_pid": std::process::id(), "kill_on_close": true})
    );
    Ok(())
}

pub(super) struct Client {
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl Drop for Client {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            thread.join().unwrap();
        }
    }
}
pub(super) fn client_enter(depth: u32, raw: bool) -> Option<Client> {
    let directory = std::env::var_os(DIRECTORY_ENV)?;
    assert!(raw && depth <= 2, "qualification runs exactly one unchanged raw chain");
    let directory = PathBuf::from(directory);
    let pid = std::process::id();
    let born = creation(unsafe { GetCurrentProcess() }).unwrap();
    let registration = json!({"depth": depth, "pid": pid, "creation_time": born,
        "session": identity::current()["session"], "desktop": thread_desktop(unsafe { GetCurrentThreadId() }).unwrap(),
        "startup": raw::startup(), "stdio": stdio(), "initial_console": console_sample(),
        "creation_contract": if depth == 0 { "unchanged SystemCommandRunner production seam" } else { "raw CREATE_UNICODE_ENVIRONMENT|EXTENDED_STARTUPINFO_PRESENT 0x80400; STARTF_USESTDHANDLES 0x100" }});
    atomic_json(&directory.join(format!("client-{depth}.json")), &registration).unwrap();
    let ack = directory.join(format!("ack-{depth}.json"));
    let start = Instant::now();
    while !ack.is_file() {
        assert!(
            start.elapsed() < READY,
            "observer did not ACK retained creation identity within 2 seconds"
        );
        std::thread::sleep(POLL);
    }
    let value: Value = serde_json::from_slice(&std::fs::read(ack).unwrap()).unwrap();
    assert_eq!(value["pid"], pid);
    assert_eq!(value["creation_time"], born);
    let stop = Arc::new(AtomicBool::new(false));
    let finished = stop.clone();
    let thread = std::thread::spawn(move || {
        let path = directory.join(format!("samples-{depth}.jsonl"));
        let mut file = File::create(path).unwrap();
        let start = Instant::now();
        for index in 0..SAMPLE_CAP {
            let row = json!({"sample": index, "elapsed_us": start.elapsed().as_micros(), "console": console_sample()});
            writeln!(file, "{row}").unwrap();
            file.flush().unwrap();
            if finished.load(Ordering::SeqCst) {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        writeln!(file, "{}", json!({"error": "console-sample-cap", "cap": SAMPLE_CAP})).unwrap();
    });
    // ACK and a live interval precede the next descendant; no provider creation flag is changed.
    std::thread::sleep(Duration::from_millis(100));
    Some(Client {
        stop,
        thread: Some(thread),
    })
}

struct Process {
    handle: OwnedHandle,
    pid: u32,
    parent: u32,
    born: u64,
    session: u32,
    depth: u32,
    discovered_us: u128,
    ack: bool,
}
struct Observer {
    directory: PathBuf,
    start: Instant,
    anchor: u32,
    anchor_born: u64,
    session: u32,
    phase: Arc<AtomicU8>, // 0 READY, 1 separate positive control, 2 provider, 3 cleanup.
    control_hwnd: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
    processes: BTreeMap<u32, Process>,
    events: Vec<Value>,
    errors: Vec<String>,
    desktop: String,
    control_seen: bool,
    max_gap_us: u128,
    last: Instant,
    scans: usize,
    cap_hit: bool,
    hook: Option<super::hooks::Hook>,
    coverage_start: u64,
    hook_control_seen: bool,
    hook_control_destroyed: bool,
    unresolved_events: usize,
    unresolved_api_errors: BTreeMap<String, usize>,
    max_hook_lag_ms: u32,
    known: BTreeMap<usize, Value>,
    image: String,
}
impl Observer {
    fn retain(&mut self) -> Result<()> {
        // Snapshot supplies only ancestry for filtering. Never retain/report foreign names/arguments/windows.
        let raw =
            unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) }.map_err(|e| failure("process snapshot", e))?;
        let snapshot = unsafe { OwnedHandle::from_raw_handle(raw.0) };
        let mut entry = PROCESSENTRY32W {
            dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        unsafe { Process32FirstW(HANDLE(snapshot.as_raw_handle()), &mut entry) }
            .map_err(|e| failure("Process32FirstW", e))?;
        let mut edges = Vec::new();
        loop {
            if edges.len() >= 16384 {
                return Err("process snapshot entry cap exceeded".into());
            }
            let end = entry
                .szExeFile
                .iter()
                .position(|c| *c == 0)
                .unwrap_or(entry.szExeFile.len());
            let same_image = String::from_utf16_lossy(&entry.szExeFile[..end]).eq_ignore_ascii_case(&self.image);
            edges.push((entry.th32ProcessID, entry.th32ParentProcessID, same_image));
            if let Err(e) = unsafe { Process32NextW(HANDLE(snapshot.as_raw_handle()), &mut entry) } {
                if e.code().0 != 0x80070012u32 as i32 {
                    return Err(failure("Process32NextW", e));
                }
                break;
            }
        }
        // Three fixed passes suffice for root/child/grandchild regardless of snapshot order.
        for _ in 0..3 {
            for &(pid, parent, same_image) in &edges {
                if !same_image {
                    continue;
                } // OS hosts/brokers are outside the owned fixture attribution scope.
                if self.processes.contains_key(&pid) {
                    continue;
                }
                let depth = if parent == self.anchor {
                    Some(0)
                } else {
                    self.processes
                        .get(&parent)
                        .filter(|p| unsafe { WaitForSingleObject(HANDLE(p.handle.as_raw_handle()), 0) } == WAIT_TIMEOUT)
                        .map(|p| p.depth + 1)
                };
                let Some(depth) = depth else {
                    continue;
                };
                if depth > 2 || self.processes.len() >= 3 {
                    return Err("unexpected owned process topology".into());
                }
                let handle = open(pid)?;
                let born = creation(HANDLE(handle.as_raw_handle()))?;
                let session = session(pid)?;
                if session != self.session {
                    return Err("owned process changed caller session".into());
                }
                if born < self.anchor_born {
                    return Err("owned candidate predates retained caller creation identity".into());
                }
                if let Some(p) = self.processes.get(&parent)
                    && born < p.born
                {
                    return Err("retained child predates parent identity".into());
                }
                self.processes.insert(
                    pid,
                    Process {
                        handle,
                        pid,
                        parent,
                        born,
                        session,
                        depth,
                        discovered_us: self.start.elapsed().as_micros(),
                        ack: false,
                    },
                );
            }
        }
        Ok(())
    }
    fn acknowledge(&mut self) -> Result<()> {
        for process in self.processes.values_mut() {
            if process.ack {
                continue;
            }
            let path = self.directory.join(format!("client-{}.json", process.depth));
            if !path.is_file() {
                continue;
            }
            let row: Value =
                serde_json::from_slice(&std::fs::read(path).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
            if row["pid"] != process.pid
                || row["creation_time"] != process.born
                || row["desktop"] != self.desktop
                || row["session"] != process.session
            {
                return Err("client registration differs from retained identity/current desktop".into());
            }
            let streams = row["stdio"].as_array().ok_or("missing standard-handle metadata")?;
            if streams.len() != 3
                || streams[0]["file_type"] != 2
                || streams[1]["file_type"] != 3
                || streams[2]["file_type"] != 3
                || streams[1]["handle"] == streams[2]["handle"]
                || streams.iter().any(|s| !s["handle_information_error"].is_null())
            {
                return Err("native provider standard handles differ from NUL/separate-pipe contract".into());
            }
            if unsafe { WaitForSingleObject(HANDLE(process.handle.as_raw_handle()), 0) } != WAIT_TIMEOUT {
                return Err("client exited before READY ACK".into());
            }
            atomic_json(
                &self.directory.join(format!("ack-{}.json", process.depth)),
                &json!({"pid": process.pid, "creation_time": process.born}),
            )?;
            process.ack = true;
        }
        Ok(())
    }
    fn scan(&mut self) -> Result<()> {
        self.max_gap_us = self.max_gap_us.max(self.last.elapsed().as_micros());
        self.last = Instant::now();
        self.retain()?;
        self.pump_events()?;
        // SAFETY: synchronous enumeration borrows this live Observer exclusively. Callback never panics.
        unsafe { EnumDesktopWindows(None, Some(window), LPARAM(std::ptr::from_mut(self) as isize)) }
            .map_err(|e| failure("EnumDesktopWindows", e))?;
        self.scans += 1;
        self.acknowledge()
    }
    fn record(&mut self, hwnd: HWND) -> Result<()> {
        let mut pid = 0;
        let tid = unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
        let phase = self.phase.load(Ordering::SeqCst);
        let control = phase == 1 && pid == self.anchor && hwnd.0 as usize == self.control_hwnd.load(Ordering::SeqCst);
        let Some(identity) = (if control {
            Some(self.anchor_born)
        } else {
            self.processes.get(&pid).map(|p| p.born)
        }) else {
            return Ok(());
        };
        if !control {
            let process = &self.processes[&pid];
            // PID reuse must never associate a new owner's window with an exited retained process.
            if unsafe { WaitForSingleObject(HANDLE(process.handle.as_raw_handle()), 0) } != WAIT_TIMEOUT {
                return Err("window owner retained lifetime ended during enumeration".into());
            }
        }
        if unsafe { GetAncestor(hwnd, GA_ROOT) } != hwnd {
            return Ok(());
        }
        if self.events.len() >= CAP {
            self.cap_hit = true;
            return Err("window event cap reached".into());
        }
        let mut class = [0u16; 256];
        let length = unsafe { GetClassNameW(hwnd, &mut class) };
        if length == 0 {
            return Err(format!("GetClassNameW: {}", unsafe { GetLastError() }.0));
        }
        let mut rect = RECT::default();
        unsafe { GetWindowRect(hwnd, &mut rect) }.map_err(|e| failure("GetWindowRect", e))?;
        let desktop = thread_desktop(tid)?;
        let visible = unsafe { IsWindowVisible(hwnd) }.as_bool();
        if control && visible && desktop == self.desktop {
            self.control_seen = true;
        }
        let row = json!({"elapsed_us": self.start.elapsed().as_micros(), "timestamp_100ns": ticks(unsafe { GetSystemTimeAsFileTime() }), "source": "desktop-enumeration", "phase": if control { "positive-control" } else { "provider" },
            "hwnd": hwnd.0 as usize, "owner_pid": pid, "owner_creation_time": identity, "thread": tid, "top_level": true,
            "class": String::from_utf16_lossy(&class[..length as usize]), "visible": visible, "desktop": desktop,
            "rect": [rect.left, rect.top, rect.right, rect.bottom]});
        self.known.insert(hwnd.0 as usize, row.clone());
        self.events.push(row);
        Ok(())
    }
    fn pump_events(&mut self) -> Result<()> {
        for event in super::hooks::pump()? {
            let mut pid = 0;
            unsafe { SetLastError(WIN32_ERROR(0)) };
            unsafe { GetWindowThreadProcessId(event.hwnd, Some(&mut pid)) };
            let window_error = if pid == 0 { unsafe { GetLastError() }.0 } else { 0 };
            let mut thread_error = String::new();
            // A destroyed HWND can still have a live generating thread. Query only identity, no contents.
            if pid == 0 {
                match unsafe { OpenThread(THREAD_QUERY_LIMITED_INFORMATION, false, event.thread) } {
                    Ok(raw) => {
                        let thread = unsafe { OwnedHandle::from_raw_handle(raw.0) };
                        pid = unsafe { GetProcessIdOfThread(HANDLE(thread.as_raw_handle())) };
                        if pid == 0 {
                            thread_error = format!("GetProcessIdOfThread: {}", unsafe { GetLastError() }.0);
                        }
                    }
                    Err(error) => thread_error = failure("OpenThread WinEvent identity", error),
                }
            }
            let previous = self.known.get(&(event.hwnd.0 as usize)).cloned();
            if pid == 0
                && let Some(row) = &previous
                && row["thread"] == event.thread
            {
                pid = row["owner_pid"].as_u64().unwrap_or(0) as u32;
            }
            if pid == 0 {
                self.unresolved_events += 1;
                *self
                    .unresolved_api_errors
                    .entry(format!("GetWindowThreadProcessId: {window_error}; {thread_error}"))
                    .or_default() += 1;
                continue;
            } // Unknown ownership cannot certify complete coverage.
            if pid != self.anchor && !self.processes.contains_key(&pid) {
                self.retain()?;
            }
            let owned = self.processes.contains_key(&pid);
            let control = pid == self.anchor
                && (event.hwnd.0 as usize == self.control_hwnd.load(Ordering::SeqCst)
                    || previous.as_ref().is_some_and(|row| row["phase"] == "positive-control"));
            if !owned && !control {
                continue;
            } // Persist no foreign window tuple or metadata.
            self.max_hook_lag_ms = self.max_hook_lag_ms.max(event.lag_ms);
            if event.kind != EVENT_OBJECT_DESTROY && unsafe { IsWindow(Some(event.hwnd)) }.as_bool() {
                self.record(event.hwnd)?;
            }
            if unsafe { IsWindow(Some(event.hwnd)) }.as_bool()
                && unsafe { GetAncestor(event.hwnd, GA_ROOT) } != event.hwnd
            {
                continue;
            }
            let Some(mut row) = self.known.get(&(event.hwnd.0 as usize)).cloned().or(previous) else {
                return Err("owned WinEvent lacks independently queried window metadata".into());
            };
            let born = if control {
                self.anchor_born
            } else {
                self.processes[&pid].born
            };
            if row["owner_pid"] != pid || row["owner_creation_time"] != born || row["thread"] != event.thread {
                return Err("WinEvent identity differs from retained window owner/thread".into());
            }
            if self.events.len() >= CAP {
                self.cap_hit = true;
                return Err("window event cap reached".into());
            }
            row["source"] = json!("winevent");
            row["event"] = json!(event.kind);
            row["event_tick_ms"] = json!(event.generated_ms);
            row["delivery_lag_ms"] = json!(event.lag_ms);
            row["elapsed_us"] = json!(self.start.elapsed().as_micros());
            if event.kind == EVENT_OBJECT_SHOW {
                row["visible"] = json!(true);
            }
            if event.kind == EVENT_OBJECT_HIDE || event.kind == EVENT_OBJECT_DESTROY {
                row["visible"] = json!(false);
            }
            if control && event.kind == EVENT_OBJECT_SHOW {
                self.hook_control_seen = true;
            }
            if control && event.kind == EVENT_OBJECT_DESTROY {
                self.hook_control_destroyed = true;
            }
            self.events.push(row);
        }
        Ok(())
    }
    fn finish(mut self) -> Value {
        let queue_drained = match self.pump_events() {
            Ok(()) => true,
            Err(error) => {
                self.errors.push(error);
                false
            }
        };
        let hook_closed = self.hook.as_mut().is_some_and(|hook| match hook.close() {
            Ok(()) => true,
            Err(error) => {
                self.errors.push(error);
                false
            }
        });
        let coverage_end = ticks(unsafe { GetSystemTimeAsFileTime() });
        let mut errors = self.errors;
        let mut lifetimes = Vec::new();
        for p in self.processes.values() {
            let status = unsafe { WaitForSingleObject(HANDLE(p.handle.as_raw_handle()), 0) };
            if status != WAIT_OBJECT_0 {
                errors.push(format!(
                    "owned process {} not confirmed exited, wait={}",
                    p.pid, status.0
                ));
            }
            let exit_time = if status == WAIT_OBJECT_0 {
                match process_times(HANDLE(p.handle.as_raw_handle())) {
                    Ok((_, exit)) => exit,
                    Err(error) => {
                        errors.push(error);
                        0
                    }
                }
            } else {
                0
            };
            lifetimes.push(
                json!({"pid": p.pid, "parent_pid": p.parent, "creation_time": p.born, "session": p.session, "depth": p.depth,
                "exit_time": exit_time, "discovered_us": p.discovered_us, "ack": p.ack, "retained_exit": status == WAIT_OBJECT_0}),
            );
        }
        if self.processes.len() != 3 || self.processes.values().any(|p| !p.ack) {
            errors.push("missing owned root/child/grandchild registration".into());
        }
        let counterexample = self.events.iter().any(provider_visible);
        // A GUI control proves desktop/hook readiness only. Each independently discovered provider
        // window must demonstrate its own CREATE/DESTROY delivery; do not assume console-class support.
        let windows: BTreeSet<_> = self
            .events
            .iter()
            .filter(|row| row["phase"] == "provider")
            .map(|row| {
                (
                    row["hwnd"].as_u64().unwrap_or(0),
                    row["owner_creation_time"].as_u64().unwrap_or(0),
                )
            })
            .collect();
        let window_lifecycle_complete = windows.iter().all(|(hwnd, born)| {
            [EVENT_OBJECT_CREATE, EVENT_OBJECT_DESTROY].iter().all(|kind| {
                self.events.iter().any(|row| {
                    row["source"] == "winevent"
                        && row["hwnd"] == *hwnd
                        && row["owner_creation_time"] == *born
                        && row["event"] == *kind
                })
            })
        });
        let full_lifetime = self.coverage_start != 0
            && hook_closed
            && queue_drained
            && errors.is_empty()
            && self.unresolved_events == 0
            && self.hook_control_seen
            && self.hook_control_destroyed
            && !self.cap_hit
            && window_lifecycle_complete
            && lifetimes.len() == 3
            && lifetimes.iter().all(|row| {
                row["creation_time"]
                    .as_u64()
                    .is_some_and(|born| born >= self.coverage_start)
                    && row["exit_time"]
                        .as_u64()
                        .is_some_and(|exit| exit > 0 && exit <= coverage_end)
            });
        json!({"events": self.events, "lifetimes": lifetimes, "errors": errors, "control_seen": self.control_seen,
            "scans": self.scans, "max_scan_gap_us": self.max_gap_us, "event_cap": CAP, "event_cap_hit": self.cap_hit,
            "coverage": {"scope": "owned-fixture-top-level-windows", "method": "prearmed out-of-context WinEvent pump plus independent enumeration",
                "start_100ns": self.coverage_start, "end_100ns": coverage_end, "hook_closed": hook_closed, "queue_drained": queue_drained,
                "control_show_event": self.hook_control_seen, "control_destroy_event": self.hook_control_destroyed,
                "provider_window_count": windows.len(), "window_lifecycle_complete": window_lifecycle_complete,
                "unresolved_event_owners": self.unresolved_events, "max_owned_delivery_lag_ms": self.max_hook_lag_ms,
                "unresolved_owner_api_errors": self.unresolved_api_errors,
                "outside_broker": "unknown: no client association contract", "full_lifetime": full_lifetime,
                "limit": "WinEvent delivery contract and measured queue drain; no arbitrary GUI/broker sandbox claim"},
            "visibility_counterexample": counterexample, "cleanup_confirmed": self.processes.len() == 3 && self.processes.values().all(|p| unsafe { WaitForSingleObject(HANDLE(p.handle.as_raw_handle()), 0) } == WAIT_OBJECT_0)})
    }
}
unsafe extern "system" fn window(hwnd: HWND, data: LPARAM) -> BOOL {
    let observer = unsafe { &mut *(data.0 as *mut Observer) };
    if let Err(error) = observer.record(hwnd)
        && observer.errors.len() < 16
    {
        observer.errors.push(error);
        // Continue enumeration to preserve bounded evidence; errors force unknown qualification.
    }
    true.into()
}
fn provider_visible(event: &Value) -> bool {
    event["phase"] == "provider" && event["visible"] == true
}
fn protocol_checks() {
    // Finite fake events verify phase separation and root inclusion before any OS experiment.
    assert!(!provider_visible(
        &json!({"phase": "positive-control", "visible": true})
    ));
    assert!(provider_visible(
        &json!({"phase": "provider", "depth": 0, "visible": true})
    ));
    assert!(!provider_visible(&json!({"phase": "provider", "visible": false})));
    assert!(!provider_visible(&json!({"phase": "provider", "error": "lost-event"})));
}

struct Control(HWND);
impl Drop for Control {
    fn drop(&mut self) {
        let _ = unsafe { DestroyWindow(self.0) };
    }
}
fn control(seen: &mpsc::Receiver<bool>, expected: &AtomicUsize) -> Result<Value> {
    // A predefined inert STATIC window on this detached caller's desktop, never part of the provider route.
    let start = Instant::now();
    let hwnd = unsafe {
        CreateWindowExW(
            WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW,
            w!("STATIC"),
            w!(""),
            WS_POPUP,
            0,
            0,
            16,
            16,
            None,
            None,
            None,
            None,
        )
    }
    .map_err(|e| failure("positive control CreateWindowExW", e))?;
    let window = Control(hwnd);
    expected.store(hwnd.0 as usize, Ordering::SeqCst);
    let _ = unsafe { ShowWindow(hwnd, SW_SHOWNOACTIVATE) }; // Separate synthetic control only; provider flags never change.
    while start.elapsed() < CONTROL {
        let mut message = MSG::default();
        while unsafe { PeekMessageW(&mut message, None, 0, 0, PM_REMOVE) }.as_bool() {
            unsafe { DispatchMessageW(&message) };
        }
        if seen.try_recv() == Ok(true) {
            unsafe { DestroyWindow(window.0) }.map_err(|e| failure("positive control DestroyWindow", e))?;
            expected.store(0, Ordering::SeqCst);
            if unsafe { IsWindow(Some(window.0)) }.as_bool() {
                return Err("positive control window cleanup unconfirmed".into());
            }
            let report = json!({"hwnd": window.0.0 as usize, "elapsed_us": start.elapsed().as_micros(),
                "console_hwnd": unsafe { GetConsoleWindow() }.0 as usize,
                "independently_seen": true, "destroyed": true, "is_original_red": false});
            std::mem::forget(window);
            if start.elapsed() >= Duration::from_secs(1) {
                return Err("positive control exceeded 1s limit".into());
            }
            return Ok(report);
        }
        std::thread::sleep(POLL);
    }
    Err("positive control was not independently discovered within 800ms".into())
}

pub(super) fn experiment() {
    protocol_checks();
    let directory = PathBuf::from(std::env::var_os(DIRECTORY_ENV).expect("qualification protocol directory"));
    let evidence_directory = directory.clone();
    let start = Instant::now();
    let phase = Arc::new(AtomicU8::new(0));
    let control_hwnd = Arc::new(AtomicUsize::new(0));
    let stop = Arc::new(AtomicBool::new(false));
    let (ready_tx, ready_rx) = mpsc::sync_channel(1);
    let (seen_tx, seen_rx) = mpsc::sync_channel(1);
    let observer_phase = phase.clone();
    let expected_control = control_hwnd.clone();
    let observer_stop = stop.clone();
    let observer = std::thread::spawn(move || {
        let desktop = thread_desktop(unsafe { GetCurrentThreadId() });
        let input = input_desktop();
        let anchor_born = creation(unsafe { GetCurrentProcess() });
        let caller_session = session(std::process::id());
        let ready = match (&desktop, &input, &anchor_born, &caller_session) {
            (Ok(current), Ok(input), Ok(born), Ok(session))
                if current == input && station::current()["visible"] == true && *session != 0 =>
            {
                Ok(
                    json!({"desktop": current, "input_desktop": input, "caller_pid": std::process::id(), "caller_creation_time": born, "session": session, "station": station::current()}),
                )
            }
            _ => Err(format!(
                "interactive/current-input desktop/identity qualification failed: {desktop:?}/{input:?}/{anchor_born:?}/{caller_session:?}"
            )),
        };
        let mut state = Observer {
            directory,
            start,
            anchor: std::process::id(),
            anchor_born: anchor_born.unwrap_or(0), // Unavailable identity already rejects READY above.
            session: caller_session.unwrap_or(0),
            phase: observer_phase,
            control_hwnd: expected_control,
            stop: observer_stop,
            processes: BTreeMap::new(),
            events: Vec::new(),
            errors: Vec::new(),
            desktop: desktop.unwrap_or_default(),
            control_seen: false,
            max_gap_us: 0,
            last: Instant::now(),
            scans: 0,
            cap_hit: false,
            hook: None,
            coverage_start: 0,
            hook_control_seen: false,
            hook_control_destroyed: false,
            unresolved_events: 0,
            unresolved_api_errors: BTreeMap::new(),
            max_hook_lag_ms: 0,
            known: BTreeMap::new(),
            image: std::env::current_exe()
                .unwrap()
                .file_name()
                .unwrap()
                .to_string_lossy()
                .into_owned(),
        };
        // READY follows hook installation and a successful pump/scan; providers/control have not started.
        let ready = ready.and_then(|value| {
            state.hook = Some(super::hooks::Hook::install()?);
            state.coverage_start = ticks(unsafe { GetSystemTimeAsFileTime() });
            state.scan()?;
            Ok(value)
        });
        let ready_ok = ready.is_ok();
        if ready_tx.send(ready).is_err() {
            state.errors.push("READY receiver exited".into());
        }
        if !ready_ok {
            return state.finish();
        }
        let mut notified = false;
        while start.elapsed() < Duration::from_secs(15) {
            if let Err(error) = state.scan() {
                state.errors.push(error);
                break;
            }
            if state.control_seen && state.hook_control_seen && !notified {
                let _ = seen_tx.try_send(true);
                notified = true;
            }
            if state.stop.load(Ordering::SeqCst) {
                // One final independent desktop scan after all retained clients exited.
                if let Err(error) = state.scan() {
                    state.errors.push(error);
                }
                return state.finish();
            }
            std::thread::sleep(POLL);
        }
        state
            .errors
            .push("observer exited early or exceeded finite 15s lifetime".into());
        state.finish()
    });
    let mut report = json!({"result": "visibility-observer", "route": "raw-handle-list", "ready": false,
        "classification": "unqualified", "visibility_counterexample": false, "shared_hidden_console": false,
        "full_observation": false, "runner_limit_ms": 10000, "ready_limit_ms": 2000, "control_limit_ms": 800,
        "positive_control_is_red": false, "failure": null,
        "control": {"status": "not started", "cleanup": "unknown"},
        "default_failure_state": {"event_loss": "unknown", "coverage": "unknown", "cleanup": "unknown"}});
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
        let ready = ready_rx.recv_timeout(READY).map_err(|e| format!("READY ACK: {e}"))??;
        report["ready"] = json!(true);
        report["ready_ack_us"] = json!(start.elapsed().as_micros());
        report["interactive_station"] = json!(true);
        report["ready_context"] = ready;
        phase.store(1, Ordering::SeqCst);
        report["control"] = control(&seen_rx, &control_hwnd)?;
        phase.store(2, Ordering::SeqCst);
        let executable = std::env::current_exe().map_err(|e| e.to_string())?;
        let output = SystemCommandRunner
            .run(
                executable.to_str().ok_or("non-Unicode fixture path")?,
                &["window-raw-tree", "0"],
                Duration::from_secs(10),
            )
            .map_err(|e| format!("runner: {e:?}"))?;
        if !output.success {
            return Err(format!("native tree failed: {}", output.stderr));
        }
        report["rows"] = serde_json::from_str(&format!("[{}]", output.stdout.lines().collect::<Vec<_>>().join(",")))
            .map_err(|e| e.to_string())?;
        phase.store(3, Ordering::SeqCst);
        // Retained process handles, not PID absence, qualify cleanup. Allow finite asynchronous job teardown.
        std::thread::sleep(Duration::from_millis(100));
        Ok(())
    }));
    match result {
        Ok(Ok(())) => {}
        Ok(Err(error)) => report["failure"] = json!(error),
        Err(_) => report["failure"] = json!("observer experiment panicked; no qualification"),
    }
    stop.store(true, Ordering::SeqCst);
    match observer.join() {
        Ok(observation) => {
            report["visibility_counterexample"] = observation["visibility_counterexample"].clone();
            report["full_observation"] = observation["coverage"]["full_lifetime"].clone();
            report["observation"] = observation;
        }
        Err(_) => report["failure"] = json!("independent observer panicked/early exit"),
    }
    report["elapsed_us"] = json!(start.elapsed().as_micros());
    // Preserve bounded protocol/sampler evidence before the parent's Scratch directory is removed.
    let mut protocol = Vec::new();
    let mut protocol_errors = Vec::new();
    for depth in 0..3 {
        let registration = std::fs::read(evidence_directory.join(format!("client-{depth}.json")))
            .map_err(|e| e.to_string())
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).map_err(|e| e.to_string()));
        let samples = std::fs::read_to_string(evidence_directory.join(format!("samples-{depth}.jsonl")))
            .map_err(|e| e.to_string())
            .and_then(|text| {
                if text.lines().count() > SAMPLE_CAP + 1 {
                    return Err("console sample cap exceeded".into());
                }
                text.lines()
                    .map(|line| serde_json::from_str::<Value>(line).map_err(|e| e.to_string()))
                    .collect::<Result<Vec<_>>>()
            });
        if registration.is_err() || samples.is_err() {
            protocol_errors.push(format!(
                "depth {depth}: registration={:?}, samples={:?}",
                registration.as_ref().err(),
                samples.as_ref().err()
            ));
        }
        if let Ok(samples) = &samples
            && (samples.is_empty()
                || samples
                    .iter()
                    .any(|s| !s["error"].is_null() || s["console"]["members"].is_null()))
        {
            protocol_errors.push(format!("depth {depth}: missing, capped or failed console samples"));
        }
        protocol.push(json!({"depth": depth, "registration": registration.ok(), "samples": samples.ok()}));
    }
    report["client_protocol"] = json!(protocol);
    report["protocol_errors"] = json!(protocol_errors);
    // Unknown pre-discovery/broker coverage is never promoted to PASS, even if candidate association is hidden.
    if let Some(rows) = report["rows"].as_array() {
        let hidden = rows.len() == 3
            && rows.iter().all(|r| {
                r["valid"] == true
                    && r["enumerated"] == true
                    && r["visible"] == false
                    && r["class"] == "ConsoleWindowClass"
                    && r["hwnd"] != 0
                    && r["hwnd"] == rows[0]["hwnd"]
                    && r["owner"] != 0
                    && r["owner"] == rows[0]["owner"]
                    && r["desktop"] == r["window_desktop"]
            });
        report["shared_hidden_console"] = json!(hidden);
    }
    println!("{report}");
}
