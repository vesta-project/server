//! Windows dev-agent launch with an explicit inherited-handle allowlist.
//! Rust's stable Command API inherits unrelated inheritable handles on Windows.
//! Those include PowerShell pipeline handles that can hold the terminal open.
#![allow(unsafe_code)]

use std::{
    cmp::Ordering,
    ffi::OsStr,
    fs::File,
    io,
    mem::{size_of, size_of_val},
    os::windows::{
        ffi::OsStrExt,
        io::{AsRawHandle, FromRawHandle, OwnedHandle},
        process::ExitStatusExt,
    },
    process::{Command, ExitStatus},
    ptr,
};
use windows_sys::Win32::{
    Foundation::{DUPLICATE_SAME_ACCESS, DuplicateHandle, HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT},
    Globalization::{CSTR_EQUAL, CompareStringOrdinal},
    System::Threading::{
        CREATE_BREAKAWAY_FROM_JOB, CREATE_NEW_PROCESS_GROUP, CREATE_UNICODE_ENVIRONMENT,
        CreateProcessW, DETACHED_PROCESS, DeleteProcThreadAttributeList,
        EXTENDED_STARTUPINFO_PRESENT, GetCurrentProcess, GetExitCodeProcess,
        InitializeProcThreadAttributeList, LPPROC_THREAD_ATTRIBUTE_LIST,
        PROC_THREAD_ATTRIBUTE_HANDLE_LIST, PROCESS_INFORMATION, STARTF_USESTDHANDLES,
        STARTUPINFOEXW, UpdateProcThreadAttribute, WaitForSingleObject,
    },
};

pub struct Process {
    handle: OwnedHandle,
    id: u32,
}
impl Process {
    pub fn id(&self) -> u32 {
        self.id
    }
    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        self.observe(0)
    }
    #[cfg(test)]
    pub fn wait(&mut self) -> io::Result<ExitStatus> {
        self.observe(windows_sys::Win32::System::Threading::INFINITE)?
            .ok_or_else(|| io::Error::other("process wait timed out"))
    }
    fn observe(&self, timeout: u32) -> io::Result<Option<ExitStatus>> {
        // SAFETY: the owned process handle stays live for this call.
        match unsafe { WaitForSingleObject(self.handle.as_raw_handle(), timeout) } {
            WAIT_TIMEOUT => Ok(None),
            WAIT_OBJECT_0 => {
                let mut code = 0;
                // SAFETY: valid process handle and output pointer.
                if unsafe { GetExitCodeProcess(self.handle.as_raw_handle(), &mut code) } == 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(Some(ExitStatus::from_raw(code)))
            }
            _ => Err(io::Error::last_os_error()),
        }
    }
}

fn wide(value: &OsStr) -> io::Result<Vec<u16>> {
    let mut value: Vec<_> = value.encode_wide().collect();
    if value.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "NUL in process argument",
        ));
    }
    value.push(0);
    Ok(value)
}

// Microsoft CRT quoting: double runs of backslashes before a quote or the final
// closing quote. No shell, PowerShell parser or command substitution is involved.
fn quoted(value: &OsStr, output: &mut Vec<u16>) {
    output.push(b'"' as u16);
    let mut slashes = 0;
    for unit in value.encode_wide() {
        if unit == b'\\' as u16 {
            slashes += 1;
            continue;
        }
        output.extend(std::iter::repeat_n(
            b'\\' as u16,
            if unit == b'"' as u16 {
                slashes * 2 + 1
            } else {
                slashes
            },
        ));
        output.push(unit);
        slashes = 0;
    }
    output.extend(std::iter::repeat_n(b'\\' as u16, slashes * 2));
    output.push(b'"' as u16);
}

fn insensitive(a: &[u16], b: &[u16]) -> Ordering {
    // SAFETY: slices are live and their lengths fit i32 (OS environment names).
    unsafe { CompareStringOrdinal(a.as_ptr(), a.len() as i32, b.as_ptr(), b.len() as i32, 1) }
        .cmp(&CSTR_EQUAL)
}

fn environment(command: &Command) -> io::Result<Vec<u16>> {
    let mut pairs: Vec<_> = std::env::vars_os()
        .map(|(key, value)| {
            (
                key.encode_wide().collect::<Vec<_>>(),
                value.encode_wide().collect::<Vec<_>>(),
            )
        })
        .collect();
    for (key, value) in command.get_envs() {
        let key: Vec<_> = key.encode_wide().collect();
        pairs.retain(|(old, _)| insensitive(old, &key) != Ordering::Equal);
        if let Some(value) = value {
            pairs.push((key, value.encode_wide().collect()));
        }
    }
    pairs.sort_by(|(a, _), (b, _)| insensitive(a, b));
    let mut block = Vec::new();
    for (key, value) in pairs {
        if key.contains(&0) || value.contains(&0) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "NUL in process environment",
            ));
        }
        block.extend(key);
        block.push(b'=' as u16);
        block.extend(value);
        block.push(0);
    }
    block.push(0);
    if block.len() == 1 {
        block.push(0);
    }
    Ok(block)
}

fn inherited(file: &File) -> io::Result<OwnedHandle> {
    let mut handle = ptr::null_mut();
    // SAFETY: duplicate a borrowed file handle in this process; only the new,
    // separately owned handle is inheritable. Originals are never modified.
    if unsafe {
        DuplicateHandle(
            GetCurrentProcess(),
            file.as_raw_handle(),
            GetCurrentProcess(),
            &mut handle,
            0,
            1,
            DUPLICATE_SAME_ACCESS,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: DuplicateHandle succeeded and transferred one owned handle.
    Ok(unsafe { OwnedHandle::from_raw_handle(handle) })
}

struct Attributes<'a> {
    pointer: LPPROC_THREAD_ATTRIBUTE_LIST,
    _storage: Vec<usize>,
    _handles: std::marker::PhantomData<&'a [HANDLE]>,
}
impl<'a> Attributes<'a> {
    fn handles(handles: &'a [HANDLE]) -> io::Result<Self> {
        let mut size = 0;
        // SAFETY: documented sizing query; no attribute list exists yet.
        unsafe {
            InitializeProcThreadAttributeList(ptr::null_mut(), 1, 0, &mut size);
        }
        if size == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut storage = vec![0_usize; size.div_ceil(size_of::<usize>())];
        let pointer = storage.as_mut_ptr().cast();
        // SAFETY: aligned storage has at least the requested size.
        if unsafe { InitializeProcThreadAttributeList(pointer, 1, 0, &mut size) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let attributes = Self {
            pointer,
            _storage: storage,
            _handles: std::marker::PhantomData,
        };
        // SAFETY: initialized list and borrowed handle array remain live until
        // CreateProcessW returns. All listed handles were duplicated inheritable.
        if unsafe {
            UpdateProcThreadAttribute(
                pointer,
                0,
                PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
                handles.as_ptr().cast(),
                size_of_val(handles),
                ptr::null_mut(),
                ptr::null(),
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(attributes)
    }
}
impl Drop for Attributes<'_> {
    fn drop(&mut self) {
        // SAFETY: delete the initialized list before its backing storage drops.
        unsafe {
            DeleteProcThreadAttributeList(self.pointer);
        }
    }
}

pub fn spawn(command: &Command, log: &File) -> io::Result<Process> {
    spawn_in(
        command,
        log,
        vesta_supervisor::Containment::RequireIndependent,
    )
}

fn spawn_in(
    command: &Command,
    log: &File,
    containment: vesta_supervisor::Containment,
) -> io::Result<Process> {
    let executable = wide(command.get_program())?;
    let mut line = Vec::new();
    quoted(command.get_program(), &mut line);
    for argument in command.get_args() {
        line.push(b' ' as u16);
        quoted(argument, &mut line);
    }
    if line.contains(&0) || line.len() >= 32767 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid Windows command length",
        ));
    }
    line.push(0);
    let environment = environment(command)?;
    let directory = command
        .get_current_dir()
        .map(|path| wide(path.as_os_str()))
        .transpose()?;
    let null = inherited(&File::open("NUL")?)?;
    let log = inherited(log)?;
    let handles = [null.as_raw_handle(), log.as_raw_handle()];
    let attributes = Attributes::handles(&handles)?;
    let startup = STARTUPINFOEXW {
        StartupInfo: windows_sys::Win32::System::Threading::STARTUPINFOW {
            cb: size_of::<STARTUPINFOEXW>() as u32,
            dwFlags: STARTF_USESTDHANDLES,
            hStdInput: handles[0],
            hStdOutput: handles[1],
            hStdError: handles[1],
            ..Default::default()
        },
        lpAttributeList: attributes.pointer,
    };
    let mut info = PROCESS_INFORMATION::default();
    let flags = DETACHED_PROCESS
        | CREATE_NEW_PROCESS_GROUP
        | CREATE_UNICODE_ENVIRONMENT
        | EXTENDED_STARTUPINFO_PRESENT;
    let create = |flags, line: &mut Vec<u16>, info: &mut PROCESS_INFORMATION| {
        // SAFETY: all NUL-terminated strings, environment, handle array, backing
        // attribute storage and output info remain valid throughout this call.
        unsafe {
            CreateProcessW(
                executable.as_ptr(),
                line.as_mut_ptr(),
                ptr::null(),
                ptr::null(),
                1,
                flags,
                environment.as_ptr().cast(),
                directory.as_ref().map_or(ptr::null(), |v| v.as_ptr()),
                &startup.StartupInfo,
                info,
            )
        }
    };
    let mut attempt = line.clone();
    if create(flags | CREATE_BREAKAWAY_FROM_JOB, &mut attempt, &mut info) == 0 {
        let error = io::Error::last_os_error();
        if containment == vesta_supervisor::Containment::RequireIndependent
            && !vesta_supervisor::pipe_security::job_allows_independent_lifetime()
        {
            return Err(error);
        }
        if create(flags, &mut line, &mut info) == 0 {
            return Err(io::Error::last_os_error());
        }
    }
    // SAFETY: successful CreateProcessW returns these two independently owned
    // handles. Close the unused thread handle; retain process observation only.
    let (handle, thread) = unsafe {
        (
            OwnedHandle::from_raw_handle(info.hProcess),
            OwnedHandle::from_raw_handle(info.hThread),
        )
    };
    drop(thread);
    Ok(Process {
        handle,
        id: info.dwProcessId,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn quoting_keeps_spaces_quotes_and_trailing_backslashes_literal() {
        for (input, expected) in [
            ("", "\"\""),
            ("path with spaces", "\"path with spaces\""),
            ("a\"b", "\"a\\\"b\""),
            ("C:\\path\\", "\"C:\\path\\\\\""),
        ] {
            let mut output = Vec::new();
            quoted(OsStr::new(input), &mut output);
            assert_eq!(String::from_utf16(&output).unwrap(), expected);
        }
    }

    #[test]
    fn owned_wait() {
        let Some(release) = std::env::var_os("VESTA_DEV_HANDLE_TEST_RELEASE") else {
            return;
        };
        println!(
            "owned wait fixture: {}",
            std::env::var("VESTA_AGENT_STAY_STOPPED").unwrap()
        );
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !std::path::Path::new(&release).exists() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }

    #[test]
    fn detached_process_does_not_inherit_unrelated_pipeline_handles() {
        use windows_sys::Win32::{
            Security::SECURITY_ATTRIBUTES,
            System::Pipes::{CreatePipe, PeekNamedPipe},
        };
        let temp = tempfile::Builder::new()
            .prefix("vesta detached handles ")
            .tempdir()
            .unwrap();
        let release = temp.path().join("release");
        let log_path = temp.path().join("agent.log");
        let log = File::create(&log_path).unwrap();
        let mut read = ptr::null_mut();
        let mut write = ptr::null_mut();
        let security = SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: ptr::null_mut(),
            bInheritHandle: 1,
        };
        // SAFETY: initialized security attributes and live output pointers.
        assert_ne!(
            unsafe { CreatePipe(&mut read, &mut write, &security, 0) },
            0
        );
        // SAFETY: successful CreatePipe transfers two independent owned handles.
        let (read, write) = unsafe {
            (
                OwnedHandle::from_raw_handle(read),
                OwnedHandle::from_raw_handle(write),
            )
        };
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args(["--exact", "detached::tests::owned_wait", "--nocapture"])
            .current_dir(temp.path())
            .env("VESTA_DEV_HANDLE_TEST_RELEASE", &release)
            .env("VESTA_AGENT_STAY_STOPPED", "1");
        let mut child = spawn_in(
            &command,
            &log,
            vesta_supervisor::Containment::AllowContained,
        )
        .unwrap();
        let still_running = child.try_wait().unwrap().is_none();
        drop(write);
        // SAFETY: borrowed valid pipe handle, no output buffers requested. With
        // no inherited writer left, PeekNamedPipe must report a broken pipe.
        let pipe_open = unsafe {
            PeekNamedPipe(
                read.as_raw_handle(),
                ptr::null_mut(),
                0,
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
            )
        } != 0;
        let pipe_error = io::Error::last_os_error().raw_os_error();
        std::fs::write(release, b"release owned fixture").unwrap();
        let status = child.wait().unwrap();
        assert!(still_running);
        assert!(!pipe_open, "child inherited a caller pipeline handle");
        assert_eq!(
            pipe_error,
            Some(windows_sys::Win32::Foundation::ERROR_BROKEN_PIPE as i32)
        );
        assert!(status.success());
        assert!(
            std::fs::read_to_string(log_path)
                .unwrap()
                .contains("owned wait fixture: 1")
        );
    }
}
