//! Isolated Windows console adapter. Only the waiting development-shell parent
//! ignores Ctrl+C/Break; child shells and commands keep their ordinary handlers.
#![allow(unsafe_code)]

use windows_sys::Win32::System::Console::{CTRL_BREAK_EVENT, CTRL_C_EVENT, SetConsoleCtrlHandler};

unsafe extern "system" fn keep_parent_alive(event: u32) -> i32 {
    i32::from(matches!(event, CTRL_C_EVENT | CTRL_BREAK_EVENT))
}

pub struct KeepParentAlive;

impl KeepParentAlive {
    pub fn install() -> std::io::Result<Self> {
        // SAFETY: static callback with the required ABI. It accesses no memory,
        // allocates nothing, and handles only the two terminal interrupt events.
        if unsafe { SetConsoleCtrlHandler(Some(keep_parent_alive), 1) } == 0 {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(Self)
        }
    }
}

impl Drop for KeepParentAlive {
    fn drop(&mut self) {
        // SAFETY: remove the same static callback after the shell has exited.
        unsafe { SetConsoleCtrlHandler(Some(keep_parent_alive), 0) };
    }
}
