//! Native ownership of the launched process family. Not a security sandbox:
//! Unix descendants can deliberately leave their group. Windows children start
//! suspended and join the owned kill-on-close job before any application code runs.
#![allow(unsafe_code)]
use super::RuntimeError;
use std::process::{Child, Command};

#[cfg(unix)]
pub(super) struct ProcessTree {
    group: Option<i32>,
}
#[cfg(unix)]
impl ProcessTree {
    pub(super) fn prepare() -> Result<Self, RuntimeError> {
        Ok(Self { group: None })
    }
    pub(super) fn configure(command: &mut Command) {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    pub(super) fn attach(&mut self, child: &Child) -> Result<(), RuntimeError> {
        let group = i32::try_from(child.id()).map_err(|_| RuntimeError::StartupUncertain)?;
        if group <= 1 {
            return Err(RuntimeError::StartupUncertain);
        }
        self.group = Some(group);
        Ok(())
    }
    pub(super) fn kill(&self, child: &mut Child) -> Result<(), RuntimeError> {
        let Some(group) = self.group else {
            return child.kill().map_err(|_| RuntimeError::KillFailed);
        };
        // SAFETY: group is the private process group established at spawn, never
        // a caller-supplied PID. Its lifetime is retained with this generation.
        let group_result = if unsafe { libc::kill(-group, libc::SIGKILL) } == 0
            || std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
        {
            Ok(())
        } else {
            Err(RuntimeError::KillFailed)
        };
        // Still terminate the exact direct child if it deliberately changed groups.
        let direct_result = child.kill().map_err(|_| RuntimeError::KillFailed);
        group_result.and(direct_result)
    }
}

#[cfg(windows)]
mod windows {
    use super::*;
    use std::{
        mem::{size_of, zeroed},
        os::windows::{
            io::{AsRawHandle, FromRawHandle, OwnedHandle},
            process::CommandExt,
        },
    };
    use windows_sys::Win32::{
        Foundation::{HANDLE, INVALID_HANDLE_VALUE},
        System::{
            Diagnostics::ToolHelp::{
                CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First,
                Thread32Next,
            },
            JobObjects::{
                AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
                JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
                SetInformationJobObject, TerminateJobObject,
            },
            Threading::{
                CREATE_SUSPENDED, GetProcessIdOfThread, OpenThread, ResumeThread,
                THREAD_QUERY_LIMITED_INFORMATION, THREAD_SUSPEND_RESUME,
            },
        },
    };
    pub(crate) struct ProcessTree {
        job: OwnedHandle,
        attached: bool,
    }
    impl ProcessTree {
        pub(crate) fn prepare() -> Result<Self, RuntimeError> {
            // SAFETY: anonymous non-inheritable job, valid owned handle thereafter.
            let raw = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
            if raw.is_null() {
                return Err(RuntimeError::SpawnFailed);
            }
            let job = unsafe { OwnedHandle::from_raw_handle(raw) };
            let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { zeroed() };
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            // SAFETY: buffer is the declared native layout and remains alive for the call.
            if unsafe {
                SetInformationJobObject(
                    raw,
                    JobObjectExtendedLimitInformation,
                    (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                    size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                )
            } == 0
            {
                return Err(RuntimeError::SpawnFailed);
            }
            Ok(Self {
                job,
                attached: false,
            })
        }
        pub(crate) fn configure(command: &mut Command) {
            command.creation_flags(CREATE_SUSPENDED);
        }
        pub(crate) fn attach(&mut self, child: &Child) -> Result<(), RuntimeError> {
            // SAFETY: child is still suspended; assignment precedes executing its code.
            if unsafe {
                AssignProcessToJobObject(
                    self.job.as_raw_handle() as HANDLE,
                    child.as_raw_handle() as HANDLE,
                )
            } == 0
            {
                return Err(RuntimeError::StartupUncertain);
            }
            self.attached = true;
            // Stable Rust does not expose the initial thread handle. Enumerate the
            // single initial thread of this suspended, pinned child, then resume it.
            // SAFETY: snapshot handle and initialized entry are used per Win32 contract.
            let raw = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
            if raw == INVALID_HANDLE_VALUE {
                return Err(RuntimeError::StartupUncertain);
            }
            let snapshot = unsafe { OwnedHandle::from_raw_handle(raw) };
            let mut entry: THREADENTRY32 = unsafe { zeroed() };
            entry.dwSize = size_of::<THREADENTRY32>() as u32;
            let mut found = None;
            let mut more = unsafe { Thread32First(snapshot.as_raw_handle() as HANDLE, &mut entry) };
            while more != 0 {
                if entry.th32OwnerProcessID == child.id()
                    && found.replace(entry.th32ThreadID).is_some()
                {
                    return Err(RuntimeError::StartupUncertain);
                }
                more = unsafe { Thread32Next(snapshot.as_raw_handle() as HANDLE, &mut entry) };
            }
            let id = found.ok_or(RuntimeError::StartupUncertain)?;
            let raw = unsafe {
                OpenThread(
                    THREAD_SUSPEND_RESUME | THREAD_QUERY_LIMITED_INFORMATION,
                    0,
                    id,
                )
            };
            if raw.is_null() {
                return Err(RuntimeError::StartupUncertain);
            }
            let thread = unsafe { OwnedHandle::from_raw_handle(raw) };
            if unsafe { GetProcessIdOfThread(thread.as_raw_handle() as HANDLE) } != child.id() {
                return Err(RuntimeError::StartupUncertain);
            }
            if unsafe { ResumeThread(thread.as_raw_handle() as HANDLE) } != 1 {
                return Err(RuntimeError::StartupUncertain);
            }
            Ok(())
        }
        pub(crate) fn kill(&self, child: &mut Child) -> Result<(), RuntimeError> {
            if !self.attached {
                return child.kill().map_err(|_| RuntimeError::KillFailed);
            }
            // SAFETY: this owned job contains only this generation's process family.
            if unsafe { TerminateJobObject(self.job.as_raw_handle() as HANDLE, 1) } == 0 {
                Err(RuntimeError::KillFailed)
            } else {
                Ok(())
            }
        }
    }
}
#[cfg(windows)]
pub(super) use windows::ProcessTree;

// Keep the Unix group leader unreaped until owner finalization, so its numeric
// process-group ID cannot be reused while Draining still permits a family kill.
#[cfg(unix)]
pub(super) fn observe(child: &mut Child) -> Result<Option<std::process::ExitStatus>, RuntimeError> {
    use std::os::unix::process::ExitStatusExt;
    // SAFETY: waitid writes this initialized native buffer for our exclusive child.
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    if unsafe {
        libc::waitid(
            libc::P_PID,
            child.id() as libc::id_t,
            &mut info,
            libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
        )
    } != 0
    {
        return if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
            Ok(None)
        } else {
            Err(RuntimeError::ObserveFailed)
        };
    }
    // SAFETY: sigchld members are initialized by successful waitid (or zero for no event).
    if unsafe { info.si_pid() } == 0 {
        return Ok(None);
    }
    let status = unsafe { info.si_status() };
    let raw = match info.si_code {
        libc::CLD_EXITED => status << 8,
        libc::CLD_KILLED => status,
        libc::CLD_DUMPED => status | 0x80,
        _ => return Err(RuntimeError::ObserveFailed),
    };
    Ok(Some(std::process::ExitStatus::from_raw(raw)))
}
#[cfg(windows)]
pub(super) fn observe(child: &mut Child) -> Result<Option<std::process::ExitStatus>, RuntimeError> {
    child.try_wait().map_err(|_| RuntimeError::ObserveFailed)
}
