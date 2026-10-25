//! Ordinary Win32 creation with the observed Go startup/creation flags, not a request for a new console.
use std::os::windows::ffi::OsStrExt as _;
use std::os::windows::io::{AsRawHandle as _, FromRawHandle as _, OwnedHandle};
use windows::Win32::Foundation::{HANDLE, WAIT_OBJECT_0};
use windows::Win32::System::Console::{GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE};
use windows::Win32::System::Threading::{
    CREATE_UNICODE_ENVIRONMENT, CreateProcessW, DeleteProcThreadAttributeList, EXTENDED_STARTUPINFO_PRESENT,
    GetExitCodeProcess, GetStartupInfoW, InitializeProcThreadAttributeList, LPPROC_THREAD_ATTRIBUTE_LIST,
    PROC_THREAD_ATTRIBUTE_HANDLE_LIST, PROCESS_INFORMATION, STARTF_USESTDHANDLES, STARTUPINFOEXW, STARTUPINFOW,
    TerminateProcess, UpdateProcThreadAttribute, WaitForSingleObject,
};
use windows::core::{PCWSTR, PWSTR};

struct Attributes(Vec<usize>);
impl Attributes {
    fn pointer(&mut self) -> LPPROC_THREAD_ATTRIBUTE_LIST {
        LPPROC_THREAD_ATTRIBUTE_LIST(self.0.as_mut_ptr().cast())
    }
}
impl Drop for Attributes {
    fn drop(&mut self) {
        unsafe { DeleteProcThreadAttributeList(self.pointer()) };
    }
}

pub(super) fn startup() -> serde_json::Value {
    let mut info = STARTUPINFOW::default();
    unsafe { GetStartupInfoW(&mut info) };
    serde_json::json!({"flags": info.dwFlags.0, "show_window": info.wShowWindow})
}

pub(super) fn child(depth: u32) {
    let program = std::env::current_exe().unwrap();
    let application: Vec<u16> = program.as_os_str().encode_wide().chain([0]).collect();
    let mut line: Vec<u16> = format!("\"{}\" window-raw-tree {depth}", program.display())
        .encode_utf16()
        .chain([0])
        .collect();
    // Only existing standard handles are selected. No visibility hint or console-creation override is supplied.
    let handles = unsafe {
        [
            GetStdHandle(STD_INPUT_HANDLE).unwrap(),
            GetStdHandle(STD_OUTPUT_HANDLE).unwrap(),
            GetStdHandle(STD_ERROR_HANDLE).unwrap(),
        ]
    };
    let mut size = 0;
    let _ = unsafe { InitializeProcThreadAttributeList(None, 1, None, &mut size) };
    assert!(size > 0);
    let mut storage = vec![0; size.div_ceil(std::mem::size_of::<usize>())];
    unsafe {
        InitializeProcThreadAttributeList(
            Some(LPPROC_THREAD_ATTRIBUTE_LIST(storage.as_mut_ptr().cast())),
            1,
            None,
            &mut size,
        )
    }
    .unwrap();
    let mut attributes = Attributes(storage);
    unsafe {
        UpdateProcThreadAttribute(
            attributes.pointer(),
            0,
            PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
            Some(handles.as_ptr().cast()),
            std::mem::size_of_val(&handles),
            None,
            None,
        )
    }
    .unwrap();
    let mut startup = STARTUPINFOEXW::default();
    startup.StartupInfo.cb = std::mem::size_of_val(&startup) as u32;
    startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES; // 0x100
    startup.StartupInfo.hStdInput = handles[0];
    startup.StartupInfo.hStdOutput = handles[1];
    startup.StartupInfo.hStdError = handles[2];
    startup.lpAttributeList = attributes.pointer();
    let mut info = PROCESS_INFORMATION::default();
    // SAFETY: owned UTF-16 buffers, initialized attributes, live inherited standard handles and writable outputs.
    unsafe {
        CreateProcessW(
            PCWSTR(application.as_ptr()),
            Some(PWSTR(line.as_mut_ptr())),
            None,
            None,
            true,
            CREATE_UNICODE_ENVIRONMENT | EXTENDED_STARTUPINFO_PRESENT,
            None,
            PCWSTR::null(),
            &startup.StartupInfo,
            &mut info,
        )
    }
    .unwrap(); // 0x80400
    let process = unsafe { OwnedHandle::from_raw_handle(info.hProcess.0) };
    let _thread = unsafe { OwnedHandle::from_raw_handle(info.hThread.0) };
    if unsafe { WaitForSingleObject(HANDLE(process.as_raw_handle()), 10_000) } != WAIT_OBJECT_0 {
        let _ = unsafe { TerminateProcess(HANDLE(process.as_raw_handle()), 1) };
        panic!("raw ordinary child did not finish");
    }
    let mut code = 0;
    unsafe { GetExitCodeProcess(HANDLE(process.as_raw_handle()), &mut code) }.unwrap();
    assert_eq!(code, 0);
}
