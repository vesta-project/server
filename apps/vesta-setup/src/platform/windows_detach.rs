//! Fixed native bootstrap only: same user/token/environment, no inherited handles.
//! Caller retains the verified executable handles through CreateProcessW.
use std::{ffi::OsStr, os::windows::ffi::OsStrExt, path::Path, process::Command};
use windows_sys::Win32::{
    Foundation::CloseHandle,
    System::Threading::{
        CREATE_NO_WINDOW, CreateProcessW, DETACHED_PROCESS, PROCESS_INFORMATION, STARTUPINFOW,
    },
};
fn wide(value: &OsStr) -> Result<Vec<u16>, String> {
    let mut out: Vec<_> = value.encode_wide().collect();
    if out.contains(&0) {
        return Err("setup_bootstrap_argument_invalid".into());
    }
    out.push(0);
    Ok(out)
}
/// CRT quoting policy also used by the existing sandbox helper, applied to UTF16
/// directly so Unicode/native paths are not converted through lossy UTF8.
fn quote(value: &OsStr) -> Result<Vec<u16>, String> {
    let mut value = wide(value)?;
    value.pop();
    let mut out = vec![34];
    let mut slashes = 0;
    for unit in value {
        if unit == 92 {
            slashes += 1;
            continue;
        }
        if unit == 34 {
            out.extend(std::iter::repeat_n(92, slashes * 2 + 1));
        } else {
            out.extend(std::iter::repeat_n(92, slashes));
        }
        out.push(unit);
        slashes = 0;
    }
    out.extend(std::iter::repeat_n(92, slashes * 2));
    out.push(34);
    Ok(out)
}
pub(super) fn spawn(command: &Command, root: &Path) -> Result<(), String> {
    // This is not a generic replacement for Command: fixed bootstrap arguments,
    // inherited environment only, no shell/custom token or environment overrides.
    if command.get_envs().next().is_some() {
        return Err("setup_bootstrap_environment_refused".into());
    }
    let args: Vec<_> = command.get_args().collect();
    if args != [OsStr::new("host"), OsStr::new("--root"), root.as_os_str()] {
        return Err("setup_bootstrap_arguments_refused".into());
    }
    let application = wide(command.get_program())?;
    let directory = wide(root.as_os_str())?;
    let mut line = quote(command.get_program())?;
    for arg in args {
        line.push(32);
        line.extend(quote(arg)?);
    }
    line.push(0);
    if line.len() > 32767 {
        return Err("setup_bootstrap_argument_invalid".into());
    }
    // SAFETY: initialized ABI structs and terminated live UTF16 buffers. Explicit
    // application avoids executable-search ambiguity. FALSE disallows *all*
    // inheritable handles; no STARTF_USESTDHANDLES, job breakaway or privilege flags.
    let mut startup: STARTUPINFOW = unsafe { std::mem::zeroed() };
    startup.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
    let mut process: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
    let success = unsafe {
        CreateProcessW(
            application.as_ptr(),
            line.as_mut_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            0,
            DETACHED_PROCESS | CREATE_NO_WINDOW,
            std::ptr::null(),
            directory.as_ptr(),
            &startup,
            &mut process,
        )
    };
    if success == 0 {
        return Err("could not start host".into());
    }
    // SAFETY: successful CreateProcessW transfers these two owned handles. Closing
    // them releases parent resources, never terminates the detached child.
    unsafe {
        CloseHandle(process.hThread);
        CloseHandle(process.hProcess);
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_arguments_preserve_quotes_spaces_unicode_and_trailing_slashes() {
        for (input, expected) in [
            ("plain", "\"plain\""),
            ("two words", "\"two words\""),
            ("", "\"\""),
            ("say \"hello\"", "\"say \\\"hello\\\"\""),
            ("C:\\path with space\\", "\"C:\\path with space\\\\\""),
            ("C:\\\u{4e16}\u{754c}\\", "\"C:\\\u{4e16}\u{754c}\\\\\""),
        ] {
            assert_eq!(
                String::from_utf16(&quote(OsStr::new(input)).unwrap()).unwrap(),
                expected
            );
        }
        assert!(quote(OsStr::new("bad\0argument")).is_err());
    }
}
