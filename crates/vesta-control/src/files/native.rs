//! Leaf no-follow opens and checks of the actual owned regular-file handle.
//! Ancestor checks remain check-then-use; this is not a native sandbox.
use super::{FileError, Result, io_error};
use std::{
    fs::{File, OpenOptions},
    path::{Path, PathBuf},
};

#[cfg(windows)]
pub(super) fn open_regular(path: &Path) -> Result<File> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::{FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ};
    let file = OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
        .map_err(io_error)?;
    windows::check(&file)?;
    Ok(file)
}
#[cfg(unix)]
pub(super) fn open_regular(path: &Path) -> Result<File> {
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)
        .map_err(io_error)?;
    let metadata = file.metadata().map_err(io_error)?;
    if !metadata.is_file() {
        return Err(FileError::NotRegular);
    }
    if metadata.nlink() != 1 {
        return Err(FileError::LinkedPath);
    }
    Ok(file)
}
#[cfg(not(any(windows, unix)))]
pub(super) fn open_regular(_path: &Path) -> Result<File> {
    Err(FileError::Unavailable)
}

#[cfg(windows)]
pub(super) fn actual_path(file: &File, _path: &Path) -> Result<PathBuf> {
    windows::path(file)
}
#[cfg(unix)]
pub(super) fn actual_path(file: &File, path: &Path) -> Result<PathBuf> {
    use std::os::unix::fs::MetadataExt;
    let actual = std::fs::canonicalize(path).map_err(io_error)?;
    let held = file.metadata().map_err(io_error)?;
    let named = std::fs::symlink_metadata(&actual).map_err(io_error)?;
    super::root::plain(&named)?;
    if held.dev() != named.dev() || held.ino() != named.ino() {
        return Err(FileError::LinkedPath);
    }
    Ok(actual)
}
#[cfg(not(any(windows, unix)))]
pub(super) fn actual_path(_file: &File, _path: &Path) -> Result<PathBuf> {
    Err(FileError::Unavailable)
}

#[cfg(windows)]
#[allow(unsafe_code)]
mod windows {
    use super::*;
    use std::{
        ffi::OsString,
        mem::zeroed,
        os::windows::{ffi::OsStringExt, io::AsRawHandle},
    };
    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT,
        FILE_TYPE_DISK, GetFileInformationByHandle, GetFileType, GetFinalPathNameByHandleW,
    };
    pub(super) fn check(file: &File) -> Result<()> {
        // SAFETY: the std File owns a live handle for this entire synchronous call;
        // the initialized output struct is correctly sized and exclusively borrowed.
        let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { zeroed() };
        let handle = file.as_raw_handle().cast();
        if unsafe { GetFileType(handle) } != FILE_TYPE_DISK
            || unsafe { GetFileInformationByHandle(handle, &mut info) } == 0
        {
            return Err(FileError::Unavailable);
        }
        if info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 || info.nNumberOfLinks != 1 {
            return Err(FileError::LinkedPath);
        }
        if info.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY != 0 {
            return Err(FileError::NotRegular);
        }
        Ok(())
    }
    pub(super) fn path(file: &File) -> Result<PathBuf> {
        check(file)?;
        let mut units = vec![0_u16; 32_768];
        // SAFETY: the owned File retains the live handle. The writable UTF-16
        // buffer has exactly the supplied capacity; no pointer escapes the call.
        let written = unsafe {
            GetFinalPathNameByHandleW(
                file.as_raw_handle().cast(),
                units.as_mut_ptr(),
                units.len() as u32,
                0,
            )
        } as usize;
        if written == 0 || written >= units.len() {
            return Err(FileError::Unavailable);
        }
        units.truncate(written);
        Ok(PathBuf::from(OsString::from_wide(&units)))
    }
}

/// Available and total bytes on the volume holding `path`.
#[cfg(windows)]
#[allow(unsafe_code)]
pub(super) fn volume_space(path: &Path) -> Result<(u64, u64)> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
    let (mut available, mut total) = (0_u64, 0_u64);
    // SAFETY: `wide` is a NUL-terminated buffer alive for the call; the outputs are
    // exclusively borrowed u64s and the optional third output is null.
    let ok = unsafe {
        GetDiskFreeSpaceExW(
            wide.as_ptr(),
            &mut available,
            &mut total,
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        return Err(FileError::Unavailable);
    }
    Ok((available, total))
}
#[cfg(unix)]
#[allow(unsafe_code)]
pub(super) fn volume_space(path: &Path) -> Result<(u64, u64)> {
    use std::os::unix::ffi::OsStrExt;
    let text =
        std::ffi::CString::new(path.as_os_str().as_bytes()).map_err(|_| FileError::InvalidPath)?;
    // SAFETY: `text` is NUL-terminated and alive for the call; `stat` is a zeroed,
    // exclusively borrowed output struct of the correct type.
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(text.as_ptr(), &mut stat) } != 0 {
        return Err(FileError::Unavailable);
    }
    let unit = stat.f_frsize as u64;
    Ok((
        (stat.f_bavail as u64).saturating_mul(unit),
        (stat.f_blocks as u64).saturating_mul(unit),
    ))
}
#[cfg(not(any(windows, unix)))]
pub(super) fn volume_space(_path: &Path) -> Result<(u64, u64)> {
    Err(FileError::Unavailable)
}

#[cfg(unix)]
pub(super) fn same_volume(a: &Path, b: &Path) -> Result<bool> {
    use std::os::unix::fs::MetadataExt;
    Ok(std::fs::metadata(a).map_err(io_error)?.dev()
        == std::fs::metadata(b).map_err(io_error)?.dev())
}
#[cfg(windows)]
#[allow(unsafe_code)]
pub(super) fn same_volume(a: &Path, b: &Path) -> Result<bool> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::GetVolumePathNameW;
    fn volume(path: &Path) -> Result<Vec<u16>> {
        let wide: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
        let mut output = vec![0_u16; 32_768];
        // SAFETY: NUL-terminated input and owned output retain their buffers for the call.
        if unsafe { GetVolumePathNameW(wide.as_ptr(), output.as_mut_ptr(), output.len() as u32) }
            == 0
        {
            return Err(FileError::Unavailable);
        }
        output.truncate(
            output
                .iter()
                .position(|unit| *unit == 0)
                .ok_or(FileError::Unavailable)?,
        );
        Ok(output)
    }
    Ok(volume(a)? == volume(b)?)
}
#[cfg(not(any(unix, windows)))]
pub(super) fn same_volume(_a: &Path, _b: &Path) -> Result<bool> {
    Err(FileError::Unavailable)
}

/// Remove only an owned staging name, including read-only stages, without clearing
/// the attribute on a published hard-link name. Never used for user targets.
#[cfg(windows)]
#[allow(unsafe_code)]
pub(super) fn remove_private_stage(path: &Path) -> Result<()> {
    use std::os::windows::{fs::OpenOptionsExt, io::AsRawHandle};
    use windows_sys::Win32::Storage::FileSystem::{
        DELETE, FILE_DISPOSITION_FLAG_DELETE, FILE_DISPOSITION_FLAG_IGNORE_READONLY_ATTRIBUTE,
        FILE_DISPOSITION_INFO_EX, FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_DELETE, FILE_SHARE_READ,
        FILE_SHARE_WRITE, FileDispositionInfoEx, SetFileInformationByHandle,
    };
    let file = OpenOptions::new()
        .access_mode(DELETE)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
        .map_err(io_error)?;
    super::root::plain(&file.metadata().map_err(io_error)?)?;
    let info = FILE_DISPOSITION_INFO_EX {
        Flags: FILE_DISPOSITION_FLAG_DELETE | FILE_DISPOSITION_FLAG_IGNORE_READONLY_ATTRIBUTE,
    };
    // SAFETY: live owned handle and correctly sized immutable disposition record.
    let ok = unsafe {
        SetFileInformationByHandle(
            file.as_raw_handle().cast(),
            FileDispositionInfoEx,
            (&info as *const FILE_DISPOSITION_INFO_EX).cast(),
            std::mem::size_of::<FILE_DISPOSITION_INFO_EX>() as u32,
        )
    };
    if ok == 0 {
        return Err(io_error(std::io::Error::last_os_error()));
    }
    Ok(())
}
#[cfg(not(windows))]
pub(super) fn remove_private_stage(path: &Path) -> Result<()> {
    std::fs::remove_file(path).map_err(io_error)
}
