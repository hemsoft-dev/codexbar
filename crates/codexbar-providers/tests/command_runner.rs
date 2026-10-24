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
        "descendant" | "detached-output" => {
            let mut command = Command::new(std::env::current_exe().unwrap());
            command.args(["hang", &args[1]]).stdin(Stdio::null());
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt as _;
                command.creation_flags(0x0800_0000); // The fixture's descendant must stay hidden too.
            }
            if args[0] == "detached-output" {
                command.stdout(Stdio::null()).stderr(Stdio::null());
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
    for mode in ["hang", "descendant", "overflow", "detached-output"] {
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
