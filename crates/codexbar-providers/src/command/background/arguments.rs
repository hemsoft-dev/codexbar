//! CreateProcess takes a command line, not argv. Native arguments use the CRT rules; batch shims use cmd's
//! distinct quoting and percent-expansion rules, as Rust's Windows Command implementation does.
use std::ffi::OsString;
use std::io;
use std::os::windows::ffi::OsStringExt as _;
use std::path::{Path, PathBuf};

use windows::Win32::System::SystemInformation::{GetSystemDirectoryW, GetWindowsDirectoryW};

pub(super) fn command_line(program: &str, args: &[&str]) -> io::Result<(Vec<u16>, Vec<u16>)> {
    let resolved = resolve(program)?;
    let batch = resolved.to_ascii_lowercase().ends_with(".cmd") || resolved.to_ascii_lowercase().ends_with(".bat");
    let (application, line) = if batch {
        batch_command(&resolved)?
    } else {
        (resolved.clone(), format!("\"{resolved}\""))
    };
    Ok((wide(&application)?, finish_line(line, args, batch)?))
}

fn batch_command(resolved: &str) -> io::Result<(String, String)> {
    let mut system = [0u16; 32768];
    // SAFETY: writable UTF-16 buffer. Resolve the system interpreter, never a PATH or COMSPEC replacement.
    let size = unsafe { GetSystemDirectoryW(Some(&mut system)) } as usize;
    if size == 0 || size >= system.len() {
        return Err(io::Error::last_os_error());
    }
    let interpreter = String::from_utf16(&system[..size]).map_err(io::Error::other)? + "\\cmd.exe";
    Ok((interpreter, format!("cmd.exe /e:ON /v:OFF /d /c \"\"{resolved}\"")))
}

fn finish_line(mut line: String, args: &[&str], batch: bool) -> io::Result<Vec<u16>> {
    for arg in args {
        line.push(' ');
        append(&mut line, arg, batch)?;
    }
    if batch {
        line.push('"');
    }
    let line = wide(&line)?;
    if line.len() > 32767 {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "command line is too long"));
    }
    Ok(line)
}

fn resolve(program: &str) -> io::Result<String> {
    wide(program)?;
    let path = Path::new(program);
    if program.is_empty() || path.file_name().is_none() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "program path has no file name",
        ));
    }
    let resolved = if path.parent().is_some_and(|parent| !parent.as_os_str().is_empty()) {
        explicit_path(path)
    } else {
        search(program)?
    };
    resolved
        .into_os_string()
        .into_string()
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "program path is not Unicode"))
}

fn explicit_path(path: &Path) -> PathBuf {
    if path
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("exe"))
    {
        return path.to_owned();
    }
    let mut suffixed = path.as_os_str().to_owned();
    suffixed.push(".exe");
    let suffixed = PathBuf::from(suffixed);
    if suffixed.exists() { suffixed } else { path.to_owned() }
}

fn search(program: &str) -> io::Result<PathBuf> {
    let name = if program.contains('.') {
        program.to_owned()
    } else {
        format!("{program}.exe")
    };
    // Match Rust Command's order, which deliberately excludes an implicit current-directory search.
    executable_dirs()
        .into_iter()
        .map(|dir| dir.join(&name))
        .find(|path| path.exists())
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "program not found"))
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

fn wide(text: &str) -> io::Result<Vec<u16>> {
    if text.contains('\0') {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "NUL in command argument"));
    }
    Ok(text.encode_utf16().chain([0]).collect())
}

fn append(line: &mut String, arg: &str, batch: bool) -> io::Result<()> {
    if batch && arg.contains(['\r', '\n']) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "newline in batch argument"));
    }
    append_validated(line, arg, batch);
    Ok(())
}

fn needs_batch_quotes(ch: char) -> bool {
    ch.is_ascii() && !ch.is_ascii_alphanumeric() && !r"#$*+-./:?@\_".contains(ch)
}

fn needs_quotes(arg: &str, batch: bool) -> bool {
    arg.is_empty()
        || if batch {
            arg.chars().any(needs_batch_quotes)
        } else {
            arg.contains([' ', '\t', '"'])
        }
}

fn append_validated(line: &mut String, arg: &str, batch: bool) {
    let quoted = needs_quotes(arg, batch);
    if quoted {
        line.push('"');
    }
    let mut slashes = 0;
    for ch in arg.chars() {
        if ch == '\\' {
            slashes += 1;
            continue;
        }
        append_character(line, ch, slashes, batch);
        slashes = 0;
    }
    append_end(line, slashes, quoted);
}

fn append_end(line: &mut String, slashes: usize, quoted: bool) {
    line.extend(std::iter::repeat_n('\\', if quoted { 2 * slashes } else { slashes }));
    if quoted {
        line.push('"');
    }
}

fn append_character(line: &mut String, ch: char, slashes: usize, batch: bool) {
    if ch == '"' {
        line.extend(std::iter::repeat_n('\\', 2 * slashes));
        line.push(if batch { '"' } else { '\\' });
    } else {
        line.extend(std::iter::repeat_n('\\', slashes));
    }
    if batch && ch == '%' {
        // cmd expands the empty substring of its built-in CD variable, leaving the literal percent.
        line.push_str("%%cd:~,");
    }
    line.push(ch);
}
