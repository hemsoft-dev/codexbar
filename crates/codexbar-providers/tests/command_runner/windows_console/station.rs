use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::StationsAndDesktops::{
    GetProcessWindowStation, GetUserObjectInformationW, UOI_FLAGS, UOI_NAME, USEROBJECTFLAGS,
};

pub(super) fn current() -> serde_json::Value {
    let mut name = [0u16; 256];
    let mut flags = USEROBJECTFLAGS::default();
    // SAFETY: read-only queries of our existing window station, with sized writable output buffers.
    unsafe {
        let station = GetProcessWindowStation().unwrap();
        GetUserObjectInformationW(
            HANDLE(station.0),
            UOI_NAME,
            Some(name.as_mut_ptr().cast()),
            std::mem::size_of_val(&name) as u32,
            None,
        )
        .unwrap();
        GetUserObjectInformationW(
            HANDLE(station.0),
            UOI_FLAGS,
            Some(std::ptr::from_mut(&mut flags).cast()),
            std::mem::size_of_val(&flags) as u32,
            None,
        )
        .unwrap();
    }
    serde_json::json!({"name": String::from_utf16(&name[..name.iter().position(|ch| *ch == 0).unwrap()]).unwrap(),
        "flags": flags.dwFlags, "visible": flags.dwFlags & 1 != 0}) // WSF_VISIBLE
}
