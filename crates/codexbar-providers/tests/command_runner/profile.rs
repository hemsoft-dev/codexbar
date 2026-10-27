//! Instrumented-build controls for the LLVM exit-time profile write of a synthetic overflow child (#275).
//!
//! Each helper diverts only its own raw profile into a stdio stream whose write callback is the runtime's
//! actual write boundary. The final overflow byte goes through the real `SystemCommandRunner`, which
//! kills the process group. A write that finishes first stays valid. A write still in progress is cut
//! off. The fixture's `held` child never returns into its exit handler, so it never starts a write. That
//! control stops exactly at the cap: only the deadline can end it, while an exiting child would finish.
use codexbar_providers::{CommandError, CommandOptions, CommandRunner, SystemCommandRunner};
use std::ffi::{CString, c_char, c_int, c_void};
use std::fs::File;
use std::io::Write as _;
use std::path::Path;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

const CAP: usize = 4 * super::MIB;
/// Raw profile version 11: 19 u64 fields (`INSTR_PROF_RAW_HEADER` in LLVM's InstrProfData.inc).
const HEADER: usize = 19 * 8;
const MAGIC: u64 = 0xff6c_7072_6f66_7281;
const VERSION: u64 = 11;
/// `ValueKindLast` 2 gives the 72-byte `INSTR_PROF_DATA` record: seven u64 fields, u32, u16[3], u16, u32.
const VALUE_KIND_LAST: u64 = 2;
const DATA_RECORD: u64 = 72;
const VTABLE_RECORD: u64 = 24;
const BYTE_COVERAGE: u64 = 1 << 60;

unsafe extern "C" {
    fn __llvm_profile_set_filename(name: *const c_char);
    fn __llvm_profile_set_file_object(file: *mut libc::FILE, merge: c_int) -> c_int;
    fn __llvm_profile_write_file() -> c_int;
}

struct Sink {
    profile: std::path::PathBuf,
    trace: File,
}
static SINK: OnceLock<Sink> = OnceLock::new();
static PROFILE: OnceLock<File> = OnceLock::new();
static POSITION: AtomicU64 = AtomicU64::new(0);
static END: AtomicU64 = AtomicU64::new(0);
static RELEASE_IN_WRITE: AtomicBool = AtomicBool::new(false);

/// Child side. Records only this helper's own order and profile write sequence.
pub(super) fn helper(dir: &Path, order: &str) -> ! {
    let _held = super::hold(&dir.join(order));
    divert(dir, order);
    match order {
        "written" => {
            super::emit("stdout", CAP);
            // SAFETY: the runtime's public writer, called before the helper can reach its exit handler.
            assert_eq!(unsafe { __llvm_profile_write_file() }, 0);
            trace("written");
            release();
        }
        "interrupted" => {
            super::emit("stdout", CAP);
            RELEASE_IN_WRITE.store(true, Ordering::SeqCst);
            std::process::exit(0);
        }
        "held" => super::held("stdout", CAP),
        _ => panic!("unknown profile order"),
    }
    super::idle()
}

fn divert(dir: &Path, order: &str) {
    let profile = dir.join(format!("{order}.profraw"));
    // A name without %m turns off online merging, so the runtime writes to the stream set below.
    let name = CString::new(profile.to_str().unwrap()).unwrap();
    let sink = Sink {
        profile,
        trace: File::create_new(dir.join(format!("{order}.trace"))).unwrap(),
    };
    assert!(SINK.set(sink).is_ok());
    // SAFETY: a valid C string. The stream stays open for the life of the process, and only this helper's
    // own profile runtime writes to it.
    unsafe {
        __llvm_profile_set_filename(name.as_ptr());
        let stream = open_stream();
        assert!(!stream.is_null());
        // Unbuffered: each runtime fwrite reaches the callback at its own boundary.
        assert_eq!(libc::setvbuf(stream, std::ptr::null_mut(), libc::_IONBF, 0), 0);
        assert_eq!(__llvm_profile_set_file_object(stream, 0), 0);
    }
    trace("diverted");
}

#[cfg(target_os = "linux")]
unsafe fn open_stream() -> *mut libc::FILE {
    #[repr(C)]
    struct CookieIo {
        read: *const c_void,
        write: extern "C" fn(*mut c_void, *const c_char, usize) -> isize,
        seek: extern "C" fn(*mut c_void, *mut i64, c_int) -> c_int,
        close: *const c_void,
    }
    unsafe extern "C" {
        fn fopencookie(cookie: *mut c_void, mode: *const c_char, io: CookieIo) -> *mut libc::FILE;
    }
    extern "C" fn write(_: *mut c_void, bytes: *const c_char, len: usize) -> isize {
        // SAFETY: stdio passes `len` readable bytes.
        boundary(unsafe { std::slice::from_raw_parts(bytes.cast(), len) });
        len as isize
    }
    extern "C" fn seek(_: *mut c_void, offset: *mut i64, whence: c_int) -> c_int {
        // SAFETY: glibc passes a valid offset to read and update.
        let offset = unsafe { &mut *offset };
        match position(*offset, whence) {
            Some(target) => {
                *offset = target as i64;
                0
            }
            None => -1,
        }
    }
    let io = CookieIo {
        read: std::ptr::null(),
        write,
        seek,
        close: std::ptr::null(),
    };
    // SAFETY: glibc fopencookie with a static mode and a callback table without read or close.
    unsafe { fopencookie(std::ptr::null_mut(), c"w".as_ptr(), io) }
}

#[cfg(target_os = "macos")]
unsafe fn open_stream() -> *mut libc::FILE {
    unsafe extern "C" {
        fn funopen(
            cookie: *const c_void,
            read: *const c_void,
            write: extern "C" fn(*mut c_void, *const c_char, c_int) -> c_int,
            seek: extern "C" fn(*mut c_void, i64, c_int) -> i64,
            close: *const c_void,
        ) -> *mut libc::FILE;
    }
    extern "C" fn write(_: *mut c_void, bytes: *const c_char, len: c_int) -> c_int {
        // SAFETY: stdio passes `len` readable bytes.
        boundary(unsafe { std::slice::from_raw_parts(bytes.cast(), len as usize) });
        len
    }
    extern "C" fn seek(_: *mut c_void, offset: i64, whence: c_int) -> i64 {
        position(offset, whence).map_or(-1, |target| target as i64)
    }
    // SAFETY: BSD funopen with a callback table without read or close.
    unsafe { funopen(std::ptr::null(), std::ptr::null(), write, seek, std::ptr::null()) }
}

/// The runtime skips empty sections with relative seeks, so the stream keeps a real position.
fn position(offset: i64, whence: c_int) -> Option<u64> {
    let base = match whence {
        libc::SEEK_SET => 0,
        libc::SEEK_CUR => POSITION.load(Ordering::SeqCst),
        libc::SEEK_END => END.load(Ordering::SeqCst),
        _ => return None,
    };
    let target = base.checked_add_signed(offset)?;
    let from = POSITION.swap(target, Ordering::SeqCst);
    trace(&format!("seek {from} {target}"));
    Some(target)
}

/// One runtime write. The interrupted order releases the final byte once the header is complete, then
/// stays inside the runtime's write until the runner kills the group.
fn boundary(bytes: &[u8]) {
    use std::os::unix::fs::FileExt as _;
    let sink = SINK.get().unwrap();
    let profile = PROFILE.get_or_init(|| File::create_new(&sink.profile).unwrap());
    let start = POSITION.fetch_add(bytes.len() as u64, Ordering::SeqCst);
    profile.write_all_at(bytes, start).unwrap();
    let end = END
        .fetch_max(start + bytes.len() as u64, Ordering::SeqCst)
        .max(start + bytes.len() as u64);
    trace(&format!("write {start} {}", bytes.len()));
    if end >= HEADER as u64 && RELEASE_IN_WRITE.swap(false, Ordering::SeqCst) {
        trace(&format!("release {end}"));
        release();
        super::idle();
    }
}

fn trace(event: &str) {
    // One write per event, so the kernel keeps every completed event across SIGKILL.
    (&SINK.get().unwrap().trace)
        .write_all(format!("{event}\n").as_bytes())
        .unwrap();
}

fn release() {
    // SAFETY: one byte from a static buffer to this helper's captured stdout, outside Rust's stdout buffer.
    assert_eq!(unsafe { libc::write(1, b"X".as_ptr().cast(), 1) }, 1);
}

#[derive(Debug, PartialEq, Eq)]
enum Raw {
    Complete,
    Truncated { declared: u64 },
}

/// Strictly parses one raw profile. The declared sections must fit the file exactly, matching the reader's
/// bad_header bound check. Value-profile data, which Rust coverage does not emit, is rejected.
fn parse(bytes: &[u8]) -> Result<Raw, String> {
    if bytes.len() < HEADER {
        return Err(format!("incomplete {}-byte header", bytes.len()));
    }
    let field: Vec<u64> = bytes[..HEADER]
        .as_chunks::<8>()
        .0
        .iter()
        .map(|chunk| u64::from_le_bytes(*chunk))
        .collect();
    if field[0] != MAGIC || field[1] & 0xffff_ffff != VERSION || field[18] != VALUE_KIND_LAST {
        return Err(format!("unsupported header {:x?}", &field[..2]));
    }
    let counter = if field[1] & BYTE_COVERAGE == 0 { 8 } else { 1 };
    let pad = |size: u64| (8 - size % 8) % 8;
    let sections = [
        field[2],
        field[3] * DATA_RECORD,
        field[4],
        field[5] * counter,
        field[6],
        field[7],
        field[8],
        field[9] * 8,
        field[10],
        field[12],
        pad(field[12]),
        field[16] * VTABLE_RECORD,
        pad(field[16] * VTABLE_RECORD),
        field[17],
        pad(field[17]),
    ];
    let declared = sections
        .iter()
        .try_fold(HEADER as u64, |sum, size| sum.checked_add(*size));
    let declared = declared.ok_or("section sizes overflow")?;
    match (bytes.len() as u64).cmp(&declared) {
        std::cmp::Ordering::Less => Ok(Raw::Truncated { declared }),
        std::cmp::Ordering::Equal if field[3] > 0 && field[5] > 0 => Ok(Raw::Complete),
        std::cmp::Ordering::Equal => Err("profile without data or counters".into()),
        std::cmp::Ordering::Greater => Err(format!(
            "{} bytes after the declared sections",
            bytes.len() as u64 - declared
        )),
    }
}

/// Parent side. Each order runs once through the real runner; every profile it produced is parsed.
pub(super) fn qualify(program: &str, scratch: &Path) {
    assert_eq!(
        parse(&[0; HEADER - 1]),
        Err(format!("incomplete {}-byte header", HEADER - 1))
    );
    for (order, timeout, expected) in [
        ("written", Duration::from_secs(3), CommandError::Failed),
        ("interrupted", Duration::from_secs(3), CommandError::Failed),
        ("held", Duration::from_secs(1), CommandError::TimedOut),
    ] {
        let dir = scratch.join(format!("profile-{order}"));
        std::fs::create_dir(&dir).unwrap();
        let options = CommandOptions {
            cwd: Some(dir.clone()),
            remove_env: Vec::new(),
            env: Vec::new(),
            output_limit: CAP,
            stop_on_stderr: None,
        };
        let start = Instant::now();
        let result = SystemCommandRunner.run_with_options(
            program,
            &["profile", dir.to_str().unwrap(), order],
            timeout,
            &options,
        );
        let elapsed = start.elapsed();
        let pid = std::fs::read_to_string(dir.join(format!("{order}.ready"))).unwrap();
        // Overflow ends the boundary orders, only the deadline ends `held`, and every idling helper is gone.
        let summary = result
            .as_ref()
            .map(|output| (output.success, output.stdout.len(), &output.stderr));
        assert_eq!(result.as_ref().err(), Some(&expected), "{order}: {summary:?}");
        super::released(&dir.join(order));
        let trace = std::fs::read_to_string(dir.join(format!("{order}.trace"))).unwrap();
        let events: Vec<&str> = trace.lines().collect();
        let mut profiles = Vec::new();
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().is_some_and(|extension| extension == "profraw") {
                profiles.push((path.file_name().unwrap().to_owned(), std::fs::read(&path).unwrap()));
            }
        }
        let parsed: Vec<_> = profiles
            .iter()
            .map(|(name, bytes)| (name, bytes.len(), parse(bytes)))
            .collect();
        println!(
            "qualify profile boundary: {order} pid={pid} result={expected:?} elapsed_ms={} events={events:?} profiles={parsed:?}",
            elapsed.as_millis()
        );
        // The furthest byte any traced write reached.
        let writes: usize = events
            .iter()
            .filter_map(|event| event.strip_prefix("write "))
            .map(|write| write.split(' ').map(|n| n.parse::<usize>().unwrap()).sum::<usize>())
            .max()
            .unwrap_or(0);
        match order {
            "written" => {
                assert_eq!(events.first(), Some(&"diverted"));
                assert_eq!(events.last(), Some(&"written"));
                assert!(writes > HEADER && !trace.contains("release"));
                assert_eq!(parsed.len(), 1);
                assert_eq!((parsed[0].1, &parsed[0].2), (writes, &Ok(Raw::Complete)));
            }
            "interrupted" => {
                assert_eq!(events.first(), Some(&"diverted"));
                assert_eq!(events.last(), Some(&format!("release {writes}").as_str()));
                assert_eq!(parsed.len(), 1);
                assert_eq!(parsed[0].1, writes);
                assert!(
                    matches!(parsed[0].2, Ok(Raw::Truncated { declared }) if declared > writes as u64),
                    "the runtime's own header must declare sections the kill cut off"
                );
            }
            _ => {
                assert_eq!(events, ["diverted"], "no profile write may start before the kill");
                assert!(parsed.is_empty());
            }
        }
    }
}
