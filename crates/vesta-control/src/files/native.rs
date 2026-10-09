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
