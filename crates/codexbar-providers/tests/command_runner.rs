//! Credential-free native subprocess fixtures. No shell or installed provider CLI is used.
use codexbar_providers::{CommandError, CommandOptions, CommandRunner, SystemCommandRunner};
use std::fs::File;
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const MIB: usize = 1024 * 1024;
const LIMIT: usize = 8 * MIB;

#[cfg(all(coverage, any(target_os = "linux", target_os = "macos")))]
#[path = "command_runner/profile.rs"]
mod profile;

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

/// Never exits, so the runner's group kill cannot land inside an instrumented build's exit-time profile write.
fn idle() -> ! {
    loop {
        std::thread::sleep(Duration::from_secs(60));
    }
}

/// Overflow, then wait for the runner's kill instead of exiting into the profile writer (#275).
fn held(stream: &str, size: usize) -> ! {
    emit(stream, size);
    idle()
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
        "emit" => emit(&args[1], args[2].parse().unwrap()),
        "held" => held(&args[1], args[2].parse().unwrap()),
        #[cfg(all(coverage, any(target_os = "linux", target_os = "macos")))]
        "profile" => profile::helper(Path::new(&args[1]), &args[2]),
        "invalid" => std::io::stdout().write_all(&[0xff, b'a']).unwrap(),
        "utf8" => {
            std::io::stdout().write_all(&vec![b'a'; 8191]).unwrap();
            std::io::stdout().write_all("€".as_bytes()).unwrap();
        }
        "scoped" => {
            assert_eq!(
                std::env::current_dir().unwrap().canonicalize().unwrap(),
                PathBuf::from(&args[1]).canonicalize().unwrap()
            );
            assert!(std::env::var_os("CODEXBAR_SYNTHETIC_PARENT_ONLY").is_none());
            assert!(std::env::var_os("META_API_KEY").is_none());
            assert!(std::env::var_os("meta_api_key").is_none());
            assert_eq!(std::env::var("MUSE_LOGIN").unwrap(), "0");
            assert_eq!(std::env::var("MUSE_NO_AUTO_UPDATE").unwrap(), "1");
            assert_eq!(std::io::stdin().read(&mut [0u8; 1]).unwrap(), 0);
        }
        // Without a working folder the child keeps the parent's, and still gets its variables (agy, chief#43).
        "unscoped" => {
            assert_eq!(std::env::current_dir().unwrap(), PathBuf::from(&args[1]));
            assert_eq!(std::env::var("AGY_CLI_DISABLE_AUTO_UPDATE").unwrap(), "true");
        }
        "scoped-parent" => {
            let options = CommandOptions {
                cwd: Some(PathBuf::from(&args[1])),
                remove_env: vec![
                    "CODEXBAR_SYNTHETIC_PARENT_ONLY",
                    "META_API_KEY",
                    "MUSE_LOGIN",
                    "MUSE_NO_AUTO_UPDATE",
                ],
                env: vec![("MUSE_LOGIN", "0"), ("MUSE_NO_AUTO_UPDATE", "1")],
                output_limit: 4 * MIB,
            };
            let executable = std::env::current_exe().unwrap();
            let program = executable.to_str().unwrap();
            assert_eq!(std::env::var("CODEXBAR_SYNTHETIC_PARENT_ONLY").unwrap(), "synthetic");
            assert!(
                SystemCommandRunner
                    .run_with_options(program, &["scoped", &args[1]], Duration::from_secs(3), &options)
                    .unwrap()
                    .success
            );
            assert_eq!(std::env::var("CODEXBAR_SYNTHETIC_PARENT_ONLY").unwrap(), "synthetic");
            assert_eq!(std::env::var("MUSE_LOGIN").unwrap(), "synthetic-parent");
            assert_eq!(
                SystemCommandRunner.run_with_options(
                    program,
                    &["held", "stdout", &(4 * MIB + 1).to_string()],
                    Duration::from_secs(3),
                    &options
                ),
                Err(CommandError::Failed)
            );
        }
        #[cfg(windows)]
        "console-parent" => console_parent(),
        #[cfg(windows)]
        "console-child" => console_child(),
        #[cfg(windows)]
        "console-grandchild" => print!("{}", console_window()),
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
            idle()
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

/// Starts the runner's child the way the release app does (chief#43): from a process with no console, like the
/// GUI-subsystem CodexBar, where a console child that isn't started without a window gets a new, visible console.
#[cfg(windows)]
fn console_parent() {
    // SAFETY: detaches this fixture process from the test's console, if it has one; its standard streams stay the
    // captured pipes. The assertion below checks the outcome either way.
    let _ = unsafe { windows::Win32::System::Console::FreeConsole() };
    assert_eq!(
        console_window(),
        "none",
        "the fixture parent has no console, like the release app"
    );
    let program = std::env::current_exe().unwrap();
    let output = SystemCommandRunner
        .run(program.to_str().unwrap(), &["console-child"], Duration::from_secs(10))
        .unwrap();
    assert!(output.success, "{}", output.stderr);
    print!("{}", output.stdout);
}

/// A background CLI and an ordinary descendant it starts with no creation flags (#271), both of which must have no
/// console window at all, visible or not.
#[cfg(windows)]
fn console_child() {
    let grandchild = Command::new(std::env::current_exe().unwrap())
        .arg("console-grandchild")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(grandchild.status.success());
    print!(
        "child={} grandchild={}",
        console_window(),
        String::from_utf8_lossy(&grandchild.stdout)
    );
}

/// This process's console window: `none` without one, else whether Windows shows it.
#[cfg(windows)]
fn console_window() -> &'static str {
    // SAFETY: plain queries of this process's console window handle and its visibility.
    let window = unsafe { windows::Win32::System::Console::GetConsoleWindow() };
    if window.is_invalid() {
        "none"
    } else if unsafe { windows::Win32::UI::WindowsAndMessaging::IsWindowVisible(window) }.as_bool() {
        "visible"
    } else {
        "hidden"
    }
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
    let scoped = Command::new(program)
        .args(["scoped-parent", scratch.0.to_str().unwrap()])
        .env("CODEXBAR_SYNTHETIC_PARENT_ONLY", "synthetic")
        .env("MUSE_LOGIN", "synthetic-parent")
        .env("META_API_KEY", "synthetic-only")
        .env("meta_api_key", "synthetic-only")
        .output()
        .unwrap();
    assert!(scoped.status.success(), "{}", String::from_utf8_lossy(&scoped.stderr));
    let here = std::env::current_dir().unwrap();
    let unscoped = CommandOptions {
        cwd: None,
        remove_env: vec!["AGY_CLI_DISABLE_AUTO_UPDATE"],
        env: vec![("AGY_CLI_DISABLE_AUTO_UPDATE", "true")],
        output_limit: MIB,
    };
    let kept = SystemCommandRunner
        .run_with_options(program, &["unscoped", here.to_str().unwrap()], timeout, &unscoped)
        .unwrap();
    assert!(kept.success, "{}", kept.stderr);
    #[cfg(windows)]
    {
        let console = Command::new(program).arg("console-parent").output().unwrap();
        assert!(console.status.success(), "{}", String::from_utf8_lossy(&console.stderr));
        assert_eq!(
            String::from_utf8_lossy(&console.stdout),
            "child=none grandchild=none",
            "background CLIs and their descendants must get no console window (chief#43)"
        );
    }
    #[cfg(all(coverage, any(target_os = "linux", target_os = "macos")))]
    profile::qualify(program, &scratch.0);
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
