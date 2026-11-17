//! Isolated Win32 end-session adapter. No UI is shown; a real top-level window
//! receives session notifications even when user32 disables console logoff hooks.
#![allow(unsafe_code)]
use super::{ShutdownReason, State};
use std::ffi::c_void;
use std::ptr::{null, null_mut};
use std::sync::{Arc, Mutex, Weak, mpsc};
use std::thread::JoinHandle;

type Hwnd = *mut c_void;
#[repr(C)]
struct Point {
    x: i32,
    y: i32,
}
#[repr(C)]
struct Message {
    hwnd: Hwnd,
    message: u32,
    wparam: usize,
    lparam: isize,
    time: u32,
    point: Point,
    private: u32,
}
#[repr(C)]
struct WindowClass {
    style: u32,
    procedure: Option<unsafe extern "system" fn(Hwnd, u32, usize, isize) -> isize>,
    class_extra: i32,
    window_extra: i32,
    instance: *mut c_void,
    icon: *mut c_void,
    cursor: *mut c_void,
    background: *mut c_void,
    menu: *const u16,
    name: *const u16,
}
#[link(name = "user32")]
unsafe extern "system" {
    fn RegisterClassW(class: *const WindowClass) -> u16;
    fn UnregisterClassW(name: *const u16, instance: *mut c_void) -> i32;
    fn CreateWindowExW(
        ex_style: u32,
        class: *const u16,
        name: *const u16,
        style: u32,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
        parent: Hwnd,
        menu: *mut c_void,
        instance: *mut c_void,
        param: *const c_void,
    ) -> Hwnd;
    fn DefWindowProcW(window: Hwnd, message: u32, wparam: usize, lparam: isize) -> isize;
    fn GetMessageW(message: *mut Message, window: Hwnd, min: u32, max: u32) -> i32;
    fn TranslateMessage(message: *const Message) -> i32;
    fn DispatchMessageW(message: *const Message) -> isize;
    fn DestroyWindow(window: Hwnd) -> i32;
    fn PostMessageW(window: Hwnd, message: u32, wparam: usize, lparam: isize) -> i32;
    fn PostQuitMessage(code: i32);
    fn EnumWindows(
        callback: Option<unsafe extern "system" fn(Hwnd, isize) -> i32>,
        param: isize,
    ) -> i32;
    fn GetWindowThreadProcessId(window: Hwnd, pid: *mut u32) -> u32;
    fn GetClassNameW(window: Hwnd, class: *mut u16, count: i32) -> i32;
}
#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetModuleHandleW(name: *const u16) -> *mut c_void;
    fn SetConsoleCtrlHandler(
        handler: Option<unsafe extern "system" fn(u32) -> i32>,
        add: i32,
    ) -> i32;
}
const WM_CLOSE: u32 = 0x0010;
const WM_DESTROY: u32 = 0x0002;
const WM_QUERYENDSESSION: u32 = 0x0011;
const WM_ENDSESSION: u32 = 0x0016;
const WM_OWNED_SHUTDOWN: u32 = 0x8056;
static ACTIVE: Mutex<Weak<State>> = Mutex::new(Weak::new());
fn active() -> Option<Arc<State>> {
    ACTIVE.lock().unwrap_or_else(|p| p.into_inner()).upgrade()
}

unsafe extern "system" fn console(event: u32) -> i32 {
    let Some(state) = active() else {
        return 0;
    };
    match event {
        0 | 1 => state.notify_at(ShutdownReason::Interrupt, std::time::Instant::now()),
        2 => state.notify_and_wait(ShutdownReason::ConsoleClose),
        5 => state.notify_and_wait(ShutdownReason::Logoff),
        6 => state.notify_and_wait(ShutdownReason::Shutdown),
        _ => return 0,
    }
    1
}
unsafe extern "system" fn window(
    window: Hwnd,
    message: u32,
    wparam: usize,
    lparam: isize,
) -> isize {
    match message {
        // Allow shutdown; do not stop on the query because another application
        // or the user may cancel it. Only a confirmed ENDSESSION initiates stop.
        WM_QUERYENDSESSION => 1,
        WM_OWNED_SHUTDOWN => {
            if let Some(state) = active() {
                let reason = match wparam {
                    0 => ShutdownReason::Interrupt,
                    1 => ShutdownReason::Terminate,
                    _ => ShutdownReason::SessionEnding,
                };
                state.notify_at(reason, std::time::Instant::now());
            }
            0
        }
        WM_ENDSESSION => {
            if wparam != 0 {
                if let Some(state) = active() {
                    state.notify_and_wait(ShutdownReason::SessionEnding);
                }
            }
            0
        }
        WM_CLOSE => {
            // SAFETY: WM_CLOSE runs on this window's owning message thread.
            unsafe {
                DestroyWindow(window);
            }
            0
        }
        WM_DESTROY => {
            // SAFETY: only ends this adapter's owning message loop.
            unsafe {
                PostQuitMessage(0);
            }
            0
        }
        // SAFETY: these are the original callback arguments from Windows.
        _ => unsafe { DefWindowProcW(window, message, wparam, lparam) },
    }
}
pub(super) struct Registration {
    window: isize,
    thread: Option<JoinHandle<()>>,
    console: bool,
    state: Arc<State>,
}
impl Registration {
    pub(super) fn open(state: Arc<State>) -> Result<Self, &'static str> {
        {
            let mut slot = ACTIVE.lock().unwrap_or_else(|p| p.into_inner());
            if slot.upgrade().is_some() {
                return Err("shutdown_signal_already_registered");
            }
            *slot = Arc::downgrade(&state);
        }
        // SAFETY: console is a static callback valid for this process lifetime.
        // Detached processes may have no console; the hidden window still works.
        let console = unsafe { SetConsoleCtrlHandler(Some(console), 1) } != 0;
        let (ready, receiver) = mpsc::sync_channel(1);
        let thread = std::thread::Builder::new()
            .name("vesta-session-end".into())
            .spawn(move || {
                let name: Vec<u16> = format!("VestaShutdown{}", std::process::id())
                    .encode_utf16()
                    .chain(Some(0))
                    .collect();
                // SAFETY: null selects the current executable's loaded module.
                let instance = unsafe { GetModuleHandleW(null()) };
                let title: Vec<u16> = "Vesta Host".encode_utf16().chain(Some(0)).collect();
                let class = WindowClass {
                    style: 0,
                    procedure: Some(window),
                    class_extra: 0,
                    window_extra: 0,
                    instance,
                    icon: null_mut(),
                    cursor: null_mut(),
                    background: null_mut(),
                    menu: null(),
                    name: name.as_ptr(),
                };
                // SAFETY: class and null-terminated name stay alive through registration.
                if unsafe { RegisterClassW(&class) } == 0 {
                    let _ = ready.send(None);
                    return;
                }
                // SAFETY: this registered class's callback is static. Null parent makes
                // a top-level (not message-only) window eligible for ENDSESSION. No
                // visible style or ShowWindow call is used.
                let window = unsafe {
                    CreateWindowExW(
                        0,
                        name.as_ptr(),
                        title.as_ptr(),
                        0,
                        0,
                        0,
                        0,
                        0,
                        null_mut(),
                        null_mut(),
                        instance,
                        null(),
                    )
                };
                if window.is_null() {
                    let _ = ready.send(None);
                    // SAFETY: no windows were created for this class.
                    unsafe {
                        UnregisterClassW(name.as_ptr(), instance);
                    }
                    return;
                }
                if ready.send(Some(window as isize)).is_err() {
                    // SAFETY: owns this thread's newly created window.
                    unsafe {
                        DestroyWindow(window);
                    }
                } else {
                    let mut message = Message {
                        hwnd: null_mut(),
                        message: 0,
                        wparam: 0,
                        lparam: 0,
                        time: 0,
                        point: Point { x: 0, y: 0 },
                        private: 0,
                    };
                    // SAFETY: message points to writable storage with the Win32 MSG
                    // layout; dispatch is confined to the owning native thread.
                    while unsafe { GetMessageW(&mut message, null_mut(), 0, 0) } > 0 {
                        unsafe {
                            TranslateMessage(&message);
                            DispatchMessageW(&message);
                        }
                    }
                }
                // SAFETY: the loop has closed its window before class unregistration.
                unsafe {
                    UnregisterClassW(name.as_ptr(), instance);
                }
            });
        let thread = match thread {
            Ok(thread) => thread,
            Err(_) => {
                cleanup(console, &state);
                return Err("shutdown_signal_registration_failed");
            }
        };
        let window = match receiver.recv() {
            Ok(Some(window)) => window,
            _ => {
                let _ = thread.join();
                cleanup(console, &state);
                return Err("shutdown_signal_registration_failed");
            }
        };
        Ok(Self {
            window,
            thread: Some(thread),
            console,
            state,
        })
    }
}
fn cleanup(console_registered: bool, state: &Arc<State>) {
    if console_registered {
        // SAFETY: removes precisely the static callback registered above.
        unsafe {
            SetConsoleCtrlHandler(Some(console), 0);
        }
    }
    let mut slot = ACTIVE.lock().unwrap_or_else(|p| p.into_inner());
    if slot
        .upgrade()
        .is_some_and(|active| Arc::ptr_eq(&active, state))
    {
        *slot = Weak::new();
    }
}
impl Drop for Registration {
    fn drop(&mut self) {
        self.state.finish();
        // SAFETY: window was returned by this adapter's native thread and is
        // retained until that thread handles this close request and is joined.
        unsafe {
            PostMessageW(self.window as Hwnd, WM_CLOSE, 0, 0);
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        cleanup(self.console, &self.state);
    }
}

/// Only called with a still-unreaped Child's ID by the shared wait helper.
pub(super) fn notify_owned_child(pid: u32, reason: ShutdownReason) -> bool {
    struct Target {
        pid: u32,
        reason: ShutdownReason,
        notified: bool,
    }
    unsafe extern "system" fn visit(window: Hwnd, param: isize) -> i32 {
        // SAFETY: EnumWindows calls synchronously while Target remains on stack.
        let target = unsafe { &mut *(param as *mut Target) };
        let mut pid = 0;
        // SAFETY: pid is writable; callback HWND is valid for API observation.
        if unsafe { GetWindowThreadProcessId(window, &mut pid) } == 0 || pid != target.pid {
            return 1;
        }
        let mut class = [0u16; 64];
        // SAFETY: class has count writable UTF-16 code units.
        let count = unsafe { GetClassNameW(window, class.as_mut_ptr(), class.len() as i32) };
        let expected: Vec<u16> = format!("VestaShutdown{}", target.pid)
            .encode_utf16()
            .collect();
        if count <= 0 || class[..count as usize] != expected {
            return 1;
        }
        let reason = match target.reason {
            ShutdownReason::Interrupt => 0,
            ShutdownReason::Terminate => 1,
            _ => 2,
        };
        // SAFETY: target is the exact owned child's private shutdown window;
        // only a scalar reason is sent, with no cross-process pointer payload.
        target.notified = unsafe { PostMessageW(window, WM_OWNED_SHUTDOWN, reason, 0) } != 0;
        i32::from(!target.notified)
    }
    let mut target = Target {
        pid,
        reason,
        notified: false,
    };
    // SAFETY: Target stays alive through synchronous enumeration and callback.
    unsafe {
        EnumWindows(Some(visit), &mut target as *mut Target as isize);
    }
    target.notified
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};
    #[test]
    fn session_query_and_cancellation_do_not_stop_and_confirmation_waits_for_cleanup() {
        let state = State::new();
        let registration = Registration::open(state.clone()).unwrap();
        // SAFETY: these branches do not dereference the window handle or call
        // native window functions. They simulate OS messages in this test process.
        unsafe {
            assert_eq!(window(null_mut(), WM_QUERYENDSESSION, 0, 0), 1);
            assert_eq!(window(null_mut(), WM_ENDSESSION, 0, 0), 0);
        }
        assert!(state.current().is_none());
        let cleanup = state.clone();
        let complete = std::thread::spawn(move || {
            let until = Instant::now() + Duration::from_secs(2);
            while cleanup.current().is_none() {
                assert!(Instant::now() < until);
                std::thread::sleep(Duration::from_millis(1));
            }
            cleanup.finish();
        });
        let before = Instant::now();
        // SAFETY: confirmed-session branch only signals the owned State; the
        // native thread remains separately owned by registration until drop.
        unsafe {
            assert_eq!(window(null_mut(), WM_ENDSESSION, 1, 0), 0);
        }
        complete.join().unwrap();
        assert_eq!(
            state.current().unwrap().reason,
            ShutdownReason::SessionEnding
        );
        assert!(before.elapsed() < Duration::from_secs(2));
        drop(registration);
    }
}
