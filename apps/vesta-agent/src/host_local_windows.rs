//! Windows-only native adapter: service token SID, protected service/admin
//! pipe DACL, local clients only, and verification of the connected server token.
#![allow(unsafe_code)]
use crate::local_files::Result;
use std::{ffi::c_void, os::windows::io::AsRawHandle, path::Path, ptr::null_mut};
use tokio::net::windows::named_pipe::{NamedPipeClient, NamedPipeServer, ServerOptions};
use windows_sys::Win32::{
    Foundation::{CloseHandle, HANDLE, LocalFree},
    Security::{
        Authorization::{
            ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
            SDDL_REVISION_1,
        },
        GetTokenInformation, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER, TokenUser,
    },
    System::{
        Pipes::GetNamedPipeServerProcessId,
        Threading::{
            GetCurrentProcess, OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
        },
    },
};
struct OwnedHandle(HANDLE);
impl Drop for OwnedHandle {
    fn drop(&mut self) {
        // SAFETY: this adapter owns a successfully opened handle.
        unsafe {
            CloseHandle(self.0);
        }
    }
}
struct LocalAllocation(*mut c_void);
impl Drop for LocalAllocation {
    fn drop(&mut self) {
        // SAFETY: conversion functions allocate with LocalAlloc, exactly one owner.
        unsafe {
            LocalFree(self.0);
        }
    }
}
fn token_account(process: HANDLE) -> Result<String> {
    let mut token = null_mut();
    // SAFETY: live borrowed process handle, writable token output.
    if unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut token) } == 0 {
        return Err("local_control_identity_failed");
    }
    let token = OwnedHandle(token);
    let mut size = 0;
    // SAFETY: documented size query, null buffer and zero capacity.
    unsafe {
        GetTokenInformation(token.0, TokenUser, null_mut(), 0, &mut size);
    }
    if size == 0 || size > 65536 {
        return Err("local_control_identity_failed");
    }
    // usize storage guarantees alignment for TOKEN_USER and native SID data.
    let mut storage = vec![0usize; (size as usize).div_ceil(std::mem::size_of::<usize>())];
    // SAFETY: aligned allocation with at least size bytes; output size stays live.
    if unsafe {
        GetTokenInformation(
            token.0,
            TokenUser,
            storage.as_mut_ptr().cast(),
            size,
            &mut size,
        )
    } == 0
    {
        return Err("local_control_identity_failed");
    }
    // SAFETY: successful TokenUser query initialized the aligned TOKEN_USER header.
    let user = unsafe { &*storage.as_ptr().cast::<TOKEN_USER>() };
    let mut string = null_mut();
    // SAFETY: SID points into the live initialized token buffer; output allocation is owned below.
    if unsafe { ConvertSidToStringSidW(user.User.Sid, &mut string) } == 0 {
        return Err("local_control_identity_failed");
    }
    let allocation = LocalAllocation(string.cast());
    let mut length = 0;
    // SID textual form is bounded; the API guarantees null termination.
    while length < 256 {
        // SAFETY: reading the API-created null-terminated SID string up to its terminator.
        if unsafe { *string.add(length) } == 0 {
            break;
        }
        length += 1;
    }
    if length == 256 {
        return Err("local_control_identity_failed");
    }
    // SAFETY: counted initialized UTF-16 prefix within the live conversion allocation.
    let sid = String::from_utf16(unsafe { std::slice::from_raw_parts(string, length) })
        .map_err(|_| "local_control_identity_failed")?;
    drop(allocation);
    Ok(sid)
}
pub fn account() -> Result<String> {
    // SAFETY: GetCurrentProcess returns a borrowed pseudo-handle, never closed here.
    token_account(unsafe { GetCurrentProcess() })
}
/// `max_instances` bounds concurrently connected clients plus the listening instance.
pub fn create(path: &Path, first: bool, max_instances: usize) -> Result<NamedPipeServer> {
    let sid = account()?;
    // Protected DACL admits the service account and elevated administrators.
    // No Everyone, network clients, or game/worker identities are admitted.
    let sddl: Vec<u16> = format!("O:{sid}D:P(A;;GA;;;{sid})(A;;GA;;;BA)")
        .encode_utf16()
        .chain(Some(0))
        .collect();
    let mut descriptor = null_mut();
    // SAFETY: terminated SDDL and writable descriptor output; conversion owns the allocation.
    if unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            SDDL_REVISION_1,
            &mut descriptor,
            null_mut(),
        )
    } == 0
    {
        return Err("local_control_acl_refused");
    }
    let descriptor = LocalAllocation(descriptor);
    let mut attributes = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor.0,
        bInheritHandle: 0,
    };
    let mut options = ServerOptions::new();
    options
        .first_pipe_instance(first)
        .reject_remote_clients(true)
        .in_buffer_size(2048)
        .out_buffer_size(2048)
        .max_instances(max_instances);
    // SAFETY: SECURITY_ATTRIBUTES and descriptor stay alive through CreateNamedPipe;
    // the kernel copies the descriptor and no pointer is retained by tokio.
    unsafe {
        options.create_with_security_attributes_raw(
            path,
            (&mut attributes as *mut SECURITY_ATTRIBUTES).cast::<c_void>(),
        )
    }
    .map_err(|_| "local_control_bind_failed")
}
pub fn verify_server(client: &NamedPipeClient, state: &Path) -> Result<()> {
    verify_server_handle(client.as_raw_handle().cast(), &directory_owner(state)?)
}
/// The handle must be a live, connected client end of a named pipe.
fn verify_server_handle(pipe: HANDLE, expected: &str) -> Result<()> {
    let mut pid = 0;
    // SAFETY: caller supplies a live connected pipe handle; pid is a writable output.
    if unsafe { GetNamedPipeServerProcessId(pipe, &mut pid) } == 0 {
        return Err("local_control_identity_failed");
    }
    // SAFETY: query-only access, no inheritance or process mutation.
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if process.is_null() {
        return Err("local_control_identity_failed");
    }
    let process = OwnedHandle(process);
    if token_account(process.0)? != expected {
        return Err("local_control_acl_refused");
    }
    Ok(())
}

// The protected host state's owner is the expected service identity. Administrators
// may connect across accounts without trusting a caller-controlled pipe process.
pub fn directory_owner(path: &Path) -> Result<String> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Security::{
        Authorization::{GetNamedSecurityInfoW, SE_FILE_OBJECT},
        OWNER_SECURITY_INFORMATION,
    };
    let path: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut owner = null_mut();
    let mut descriptor = null_mut();
    // SAFETY: terminated path and writable owner/descriptor outputs. The owner
    // points into the descriptor allocation, which stays alive through conversion.
    let status = unsafe {
        GetNamedSecurityInfoW(
            path.as_ptr(),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION,
            &mut owner,
            null_mut(),
            null_mut(),
            null_mut(),
            &mut descriptor,
        )
    };
    let _allocation = LocalAllocation(descriptor);
    if status != 0 || owner.is_null() {
        return Err("host_local_acl_refused");
    }
    let mut string = null_mut();
    // SAFETY: owner borrows the live security descriptor; conversion allocates a SID string.
    if unsafe { ConvertSidToStringSidW(owner, &mut string) } == 0 {
        return Err("host_local_acl_refused");
    }
    let _string = LocalAllocation(string.cast());
    let mut length = 0;
    // SAFETY: API-created null-terminated SID string; bounded scan.
    while length < 256 && unsafe { *string.add(length) } != 0 {
        length += 1;
    }
    if length == 256 {
        return Err("host_local_acl_refused");
    }
    // SAFETY: counted initialized prefix of the live SID string.
    String::from_utf16(unsafe { std::slice::from_raw_parts(string, length) })
        .map_err(|_| "host_local_acl_refused")
}
