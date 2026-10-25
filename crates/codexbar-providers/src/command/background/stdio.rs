//! Own the standard streams, whitelist memory and launch guard through the complete inheritance interval.
use std::fs::{File, OpenOptions};
use std::io;
use std::os::windows::io::{AsRawHandle as _, FromRawHandle as _};
use std::sync::MutexGuard;
use windows::Win32::Foundation::{HANDLE, HANDLE_FLAG_INHERIT, SetHandleInformation};
use windows::Win32::Security::SECURITY_ATTRIBUTES;
use windows::Win32::System::Pipes::CreatePipe;
use windows::Win32::System::Threading::{
    DeleteProcThreadAttributeList, InitializeProcThreadAttributeList, LPPROC_THREAD_ATTRIBUTE_LIST,
    PROC_THREAD_ATTRIBUTE_HANDLE_LIST, STARTF_USESHOWWINDOW, STARTF_USESTDHANDLES, STARTUPINFOEXW,
    UpdateProcThreadAttribute,
};

// Field order matters on error: delete attributes while their handles remain valid, then close streams,
// then release the launch guard. Handle-list memory is owned by Attributes throughout its native lifetime.
pub(super) struct Prepared {
    attributes: Attributes,
    streams: Streams,
    guard: MutexGuard<'static, ()>,
}

impl Prepared {
    pub(super) fn new() -> io::Result<Self> {
        let guard = crate::platform::spawn_guard();
        let streams = Streams::new()?;
        let attributes = streams.inherit()?;
        Ok(Self {
            streams,
            attributes,
            guard,
        })
    }

    pub(super) fn startup(&mut self) -> STARTUPINFOEXW {
        let handles = self.streams.handles();
        let mut startup = STARTUPINFOEXW::default();
        startup.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
        startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES | STARTF_USESHOWWINDOW;
        startup.StartupInfo.wShowWindow = 0; // SW_HIDE at creation; no later ShowWindow call.
        startup.StartupInfo.hStdInput = handles[0];
        startup.StartupInfo.hStdOutput = handles[1];
        startup.StartupInfo.hStdError = handles[2];
        startup.lpAttributeList = self.attributes.pointer();
        startup
    }

    pub(super) fn capture(self) -> (File, File) {
        let Self {
            streams,
            attributes,
            guard,
        } = self;
        let Streams {
            input,
            stdout,
            out,
            stderr,
            err,
        } = streams;
        drop((attributes, input, out, err));
        drop(guard);
        (stdout, stderr)
    }
}

struct Streams {
    input: File,
    stdout: File,
    out: File,
    stderr: File,
    err: File,
}
impl Streams {
    fn new() -> io::Result<Self> {
        let input = OpenOptions::new().read(true).open("NUL")?;
        let (stdout, out) = pipe()?;
        let (stderr, err) = pipe()?;
        Ok(Self {
            input,
            stdout,
            out,
            stderr,
            err,
        })
    }

    fn handles(&self) -> [HANDLE; 3] {
        [raw(&self.input), raw(&self.out), raw(&self.err)]
    }

    fn inherit(&self) -> io::Result<Attributes> {
        let handles = self.handles();
        for handle in handles {
            // SAFETY: only these owned child-side handles become inheritable. Parent read ends never do.
            unsafe { SetHandleInformation(handle, HANDLE_FLAG_INHERIT.0, HANDLE_FLAG_INHERIT) }
                .map_err(io::Error::other)?;
        }
        Attributes::handles(handles)
    }
}

fn raw(file: &File) -> HANDLE {
    HANDLE(file.as_raw_handle())
}

fn pipe() -> io::Result<(File, File)> {
    let mut read = HANDLE::default();
    let mut write = HANDLE::default();
    let security = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        ..Default::default()
    };
    // SAFETY: writable output handles; default security and noninheritable until explicitly selected above.
    unsafe { CreatePipe(&mut read, &mut write, Some(&security), 0) }.map_err(io::Error::other)?;
    // SAFETY: successful CreatePipe returned distinct owned file handles, each closed once by File.
    Ok(unsafe { (File::from_raw_handle(read.0), File::from_raw_handle(write.0)) })
}

/// Own both aligned attribute storage and stable handle-list memory until process creation finishes.
struct Attributes {
    storage: Vec<usize>,
    handles: Box<[HANDLE; 3]>,
}
impl Attributes {
    fn handles(handles: [HANDLE; 3]) -> io::Result<Self> {
        let handles = Box::new(handles);
        let mut bytes = 0;
        // SAFETY: documented sizing query. Failure with the required byte count is expected.
        let _ = unsafe { InitializeProcThreadAttributeList(None, 1, None, &mut bytes) };
        if bytes == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut storage = vec![0; bytes.div_ceil(std::mem::size_of::<usize>())];
        let pointer = LPPROC_THREAD_ATTRIBUTE_LIST(storage.as_mut_ptr().cast());
        // SAFETY: sufficiently sized and aligned writable storage. Only construct the RAII owner on success.
        unsafe { InitializeProcThreadAttributeList(Some(pointer), 1, None, &mut bytes) }.map_err(io::Error::other)?;
        let mut attributes = Self { storage, handles };
        // SAFETY: initialized list and a live exact-size array of inheritable handles, no pseudo handles.
        unsafe {
            UpdateProcThreadAttribute(
                attributes.pointer(),
                0,
                PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
                Some(attributes.handles.as_ptr().cast()),
                std::mem::size_of_val(attributes.handles.as_ref()),
                None,
                None,
            )
        }
        .map_err(io::Error::other)?;
        Ok(attributes)
    }

    fn pointer(&mut self) -> LPPROC_THREAD_ATTRIBUTE_LIST {
        LPPROC_THREAD_ATTRIBUTE_LIST(self.storage.as_mut_ptr().cast())
    }
}
impl Drop for Attributes {
    fn drop(&mut self) {
        // SAFETY: the initialized list is deleted before its backing allocation is freed.
        unsafe { DeleteProcThreadAttributeList(self.pointer()) };
    }
}
