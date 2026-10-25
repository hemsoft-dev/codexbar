//! Native environment discovery for the portable Windows launch planner.
use super::super::windows_plan::{self, LaunchPlan};
use std::ffi::OsString;
use std::io;
use std::os::windows::ffi::OsStringExt as _;
use std::path::{Path, PathBuf};
use windows::Win32::System::SystemInformation::{GetSystemDirectoryW, GetWindowsDirectoryW};

pub(super) fn command_line(program: &str, args: &[&str]) -> io::Result<LaunchPlan> {
    let resolved = windows_plan::resolve(program, &executable_dirs(), Path::exists)?;
    LaunchPlan::new(resolved, args, system_interpreter)
}

fn system_interpreter() -> io::Result<String> {
    let mut system = [0u16; 32768];
    // SAFETY: writable UTF-16 buffer. Use the system interpreter, never a PATH or COMSPEC replacement.
    let size = unsafe { GetSystemDirectoryW(Some(&mut system)) } as usize;
    if size == 0 || size >= system.len() {
        return Err(io::Error::last_os_error());
    }
    Ok(String::from_utf16(&system[..size]).map_err(io::Error::other)? + "\\cmd.exe")
}

fn executable_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<_> = std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(Path::to_owned))
        .into_iter()
        .collect();
    dirs.extend(system_dirs());
    if let Some(path) = std::env::var_os("PATH") {
        dirs.extend(std::env::split_paths(&path).filter(|path| !path.as_os_str().is_empty()));
    }
    dirs
}

fn system_dirs() -> Vec<PathBuf> {
    let queries: [fn(&mut [u16]) -> u32; 2] = [
        |buffer| unsafe { GetSystemDirectoryW(Some(buffer)) },
        |buffer| unsafe { GetWindowsDirectoryW(Some(buffer)) },
    ];
    let mut dirs = Vec::new();
    for query in queries {
        let mut buffer = [0u16; 32768];
        // Each query writes only into this supplied buffer and returns the UTF-16 length.
        let size = query(&mut buffer) as usize;
        if size > 0 && size < buffer.len() {
            dirs.push(PathBuf::from(OsString::from_wide(&buffer[..size])));
        }
    }
    dirs
}
