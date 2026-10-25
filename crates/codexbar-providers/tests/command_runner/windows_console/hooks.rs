//! Current-desktop, out-of-context window events. No DLL injection or window contents.
//! The callback only queues API event tuples. The observer discards foreign tuples before persistence.
use super::*;
use std::cell::{Cell, RefCell};
use windows::Win32::System::SystemInformation::GetTickCount64;
use windows::Win32::UI::Accessibility::{HWINEVENTHOOK, SetWinEventHook, UnhookWinEvent};
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, EVENT_OBJECT_CREATE, EVENT_OBJECT_HIDE, MSG, OBJID_WINDOW, PM_REMOVE, PeekMessageW,
    WINEVENT_OUTOFCONTEXT,
};

pub(super) const CAP: usize = 8192;
pub(super) struct Event {
    pub hwnd: HWND,
    pub kind: u32,
    pub thread: u32,
    pub generated_ms: u32,
    pub lag_ms: u32,
    pub received_ms: u64,
    pub sequence: u64,
}
thread_local! {
    static EVENTS: RefCell<Vec<Event>> = const { RefCell::new(Vec::new()) };
    static LOST: Cell<bool> = const { Cell::new(false) };
    static SEQUENCE: Cell<u64> = const { Cell::new(0) };
}
unsafe extern "system" fn callback(
    _: HWINEVENTHOOK,
    kind: u32,
    hwnd: HWND,
    object: i32,
    child: i32,
    thread: u32,
    generated_ms: u32,
) {
    if object != OBJID_WINDOW.0 || child != 0 || hwnd.0.is_null() {
        return;
    }
    // No recursive observer borrow across a Windows call. Reentrancy/overflow is a measured failure.
    EVENTS.with(|queue| {
        let Ok(mut queue) = queue.try_borrow_mut() else {
            LOST.with(|lost| lost.set(true));
            return;
        };
        if queue.len() == CAP {
            LOST.with(|lost| lost.set(true));
            return;
        }
        let received_ms = unsafe { GetTickCount64() };
        let sequence = SEQUENCE.with(|counter| {
            let next = counter.get() + 1;
            counter.set(next);
            next
        });
        queue.push(Event {
            hwnd,
            kind,
            thread,
            generated_ms,
            lag_ms: (received_ms as u32).wrapping_sub(generated_ms),
            received_ms,
            sequence,
        });
    });
}

pub(super) struct Hook(Option<HWINEVENTHOOK>);
impl Hook {
    pub fn install() -> std::result::Result<Self, String> {
        EVENTS.with(|queue| queue.borrow_mut().clear());
        LOST.with(|lost| lost.set(false));
        SEQUENCE.with(|counter| counter.set(0));
        let hook = unsafe {
            SetWinEventHook(
                EVENT_OBJECT_CREATE,
                EVENT_OBJECT_HIDE,
                None,
                Some(callback),
                0,
                0,
                WINEVENT_OUTOFCONTEXT,
            )
        };
        if hook.0.is_null() {
            return Err(format!(
                "SetWinEventHook: {}",
                unsafe { windows::Win32::Foundation::GetLastError() }.0
            ));
        }
        Ok(Self(Some(hook)))
    }
    pub fn close(&mut self) -> std::result::Result<(), String> {
        if let Some(hook) = self.0.take()
            && !unsafe { UnhookWinEvent(hook) }.as_bool()
        {
            return Err(format!(
                "UnhookWinEvent: {}",
                unsafe { windows::Win32::Foundation::GetLastError() }.0
            ));
        }
        Ok(())
    }
}
impl Drop for Hook {
    fn drop(&mut self) {
        let _ = self.close();
    }
}

pub(super) fn pump() -> std::result::Result<Vec<Event>, String> {
    let mut message = MSG::default();
    let mut count = 0;
    while unsafe { PeekMessageW(&mut message, None, 0, 0, PM_REMOVE) }.as_bool() {
        count += 1;
        if count > CAP {
            return Err("observer message cap exceeded".into());
        }
        unsafe { DispatchMessageW(&message) };
    }
    if LOST.with(Cell::get) {
        return Err("observer callback reentrancy/queue overflow lost events".into());
    }
    Ok(EVENTS.with(|queue| std::mem::take(&mut *queue.borrow_mut())))
}
