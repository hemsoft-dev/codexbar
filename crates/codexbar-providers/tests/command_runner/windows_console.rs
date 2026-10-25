use super::*;
use windows::Win32::Foundation::{HANDLE, HWND, LPARAM};
use windows::Win32::System::Console::{GetConsoleProcessList, GetConsoleWindow};
use windows::Win32::System::StationsAndDesktops::{
    EnumDesktopWindows, GetThreadDesktop, GetUserObjectInformationW, UOI_NAME,
};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::WindowsAndMessaging::{GetClassNameW, GetWindowThreadProcessId, IsWindow, IsWindowVisible};
use windows::core::BOOL;

#[path = "windows_console/identity.rs"]
mod identity;

struct WindowSearch {
    target: HWND,
    found: bool,
}
unsafe extern "system" fn visit(window: HWND, data: LPARAM) -> BOOL {
    // SAFETY: enumeration is synchronous; data points to the exclusive live WindowSearch below.
    let search = unsafe { &mut *(data.0 as *mut WindowSearch) };
    search.found |= search.target == window;
    true.into()
}

fn enumerated(window: HWND) -> bool {
    let mut search = WindowSearch {
        target: window,
        found: false,
    };
    // SAFETY: only enumerate our current desktop; callback borrows this stack state synchronously.
    unsafe {
        EnumDesktopWindows(
            Some(GetThreadDesktop(GetCurrentThreadId()).unwrap()),
            Some(visit),
            LPARAM(std::ptr::from_mut(&mut search) as isize),
        )
        .unwrap();
    }
    search.found
}

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
    let mut members = [0; 16];
    let count = unsafe { GetConsoleProcessList(&mut members) } as usize;
    assert!(count <= members.len());
    let mut class = [0u16; 256];
    let length = unsafe { GetClassNameW(window, &mut class) } as usize;
    let mut visible = false;
    for _ in 0..5 {
        visible |= unsafe { IsWindowVisible(window) }.as_bool();
        std::thread::sleep(Duration::from_millis(20));
    }
    let row = serde_json::json!({
        "depth": depth, "pid": std::process::id(), "hwnd": window.0 as usize,
        "visible": visible, "owner": owner,
        "valid": unsafe { IsWindow(Some(window)) }.as_bool(),
        "enumerated": enumerated(window), "members": members[..count],
        "class": String::from_utf16(&class[..length]).unwrap(),
        "identity": identity::current(), "cwd": std::env::current_dir().unwrap(),
        "inherited_env": std::env::var("CODEXBAR_SYNTHETIC_INHERITED").unwrap(),
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
    // Reject the actual regression before the positive hidden-console association checks below.
    for row in &rows {
        assert_eq!(
            row["visible"], false,
            "background process allocated a visible console: {row}"
        );
    }
    for (depth, row) in rows.iter().enumerate() {
        assert_eq!(row["depth"], depth);
        assert_ne!(
            row["hwnd"], 0,
            "missing console is not inherited hidden-console proof: {row}"
        );
        assert_ne!(row["owner"], 0, "console owner must be known");
        assert_eq!(row["valid"], true);
        assert_eq!(
            row["enumerated"], true,
            "message-only or foreign HWND is not window evidence: {row}"
        );
        assert_eq!(row["class"], "ConsoleWindowClass");
        assert_eq!(row["hwnd"], rows[0]["hwnd"]);
        assert_eq!(row["owner"], rows[0]["owner"]);
        assert_eq!(row["desktop"], row["window_desktop"]);
        assert_eq!(row["desktop"], rows[0]["desktop"]);
        assert!(row["members"].as_array().unwrap().contains(&row["pid"]));
        for ancestor in &rows[..depth] {
            assert!(row["members"].as_array().unwrap().contains(&ancestor["pid"]));
        }
        assert_eq!(
            row["identity"],
            identity::current(),
            "usage query must retain the caller's user/logon/session"
        );
        assert_eq!(row["desktop"], desktop(unsafe { GetCurrentThreadId() }));
        assert_eq!(row["cwd"], serde_json::json!(std::env::current_dir().unwrap()));
        assert_eq!(row["inherited_env"], "native-fixture");
    }
    let window = HWND(rows[0]["hwnd"].as_u64().unwrap() as *mut std::ffi::c_void);
    let start = Instant::now();
    while unsafe { IsWindow(Some(window)) }.as_bool() {
        assert!(
            start.elapsed() < Duration::from_secs(3),
            "the final console client exited but its window remained"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

pub(super) fn no_console_parent() {
    use std::os::windows::process::CommandExt as _;
    // Only the test's CodexBar-like parent is suppressed. The production runner launches the root.
    let output = Command::new(std::env::current_exe().unwrap())
        .arg("window-parent")
        .env("CODEXBAR_SYNTHETIC_INHERITED", "native-fixture")
        .creation_flags(0x0800_0000)
        .output()
        .unwrap();
    print!("{}", String::from_utf8_lossy(&output.stdout));
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
}

fn handles() -> u32 {
    use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessHandleCount};
    let mut count = 0;
    // SAFETY: query only this fixture process's handle count.
    unsafe { GetProcessHandleCount(GetCurrentProcess(), &mut count) }.unwrap();
    count
}

pub(super) fn compatibility() {
    use std::os::windows::process::CommandExt as _;
    let executable = std::env::current_exe().unwrap();
    let program = executable.to_str().unwrap();
    let timeout = Duration::from_secs(10);
    let args = [
        "argv",
        "-p",
        "/usage",
        "--output-format",
        "json",
        "--print-timeout",
        "25s",
        "",
        "two words",
        "a\"b",
        "x\\\"y",
        "ends with space \\",
        "&|<>()^",
        "%PATH%",
        "!literal!",
        "é漢字",
        "tab\tvalue",
    ];
    let native = SystemCommandRunner.run(program, &args, timeout).unwrap();
    assert!(native.success);
    assert_eq!(serde_json::from_str::<Vec<String>>(&native.stdout).unwrap(), args[1..]);
    let scratch = Scratch::new();
    // An implicit cwd search would execute this invalid image instead of the fixture beside its parent.
    std::fs::write(
        scratch.marker(executable.file_name().unwrap().to_str().unwrap()),
        b"not an executable",
    )
    .unwrap();
    let lookup = Command::new(program)
        .arg("lookup-parent")
        .current_dir(&scratch.0)
        .creation_flags(0x0800_0000)
        .output()
        .unwrap();
    assert!(
        lookup.status.success(),
        "bare-name resolver changed: {}",
        String::from_utf8_lossy(&lookup.stderr)
    );
    let shim = scratch.marker("native shim.cmd");
    std::fs::write(&shim, format!("@echo off\r\n@\"{program}\" argv %*\r\n")).unwrap();
    let batch_args = &args[1..];
    let expected = Command::new(&shim)
        .args(batch_args)
        .creation_flags(0x0800_0000)
        .output()
        .unwrap();
    assert!(expected.status.success());
    let batch = SystemCommandRunner
        .run(shim.to_str().unwrap(), batch_args, timeout)
        .unwrap();
    assert!(batch.success, "{}", batch.stderr);
    assert_eq!(
        batch.stdout.as_bytes(),
        expected.stdout,
        "batch shims must preserve Rust Command argument behavior"
    );
    assert_eq!(batch.stderr.as_bytes(), expected.stderr);
    let before = handles();
    for _ in 0..12 {
        assert!(SystemCommandRunner.run(program, &["stdin"], timeout).unwrap().success);
    }
    assert_eq!(
        handles(),
        before,
        "repeated queries must release process, thread, pipe and job handles"
    );
    let marker = scratch.marker("parent-exit");
    let status = Command::new(program)
        .args(["runner-exit", marker.to_str().unwrap()])
        .creation_flags(0x0800_0000)
        .status()
        .unwrap();
    assert!(status.success());
    // Job-handle closure on abrupt parent exit is asynchronous. Wait only on our ready-file identity.
    let start = Instant::now();
    loop {
        let file = File::open(&marker).unwrap();
        if file.try_lock().is_ok() {
            break;
        }
        assert!(
            start.elapsed() < Duration::from_secs(3),
            "owned tree outlived its runner process"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    released(&marker);
    println!("Windows native and batch arguments, repeated handle cleanup and abrupt-parent job cleanup passed");
}

pub(super) fn lookup() {
    let executable = std::env::current_exe().unwrap();
    let name = executable.file_stem().unwrap().to_str().unwrap();
    let output = SystemCommandRunner
        .run(name, &["argv", "lookup-ok"], Duration::from_secs(10))
        .unwrap();
    assert!(output.success);
    assert_eq!(
        serde_json::from_str::<Vec<String>>(&output.stdout).unwrap(),
        ["lookup-ok"]
    );
}

pub(super) fn exit_with_owned_tree(marker: &Path) -> ! {
    let marker = marker.to_owned();
    let runner_marker = marker.clone();
    std::thread::spawn(move || {
        let executable = std::env::current_exe().unwrap();
        let _ = SystemCommandRunner.run(
            executable.to_str().unwrap(),
            &["descendant", runner_marker.to_str().unwrap()],
            Duration::from_secs(60),
        );
    });
    let start = Instant::now();
    while !marker.with_extension("ready").is_file() {
        assert!(start.elapsed() < Duration::from_secs(5));
        std::thread::sleep(Duration::from_millis(5));
    }
    // No Rust destructors run: only Windows closing the runner's job handle can stop the owned descendant.
    std::process::exit(0)
}
