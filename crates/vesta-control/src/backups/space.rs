//! Free space available to this process on the disk holding a path.
use std::path::Path;

/// Bytes available to this process on the volume holding `path`, measured at its
/// nearest existing ancestor. `None` when it cannot be determined; callers refuse.
pub fn available(path: &Path) -> Option<u64> {
    let existing = path.ancestors().find(|p| p.exists())?;
    query(existing)
}

#[cfg(windows)]
#[allow(unsafe_code)]
fn query(path: &Path) -> Option<u64> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
    let wide: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let mut free_to_caller = 0u64;
    // SAFETY: `wide` is a NUL-terminated UTF-16 path that outlives the call; the
    // only output pointer is a valid u64 and the other outputs are optional (null).
    let ok = unsafe {
        GetDiskFreeSpaceExW(
            wide.as_ptr(),
            &mut free_to_caller,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    (ok != 0).then_some(free_to_caller)
}

#[cfg(unix)]
#[allow(unsafe_code)]
fn query(path: &Path) -> Option<u64> {
    use std::os::unix::ffi::OsStrExt;
    let c_path = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut stat = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: `c_path` is a valid NUL-terminated string and `stat` is writable
    // storage for exactly one statvfs, read only after success.
    let rc = unsafe { libc::statvfs(c_path.as_ptr(), stat.as_mut_ptr()) };
    if rc != 0 {
        return None;
    }
    // SAFETY: statvfs returned 0, so it initialized `stat`.
    let stat = unsafe { stat.assume_init() };
    #[allow(clippy::unnecessary_cast)]
    (stat.f_bavail as u64).checked_mul(stat.f_frsize as u64)
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_temp_directory_has_measurable_free_space() {
        let temp = tempfile::tempdir().unwrap();
        assert!(super::available(temp.path()).is_some_and(|b| b > 0));
        // A not-yet-created path is measured at its existing ancestor.
        assert!(super::available(&temp.path().join("a/b/c")).is_some());
    }
}
