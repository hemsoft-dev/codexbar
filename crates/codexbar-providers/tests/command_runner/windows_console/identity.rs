//! Read only the synthetic fixture's existing primary token. No credential store or account is contacted.
use std::os::windows::io::{AsRawHandle as _, FromRawHandle as _, OwnedHandle};
use windows::Win32::Foundation::HANDLE;
use windows::Win32::Security::{
    GetLengthSid, GetTokenInformation, TOKEN_QUERY, TOKEN_STATISTICS, TOKEN_USER, TokenSessionId, TokenStatistics,
    TokenUser,
};
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

pub(super) fn current() -> serde_json::Value {
    let mut raw = HANDLE::default();
    // SAFETY: read-only access to this process's primary token; the returned handle has one RAII owner.
    unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut raw) }.unwrap();
    let token = unsafe { OwnedHandle::from_raw_handle(raw.0) };
    let raw = HANDLE(token.as_raw_handle());
    let mut buffer = [0usize; 256];
    let mut length = 0;
    let mut statistics = TOKEN_STATISTICS::default();
    let mut session = 0u32;
    // SAFETY: aligned storage larger than TOKEN_USER plus maximum SID size; exact sized outputs for other classes.
    unsafe {
        GetTokenInformation(
            raw,
            TokenUser,
            Some(buffer.as_mut_ptr().cast()),
            std::mem::size_of_val(&buffer) as u32,
            &mut length,
        )
        .unwrap();
        GetTokenInformation(
            raw,
            TokenStatistics,
            Some(std::ptr::from_mut(&mut statistics).cast()),
            std::mem::size_of_val(&statistics) as u32,
            &mut length,
        )
        .unwrap();
        GetTokenInformation(
            raw,
            TokenSessionId,
            Some(std::ptr::from_mut(&mut session).cast()),
            std::mem::size_of_val(&session) as u32,
            &mut length,
        )
        .unwrap();
        let user = &*buffer.as_ptr().cast::<TOKEN_USER>();
        let sid = std::slice::from_raw_parts(user.User.Sid.0.cast::<u8>(), GetLengthSid(user.User.Sid) as usize);
        serde_json::json!({"user_sid": sid, "session": session,
            "logon_low": statistics.AuthenticationId.LowPart, "logon_high": statistics.AuthenticationId.HighPart})
    }
}
