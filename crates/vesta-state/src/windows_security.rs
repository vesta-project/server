//! Handle-based Windows protection. All native calls/unsafe code are confined to this module.
//! Files own their handles through std RAII; Windows-allocated security descriptors use LocalFree.
use crate::{Result, StateError};
use std::{
    ffi::c_void,
    fs::{File, OpenOptions},
    mem::{size_of, zeroed},
    os::windows::{ffi::OsStrExt, fs::OpenOptionsExt, io::AsRawHandle},
    path::{Component, Path, Prefix},
    ptr::{addr_of, null, null_mut},
};
use windows_sys::Win32::{
    Foundation::{ERROR_SUCCESS, HANDLE, LocalFree},
    Globalization::{CSTR_EQUAL, CompareStringOrdinal},
    Security::{
        ACCESS_ALLOWED_ACE, ACL, ACL_REVISION, ACL_SIZE_INFORMATION, AclSizeInformation,
        AddAccessAllowedAceEx,
        Authorization::{GetSecurityInfo, SE_FILE_OBJECT, SetSecurityInfo},
        CONTAINER_INHERIT_ACE, DACL_SECURITY_INFORMATION, EqualSid, GetAce, GetAclInformation,
        GetLengthSid, GetSecurityDescriptorControl, InitializeAcl, IsValidSid, OBJECT_INHERIT_ACE,
        OWNER_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR,
        PSID, SE_DACL_PROTECTED,
    },
    Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, FILE_ALL_ACCESS, FILE_ATTRIBUTE_DIRECTORY,
        FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
        FILE_GENERIC_READ, FILE_GENERIC_WRITE, FILE_READ_ATTRIBUTES, FILE_SHARE_READ,
        FILE_SHARE_WRITE, FILE_TYPE_DISK, GetDriveTypeW, GetFileInformationByHandle, GetFileType,
        GetVolumeInformationW, GetVolumePathNameW, READ_CONTROL, WRITE_DAC,
    },
    System::{
        SystemServices::{ACCESS_ALLOWED_ACE_TYPE, FILE_PERSISTENT_ACLS},
        WindowsProgramming::DRIVE_FIXED,
    },
};

fn permissions_failed() -> StateError {
    StateError::new("state_permissions_failed")
}

fn handle(file: &File) -> HANDLE {
    file.as_raw_handle().cast()
}

/// Reject alternate streams, DOS devices, remote/device namespaces, and ambiguous Win32 spellings.
pub(crate) fn validate_path(path: &Path) -> Result<()> {
    let failure = || StateError::new("state_dir_invalid");
    if path.has_root() && !path.is_absolute() {
        return Err(failure());
    }
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => match prefix.kind() {
                Prefix::Disk(_) | Prefix::VerbatimDisk(_) if path.is_absolute() => {}
                _ => return Err(failure()),
            },
            Component::Normal(name) => {
                let units: Vec<_> = name.encode_wide().collect();
                if units.is_empty()
                    || units
                        .last()
                        .is_some_and(|unit| *unit == b'.' as u16 || *unit == b' ' as u16)
                    || units.iter().any(|unit| {
                        *unit < 32
                            || [b':', b'<', b'>', b'"', b'|', b'?', b'*']
                                .iter()
                                .any(|bad| *unit == *bad as u16)
                    })
                {
                    return Err(failure());
                }
                let base: Vec<_> = units
                    .iter()
                    .take_while(|unit| **unit != b'.' as u16)
                    .map(|unit| {
                        if (*unit >= b'a' as u16) && (*unit <= b'z' as u16) {
                            *unit - 32
                        } else {
                            *unit
                        }
                    })
                    .collect();
                let reserved = ["CON", "PRN", "AUX", "NUL", "CONIN$", "CONOUT$"];
                if reserved
                    .iter()
                    .any(|name| base == name.encode_utf16().collect::<Vec<_>>())
                    || (base.len() == 4
                        && (base[..3] == [b'C' as u16, b'O' as u16, b'M' as u16]
                            || base[..3] == [b'L' as u16, b'P' as u16, b'T' as u16])
                        && ((b'0' as u16..=b'9' as u16).contains(&base[3])
                            || [0x00b9, 0x00b2, 0x00b3].contains(&base[3])))
                {
                    return Err(failure());
                }
            }
            Component::RootDir | Component::CurDir | Component::ParentDir => {}
        }
    }
    Ok(())
}

/// Use the OS's ordinal case-insensitive comparison, not an ASCII/Unicode lowercase approximation.
pub(crate) fn is_within(path: &Path, root: &Path) -> Result<bool> {
    let mut path_components = path.components();
    for expected in root.components() {
        let Some(actual) = path_components.next() else {
            return Ok(false);
        };
        // canonicalize returns verbatim disk prefixes; callers may also supply ordinary disk paths.
        let disk = |component: Component<'_>| match component {
            Component::Prefix(prefix) => match prefix.kind() {
                Prefix::Disk(drive) | Prefix::VerbatimDisk(drive) => {
                    Some(drive.to_ascii_uppercase())
                }
                _ => None,
            },
            _ => None,
        };
        if let (Some(actual), Some(expected)) = (disk(actual), disk(expected)) {
            if actual != expected {
                return Ok(false);
            }
            continue;
        }
        let actual: Vec<_> = actual.as_os_str().encode_wide().collect();
        let expected: Vec<_> = expected.as_os_str().encode_wide().collect();
        let actual_len =
            i32::try_from(actual.len()).map_err(|_| StateError::new("state_dir_invalid"))?;
        let expected_len =
            i32::try_from(expected.len()).map_err(|_| StateError::new("state_dir_invalid"))?;
        // SAFETY: both slices stay alive and their exact element counts fit the signed API lengths.
        let comparison = unsafe {
            CompareStringOrdinal(
                actual.as_ptr(),
                actual_len,
                expected.as_ptr(),
                expected_len,
                1,
            )
        };
        if comparison == 0 {
            return Err(StateError::new("state_dir_invalid"));
        }
        if comparison != CSTR_EQUAL {
            return Ok(false);
        }
    }
    Ok(true)
}

pub(crate) fn ensure_supported_filesystem(path: &Path) -> Result<()> {
    let failure = || StateError::new("state_filesystem_unsupported");
    let mut existing = path;
    while std::fs::symlink_metadata(existing).is_err() {
        existing = existing.parent().ok_or_else(failure)?;
    }
    let input = wide_path(existing)?;
    // Windows limits extended-length paths to 32,767 UTF-16 code units.
    let mut volume = vec![0u16; 32_768];
    // SAFETY: null-terminated input and correctly sized writable output live for the calls.
    let resolved =
        unsafe { GetVolumePathNameW(input.as_ptr(), volume.as_mut_ptr(), volume.len() as u32) };
    if resolved == 0 {
        return Err(failure());
    }
    // SAFETY: GetVolumePathNameW supplied a null-terminated root in this allocation.
    if unsafe { GetDriveTypeW(volume.as_ptr()) } != DRIVE_FIXED {
        return Err(failure());
    }
    let mut flags = 0;
    // SAFETY: only flags is requested; optional output pointers are null, with matching zero sizes.
    let result = unsafe {
        GetVolumeInformationW(
            volume.as_ptr(),
            null_mut(),
            0,
            null_mut(),
            null_mut(),
            &mut flags,
            null_mut(),
            0,
        )
    };
    if result == 0 || flags & FILE_PERSISTENT_ACLS == 0 {
        return Err(failure());
    }
    Ok(())
}

fn wide_path(path: &Path) -> Result<Vec<u16>> {
    let mut units: Vec<_> = path.as_os_str().encode_wide().collect();
    if units.contains(&0) || units.len() >= 32_767 {
        return Err(StateError::new("state_dir_invalid"));
    }
    units.push(0);
    Ok(units)
}

fn open_inspection(path: &Path, change_permissions: bool) -> Result<File> {
    let access =
        FILE_READ_ATTRIBUTES | READ_CONTROL | if change_permissions { WRITE_DAC } else { 0 };
    // Deny delete-sharing while inspecting/tightening the leaf. Ancestor replacement remains a race.
    OpenOptions::new()
        .access_mode(access)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)
        .map_err(|_| permissions_failed())
}

fn verify_handle(file: &File, directory: bool) -> Result<()> {
    // SAFETY: zero is a valid initial value for this POD output, and File owns a live handle.
    let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { zeroed() };
    let result = unsafe { GetFileInformationByHandle(handle(file), &mut info) };
    // SAFETY: File's live handle is borrowed and never retained by this call.
    let disk = unsafe { GetFileType(handle(file)) } == FILE_TYPE_DISK;
    if result == 0
        || !disk
        || info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0
        || (info.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY != 0) != directory
        || (!directory && info.nNumberOfLinks != 1)
    {
        return Err(StateError::new("state_file_linked"));
    }
    Ok(())
}

pub(crate) fn verify_regular(path: &Path) -> Result<()> {
    let file = open_inspection(path, false).map_err(|_| StateError::new("state_file_linked"))?;
    verify_handle(&file, false)
}

pub(crate) fn open_regular(path: &Path, create_new: bool, create: bool) -> Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .access_mode(FILE_GENERIC_READ | FILE_GENERIC_WRITE | READ_CONTROL | WRITE_DAC)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .create_new(create_new)
        .create(create)
        .open(path)?;
    verify_handle(&file, false)?;
    Ok(file)
}

pub(crate) fn open_execution_file(path: &Path) -> Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .access_mode(FILE_GENERIC_READ | READ_CONTROL)
        .share_mode(FILE_SHARE_READ)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)?;
    verify_handle(&file, false)?;
    Ok(file)
}

pub(crate) fn tighten_directory(path: &Path) -> Result<()> {
    let file = open_inspection(path, true)?;
    tighten_handle(&file, true)
}

struct SecurityDescriptor(PSECURITY_DESCRIPTOR);

impl Drop for SecurityDescriptor {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: GetSecurityInfo allocates this one descriptor with LocalAlloc; owner/DACL
            // pointers borrow inside it and are not freed separately. It is freed exactly once.
            unsafe {
                LocalFree(self.0);
            }
        }
    }
}

struct SecurityInfo {
    descriptor: SecurityDescriptor,
    owner: PSID,
    dacl: *mut ACL,
}

fn security_info(file: &File) -> Result<SecurityInfo> {
    let mut descriptor = SecurityDescriptor(null_mut());
    let mut owner = null_mut();
    let mut dacl = null_mut();
    // SAFETY: File is live, outputs are correctly typed and initialized. Descriptor RAII also
    // handles an allocation returned on an error path. Owner/DACL borrow that allocation.
    let result = unsafe {
        GetSecurityInfo(
            handle(file),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            &mut owner,
            null_mut(),
            &mut dacl,
            null_mut(),
            &mut descriptor.0,
        )
    };
    if result != ERROR_SUCCESS || descriptor.0.is_null() || owner.is_null() {
        return Err(permissions_failed());
    }
    // SAFETY: owner was returned inside the live OS security descriptor.
    if unsafe { IsValidSid(owner) } == 0 {
        return Err(permissions_failed());
    }
    Ok(SecurityInfo {
        descriptor,
        owner,
        dacl,
    })
}

/// Install and read back one explicit full-access ACE for the exact existing owner SID, with
/// inheritance protection. Directory ACEs propagate owner-only access to children; child ACLs
/// are still explicitly protected before SQLite is allowed to use them.
pub(crate) fn tighten_handle(file: &File, directory: bool) -> Result<()> {
    verify_handle(file, directory)?;
    let before = security_info(file)?;
    // SAFETY: before.owner was validated and its backing descriptor is alive throughout.
    let sid_bytes = unsafe { GetLengthSid(before.owner) } as usize;
    let acl_bytes =
        size_of::<ACL>() + size_of::<ACCESS_ALLOWED_ACE>() - size_of::<u32>() + sid_bytes;
    if acl_bytes > u16::MAX as usize {
        return Err(permissions_failed());
    }
    // u32 storage guarantees the DWORD alignment required by ACL/ACE. Windows initializes it.
    let mut buffer = vec![0u32; acl_bytes.div_ceil(size_of::<u32>())];
    let acl = buffer.as_mut_ptr().cast::<ACL>();
    let flags = if directory {
        OBJECT_INHERIT_ACE | CONTAINER_INHERIT_ACE
    } else {
        0
    };
    // SAFETY: allocated size/alignment cover the ACL and single variable-size owner SID ACE.
    // The SID/ACL backing allocations remain alive until SetSecurityInfo returns.
    let initialized = unsafe { InitializeAcl(acl, acl_bytes as u32, ACL_REVISION) };
    if initialized == 0 {
        return Err(permissions_failed());
    }
    let added =
        unsafe { AddAccessAllowedAceEx(acl, ACL_REVISION, flags, FILE_ALL_ACCESS, before.owner) };
    if added == 0 {
        return Err(permissions_failed());
    }
    let result = unsafe {
        SetSecurityInfo(
            handle(file),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            null_mut(),
            null_mut(),
            acl,
            null(),
        )
    };
    if result != ERROR_SUCCESS {
        return Err(permissions_failed());
    }
    verify_owner_only(file, &before, flags)?;
    verify_handle(file, directory)?;
    Ok(())
}

fn verify_owner_only(file: &File, before: &SecurityInfo, flags: u32) -> Result<()> {
    let after = security_info(file)?;
    let mut control = 0;
    let mut revision = 0;
    // SAFETY: both SIDs and the descriptor are borrowed from live OS allocations.
    let same_owner = unsafe { EqualSid(before.owner, after.owner) } != 0;
    let controlled =
        unsafe { GetSecurityDescriptorControl(after.descriptor.0, &mut control, &mut revision) }
            != 0;
    if !same_owner || !controlled || control & SE_DACL_PROTECTED == 0 || after.dacl.is_null() {
        return Err(permissions_failed());
    }
    // SAFETY: POD output; after.dacl points into a live OS descriptor.
    let mut size: ACL_SIZE_INFORMATION = unsafe { zeroed() };
    let read = unsafe {
        GetAclInformation(
            after.dacl,
            (&mut size as *mut ACL_SIZE_INFORMATION).cast(),
            size_of::<ACL_SIZE_INFORMATION>() as u32,
            AclSizeInformation,
        )
    };
    if read == 0 || size.AceCount != 1 {
        return Err(permissions_failed());
    }
    let mut ace: *mut c_void = null_mut();
    // SAFETY: ACL is live, GetAclInformation confirmed precisely one ACE.
    if unsafe { GetAce(after.dacl, 0, &mut ace) } == 0 || ace.is_null() {
        return Err(permissions_failed());
    }
    // SAFETY: GetAce returned a DWORD-aligned ACE inside the OS ACL. Read the common header first;
    // only interpret ACCESS_ALLOWED_ACE after checking its type and minimum fixed size.
    let header = unsafe { &*ace.cast::<windows_sys::Win32::Security::ACE_HEADER>() };
    if header.AceType as u32 != ACCESS_ALLOWED_ACE_TYPE
        || header.AceFlags as u32 != flags
        || (header.AceSize as usize) < size_of::<ACCESS_ALLOWED_ACE>()
    {
        return Err(permissions_failed());
    }
    let allowed = ace.cast::<ACCESS_ALLOWED_ACE>();
    // SAFETY: fixed-size fields fit the validated allow ACE. SidStart is the beginning of the
    // variable-length SID; the OS descriptor supplies its valid bounds, checked below as well.
    let mask = unsafe { (*allowed).Mask };
    let sid = unsafe { addr_of!((*allowed).SidStart).cast_mut().cast::<c_void>() };
    if mask != FILE_ALL_ACCESS || unsafe { IsValidSid(sid) } == 0 {
        return Err(permissions_failed());
    }
    let sid_bytes = unsafe { GetLengthSid(sid) } as usize;
    let required = size_of::<ACCESS_ALLOWED_ACE>() - size_of::<u32>() + sid_bytes;
    if required > header.AceSize as usize || unsafe { EqualSid(after.owner, sid) } == 0 {
        return Err(permissions_failed());
    }
    Ok(())
}
