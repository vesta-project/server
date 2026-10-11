//! Reviewed Windows-only native adapter: current token SID, owner-only protected
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
pub fn create(path: &Path, first: bool) -> Result<NamedPipeServer> {
    let sid = account()?;
    // Protected DACL containing only the current OS account; no Everyone, network,
    // administrators, SYSTEM, pack identity or invented canonical owner grant.
    let sddl: Vec<u16> = format!("O:{sid}D:P(A;;GA;;;{sid})")
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
        .max_instances(2);
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
pub fn verify_server(client: &NamedPipeClient) -> Result<()> {
    let mut pid = 0;
    // SAFETY: client owns the live connected pipe, and pid is a writable output.
    if unsafe { GetNamedPipeServerProcessId(client.as_raw_handle().cast(), &mut pid) } == 0 {
        return Err("local_control_identity_failed");
    }
    // SAFETY: query-only access, no inheritance or process mutation.
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if process.is_null() {
        return Err("local_control_identity_failed");
    }
    let process = OwnedHandle(process);
    if token_account(process.0)? != account()? {
        return Err("local_control_acl_refused");
    }
    Ok(())
}
