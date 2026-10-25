//! Credential-free native subprocess fixtures. No shell or installed provider CLI is used.
use codexbar_providers::{CommandError, CommandRunner, SystemCommandRunner};
use std::fs::File;
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const MIB: usize = 1024 * 1024;
const LIMIT: usize = 8 * MIB;

fn emit(stream: &str, size: usize) {
    std::thread::scope(|scope| {
        if stream != "stderr" {
            scope.spawn(move || std::io::stdout().write_all(&vec![b'X'; size]).unwrap());
        }
        if stream != "stdout" {
            scope.spawn(move || std::io::stderr().write_all(&vec![b'Y'; size]).unwrap());
        }
    });
}

fn hold(path: &Path) -> File {
    let mut file = File::create(path).unwrap();
    file.lock().unwrap();
    file.write_all(b"ready").unwrap();
    file.flush().unwrap();
    let ready = path.with_extension("ready");
    let staging = path.with_extension("ready.tmp");
    std::fs::write(&staging, std::process::id().to_string()).unwrap();
    std::fs::rename(staging, ready).unwrap();
    file
}

fn fixture(args: &[String]) -> ! {
    match args[0].as_str() {
        #[cfg(windows)]
        "window-parent" => windows_console::qualify(),
        #[cfg(windows)]
        "window-tree" => windows_console::tree(args[1].parse().unwrap()),
        "emit" => emit(&args[1], args[2].parse().unwrap()),
        "invalid" => std::io::stdout().write_all(&[0xff, b'a']).unwrap(),
        "utf8" => {
            std::io::stdout().write_all(&vec![b'a'; 8191]).unwrap();
            std::io::stdout().write_all("€".as_bytes()).unwrap();
        }
        "failure" => {
            std::io::stderr().write_all(b"synthetic failure").unwrap();
            std::process::exit(7);
        }
        "stdin" => assert_eq!(std::io::stdin().read(&mut [0u8; 1]).unwrap(), 0),
        "hang" | "overflow" => {
            let _held = hold(Path::new(&args[1]));
            if args[0] == "overflow" {
                emit("both", LIMIT + 1);
            }
            loop {
                std::thread::sleep(Duration::from_secs(60));
            }
        }
        "descendant" | "detached-output" | "redirected-inherited" => {
            let mut command = Command::new(std::env::current_exe().unwrap());
            command.args(["hang", &args[1]]).stdin(Stdio::null());
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt as _;
                command.creation_flags(0x0800_0000); // The fixture's descendant must stay hidden too.
            }
            if args[0] != "descendant" {
                command.stdout(Stdio::null()).stderr(Stdio::null());
                #[cfg(windows)]
                capture_inheritance(args[0] == "detached-output");
            }
            #[expect(
                clippy::zombie_processes,
                reason = "Fixture deliberately exits first to test runner process-tree cleanup"
            )]
            let _child = command.spawn().unwrap();
            let start = Instant::now();
            while !Path::new(&args[1]).with_extension("ready").is_file() {
                assert!(start.elapsed() < Duration::from_secs(2));
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        _ => panic!("unknown fixture"),
    }
    std::process::exit(0);
}

#[cfg(windows)]
fn capture_inheritance(clear: bool) {
    use std::os::windows::io::AsRawHandle as _;
    use windows::Win32::Foundation::{
        GetHandleInformation, HANDLE, HANDLE_FLAG_INHERIT, HANDLE_FLAGS, SetHandleInformation,
    };
    // Rust's Windows spawn inherits every inheritable handle, including old standard handles even
    // when the descendant's standard streams are redirected. This fixture owns these two handles.
    for raw in [std::io::stdout().as_raw_handle(), std::io::stderr().as_raw_handle()] {
        let handle = HANDLE(raw);
        let mut flags = 0;
        // SAFETY: valid standard handles supplied by the runner, and writable flag storage.
        unsafe { GetHandleInformation(handle, &mut flags) }.unwrap();
        assert_ne!(
            flags & HANDLE_FLAG_INHERIT.0,
            0,
            "the control must inherit the captured pipe"
        );
        if clear {
            // SAFETY: only this fixture's captured handles change; the new descendant uses null streams.
            unsafe { SetHandleInformation(handle, HANDLE_FLAG_INHERIT.0, HANDLE_FLAGS(0)) }.unwrap();
            unsafe { GetHandleInformation(handle, &mut flags) }.unwrap();
            assert_eq!(flags & HANDLE_FLAG_INHERIT.0, 0);
        }
    }
}

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let mut random = [0u8; 8];
        getrandom::fill(&mut random).unwrap();
        let dir = std::env::temp_dir().join(format!("codexbar-command-{:x}", u64::from_le_bytes(random)));
        std::fs::create_dir(&dir).unwrap();
        Self(dir)
    }
    fn marker(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn released(path: &Path) {
    let pid: u32 = std::fs::read_to_string(path.with_extension("ready"))
        .unwrap()
        .parse()
        .unwrap();
    assert!(pid > 0, "the fixture really started");
    #[cfg(windows)]
    assert_process_exited(pid);
    #[cfg(target_os = "macos")]
    assert_process_exited(pid);
    let mut file = File::open(path).unwrap();
    let start = Instant::now();
    loop {
        match file.try_lock() {
            Ok(()) => break,
            // Exit has been verified separately; OS file-lock cleanup can finish slightly later.
            Err(std::fs::TryLockError::WouldBlock)
                if cfg!(any(windows, target_os = "macos")) && start.elapsed() < Duration::from_secs(1) =>
            {
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(error) => panic!("fixture resource was not released: {error}"),
        }
    }
    let mut contents = String::new();
    file.read_to_string(&mut contents).unwrap();
    assert_eq!(contents, "ready");
}

#[cfg(target_os = "macos")]
fn assert_process_exited(pid: u32) {
    // SAFETY: this is a read-only query of our fixture PID, with an exactly sized output buffer.
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of_val(&info) as i32;
    let count = unsafe {
        libc::proc_pidinfo(
            pid as i32,
            libc::PROC_PIDTBSDINFO,
            0,
            std::ptr::from_mut(&mut info).cast(),
            size,
        )
    };
    if count == 0 {
        assert_eq!(std::io::Error::last_os_error().raw_os_error(), Some(libc::ESRCH));
    } else {
        assert_eq!(count, size);
        assert_eq!(info.pbi_status, libc::SZOMB, "fixture {pid} must already be terminated");
    }
}

#[cfg(windows)]
fn assert_process_exited(pid: u32) {
    use windows::Win32::Foundation::{CloseHandle, ERROR_INVALID_PARAMETER, WAIT_OBJECT_0};
    use windows::Win32::System::Threading::{OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject};
    // SAFETY: this fixture PID comes from its own isolated ready file. Open only for a nonblocking exit query.
    match unsafe { OpenProcess(PROCESS_SYNCHRONIZE, false, pid) } {
        Ok(handle) => {
            let state = unsafe { WaitForSingleObject(handle, 0) };
            let _ = unsafe { CloseHandle(handle) };
            assert_eq!(state, WAIT_OBJECT_0, "the fixture process must already be terminated");
        }
        Err(error) => assert_eq!(
            error.code(),
            ERROR_INVALID_PARAMETER.to_hresult(),
            "only an absent PID qualifies"
        ),
    }
}

fn main() {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if !args.is_empty() {
        fixture(&args);
    }
    #[cfg(windows)]
    windows_console::no_console_parent();
    let executable = std::env::current_exe().unwrap();
    let program = executable.to_str().unwrap();
    let run = |args: &[&str], timeout| SystemCommandRunner.run(program, args, timeout);
    let timeout = Duration::from_secs(3);
    let control = Command::new(program)
        .args(["emit", "stdout", "1048576"])
        .output()
        .unwrap();
    assert!(control.status.success());
    assert_eq!(control.stdout.len(), MIB);
    let size = MIB.to_string();
    for stream in ["stdout", "stderr", "both"] {
        let output = run(&["emit", stream, &size], timeout).expect("finite output must not fill a pipe and time out");
        assert!(output.success);
        assert_eq!(
            output.stdout,
            if stream == "stderr" {
                String::new()
            } else {
                "X".repeat(MIB)
            }
        );
        assert_eq!(
            output.stderr,
            if stream == "stdout" {
                String::new()
            } else {
                "Y".repeat(MIB)
            }
        );
    }
    let boundary = run(&["emit", "both", &LIMIT.to_string()], Duration::from_secs(10)).unwrap();
    assert_eq!(boundary.stdout.len(), LIMIT);
    assert_eq!(boundary.stderr.len(), LIMIT);
    assert_eq!(run(&["invalid"], timeout).unwrap().stdout, "\u{fffd}a");
    assert_eq!(run(&["utf8"], timeout).unwrap().stdout, "a".repeat(8191) + "€");
    let failure = run(&["failure"], timeout).unwrap();
    assert!(!failure.success);
    assert_eq!(failure.stderr, "synthetic failure");
    assert_eq!(run(&["stdin"], timeout).unwrap().stdout, "");
    assert_eq!(
        SystemCommandRunner.run("codexbar-missing-fixture-242", &[], timeout),
        Err(CommandError::NotFound)
    );
    let scratch = Scratch::new();
    #[cfg(not(windows))]
    let modes = ["hang", "descendant", "overflow", "detached-output"];
    #[cfg(windows)]
    let modes = [
        "hang",
        "descendant",
        "overflow",
        "detached-output",
        "redirected-inherited",
    ];
    for mode in modes {
        println!("qualify command fixture: {mode}");
        let marker = scratch.marker(mode);
        let start = Instant::now();
        let result = run(&[mode, marker.to_str().unwrap()], timeout);
        match mode {
            "overflow" => assert_eq!(result, Err(CommandError::Failed)),
            "detached-output" => assert!(result.unwrap().success),
            _ => assert_eq!(result, Err(CommandError::TimedOut)),
        }
        assert!(
            start.elapsed() < timeout + Duration::from_secs(2),
            "deadline and cleanup must remain bounded"
        );
        released(&marker);
    }
    println!(
        "command runner: finite streams, cap boundary, overflow, timeout, descendant cleanup, exit status, UTF-8 and null stdin passed"
    );
}

#[cfg(windows)]
mod windows_console {
    use super::*;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::System::Console::GetConsoleWindow;
    use windows::Win32::System::StationsAndDesktops::{GetThreadDesktop, GetUserObjectInformationW, UOI_NAME};
    use windows::Win32::System::Threading::GetCurrentThreadId;
    use windows::Win32::UI::WindowsAndMessaging::{GetWindowThreadProcessId, IsWindowVisible};

    fn desktop(thread: u32) -> String {
        let mut name = [0u16; 256];
        // SAFETY: query a live thread's desktop into an exactly sized, writable UTF-16 buffer.
        unsafe {
            let handle = GetThreadDesktop(thread).unwrap();
            GetUserObjectInformationW(
                HANDLE(handle.0),
                UOI_NAME,
                Some(name.as_mut_ptr().cast()),
                std::mem::size_of_val(&name) as u32,
                None,
            )
            .unwrap();
        }
        String::from_utf16(&name[..name.iter().position(|ch| *ch == 0).unwrap()]).unwrap()
    }

    pub(super) fn tree(depth: u32) {
        // SAFETY: read-only queries of this fixture's actual console window and owning thread/process.
        let window = unsafe { GetConsoleWindow() };
        let mut owner = 0;
        let thread = unsafe { GetWindowThreadProcessId(window, Some(&mut owner)) };
        let row = serde_json::json!({
            "depth": depth, "pid": std::process::id(), "hwnd": window.0 as usize,
            "visible": unsafe { IsWindowVisible(window) }.as_bool(), "owner": owner,
            "desktop": desktop(unsafe { GetCurrentThreadId() }),
            "window_desktop": if thread == 0 { String::new() } else { desktop(thread) },
        });
        println!("{row}");
        std::io::stdout().flush().unwrap();
        if depth < 2 {
            // Ordinary child and grandchild. No creation flags, startup hints, vendor settings or hiding.
            assert!(
                Command::new(std::env::current_exe().unwrap())
                    .args(["window-tree", &(depth + 1).to_string()])
                    .status()
                    .unwrap()
                    .success()
            );
        }
    }

    pub(super) fn qualify() {
        assert!(
            unsafe { GetConsoleWindow() }.0.is_null(),
            "the runner's parent must have no console"
        );
        let executable = std::env::current_exe().unwrap();
        let output = SystemCommandRunner
            .run(
                executable.to_str().unwrap(),
                &["window-tree", "0"],
                Duration::from_secs(10),
            )
            .unwrap();
        assert!(output.success, "native tree failed: {}", output.stderr);
        let rows: Vec<serde_json::Value> = output
            .stdout
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        println!("ordinary descendant console observations: {}", output.stdout);
        assert_eq!(
            rows.len(),
            3,
            "root, ordinary child and ordinary grandchild must report"
        );
        for (depth, row) in rows.iter().enumerate() {
            assert_eq!(row["depth"], depth);
            assert_eq!(
                row["visible"], false,
                "background process allocated a visible console: {row}"
            );
        }
    }

    pub(super) fn no_console_parent() {
        use std::os::windows::process::CommandExt as _;
        // Only the test's CodexBar-like parent is suppressed. The production runner launches the root.
        let output = Command::new(std::env::current_exe().unwrap())
            .arg("window-parent")
            .creation_flags(0x0800_0000)
            .output()
            .unwrap();
        print!("{}", String::from_utf8_lossy(&output.stdout));
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    }
}
