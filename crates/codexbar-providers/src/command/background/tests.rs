use super::*;
use std::os::windows::io::BorrowedHandle;
use windows::Win32::Foundation::CompareObjectHandles;

#[test]
fn abandoning_a_suspended_launch_terminates_the_retained_process_before_provider_code_runs() {
    let executable = std::env::current_exe().unwrap();
    let mut plan = arguments::command_line(executable.to_str().unwrap(), &["--list"]).unwrap();
    let mut streams = stdio::Prepared::new().unwrap();
    let suspended = Suspended::create(&mut plan, &mut streams).unwrap();
    let retained = suspended.child.as_ref().unwrap().process.try_clone().unwrap();
    assert_eq!(
        unsafe { WaitForSingleObject(HANDLE(retained.as_raw_handle()), 0) },
        WAIT_TIMEOUT
    );
    drop(streams);
    drop(suspended);
    assert_eq!(
        unsafe { WaitForSingleObject(HANDLE(retained.as_raw_handle()), 0) },
        WAIT_OBJECT_0
    );
}

#[test]
fn failed_native_creation_releases_pipes_attributes_handles_and_the_launch_guard() {
    // An explicit missing EXE reaches native creation after stdio and the whitelist have been prepared.
    let missing = std::env::temp_dir()
        .join("codexbar-native-missing-271")
        .join("missing.exe");
    assert!(!missing.exists());
    let mut plan = arguments::command_line(missing.to_str().unwrap(), &[]).unwrap();
    let mut streams = stdio::Prepared::new().unwrap();
    let startup = streams.startup();
    let originals = [
        startup.StartupInfo.hStdInput,
        startup.StartupInfo.hStdOutput,
        startup.StartupInfo.hStdError,
    ];
    // Retain object identity so another unit test reusing a released handle value cannot affect this proof.
    let retained: Vec<_> = originals
        .iter()
        .map(|handle| {
            // SAFETY: streams owns each live handle for the full duplication call.
            unsafe { BorrowedHandle::borrow_raw(handle.0) }
                .try_clone_to_owned()
                .unwrap()
        })
        .collect();
    for (handle, retained) in originals.iter().zip(&retained) {
        assert!(unsafe { CompareObjectHandles(*handle, HANDLE(retained.as_raw_handle())) }.as_bool());
    }
    assert_eq!(
        Suspended::create(&mut plan, &mut streams).err().unwrap().kind(),
        io::ErrorKind::NotFound
    );
    drop(streams);
    for (handle, retained) in originals.iter().zip(&retained) {
        // SAFETY: read-only comparison; a closed/reused value must no longer refer to the retained original object.
        assert!(!unsafe { CompareObjectHandles(*handle, HANDLE(retained.as_raw_handle())) }.as_bool());
    }
    drop(crate::platform::spawn_guard());
}
