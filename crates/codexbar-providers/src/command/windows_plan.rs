//! Portable Windows launch policy: executable selection, CRT/batch argument encoding and native buffer bounds.
//! This is production Windows policy, also compiled on other hosts for credential-free behavioral tests.
use std::io;
use std::path::{Path, PathBuf};

pub(super) struct LaunchPlan {
    pub application: Vec<u16>,
    pub line: Vec<u16>,
}

impl LaunchPlan {
    pub(super) fn new(
        resolved: String,
        args: &[&str],
        interpreter: impl FnOnce() -> io::Result<String>,
    ) -> io::Result<Self> {
        let lower = resolved.to_ascii_lowercase();
        let batch = lower.ends_with(".cmd") || lower.ends_with(".bat");
        let (application, line) = if batch {
            (interpreter()?, format!("cmd.exe /e:ON /v:OFF /d /c \"\"{resolved}\""))
        } else {
            (resolved.clone(), format!("\"{resolved}\""))
        };
        Ok(Self {
            application: wide(&application)?,
            line: finish_line(line, args, batch)?,
        })
    }
}

/// Search only the supplied application/system/PATH directories, never an implicit working directory.
/// Explicit paths prefer an existing `.exe` suffix as Rust Command does, then retain the original path.
pub(super) fn resolve(program: &str, directories: &[PathBuf], exists: impl Fn(&Path) -> bool) -> io::Result<String> {
    wide(program)?;
    let name = program.rsplit(['\\', '/']).next().unwrap();
    if name.is_empty() || matches!(name, "." | "..") || name.ends_with(':') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "program path has no file name",
        ));
    }
    let path = if program.contains(['\\', '/', ':']) {
        explicit_path(program, &exists)
    } else {
        let name = if program.contains('.') {
            program.to_owned()
        } else {
            format!("{program}.exe")
        };
        directories
            .iter()
            .map(|dir| dir.join(&name))
            .find(|path| exists(path))
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "program not found"))?
    };
    path.into_os_string()
        .into_string()
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "program path is not Unicode"))
}

fn explicit_path(program: &str, exists: &impl Fn(&Path) -> bool) -> PathBuf {
    let path = PathBuf::from(program);
    if program.to_ascii_lowercase().ends_with(".exe") {
        return path;
    }
    let mut suffix = path.as_os_str().to_owned();
    suffix.push(".exe");
    let suffix = PathBuf::from(suffix);
    if exists(&suffix) { suffix } else { path }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn decoded(buffer: &[u16]) -> String {
        assert_eq!(buffer.last(), Some(&0));
        assert!(!buffer[..buffer.len() - 1].contains(&0));
        String::from_utf16(&buffer[..buffer.len() - 1]).unwrap()
    }

    fn plan(program: &str, args: &[&str]) -> io::Result<LaunchPlan> {
        LaunchPlan::new(program.to_owned(), args, || {
            Ok(r"C:\Windows\System32\cmd.exe".to_owned())
        })
    }

    #[test]
    fn executable_identity_uses_only_the_ordered_application_system_and_path_candidates() {
        let dirs = ["application", "system", "windows", "user-path"].map(PathBuf::from);
        let user = dirs[3].join("tool.exe");
        assert_eq!(
            resolve("tool", &dirs, |path| path == user).unwrap(),
            user.to_str().unwrap()
        );
        let application = dirs[0].join("tool.exe");
        assert_eq!(
            resolve("tool", &dirs, |path| path == user || path == application).unwrap(),
            application.to_str().unwrap()
        );
        let system = dirs[1].join("tool.exe");
        assert_eq!(
            resolve("tool.exe", &dirs, |path| path == user || path == system).unwrap(),
            system.to_str().unwrap()
        );
        let shim = dirs[3].join("tool.cmd");
        assert_eq!(
            resolve("tool.cmd", &dirs, |path| path == shim).unwrap(),
            shim.to_str().unwrap()
        );
        assert_eq!(
            resolve("tool", &dirs, |_| false).unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
        // There is no implicit current-directory probe, even if an executable happens to exist there.
        assert_eq!(
            resolve("tool", &[], |_| panic!("must not search the working directory"))
                .unwrap_err()
                .kind(),
            io::ErrorKind::NotFound
        );
    }

    #[test]
    fn explicit_windows_paths_do_not_search_or_redirect_an_exe_identity() {
        for program in [
            r"C:\tools\tool.EXE",
            r"\\server\share\tool.exe",
            r".\tool.exe",
            "./tool.exe",
        ] {
            assert_eq!(
                resolve(program, &[], |_| panic!(
                    "explicit exe must go directly to CreateProcessW"
                ))
                .unwrap(),
                program
            );
        }
        for program in [r"C:\tools\tool", r"C:tool", r".\tool.cmd", "./tool.bat"] {
            assert_eq!(resolve(program, &[], |_| false).unwrap(), program);
            let suffixed = format!("{program}.exe");
            assert_eq!(
                resolve(program, &[], |path| path == Path::new(&suffixed)).unwrap(),
                suffixed
            );
        }
    }

    #[test]
    fn invalid_program_paths_cannot_become_search_candidates() {
        for program in ["", ".", "..", "C:", r"C:\", "a/", "a\\", "a/..", "a\0b"] {
            assert_eq!(
                resolve(program, &[], |_| panic!("invalid program must not reach lookup"))
                    .unwrap_err()
                    .kind(),
                io::ErrorKind::InvalidInput
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn non_unicode_directory_is_rejected_instead_of_selecting_a_different_program() {
        use std::os::unix::ffi::OsStringExt as _;
        let directory = PathBuf::from(std::ffi::OsString::from_vec(vec![b'/', 0xff]));
        assert_eq!(
            resolve("tool", &[directory], |_| true).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
    }

    #[test]
    fn native_launch_keeps_usage_arguments_and_never_requests_an_interpreter() {
        let program = r"C:\Tools\provider.exe";
        let plan = LaunchPlan::new(
            program.to_owned(),
            &["-p", "/usage", "--output-format", "json", "--print-timeout", "25s"],
            || panic!("native launch must not discover or start cmd.exe"),
        )
        .unwrap();
        assert_eq!(decoded(&plan.application), program);
        assert_eq!(
            decoded(&plan.line),
            r#""C:\Tools\provider.exe" -p /usage --output-format json --print-timeout 25s"#
        );
        assert_eq!(decoded(&self::plan("app.exe", &[]).unwrap().line), "\"app.exe\"");
    }

    #[test]
    fn crt_arguments_preserve_empty_whitespace_quotes_backslashes_and_shell_characters() {
        // Golden encodings are independently exercised by the actual Windows argv fixture too.
        for (input, encoded) in [
            ("", r#""""#),
            ("two words", r#""two words""#),
            ("a\"b", r#""a\"b""#),
            (r#"x\"y"#, r#""x\\\"y""#),
            ("trailing space \\", "\"trailing space \\\\\""),
            (r"\", r"\"),
            ("&|<>()^", "&|<>()^"),
            ("%PATH%", "%PATH%"),
            ("tab\tvalue", "\"tab\tvalue\""),
            ("é漢😀", "é漢😀"),
            ("a\nb", "a\nb"),
        ] {
            assert_eq!(
                decoded(&plan("app.exe", &[input]).unwrap().line),
                format!("\"app.exe\" {encoded}")
            );
        }
    }

    #[test]
    fn batch_shims_select_the_system_interpreter_and_preserve_cmd_literal_rules() {
        for program in [r"C:\Tools\provider.CMD", r"C:\Tools\provider.bat"] {
            let launch = plan(
                program,
                &[
                    "-p",
                    "/usage",
                    "",
                    "two words",
                    "a\"b",
                    "%PATH%",
                    "!value!",
                    "&|<>()^",
                    "é漢😀",
                    "trailing space \\",
                ],
            )
            .unwrap();
            assert_eq!(decoded(&launch.application), r"C:\Windows\System32\cmd.exe");
            let expected_args = "-p /usage \"\" \"two words\" \"a\"\"b\" \"%%cd:~,%PATH%%cd:~,%\" \"!value!\" \"&|<>()^\" é漢😀 \"trailing space \\\\\"";
            assert_eq!(
                decoded(&launch.line),
                format!("cmd.exe /e:ON /v:OFF /d /c \"\"{program}\" {expected_args}\"")
            );
        }
    }

    #[test]
    fn invalid_arguments_interpreter_errors_and_native_limits_fail_before_process_creation() {
        for program in ["app.exe", "shim.cmd"] {
            assert_eq!(
                plan(program, &["a\0b"]).err().unwrap().kind(),
                io::ErrorKind::InvalidInput
            );
        }
        assert_eq!(
            plan("app\0.exe", &[]).err().unwrap().kind(),
            io::ErrorKind::InvalidInput
        );
        for value in ["a\nb", "a\rb"] {
            assert_eq!(
                plan("shim.cmd", &[value]).err().unwrap().kind(),
                io::ErrorKind::InvalidInput
            );
        }
        assert_eq!(
            LaunchPlan::new("shim.cmd".into(), &[], || Err(io::ErrorKind::PermissionDenied.into()))
                .err()
                .unwrap()
                .kind(),
            io::ErrorKind::PermissionDenied
        );
        assert_eq!(
            LaunchPlan::new("shim.cmd".into(), &[], || Ok("cmd\0.exe".into()))
                .err()
                .unwrap()
                .kind(),
            io::ErrorKind::InvalidInput
        );
        let exact = "a".repeat(32756);
        assert_eq!(plan("app.exe", &[&exact]).unwrap().line.len(), 32767);
        assert_eq!(
            plan("app.exe", &[&(exact + "a")]).err().unwrap().kind(),
            io::ErrorKind::InvalidInput
        );
        assert!(
            plan("app.exe", &[&"😀".repeat(16000)]).is_ok(),
            "limit counts UTF-16 units, not UTF-8 bytes"
        );
        assert_eq!(
            plan("app.exe", &[&"😀".repeat(16400)]).err().unwrap().kind(),
            io::ErrorKind::InvalidInput
        );
    }
}
